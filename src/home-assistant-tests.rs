use super::*;
use crate::cameras::Closed;
use std::sync::Mutex;

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<String>>,
    playing: Mutex<Option<String>>,
}

impl Recorder {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

impl Tv for Recorder {
    fn prepare_overlay(&self) -> Result<(), String> {
        self.record("prepare".into());
        Ok(())
    }
    fn launch_overlay(&self, params: &Value) -> Result<(), String> {
        self.record(format!("launch {params}"));
        Ok(())
    }
    fn close_overlay(&self) -> Result<(), String> {
        self.record("close".into());
        Ok(())
    }
    fn pause_media(&self) -> Result<Option<String>, String> {
        self.record("pause".into());
        Ok(self.playing.lock().unwrap().take())
    }
    fn resume_media(&self, id: &str) -> Result<(), String> {
        self.record(format!("resume {id}"));
        Ok(())
    }
    fn toast(&self, message: &str) -> Result<(), String> {
        self.record(format!("toast {message}"));
        Ok(())
    }
}

const LAN: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

struct Harness {
    api: Api<Recorder>,
    tv: Arc<Recorder>,
    events: mpsc::Receiver<Event>,
    now: Instant,
}

impl Harness {
    fn new() -> Self {
        let tv = Arc::new(Recorder::default());
        let (sender, events) = mpsc::channel();
        let api = Api::new(tv.clone(), None, sender).unwrap();
        api.overlay_ready.store(true, Ordering::Release);
        Self {
            api,
            tv,
            events,
            now: Instant::now(),
        }
    }

    fn call(&mut self, method: &str, path: &str, token: Option<&str>, body: Value) -> Reply {
        let authorization = token.map(|token| format!("Bearer {token}"));
        let body = body.to_string();
        self.api.handle(
            &Request {
                method,
                path,
                query: "",
                authorization: authorization.as_deref(),
                origin: None,
                integration: None,
                peer: LAN,
                body: body.as_bytes(),
            },
            self.now,
        )
    }

    fn code(&self) -> String {
        self.events
            .try_iter()
            .find_map(|event| match event {
                Event::PairingCode(code) => Some(code),
                _ => None,
            })
            .expect("a pairing code")
    }

    fn pair(&mut self) -> String {
        let started = self.call("POST", "/api/pair", None, json!({"name":"HA"}));
        assert_eq!(started.status, 200, "{:?}", started.body);
        let id = started.body["pairing_id"].as_str().unwrap().to_owned();
        let code = self.code();
        let confirmed = self.call(
            "POST",
            "/api/pair/confirm",
            None,
            json!({"pairing_id": id, "code": code}),
        );
        assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);
        assert_eq!(confirmed.body["id"], self.api.store().device_id.as_str());
        confirmed.body["token"].as_str().unwrap().to_owned()
    }
}

fn camera() -> Value {
    json!({"stream_url":"http://ha.local:8123/api/camera_proxy_stream/camera.door?token=abc","title":"Front door"})
}

#[test]
fn pairing_shows_a_code_on_the_tv_and_stores_only_a_token_digest() {
    let mut h = Harness::new();
    let token = h.pair();
    assert_eq!(token.len(), 64);
    let store = h.api.store();
    assert_eq!(store.device_id.len(), 32);
    assert_eq!(store.clients.len(), 1);
    assert_eq!(store.clients[0].name, "HA");
    assert_eq!(store.clients[0].token, digest_hex(&token));
    assert!(!store.clients[0].token.contains(&token));
    assert!(h.tv.calls().iter().any(
        |call| call.starts_with("toast Home Assistant pairing code: ")
            && call.len() == "toast Home Assistant pairing code: ".len() + 6
    ));
    assert_eq!(
        h.call("GET", "/api/status", Some(&token), json!({})).status,
        200
    );
}

#[test]
fn pairing_codes_expire_and_lock_out_after_repeated_guesses() {
    let mut h = Harness::new();
    let started = h.call("POST", "/api/pair", None, json!({}));
    let id = started.body["pairing_id"].as_str().unwrap().to_owned();
    let code = h.code();
    let wrong = if code == "000000" { "000001" } else { "000000" };
    for _ in 0..PAIRING_ATTEMPTS - 1 {
        let reply = h.call(
            "POST",
            "/api/pair/confirm",
            None,
            json!({"pairing_id": id, "code": wrong}),
        );
        assert_eq!(reply.status, 403);
    }
    assert_eq!(
        h.call(
            "POST",
            "/api/pair/confirm",
            None,
            json!({"pairing_id": id, "code": wrong})
        )
        .body["error"],
        "Too many wrong codes; start again"
    );
    // The correct code no longer works once the request is locked out.
    let reply = h.call(
        "POST",
        "/api/pair/confirm",
        None,
        json!({"pairing_id": id, "code": code}),
    );
    assert_eq!(reply.status, 404);

    assert_eq!(h.call("POST", "/api/pair", None, json!({})).status, 429);
    h.now += PAIRING_COOLDOWN;
    let started = h.call("POST", "/api/pair", None, json!({}));
    let id = started.body["pairing_id"].as_str().unwrap().to_owned();
    let code = h.code();
    h.now += PAIRING_TTL;
    let reply = h.call(
        "POST",
        "/api/pair/confirm",
        None,
        json!({"pairing_id": id, "code": code}),
    );
    assert_eq!(reply.status, 404);
    assert!(h.api.store().clients.is_empty());
}

#[test]
fn protected_endpoints_need_a_paired_token_and_reject_browsers_and_remote_peers() {
    let mut h = Harness::new();
    assert_eq!(h.call("POST", "/api/overlay", None, camera()).status, 401);
    assert_eq!(
        h.call("POST", "/api/overlay", Some("guess"), camera())
            .status,
        401
    );
    let token = h.pair();
    let authorization = format!("Bearer {token}");
    let body = camera().to_string();
    let request = |origin, peer| Request {
        method: "POST",
        path: "/api/overlay",
        query: "",
        authorization: Some(&authorization),
        origin,
        integration: None,
        peer,
        body: body.as_bytes(),
    };
    let browser = h
        .api
        .handle(&request(Some("http://evil.example"), LAN), h.now);
    assert_eq!(browser.status, 403);
    let internet = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
    assert_eq!(h.api.handle(&request(None, internet), h.now).status, 403);
    assert!(!h.tv.calls().iter().any(|call| call.starts_with("launch")));

    assert_eq!(
        h.call("DELETE", "/api/pair", Some(&token), json!({}))
            .status,
        200
    );
    assert_eq!(
        h.call("POST", "/api/overlay", Some(&token), camera())
            .status,
        401
    );
}

#[test]
fn overlay_requests_are_validated_before_anything_reaches_the_tv() {
    let mut h = Harness::new();
    let token = h.pair();
    for body in [
        json!({}),
        json!({"stream_url":"javascript:alert(1)"}),
        json!({"stream_url":"file:///etc/passwd"}),
        json!({"stream_url":"http:///no-host"}),
        json!({"stream_url":"http://ha/x\" onerror=\"x"}),
        json!({"snapshot_url":"http://ha/s.jpg","duration":2}),
        json!({"snapshot_url":"http://ha/s.jpg","refresh":0}),
        json!({"snapshot_url":"http://ha/s.jpg","position":"center"}),
        json!({"snapshot_url":"http://ha/s.jpg","pause":"yes"}),
        json!({"snapshot_url":"http://ha/s.jpg","title":"x".repeat(81)}),
        json!({"snapshot_url":"http://ha/s.jpg","title":"line\nbreak"}),
        json!({"snapshot_url":"http://ha/s.jpg","hls_url":"javascript:alert(1)"}),
        json!({"hls_url":"http://ha/api/hls/x/master_playlist.m3u8"}),
    ] {
        let reply = h.call("POST", "/api/overlay", Some(&token), body.clone());
        assert_eq!(reply.status, 400, "{body} → {:?}", reply.body);
    }
    assert!(!h.tv.calls().iter().any(|call| call.starts_with("launch")));
}

#[test]
fn showing_launches_the_overlay_with_defaults_and_a_session_nonce() {
    let mut h = Harness::new();
    let token = h.pair();
    let reply = h.call("POST", "/api/overlay", Some(&token), camera());
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    assert_eq!(reply.body["paused"], false);
    let launch =
        h.tv.calls()
            .into_iter()
            .find_map(|call| call.strip_prefix("launch ").map(str::to_owned))
            .unwrap();
    let params: Value = serde_json::from_str(&launch).unwrap();
    assert_eq!(params["session"], reply.body["session"]);
    assert_eq!(params["nonce"].as_str().unwrap().len(), 32);
    assert_eq!(params["port"], PORT);
    assert_eq!(params["title"], "Front door");
    assert_eq!(params["position"], "top-right");
    assert_eq!(params["size"], "medium");
    assert_eq!(params["duration"], 30.);
    assert!(params["snapshot"].is_null());
    assert!(params["hls"].is_null());
    assert!(!h.tv.calls().contains(&"pause".to_owned()));
}

#[test]
fn a_card_with_an_hls_playlist_passes_it_to_the_overlay() {
    let mut h = Harness::new();
    let token = h.pair();
    let mut body = camera();
    body["hls_url"] = json!("http://ha.local:8123/api/hls/abc/master_playlist.m3u8");
    let reply = h.call("POST", "/api/overlay", Some(&token), body);
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    let launch =
        h.tv.calls()
            .into_iter()
            .find_map(|call| call.strip_prefix("launch ").map(str::to_owned))
            .unwrap();
    let params: Value = serde_json::from_str(&launch).unwrap();
    assert_eq!(
        params["hls"],
        "http://ha.local:8123/api/hls/abc/master_playlist.m3u8"
    );
    // The motion-JPEG stays as the card's picture until the video plays.
    assert_eq!(
        params["stream"],
        "http://ha.local:8123/api/camera_proxy_stream/camera.door?token=abc"
    );
    assert!(params.get("mode").is_none());
}

#[test]
fn pausing_resumes_once_when_the_overlay_reports_closing() {
    let mut h = Harness::new();
    let token = h.pair();
    *h.tv.playing.lock().unwrap() = Some("_media1".into());
    let mut body = camera();
    body["pause"] = json!(true);
    let reply = h.call("POST", "/api/overlay", Some(&token), body.clone());
    assert_eq!(reply.body["paused"], true);
    // A second ring while showing replaces the overlay and keeps the original paused media.
    let second = h.call("POST", "/api/overlay", Some(&token), body);
    assert_eq!(second.body["paused"], true);
    assert_eq!(
        h.tv.calls().iter().filter(|call| *call == "pause").count(),
        1
    );
    let launch =
        h.tv.calls()
            .into_iter()
            .rev()
            .find_map(|call| call.strip_prefix("launch ").map(str::to_owned))
            .unwrap();
    let params: Value = serde_json::from_str(&launch).unwrap();
    let closed = json!({"session": params["session"], "nonce": params["nonce"]}).to_string();
    let report = |peer, nonce: &str| {
        let body = json!({"session": params["session"], "nonce": nonce}).to_string();
        (peer, body)
    };
    // From the LAN, or with the wrong nonce, the report is refused or ignored.
    for (peer, body) in [
        report(LAN, params["nonce"].as_str().unwrap()),
        report(LOOPBACK, "x"),
    ] {
        h.api.handle(
            &Request {
                method: "POST",
                path: "/api/overlay/closed",
                query: "",
                authorization: None,
                origin: Some("null"),
                integration: None,
                peer,
                body: body.as_bytes(),
            },
            h.now,
        );
    }
    assert!(!h.tv.calls().iter().any(|call| call.starts_with("resume")));
    let reply = h.api.handle(
        &Request {
            method: "POST",
            path: "/api/overlay/closed",
            query: "",
            authorization: None,
            origin: Some("null"),
            integration: None,
            peer: LOOPBACK,
            body: closed.as_bytes(),
        },
        h.now,
    );
    assert_eq!(reply.body["ended"], true);
    h.now += Duration::from_secs(3600);
    h.api.tick(h.now);
    assert_eq!(
        h.tv.calls()
            .iter()
            .filter(|call| *call == "resume _media1")
            .count(),
        1
    );
}

#[test]
fn an_overlay_that_never_reports_back_is_closed_and_resumed_after_its_duration() {
    let mut h = Harness::new();
    let token = h.pair();
    *h.tv.playing.lock().unwrap() = Some("_media2".into());
    let mut body = camera();
    body["pause"] = json!(true);
    body["duration"] = json!(10);
    h.call("POST", "/api/overlay", Some(&token), body);
    h.api.tick(h.now + Duration::from_secs(10));
    assert!(!h.tv.calls().iter().any(|call| call.starts_with("resume")));
    h.api.tick(h.now + Duration::from_secs(10) + SESSION_GRACE);
    let calls = h.tv.calls();
    assert_eq!(&calls[calls.len() - 2..], ["close", "resume _media2"]);
}

#[test]
fn dismiss_closes_and_resumes_and_notify_sends_a_toast() {
    let mut h = Harness::new();
    let token = h.pair();
    *h.tv.playing.lock().unwrap() = Some("_media3".into());
    let mut body = camera();
    body["pause"] = json!(true);
    h.call("POST", "/api/overlay", Some(&token), body);
    assert_eq!(
        h.call("DELETE", "/api/overlay", Some(&token), json!({}))
            .status,
        200
    );
    let calls = h.tv.calls();
    assert_eq!(&calls[calls.len() - 2..], ["close", "resume _media3"]);
    // Dismissing with nothing showing is harmless.
    h.call("DELETE", "/api/overlay", Some(&token), json!({}));
    assert_eq!(h.tv.calls().len(), calls.len());

    let reply = h.call(
        "POST",
        "/api/notify",
        Some(&token),
        json!({"message":"Washer finished"}),
    );
    assert_eq!(reply.status, 200);
    assert_eq!(h.tv.calls().last().unwrap(), "toast Washer finished");
    assert_eq!(
        h.call("POST", "/api/notify", Some(&token), json!({}))
            .status,
        400
    );
}

/// The newest camera list sent to Luma: (camera IDs with `*` for hidden, live, open).
fn sent_cameras(h: &Harness) -> Option<(Vec<String>, bool, bool)> {
    h.events.try_iter().fold(None, |last, event| match event {
        Event::Cameras {
            cameras,
            live,
            open,
        } => Some((
            cameras
                .iter()
                .map(|c| format!("{}{}", c.id, if c.hidden { "*" } else { "" }))
                .collect(),
            live,
            open,
        )),
        _ => last,
    })
}

#[test]
fn cameras_are_validated_and_sent_to_the_gallery_with_new_urls() {
    let mut h = Harness::new();
    // Before pairing, the Cameras app explains what is missing instead of opening.
    h.api.show_cameras();
    assert_eq!(h.events.try_recv(), Ok(Event::Notice("camerasNotPaired")));
    let token = h.pair();
    let _ = h.events.try_iter().count();
    h.api.show_cameras();
    assert_eq!(h.events.try_recv(), Ok(Event::Notice("camerasNone")));

    for bad in [
        json!({}),
        json!({"cameras": "camera.door"}),
        json!({"cameras": [{"name": "No ID", "stream_url": "http://ha/x"}]}),
        json!({"cameras": [{"id": "camera.door"}]}),
        json!({"cameras": [{"id": "camera.door", "stream_url": "file:///etc/passwd"}]}),
    ] {
        assert_eq!(
            h.call("POST", "/api/cameras", Some(&token), bad).status,
            400
        );
    }
    assert_eq!(
        h.call("POST", "/api/cameras", None, json!({"cameras": []}))
            .status,
        401
    );
    let list = |token: &str| {
        json!({"cameras": [
            {"id": "camera.door", "name": "Front door",
             "stream_url": format!("http://ha:8123/api/camera_proxy_stream/camera.door?token={token}"),
             "snapshot_url": format!("http://ha:8123/api/camera_proxy/camera.door?token={token}")},
            {"id": "camera.yard", "snapshot_url": format!("http://ha:8123/api/camera_proxy/camera.yard?token={token}")},
        ]})
    };
    let stored = h.call("POST", "/api/cameras", Some(&token), list("a"));
    assert_eq!(stored.body, json!({"cameras": 2}));
    assert_eq!(
        h.call("GET", "/api/status", Some(&token), json!({})).body["cameras"],
        2
    );
    // A changed list goes to Luma, which uses it only while the gallery is open.
    assert_eq!(
        sent_cameras(&h),
        Some((
            vec!["0/camera.door".into(), "0/camera.yard".into()],
            true,
            false
        ))
    );

    h.api.show_cameras();
    let mut opened = None;
    for event in h.events.try_iter() {
        if let Event::Cameras { cameras, open, .. } = event {
            opened = Some((cameras, open));
        }
    }
    let (cameras, open) = opened.unwrap();
    assert!(open);
    assert_eq!(cameras[0].name, "Front door");
    assert_eq!(cameras[1].name, "camera.yard");
    assert_eq!(cameras[1].stream, None);
    // Luma's gallery is native: nothing is launched on the TV.
    assert!(h.tv.calls().iter().all(|call| !call.starts_with("launch")));

    // Rotated tokens send the new URLs; an unchanged list sends nothing.
    h.call("POST", "/api/cameras", Some(&token), list("a"));
    assert_eq!(sent_cameras(&h), None);
    h.call("POST", "/api/cameras", Some(&token), list("b"));
    assert!(sent_cameras(&h).is_some());
    assert!(
        h.api.gallery()[0]
            .snapshot
            .as_deref()
            .unwrap()
            .ends_with("token=b")
    );

    // Only the first cameras are kept.
    let many: Vec<_> = (0..40)
        .map(|i| json!({"id": format!("camera.c{i}"), "snapshot_url": "http://ha/s"}))
        .collect();
    let reply = h.call(
        "POST",
        "/api/cameras",
        Some(&token),
        json!({"cameras": many}),
    );
    assert_eq!(reply.body, json!({"cameras": MAX_CAMERAS}));
    // Turning Home Assistant off empties the list, which closes an open gallery.
    let _ = h.events.try_iter().count();
    h.api.forget();
    assert_eq!(sent_cameras(&h), Some((vec![], true, false)));
}

#[test]
fn each_server_keeps_its_own_cameras_and_unpairing_drops_only_them() {
    let mut h = Harness::new();
    let first = h.pair();
    h.now += PAIRING_COOLDOWN;
    let second = h.pair();
    let camera = |id: &str| json!({"id": id, "snapshot_url": format!("http://ha/{id}")});
    h.call(
        "POST",
        "/api/cameras",
        Some(&second),
        json!({"cameras": [camera("camera.b1"), camera("camera.b2")]}),
    );
    let both = h.call(
        "POST",
        "/api/cameras",
        Some(&first),
        json!({"cameras": [camera("camera.a1")]}),
    );
    assert_eq!(both.body, json!({"cameras": 3}));
    // Status counts only the asking server's cameras.
    assert_eq!(
        h.call("GET", "/api/status", Some(&first), json!({})).body["cameras"],
        1
    );
    // Pairing order, whichever server pushed last.
    let ids: Vec<_> = h.api.all_cameras().iter().map(|c| c.id.clone()).collect();
    assert_eq!(ids, ["camera.a1", "camera.b1", "camera.b2"]);
    // The gallery tells servers' cameras apart even when they share an entity ID.
    h.call(
        "POST",
        "/api/cameras",
        Some(&second),
        json!({"cameras": [camera("camera.a1")]}),
    );
    let gallery = h.api.gallery();
    assert_eq!(gallery[0].id, "0/camera.a1");
    assert_eq!(gallery[1].id, "1/camera.a1");
    // A second push from one server replaces only its own list.
    let again = h.call(
        "POST",
        "/api/cameras",
        Some(&second),
        json!({"cameras": [camera("camera.b3")]}),
    );
    assert_eq!(again.body, json!({"cameras": 2}));
    let _ = h.events.try_iter().count();
    h.call("DELETE", "/api/pair", Some(&first), json!({}));
    let ids: Vec<_> = h.api.all_cameras().iter().map(|c| c.id.clone()).collect();
    assert_eq!(ids, ["camera.b3"]);
    // Unpairing tells an open gallery.
    assert_eq!(
        sent_cameras(&h),
        Some((vec!["0/camera.b3".into()], true, false))
    );
}

#[test]
fn an_uninstalled_overlay_is_prepared_instead_of_launched() {
    let mut h = Harness::new();
    let token = h.pair();
    h.api.overlay_ready.store(false, Ordering::Release);
    let reply = h.call("POST", "/api/overlay", Some(&token), camera());
    assert_eq!(reply.status, 503);
    assert!(!h.tv.calls().iter().any(|call| call.starts_with("launch")));
}

#[test]
fn pairings_persist_privately_and_forgetting_removes_them() {
    let directory = std::env::temp_dir().join(format!("luma-ha-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("home-assistant.json");
    let tv = Arc::new(Recorder::default());
    let (sender, events) = mpsc::channel();
    let api = Api::new(tv.clone(), Some(path.clone()), sender.clone()).unwrap();
    let device = api.store().device_id.clone();
    let mut h = Harness {
        api,
        tv,
        events,
        now: Instant::now(),
    };
    let token = h.pair();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(!saved.contains(&token));
    let mut reopened = Api::new(h.tv.clone(), Some(path.clone()), sender).unwrap();
    assert_eq!(reopened.store().device_id, device);
    assert_eq!(reopened.store().clients.len(), 1);
    reopened.forget();
    assert!(reopened.store().clients.is_empty());
    let saved: Store = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(saved.clients.is_empty());
    assert_eq!(saved.device_id, device);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn the_listener_answers_info_over_http() {
    use std::io::Read;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let mut h = Harness::new();
    let client = thread::spawn(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .write_all(b"GET /api/info HTTP/1.1\r\nHost: tv\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    });
    let (stream, peer) = listener.accept().unwrap();
    serve(&mut h.api, stream, peer);
    let response = client.join().unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    let body: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["name"], "Luma");
    assert_eq!(body["api"], API_VERSION);
    assert_eq!(body["id"], h.api.store().device_id.as_str());
}

#[test]
fn status_reports_versions_and_settings_name_servers_whose_integration_is_too_old() {
    let mut h = Harness::new();
    let token = h.pair();
    let status = h.call("GET", "/api/status", Some(&token), json!({}));
    assert_eq!(status.body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(status.body["api"], API_VERSION);
    assert_eq!(status.body["min_integration"], MIN_INTEGRATION);
    // An integration from before the header is level 1, which this Luma still accepts.
    assert_eq!(h.api.store().clients[0].integration, INTEGRATION_UNSTATED);
    assert!(
        !h.events
            .try_iter()
            .any(|event| matches!(event, Event::IntegrationOutdated(_)))
    );

    let call = |h: &mut Harness, level: &str| {
        let authorization = format!("Bearer {token}");
        h.api.handle(
            &Request {
                method: "GET",
                path: "/api/status",
                query: "",
                authorization: Some(&authorization),
                origin: None,
                integration: Some(level),
                peer: LAN,
                body: b"",
            },
            h.now,
        )
    };
    h.api.min_integration = 3;
    let status = call(&mut h, "2");
    assert_eq!(status.body["min_integration"], 3);
    assert_eq!(h.api.store().clients[0].integration, 2);
    assert_eq!(
        h.events.try_recv(),
        Ok(Event::IntegrationOutdated(vec!["HA".into()]))
    );
    // Only changes are reported.
    call(&mut h, "2");
    assert!(h.events.try_recv().is_err());
    call(&mut h, "3");
    assert_eq!(h.events.try_recv(), Ok(Event::IntegrationOutdated(vec![])));
    call(&mut h, "2");
    assert_eq!(
        h.events.try_recv(),
        Ok(Event::IntegrationOutdated(vec!["HA".into()]))
    );
    // Unpairing the server clears the note.
    assert_eq!(
        h.call("DELETE", "/api/pair", Some(&token), json!({}))
            .status,
        200
    );
    assert!(
        h.events
            .try_iter()
            .any(|event| event == Event::IntegrationOutdated(vec![]))
    );
}

#[test]
fn the_gallery_arrangement_is_saved_and_applied_to_later_lists() {
    let mut h = Harness::new();
    let token = h.pair();
    let camera = |id: &str| json!({"id": id, "snapshot_url": format!("http://ha/{id}")});
    h.call(
        "POST",
        "/api/cameras",
        Some(&token),
        json!({"cameras": [camera("camera.a"), camera("camera.b"), camera("camera.c")]}),
    );
    for bad in [
        json!({"order": "camera.a"}),
        json!({"hidden": [3]}),
        json!({"live": "no"}),
        json!({"names": ["x"]}),
        json!({"names": {"0/camera.a": 3}}),
    ] {
        assert_eq!(h.api.arrange(&bad).status, 400);
    }
    // Cameras are stored by gallery ID: the server's position and the entity ID.
    let saved = h.api.arrange(
        &json!({"order": ["0/camera.c", "0/camera.a"], "hidden": ["0/camera.b"], "live": false}),
    );
    assert_eq!(saved.status, 200, "{:?}", saved.body);
    let gallery = &h.api.store().gallery;
    assert_eq!(gallery.order, ["0/camera.c", "0/camera.a"]);
    assert_eq!(gallery.hidden, ["0/camera.b"]);
    assert!(gallery.snapshots);

    // Later lists apply it; unplaced cameras follow in Home Assistant's order.
    let _ = h.events.try_iter().count();
    h.call(
        "POST",
        "/api/cameras",
        Some(&token),
        json!({"cameras": [camera("camera.a"), camera("camera.b"), camera("camera.c"), camera("camera.d")]}),
    );
    assert_eq!(
        sent_cameras(&h),
        Some((
            vec![
                "0/camera.c".into(),
                "0/camera.a".into(),
                "0/camera.b*".into(),
                "0/camera.d".into()
            ],
            false,
            false
        ))
    );

    // A camera Home Assistant stopped sending keeps its place and stays hidden for when it returns.
    h.call(
        "POST",
        "/api/cameras",
        Some(&token),
        json!({"cameras": [camera("camera.a"), camera("camera.c")]}),
    );
    assert_eq!(
        h.api
            .arrange(&json!({"order": ["0/camera.a", "0/camera.c"], "hidden": []}))
            .status,
        200
    );
    let gallery = &h.api.store().gallery;
    assert_eq!(gallery.order, ["0/camera.a", "0/camera.c"]);
    assert_eq!(gallery.hidden, ["0/camera.b"]);
    // Leaving `live` out keeps the earlier choice.
    assert!(gallery.snapshots);

    // A camera the gallery did not report (it came back in between) keeps its entries too.
    h.call(
        "POST",
        "/api/cameras",
        Some(&token),
        json!({"cameras": [camera("camera.a"), camera("camera.b"), camera("camera.c")]}),
    );
    h.api
        .arrange(&json!({"order": ["0/camera.c", "0/camera.a"], "hidden": []}));
    let gallery = &h.api.store().gallery;
    assert_eq!(gallery.order, ["0/camera.c", "0/camera.a"]);
    assert_eq!(gallery.hidden, ["0/camera.b"]);

    // The owner's names are saved, trimmed, and come back as each camera's label.
    h.api.arrange(&json!({
        "order": ["0/camera.c", "0/camera.a", "0/camera.b"],
        "names": {"0/camera.a": "  Porch ", "0/camera.b": "Garage", "0/camera.c": " "},
    }));
    let labels: Vec<_> = h
        .api
        .gallery()
        .into_iter()
        .map(|camera| (camera.id, camera.label))
        .collect();
    assert_eq!(
        labels,
        [
            ("0/camera.c".to_owned(), None),
            ("0/camera.a".to_owned(), Some("Porch".to_owned())),
            ("0/camera.b".to_owned(), Some("Garage".to_owned())),
        ]
    );
    // A name left out for a reported camera is removed; an unreported camera keeps its own.
    h.api.arrange(&json!({
        "order": ["0/camera.c", "0/camera.a"],
        "names": {},
    }));
    let names = &h.api.store().gallery.names;
    assert_eq!(names.len(), 1);
    assert_eq!(names["0/camera.b"], "Garage");
}

#[test]
fn two_servers_sharing_an_entity_id_are_arranged_separately() {
    let mut h = Harness::new();
    let first = h.pair();
    h.now += PAIRING_COOLDOWN;
    let second = h.pair();
    let door = json!({"cameras": [{"id": "camera.door", "snapshot_url": "http://ha/door"}]});
    // Only the second server has sent cameras (say, just after Luma restarted): its cameras
    // still carry its place in pairing order, so saved choices stay with them.
    h.call("POST", "/api/cameras", Some(&second), door.clone());
    assert_eq!(h.api.gallery()[0].id, "1/camera.door");
    h.call("POST", "/api/cameras", Some(&first), door);
    h.api.arrange(&json!({
        "order": ["1/camera.door", "0/camera.door"],
        "hidden": ["1/camera.door"],
        "names": {"0/camera.door": "Front", "1/camera.door": "Back"},
    }));
    let gallery: Vec<_> = h
        .api
        .gallery()
        .into_iter()
        .map(|camera| (camera.id, camera.hidden))
        .collect();
    assert_eq!(
        gallery,
        [
            ("1/camera.door".to_owned(), true),
            ("0/camera.door".to_owned(), false)
        ]
    );
    // Unpairing the first server parks its entries; the second moves up and keeps its own.
    h.call("DELETE", "/api/pair", Some(&first), json!({}));
    assert_eq!(h.api.store().gallery.hidden, ["0/camera.door"]);
    assert_eq!(h.api.store().gallery.order, ["0/camera.door"]);
    assert_eq!(
        h.api.store().gallery.names.iter().collect::<Vec<_>>(),
        [(&"0/camera.door".to_owned(), &"Back".to_owned())]
    );
    let gallery: Vec<_> = h
        .api
        .gallery()
        .into_iter()
        .map(|camera| (camera.id, camera.hidden))
        .collect();
    assert_eq!(gallery, [("0/camera.door".to_owned(), true)]);
    assert_eq!(h.api.store().gallery.parked.len(), 1);
    assert_eq!(
        h.api.store().gallery.parked[0].names["camera.door"],
        "Front"
    );
    // Turning Home Assistant off forgets every pairing but parks the arrangement. Both servers
    // paired as "HA" without an instance ID, so both arrangements are kept, not one lost.
    h.api.forget();
    let gallery = &h.api.store().gallery;
    assert!(gallery.order.is_empty() && gallery.hidden.is_empty() && gallery.names.is_empty());
    assert_eq!(gallery.parked.len(), 2);
    assert_eq!(gallery.parked[1].hidden, ["camera.door"]);
    // Which "HA" pairs again cannot be told, so it gets neither arrangement rather than
    // possibly the other server's.
    let token = pair_as(&mut h, "HA", None);
    h.call(
        "POST",
        "/api/cameras",
        Some(&token),
        json!({"cameras": [{"id": "camera.door", "snapshot_url": "http://ha/door"}]}),
    );
    assert_eq!(h.api.gallery()[0].label, None);
    assert!(!h.api.gallery()[0].hidden);
    assert_eq!(h.api.store().gallery.parked.len(), 2);
}

/// Pairs as the server `name`, optionally with a Home Assistant instance ID.
fn pair_as(h: &mut Harness, name: &str, server: Option<&str>) -> String {
    h.now += PAIRING_COOLDOWN;
    let mut body = json!({"name": name});
    if let Some(server) = server {
        body["server"] = json!(server);
    }
    let started = h.call("POST", "/api/pair", None, body);
    assert_eq!(started.status, 200, "{:?}", started.body);
    let id = started.body["pairing_id"].as_str().unwrap().to_owned();
    let code = h.code();
    let confirmed = h.call(
        "POST",
        "/api/pair/confirm",
        None,
        json!({"pairing_id": id, "code": code}),
    );
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);
    confirmed.body["token"].as_str().unwrap().to_owned()
}

#[test]
fn camera_names_and_order_come_back_when_the_same_server_pairs_again() {
    let mut h = Harness::new();
    let token = pair_as(&mut h, "Home", Some("abc"));
    let cameras = json!({"cameras": [
        {"id": "camera.a", "snapshot_url": "http://ha/a"},
        {"id": "camera.b", "snapshot_url": "http://ha/b"},
    ]});
    h.call("POST", "/api/cameras", Some(&token), cameras.clone());
    h.api.arrange(&json!({
        "order": ["0/camera.b", "0/camera.a"],
        "hidden": ["0/camera.a"],
        "names": {"0/camera.b": "Porch"},
        "live": false,
    }));
    // Home Assistant turned off and on in Luma, then the server pairs again under a new name.
    h.api.forget();
    assert!(h.api.store().gallery.snapshots, "the live setting stays");
    let other = pair_as(&mut h, "Cabin", Some("other"));
    let token = pair_as(&mut h, "Home renamed", Some("abc"));
    h.call("POST", "/api/cameras", Some(&other), json!({"cameras": []}));
    h.call("POST", "/api/cameras", Some(&token), cameras.clone());
    let gallery: Vec<_> = h
        .api
        .gallery()
        .into_iter()
        .map(|camera| (camera.id, camera.hidden, camera.label))
        .collect();
    assert_eq!(
        gallery,
        [
            ("1/camera.b".to_owned(), false, Some("Porch".to_owned())),
            ("1/camera.a".to_owned(), true, None),
        ]
    );
    assert!(h.api.store().gallery.parked.is_empty());
    // An integration that sends no instance ID is recognised by the name it pairs under.
    h.call("DELETE", "/api/pair", Some(&token), json!({}));
    let token = pair_as(&mut h, "Home renamed", None);
    h.call("POST", "/api/cameras", Some(&token), cameras);
    assert_eq!(h.api.gallery()[0].label.as_deref(), Some("Porch"));
}

#[test]
fn pairing_codes_that_lapse_are_reported() {
    let mut h = Harness::new();
    h.call("POST", "/api/pair", None, json!({"name": "HA"}));
    let _ = h.code();
    h.api.tick(h.now + PAIRING_TTL);
    assert!(
        h.events
            .try_iter()
            .any(|event| event == Event::PairingEnded)
    );
}

#[test]
fn settings_from_a_newer_luma_survive_a_save() {
    let store: Store = serde_json::from_value(json!({
        "device_id": "d",
        "clients": [{"name": "HA", "token": "t", "paired": 1, "future": [1]}],
        "gallery": {"order": [], "future_gallery": true},
        "future_store": "x",
    }))
    .unwrap();
    let saved = serde_json::to_value(&store).unwrap();
    assert_eq!(saved["future_store"], "x");
    assert_eq!(saved["gallery"]["future_gallery"], true);
    assert_eq!(saved["clients"][0]["future"], json!([1]));
}

fn video(url: &str) -> crate::cameras::VideoRequest {
    crate::cameras::VideoRequest {
        recorded: None,
        history: false,
        url: url.into(),
        title: "Door".into(),
        snapshot: Some("http://ha/s".into()),
    }
}

#[test]
fn camera_video_plays_in_the_overlay_and_reports_how_it_closed() {
    let mut h = Harness::new();
    let token = h.pair();
    let reply = h.call(
        "POST",
        "/api/cameras",
        Some(&token),
        json!({"cameras": [{"id": "camera.door", "snapshot_url": "http://ha/s",
            "hls_url": "http://ha:8123/api/luma/stream/camera.door?token=t"}]}),
    );
    assert_eq!(reply.status, 200);
    assert_eq!(
        h.api.gallery()[0].hls.as_deref(),
        Some("http://ha:8123/api/luma/stream/camera.door?token=t")
    );
    assert_eq!(
        h.call(
            "POST",
            "/api/cameras",
            Some(&token),
            json!({"cameras": [{"id": "camera.door", "snapshot_url": "http://ha/s",
                "hls_url": "javascript:alert(1)"}]}),
        )
        .status,
        400
    );
    let _ = h.events.try_iter().count();
    let launched = |h: &Harness| -> Value {
        let calls = h.tv.calls();
        let last = calls
            .iter()
            .rev()
            .find(|call| call.starts_with("launch "))
            .unwrap();
        serde_json::from_str(&last["launch ".len()..]).unwrap()
    };
    let closed = |h: &mut Harness, params: &Value, failed: bool| {
        let body =
            json!({"session": params["session"], "nonce": params["nonce"], "failed": failed})
                .to_string();
        h.api.handle(
            &Request {
                method: "POST",
                path: "/api/overlay/closed",
                query: "",
                authorization: None,
                origin: Some("null"),
                integration: None,
                peer: LOOPBACK,
                body: body.as_bytes(),
            },
            h.now,
        )
    };

    h.api.show_video(
        &video("http://ha:8123/api/hls/x/master_playlist.m3u8"),
        h.now,
    );
    let params = launched(&h);
    assert_eq!(params["mode"], "video");
    assert_eq!(
        params["url"],
        "http://ha:8123/api/hls/x/master_playlist.m3u8"
    );
    assert_eq!(params["title"], "Door");
    assert_eq!(params["snapshot"], "http://ha/s");
    assert_eq!(closed(&mut h, &params, true).body, json!({"ended": true}));
    assert_eq!(
        h.events.try_recv(),
        Ok(Event::VideoClosed(Closed {
            failed: true,
            ..Default::default()
        }))
    );

    // Home Assistant's dismiss closes doorbell cards, never a camera being watched.
    h.api.show_video(&video("http://ha/x.m3u8"), h.now);
    let calls = h.tv.calls().len();
    h.call("DELETE", "/api/overlay", Some(&token), json!({}));
    assert_eq!(h.tv.calls().len(), calls);
    assert!(h.events.try_recv().is_err());

    // Back in the player.
    h.api.show_video(&video("http://ha/x.m3u8"), h.now);
    let params = launched(&h);
    closed(&mut h, &params, false);
    assert_eq!(
        h.events.try_recv(),
        Ok(Event::VideoClosed(Closed::default()))
    );

    // A doorbell card replaces the video; the gallery leaves the camera.
    h.api.show_video(&video("http://ha/x.m3u8"), h.now);
    h.call("POST", "/api/overlay", Some(&token), camera());
    assert_eq!(
        h.events.try_recv(),
        Ok(Event::VideoClosed(Closed::default()))
    );

    // The gallery closing the video closes the overlay and resumes what the card had paused.
    h.call("DELETE", "/api/overlay", Some(&token), json!({}));
    *h.tv.playing.lock().unwrap() = Some("_media7".into());
    let mut paused = camera();
    paused["pause"] = json!(true);
    h.call("POST", "/api/overlay", Some(&token), paused);
    h.api.show_video(&video("http://ha/x.m3u8"), h.now);
    let _ = h.events.try_iter().count();
    h.api.close_video();
    let calls = h.tv.calls();
    assert_eq!(&calls[calls.len() - 2..], ["close", "resume _media7"]);
    // The gallery left the camera itself; a report now could cancel its next video.
    assert!(h.events.try_recv().is_err());

    let _ = h.events.try_iter().count();
    // Without the overlay app installed yet, the gallery hears at once that it failed.
    h.api.overlay_ready.store(false, Ordering::Release);
    h.api.show_video(&video("http://ha/x.m3u8"), h.now);
    assert_eq!(
        h.events.try_recv(),
        Ok(Event::VideoClosed(Closed {
            failed: true,
            ..Default::default()
        }))
    );
}

#[test]
fn ok_on_a_doorbell_card_keeps_it_full_screen_until_closed() {
    let mut h = Harness::new();
    let token = h.pair();
    *h.tv.playing.lock().unwrap() = Some("_media1".into());
    let mut body = camera();
    body["pause"] = json!(true);
    let reply = h.call("POST", "/api/overlay", Some(&token), body);
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    let launch =
        h.tv.calls()
            .into_iter()
            .find_map(|call| call.strip_prefix("launch ").map(str::to_owned))
            .unwrap();
    let params: Value = serde_json::from_str(&launch).unwrap();
    let report = |h: &mut Harness, path: &str, peer, nonce: &Value| {
        let body = json!({"session": params["session"], "nonce": nonce}).to_string();
        h.api.handle(
            &Request {
                method: "POST",
                path,
                query: "",
                authorization: None,
                origin: Some("null"),
                integration: None,
                peer,
                body: body.as_bytes(),
            },
            h.now,
        )
    };
    let expand = "/api/overlay/expand";
    assert_eq!(report(&mut h, expand, LAN, &params["nonce"]).status, 403);
    assert_eq!(
        report(&mut h, expand, LOOPBACK, &json!("x")).body["expanded"],
        false
    );
    assert_eq!(
        report(&mut h, expand, LOOPBACK, &params["nonce"]).body["expanded"],
        true
    );
    // Expanded, the card outlives its duration; closing resumes the paused media as before.
    h.now += Duration::from_secs(600);
    h.api.tick(h.now);
    assert!(!h.tv.calls().iter().any(|call| call.starts_with("resume")));
    assert_eq!(
        report(&mut h, "/api/overlay/closed", LOOPBACK, &params["nonce"]).body["ended"],
        true
    );
    assert!(h.tv.calls().iter().any(|call| call == "resume _media1"));
}

#[test]
fn recorded_video_loads_piece_by_piece_with_the_cameras_current_token() {
    let mut h = Harness::new();
    let token = h.pair();
    let send = |h: &mut Harness, token: &str, history: &str| {
        h.call(
            "POST",
            "/api/cameras",
            Some(token),
            json!({"cameras": [{"id": "camera.door", "snapshot_url": "http://ha/s",
                "history_url": history}]}),
        )
    };
    assert_eq!(
        send(
            &mut h,
            &token,
            "http://ha/api/luma/history/camera.door?token=a"
        )
        .status,
        200
    );
    assert_eq!(send(&mut h, &token, "file:///etc/passwd").status, 400);
    send(
        &mut h,
        &token,
        "http://ha/api/luma/history/camera.door?token=a",
    );
    assert_eq!(
        h.api.gallery()[0].history.as_deref(),
        Some("http://ha/api/luma/history/camera.door?token=a")
    );
    h.api.show_video(
        &crate::cameras::VideoRequest {
            url: String::new(),
            recorded: Some(("0/camera.door".into(), 1_800_000_000)),
            history: true,
            title: "Door".into(),
            snapshot: None,
        },
        h.now,
    );
    let calls = h.tv.calls();
    let launch = calls
        .iter()
        .rev()
        .find(|c| c.starts_with("launch "))
        .unwrap();
    let params: Value = serde_json::from_str(&launch["launch ".len()..]).unwrap();
    assert_eq!(params["mode"], "recorded");
    assert_eq!(params["start"], 1_800_000_000u64);
    let piece = |h: &mut Harness, query: String| {
        h.api.handle(
            &Request {
                method: "GET",
                path: "/api/overlay/recorded",
                query: &query,
                authorization: None,
                origin: None,
                integration: None,
                peer: LOOPBACK,
                body: b"",
            },
            h.now,
        )
    };
    let auth = format!(
        "session={}&nonce={}",
        params["session"].as_str().unwrap(),
        params["nonce"].as_str().unwrap()
    );
    // The token rotates between pieces: each redirect carries the current one.
    send(
        &mut h,
        &token,
        "http://ha/api/luma/history/camera.door?token=b",
    );
    let reply = piece(&mut h, format!("{auth}&start=100&end=400"));
    assert_eq!(reply.status, 302);
    assert_eq!(
        reply.location.as_deref(),
        Some("http://ha/api/luma/history/camera.door?token=b&start=100&end=400")
    );
    assert_eq!(
        piece(&mut h, format!("{auth}&start=100&end=10000")).status,
        400
    );
    assert_eq!(piece(&mut h, format!("{auth}&start=x&end=400")).status, 400);
    assert_eq!(
        piece(&mut h, "session=x&nonce=y&start=1&end=2".into()).status,
        403
    );
    // Only from the TV itself.
    let remote = h.api.handle(
        &Request {
            method: "GET",
            path: "/api/overlay/recorded",
            query: &format!("{auth}&start=100&end=400"),
            authorization: None,
            origin: None,
            integration: None,
            peer: LAN,
            body: b"",
        },
        h.now,
    );
    assert_eq!(remote.status, 403);

    // Left in the player: the gallery carries on from where it was.
    let body = json!({"session": params["session"], "nonce": params["nonce"], "behind": 3600.5,
        "key": "left", "paused": true})
    .to_string();
    h.api.handle(
        &Request {
            method: "POST",
            path: "/api/overlay/closed",
            query: "",
            authorization: None,
            origin: Some("null"),
            integration: None,
            peer: LOOPBACK,
            body: body.as_bytes(),
        },
        h.now,
    );
    assert_eq!(
        h.events.try_iter().last(),
        Some(Event::VideoClosed(Closed {
            failed: false,
            behind: Some(3600.5),
            key: Some("left".into()),
            paused: true,
        }))
    );
}

#[test]
fn home_assistant_sets_streaming_keys_without_ever_erasing_one() {
    let dir = std::env::temp_dir().join(format!("luma-ha-keys-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let keys_path = dir.join("streaming-keys.json");
    let mut h = Harness::new();
    let (sender, events) = mpsc::channel();
    h.api = Api::new(h.tv.clone(), Some(dir.join("home-assistant.json")), sender).unwrap();
    h.events = events;
    let token = h.pair();
    let _ = h.events.try_iter().count();
    let motn = "motn-key-0123456789abcdef";
    let tmdb = "eyJhbGciOiJIUzI1NiJ9.e30.signature";

    // Only paired servers may set keys.
    let refused = h.call("PUT", "/api/streaming-keys", None, json!({"motn": motn}));
    assert_eq!(refused.status, 401);
    assert!(!keys_path.exists());

    let saved = h.call(
        "PUT",
        "/api/streaming-keys",
        Some(&token),
        json!({"motn": motn, "tmdb": tmdb}),
    );
    assert_eq!(saved.status, 200, "{:?}", saved.body);
    assert_eq!(saved.body, json!({"motn": true, "tmdb": true}));
    let stored = crate::streaming_keys::load(&keys_path);
    assert_eq!((stored.motn.as_str(), stored.tmdb.as_str()), (motn, tmdb));
    assert!(stored.home_assistant);
    assert_eq!(
        h.events.try_recv(),
        Ok(Event::StreamingKeys(crate::streaming_keys::Status {
            motn: true,
            tmdb: true,
            home_assistant: true,
        }))
    );
    // Keys are never echoed back to Home Assistant, but status says which it holds.
    assert!(!saved.body.to_string().contains("0123456789"));
    let status = h.call("GET", "/api/status", Some(&token), json!({}));
    assert_eq!(
        status.body["streaming_keys"],
        json!({"motn": true, "tmdb": true, "home_assistant": true})
    );
    assert!(!status.body.to_string().contains("0123456789"));

    // A cleared field in Home Assistant keeps the TV's key; an unchanged push stays quiet.
    let cleared = h.call(
        "PUT",
        "/api/streaming-keys",
        Some(&token),
        json!({"motn": "", "tmdb": null}),
    );
    assert_eq!(cleared.body, json!({"motn": true, "tmdb": true}));
    assert_eq!(crate::streaming_keys::load(&keys_path), stored);
    assert!(h.events.try_recv().is_err());

    for bad in [json!({"motn": 5}), json!({"tmdb": "has spaces in it"})] {
        assert_eq!(
            h.call("PUT", "/api/streaming-keys", Some(&token), bad)
                .status,
            400
        );
    }
    assert_eq!(crate::streaming_keys::load(&keys_path), stored);
    std::fs::remove_dir_all(&dir).unwrap();
}
