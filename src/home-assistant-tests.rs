use super::*;
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
                authorization: authorization.as_deref(),
                origin: None,
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
        authorization: Some(&authorization),
        origin,
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
    assert!(!h.tv.calls().contains(&"pause".to_owned()));
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
                authorization: None,
                origin: Some("null"),
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
            authorization: None,
            origin: Some("null"),
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

#[test]
fn cameras_are_validated_and_open_as_a_gallery_refreshed_with_new_urls() {
    let mut h = Harness::new();
    // Before pairing, the Cameras app explains what is missing instead of launching anything.
    h.api.show_cameras(h.now);
    assert_eq!(h.events.try_recv(), Ok(Event::Notice("camerasNotPaired")));
    let token = h.pair();
    let _ = h.events.try_iter().count();
    h.api.show_cameras(h.now);
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
    assert!(h.tv.calls().iter().all(|call| !call.starts_with("launch")));

    h.api.show_cameras(h.now);
    let launches: Vec<_> =
        h.tv.calls()
            .into_iter()
            .filter(|call| call.starts_with("launch "))
            .collect();
    assert_eq!(launches.len(), 1);
    let params: Value = serde_json::from_str(&launches[0]["launch ".len()..]).unwrap();
    assert_eq!(params["mode"], "cameras");
    assert_eq!(params["port"], PORT);
    assert_eq!(params["cameras"][0]["name"], "Front door");
    assert_eq!(params["cameras"][1]["name"], "camera.yard");
    assert_eq!(params["cameras"][1]["stream"], Value::Null);
    let session = params["session"].as_str().unwrap().to_owned();

    // Rotated tokens relaunch the open gallery with the same session; an unchanged list does not.
    h.call("POST", "/api/cameras", Some(&token), list("a"));
    h.call("POST", "/api/cameras", Some(&token), list("b"));
    let launches: Vec<_> =
        h.tv.calls()
            .into_iter()
            .filter(|call| call.starts_with("launch "))
            .collect();
    assert_eq!(launches.len(), 2);
    let params: Value = serde_json::from_str(&launches[1]["launch ".len()..]).unwrap();
    assert_eq!(params["session"], session.as_str());
    assert!(
        params["cameras"][0]["snapshot"]
            .as_str()
            .unwrap()
            .ends_with("token=b")
    );

    // Once the page reports closing, new lists no longer relaunch it.
    let nonce = params["nonce"].as_str().unwrap().to_owned();
    let body = json!({"session": session, "nonce": nonce}).to_string();
    let mut closed = h.api.handle(
        &Request {
            method: "POST",
            path: "/api/overlay/closed",
            authorization: None,
            origin: Some("null"),
            peer: LOOPBACK,
            body: body.as_bytes(),
        },
        h.now,
    );
    assert_eq!(closed.body, json!({"ended": true}));
    h.call("POST", "/api/cameras", Some(&token), list("c"));
    assert_eq!(
        h.tv.calls()
            .iter()
            .filter(|call| call.starts_with("launch "))
            .count(),
        2
    );

    // Only the first cameras are kept.
    let many: Vec<_> = (0..40)
        .map(|i| json!({"id": format!("camera.c{i}"), "snapshot_url": "http://ha/s"}))
        .collect();
    closed = h.call(
        "POST",
        "/api/cameras",
        Some(&token),
        json!({"cameras": many}),
    );
    assert_eq!(closed.body, json!({"cameras": MAX_CAMERAS}));
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
    // A second push from one server replaces only its own list.
    let again = h.call(
        "POST",
        "/api/cameras",
        Some(&second),
        json!({"cameras": [camera("camera.b3")]}),
    );
    assert_eq!(again.body, json!({"cameras": 2}));
    h.call("DELETE", "/api/pair", Some(&first), json!({}));
    let ids: Vec<_> = h.api.all_cameras().iter().map(|c| c.id.clone()).collect();
    assert_eq!(ids, ["camera.b3"]);
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
