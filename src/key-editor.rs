//! Streaming search keys entered from the owner's phone or computer. While the TV shows the QR
//! dialog this worker serves a small page; each key is checked against its API before it is
//! saved, so a typo is caught on the phone rather than at the first search. Keys never travel
//! back to the page: it only sees whether each one is set and its last four characters.
use crate::companion::{
    Request, Session, error_response, listen, local_peer, random_token, read_exact,
    redirect_to_page, response, session,
};
use crate::openapi::Error;
use crate::streaming_keys::{self, Keys, Status, clean, mask};
use serde::Deserialize;
use std::{
    io::BufReader,
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const MAX_BODY: usize = 4096;
const SESSION_TIME: Duration = Duration::from_secs(600);
const SESSION_REQUEST_LIMIT: usize = 128;
// Long enough for both keys to be checked against their APIs.
const REQUEST_TIME: Duration = Duration::from_secs(30);
const STOP: u64 = u64::MAX;
/// Ghostbusters (1984): any title works, this one is certain to exist on TMDB.
const CHECK_IMDB_ID: &str = "tt0087332";

pub enum Event {
    Session(u64, Result<Session, String>),
    /// The keys were saved; the TV refreshes what Settings shows.
    Saved(Status),
    Ended(u64),
}

pub struct Service {
    desired: Arc<AtomicU64>,
    wake: mpsc::Sender<()>,
    pub receiver: mpsc::Receiver<Event>,
    active: bool,
    generation: u64,
}

impl Service {
    pub fn new(path: PathBuf, loopback_only: bool) -> Result<Self, String> {
        let desired = Arc::new(AtomicU64::new(0));
        let worker_desired = desired.clone();
        let (sender, receiver) = mpsc::sync_channel(4);
        let (wake, updates) = mpsc::channel();
        thread::Builder::new()
            .name("luma-key-editor".into())
            .spawn(move || worker(&worker_desired, &path, &sender, &updates, loopback_only))
            .map_err(|e| format!("Could not start key editor worker: {e}"))?;
        Ok(Self {
            desired,
            wake,
            receiver,
            active: false,
            generation: 0,
        })
    }

    pub fn set_active(&mut self, active: bool) {
        if self.active != active {
            self.active = active;
            self.generation += 1;
            self.desired
                .store(if active { self.generation } else { 0 }, Ordering::Release);
            let _ = self.wake.send(());
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.desired.store(STOP, Ordering::Release);
        let _ = self.wake.send(());
    }
}

/// What the page submits. A missing field keeps the stored key; an empty one removes it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Change {
    motn: Option<String>,
    tmdb: Option<String>,
}

/// Applies a change without checking keys against their APIs.
fn apply(keys: &Keys, change: &Change) -> Result<Keys, String> {
    let mut next = keys.clone();
    next.home_assistant = false;
    if let Some(motn) = &change.motn {
        next.motn = clean(motn).map_err(|e| format!("Movie of the Night: {e}"))?;
    }
    if let Some(tmdb) = &change.tmdb {
        next.tmdb = clean(tmdb).map_err(|e| format!("TMDB: {e}"))?;
    }
    Ok(next)
}

/// The outcome of trying a key once against its API.
#[derive(Debug, PartialEq, Eq)]
enum Check {
    Accepted,
    /// The service refused the key: it is wrong, revoked or not subscribed.
    Rejected(String),
    /// The service could not be reached; the key is saved unchecked.
    Unchecked(String),
}

fn classify(result: Result<(), Error>) -> Check {
    match result {
        Ok(()) => Check::Accepted,
        Err(Error::Status { status, message }) if matches!(status, 401 | 403) => {
            Check::Rejected(if message.is_empty() {
                format!("HTTP {status}")
            } else {
                message
            })
        }
        Err(error) => Check::Unchecked(error.to_string()),
    }
}

fn check_motn(keys: &Keys) -> Check {
    let Some(client) = keys.motn() else {
        return Check::Accepted;
    };
    classify(
        client
            .get_genres(&crate::motn_api::GetGenresParams::default())
            .map(drop),
    )
}

fn check_tmdb(keys: &Keys) -> Check {
    let Some(client) = keys.tmdb() else {
        return Check::Accepted;
    };
    classify(
        client
            .find_by_id(&crate::tmdb_api::FindByIdParams {
                external_id: CHECK_IMDB_ID.into(),
                external_source: crate::tmdb_api::ExternalSource::ImdbId,
                language: None,
            })
            .map(drop),
    )
}

/// Checks each changed key, then reports what the page should say. `Err` means nothing is saved.
fn verify(
    current: &Keys,
    next: &Keys,
    motn: impl FnOnce(&Keys) -> Check,
    tmdb: impl FnOnce(&Keys) -> Check,
) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    let mut rejected = Vec::new();
    for (name, changed, check) in [
        (
            "Movie of the Night",
            next.motn != current.motn && !next.motn.is_empty(),
            Box::new(motn) as Box<dyn FnOnce(&Keys) -> Check>,
        ),
        (
            "TMDB",
            next.tmdb != current.tmdb && !next.tmdb.is_empty(),
            Box::new(tmdb),
        ),
    ] {
        if !changed {
            continue;
        }
        match check(next) {
            Check::Accepted => {}
            Check::Rejected(message) => {
                rejected.push(format!("{name} refused this key ({message})."))
            }
            Check::Unchecked(message) => warnings.push(format!(
                "Saved, but {name} could not be reached to check the key ({message})."
            )),
        }
    }
    if rejected.is_empty() {
        Ok(warnings)
    } else {
        Err(format!("{} Nothing was saved.", rejected.join(" ")))
    }
}

/// The page's whole view of the stored keys: set or not, and the last four characters.
fn listing(keys: &Keys) -> serde_json::Value {
    let status = keys.status();
    serde_json::json!({
        "motn": {"set": status.motn, "hint": mask(&keys.motn)},
        "tmdb": {"set": status.tmdb, "hint": mask(&keys.tmdb)},
    })
}

fn worker(
    desired: &AtomicU64,
    path: &Path,
    sender: &mpsc::SyncSender<Event>,
    wake: &mpsc::Receiver<()>,
    loopback_only: bool,
) {
    let mut handled = 0;
    loop {
        let generation = desired.load(Ordering::Acquire);
        if generation == STOP {
            break;
        }
        if generation == 0 || generation == handled {
            if wake.recv().is_err() {
                break;
            }
            continue;
        }
        handled = generation;
        let opened = (|| {
            let listener = listen(loopback_only)?;
            let token = random_token()?;
            let address = listener.local_addr().map_err(|e| e.to_string())?;
            let info = session(address, &token)?;
            Ok::<_, String>((listener, token, address, info))
        })();
        let (listener, token, address, info) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                let _ = sender.send(Event::Session(generation, Err(error)));
                continue;
            }
        };
        if sender.send(Event::Session(generation, Ok(info))).is_err() {
            break;
        }
        let deadline = Instant::now() + SESSION_TIME;
        let mut requests = 0;
        while desired.load(Ordering::Acquire) == generation
            && Instant::now() < deadline
            && requests < SESSION_REQUEST_LIMIT
        {
            match listener.accept() {
                Ok((mut stream, peer)) => {
                    if !local_peer(peer.ip()) {
                        continue;
                    }
                    requests += 1;
                    let valid = || {
                        desired.load(Ordering::Acquire) == generation && Instant::now() < deadline
                    };
                    let host = address.to_string();
                    let store = |keys: &Keys| {
                        if !valid() {
                            return Err("Session ended".to_owned());
                        }
                        streaming_keys::save(path, keys)?;
                        sender
                            .send(Event::Saved(keys.status()))
                            .map_err(|_| "Launcher stopped".to_owned())
                    };
                    if let Err(error) = serve(&mut stream, &host, &token, path, valid, store) {
                        crate::telemetry::log_warn!("key-editor", "{error}");
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(25))
                }
                Err(error) => {
                    crate::telemetry::log_warn!("key-editor", "server: {error}");
                    break;
                }
            }
        }
        drop(listener);
        if sender.send(Event::Ended(generation)).is_err() {
            break;
        }
    }
}

fn json(stream: &mut TcpStream, status: &str, body: &serde_json::Value) -> Result<(), String> {
    response(
        stream,
        status,
        "application/json; charset=utf-8",
        body.to_string().as_bytes(),
    )
}

/// Serves one request. Saved keys are written by `store` before the success reply.
fn serve(
    stream: &mut TcpStream,
    host: &str,
    token: &str,
    path: &Path,
    valid: impl Fn() -> bool,
    store: impl FnOnce(&Keys) -> Result<(), String>,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + REQUEST_TIME;
    let mut reader = BufReader::new(&mut *stream);
    let request = match crate::companion::request(
        &mut reader,
        &valid,
        deadline,
        MAX_BODY,
        "Request too large",
    ) {
        Ok(request) => request,
        Err(error) => return error_response(stream, "400 Bad Request", &error),
    };
    if request.foreign(host) {
        return error_response(stream, "403 Forbidden", "Open the address shown on your TV");
    }
    if !valid() {
        return Err("Session ended".into());
    }
    if request.method == "GET" && request.length == 0 && request.target == format!("/{token}") {
        return redirect_to_page(stream, token);
    }
    let Some(route) = request.target.strip_prefix(&format!("/{token}/")) else {
        return error_response(stream, "404 Not Found", "Session not found");
    };
    if request.method == "GET" && request.length == 0 {
        if route == "keys.json" {
            return json(stream, "200 OK", &listing(&streaming_keys::load(path)));
        }
        let (kind, body): (&str, &[u8]) = match route {
            "" => (
                "text/html; charset=utf-8",
                include_bytes!("../assets/key-editor/index.html"),
            ),
            // The shortcut editor's styles: both pages are the same kind of small form.
            "app.css" => (
                "text/css; charset=utf-8",
                include_bytes!("../assets/tile-editor/app.css"),
            ),
            "app.js" => (
                "text/javascript; charset=utf-8",
                include_bytes!("../assets/key-editor/app.js"),
            ),
            "keys.js" => (
                "text/javascript; charset=utf-8",
                include_bytes!("../assets/key-editor/keys.js"),
            ),
            "luma-mark.svg" => ("image/svg+xml", include_bytes!("../assets/app-icon.svg")),
            _ => return error_response(stream, "404 Not Found", "Not found"),
        };
        return response(stream, "200 OK", kind, body);
    }
    if request.method != "POST"
        || route != "save"
        || request.headers.get("x-luma-token").map(String::as_str) != Some(token)
    {
        return error_response(stream, "403 Forbidden", "Invalid request");
    }
    if request
        .headers
        .get("content-type")
        .is_none_or(|value| value.split(';').next() != Some("application/json"))
    {
        return error_response(stream, "415 Unsupported Media Type", "Send JSON");
    }
    let change = match read_change(&mut reader, &request, &valid, deadline) {
        Ok(change) => change,
        Err(error) => {
            drop(reader);
            return error_response(stream, "400 Bad Request", &error);
        }
    };
    drop(reader);
    let current = streaming_keys::load(path);
    let next = match apply(&current, &change) {
        Ok(next) => next,
        Err(error) => return error_response(stream, "422 Unprocessable Content", &error),
    };
    let warnings = match verify(&current, &next, check_motn, check_tmdb) {
        Ok(warnings) => warnings,
        Err(error) => return error_response(stream, "422 Unprocessable Content", &error),
    };
    if !valid() {
        return Err("Session ended".into());
    }
    store(&next)?;
    let mut body = listing(&next);
    body["warnings"] = serde_json::json!(warnings);
    // Saved even if the phone disconnects before receiving its reply.
    let _ = json(stream, "200 OK", &body);
    Ok(())
}

fn read_change(
    reader: &mut impl std::io::Read,
    request: &Request,
    valid: &impl Fn() -> bool,
    deadline: Instant,
) -> Result<Change, String> {
    let mut body = vec![0; request.length];
    read_exact(reader, &mut body, valid, deadline)?;
    serde_json::from_slice(&body).map_err(|_| "Invalid keys".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOTN: &str = "motn-key-0123456789abcdef";
    const TMDB: &str = "eyJhbGciOiJIUzI1NiJ9.e30.signature";

    fn keys(motn: &str, tmdb: &str) -> Keys {
        Keys {
            motn: motn.into(),
            tmdb: tmdb.into(),
            home_assistant: false,
        }
    }

    #[test]
    fn missing_fields_keep_keys_and_empty_ones_remove_them() {
        let current = keys(MOTN, TMDB);
        let change: Change = serde_json::from_str(r#"{"tmdb": ""}"#).unwrap();
        assert_eq!(apply(&current, &change).unwrap(), keys(MOTN, ""));
        let change: Change = serde_json::from_str("{}").unwrap();
        assert_eq!(apply(&current, &change).unwrap(), current);
        let change: Change = serde_json::from_str(r#"{"motn": " short "}"#).unwrap();
        assert!(
            apply(&current, &change)
                .unwrap_err()
                .starts_with("Movie of the Night")
        );
    }

    #[test]
    fn only_changed_keys_are_checked() {
        let current = keys(MOTN, TMDB);
        let next = keys(MOTN, "eyJhbGciOiJIUzI1NiJ9.e30.another");
        let warnings = verify(
            &current,
            &next,
            |_| panic!("unchanged Movie of the Night key was checked"),
            |_| Check::Accepted,
        )
        .unwrap();
        assert!(warnings.is_empty());
        // Removing a key needs no check.
        verify(
            &current,
            &keys("", TMDB),
            |_| panic!("checked a removed key"),
            |_| panic!("checked an unchanged key"),
        )
        .unwrap();
    }

    #[test]
    fn a_refused_key_saves_nothing_and_an_unreachable_service_saves_with_a_warning() {
        let current = Keys::default();
        let next = keys(MOTN, TMDB);
        let error = verify(
            &current,
            &next,
            |_| Check::Rejected("You are not subscribed to this API.".into()),
            |_| Check::Accepted,
        )
        .unwrap_err();
        assert!(error.contains("Movie of the Night refused"), "{error}");
        assert!(error.ends_with("Nothing was saved."), "{error}");
        let warnings = verify(
            &current,
            &next,
            |_| Check::Accepted,
            |_| Check::Unchecked("Network error: timeout".into()),
        )
        .unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("Saved, but TMDB"), "{warnings:?}");
    }

    #[test]
    fn only_auth_failures_count_as_refusals() {
        let status = |status, message: &str| {
            Err(Error::Status {
                status,
                message: message.into(),
            })
        };
        assert_eq!(classify(Ok(())), Check::Accepted);
        assert_eq!(
            classify(status(401, "Invalid API key")),
            Check::Rejected("Invalid API key".into())
        );
        assert_eq!(
            classify(status(403, "")),
            Check::Rejected("HTTP 403".into())
        );
        assert!(matches!(classify(status(500, "")), Check::Unchecked(_)));
        assert!(matches!(
            classify(Err(Error::Network("timeout".into()))),
            Check::Unchecked(_)
        ));
    }

    #[test]
    fn the_page_never_receives_a_key() {
        let body = listing(&keys(MOTN, TMDB)).to_string();
        assert!(!body.contains("0123456789"), "{body}");
        assert!(!body.contains("signature"), "{body}");
        assert!(body.contains("…cdef") && body.contains("…ture"), "{body}");
    }
}
