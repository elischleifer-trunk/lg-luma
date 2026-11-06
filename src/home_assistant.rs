//! Home Assistant integration: an opt-in LAN API that paired Home Assistant servers call to show a
//! camera feed in a corner overlay above whatever is playing, or a plain system toast.
//!
//! Pairing never needs a password typed on the TV: Home Assistant asks to pair, the TV shows a
//! six-digit code, and the user types it into Home Assistant, which receives a bearer token. Only
//! SHA-256 digests of tokens are stored. Turning the setting off forgets every paired server.
//!
//! The overlay itself is the separate web app `com.ecklf.luma.overlay` (see `overlay/`), whose
//! transparent `overlay` window type keeps the foreground app playing underneath it. Luma installs
//! it from its own package, launches it with the camera details and, when asked, pauses the
//! foreground media first and resumes it once the overlay closes.
//!
//! Home Assistant also pushes its camera list (`POST /api/cameras`) whenever a camera's signed
//! proxy URLs rotate. Luma's built-in Cameras app opens the same overlay app as a full-screen
//! gallery of those cameras, relaunching it with fresh URLs while it stays open.
use crate::companion;
use ring::digest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// "LUMA" on a phone keypad.
pub const PORT: u16 = 5862;
/// Opens HACS on the owner's Home Assistant with this repository's integration ready to install.
pub const HACS_LINK: &str = "https://my.home-assistant.io/redirect/hacs_repository/?owner=elischleifer-trunk&repository=lg-luma&category=integration";
pub const OVERLAY_ID: &str = luma_tv_helper::overlay::OVERLAY;
const API_VERSION: u32 = 1;
const MAX_BODY: usize = 32 * 1024;
/// Cameras kept from Home Assistant's list; more are dropped.
pub const MAX_CAMERAS: usize = 32;
const MAX_CLIENTS: usize = 8;
const MAX_URL: usize = 4096;
const PAIRING_TTL: Duration = Duration::from_secs(120);
const PAIRING_ATTEMPTS: u8 = 5;
const PAIRING_COOLDOWN: Duration = Duration::from_secs(3);
/// The overlay dismisses itself; this margin covers launch time before Luma resumes playback.
const SESSION_GRACE: Duration = Duration::from_secs(5);
/// A gallery left open stops refreshing its URLs after this long; the page closes itself sooner.
const GALLERY_TTL: Duration = Duration::from_secs(60 * 60);

/// What the TV does for the API. `Live` calls webOS; tests substitute a recorder.
pub trait Tv: Send + Sync + 'static {
    fn prepare_overlay(&self) -> Result<(), String>;
    fn launch_overlay(&self, params: &Value) -> Result<(), String>;
    fn close_overlay(&self) -> Result<(), String>;
    /// The paused pipeline's media ID, or None when nothing was playing.
    fn pause_media(&self) -> Result<Option<String>, String>;
    fn resume_media(&self, id: &str) -> Result<(), String>;
    fn toast(&self, message: &str) -> Result<(), String>;
}

pub struct Live {
    pub webos: bool,
}

impl Tv for Live {
    fn prepare_overlay(&self) -> Result<(), String> {
        if !self.webos {
            return Ok(());
        }
        crate::platform::overlay_helper("install", None).map(|_| ())
    }
    fn launch_overlay(&self, params: &Value) -> Result<(), String> {
        if !self.webos {
            eprintln!("Luma Home Assistant overlay (desktop preview): {params}");
            return Ok(());
        }
        crate::platform::launch_with_params(OVERLAY_ID, params)
    }
    fn close_overlay(&self) -> Result<(), String> {
        if !self.webos {
            return Ok(());
        }
        crate::platform::overlay_helper("close", None).map(|_| ())
    }
    fn pause_media(&self) -> Result<Option<String>, String> {
        if !self.webos {
            return Ok(None);
        }
        let reply = crate::platform::overlay_helper("pause", None)?;
        Ok(reply["mediaId"].as_str().map(str::to_owned))
    }
    fn resume_media(&self, id: &str) -> Result<(), String> {
        if !self.webos {
            return Ok(());
        }
        crate::platform::overlay_helper("resume", Some(id)).map(|_| ())
    }
    fn toast(&self, message: &str) -> Result<(), String> {
        if !self.webos {
            eprintln!("Luma Home Assistant toast (desktop preview): {message}");
            return Ok(());
        }
        crate::platform::toast(message)
    }
}

/// Reported to the SDL thread, which shows them as Luma notices when Luma is in front.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Listening(String),
    PairingCode(String),
    Paired(String),
    Failed(String),
    /// A translation key to show as a Luma notice.
    Notice(&'static str),
}

/// One Home Assistant camera, with URLs the TV can load directly.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Camera {
    pub id: String,
    pub name: String,
    pub stream: Option<String>,
    pub snapshot: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Store {
    /// Stable per installation, so Home Assistant recognises a TV whose address changed.
    pub device_id: String,
    pub clients: Vec<Client>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Client {
    pub name: String,
    /// Hex SHA-256 of the bearer token.
    pub token: String,
    pub paired: u64,
}

struct Pairing {
    id: String,
    code: String,
    name: String,
    expires: Instant,
    attempts: u8,
}

struct Session {
    id: String,
    nonce: String,
    media: Option<String>,
    ends: Instant,
    /// The Cameras gallery rather than a doorbell card; refreshed when new URLs arrive.
    gallery: bool,
}

pub struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub authorization: Option<&'a str>,
    pub origin: Option<&'a str>,
    pub peer: IpAddr,
    pub body: &'a [u8],
}

#[derive(Debug, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
}

fn reply(status: u16, body: Value) -> Reply {
    Reply { status, body }
}

fn error(status: u16, message: &str) -> Reply {
    reply(status, json!({"error": message}))
}

fn digest_hex(token: &str) -> String {
    digest::digest(&digest::SHA256, token.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut bytes = [0; N];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| format!("Secure random source unavailable: {e}"))?;
    Ok(bytes)
}

fn random_hex(bytes: usize) -> Result<String, String> {
    let random = random_bytes::<32>()?;
    Ok(random[..bytes.min(32)]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Six uniformly distributed decimal digits.
fn pairing_code() -> Result<String, String> {
    loop {
        let value = u32::from_le_bytes(random_bytes::<4>()?);
        // 4_294_000_000 is the largest multiple of 1_000_000 below 2^32, avoiding modulo bias.
        if value < 4_294_000_000 {
            return Ok(format!("{:06}", value % 1_000_000));
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn text(body: &Value, key: &str, max: usize) -> Result<Option<String>, Reply> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.chars().count() > max || value.chars().any(char::is_control) {
                Err(error(
                    400,
                    &format!("{key} must be at most {max} plain characters"),
                ))
            } else if value.is_empty() {
                Ok(None)
            } else {
                Ok(Some(value.to_owned()))
            }
        }
        Some(_) => Err(error(400, &format!("{key} must be a string"))),
    }
}

/// Camera URLs are loaded by the overlay page; only absolute http(s) URLs without whitespace.
fn camera_url(body: &Value, key: &str) -> Result<Option<String>, Reply> {
    let Some(url) = text(body, key, MAX_URL)? else {
        return Ok(None);
    };
    let lower = url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"));
    if rest.is_none_or(|rest| rest.is_empty() || rest.starts_with('/'))
        || url
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '<' | '>' | '\\'))
    {
        return Err(error(400, &format!("{key} must be an http or https URL")));
    }
    Ok(Some(url))
}

fn number(body: &Value, key: &str, default: f64, min: f64, max: f64) -> Result<f64, Reply> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_f64()
            .filter(|value| (min..=max).contains(value))
            .ok_or_else(|| error(400, &format!("{key} must be a number from {min} to {max}"))),
    }
}

fn choice(body: &Value, key: &str, allowed: &[&'static str]) -> Result<&'static str, Reply> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(allowed[0]),
        Some(value) => value
            .as_str()
            .and_then(|value| allowed.iter().find(|allowed| **allowed == value))
            .copied()
            .ok_or_else(|| error(400, &format!("{key} must be one of {}", allowed.join(", ")))),
    }
}

pub struct Api<T: Tv> {
    tv: Arc<T>,
    store: Store,
    store_path: Option<PathBuf>,
    events: mpsc::Sender<Event>,
    pairing: Option<Pairing>,
    last_pairing: Option<Instant>,
    session: Option<Session>,
    /// Each paired server's cameras, keyed by its token digest; the gallery shows them merged.
    cameras: Vec<(String, Vec<Camera>)>,
    overlay_ready: Arc<AtomicBool>,
    preparing: Arc<AtomicBool>,
}

impl<T: Tv> Api<T> {
    pub fn new(
        tv: Arc<T>,
        store_path: Option<PathBuf>,
        events: mpsc::Sender<Event>,
    ) -> Result<Self, String> {
        let mut store = store_path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Store>(&bytes).ok())
            .unwrap_or_default();
        store.clients.truncate(MAX_CLIENTS);
        let mut api = Self {
            tv,
            store,
            store_path,
            events,
            pairing: None,
            last_pairing: None,
            session: None,
            cameras: Vec::new(),
            overlay_ready: Arc::new(AtomicBool::new(false)),
            preparing: Arc::new(AtomicBool::new(false)),
        };
        if api.store.device_id.len() != 32 {
            api.store.device_id = random_hex(16)?;
            api.save();
        }
        Ok(api)
    }

    #[cfg(test)]
    pub fn store(&self) -> &Store {
        &self.store
    }

    fn save(&self) {
        let Some(path) = &self.store_path else {
            return;
        };
        let result = serde_json::to_vec_pretty(&self.store)
            .map_err(|e| e.to_string())
            .and_then(|bytes| write_private(path, &bytes));
        if let Err(error) = result {
            eprintln!("Luma Home Assistant: could not save pairings: {error}");
        }
    }

    /// Forgets every paired server, e.g. when the setting is turned off.
    pub fn forget(&mut self) {
        if self.session.is_some() {
            let _ = self.tv.close_overlay();
            self.end_session();
        }
        self.pairing = None;
        self.cameras.clear();
        if !self.store.clients.is_empty() {
            self.store.clients.clear();
            self.save();
        }
    }

    /// Installs or updates the overlay app on a background thread, once at a time.
    pub fn prepare(&self) {
        if self.overlay_ready.load(Ordering::Acquire) || self.preparing.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let (tv, ready, preparing) = (
            self.tv.clone(),
            self.overlay_ready.clone(),
            self.preparing.clone(),
        );
        let events = self.events.clone();
        thread::spawn(move || {
            match tv.prepare_overlay() {
                Ok(()) => ready.store(true, Ordering::Release),
                Err(error) => {
                    eprintln!("Luma Home Assistant overlay install: {error}");
                    let _ = events.send(Event::Failed(error));
                }
            }
            preparing.store(false, Ordering::Release);
        });
    }

    fn authorized(&self, request: &Request) -> Option<usize> {
        let token = request.authorization?.strip_prefix("Bearer ")?.trim();
        if token.is_empty() {
            return None;
        }
        let hash = digest_hex(token);
        self.store
            .clients
            .iter()
            .position(|client| client.token == hash)
    }

    /// Resumes paused media and clears the session once the overlay is gone.
    fn end_session(&mut self) {
        if let Some(session) = self.session.take()
            && let Some(media) = session.media
            && let Err(error) = self.tv.resume_media(&media)
        {
            eprintln!("Luma Home Assistant resume: {error}");
        }
    }

    /// Ends an overlay whose duration passed without the page reporting that it closed.
    pub fn tick(&mut self, now: Instant) {
        if self
            .session
            .as_ref()
            .is_some_and(|session| now >= session.ends)
        {
            let _ = self.tv.close_overlay();
            self.end_session();
        }
        if self.pairing.as_ref().is_some_and(|p| now >= p.expires) {
            self.pairing = None;
        }
    }

    pub fn handle(&mut self, request: &Request, now: Instant) -> Reply {
        if !companion::local_peer(request.peer) {
            return error(403, "Only devices on the TV's local network may connect");
        }
        let closed = request.path == "/api/overlay/closed";
        // Browsers send Origin; Home Assistant does not. Only the overlay page itself, a local
        // file with the opaque "null" origin, reports back, and only from the TV.
        if request.origin.is_some() && !(closed && request.peer.is_loopback()) {
            return error(403, "Browser requests are not accepted");
        }
        let body = if request.body.is_empty() {
            json!({})
        } else {
            match serde_json::from_slice::<Value>(request.body) {
                Ok(body @ Value::Object(_)) => body,
                _ => return error(400, "The body must be a JSON object"),
            }
        };
        match (request.method, request.path) {
            ("GET", "/api/info") => reply(
                200,
                json!({
                    "id": self.store.device_id,
                    "name": "Luma",
                    "version": env!("CARGO_PKG_VERSION"),
                    "api": API_VERSION,
                }),
            ),
            ("POST", "/api/pair") => self.pair(&body, now),
            ("POST", "/api/pair/confirm") => self.confirm(&body, now),
            ("POST", "/api/overlay/closed") => {
                if !request.peer.is_loopback() {
                    return error(403, "Only the overlay app reports closing");
                }
                let matches = self.session.as_ref().is_some_and(|session| {
                    body["session"] == session.id.as_str()
                        && body["nonce"] == session.nonce.as_str()
                });
                if matches {
                    self.end_session();
                }
                reply(200, json!({"ended": matches}))
            }
            (method, path) => {
                let Some(client) = self.authorized(request) else {
                    return error(401, "Pair this Home Assistant server with the TV again");
                };
                match (method, path) {
                    ("DELETE", "/api/pair") => {
                        let removed = self.store.clients.remove(client);
                        self.cameras.retain(|(owner, _)| *owner != removed.token);
                        self.save();
                        reply(200, json!({"paired": false}))
                    }
                    ("GET", "/api/status") => reply(
                        200,
                        json!({
                            "overlay": self.session.as_ref().map(|s| s.id.as_str()),
                            "overlay_ready": self.overlay_ready.load(Ordering::Acquire),
                            // This server's own cameras, so each one notices a TV that
                            // restarted and lost its list.
                            "cameras": self.cameras.iter()
                                .find(|(owner, _)| *owner == self.store.clients[client].token)
                                .map_or(0, |(_, cameras)| cameras.len()),
                        }),
                    ),
                    ("POST", "/api/cameras") => self.set_cameras(client, &body),
                    ("POST", "/api/overlay") => self.show(&body, now),
                    ("DELETE", "/api/overlay") => {
                        if self.session.is_some() {
                            if let Err(error) = self.tv.close_overlay() {
                                eprintln!("Luma Home Assistant close: {error}");
                            }
                            self.end_session();
                        }
                        reply(200, json!({"overlay": null}))
                    }
                    ("POST", "/api/notify") => {
                        let message = match text(&body, "message", 300) {
                            Ok(Some(message)) => message,
                            Ok(None) => return error(400, "message is required"),
                            Err(reply) => return reply,
                        };
                        match self.tv.toast(&message) {
                            Ok(()) => reply(200, json!({"shown": true})),
                            Err(e) => error(502, &e),
                        }
                    }
                    _ => error(404, "Unknown endpoint"),
                }
            }
        }
    }

    fn pair(&mut self, body: &Value, now: Instant) -> Reply {
        let name = match text(body, "name", 64) {
            Ok(name) => name.unwrap_or_else(|| "Home Assistant".into()),
            Err(reply) => return reply,
        };
        if self
            .last_pairing
            .is_some_and(|last| now.duration_since(last) < PAIRING_COOLDOWN)
        {
            return error(429, "Wait a moment before requesting another code");
        }
        if self.store.clients.len() >= MAX_CLIENTS {
            return error(
                409,
                "Too many paired servers; turn Home Assistant off and on in Luma",
            );
        }
        let (id, code) = match (random_hex(16), pairing_code()) {
            (Ok(id), Ok(code)) => (id, code),
            (Err(e), _) | (_, Err(e)) => return error(500, &e),
        };
        self.last_pairing = Some(now);
        let _ = self.events.send(Event::PairingCode(code.clone()));
        if let Err(e) = self
            .tv
            .toast(&format!("Home Assistant pairing code: {code}"))
        {
            eprintln!("Luma Home Assistant pairing toast: {e}");
        }
        self.pairing = Some(Pairing {
            id: id.clone(),
            code,
            name,
            expires: now + PAIRING_TTL,
            attempts: 0,
        });
        reply(
            200,
            json!({"pairing_id": id, "expires_in": PAIRING_TTL.as_secs()}),
        )
    }

    fn confirm(&mut self, body: &Value, now: Instant) -> Reply {
        let Some(pairing) = self
            .pairing
            .as_mut()
            .filter(|p| now < p.expires && body["pairing_id"] == p.id.as_str())
        else {
            return error(404, "This pairing request expired; start again");
        };
        pairing.attempts += 1;
        let code = body["code"].as_str().unwrap_or_default().trim();
        if code != pairing.code {
            if pairing.attempts >= PAIRING_ATTEMPTS {
                self.pairing = None;
                return error(403, "Too many wrong codes; start again");
            }
            return error(403, "Wrong code");
        }
        let Some(pairing) = self.pairing.take() else {
            return error(404, "This pairing request expired; start again");
        };
        let token = match random_hex(32) {
            Ok(token) => token,
            Err(e) => return error(500, &e),
        };
        self.store.clients.push(Client {
            name: pairing.name.clone(),
            token: digest_hex(&token),
            paired: unix_now(),
        });
        self.save();
        let _ = self.events.send(Event::Paired(pairing.name));
        let _ = self.tv.toast("Home Assistant paired with Luma");
        self.prepare();
        reply(200, json!({"token": token, "id": self.store.device_id}))
    }

    fn show(&mut self, body: &Value, now: Instant) -> Reply {
        let parsed = (|| {
            let stream = camera_url(body, "stream_url")?;
            let snapshot = camera_url(body, "snapshot_url")?;
            if stream.is_none() && snapshot.is_none() {
                return Err(error(400, "stream_url or snapshot_url is required"));
            }
            Ok((
                stream,
                snapshot,
                text(body, "title", 80)?,
                text(body, "message", 200)?,
                number(body, "duration", 30., 5., 600.)?,
                number(body, "refresh", 1., 0.25, 30.)?,
                choice(
                    body,
                    "position",
                    &["top-right", "top-left", "bottom-right", "bottom-left"],
                )?,
                choice(body, "size", &["medium", "small", "large"])?,
                match body.get("pause") {
                    None | Some(Value::Null) => false,
                    Some(Value::Bool(pause)) => *pause,
                    Some(_) => return Err(error(400, "pause must be true or false")),
                },
            ))
        })();
        let (stream, snapshot, title, message, duration, refresh, position, size, pause) =
            match parsed {
                Ok(parsed) => parsed,
                Err(reply) => return reply,
            };
        if !self.overlay_ready.load(Ordering::Acquire) {
            self.prepare();
            return error(
                503,
                "The overlay app is still being installed; try again shortly",
            );
        }
        // A new ring while an overlay is up replaces it; keep the media it paused.
        let mut media = self.session.take().and_then(|session| session.media);
        if pause && media.is_none() {
            media = self.tv.pause_media().unwrap_or_else(|error| {
                eprintln!("Luma Home Assistant pause: {error}");
                None
            });
        }
        let (id, nonce) = match (random_hex(8), random_hex(16)) {
            (Ok(id), Ok(nonce)) => (id, nonce),
            (Err(e), _) | (_, Err(e)) => return error(500, &e),
        };
        let params = json!({
            "session": id,
            "nonce": nonce,
            "port": PORT,
            "stream": stream,
            "snapshot": snapshot,
            "refresh": refresh,
            "title": title,
            "message": message,
            "duration": duration,
            "position": position,
            "size": size,
        });
        let paused = media.is_some();
        self.session = Some(Session {
            id: id.clone(),
            nonce,
            media,
            ends: now + Duration::from_secs_f64(duration) + SESSION_GRACE,
            gallery: false,
        });
        if let Err(e) = self.tv.launch_overlay(&params) {
            self.end_session();
            return error(502, &format!("The TV did not open the overlay: {e}"));
        }
        reply(200, json!({"session": id, "paused": paused}))
    }

    /// Replaces the camera list. Home Assistant sends every camera each time a camera's access
    /// token rotates, so an open gallery is relaunched with the fresh URLs.
    /// Every paired server's cameras in pairing order, up to [`MAX_CAMERAS`].
    fn all_cameras(&self) -> Vec<&Camera> {
        self.cameras
            .iter()
            .flat_map(|(_, cameras)| cameras)
            .take(MAX_CAMERAS)
            .collect()
    }

    fn set_cameras(&mut self, client: usize, body: &Value) -> Reply {
        let Some(list) = body.get("cameras").and_then(Value::as_array) else {
            return error(400, "cameras must be a list");
        };
        let mut cameras = Vec::new();
        for entry in list.iter().take(MAX_CAMERAS) {
            if !entry.is_object() {
                return error(400, "Each camera must be an object");
            }
            let parsed = (|| {
                let id = text(entry, "id", 255)?.ok_or_else(|| error(400, "id is required"))?;
                let stream = camera_url(entry, "stream_url")?;
                let snapshot = camera_url(entry, "snapshot_url")?;
                if stream.is_none() && snapshot.is_none() {
                    return Err(error(400, "stream_url or snapshot_url is required"));
                }
                let name = text(entry, "name", 80)?.unwrap_or_else(|| id.clone());
                Ok(Camera {
                    id,
                    name,
                    stream,
                    snapshot,
                })
            })();
            match parsed {
                Ok(camera) => cameras.push(camera),
                Err(reply) => return reply,
            }
        }
        let before: Vec<Camera> = self.all_cameras().into_iter().cloned().collect();
        let owner = self.store.clients[client].token.clone();
        self.cameras.retain(|(key, _)| *key != owner);
        if !cameras.is_empty() {
            self.cameras.push((owner, cameras));
            // Keep servers in pairing order so the gallery does not reshuffle between pushes.
            let clients = &self.store.clients;
            self.cameras
                .sort_by_key(|(key, _)| clients.iter().position(|client| client.token == *key));
        }
        let total = self.all_cameras().len();
        let changed = before.iter().ne(self.all_cameras());
        if changed && self.session.as_ref().is_some_and(|session| session.gallery) {
            if total == 0 {
                let _ = self.tv.close_overlay();
                self.end_session();
            } else if let Err(e) = self.launch_gallery() {
                eprintln!("Luma Home Assistant cameras refresh: {e}");
            }
        }
        reply(200, json!({"cameras": total}))
    }

    fn gallery_params(&self, session: &Session) -> Value {
        // Two servers can share an entity ID; the page tells cameras apart by ID, so prefix
        // each with its server's position, which stays stable while the servers stay paired.
        let cameras: Vec<Camera> = self
            .cameras
            .iter()
            .enumerate()
            .flat_map(|(server, (_, cameras))| {
                cameras.iter().map(move |camera| Camera {
                    id: format!("{server}/{}", camera.id),
                    ..camera.clone()
                })
            })
            .take(MAX_CAMERAS)
            .collect();
        json!({
            "mode": "cameras",
            "session": session.id,
            "nonce": session.nonce,
            "port": PORT,
            "cameras": cameras,
        })
    }

    fn launch_gallery(&self) -> Result<(), String> {
        let Some(session) = self.session.as_ref().filter(|session| session.gallery) else {
            return Ok(());
        };
        self.tv.launch_overlay(&self.gallery_params(session))
    }

    /// Opens the full-screen Cameras gallery, Luma's built-in Cameras app.
    pub fn show_cameras(&mut self, now: Instant) {
        if self.store.clients.is_empty() {
            let _ = self.events.send(Event::Notice("camerasNotPaired"));
            return;
        }
        if self.all_cameras().is_empty() {
            let _ = self.events.send(Event::Notice("camerasNone"));
            return;
        }
        if !self.overlay_ready.load(Ordering::Acquire) {
            self.prepare();
            let _ = self.events.send(Event::Notice("camerasInstalling"));
            return;
        }
        // A doorbell card that paused playback keeps its paused media for the gallery.
        let media = self.session.take().and_then(|session| session.media);
        let (id, nonce) = match (random_hex(8), random_hex(16)) {
            (Ok(id), Ok(nonce)) => (id, nonce),
            (Err(e), _) | (_, Err(e)) => {
                let _ = self.events.send(Event::Failed(e));
                return;
            }
        };
        self.session = Some(Session {
            id,
            nonce,
            media,
            ends: now + GALLERY_TTL,
            gallery: true,
        });
        if let Err(e) = self.launch_gallery() {
            self.end_session();
            let _ = self.events.send(Event::Failed(e));
        }
    }
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension(format!("json.new-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = options
        .open(&temporary)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&temporary, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|e| e.to_string())
}

enum Command {
    Enable,
    Disable,
    ShowCameras,
}

/// Owns the listener thread. Enabling binds the port; disabling closes it and forgets pairings.
pub struct Service {
    commands: mpsc::Sender<Command>,
    pub events: mpsc::Receiver<Event>,
    enabled: bool,
}

impl Service {
    pub fn new(tv: impl Tv, store_path: Option<PathBuf>, port: u16) -> Self {
        let (commands, command_rx) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        let tv = Arc::new(tv);
        thread::Builder::new()
            .name("home-assistant".into())
            .spawn(move || run(tv, store_path, port, &command_rx, event_tx))
            .expect("Home Assistant thread");
        Self {
            commands,
            events,
            enabled: false,
        }
    }

    /// Opens the Cameras gallery; problems come back as [`Event::Notice`].
    pub fn show_cameras(&self) {
        if self.enabled {
            let _ = self.commands.send(Command::ShowCameras);
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled != self.enabled {
            self.enabled = enabled;
            let _ = self.commands.send(if enabled {
                Command::Enable
            } else {
                Command::Disable
            });
        }
    }
}

fn run<T: Tv>(
    tv: Arc<T>,
    store_path: Option<PathBuf>,
    port: u16,
    commands: &mpsc::Receiver<Command>,
    events: mpsc::Sender<Event>,
) {
    let mut api = match Api::new(tv, store_path, events.clone()) {
        Ok(api) => api,
        Err(error) => {
            let _ = events.send(Event::Failed(error));
            return;
        }
    };
    let mut listener: Option<TcpListener> = None;
    loop {
        let command = if listener.is_some() {
            match commands.recv_timeout(Duration::from_millis(50)) {
                Ok(command) => Some(command),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => return,
            }
        };
        match command {
            Some(Command::Enable) if listener.is_none() => {
                // Every interface: the TV's address may change, so peers are checked per request.
                match TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
                    .and_then(|l| l.set_nonblocking(true).map(|()| l))
                {
                    Ok(bound) => {
                        let _ = events.send(Event::Listening(address(port)));
                        listener = Some(bound);
                        api.prepare();
                    }
                    Err(error) => {
                        let _ = events.send(Event::Failed(format!("Port {port}: {error}")));
                    }
                }
            }
            Some(Command::Disable) => {
                listener = None;
                api.forget();
            }
            Some(Command::ShowCameras) => api.show_cameras(Instant::now()),
            _ => {}
        }
        api.tick(Instant::now());
        let Some(bound) = &listener else {
            continue;
        };
        // Requests are small and handled in turn; each connection has a short deadline.
        while let Ok((stream, peer)) = bound.accept() {
            serve(&mut api, stream, peer);
        }
    }
}

/// The address to type into Home Assistant, for the on-screen notice.
fn address(port: u16) -> String {
    let ip = if_addrs::get_if_addrs()
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|interface| match interface.ip() {
            IpAddr::V4(ip) if ip.is_private() || ip.is_link_local() => Some(ip),
            _ => None,
        })
        .next();
    ip.map_or_else(|| format!("port {port}"), |ip| format!("{ip}:{port}"))
}

fn serve<T: Tv>(api: &mut Api<T>, mut stream: TcpStream, peer: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let valid = || true;
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let result = (|| {
        if !companion::local_peer(peer.ip()) {
            return Err((
                403,
                "Only devices on the TV's local network may connect".to_owned(),
            ));
        }
        let request = companion::request(&mut stream, &valid, deadline, MAX_BODY, "Body too large")
            .map_err(|e| (400, e))?;
        let mut body = vec![0; request.length];
        companion::read_exact(&mut stream, &mut body, &valid, deadline).map_err(|e| (400, e))?;
        let path = request.target.split('?').next().unwrap_or_default();
        Ok(api.handle(
            &Request {
                method: &request.method,
                path,
                authorization: request.headers.get("authorization").map(String::as_str),
                origin: request.headers.get("origin").map(String::as_str),
                peer: peer.ip(),
                body: &body,
            },
            Instant::now(),
        ))
    })();
    let reply = result.unwrap_or_else(|(status, message)| error(status, &message));
    let status = match reply.status {
        200 => "200 OK",
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        403 => "403 Forbidden",
        404 => "404 Not Found",
        409 => "409 Conflict",
        429 => "429 Too Many Requests",
        502 => "502 Bad Gateway",
        503 => "503 Service Unavailable",
        _ => "500 Internal Server Error",
    };
    let _ = companion::response(
        &mut stream,
        status,
        "application/json; charset=utf-8",
        reply.body.to_string().as_bytes(),
    );
}

#[cfg(test)]
#[path = "home-assistant-tests.rs"]
mod tests;
