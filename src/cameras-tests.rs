use super::*;
use crate::model::Preferences;

fn jpeg(color: [u8; 3], width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb(color));
    let mut bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Jpeg)
        .unwrap();
    bytes.into_inner()
}

/// Hands out a body in small pieces, as a network read would.
struct Chunks(Vec<u8>, usize);
impl Read for Chunks {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let n = buffer.len().min(7).min(self.0.len() - self.1);
        buffer[..n].copy_from_slice(&self.0[self.1..self.1 + n]);
        self.1 += n;
        Ok(n)
    }
}

#[test]
fn motion_jpeg_parts_split_with_or_without_content_length() {
    let red = jpeg([255, 0, 0], 8, 8);
    let blue = jpeg([0, 0, 255], 8, 8);
    let mut body = Vec::new();
    body.extend(
        format!(
            "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
            red.len()
        )
        .bytes(),
    );
    body.extend(&red);
    body.extend(b"\r\n--frame\r\nContent-Type: image/jpeg\r\n\r\n");
    body.extend(&blue);
    body.extend(b"\r\n--frame--\r\n");
    let mut parts = Parts::new(Chunks(body, 0));
    assert_eq!(parts.next_frame().unwrap().as_deref(), Some(&red[..]));
    assert_eq!(parts.next_frame().unwrap().as_deref(), Some(&blue[..]));
    assert_eq!(parts.next_frame().unwrap(), None);
}

#[test]
fn a_slow_reader_skips_to_the_newest_buffered_frame() {
    let frames: Vec<_> = (0..3).map(|i| jpeg([i * 80, 0, 0], 8, 8)).collect();
    let mut body = Vec::new();
    for frame in &frames {
        body.extend(b"--f\r\n\r\n");
        body.extend(frame);
    }
    // One large read holds all three frames: only the last is decoded.
    let mut parts = Parts::new(Cursor::new(body));
    assert_eq!(parts.next_frame().unwrap().as_deref(), Some(&frames[2][..]));
    assert_eq!(parts.next_frame().unwrap(), None);
}

#[test]
fn frames_shrink_to_their_drawn_size_and_garbage_is_rejected() {
    let big = decode(&jpeg([0, 200, 0], 1280, 720), (640, 360)).unwrap();
    assert_eq!(big.dimensions(), (640, 360));
    let small = decode(&jpeg([0, 200, 0], 320, 180), (640, 360)).unwrap();
    assert_eq!(small.dimensions(), (320, 180));
    assert!(decode(b"not an image", (640, 360)).is_none());
}

#[test]
fn layouts_keep_sixteen_by_nine_tiles_inside_the_grid() {
    for count in 1..=32 {
        let layout = Layout::new(count);
        let (w, h) = layout.tile;
        assert!((w / h - 16. / 9.).abs() < 0.02, "{count}: {w}x{h}");
        let last_visible = (layout.visible_rows * layout.cols).min(count) - 1;
        let first = layout.frame(0, 0);
        let last = layout.frame(last_visible, 0);
        assert!(first.x >= GRID.x && first.y >= GRID.y, "{count}");
        assert!(last.x + last.w <= GRID.x + GRID.w + 0.5, "{count}");
        assert!(
            last.y + last.h + CAPTION <= GRID.y + GRID.h + 0.5,
            "{count}"
        );
    }
    assert_eq!(Layout::new(4).visible_rows, 2);
    assert_eq!(Layout::new(12).visible_rows, 3);
    assert_eq!(Layout::new(12).rows, 3);
    assert_eq!(Layout::new(13).rows, 4);
}

fn camera(id: &str) -> Camera {
    Camera {
        id: format!("0/camera.{id}"),
        name: id.into(),
        stream: Some(format!("http://ha/stream/{id}")),
        snapshot: Some(format!("http://ha/snap/{id}")),
        hls: None,
        hidden: false,
    }
}

fn open(ids: &[&str]) -> State {
    let mut state = State::new(Preferences::default());
    // Opened from the Cameras app in the Apps grid, where Back returns.
    state.preferences.home_assistant = true;
    state.sync_custom_tiles();
    state.page = crate::state::Page::Apps;
    state.focus = Target::App("luma.cameras".into());
    state.camera_list(ids.iter().map(|id| camera(id)).collect(), true, true);
    state
}

fn cam(id: &str) -> Target {
    Target::Camera(format!("0/camera.{id}"))
}

fn order(state: &State) -> Vec<String> {
    state
        .cameras
        .cameras
        .iter()
        .map(|c| format!("{}{}", c.name, if c.hidden { "*" } else { "" }))
        .collect()
}

#[test]
fn the_gallery_opens_on_the_first_camera_and_arrows_follow_the_grid() {
    let mut state = open(&["a", "b", "c", "d", "e"]);
    assert!(state.cameras_open());
    assert_eq!(state.focus, cam("a"));
    // Five cameras in three columns: a b c / d e.
    state.direction(Direction::Right, &[]);
    state.direction(Direction::Right, &[]);
    assert_eq!(state.focus, cam("c"));
    state.direction(Direction::Right, &[]);
    assert_eq!(state.focus, cam("c"));
    state.direction(Direction::Down, &[]);
    assert_eq!(state.focus, cam("e"));
    state.direction(Direction::Up, &[]);
    state.direction(Direction::Up, &[]);
    assert_eq!(state.focus, Target::CameraGear);
    state.direction(Direction::Down, &[]);
    assert_eq!(state.focus, cam("c"));
    // OK enlarges, OK or Back returns to the grid, Back again closes the app.
    state.activate();
    assert_eq!(state.cameras.enlarged.as_deref(), Some("0/camera.c"));
    assert_eq!(state.camera_watches(true).len(), 1);
    assert!(matches!(
        state.camera_watches(true)[0].source,
        Source::Stream { .. }
    ));
    state.back();
    assert_eq!(state.cameras.enlarged, None);
    assert_eq!(state.camera_watches(true).len(), 5);
    assert!(state.camera_watches(false).is_empty());
    state.back();
    assert!(!state.cameras_open());
    assert_eq!(state.focus, Target::App("luma.cameras".into()));
}

#[test]
fn hold_ok_moves_or_hides_a_camera_and_settings_show_it_again() {
    let mut state = open(&["a", "b", "c"]);
    state.hold();
    assert_eq!(
        state.cameras.sheet,
        Some(Sheet::Options("0/camera.a".into()))
    );
    assert_eq!(state.focus, Target::CameraMove);
    state.activate();
    assert!(state.cameras.moving);
    state.direction(Direction::Right, &[]);
    state.direction(Direction::Down, &[]);
    assert_eq!(order(&state), ["b", "c", "a"]);
    assert_eq!(state.focus, cam("a"));
    let saved = state.cameras.save.take().unwrap();
    assert_eq!(
        saved["order"],
        json!(["0/camera.b", "0/camera.c", "0/camera.a"])
    );
    state.activate();
    assert!(!state.cameras.moving);
    assert_eq!(state.cameras.enlarged, None);

    // Hide b: focus takes the camera in its place.
    state.direction(Direction::Left, &[]);
    state.direction(Direction::Up, &[]);
    assert_eq!(state.focus, cam("b"));
    state.hold();
    state.direction(Direction::Down, &[]);
    assert_eq!(state.focus, Target::CameraHide);
    state.activate();
    assert_eq!(order(&state), ["b*", "c", "a"]);
    assert_eq!(state.focus, cam("c"));
    assert_eq!(
        state.cameras.save.take().unwrap()["hidden"],
        json!(["0/camera.b"])
    );
    assert_eq!(state.camera_watches(true).len(), 2);

    // The gear opens settings: live video, then every camera, hidden ones included.
    state.direction(Direction::Up, &[]);
    assert_eq!(state.focus, Target::CameraGear);
    state.activate();
    assert_eq!(state.cameras.sheet, Some(Sheet::Settings));
    assert_eq!(state.focus, Target::CameraLive);
    state.activate();
    assert!(!state.cameras.live);
    assert_eq!(state.cameras.save.take().unwrap()["live"], json!(false));
    state.direction(Direction::Down, &[]);
    assert_eq!(state.focus, Target::CameraShown("0/camera.b".into()));
    state.activate();
    assert_eq!(order(&state), ["b", "c", "a"]);
    state.back();
    assert_eq!(state.cameras.sheet, None);
    assert_eq!(state.focus, Target::CameraGear);

    // Without live video an enlarged camera polls snapshots.
    state.direction(Direction::Down, &[]);
    state.activate();
    assert!(matches!(
        state.camera_watches(true)[0].source,
        Source::Snapshots { .. }
    ));
}

#[test]
fn fresh_urls_keep_the_arrangement_and_a_lone_camera_cannot_move() {
    let mut state = open(&["a", "b"]);
    state.hold();
    state.direction(Direction::Down, &[]);
    state.activate();
    assert_eq!(order(&state), ["a*", "b"]);
    // Home Assistant rotates tokens and adds a camera: the open gallery keeps its own order and
    // puts the new camera where Home Assistant's arranged list does (here first, then after b).
    let mut fresh = vec![camera("c"), camera("b"), camera("a")];
    fresh[1].snapshot = Some("http://ha/snap/b?token=new".into());
    let fresh_b = fresh[1].clone();
    state.camera_list(fresh, true, false);
    assert_eq!(order(&state), ["c", "a*", "b"]);

    assert_eq!(
        state
            .cameras
            .camera("0/camera.b")
            .unwrap()
            .snapshot
            .as_deref(),
        Some("http://ha/snap/b?token=new")
    );
    state.camera_list(
        vec![camera("c"), camera("a"), fresh_b.clone(), camera("d")],
        true,
        false,
    );
    assert_eq!(order(&state), ["c", "a*", "b", "d"]);
    // Frames for unknown cameras are ignored; known ones are kept.
    state.camera_update(Update::Frame(Arc::new(Frame::new(
        "0/camera.x",
        RgbaImage::new(1, 1),
        false,
    ))));
    state.camera_update(Update::Frame(Arc::new(Frame::new(
        "0/camera.b",
        RgbaImage::new(1, 1),
        true,
    ))));
    assert_eq!(state.cameras.frames.len(), 1);

    let mut lone = open(&["a"]);
    lone.hold();
    assert_eq!(lone.focus, Target::CameraHide);
    lone.activate();
    // Every camera hidden: the settings button holds focus.
    assert_eq!(lone.focus, Target::CameraGear);
    assert!(lone.camera_watches(true).is_empty());
    // An empty list closes the gallery.
    lone.camera_list(vec![], true, false);
    assert!(!lone.cameras_open());
}

#[test]
fn the_service_keeps_a_worker_while_a_camera_is_watched_the_same_way() {
    let mut service = Service::default();
    // Nothing listens here: workers fail quietly and are stopped by the next sync.
    let watch = |id: &str, stream: bool| Watch {
        id: id.into(),
        source: if stream {
            Source::Stream {
                url: "http://127.0.0.1:9/s".into(),
                fallback: None,
            }
        } else {
            Source::Snapshots {
                url: "http://127.0.0.1:9/s".into(),
                every: REFRESH,
            }
        },
        size: (640, 360),
    };
    service.sync(&[watch("a", false), watch("b", false)]);
    assert_eq!(service.watched(), ["a", "b"]);
    let first = service.workers["a"].stop.clone();
    service.sync(&[watch("a", false)]);
    assert_eq!(service.watched(), ["a"]);
    assert!(!first.load(Ordering::Acquire));
    // Enlarging switches the camera to its stream: a new worker.
    service.sync(&[watch("a", true)]);
    assert!(first.load(Ordering::Acquire));
    service.sync(&[]);
    assert!(service.watched().is_empty());
}

#[test]
fn frames_from_a_closed_gallery_never_reappear() {
    let mut state = open(&["a"]);
    let frame = |live| {
        Update::Frame(Arc::new(Frame::new(
            "0/camera.a",
            RgbaImage::new(1, 1),
            live,
        )))
    };
    state.camera_update(frame(true));
    assert_eq!(state.cameras.frames.len(), 1);
    state.back();
    assert!(!state.cameras_open());
    // A frame queued before the workers stopped arrives after the gallery closed.
    state.camera_update(frame(true));
    state.camera_update(Update::Unavailable("0/camera.a".into()));
    assert!(state.cameras.frames.is_empty());
    assert!(state.cameras.unavailable.is_empty());
    // Reopening starts without any earlier picture.
    state.camera_list(vec![camera("a")], true, true);
    assert!(state.cameras.frames.is_empty());
}

fn streaming(ids: &[&str]) -> State {
    let mut state = State::new(Preferences::default());
    state.preferences.home_assistant = true;
    state.sync_custom_tiles();
    let cameras = ids
        .iter()
        .map(|id| Camera {
            hls: Some(format!(
                "http://ha:8123/api/luma/stream/camera.{id}?token=t"
            )),
            ..camera(id)
        })
        .collect();
    state.camera_list(cameras, true, true);
    state
}

#[test]
fn an_enlarged_camera_plays_home_assistants_hls_video_over_the_mjpeg_stream() {
    let mut state = streaming(&["a", "b"]);
    state.activate();
    assert_eq!(state.cameras.video, Video::Resolving("0/camera.a".into()));
    let (id, endpoint) = state.cameras.resolve.take().unwrap();
    assert_eq!(endpoint, "http://ha:8123/api/luma/stream/camera.a?token=t");
    // While Home Assistant starts the stream, the motion-JPEG stream shows.
    assert!(matches!(
        state.camera_watches(true)[0].source,
        Source::Stream { .. }
    ));
    state.camera_update(Update::Video {
        id,
        result: Ok("http://ha:8123/api/hls/x/master_playlist.m3u8".into()),
    });
    assert_eq!(state.cameras.video, Video::Playing("0/camera.a".into()));
    assert_eq!(
        state.cameras.play.take(),
        Some((
            "http://ha:8123/api/hls/x/master_playlist.m3u8".into(),
            "a".into()
        ))
    );
    // The stream keeps running underneath: it shows until the overlay's video plays.
    assert!(matches!(
        state.camera_watches(true)[0].source,
        Source::Stream { .. }
    ));
    // Back in the overlay app leaves the enlarged camera too.
    state.camera_video_closed(false);
    assert_eq!(state.cameras.enlarged, None);
    assert_eq!(state.cameras.video, Video::Off);
    assert!(!state.cameras.stop_video);

    // Leaving the camera in Luma (here: the list dropped it) closes the video.
    state.activate();
    let (id, _) = state.cameras.resolve.take().unwrap();
    state.camera_update(Update::Video {
        id,
        result: Ok("http://ha/x.m3u8".into()),
    });
    state.camera_list(
        vec![streaming(&["b"]).cameras.cameras[0].clone()],
        true,
        false,
    );
    assert_eq!(state.cameras.enlarged, None);
    assert!(state.cameras.stop_video);
}

#[test]
fn a_camera_whose_video_fails_stays_on_the_mjpeg_stream() {
    let mut state = streaming(&["a"]);
    state.activate();
    let (id, _) = state.cameras.resolve.take().unwrap();
    state.camera_update(Update::Video {
        id: id.clone(),
        result: Ok("http://ha/x.m3u8".into()),
    });
    // The overlay app could not play it: the enlarged camera stays, on the motion-JPEG stream.
    state.camera_video_closed(true);
    assert_eq!(state.cameras.enlarged.as_deref(), Some(id.as_str()));
    assert!(matches!(
        state.camera_watches(true)[0].source,
        Source::Stream { .. }
    ));
    // Enlarging it again does not try the video again during this visit.
    state.back();
    state.activate();
    assert!(state.cameras.resolve.is_none());
    assert_eq!(state.cameras.video, Video::Off);
    // A stream Home Assistant cannot start counts the same; so does a late answer.
    let mut other = streaming(&["b"]);
    other.activate();
    let (id, _) = other.cameras.resolve.take().unwrap();
    other.camera_update(Update::Video {
        id: id.clone(),
        result: Err("does not support play stream service".into()),
    });
    assert_eq!(other.cameras.video_failed, std::slice::from_ref(&id));
    other.camera_update(Update::Video {
        id,
        result: Ok("http://ha/late.m3u8".into()),
    });
    assert!(other.cameras.play.is_none());
    // Without live video, an enlarged camera never asks for it.
    let mut still = streaming(&["c"]);
    still.cameras.live = false;
    still.activate();
    assert!(still.cameras.resolve.is_none());
}

#[test]
fn the_hls_endpoint_answers_with_a_playlist_on_the_same_server() {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        for body in [
            r#"{"url": "/api/hls/abc/master_playlist.m3u8"}"#,
            r#"{"url": "//evil.example/x.m3u8"}"#,
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    let endpoint = format!("http://127.0.0.1:{port}/api/luma/stream/camera.a?token=t");
    assert_eq!(
        hls_address(&agent(), &endpoint).unwrap(),
        format!("http://127.0.0.1:{port}/api/hls/abc/master_playlist.m3u8")
    );
    // Only a path on the same server is accepted.
    assert!(hls_address(&agent(), &endpoint).is_err());
    server.join().unwrap();
    assert_eq!(
        origin("https://ha.local:8123/a/b?c"),
        "https://ha.local:8123"
    );
}
