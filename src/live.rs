//! A camera's own video, decoded by the TV's hardware decoder.
//!
//! The Home Assistant integration's relay (`custom_components/luma/relay.py`) sends a camera's
//! H.264 or HEVC as it arrives, starting with what the camera sent since its last keyframe. A
//! reader thread takes that stream and feeds LG's direct media decoder (`libNDL_directmedia`,
//! webOS 5 and later), which draws on the TV's video plane, behind Luma's window: the gallery
//! shows it by leaving that part of its window transparent.
//!
//! The decoder is loaded and unloaded on the SDL thread (it looks up the current GL window);
//! frames are fed from the reader thread. Desktop builds and TVs without the library have no
//! decoder, and the gallery plays Home Assistant's HLS stream in the overlay app instead.
use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Instant,
};

const MAGIC: &[u8; 8] = b"LUMAVID1";
const KEYFRAME: u8 = 1;
const CACHED: u8 = 2;
/// Largest encoded frame accepted.
const MAX_FRAME: usize = 8 * 1024 * 1024;
/// The picture counts as showing once this many frames have been fed after the first keyframe.
const SHOWN_AFTER: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Codec {
    H264,
    Hevc,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// The relay answered: the decoder can be loaded for this video.
    Opened {
        codec: Codec,
        width: u32,
        height: u32,
    },
    /// The decoder has pictures: the gallery can uncover the video plane.
    Showing,
    Failed(String),
}

/// The start of a relayed stream.
pub fn parse_header(reader: &mut impl Read) -> Result<(Codec, u32, u32), String> {
    let mut magic = [0; 8];
    reader.read_exact(&mut magic).map_err(|e| e.to_string())?;
    if &magic != MAGIC {
        return Err("not a relayed video stream".into());
    }
    let mut length = [0; 4];
    reader.read_exact(&mut length).map_err(|e| e.to_string())?;
    let length = u32::from_be_bytes(length) as usize;
    if length > 4096 {
        return Err("the stream header is too large".into());
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    let header: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let codec = match header["codec"].as_str() {
        Some("h264") => Codec::H264,
        Some("hevc") => Codec::Hevc,
        other => return Err(format!("{other:?} video cannot be decoded")),
    };
    let size = |key: &str| {
        header[key]
            .as_u64()
            .filter(|size| (16..=8192).contains(size))
            .map(|size| size as u32)
            .ok_or_else(|| format!("the stream has no {key}"))
    };
    Ok((codec, size("width")?, size("height")?))
}

/// One frame: its flags, its time in microseconds and its Annex B bytes into `data`.
pub fn read_frame(reader: &mut impl Read, data: &mut Vec<u8>) -> Result<(u8, i64), String> {
    let mut head = [0; 13];
    reader.read_exact(&mut head).map_err(|e| e.to_string())?;
    let length = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    if length > MAX_FRAME {
        return Err("a frame is too large".into());
    }
    let mut pts = [0; 8];
    pts.copy_from_slice(&head[5..13]);
    data.resize(length, 0);
    reader.read_exact(data).map_err(|e| e.to_string())?;
    Ok((head[4], i64::from_be_bytes(pts)))
}

/// Whether this TV has a decoder Luma can feed.
pub fn available() -> bool {
    decoder::available()
}

/// One camera's video: the reader thread and, once loaded, the decoder.
pub struct Live {
    stop: Arc<AtomicBool>,
    /// Set once the decoder is loaded: the reader feeds it from then on.
    loaded: Arc<AtomicBool>,
    events: mpsc::Receiver<Event>,
    opened: bool,
}

impl Live {
    pub fn start(url: String) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let loaded = Arc::new(AtomicBool::new(false));
        let (sender, events) = mpsc::channel();
        let (stopped, ready) = (stop.clone(), loaded.clone());
        let spawned = thread::Builder::new()
            .name("camera-live".into())
            .spawn(move || {
                if let Err(error) = read(&url, &stopped, &ready, &sender)
                    && !stopped.load(Ordering::Acquire)
                {
                    let _ = sender.send(Event::Failed(error));
                }
            });
        if let Err(error) = spawned {
            crate::telemetry::log_warn!("cameras", "{error}");
        }
        Self {
            stop,
            loaded,
            events,
            opened: false,
        }
    }

    /// What happened since the last call. Call on the SDL thread: it loads the decoder there
    /// when the relay has answered.
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(Event::Opened {
                    codec,
                    width,
                    height,
                }) => {
                    let started = Instant::now();
                    match decoder::open(codec, width, height) {
                        Ok(()) => {
                            self.opened = true;
                            self.loaded.store(true, Ordering::Release);
                            crate::telemetry::log_info!(
                                "cameras",
                                "decoder loaded for {width}x{height} {codec:?} in {} ms",
                                started.elapsed().as_millis()
                            );
                        }
                        Err(error) => {
                            self.stop.store(true, Ordering::Release);
                            events.push(Event::Failed(error));
                        }
                    }
                }
                Ok(event) => events.push(event),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // The reader ended without saying why (it was stopped, or panicked).
                    if !self.stop.swap(true, Ordering::AcqRel) {
                        events.push(Event::Failed("the video reader stopped".into()));
                    }
                    break;
                }
            }
        }
        events
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.loaded.store(false, Ordering::Release);
        if self.opened {
            decoder::close();
        }
    }
}

fn read(
    url: &str,
    stop: &AtomicBool,
    loaded: &AtomicBool,
    events: &mpsc::Sender<Event>,
) -> Result<(), String> {
    let response = crate::cameras::agent()
        .get(url)
        .call()
        .map_err(|e| e.to_string())?;
    let mut reader =
        std::io::BufReader::with_capacity(256 * 1024, response.into_body().into_reader());
    let (codec, width, height) = parse_header(&mut reader)?;
    let _ = events.send(Event::Opened {
        codec,
        width,
        height,
    });
    let began = Instant::now();
    // Frames that arrive while the decoder loads wait here, from the latest keyframe on.
    let mut waiting: Vec<Vec<u8>> = Vec::new();
    let mut data = Vec::new();
    let mut fed = 0usize;
    let mut shown = false;
    let mut feed = |bytes: &[u8]| -> Result<(), String> {
        decoder::feed(bytes, began.elapsed().as_millis() as i64)?;
        fed += 1;
        if !shown && fed >= SHOWN_AFTER {
            shown = true;
            let _ = events.send(Event::Showing);
        }
        Ok(())
    };
    while !stop.load(Ordering::Acquire) {
        let (flags, _pts) = read_frame(&mut reader, &mut data)?;
        if !loaded.load(Ordering::Acquire) {
            if flags & KEYFRAME != 0 {
                waiting.clear();
            }
            if flags & KEYFRAME != 0 || !waiting.is_empty() {
                waiting.push(data.clone());
            }
            continue;
        }
        for frame in waiting.drain(..) {
            feed(&frame)?;
        }
        let _ = flags & CACHED;
        feed(&data)?;
    }
    Ok(())
}

#[cfg(feature = "webos")]
mod decoder {
    //! LG's direct media decoder, loaded at run time so Luma still starts where it is missing.
    use super::Codec;
    use std::{
        ffi::{CStr, CString, c_char, c_int, c_longlong, c_uint, c_void},
        sync::{Mutex, OnceLock},
    };

    type Released = extern "C" fn(*const c_char);
    type Loaded = extern "C" fn(c_int, c_longlong, *const c_char);

    type Init = unsafe extern "C" fn(*const c_char, Released) -> c_int;
    type Load = unsafe extern "C" fn(*mut c_void, Loaded) -> c_int;
    type Unload = unsafe extern "C" fn() -> c_int;
    type Play = unsafe extern "C" fn(*mut c_void, c_uint, c_longlong) -> c_int;
    type Area = unsafe extern "C" fn(c_int, c_int, c_int, c_int) -> c_int;
    type LastError = unsafe extern "C" fn() -> *const c_char;

    struct Library {
        init: Init,
        load: Load,
        unload: Unload,
        play: Play,
        area: Area,
        error: LastError,
    }

    /// `NDL_DIRECTMEDIA_DATA_INFO_T`: the video's size and type, then the audio description
    /// (none here), which holds a double and so aligns the whole to eight bytes.
    #[repr(C, align(8))]
    struct Info {
        width: c_int,
        height: c_int,
        kind: c_int,
        unknown: c_int,
        audio: [u8; 48],
    }

    static LIBRARY: OnceLock<Option<Library>> = OnceLock::new();
    /// Whether the library was initialised, and whether a video is loaded.
    static STATE: Mutex<(bool, bool)> = Mutex::new((false, false));

    fn library() -> Option<&'static Library> {
        LIBRARY
            .get_or_init(|| unsafe {
                let handle = libc::dlopen(
                    c"libNDL_directmedia.so.1".as_ptr(),
                    libc::RTLD_NOW | libc::RTLD_GLOBAL,
                );
                if handle.is_null() {
                    return None;
                }
                let symbol = |name: &CStr| {
                    let address = libc::dlsym(handle, name.as_ptr());
                    (!address.is_null()).then_some(address)
                };
                // SAFETY: the signatures are those of webOS's NDL_directmedia_v2.h.
                Some(Library {
                    init: std::mem::transmute::<*mut c_void, Init>(symbol(c"NDL_DirectMediaInit")?),
                    load: std::mem::transmute::<*mut c_void, Load>(symbol(c"NDL_DirectMediaLoad")?),
                    unload: std::mem::transmute::<*mut c_void, Unload>(symbol(
                        c"NDL_DirectMediaUnload",
                    )?),
                    play: std::mem::transmute::<*mut c_void, Play>(symbol(c"NDL_DirectVideoPlay")?),
                    area: std::mem::transmute::<*mut c_void, Area>(symbol(
                        c"NDL_DirectVideoSetArea",
                    )?),
                    error: std::mem::transmute::<*mut c_void, LastError>(symbol(
                        c"NDL_DirectMediaGetError",
                    )?),
                })
            })
            .as_ref()
    }

    pub fn available() -> bool {
        library().is_some()
    }

    fn last_error(library: &Library) -> String {
        // SAFETY: the library returns a static or null string.
        unsafe {
            let message = (library.error)();
            if message.is_null() {
                "the decoder failed".into()
            } else {
                CStr::from_ptr(message).to_string_lossy().into_owned()
            }
        }
    }

    extern "C" fn released(kind: *const c_char) {
        let kind = if kind.is_null() {
            String::new()
        } else {
            // SAFETY: a string from the library, valid during the call.
            unsafe { CStr::from_ptr(kind) }
                .to_string_lossy()
                .into_owned()
        };
        crate::telemetry::log_warn!("cameras", "the TV took the decoder back: {kind}");
    }

    extern "C" fn loaded(kind: c_int, number: c_longlong, text: *const c_char) {
        // 0x16 loaded, 0x17 unloaded, 0x1a playing; anything else is worth reading.
        if matches!(kind, 0x16 | 0x17 | 0x1a) {
            return;
        }
        let text = if text.is_null() {
            String::new()
        } else {
            // SAFETY: a string from the library, valid during the call.
            unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned()
        };
        crate::telemetry::log_warn!("cameras", "decoder: {kind:#x} {number} {text}");
    }

    /// Loads the decoder for a video, drawn as large as fits the screen. SDL thread only.
    pub fn open(codec: Codec, width: u32, height: u32) -> Result<(), String> {
        let library = library().ok_or("this TV has no decoder Luma can use")?;
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: called on the SDL thread with a current GL window, as the library expects.
        unsafe {
            if !state.0 {
                let app = std::env::var("APPID").unwrap_or_default();
                let app = CString::new(app).map_err(|e| e.to_string())?;
                if (library.init)(app.as_ptr(), released) != 0 {
                    return Err(last_error(library));
                }
                state.0 = true;
            }
            if state.1 {
                (library.unload)();
                state.1 = false;
            }
            let mut info = Info {
                width: width as c_int,
                height: height as c_int,
                kind: match codec {
                    Codec::H264 => 1,
                    Codec::Hevc => 2,
                },
                unknown: 0,
                audio: [0; 48],
            };
            if (library.load)((&raw mut info).cast(), loaded) != 0 {
                return Err(last_error(library));
            }
            state.1 = true;
            // Keep the picture's shape inside the 1920x1080 screen.
            let scale = (1920. / width as f32).min(1080. / height as f32);
            let (w, h) = (
                (width as f32 * scale) as c_int,
                (height as f32 * scale) as c_int,
            );
            if (w, h) != (1920, 1080) {
                (library.area)((1920 - w) / 2, (1080 - h) / 2, w, h);
            }
        }
        Ok(())
    }

    pub fn feed(bytes: &[u8], pts: i64) -> Result<(), String> {
        let library = library().ok_or("no decoder")?;
        // SAFETY: the library copies the frame during the call.
        let result =
            unsafe { (library.play)(bytes.as_ptr().cast_mut().cast(), bytes.len() as c_uint, pts) };
        if result == 0 {
            Ok(())
        } else {
            Err(last_error(library))
        }
    }

    pub fn close() {
        let Some(library) = library() else { return };
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if state.1 {
            // SAFETY: unloading the video this module loaded.
            unsafe { (library.unload)() };
            state.1 = false;
        }
    }
}

#[cfg(not(feature = "webos"))]
mod decoder {
    use super::Codec;

    pub fn available() -> bool {
        false
    }
    pub fn open(_codec: Codec, _width: u32, _height: u32) -> Result<(), String> {
        Err("no decoder in desktop builds".into())
    }
    pub fn feed(_bytes: &[u8], _pts: i64) -> Result<(), String> {
        Err("no decoder in desktop builds".into())
    }
    pub fn close() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn stream(header: &str, frames: &[(u8, i64, &[u8])]) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend((header.len() as u32).to_be_bytes());
        bytes.extend(header.as_bytes());
        for (flags, pts, data) in frames {
            bytes.extend((data.len() as u32).to_be_bytes());
            bytes.push(*flags);
            bytes.extend(pts.to_be_bytes());
            bytes.extend(*data);
        }
        bytes
    }

    #[test]
    fn a_relayed_stream_is_a_header_then_frames() {
        let bytes = stream(
            r#"{"codec":"h264","width":2688,"height":1512}"#,
            &[(KEYFRAME | CACHED, 1_000, b"key"), (0, 34_333, b"next")],
        );
        let mut reader = Cursor::new(bytes);
        assert_eq!(parse_header(&mut reader), Ok((Codec::H264, 2688, 1512)));
        let mut data = Vec::new();
        assert_eq!(
            read_frame(&mut reader, &mut data),
            Ok((KEYFRAME | CACHED, 1_000))
        );
        assert_eq!(data, b"key");
        assert_eq!(read_frame(&mut reader, &mut data), Ok((0, 34_333)));
        assert_eq!(data, b"next");
        assert!(read_frame(&mut reader, &mut data).is_err());
    }

    #[test]
    fn other_streams_and_codecs_are_refused() {
        let mut other = Cursor::new(b"<html>not video</html>".to_vec());
        assert!(parse_header(&mut other).is_err());
        let vp9 = stream(r#"{"codec":"vp9","width":1280,"height":720}"#, &[]);
        assert!(parse_header(&mut Cursor::new(vp9)).is_err());
        let no_size = stream(r#"{"codec":"hevc","width":0,"height":720}"#, &[]);
        assert!(parse_header(&mut Cursor::new(no_size)).is_err());
        let hevc = stream(r#"{"codec":"hevc","width":3840,"height":2160}"#, &[]);
        assert_eq!(
            parse_header(&mut Cursor::new(hevc)),
            Ok((Codec::Hevc, 3840, 2160))
        );
        // A frame longer than any real one is not read into memory.
        let mut huge = (u32::MAX).to_be_bytes().to_vec();
        huge.extend([0; 9]);
        assert!(read_frame(&mut Cursor::new(huge), &mut Vec::new()).is_err());
    }
}
