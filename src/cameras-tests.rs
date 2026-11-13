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
    // Five or more: three across, two rows on screen, the rest scroll.
    assert_eq!(Layout::new(6).cols, 3);
    assert_eq!(Layout::new(6).visible_rows, 2);
    assert_eq!(Layout::new(12).cols, 3);
    assert_eq!(Layout::new(12).visible_rows, 2);
    assert_eq!(Layout::new(12).rows, 4);
    assert_eq!(Layout::new(13).rows, 5);
}

fn camera(id: &str) -> Camera {
    Camera {
        id: format!("0/camera.{id}"),
        name: id.into(),
        stream: Some(format!("http://ha/stream/{id}")),
        snapshot: Some(format!("http://ha/snap/{id}")),
        hls: None,
        history: None,
        hidden: false,
        label: None,
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
    // OK enlarges, Back returns to the grid, Back again closes the app.
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
    assert_eq!(state.focus, Target::CameraRename);
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
    assert_eq!(lone.focus, Target::CameraRename);
    lone.direction(Direction::Down, &[]);
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
fn rename_types_a_name_over_the_gallery_and_reset_restores_home_assistants() {
    let mut state = open(&["a", "b"]);
    state.direction(Direction::Right, &[]);
    state.hold();
    state.direction(Direction::Down, &[]);
    assert_eq!(state.focus, Target::CameraRename);
    state.activate();
    // The keyboard takes over; the gallery stays drawn and fed behind it.
    assert!(!state.cameras_open());
    assert!(state.cameras_visible());
    assert!(state.text_input_active());
    assert_eq!(state.camera_watches(true).len(), 2);
    state.text("  Front door ");
    state.finish_text_input();
    assert!(state.cameras_open());
    assert_eq!(state.cameras.sheet, None);
    assert_eq!(state.focus, cam("b"));
    let renamed = state.cameras.camera("0/camera.b").unwrap();
    assert_eq!(renamed.title(), "Front door");
    assert_eq!(renamed.name, "b");
    let saved = state.cameras.save.take().unwrap();
    assert_eq!(saved["names"], json!({"0/camera.b": "Front door"}));

    // A refreshed list from Home Assistant keeps the name shown.
    state.camera_list(vec![camera("a"), camera("b")], true, false);
    assert_eq!(
        state.cameras.camera("0/camera.b").unwrap().title(),
        "Front door"
    );

    // Back with nothing typed keeps the name.
    state.hold();
    state.direction(Direction::Down, &[]);
    state.activate();
    state.back();
    assert!(state.cameras_open());
    assert_eq!(state.focus, cam("b"));
    assert!(state.cameras.save.is_none());

    // Reset name is offered for a renamed camera and goes back to Home Assistant's.
    state.hold();
    assert_eq!(
        state.cameras.options(),
        [
            Target::CameraMove,
            Target::CameraRename,
            Target::CameraNameReset,
            Target::CameraHide,
            Target::CameraSettings
        ]
    );
    state.direction(Direction::Down, &[]);
    state.direction(Direction::Down, &[]);
    state.activate();
    assert_eq!(state.cameras.camera("0/camera.b").unwrap().title(), "b");
    assert_eq!(state.focus, Target::CameraRename);
    assert_eq!(state.cameras.save.take().unwrap()["names"], json!({}));

    // Going Home while renaming closes the keyboard and the gallery together.
    state.activate();
    assert!(state.text_input_active());
    state.return_home();
    assert!(!state.cameras_visible());
    assert!(!state.text_input_active());
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
        record: false,
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
        Some(VideoRequest {
            recorded: None,
            history: false,
            url: "http://ha:8123/api/hls/x/master_playlist.m3u8".into(),
            title: "a".into(),
            // The overlay shows the camera's snapshots until the video plays.
            snapshot: Some("http://ha/snap/a".into()),
        })
    );
    // The stream keeps running underneath: it shows until the overlay's video plays.
    assert!(matches!(
        state.camera_watches(true)[0].source,
        Source::Stream { .. }
    ));
    // Back in the overlay app leaves the enlarged camera too.
    state.camera_video_closed(Closed::default());
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
    state.camera_video_closed(Closed {
        failed: true,
        ..Default::default()
    });
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

#[test]
fn a_late_close_report_leaves_the_next_cameras_video_alone() {
    let mut state = streaming(&["a", "b"]);
    state.activate();
    let (id, _) = state.cameras.resolve.take().unwrap();
    state.camera_update(Update::Video {
        id,
        result: Ok("http://ha/a.m3u8".into()),
    });
    // Leave camera a and enlarge camera b while its stream starts.
    state.back();
    assert!(state.cameras.stop_video);
    state.direction(Direction::Right, &[]);
    state.activate();
    assert_eq!(state.cameras.video, Video::Resolving("0/camera.b".into()));
    // A report about a's video arriving now changes nothing.
    state.camera_video_closed(Closed::default());
    assert_eq!(state.cameras.video, Video::Resolving("0/camera.b".into()));
    assert_eq!(state.cameras.enlarged.as_deref(), Some("0/camera.b"));
}

#[test]
fn kept_frames_reach_back_two_minutes_within_their_memory() {
    let start = Instant::now();
    let mut recording = Recording::default();
    for second in 0..200 {
        recording.push("a", start + Duration::from_secs(second), &[0; 1000]);
    }
    let (oldest, newest) = recording.span().unwrap();
    assert_eq!(newest - oldest, REWIND);
    assert_eq!(recording.bytes, 121 * 1000);
    // The frame on screen at a moment is the newest one that had arrived, else the oldest.
    let at = |offset: f32| {
        let (taken, _) = recording
            .at(start + Duration::from_secs_f32(offset))
            .unwrap();
        (taken - start).as_secs()
    };
    assert_eq!(at(150.5), 150);
    assert_eq!(at(10.), 79);
    // Large frames are dropped by memory before time.
    recording.push("a", newest + Duration::from_secs(1), &vec![0; REWIND_BYTES]);
    assert_eq!(recording.frames.len(), 1);
    // Another camera starts afresh.
    recording.push("b", newest, &[0; 10]);
    assert_eq!((recording.frames.len(), recording.bytes), (1, 10));
}

#[test]
fn the_replay_thread_decodes_the_kept_frame_asked_for() {
    let mut service = Service::default();
    let start = Instant::now();
    {
        let mut recording = service.replay.lock();
        recording.push("a", start, &jpeg([255, 0, 0], 8, 8));
        recording.push(
            "a",
            start + Duration::from_secs(1),
            &jpeg([0, 0, 255], 8, 8),
        );
    }
    assert_eq!(
        service.recorded(),
        Some((start, start + Duration::from_secs(1)))
    );
    service.replay("a", Want::Kept(start + Duration::from_millis(500)));
    let Update::Replay {
        id,
        frame: Some(frame),
    } = service
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
    else {
        panic!("expected a replayed frame");
    };
    assert_eq!(id, "a");
    assert_eq!(frame.key, replay_key("a"));
    assert!(frame.pixels.get_pixel(4, 4)[0] > 200);
    // The same frame is not decoded twice.
    service.replay("a", Want::Kept(start + Duration::from_millis(900)));
    assert!(
        service
            .receiver
            .recv_timeout(Duration::from_millis(300))
            .is_err()
    );
    // Closing the enlarged camera frees its kept frames.
    service.sync(&[]);
    assert_eq!(service.recorded(), None);
}

fn protect(ids: &[&str]) -> State {
    let mut state = State::new(Preferences::default());
    state.preferences.home_assistant = true;
    state.sync_custom_tiles();
    let cameras = ids
        .iter()
        .map(|id| Camera {
            hls: Some(format!("http://ha/api/luma/stream/camera.{id}?token=t")),
            history: Some(format!("http://ha/api/luma/history/camera.{id}?token=t")),
            ..camera(id)
        })
        .collect();
    state.camera_list(cameras, true, true);
    state
}

const UNIX: u64 = 1_800_000_000;

#[test]
fn left_right_and_ok_scrub_through_the_tvs_kept_frames() {
    let mut state = open(&["a", "b"]);
    state.activate();
    assert!(state.camera_watches(true)[0].record);
    let now = Instant::now() + Duration::from_secs(600);
    let gallery = &mut state.cameras;
    // Nothing kept yet: Left stays live.
    gallery.skip(true, now);
    assert_eq!(gallery.playback, Playback::Live);
    gallery.recorded = Some((now - Duration::from_secs(90), now));
    gallery.skip(true, now);
    assert_eq!(gallery.lag(now), Duration::from_secs(10));
    // Playing behind live keeps that distance.
    let later = now + Duration::from_secs(3);
    gallery.recorded = Some((later - Duration::from_secs(90), later));
    let (want, changed) = gallery.replay_position(later, UNIX);
    assert!(changed);
    assert_eq!(want, Some(Want::Kept(later - Duration::from_secs(10))));
    // OK pauses there; time passing leaves it further behind live.
    gallery.toggle_pause(later);
    let paused = later + Duration::from_secs(20);
    assert_eq!(gallery.lag(paused), Duration::from_secs(30));
    // Held Left steps further the longer it is held, never past the oldest kept frame.
    let mut held = paused;
    for _ in 0..7 {
        gallery.skip(true, held);
        held += Duration::from_millis(200);
    }
    held -= Duration::from_millis(200);
    assert_eq!(gallery.lag(held), gallery.kept(held));
    // OK plays from there; Right up to the newest frame goes live.
    gallery.toggle_pause(paused);
    assert!(matches!(
        gallery.playback,
        Playback::Replay { speed: 1, .. }
    ));
    let right = paused + Duration::from_secs(5);
    for step in 0..12 {
        gallery.skip(false, right + Duration::from_secs(step));
    }
    assert_eq!(gallery.playback, Playback::Live);
    // Pausing live holds the newest frame; leaving the camera goes back to live.
    gallery.toggle_pause(paused);
    assert!(matches!(
        gallery.playback,
        Playback::Replay { speed: 0, .. }
    ));
    state.back();
    assert_eq!(state.cameras.playback, Playback::Live);
    assert!(state.cameras.scrub.is_none());
    assert!(!state.camera_watches(true)[0].record);
}

#[test]
fn held_scrubbing_speeds_up_and_fast_playback_ends_at_live() {
    assert_eq!(step(Duration::ZERO), Duration::from_secs(10));
    assert_eq!(step(Duration::from_secs(2)), Duration::from_secs(60));
    assert_eq!(step(Duration::from_secs(4)), Duration::from_secs(300));
    assert_eq!(step(Duration::from_secs(9)), Duration::from_secs(1800));
    let mut state = protect(&["a"]);
    state.activate();
    let now = Instant::now() + Duration::from_secs(600);
    let gallery = &mut state.cameras;
    // Protect's recordings reach a day back, whatever the TV kept.
    let mut at = now;
    for _ in 0..40 {
        gallery.skip(true, at);
        at += Duration::from_millis(200);
    }
    // 10 s for the first second, a minute for two more, then five minutes, then half hours.
    assert_eq!(
        gallery.lag(at),
        Duration::from_secs(5 * 10 + 10 * 60 + 15 * 300 + 10 * 1800)
    );
    // Rewind and fast-forward step through the speeds; OK plays at normal speed again.
    gallery.faster(true, at);
    gallery.faster(true, at);
    assert!(matches!(
        gallery.playback,
        Playback::Replay { speed: -4, .. }
    ));
    gallery.faster(false, at);
    assert!(matches!(
        gallery.playback,
        Playback::Replay { speed: 2, .. }
    ));
    for _ in 0..8 {
        gallery.faster(false, at);
    }
    assert!(matches!(
        gallery.playback,
        Playback::Replay { speed: 64, .. }
    ));
    // At 64× it catches up 63 s a second, then it is live.
    let lag = gallery.lag(at).as_secs_f64();
    assert_eq!(
        gallery.lag(at + Duration::from_secs(60)).as_secs_f64(),
        lag - 63. * 60.
    );
    gallery.replay_position(at + Duration::from_secs(400), UNIX);
    assert_eq!(gallery.playback, Playback::Live);
    // Rewinding stops at the oldest moment, paused.
    gallery.faster(true, at);
    gallery.replay_position(at + Duration::from_secs(48 * 3600), UNIX);
    assert!(matches!(
        gallery.playback,
        Playback::Replay { speed: 0, .. }
    ));
    assert_eq!(gallery.lag(at + Duration::from_secs(48 * 3600)), HISTORY);
}

#[test]
fn beyond_the_kept_frames_protect_shows_recorded_pictures_and_plays_recorded_video() {
    let mut state = protect(&["a"]);
    state.activate();
    let now = Instant::now() + Duration::from_secs(600);
    let history = "http://ha/api/luma/history/camera.a?token=t";
    // Home Assistant could not start the live video (so it shows the motion-JPEG stream).
    let (id, _) = state.cameras.resolve.take().unwrap();
    state.camera_update(Update::Video {
        id,
        result: Err("no stream".into()),
    });
    // Enlarging asks what Protect saw over the last day.
    state.camera_replay(None, now, UNIX);
    assert_eq!(
        state.cameras.fetch_marks.take(),
        Some((
            "0/camera.a".into(),
            format!("{history}&events=1&start={}&end={UNIX}", UNIX - 86400)
        ))
    );
    // Paused an hour back: Protect's picture from then.
    let gallery = &mut state.cameras;
    gallery.recorded = Some((now - Duration::from_secs(60), now));
    gallery.playback = Playback::Replay {
        lag: Duration::from_secs(3600),
        since: now,
        speed: 0,
    };
    let want = state.camera_replay(Some((now - Duration::from_secs(60), now)), now, UNIX);
    assert_eq!(
        want,
        Some((
            "0/camera.a".into(),
            Want::Recorded(format!("{history}&at={}&width=1280", UNIX - 3600))
        ))
    );
    assert!(state.cameras.play.is_none());
    // Playing at normal speed: the overlay app plays Protect's video from there.
    state.cameras.toggle_pause(now);
    state.camera_replay(Some((now - Duration::from_secs(60), now)), now, UNIX);
    let video = state.cameras.play.take().unwrap();
    assert_eq!(video.recorded, Some(("0/camera.a".into(), UNIX - 3600)));
    assert_eq!(state.cameras.video, Video::Recorded("0/camera.a".into()));
    // Left there closes it, reporting where it was; the gallery carries on 10 s earlier and
    // waits for scrubbing to settle before playing video again.
    state.camera_video_closed_at(
        Closed {
            behind: Some(3500.),
            key: Some("left".into()),
            ..Default::default()
        },
        now,
        UNIX,
    );
    assert_eq!(state.cameras.video, Video::Off);
    assert_eq!(state.cameras.enlarged.as_deref(), Some("0/camera.a"));
    assert_eq!(state.cameras.lag(now), Duration::from_secs(3510));
    state.camera_replay(None, now + Duration::from_millis(500), UNIX);
    assert!(state.cameras.play.is_none());
    state.camera_replay(None, now + Duration::from_secs(2), UNIX);
    assert!(state.cameras.play.take().is_some());
    // Video Protect cannot play: pictures instead for the rest of the visit.
    state.camera_video_closed_at(
        Closed {
            failed: true,
            ..Default::default()
        },
        now,
        UNIX,
    );
    assert_eq!(state.cameras.recorded_failed, ["0/camera.a"]);
    state.camera_replay(None, now + Duration::from_secs(5), UNIX);
    assert!(state.cameras.play.is_none());
    assert_eq!(state.cameras.enlarged.as_deref(), Some("0/camera.a"));
}

#[test]
fn up_and_down_jump_between_what_protect_saw() {
    let mut state = protect(&["a"]);
    state.activate();
    let now = Instant::now();
    let mark = |ago: u64, kind| Mark {
        start: UNIX - ago,
        end: UNIX - ago + 20,
        kind,
    };
    state.camera_update(Update::Marks {
        id: "0/camera.a".into(),
        marks: vec![mark(7200, Kind::Person), mark(600, Kind::Doorbell)],
    });
    let gallery = &mut state.cameras;
    assert_eq!(gallery.marks.len(), 2);
    // Up from live: to just before the doorbell, playing; again: the person.
    gallery.jump(true, now, UNIX);
    assert_eq!(gallery.lag(now), Duration::from_secs(602));
    assert!(matches!(
        gallery.playback,
        Playback::Replay { speed: 1, .. }
    ));
    gallery.jump(true, now, UNIX);
    assert_eq!(gallery.lag(now), Duration::from_secs(7202));
    gallery.jump(true, now, UNIX);
    assert_eq!(gallery.lag(now), Duration::from_secs(7202));
    // Down: the doorbell, then live.
    gallery.jump(false, now, UNIX);
    assert_eq!(gallery.lag(now), Duration::from_secs(602));
    gallery.jump(false, now, UNIX);
    assert_eq!(gallery.playback, Playback::Live);
    // Marks for a camera no longer enlarged are dropped.
    state.camera_update(Update::Marks {
        id: "0/camera.b".into(),
        marks: vec![],
    });
    assert_eq!(state.cameras.marks.len(), 2);
    assert_eq!(
        parse_marks(br#"{"events": [{"start": 20, "end": 30, "kind": "vehicle"}, {"start": 10, "kind": "ring"}, {"end": 1}]}"#)
            .unwrap(),
        [
            Mark { start: 10, end: 10, kind: Kind::Doorbell },
            Mark { start: 20, end: 30, kind: Kind::Vehicle },
        ]
    );
}

#[test]
fn the_live_video_hands_scrubbing_past_its_buffer_to_the_gallery() {
    let mut state = protect(&["a"]);
    state.activate();
    let (id, _) = state.cameras.resolve.take().unwrap();
    state.camera_update(Update::Video {
        id,
        result: Ok("http://ha/a.m3u8".into()),
    });
    let now = Instant::now();
    // Left at the start of what the HLS video kept: the gallery goes on from 2 minutes back.
    state.camera_video_closed_at(
        Closed {
            behind: Some(120.),
            key: Some("left".into()),
            paused: true,
            ..Default::default()
        },
        now,
        UNIX,
    );
    assert_eq!(state.cameras.enlarged.as_deref(), Some("0/camera.a"));
    assert_eq!(state.cameras.lag(now), Duration::from_secs(130));
    assert!(matches!(
        state.cameras.playback,
        Playback::Replay { speed: 0, .. }
    ));
    // The live video waits until the camera is live again.
    assert!(state.cameras.resolve.is_none());
    state.cameras.jump(false, now, UNIX);
    assert_eq!(state.cameras.playback, Playback::Live);
    assert_eq!(state.cameras.video, Video::Resolving("0/camera.a".into()));
}

#[test]
fn replayed_pictures_show_only_while_the_enlarged_camera_is_rewound() {
    let mut state = protect(&["a"]);
    state.activate();
    let replay = |id: &str, frame: bool| Update::Replay {
        id: id.into(),
        frame: frame.then(|| Arc::new(Frame::new(&replay_key(id), RgbaImage::new(1, 1), false))),
    };
    state.camera_update(replay("0/camera.a", true));
    assert!(state.cameras.replay.is_none());
    state.cameras.skip(true, Instant::now());
    state.camera_update(replay("0/camera.b", true));
    assert!(state.cameras.replay.is_none());
    state.camera_update(replay("0/camera.a", true));
    assert!(state.cameras.replay.is_some());
    // Nothing recorded then: the last picture stays, marked.
    state.camera_update(replay("0/camera.a", false));
    assert!(state.cameras.replay.is_some() && state.cameras.not_recorded);
    assert_eq!(clock(Duration::from_millis(65_900)), "-1:05");
    assert_eq!(clock(Duration::from_secs(86400)), "-24:00:00");
    assert_eq!(speed_label(-8), "-8\u{d7}");
}

#[test]
fn ok_on_recorded_video_pauses_and_back_resets_the_camera() {
    let mut state = protect(&["a", "b"]);
    state.activate();
    let (id, _) = state.cameras.resolve.take().unwrap();
    state.camera_update(Update::Video {
        id,
        result: Err("no stream".into()),
    });
    let now = Instant::now() + Duration::from_secs(600);
    state.cameras.playback = Playback::Replay {
        lag: Duration::from_secs(3600),
        since: now,
        speed: 1,
    };
    state.camera_replay(None, now, UNIX);
    assert!(state.cameras.play.take().is_some());
    // While the recorded video plays, no pictures load behind it.
    assert_eq!(state.camera_replay(None, now, UNIX), None);
    // OK in the recorded video (which was playing): paused there, and it stays closed.
    state.camera_video_closed_at(
        Closed {
            behind: Some(3600.),
            key: Some("ok".into()),
            ..Default::default()
        },
        now,
        UNIX,
    );
    assert!(matches!(
        state.cameras.playback,
        Playback::Replay { speed: 0, .. }
    ));
    state.camera_replay(None, now + Duration::from_secs(5), UNIX);
    assert!(state.cameras.play.is_none());
    // Back in the recorded video leaves the camera with nothing of where it was.
    state.cameras.toggle_pause(now + Duration::from_secs(5));
    state.camera_replay(None, now + Duration::from_secs(5), UNIX);
    assert!(state.cameras.play.take().is_some());
    state.cameras.marks = vec![Mark {
        start: UNIX,
        end: UNIX,
        kind: Kind::Person,
    }];
    state.camera_video_closed_at(Closed::default(), now, UNIX);
    assert_eq!(state.cameras.enlarged, None);
    assert_eq!(state.cameras.playback, Playback::Live);
    assert!(state.cameras.marks.is_empty() && state.cameras.replay.is_none());
    // A failure lasts only this visit.
    state.cameras.recorded_failed.push("0/camera.a".into());
    let cameras = state.cameras.cameras.clone();
    state.back();
    state.camera_list(cameras, true, true);
    assert!(state.cameras.recorded_failed.is_empty());
}

#[test]
fn a_camera_paused_past_its_oldest_kept_frame_stays_still() {
    let mut state = open(&["a"]);
    state.activate();
    let now = Instant::now() + Duration::from_secs(600);
    let gallery = &mut state.cameras;
    gallery.recorded = Some((now - Duration::from_secs(60), now));
    gallery.skip(true, now);
    gallery.toggle_pause(now);
    gallery.replay = Some(Arc::new(Frame::new("x", RgbaImage::new(1, 1), false)));
    // Still within the kept frames: the frame from then.
    assert!(matches!(
        gallery.replay_position(now, UNIX).0,
        Some(Want::Kept(_))
    ));
    // Two minutes later the frame it paused on is gone: the picture stays as it was.
    let later = now + Duration::from_secs(120);
    gallery.recorded = Some((later - Duration::from_secs(60), later));
    assert_eq!(gallery.replay_position(later, UNIX).0, None);
}

#[test]
fn up_on_the_live_video_with_nothing_earlier_stays_live() {
    let mut state = protect(&["a"]);
    state.activate();
    let (id, _) = state.cameras.resolve.take().unwrap();
    state.camera_update(Update::Video {
        id,
        result: Ok("http://ha/a.m3u8".into()),
    });
    state.camera_video_closed_at(
        Closed {
            behind: Some(2.),
            key: Some("up".into()),
            ..Default::default()
        },
        Instant::now(),
        UNIX,
    );
    assert_eq!(state.cameras.playback, Playback::Live);
    assert_eq!(state.cameras.video, Video::Resolving("0/camera.a".into()));
}

#[test]
fn up_on_recorded_video_with_nothing_earlier_stays_in_the_recordings() {
    let mut state = protect(&["a"]);
    state.activate();
    let (id, _) = state.cameras.resolve.take().unwrap();
    state.camera_update(Update::Video {
        id,
        result: Err("no stream".into()),
    });
    let now = Instant::now() + Duration::from_secs(600);
    state.cameras.playback = Playback::Replay {
        lag: Duration::from_secs(3600),
        since: now,
        speed: 1,
    };
    state.camera_replay(None, now, UNIX);
    assert!(state.cameras.play.take().is_some());
    state.camera_video_closed_at(
        Closed {
            behind: Some(3600.),
            key: Some("up".into()),
            ..Default::default()
        },
        now,
        UNIX,
    );
    assert_eq!(state.cameras.lag(now), Duration::from_secs(3600));
    assert!(state.cameras.resolve.is_none());
}
