//! Home Assistant integration: an opt-in LAN API that paired Home Assistant servers call to show a
//! camera feed in a corner overlay above whatever is playing, or a plain system toast.
//!
//! Pairing never needs a password typed on the TV: Home Assistant asks to pair, the TV shows a
//! six-digit code, and the user types it into Home Assistant, which receives a bearer token. Only
//! SHA-256 digests of tokens are stored. Turning the setting off forgets every paired server, but
//! not how the owner arranged its cameras: that comes back when the same server pairs again.
//!
//! The overlay itself is the separate web app `com.ecklf.luma.overlay` (see `overlay/`), whose
//! transparent `overlay` window type keeps the foreground app playing underneath it. Luma installs
//! it from its own package, launches it with the camera details and, when asked, pauses the
//! foreground media first and resumes it once the overlay closes.
//!
//! Home Assistant also pushes its camera list (`POST /api/cameras`) whenever a camera's signed
//! proxy URLs rotate. Luma's built-in Cameras app shows them natively (see `cameras`): this
//! thread sends the list, with the owner's saved arrangement applied, as [`Event::Cameras`], and
//! stores the arrangement the gallery sends back. For an enlarged camera with Home Assistant's
//! HLS stream, it opens the same overlay app as a full-screen video player and reports back
//! ([`Event::VideoClosed`]) when that closes.
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
        atomic::{AtomicBool, AtomicU64, Ordering},
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
/// What this Luma's API offers. Raise it only when Home Assistant can tell an older TV is missing
/// something it needs; it compares this with its own minimum and asks the owner to update Luma.
const API_VERSION: u32 = 1;
/// The oldest Home Assistant integration this Luma works with, by the level the integration sends
/// as `X-Luma-Integration`. Raise it only when an older integration cannot work with this TV: the
/// servers below it are named in Settings, and Home Assistant reads it from `/api/status`.
const MIN_INTEGRATION: u32 = 1;
/// The level of an integration that sends no `X-Luma-Integration` header: every integration from
/// before the header existed.
const INTEGRATION_UNSTATED: u32 = 1;
const MAX_BODY: usize = 32 * 1024;
/// Cameras kept from Home Assistant's list; more are dropped.
pub const MAX_CAMERAS: usize = 32;
/// Remembered entries in the gallery's order and hidden lists, including absent cameras.
const MAX_ARRANGED: usize = 64;
const MAX_CLIENTS: usize = 8;
/// Arrangements kept for servers that are no longer paired; the oldest go first.
const MAX_PARKED: usize = 16;
const MAX_URL: usize = 4096;
const PAIRING_TTL: Duration = Duration::from_secs(120);
const PAIRING_ATTEMPTS: u8 = 5;
const PAIRING_COOLDOWN: Duration = Duration::from_secs(3);
/// A camera video left playing is closed after this long.
const VIDEO_TTL: Duration = Duration::from_secs(60 * 60);
/// The overlay dismisses itself; this margin covers launch time before Luma resumes playback.
const SESSION_GRACE: Duration = Duration::from_secs(5);

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
    /// The code expired or was locked out after wrong guesses, without pairing.
    PairingEnded,
    Paired(String),
    Failed(String),
    /// A translation key to show as a Luma notice.
    Notice(&'static str),
    /// Every camera for the Cameras app, arranged as the owner left it. `open` answers the
    /// app being opened; otherwise the list changed (rotated URLs, cameras added or removed).
    Cameras {
        cameras: Vec<crate::cameras::Camera>,
        live: bool,
        open: bool,
    },
    /// Home Assistant saved streaming search keys; Settings shows the new status.
    StreamingKeys(crate::streaming_keys::Status),
    /// The names of paired servers whose Luma integration is older than this TV needs; sent
    /// whenever the list changes, including at startup when a stored server is outdated.
    IntegrationOutdated(Vec<String>),
    /// The camera video closed: Back, a doorbell card replacing it, a key the gallery carries on
    /// with, or (`failed`) the video could not be played, so the gallery keeps its own picture.
    VideoClosed(crate::cameras::Closed),
}

/// One Home Assistant camera, with URLs the TV can load directly.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Camera {
    pub id: String,
    pub name: String,
    pub stream: Option<String>,
    pub snapshot: Option<String>,
    /// The integration's endpoint that starts Home Assistant's HLS stream for this camera.
    pub hls: Option<String>,
    /// The integration's endpoint into what UniFi Protect recorded of this camera.
    pub history: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Store {
    /// Stable per installation, so Home Assistant recognises a TV whose address changed.
    pub device_id: String,
    pub clients: Vec<Client>,
    pub gallery: Gallery,
    /// Fields from a newer Luma, kept so that going back a version and forward again loses
    /// nothing.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// How the owner arranged the Cameras gallery, by gallery camera ID: the server's position in
/// pairing order and the entity ID (`0/camera.door`). Two servers' cameras that share an entity
/// ID are arranged separately; the arrangement survives token rotation and restarts.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Gallery {
    pub order: Vec<String>,
    pub hidden: Vec<String>,
    /// Enlarged cameras poll snapshots instead of opening the motion-JPEG stream.
    pub snapshots: bool,
    /// The owner's names for cameras, shown instead of Home Assistant's.
    pub names: std::collections::BTreeMap<String, String>,
    /// Arrangements of servers that were unpaired or forgotten, restored when they pair again.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parked: Vec<Parked>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// One unpaired server's part of the gallery arrangement, by entity ID (`camera.door`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Parked {
    /// The server's Home Assistant instance ID, when its integration sent one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub server: String,
    pub name: String,
    pub order: Vec<String>,
    pub hidden: Vec<String>,
    pub names: std::collections::BTreeMap<String, String>,
}

impl Parked {
    /// Whether this is the arrangement of `client`, which is pairing again. The instance ID
    /// decides when both have one; an arrangement with a different ID is another server.
    fn belongs_to(&self, client: &Client) -> bool {
        if self.server.is_empty() || client.server.is_empty() {
            self.name == client.name
        } else {
            self.server == client.server
        }
    }
}

impl Gallery {
    /// Moves the entries of the server at `index` in pairing order to [`Gallery::parked`].
    /// Later servers keep their places.
    fn park(&mut self, index: usize, client: &Client) {
        let prefix = format!("{index}/");
        let strip = |id: &String| id.strip_prefix(&prefix).map(str::to_owned);
        let parked = Parked {
            server: client.server.clone(),
            name: client.name.clone(),
            order: self.order.iter().filter_map(strip).collect(),
            hidden: self.hidden.iter().filter_map(strip).collect(),
            names: self
                .names
                .iter()
                .filter_map(|(id, name)| Some((strip(id)?, name.clone())))
                .collect(),
        };
        self.order.retain(|id| !id.starts_with(&prefix));
        self.hidden.retain(|id| !id.starts_with(&prefix));
        self.names.retain(|id, _| !id.starts_with(&prefix));
        // The newest arrangement replaces an older one for the same server. Without instance
        // IDs, two servers can share a name, so both are kept rather than one lost.
        self.parked
            .retain(|old| parked.server.is_empty() || old.server != parked.server);
        if !parked.order.is_empty() || !parked.hidden.is_empty() || !parked.names.is_empty() {
            self.parked.push(parked);
        }
        let excess = self.parked.len().saturating_sub(MAX_PARKED);
        self.parked.drain(..excess);
    }

    /// The server just paired at `index` gets back the arrangement it had, if any.
    fn restore(&mut self, index: usize, client: &Client) {
        // An instance ID match is certain. A match by name only counts when no other parked
        // server shares that name, so one server never gets another's arrangement.
        let by_id = (!client.server.is_empty())
            .then(|| {
                self.parked
                    .iter()
                    .position(|parked| parked.server == client.server)
            })
            .flatten();
        let position = by_id.or_else(|| {
            let mut named = self
                .parked
                .iter()
                .enumerate()
                .filter(|(_, parked)| parked.belongs_to(client));
            match (named.next(), named.next()) {
                (Some((position, _)), None) => Some(position),
                _ => None,
            }
        });
        let Some(position) = position else {
            return;
        };
        let parked = self.parked.remove(position);
        let id = |entity: &String| format!("{index}/{entity}");
        for entity in &parked.order {
            let id = id(entity);
            if !self.order.contains(&id) {
                self.order.push(id);
            }
        }
        for entity in &parked.hidden {
            let id = id(entity);
            if !self.hidden.contains(&id) {
                self.hidden.push(id);
            }
        }
        for (entity, name) in &parked.names {
            self.names.entry(id(entity)).or_insert_with(|| name.clone());
        }
        self.order.truncate(MAX_ARRANGED);
        self.hidden.truncate(MAX_ARRANGED);
        while self.names.len() > MAX_ARRANGED {
            self.names.pop_last();
        }
    }

    /// The server at `index` in pairing order was unpaired: its entries are parked, and later
    /// servers, which move up one place, keep theirs.
    fn unpaired(&mut self, index: usize, client: &Client) {
        self.park(index, client);
        // An entry's new ID, or none if it belonged to the unpaired server.
        let moved = |id: &str| -> Option<String> {
            let Some((server, entity)) = id.split_once('/') else {
                return Some(id.into());
            };
            match server.parse::<usize>() {
                Ok(server) if server == index => None,
                Ok(server) if server > index => Some(format!("{}/{entity}", server - 1)),
                _ => Some(id.into()),
            }
        };
        self.order = self.order.iter().filter_map(|id| moved(id)).collect();
        self.hidden = self.hidden.iter().filter_map(|id| moved(id)).collect();
        self.names = std::mem::take(&mut self.names)
            .into_iter()
            .filter_map(|(id, name)| Some((moved(&id)?, name)))
            .collect();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Client {
    pub name: String,
    /// Hex SHA-256 of the bearer token.
    pub token: String,
    pub paired: u64,
    /// Home Assistant's instance ID, which tells the server apart when it pairs again.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub server: String,
    /// The integration level this server last sent with a request (see [`MIN_INTEGRATION`]).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub integration: u32,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// The integration level a request states in `X-Luma-Integration`.
fn integration_level(header: Option<&str>) -> u32 {
    header
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(INTEGRATION_UNSTATED)
}

struct Pairing {
    id: String,
    code: String,
    name: String,
    server: String,
    expires: Instant,
    attempts: u8,
}

struct Session {
    id: String,
    nonce: String,
    media: Option<String>,
    ends: Instant,
    /// A camera video from the Cameras app rather than a doorbell card.
    video: bool,
    /// Recorded video of this gallery camera, which the overlay app loads piece by piece.
    recorded: Option<String>,
}

pub struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    /// The query string, without `?`.
    pub query: &'a str,
    pub authorization: Option<&'a str>,
    pub origin: Option<&'a str>,
    /// `X-Luma-Integration`: the Home Assistant integration's level.
    pub integration: Option<&'a str>,
    pub peer: IpAddr,
    pub body: &'a [u8],
}

#[derive(Debug, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
    /// Where a 302 redirects.
    pub location: Option<String>,
    /// `GET /api/events`: the connection becomes a stream of Luma's log.
    pub events: Option<Box<Subscription>>,
}

/// One server's event stream: where it resumes, and which of its streams it is. A server that
/// connects again replaces its earlier stream, which may be a connection that died silently.
#[derive(Clone, Debug, PartialEq)]
pub struct Subscription {
    pub cursor: crate::telemetry::Cursor,
    owner: String,
    id: u64,
}

fn reply(status: u16, body: Value) -> Reply {
    Reply {
        status,
        body,
        location: None,
        events: None,
    }
}

/// A query parameter's value (no percent-decoding: only numbers and hex are expected).
fn query<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
}

/// The longest piece of recorded video the overlay app may ask for.
const MAX_RECORDED_PIECE: u64 = 15 * 60;

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

/// A list of gallery camera IDs (`0/camera.door`), without duplicates.
fn camera_ids(body: &Value, key: &str) -> Result<Option<Vec<String>>, Reply> {
    let list = match body.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Array(list)) if list.len() <= MAX_ARRANGED => list,
        Some(_) => {
            return Err(error(
                400,
                &format!("{key} must be a list of at most {MAX_ARRANGED} camera IDs"),
            ));
        }
    };
    let mut ids: Vec<String> = Vec::new();
    for value in list {
        let id = match value.as_str() {
            Some(id) if id.len() <= 300 && !id.chars().any(char::is_control) => id,
            _ => return Err(error(400, &format!("{key} must hold camera IDs"))),
        };
        if !id.is_empty() && !ids.iter().any(|known| known == id) {
            ids.push(id.to_owned());
        }
    }
    Ok(Some(ids))
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
    /// Changes whenever a server is unpaired, which ends every open event stream; the servers
    /// still paired reconnect from where they left off.
    pairings: Arc<AtomicU64>,
    /// Each server's current event stream, by token digest.
    streams: Arc<std::sync::Mutex<std::collections::BTreeMap<String, u64>>>,
    next_stream: u64,
    /// The last [`Event::IntegrationOutdated`] list, so it is sent only when it changes. The SDL
    /// thread starts with none outdated.
    outdated: Vec<String>,
    /// [`MIN_INTEGRATION`]; tests raise it.
    min_integration: u32,
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
            pairings: Arc::new(AtomicU64::new(0)),
            streams: Arc::default(),
            next_stream: 0,
            outdated: Vec::new(),
            min_integration: MIN_INTEGRATION,
        };
        if api.store.device_id.len() != 32 {
            api.store.device_id = random_hex(16)?;
            api.save();
        }
        api.send_outdated();
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
            crate::telemetry::log_error!("home-assistant", "could not save pairings: {error}");
        }
    }

    /// Forgets every paired server, e.g. when the setting is turned off.
    pub fn forget(&mut self) {
        self.pairings.fetch_add(1, Ordering::AcqRel);
        crate::telemetry::wake_streams();
        if self.session.is_some() {
            let _ = self.tv.close_overlay();
            self.end_session();
        }
        if self.pairing.take().is_some() {
            let _ = self.events.send(Event::PairingEnded);
        }
        if !self.cameras.is_empty() {
            self.cameras.clear();
            self.send_cameras(false);
        }
        let before = self.store.clone();
        // Gallery IDs name servers by pairing order, which starts over; each server's
        // arrangement is parked until it pairs again.
        let clients = std::mem::take(&mut self.store.clients);
        let gallery = &mut self.store.gallery;
        for (index, client) in clients.iter().enumerate() {
            gallery.park(index, client);
        }
        // Whatever is left belonged to no paired server.
        gallery.order.clear();
        gallery.hidden.clear();
        gallery.names.clear();
        if self.store != before {
            self.save();
        }
        self.send_outdated();
    }

    /// Tells the SDL thread which paired servers need a newer integration, when that changed.
    fn send_outdated(&mut self) {
        let outdated: Vec<String> = self
            .store
            .clients
            .iter()
            .filter(|client| client.integration != 0 && client.integration < self.min_integration)
            .map(|client| client.name.clone())
            .collect();
        if self.outdated != outdated {
            let _ = self
                .events
                .send(Event::IntegrationOutdated(outdated.clone()));
            self.outdated = outdated;
        }
    }

    /// Records the integration level a paired server sent with a request.
    fn integration_seen(&mut self, client: usize, request: &Request) {
        let level = integration_level(request.integration);
        if self.store.clients[client].integration != level {
            self.store.clients[client].integration = level;
            self.save();
            self.send_outdated();
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
                    crate::telemetry::log_error!("overlay", "install: {error}");
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
        self.finish_session(crate::cameras::Closed::default());
    }

    /// Ends the session; a camera video tells the gallery how it closed.
    fn finish_session(&mut self, closed: crate::cameras::Closed) {
        let Some(session) = self.session.take() else {
            return;
        };
        if session.video {
            let _ = self.events.send(Event::VideoClosed(closed));
        }
        if let Some(media) = session.media
            && let Err(error) = self.tv.resume_media(&media)
        {
            crate::telemetry::log_warn!("overlay", "resume: {error}");
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
            let _ = self.events.send(Event::PairingEnded);
        }
    }

    pub fn handle(&mut self, request: &Request, now: Instant) -> Reply {
        if !companion::local_peer(request.peer) {
            return error(403, "Only devices on the TV's local network may connect");
        }
        let closed = matches!(
            request.path,
            "/api/overlay/closed" | "/api/overlay/expand" | "/api/overlay/recorded"
        );
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
                    "min_integration": self.min_integration,
                }),
            ),
            ("POST", "/api/pair") => self.pair(&body, now),
            ("POST", "/api/pair/confirm") => self.confirm(&body, request, now),
            // OK on a doorbell card: full screen, it stays until the owner closes it.
            ("POST", "/api/overlay/expand") => {
                if !request.peer.is_loopback() {
                    return error(403, "Only the overlay app expands its card");
                }
                let session = self.session.as_mut().filter(|session| {
                    !session.video
                        && body["session"] == session.id.as_str()
                        && body["nonce"] == session.nonce.as_str()
                });
                let expanded = session.is_some();
                if let Some(session) = session {
                    session.ends = now + VIDEO_TTL;
                }
                reply(200, json!({"expanded": expanded}))
            }
            ("POST", "/api/overlay/closed") => {
                if !request.peer.is_loopback() {
                    return error(403, "Only the overlay app reports closing");
                }
                let matches = self.session.as_ref().is_some_and(|session| {
                    body["session"] == session.id.as_str()
                        && body["nonce"] == session.nonce.as_str()
                });
                if matches {
                    let key = body["key"].as_str().filter(|key| {
                        matches!(
                            *key,
                            "ok" | "left" | "right" | "up" | "down" | "rewind" | "forward" | "live"
                        )
                    });
                    if body["failed"] == true {
                        crate::telemetry::log_warn!(
                            "overlay",
                            "the camera video could not play; the gallery keeps its own picture"
                        );
                    }
                    self.finish_session(crate::cameras::Closed {
                        failed: body["failed"] == true,
                        behind: body["behind"].as_f64().filter(|_| key.is_some()),
                        key: key.map(str::to_owned),
                        paused: body["paused"] == true,
                    });
                }
                reply(200, json!({"ended": matches}))
            }
            // The overlay app's recorded video, a piece at a time: redirected to the camera's
            // history endpoint with its current token, which Home Assistant rotates.
            ("GET", "/api/overlay/recorded") => {
                if !request.peer.is_loopback() {
                    return error(403, "Only the overlay app loads recorded video");
                }
                self.recorded_piece(request.query)
            }
            (method, path) => {
                let Some(client) = self.authorized(request) else {
                    return error(401, "Pair this Home Assistant server with the TV again");
                };
                self.integration_seen(client, request);
                match (method, path) {
                    ("DELETE", "/api/pair") => {
                        self.pairings.fetch_add(1, Ordering::AcqRel);
                        crate::telemetry::wake_streams();
                        let removed = self.store.clients.remove(client);
                        self.cameras.retain(|(owner, _)| *owner != removed.token);
                        self.store.gallery.unpaired(client, &removed);
                        self.save();
                        self.send_cameras(false);
                        self.send_outdated();
                        reply(200, json!({"paired": false}))
                    }
                    // Luma's log, live: what it kept since the cursor `after`, then each
                    // entry as it happens. `serve` hands the connection to its own thread.
                    ("GET", "/api/events") => {
                        if !crate::telemetry::stream_available() {
                            return error(503, "Too many event streams are open");
                        }
                        let after = query(request.query, "after")
                            .unwrap_or_default()
                            .replace("%3A", ":")
                            .replace("%3a", ":");
                        let owner = self.store.clients[client].token.clone();
                        self.next_stream += 1;
                        self.streams
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .insert(owner.clone(), self.next_stream);
                        crate::telemetry::wake_streams();
                        Reply {
                            events: Some(Box::new(Subscription {
                                cursor: crate::telemetry::Cursor::parse(&after),
                                owner,
                                id: self.next_stream,
                            })),
                            ..reply(200, json!({}))
                        }
                    }
                    ("GET", "/api/diagnostics") => {
                        let mut snapshot = crate::telemetry::snapshot();
                        snapshot["overlay_ready"] =
                            json!(self.overlay_ready.load(Ordering::Acquire));
                        snapshot["paired_servers"] = json!(self.store.clients.len());
                        snapshot["cameras"] = json!(self.all_cameras().len());
                        reply(200, snapshot)
                    }
                    ("GET", "/api/status") => reply(
                        200,
                        json!({
                            // Home Assistant compares these with its own levels and asks the
                            // owner to update whichever side is too old.
                            "version": env!("CARGO_PKG_VERSION"),
                            "api": API_VERSION,
                            "min_integration": self.min_integration,
                            "overlay": self.session.as_ref().map(|s| s.id.as_str()),
                            "overlay_ready": self.overlay_ready.load(Ordering::Acquire),
                            // This server's own cameras, so each one notices a TV that
                            // restarted and lost its list.
                            "cameras": self.cameras.iter()
                                .find(|(owner, _)| *owner == self.store.clients[client].token)
                                .map_or(0, |(_, cameras)| cameras.len()),
                            // Which streaming search keys the TV holds and whether Home
                            // Assistant set them, so it pushes its keys again after a reset or
                            // a change on the TV. Never the keys themselves.
                            "streaming_keys": self.streaming_keys_path().map(|path| {
                                let status = crate::streaming_keys::load(&path).status();
                                json!({
                                    "motn": status.motn,
                                    "tmdb": status.tmdb,
                                    "mdblist": status.mdblist,
                                    "home_assistant": status.home_assistant,
                                })
                            }),
                        }),
                    ),
                    ("POST", "/api/cameras") => self.set_cameras(client, &body),
                    ("PUT", "/api/streaming-keys") => self.set_streaming_keys(&body),
                    ("POST", "/api/overlay") => self.show(&body, now),
                    ("DELETE", "/api/overlay") => {
                        // Dismissing closes a doorbell card, never a camera the owner is
                        // watching full screen in the Cameras app.
                        if self.session.as_ref().is_some_and(|session| !session.video) {
                            if let Err(error) = self.tv.close_overlay() {
                                crate::telemetry::log_warn!("overlay", "close: {error}");
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
        let server = match text(body, "server", 64) {
            Ok(server) => server.unwrap_or_default(),
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
            crate::telemetry::log_warn!("home-assistant", "pairing toast: {e}");
        }
        self.pairing = Some(Pairing {
            id: id.clone(),
            code,
            name,
            server,
            expires: now + PAIRING_TTL,
            attempts: 0,
        });
        reply(
            200,
            json!({"pairing_id": id, "expires_in": PAIRING_TTL.as_secs()}),
        )
    }

    fn confirm(&mut self, body: &Value, request: &Request, now: Instant) -> Reply {
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
                let _ = self.events.send(Event::PairingEnded);
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
        let client = Client {
            name: pairing.name.clone(),
            token: digest_hex(&token),
            paired: unix_now(),
            server: pairing.server,
            integration: integration_level(request.integration),
            extra: Default::default(),
        };
        self.store
            .gallery
            .restore(self.store.clients.len(), &client);
        self.store.clients.push(client);
        self.save();
        self.send_outdated();
        let _ = self.events.send(Event::Paired(pairing.name));
        let _ = self.tv.toast("Home Assistant paired with Luma");
        self.prepare();
        reply(200, json!({"token": token, "id": self.store.device_id}))
    }

    fn show(&mut self, body: &Value, now: Instant) -> Reply {
        let parsed = (|| {
            let stream = camera_url(body, "stream_url")?;
            let snapshot = camera_url(body, "snapshot_url")?;
            // Optional HLS playlist; the card shows the MJPEG or snapshots until it plays.
            let hls = camera_url(body, "hls_url")?;
            if stream.is_none() && snapshot.is_none() {
                return Err(error(400, "stream_url or snapshot_url is required"));
            }
            Ok((
                stream,
                snapshot,
                hls,
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
        let (stream, snapshot, hls, title, message, duration, refresh, position, size, pause) =
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
        // A new ring while an overlay is up replaces it; keep the media it paused. A camera video
        // it replaces is over, so the gallery leaves its enlarged camera.
        let replaced = self.session.take();
        if replaced.as_ref().is_some_and(|session| session.video) {
            let _ = self
                .events
                .send(Event::VideoClosed(crate::cameras::Closed::default()));
        }
        let mut media = replaced.and_then(|session| session.media);
        if pause && media.is_none() {
            media = self.tv.pause_media().unwrap_or_else(|error| {
                crate::telemetry::log_warn!("overlay", "pause: {error}");
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
            "hls": hls,
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
            video: false,
            recorded: None,
        });
        let launching = Instant::now();
        if let Err(e) = self.tv.launch_overlay(&params) {
            self.end_session();
            crate::telemetry::log_error!("overlay", "the camera card did not open: {e}");
            return error(502, &format!("The TV did not open the overlay: {e}"));
        }
        crate::telemetry::log_info!(
            "overlay",
            "camera card opened in {} ms ({}{})",
            launching.elapsed().as_millis(),
            if hls.is_some() {
                "HLS"
            } else if stream.is_some() {
                "motion-JPEG"
            } else {
                "snapshots"
            },
            if paused { ", show paused" } else { "" }
        );
        reply(200, json!({"session": id, "paused": paused}))
    }

    /// Every paired server's cameras in pairing order, up to [`MAX_CAMERAS`].
    fn all_cameras(&self) -> Vec<&Camera> {
        self.cameras
            .iter()
            .flat_map(|(_, cameras)| cameras)
            .take(MAX_CAMERAS)
            .collect()
    }

    /// Replaces the camera list. Home Assistant sends every camera each time a camera's access
    /// token rotates, so an open gallery gets the fresh URLs.
    /// Streaming search keys sit beside the pairings, which sit beside the preferences.
    fn streaming_keys_path(&self) -> Option<PathBuf> {
        self.store_path
            .as_ref()
            .map(|store| store.with_file_name("streaming-keys.json"))
    }

    /// Home Assistant is the source of truth for streaming search keys: a non-empty key it
    /// sends replaces the TV's, an empty or missing one leaves the TV's key alone.
    fn set_streaming_keys(&mut self, body: &Value) -> Reply {
        let Some(path) = self.streaming_keys_path() else {
            return error(503, "This TV has no storage for keys");
        };
        let field = |key| match body.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.as_str())),
            Some(_) => Err(error(400, &format!("{key} must be a string"))),
        };
        let (motn, tmdb, mdblist) = match (field("motn"), field("tmdb"), field("mdblist")) {
            (Ok(motn), Ok(tmdb), Ok(mdblist)) => (motn, tmdb, mdblist),
            (Err(reply), _, _) | (_, Err(reply), _) | (_, _, Err(reply)) => return reply,
        };
        let current = crate::streaming_keys::load(&path);
        let next =
            match crate::streaming_keys::merge_from_home_assistant(&current, motn, tmdb, mdblist) {
                Ok(next) => next,
                Err(message) => return error(400, &message),
            };
        if next != current {
            if let Err(message) = crate::streaming_keys::save(&path, &next) {
                crate::telemetry::log_error!(
                    "home-assistant",
                    "could not save streaming keys: {message}"
                );
                return error(500, "Could not save the keys");
            }
            let _ = self.events.send(Event::StreamingKeys(next.status()));
        }
        let status = next.status();
        reply(
            200,
            json!({"motn": status.motn, "tmdb": status.tmdb, "mdblist": status.mdblist}),
        )
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
                let hls = camera_url(entry, "hls_url")?;
                let history = camera_url(entry, "history_url")?;
                if stream.is_none() && snapshot.is_none() {
                    return Err(error(400, "stream_url or snapshot_url is required"));
                }
                let name = text(entry, "name", 80)?.unwrap_or_else(|| id.clone());
                Ok(Camera {
                    id,
                    name,
                    stream,
                    snapshot,
                    hls,
                    history,
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
        if before.iter().ne(self.all_cameras()) {
            self.send_cameras(false);
        }
        reply(200, json!({"cameras": total}))
    }

    /// Saves the gallery's arrangement. `order` lists every camera the gallery knows, hidden
    /// ones included; saved entries for any other camera (absent now, or not yet sent to the
    /// gallery) are kept after the ones it reported.
    pub fn arrange(&mut self, body: &Value) -> Reply {
        let order = match camera_ids(body, "order") {
            Ok(order) => order,
            Err(reply) => return reply,
        };
        let hidden = match camera_ids(body, "hidden") {
            Ok(hidden) => hidden,
            Err(reply) => return reply,
        };
        let known: Vec<String> = order
            .clone()
            .unwrap_or_else(|| self.gallery().into_iter().map(|camera| camera.id).collect());
        let merge = |given: Option<Vec<String>>, old: &[String]| {
            given.map(|mut list| {
                list.extend(
                    old.iter()
                        .filter(|id| !known.contains(id) && !list.contains(id))
                        .cloned()
                        .collect::<Vec<_>>(),
                );
                list.truncate(MAX_ARRANGED);
                list
            })
        };
        let order = merge(order, &self.store.gallery.order);
        let hidden = merge(hidden, &self.store.gallery.hidden);
        let names = match body.get("names") {
            None | Some(Value::Null) => None,
            Some(Value::Object(given)) => {
                let mut names = std::collections::BTreeMap::new();
                for (id, name) in given {
                    let Some(name) = name.as_str() else {
                        return error(400, "names must map camera IDs to text");
                    };
                    let name = crate::model::clean_input_name(name);
                    if id.len() <= 256 && !name.is_empty() {
                        names.insert(id.clone(), name);
                    }
                }
                // Names for cameras the gallery did not report are kept.
                for (id, name) in &self.store.gallery.names {
                    if !known.contains(id) && !names.contains_key(id) {
                        names.insert(id.clone(), name.clone());
                    }
                }
                if names.len() > MAX_ARRANGED {
                    return error(400, "too many camera names");
                }
                Some(names)
            }
            Some(_) => return error(400, "names must be an object"),
        };
        let snapshots = match body.get("live") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(live)) => Some(!live),
            Some(_) => return error(400, "live must be true or false"),
        };
        let gallery = &mut self.store.gallery;
        let before = gallery.clone();
        if let Some(order) = order {
            gallery.order = order;
        }
        if let Some(hidden) = hidden {
            gallery.hidden = hidden;
        }
        if let Some(snapshots) = snapshots {
            gallery.snapshots = snapshots;
        }
        if let Some(names) = names {
            gallery.names = names;
        }
        if *gallery != before {
            self.save();
        }
        reply(200, json!({"saved": true}))
    }

    /// Every camera for the gallery, in the owner's order, with hidden ones marked.
    fn gallery(&self) -> Vec<crate::cameras::Camera> {
        let arranged = &self.store.gallery;
        // Two servers can share an entity ID; the gallery tells cameras apart by ID, so prefix
        // each with its server's place in pairing order. That stays the same whichever servers
        // have sent cameras since Luma started, while the servers stay paired.
        let clients = &self.store.clients;
        let mut cameras: Vec<(&Camera, String)> = self
            .cameras
            .iter()
            .enumerate()
            .flat_map(|(index, (owner, cameras))| {
                let server = clients
                    .iter()
                    .position(|client| client.token == *owner)
                    .unwrap_or(index);
                cameras
                    .iter()
                    .map(move |camera| (camera, format!("{server}/{}", camera.id)))
            })
            .take(MAX_CAMERAS)
            .collect();
        // The owner's order first; cameras never placed keep Home Assistant's order after them.
        cameras.sort_by_key(|(_, id)| {
            arranged
                .order
                .iter()
                .position(|item| item == id)
                .unwrap_or(usize::MAX)
        });
        cameras
            .into_iter()
            .map(|(camera, id)| crate::cameras::Camera {
                hidden: arranged.hidden.contains(&id),
                label: arranged.names.get(&id).cloned(),
                id,
                name: camera.name.clone(),
                stream: camera.stream.clone(),
                snapshot: camera.snapshot.clone(),
                hls: camera.hls.clone(),
                history: camera.history.clone(),
            })
            .collect()
    }

    /// A gallery camera (`0/camera.door`) as Home Assistant last sent it.
    fn gallery_camera(&self, id: &str) -> Option<Camera> {
        let (server, entity) = id.split_once('/')?;
        let server: usize = server.parse().ok()?;
        let owner = &self.store.clients.get(server)?.token;
        self.cameras
            .iter()
            .find(|(key, _)| key == owner)?
            .1
            .iter()
            .find(|camera| camera.id == entity)
            .cloned()
    }

    /// Redirects a piece of the session's recorded video to the camera's history endpoint.
    fn recorded_piece(&self, request: &str) -> Reply {
        let Some(session) = self.session.as_ref().filter(|session| {
            query(request, "session") == Some(session.id.as_str())
                && query(request, "nonce") == Some(session.nonce.as_str())
        }) else {
            return error(403, "No such camera video");
        };
        let number = |key| query(request, key).and_then(|value| value.parse::<u64>().ok());
        let (Some(start), Some(end)) = (number("start"), number("end")) else {
            return error(400, "start and end are required");
        };
        if end <= start || end - start > MAX_RECORDED_PIECE {
            return error(400, "At most 15 minutes of video at once");
        }
        let Some(history) = session
            .recorded
            .as_deref()
            .and_then(|id| self.gallery_camera(id))
            .and_then(|camera| camera.history)
        else {
            return error(404, "This camera has no recordings");
        };
        Reply {
            status: 302,
            body: json!({}),
            location: Some(format!("{history}&start={start}&end={end}")),
            events: None,
        }
    }

    fn send_cameras(&self, open: bool) {
        let _ = self.events.send(Event::Cameras {
            cameras: self.gallery(),
            live: !self.store.gallery.snapshots,
            open,
        });
    }

    /// Plays a camera's HLS stream full screen in the overlay app, above the Cameras gallery.
    /// Whatever happens, the gallery hears back through [`Event::VideoClosed`].
    pub fn show_video(&mut self, video: &crate::cameras::VideoRequest, now: Instant) {
        let failed = |api: &mut Self| {
            let _ = api.events.send(Event::VideoClosed(crate::cameras::Closed {
                failed: true,
                ..Default::default()
            }));
        };
        if !self.overlay_ready.load(Ordering::Acquire) {
            self.prepare();
            failed(self);
            return;
        }
        let (id, nonce) = match (random_hex(8), random_hex(16)) {
            (Ok(id), Ok(nonce)) => (id, nonce),
            _ => return failed(self),
        };
        // A doorbell card still up is replaced; keep the media it paused.
        let media = self.session.take().and_then(|session| session.media);
        // Recorded video: the page loads it a piece at a time through `/api/overlay/recorded`.
        let params = match &video.recorded {
            Some((_, start)) => json!({
                "mode": "recorded",
                "session": id,
                "nonce": nonce,
                "port": PORT,
                "start": start,
                "title": video.title,
                "snapshot": video.snapshot,
            }),
            None => json!({
                "mode": "video",
                "session": id,
                "nonce": nonce,
                "port": PORT,
                "url": video.url,
                "title": video.title,
                "snapshot": video.snapshot,
                "history": video.history,
            }),
        };
        self.session = Some(Session {
            recorded: video.recorded.as_ref().map(|(camera, _)| camera.clone()),
            id,
            nonce,
            media,
            ends: now + VIDEO_TTL,
            video: true,
        });
        if let Err(error) = self.tv.launch_overlay(&params) {
            crate::telemetry::log_warn!("overlay", "video: {error}");
            self.finish_session(crate::cameras::Closed {
                failed: true,
                ..Default::default()
            });
        }
    }

    /// Closes the camera video when the gallery leaves the camera some other way.
    pub fn close_video(&mut self) {
        if self.session.as_ref().is_some_and(|session| session.video)
            && let Some(session) = self.session.take()
        {
            let _ = self.tv.close_overlay();
            // Resume media a doorbell card had paused before the video replaced it. No
            // VideoClosed: the gallery left this camera itself, and may already be starting
            // another camera's video that a late event would cancel.
            if let Some(media) = session.media
                && let Err(error) = self.tv.resume_media(&media)
            {
                crate::telemetry::log_warn!("overlay", "resume: {error}");
            }
        }
    }

    /// Opens the Cameras gallery, Luma's built-in Cameras app.
    pub fn show_cameras(&mut self) {
        if self.store.clients.is_empty() {
            let _ = self.events.send(Event::Notice("camerasNotPaired"));
        } else if self.all_cameras().is_empty() {
            let _ = self.events.send(Event::Notice("camerasNone"));
        } else {
            self.send_cameras(true);
        }
    }
}

/// Atomically replaces `path` with a file only Luma can read (mode 0600 on Unix).
pub(crate) fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
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
    Arrange(Value),
    Video(crate::cameras::VideoRequest),
    CloseVideo,
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

    /// Opens the Cameras gallery: the cameras come back as [`Event::Cameras`], problems as
    /// [`Event::Notice`].
    pub fn show_cameras(&self) {
        if self.enabled {
            let _ = self.commands.send(Command::ShowCameras);
        }
    }

    /// Plays a camera's HLS stream in the overlay app; the result is [`Event::VideoClosed`].
    pub fn show_video(&self, video: crate::cameras::VideoRequest) {
        if self.enabled {
            let _ = self.commands.send(Command::Video(video));
        }
    }

    pub fn close_video(&self) {
        if self.enabled {
            let _ = self.commands.send(Command::CloseVideo);
        }
    }

    /// Saves how the gallery is arranged: `{"order", "hidden", "live"}`.
    pub fn arrange(&self, arrangement: Value) {
        if self.enabled {
            let _ = self.commands.send(Command::Arrange(arrangement));
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
            Some(Command::ShowCameras) => api.show_cameras(),
            Some(Command::Video(video)) => api.show_video(&video, Instant::now()),
            Some(Command::CloseVideo) => api.close_video(),
            Some(Command::Arrange(arrangement)) => {
                let saved = api.arrange(&arrangement);
                if saved.status != 200 {
                    crate::telemetry::log_warn!("cameras", "arrangement not saved: {}", saved.body);
                }
            }
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
        let (path, query) = request
            .target
            .split_once('?')
            .unwrap_or((&request.target, ""));
        Ok(api.handle(
            &Request {
                method: &request.method,
                path,
                query,
                authorization: request.headers.get("authorization").map(String::as_str),
                origin: request.headers.get("origin").map(String::as_str),
                integration: request
                    .headers
                    .get("x-luma-integration")
                    .map(String::as_str),
                peer: peer.ip(),
                body: &body,
            },
            Instant::now(),
        ))
    })();
    let reply = result.unwrap_or_else(|(status, message)| error(status, &message));
    if let (200, Some(subscription)) = (reply.status, reply.events.clone()) {
        let (pairings, streams) = (api.pairings.clone(), api.streams.clone());
        let generation = pairings.load(Ordering::Acquire);
        let Subscription { cursor, owner, id } = *subscription;
        crate::telemetry::stream(stream, cursor, move || {
            pairings.load(Ordering::Acquire) == generation
                && streams
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&owner)
                    == Some(&id)
        });
        return;
    }
    if let (302, Some(location)) = (reply.status, &reply.location) {
        use std::io::Write;
        let _ = write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n\r\n"
        );
        return;
    }
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
