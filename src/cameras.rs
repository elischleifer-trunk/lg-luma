//! Luma's built-in Cameras app: Home Assistant's cameras in a native full-screen gallery.
//!
//! The Home Assistant thread owns the camera list (with signed URLs that rotate every few
//! minutes) and the owner's saved arrangement; it sends both here as [`Camera`] lists. The
//! gallery asks [`Service`] for what it shows right now, as [`Watch`]es: snapshot polling for
//! grid tiles, and the motion-JPEG stream for an enlarged camera. Each watched camera has its
//! own worker thread that fetches, decodes and downscales frames off the SDL thread.
//!
//! Reading the stream itself (rather than leaving it to a browser) tells Luma exactly when
//! frames arrive, so a stream that stalls or fails switches to back-to-back snapshots.
//!
//! Home Assistant's motion-JPEG stream is made of snapshots for most cameras, about two a
//! second. A camera that can stream also has an HLS endpoint: an enlarged camera with live video
//! asks it for the address of Home Assistant's HLS stream (the camera's own H.264 video) and the
//! overlay app plays that full screen with the TV's hardware decoder. The motion-JPEG stream
//! keeps running underneath: it shows until the video plays, and stays if it cannot.
use crate::model::clean_input_name;
use crate::state::{Direction, Effect, Overlay, Rect, State, Target, TextField};
use image::{ImageReader, Limits, RgbaImage};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{Cursor, Read},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

/// Grid snapshots refresh this often, or [`SLOW_REFRESH`] above nine cameras, so the gallery
/// never holds more than one request per camera and Home Assistant is not flooded.
const REFRESH: Duration = Duration::from_secs(2);
const SLOW_REFRESH: Duration = Duration::from_secs(4);
/// An enlarged camera without live video loads snapshots back to back, at most this often.
const SNAPSHOT_GAP: Duration = Duration::from_millis(250);
/// A stream that delivers no complete frame for this long has stalled.
const STALL: Duration = Duration::from_secs(5);
/// Failed fetches retry after this long.
const RETRY: Duration = Duration::from_secs(3);
/// Largest snapshot or motion-JPEG frame accepted.
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// Decoding is the expensive part on a TV; at most this many run at once.
const DECODERS: usize = 2;
/// The enlarged camera's frames are kept at most at the screen's logical size.
pub const FULL_SIZE: (u32, u32) = (1920, 1080);

/// One camera as the gallery shows it. `id` is unique across paired servers.
#[derive(Clone, Debug, PartialEq)]
pub struct Camera {
    pub id: String,
    pub name: String,
    pub stream: Option<String>,
    pub snapshot: Option<String>,
    /// The integration's endpoint that starts Home Assistant's HLS stream for this camera.
    pub hls: Option<String>,
    pub hidden: bool,
    /// The owner's own name for the camera, shown instead of Home Assistant's `name`.
    pub label: Option<String>,
}

impl Camera {
    /// The name the gallery shows.
    pub fn title(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }
}

/// A decoded frame, uploaded to the GPU once per `serial`.
pub struct Frame {
    /// The camera it belongs to: the renderer keeps one texture per camera.
    pub key: String,
    pub serial: u64,
    pub pixels: RgbaImage,
    /// From the live stream rather than a snapshot.
    pub live: bool,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("key", &self.key)
            .field("serial", &self.serial)
            .field("size", &self.pixels.dimensions())
            .field("live", &self.live)
            .finish()
    }
}

static SERIAL: AtomicU64 = AtomicU64::new(1);

impl Frame {
    pub fn new(key: &str, pixels: RgbaImage, live: bool) -> Self {
        Self {
            key: key.to_owned(),
            serial: SERIAL.fetch_add(1, Ordering::Relaxed),
            pixels,
            live,
        }
    }
}

/// Where a watched camera's frames come from.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Snapshots {
        url: String,
        every: Duration,
    },
    /// The motion-JPEG stream, then `fallback` snapshots once it fails or stalls.
    Stream {
        url: String,
        fallback: Option<String>,
    },
}

/// A camera the gallery shows right now, and the size its frames are drawn at.
#[derive(Clone, Debug, PartialEq)]
pub struct Watch {
    pub id: String,
    pub source: Source,
    pub size: (u32, u32),
}

#[derive(Debug)]
pub enum Update {
    Frame(Arc<Frame>),
    /// The camera could not be loaded; its last frame, if any, stays on screen.
    Unavailable(String),
    /// The address of a camera's HLS stream, or why there is none.
    Video {
        id: String,
        result: Result<String, String>,
    },
}

/// A camera video for the overlay app to play.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoRequest {
    /// Home Assistant's HLS playlist.
    pub url: String,
    pub title: String,
    /// Shown over the video until it plays, hiding the TV's black video plane meanwhile.
    pub snapshot: Option<String>,
}

/// Home Assistant's HLS video for the enlarged camera.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Video {
    #[default]
    Off,
    /// Asking the integration to start the stream.
    Resolving(String),
    /// Handed to the overlay app, which shows it once it plays; until then, and if it cannot,
    /// the motion-JPEG stream shows from underneath.
    Playing(String),
}

/// `http://host:port` of a URL.
fn origin(url: &str) -> &str {
    let start = url.find("://").map_or(0, |at| at + 3);
    url[start..]
        .find('/')
        .map_or(url, |end| &url[..start + end])
}

/// Limits concurrent decodes.
#[derive(Default)]
struct Gate {
    busy: Mutex<usize>,
    free: Condvar,
}

impl Gate {
    fn run<T>(&self, work: impl FnOnce() -> T) -> T {
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        while *busy >= DECODERS {
            busy = self.free.wait(busy).unwrap_or_else(|e| e.into_inner());
        }
        *busy += 1;
        drop(busy);
        let result = work();
        *self.busy.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        self.free.notify_one();
        result
    }
}

struct Worker {
    watch: Arc<Mutex<Watch>>,
    stop: Arc<AtomicBool>,
}

/// Runs one worker per watched camera.
pub struct Service {
    workers: HashMap<String, Worker>,
    sender: mpsc::Sender<Update>,
    pub receiver: mpsc::Receiver<Update>,
    gate: Arc<Gate>,
}

impl Default for Service {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            workers: HashMap::new(),
            sender,
            receiver,
            gate: Arc::default(),
        }
    }
}

fn same_kind(a: &Watch, b: &Watch) -> bool {
    a.size == b.size && std::mem::discriminant(&a.source) == std::mem::discriminant(&b.source)
}

impl Service {
    /// Asks the integration's HLS endpoint to start a camera's stream, answering with
    /// [`Update::Video`].
    pub fn resolve(&self, id: String, endpoint: String) {
        let sender = self.sender.clone();
        let spawned = thread::Builder::new()
            .name("camera-video".into())
            .spawn(move || {
                let result = hls_address(&agent(), &endpoint);
                let _ = sender.send(Update::Video { id, result });
            });
        if let Err(error) = spawned {
            eprintln!("Luma cameras: {error}");
        }
    }

    /// Watches exactly these cameras. A camera already watched the same way keeps its worker
    /// (and a connected stream) and only takes new URLs; the others stop.
    pub fn sync(&mut self, watches: &[Watch]) {
        self.workers.retain(|id, worker| {
            let keep = watches
                .iter()
                .find(|watch| &watch.id == id)
                .filter(|watch| {
                    same_kind(
                        &worker.watch.lock().unwrap_or_else(|e| e.into_inner()),
                        watch,
                    )
                });
            match keep {
                Some(watch) => {
                    let mut current = worker.watch.lock().unwrap_or_else(|e| e.into_inner());
                    if *current != *watch {
                        *current = watch.clone();
                    }
                    true
                }
                None => {
                    worker.stop.store(true, Ordering::Release);
                    false
                }
            }
        });
        for watch in watches {
            if self.workers.contains_key(&watch.id) {
                continue;
            }
            let worker = Worker {
                watch: Arc::new(Mutex::new(watch.clone())),
                stop: Arc::new(AtomicBool::new(false)),
            };
            let (shared, stop, sender, gate) = (
                worker.watch.clone(),
                worker.stop.clone(),
                self.sender.clone(),
                self.gate.clone(),
            );
            let spawned = thread::Builder::new()
                .name("camera".into())
                .spawn(move || run(&shared, &stop, &sender, &gate));
            if let Err(error) = spawned {
                eprintln!("Luma cameras: {error}");
                continue;
            }
            self.workers.insert(watch.id.clone(), worker);
        }
    }

    #[cfg(test)]
    pub fn watched(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.workers.keys().cloned().collect();
        ids.sort();
        ids
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        for worker in self.workers.values() {
            worker.stop.store(true, Ordering::Release);
        }
    }
}

/// The absolute address of the HLS playlist the integration's endpoint started.
fn hls_address(agent: &ureq::Agent, endpoint: &str) -> Result<String, String> {
    let bytes = fetch(agent, endpoint)?;
    let reply: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let path = reply["url"]
        .as_str()
        .filter(|path| path.starts_with('/') && !path.starts_with("//"))
        .ok_or("Home Assistant did not return a stream address")?;
    Ok(format!("{}{path}", origin(endpoint)))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .max_redirects(2)
        .timeout_connect(Some(Duration::from_secs(3)))
        // Home Assistant may take a few seconds to grab a frame from an RTSP camera.
        .timeout_recv_response(Some(Duration::from_secs(10)))
        // A stream with no data for this long has stalled; the read fails instead of waiting.
        .timeout_per_read(Some(STALL))
        .proxy(None)
        .build()
        .new_agent()
}

/// Sleeps for `duration`, returning early (false) once the worker is stopped.
fn pause(stop: &AtomicBool, duration: Duration) -> bool {
    let end = Instant::now() + duration;
    while !stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if now >= end {
            return true;
        }
        thread::sleep((end - now).min(Duration::from_millis(50)));
    }
    false
}

fn run(shared: &Mutex<Watch>, stop: &AtomicBool, sender: &mpsc::Sender<Update>, gate: &Gate) {
    let agent = agent();
    let watch = || shared.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let id = watch().id;
    let send = |update: Update| {
        if !stop.load(Ordering::Acquire) {
            let _ = sender.send(update);
        }
    };
    let deliver = |bytes: &[u8], live: bool| {
        let size = watch().size;
        match gate.run(|| decode(bytes, size)) {
            Some(pixels) => send(Update::Frame(Arc::new(Frame::new(&id, pixels, live)))),
            None => eprintln!("Luma cameras {id}: a frame could not be decoded"),
        }
    };
    // Snapshots from whatever URL the watch holds now, which Home Assistant rotates.
    let snapshots = |gap: Option<Duration>| {
        let mut failures = 0;
        loop {
            let (url, every) = match watch().source {
                Source::Snapshots { url, every } => (Some(url), every),
                Source::Stream { fallback, .. } => (fallback, SNAPSHOT_GAP),
            };
            let Some(url) = url else { return };
            let started = Instant::now();
            match fetch(&agent, &url) {
                Ok(bytes) => {
                    failures = 0;
                    deliver(&bytes, false);
                }
                Err(error) => {
                    failures += 1;
                    if failures == 1 {
                        eprintln!("Luma cameras {id}: {error}");
                    }
                    if failures >= 2 {
                        send(Update::Unavailable(id.clone()));
                    }
                }
            }
            let wait = if failures > 0 {
                RETRY
            } else {
                gap.unwrap_or(every).saturating_sub(started.elapsed())
            };
            if !pause(stop, wait) {
                return;
            }
        }
    };
    match watch().source {
        Source::Snapshots { .. } => snapshots(None),
        Source::Stream { .. } => loop {
            let Source::Stream { url, fallback } = watch().source else {
                return;
            };
            match stream(&agent, &url, stop, |bytes| deliver(bytes, true)) {
                Ok(()) => return,
                Err(error) => eprintln!("Luma cameras {id}: live video stopped: {error}"),
            }
            if stop.load(Ordering::Acquire) {
                return;
            }
            if fallback.is_some() {
                // Snapshots until the camera is enlarged again, which starts a new stream.
                snapshots(Some(SNAPSHOT_GAP));
                return;
            }
            send(Update::Unavailable(id.clone()));
            if !pause(stop, RETRY) {
                return;
            }
        },
    }
}

fn fetch(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let mut response = agent.get(url).call().map_err(|e| e.to_string())?;
    response
        .body_mut()
        .with_config()
        .limit(MAX_FRAME_BYTES as u64)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

/// Reads the motion-JPEG stream, handing each newest complete frame to `frame`. Returns Ok when
/// stopped and an error when the stream fails, ends or stalls.
fn stream(
    agent: &ureq::Agent,
    url: &str,
    stop: &AtomicBool,
    mut frame: impl FnMut(&[u8]),
) -> Result<(), String> {
    let response = agent.get(url).call().map_err(|e| e.to_string())?;
    let mut parts = Parts::new(response.into_body().into_reader());
    // A read with no data for STALL fails (the agent's per-read timeout), ending the stream.
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        match parts.next_frame().map_err(|e| e.to_string())? {
            Some(bytes) => frame(&bytes),
            None => return Err("the stream ended".into()),
        }
    }
}

/// Splits a `multipart/x-mixed-replace` motion-JPEG body into JPEG frames. A part's
/// Content-Length is used when given; otherwise the frame runs from the JPEG start marker to
/// its end marker. When several frames are already buffered, only the newest is returned, so
/// a slow decoder skips frames rather than falling behind.
pub struct Parts<R> {
    reader: R,
    buffer: Vec<u8>,
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| position + from)
}

/// The Content-Length of the part headers just before `start`, if any.
fn part_length(headers: &[u8]) -> Option<usize> {
    let text = String::from_utf8_lossy(&headers[headers.len().saturating_sub(1024)..]);
    text.lines().rev().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().ok())?
    })
}

impl<R: Read> Parts<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
        }
    }

    /// The next complete frame in the buffer as (start, end), skipping to the newest.
    fn complete(&self) -> Option<(usize, usize)> {
        let mut found = None;
        let mut from = 0;
        while let Some(start) = find(&self.buffer, &[0xff, 0xd8, 0xff], from) {
            let end = match part_length(&self.buffer[from..start]) {
                Some(length) if start + length <= self.buffer.len() => Some(start + length),
                Some(_) => None,
                None => find(&self.buffer, &[0xff, 0xd9], start + 2).map(|end| end + 2),
            };
            let Some(end) = end else { break };
            found = Some((start, end));
            from = end;
        }
        found
    }

    pub fn next_frame(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        let mut chunk = [0; 64 * 1024];
        loop {
            if let Some((start, end)) = self.complete() {
                let frame = self.buffer[start..end].to_vec();
                self.buffer.drain(..end);
                return Ok(Some(frame));
            }
            if self.buffer.len() > MAX_FRAME_BYTES {
                return Err(std::io::Error::other("a frame is too large"));
            }
            let read = self.reader.read(&mut chunk)?;
            if read == 0 {
                return Ok(None);
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }
}

/// Decodes a JPEG (or PNG/WebP snapshot), shrunk to fit `size`.
pub fn decode(bytes: &[u8], size: (u32, u32)) -> Option<RgbaImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    let image = if image.width() > size.0 || image.height() > size.1 {
        image.thumbnail(size.0, size.1)
    } else {
        image
    };
    Some(image.into_rgba8())
}

// ---- Layout -------------------------------------------------------------------------------

/// The grid area inside the TV-safe margins, below the header.
pub const GRID: Rect = Rect::new(96., 144., 1728., 882.);
pub const GAP: f32 = 24.;
/// Space under each tile for the camera's name.
pub const CAPTION: f32 = 44.;
/// The settings button at the top right.
pub const GEAR: Rect = Rect::new(1756., 48., 68., 68.);
/// The options and settings sheets slide in from the right.
pub const SHEET: Rect = Rect::new(1344., 0., 576., 1080.);
pub const SHEET_ROW: f32 = 76.8;
pub const SHEET_TOP: f32 = 152.;
pub const SHEET_ROWS: usize = 11;

/// Columns for a camera count: one large view, then 2×2, then three across.
pub fn columns(count: usize) -> usize {
    match count {
        0 | 1 => 1,
        2..=4 => 2,
        _ => 3,
    }
}

/// Tiles keep 16:9 and are sized so two rows fit (one for a lone camera); more rows scroll.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub cols: usize,
    pub rows: usize,
    pub visible_rows: usize,
    pub tile: (f32, f32),
    pub origin: (f32, f32),
}

impl Layout {
    pub fn new(count: usize) -> Self {
        let cols = columns(count);
        let rows = count.div_ceil(cols).max(1);
        let visible_rows = rows.min(if cols == 1 { 1 } else { 2 });
        let widest = (GRID.w - (cols - 1) as f32 * GAP) / cols as f32;
        let tallest = (GRID.h - visible_rows as f32 * CAPTION - (visible_rows - 1) as f32 * GAP)
            / visible_rows as f32;
        let h = (widest * 9. / 16.).min(tallest).floor();
        let w = (h * 16. / 9.).floor();
        let width = cols as f32 * w + (cols - 1) as f32 * GAP;
        let height = visible_rows as f32 * (h + CAPTION) + (visible_rows - 1) as f32 * GAP;
        Self {
            cols,
            rows,
            visible_rows,
            tile: (w, h),
            origin: (
                (GRID.x + (GRID.w - width) / 2.).round(),
                (GRID.y + (GRID.h - height) / 2.).round(),
            ),
        }
    }

    /// The picture of tile `index` with the grid scrolled to `first_row`.
    pub fn frame(&self, index: usize, first_row: usize) -> Rect {
        let row = (index / self.cols) as f32 - first_row as f32;
        let col = (index % self.cols) as f32;
        Rect::new(
            self.origin.0 + col * (self.tile.0 + GAP),
            self.origin.1 + row * (self.tile.1 + CAPTION + GAP),
            self.tile.0,
            self.tile.1,
        )
    }

    /// Grid frames are decoded at their drawn size.
    pub fn frame_size(&self) -> (u32, u32) {
        (self.tile.0 as u32, self.tile.1 as u32)
    }
}

// ---- Gallery state ------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Sheet {
    /// Hold OK on a camera: Move, Rename, Reset name, Hide, Camera settings.
    Options(String),
    /// Live video and which cameras are shown.
    Settings,
}

#[derive(Default)]
pub struct Gallery {
    /// Every camera in the owner's order, hidden ones included.
    pub cameras: Vec<Camera>,
    /// Enlarged cameras open the live stream; off, they poll snapshots.
    pub live: bool,
    pub frames: HashMap<String, Arc<Frame>>,
    pub unavailable: Vec<String>,
    pub enlarged: Option<String>,
    pub sheet: Option<Sheet>,
    /// Arrows move the focused camera until OK or Back.
    pub moving: bool,
    pub first_row: usize,
    pub sheet_first: usize,
    /// The arrangement to save, taken by the main loop for the Home Assistant service.
    pub save: Option<Value>,
    pub video: Video,
    /// Cameras whose HLS video could not be played: they stay on the motion-JPEG stream.
    pub video_failed: Vec<String>,
    /// For the main loop: start this camera's HLS stream (id, endpoint).
    pub resolve: Option<(String, String)>,
    /// For the main loop: play this camera video full screen.
    pub play: Option<VideoRequest>,
    /// For the main loop: close the video the overlay app is playing.
    pub stop_video: bool,
    /// The camera being renamed while the keyboard replaces the gallery's overlay, and the
    /// gallery's own Back target to restore with it.
    pub renaming: Option<(String, Target)>,
    /// The typed name while renaming.
    pub name_draft: String,
}

impl Gallery {
    pub fn visible(&self) -> Vec<&Camera> {
        self.cameras
            .iter()
            .filter(|camera| !camera.hidden)
            .collect()
    }

    fn index(&self, id: &str) -> Option<usize> {
        self.visible().iter().position(|camera| camera.id == id)
    }

    pub fn camera(&self, id: &str) -> Option<&Camera> {
        self.cameras.iter().find(|camera| camera.id == id)
    }

    fn arrangement(&self) -> Value {
        json!({
            "order": self.cameras.iter().map(|camera| &camera.id).collect::<Vec<_>>(),
            "hidden": self.cameras.iter().filter(|camera| camera.hidden)
                .map(|camera| &camera.id).collect::<Vec<_>>(),
            "live": self.live,
            "names": self.cameras.iter()
                .filter_map(|camera| Some((camera.id.clone(), json!(camera.label.as_ref()?))))
                .collect::<serde_json::Map<_, _>>(),
        })
    }

    /// The options a camera's sheet offers, in order.
    pub fn options(&self) -> Vec<Target> {
        let mut options = Vec::new();
        if self.visible().len() > 1 {
            options.push(Target::CameraMove);
        }
        options.push(Target::CameraRename);
        if let Some(Sheet::Options(id)) = &self.sheet
            && self.camera(id).is_some_and(|camera| camera.label.is_some())
        {
            options.push(Target::CameraNameReset);
        }
        options.push(Target::CameraHide);
        options.push(Target::CameraSettings);
        options
    }

    /// The settings sheet's rows, in order.
    pub fn settings(&self) -> Vec<Target> {
        std::iter::once(Target::CameraLive)
            .chain(
                self.cameras
                    .iter()
                    .map(|camera| Target::CameraShown(camera.id.clone())),
            )
            .collect()
    }

    /// Keeps the grid row holding `index` on screen.
    fn reveal(&mut self, index: usize) {
        let layout = Layout::new(self.visible().len());
        let row = index / layout.cols;
        if row < self.first_row {
            self.first_row = row;
        } else if row >= self.first_row + layout.visible_rows {
            self.first_row = row + 1 - layout.visible_rows;
        }
        self.first_row = self
            .first_row
            .min(layout.rows.saturating_sub(layout.visible_rows));
    }

    fn reveal_sheet(&mut self, index: usize) {
        if index < self.sheet_first {
            self.sheet_first = index;
        } else if index >= self.sheet_first + SHEET_ROWS {
            self.sheet_first = index + 1 - SHEET_ROWS;
        }
    }

    /// What the gallery shows now: the enlarged camera, or every visible tile.
    /// Leaves the enlarged camera, closing its video.
    fn shrink(&mut self) {
        self.enlarged = None;
        if matches!(std::mem::take(&mut self.video), Video::Playing(_)) {
            self.stop_video = true;
        }
        self.resolve = None;
    }

    pub fn watches(&self) -> Vec<Watch> {
        if let Some(camera) = self.enlarged.as_deref().and_then(|id| self.camera(id)) {
            let live = camera
                .stream
                .as_ref()
                .filter(|_| self.live || camera.snapshot.is_none());
            let source = if let Some(stream) = live {
                Source::Stream {
                    url: stream.clone(),
                    fallback: camera.snapshot.clone(),
                }
            } else if let Some(snapshot) = &camera.snapshot {
                Source::Snapshots {
                    url: snapshot.clone(),
                    every: SNAPSHOT_GAP,
                }
            } else {
                return vec![];
            };
            return vec![Watch {
                id: camera.id.clone(),
                source,
                size: FULL_SIZE,
            }];
        }
        let visible = self.visible();
        let layout = Layout::new(visible.len());
        let every = if visible.len() > 9 {
            SLOW_REFRESH
        } else {
            REFRESH
        };
        visible
            .iter()
            .filter_map(|camera| {
                let source = match (&camera.snapshot, &camera.stream) {
                    (Some(url), _) => Source::Snapshots {
                        url: url.clone(),
                        every,
                    },
                    // No snapshot URL: the tile shows the stream itself.
                    (None, Some(url)) => Source::Stream {
                        url: url.clone(),
                        fallback: None,
                    },
                    (None, None) => return None,
                };
                Some(Watch {
                    id: camera.id.clone(),
                    source,
                    size: layout.frame_size(),
                })
            })
            .collect()
    }
}

impl State {
    pub fn cameras_open(&self) -> bool {
        matches!(self.overlay, Some(Overlay::Cameras { .. }))
    }

    /// The gallery is on screen: open, or behind the keyboard renaming one of its cameras.
    pub fn cameras_visible(&self) -> bool {
        self.cameras_open() || self.cameras.renaming.is_some()
    }

    /// Home Assistant's cameras, with the saved arrangement applied. `open` comes from the
    /// Cameras app; other lists are fresh URLs or a changed list for an open gallery, which
    /// keeps its arrangement on screen and adds new cameras at the end.
    pub fn camera_list(&mut self, cameras: Vec<Camera>, live: bool, open: bool) {
        let already_open = self.cameras_visible();
        let gallery = &mut self.cameras;
        if open {
            if already_open {
                gallery
                    .frames
                    .retain(|id, _| cameras.iter().any(|c| &c.id == id));
            } else {
                gallery.frames.clear();
            }
            gallery.cameras = cameras;
            gallery.live = live;
            gallery.unavailable.clear();
            gallery.video_failed.clear();
            gallery.shrink();
            gallery.sheet = None;
            gallery.moving = false;
            gallery.first_row = 0;
            gallery.save = None;
            if !already_open {
                let restore = self.focus.clone();
                self.overlay = Some(Overlay::Cameras { restore });
                self.pressed = None;
            }
            self.focus = self.first_camera();
            self.dirty = true;
            return;
        }
        if !already_open {
            return;
        }
        // Known cameras keep the order and visibility shown; a new or returning camera goes
        // after the camera Home Assistant's (arranged) list puts before it.
        let mut merged: Vec<Camera> = gallery
            .cameras
            .iter()
            .filter_map(|camera| {
                let mut next = cameras.iter().find(|c| c.id == camera.id)?.clone();
                next.hidden = camera.hidden;
                next.label = camera.label.clone();
                Some(next)
            })
            .collect();
        for (index, camera) in cameras.iter().enumerate() {
            if merged.iter().any(|c| c.id == camera.id) {
                continue;
            }
            let at = cameras[..index]
                .iter()
                .rev()
                .find_map(|before| merged.iter().position(|c| c.id == before.id))
                .map_or(0, |position| position + 1);
            merged.insert(at, camera.clone());
        }
        if merged.is_empty() {
            self.close_cameras();
            return;
        }
        gallery
            .frames
            .retain(|id, _| merged.iter().any(|c| &c.id == id));
        gallery.cameras = merged;
        if gallery
            .enlarged
            .as_deref()
            .is_some_and(|id| gallery.index(id).is_none())
        {
            gallery.shrink();
        }
        if let Some(Sheet::Options(id)) = &gallery.sheet
            && gallery.index(id).is_none()
        {
            gallery.sheet = None;
            self.focus = self.first_camera();
        }
        self.reconcile_camera_focus();
        self.dirty = true;
    }

    fn first_camera(&self) -> Target {
        self.cameras
            .visible()
            .first()
            .map_or(Target::CameraGear, |camera| {
                Target::Camera(camera.id.clone())
            })
    }

    /// After cameras are hidden or removed, focus a nearby one.
    fn reconcile_camera_focus(&mut self) {
        if let Target::Camera(id) = &self.focus
            && self.cameras.index(id).is_none()
        {
            self.focus = self.first_camera();
        }
        if let Target::CameraShown(id) = &self.focus
            && self.cameras.camera(id).is_none()
        {
            self.focus = Target::CameraLive;
        }
        if let Target::Camera(id) = &self.focus
            && let Some(index) = self.cameras.index(id)
        {
            self.cameras.reveal(index);
        }
    }

    pub fn camera_update(&mut self, update: Update) {
        // Frames still queued when the gallery closed must not come back with it.
        if !self.cameras_visible() {
            return;
        }
        let gallery = &mut self.cameras;
        match update {
            Update::Frame(frame) => {
                if gallery.camera(&frame.key).is_none() {
                    return;
                }
                gallery.unavailable.retain(|id| *id != frame.key);
                gallery.frames.insert(frame.key.clone(), frame);
            }
            Update::Unavailable(id) => {
                if !gallery.unavailable.contains(&id) {
                    gallery.unavailable.push(id);
                }
            }
            Update::Video { id, result } => {
                // Only for the camera still enlarged and waiting for it.
                if gallery.video != Video::Resolving(id.clone()) {
                    return;
                }
                match result {
                    Ok(url) => {
                        let title = gallery
                            .camera(&id)
                            .map_or_else(String::new, |c| c.title().to_owned());
                        let snapshot = gallery.camera(&id).and_then(|c| c.snapshot.clone());
                        gallery.video = Video::Playing(id);
                        gallery.play = Some(VideoRequest {
                            url,
                            title,
                            snapshot,
                        });
                    }
                    Err(error) => {
                        eprintln!("Luma cameras {id}: no HLS stream: {error}");
                        gallery.video = Video::Off;
                        gallery.video_failed.push(id);
                    }
                }
            }
        }
        self.dirty = true;
    }

    /// The overlay app's video closed. Back there also leaves the enlarged camera; a video that
    /// could not play leaves the motion-JPEG stream on screen for the rest of this visit.
    pub fn camera_video_closed(&mut self, failed: bool) {
        let gallery = &mut self.cameras;
        // Only a video being played can close; a camera whose stream is still starting is not
        // affected by a late report about an earlier one.
        let Video::Playing(id) = gallery.video.clone() else {
            return;
        };
        gallery.video = Video::Off;
        if failed {
            gallery.video_failed.push(id);
        } else if gallery.enlarged.as_deref() == Some(id.as_str()) {
            gallery.enlarged = None;
        }
        self.dirty = true;
    }

    /// The gallery is open and every camera on screen has a frame (scripted captures wait
    /// for this).
    pub fn cameras_shown(&self) -> bool {
        self.cameras_open()
            && self.cameras.watches().iter().all(|watch| {
                self.cameras.frames.get(&watch.id).is_some_and(|frame| {
                    // An enlarged camera waits for the source it is watched with.
                    self.cameras.enlarged.is_none()
                        || frame.live == matches!(watch.source, Source::Stream { .. })
                })
            })
    }

    /// The cameras to fetch frames for: none unless the gallery is open and Luma is seen.
    pub fn camera_watches(&self, visible: bool) -> Vec<Watch> {
        if visible && self.cameras_visible() {
            self.cameras.watches()
        } else {
            vec![]
        }
    }

    fn save_cameras(&mut self) {
        self.cameras.save = Some(self.cameras.arrangement());
    }

    pub fn close_cameras(&mut self) {
        if let Some((_, restore)) = self.cameras.renaming.take() {
            // The keyboard took the gallery's place; it goes with it.
            self.overlay = None;
            self.panel_started = None;
            self.cameras.name_draft.clear();
            self.focus = restore;
            self.reconcile_focus();
        }
        if let Some(Overlay::Cameras { restore }) = self.overlay.take() {
            self.focus = restore;
            self.reconcile_focus();
        }
        let gallery = &mut self.cameras;
        gallery.shrink();
        gallery.sheet = None;
        gallery.moving = false;
        // Frames are large; a reopened gallery fetches fresh ones.
        gallery.frames.clear();
        gallery.unavailable.clear();
        self.pressed = None;
        self.dirty = true;
    }

    pub fn camera_direction(&mut self, direction: Direction) -> Vec<Effect> {
        let gallery = &self.cameras;
        if gallery.enlarged.is_some() {
            return vec![];
        }
        if gallery.sheet.is_some() {
            let rows = match gallery.sheet {
                Some(Sheet::Settings) => gallery.settings(),
                _ => gallery.options(),
            };
            let index = rows.iter().position(|row| *row == self.focus).unwrap_or(0);
            let next = match direction {
                Direction::Up => index.saturating_sub(1),
                Direction::Down => (index + 1).min(rows.len() - 1),
                Direction::Left | Direction::Right => index,
            };
            self.focus = rows[next].clone();
            if matches!(gallery.sheet, Some(Sheet::Settings)) {
                self.cameras.reveal_sheet(next);
            }
            return vec![];
        }
        let visible: Vec<String> = gallery.visible().iter().map(|c| c.id.clone()).collect();
        let cols = columns(visible.len());
        if self.focus == Target::CameraGear {
            if matches!(direction, Direction::Down | Direction::Left) && !visible.is_empty() {
                let index = (cols - 1).min(visible.len() - 1);
                self.focus = Target::Camera(visible[index].clone());
                self.cameras.reveal(index);
            }
            return vec![];
        }
        let Target::Camera(id) = &self.focus else {
            self.focus = self.first_camera();
            return vec![];
        };
        let Some(index) = visible.iter().position(|v| v == id) else {
            self.focus = self.first_camera();
            return vec![];
        };
        let last = visible.len() - 1;
        let next = match direction {
            Direction::Left if index % cols > 0 => index - 1,
            Direction::Right if index % cols < cols - 1 && index < last => index + 1,
            Direction::Up if index >= cols => index - cols,
            // Down from a row above a shorter last row lands on that row's last camera.
            Direction::Down if index / cols < last / cols => (index + cols).min(last),
            Direction::Up if !self.cameras.moving => {
                self.focus = Target::CameraGear;
                return vec![];
            }
            _ => index,
        };
        if next == index {
            return vec![];
        }
        if self.cameras.moving {
            // The moved camera takes the new place; hidden ones keep to the end.
            let mut order = visible.clone();
            let moved = order.remove(index);
            order.insert(next, moved);
            let gallery = &mut self.cameras;
            let mut cameras = std::mem::take(&mut gallery.cameras);
            let hidden: Vec<Camera> = cameras.iter().filter(|c| c.hidden).cloned().collect();
            cameras.retain(|c| !c.hidden);
            cameras.sort_by_key(|c| order.iter().position(|id| *id == c.id));
            cameras.extend(hidden);
            gallery.cameras = cameras;
            self.save_cameras();
        } else {
            self.focus = Target::Camera(visible[next].clone());
        }
        self.cameras.reveal(next);
        vec![]
    }

    /// OK in the gallery.
    pub fn camera_activate(&mut self) -> Vec<Effect> {
        self.dirty = true;
        let gallery = &mut self.cameras;
        if gallery.enlarged.is_some() {
            gallery.shrink();
            return vec![];
        }
        if std::mem::take(&mut gallery.moving) {
            return vec![];
        }
        match self.focus.clone() {
            Target::Camera(id) if gallery.sheet.is_none() => {
                // With live video, ask for Home Assistant's HLS stream; the motion-JPEG stream
                // shows meanwhile, and instead when there is none.
                if gallery.live
                    && !gallery.video_failed.contains(&id)
                    && let Some(endpoint) = gallery.camera(&id).and_then(|c| c.hls.clone())
                {
                    gallery.video = Video::Resolving(id.clone());
                    gallery.resolve = Some((id.clone(), endpoint));
                }
                gallery.enlarged = Some(id);
            }
            Target::CameraGear => self.open_camera_settings(),
            Target::CameraMove => {
                if let Some(Sheet::Options(id)) = gallery.sheet.take() {
                    gallery.moving = true;
                    self.focus = Target::Camera(id);
                }
            }
            Target::CameraRename => {
                if let Some(Sheet::Options(id)) = gallery.sheet.take() {
                    self.rename_camera(id);
                }
            }
            Target::CameraNameReset => {
                if let Some(Sheet::Options(id)) = &gallery.sheet
                    && let Some(camera) = gallery.cameras.iter_mut().find(|c| &c.id == id)
                {
                    camera.label = None;
                    self.focus = Target::CameraRename;
                    self.save_cameras();
                }
            }
            Target::CameraHide => {
                if let Some(Sheet::Options(id)) = gallery.sheet.take() {
                    let index = gallery.index(&id).unwrap_or(0);
                    if let Some(camera) = gallery.cameras.iter_mut().find(|c| c.id == id) {
                        camera.hidden = true;
                    }
                    // Focus the camera that took its place.
                    let visible = gallery.visible();
                    self.focus = visible
                        .get(index.min(visible.len().saturating_sub(1)))
                        .map_or(Target::CameraGear, |c| Target::Camera(c.id.clone()));
                    self.reconcile_camera_focus();
                    self.save_cameras();
                }
            }
            Target::CameraSettings => self.open_camera_settings(),
            Target::CameraLive => {
                gallery.live = !gallery.live;
                self.save_cameras();
            }
            Target::CameraShown(id) => {
                if let Some(camera) = gallery.cameras.iter_mut().find(|c| c.id == id) {
                    camera.hidden = !camera.hidden;
                }
                self.save_cameras();
            }
            _ => {}
        }
        vec![]
    }

    /// The keyboard takes the gallery's overlay while it is open; the gallery stays drawn
    /// behind it and comes back when the name is committed.
    fn rename_camera(&mut self, id: String) {
        let Some(Overlay::Cameras { restore }) = self.overlay.clone() else {
            return;
        };
        self.cameras.renaming = Some((id, restore));
        self.open_keyboard(TextField::CameraName);
    }

    /// Closing the keyboard by any route keeps a typed name. An empty field leaves the name
    /// unchanged (the TV keyboard cannot start with it); Reset name removes it explicitly.
    pub(crate) fn commit_camera_name(&mut self) -> Vec<Effect> {
        let draft = std::mem::take(&mut self.cameras.name_draft);
        let Some((id, restore)) = self.cameras.renaming.take() else {
            return vec![];
        };
        self.overlay = Some(Overlay::Cameras { restore });
        self.panel_started = None;
        self.focus = Target::Camera(id.clone());
        let name = clean_input_name(&draft);
        if !name.is_empty()
            && let Some(camera) = self.cameras.cameras.iter_mut().find(|c| c.id == id)
        {
            let label = (name != camera.name).then_some(name);
            if camera.label != label {
                camera.label = label;
                self.save_cameras();
            }
        }
        self.reconcile_camera_focus();
        self.dirty = true;
        vec![]
    }

    fn open_camera_settings(&mut self) {
        self.cameras.sheet = Some(Sheet::Settings);
        self.cameras.sheet_first = 0;
        self.focus = Target::CameraLive;
        self.dirty = true;
    }

    /// Hold OK (or the Menu key) on a camera opens its options.
    pub fn camera_options(&mut self) {
        let gallery = &mut self.cameras;
        if gallery.enlarged.is_some() || gallery.sheet.is_some() || gallery.moving {
            return;
        }
        if let Target::Camera(id) = &self.focus {
            gallery.sheet = Some(Sheet::Options(id.clone()));
            self.focus = gallery.options()[0].clone();
            self.dirty = true;
        }
    }

    /// Back in the gallery: ends moving, closes a sheet or the enlarged camera, then the app.
    pub fn camera_back(&mut self) -> Vec<Effect> {
        self.dirty = true;
        let gallery = &mut self.cameras;
        if std::mem::take(&mut gallery.moving) {
            return vec![];
        }
        if gallery.enlarged.is_some() {
            gallery.shrink();
            return vec![];
        }
        if let Some(sheet) = gallery.sheet.take() {
            self.focus = match sheet {
                Sheet::Options(id) => Target::Camera(id),
                Sheet::Settings => Target::CameraGear,
            };
            self.reconcile_camera_focus();
            return vec![];
        }
        self.close_cameras();
        vec![]
    }
}

#[cfg(test)]
#[path = "cameras-tests.rs"]
mod tests;
