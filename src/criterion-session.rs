//! The Criterion Channel sign-in, kept so the player never has to sign in again.
//!
//! The Channel's site keeps its sign-in in cookies: `__session` on `www.criterionchannel.com`
//! (HttpOnly, about 30 days, renewed as the site is used) and Auth0's own on
//! `login.criterion.com`. Luma keeps the latest set in a file only Luma can read, beside the
//! Criterion account, and:
//!
//! - **before each film**, the player's start page asks Luma to get it ready
//!   (`POST /api/criterion/prepare`). Luma reads the player's cookies through the web engine's
//!   DevTools (`devtools`), keeps whichever sign-in lasts longer, and gives the player the stored
//!   one when it is newer. With neither, it takes the TV's Web Browser's sign-in, if any.
//! - **in the background**, every few days, it uses the stored sign-in once over HTTPS, which
//!   keeps it alive, and keeps whatever the Channel renews. A sign-in the Channel no longer
//!   accepts is marked invalid and never given to the player.
//! - **Home Assistant** keeps a copy (`GET`/`PUT /api/criterion-session`) and sends it back when
//!   the TV has none, an older one or an invalid one.
//!
//! The cookies are never logged and never leave the TV except to Home Assistant over the paired
//! API and to the Channel itself.
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The Channel's own sign-in cookie, the one that says how long a sign-in lasts.
pub const SESSION_COOKIE: &str = "__session";
const SITE: &str = "www.criterionchannel.com";
const MAX_COOKIES: usize = 64;
/// How often the background check uses the sign-in, well inside its 30 days.
pub const RENEW_EVERY: Duration = Duration::from_secs(3 * 24 * 60 * 60);
/// How often the background check wakes to see whether that is due.
const WAKE_EVERY: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// `.criterionchannel.com` for a domain cookie, `www.criterionchannel.com` for a host one.
    pub domain: String,
    #[serde(default = "root_path")]
    pub path: String,
    /// Unix seconds; none for a cookie that lasts as long as the browser.
    #[serde(default)]
    pub expires: Option<f64>,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub same_site: Option<String>,
}

fn root_path() -> String {
    "/".into()
}

impl std::fmt::Debug for Cookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cookie")
            .field("name", &self.name)
            .field("domain", &self.domain)
            .finish_non_exhaustive()
    }
}

impl Cookie {
    fn host(&self) -> &str {
        self.domain.trim_start_matches('.')
    }

    fn is_session(&self) -> bool {
        self.name == SESSION_COOKIE && matches!(self.host(), SITE | "criterionchannel.com")
    }

    fn expired(&self, now: f64) -> bool {
        self.expires.is_some_and(|at| at <= now)
    }

    /// Whether a request to `host` at `path` carries this cookie.
    fn sent_to(&self, host: &str, path: &str) -> bool {
        let domain_match = if self.domain.starts_with('.') {
            host == self.host() || host.ends_with(&self.domain)
        } else {
            host == self.domain
        };
        domain_match && path.starts_with(&self.path)
    }

    /// As DevTools' `Network.setCookies` takes it: a host cookie by its URL, so it stays a host
    /// cookie; a domain cookie by its domain.
    pub fn to_devtools(&self) -> Value {
        let mut cookie = json!({
            "name": self.name,
            "value": self.value,
            "path": self.path,
            "secure": self.secure,
            "httpOnly": self.http_only,
        });
        if self.domain.starts_with('.') {
            cookie["domain"] = json!(self.domain);
        } else {
            cookie["url"] = json!(format!("https://{}{}", self.domain, self.path));
        }
        if let Some(expires) = self.expires {
            cookie["expires"] = json!(expires);
        }
        if let Some(same_site) = &self.same_site {
            cookie["sameSite"] = json!(same_site);
        }
        cookie
    }

    /// From DevTools' `Network.getCookies`, where a browser-session cookie has `session: true`.
    pub fn from_devtools(value: &Value) -> Option<Self> {
        let text = |key: &str| value.get(key)?.as_str().map(str::to_owned);
        let flag = |key: &str| value.get(key).and_then(Value::as_bool).unwrap_or(false);
        let session = flag("session");
        Some(Self {
            name: text("name")?,
            value: text("value")?,
            domain: text("domain")?,
            path: text("path").unwrap_or_else(root_path),
            expires: (!session)
                .then(|| value.get("expires").and_then(Value::as_f64))
                .flatten()
                .filter(|at| *at > 0.),
            secure: flag("secure"),
            http_only: flag("httpOnly"),
            same_site: text("sameSite"),
        })
    }
}

fn allowed_domain(domain: &str) -> bool {
    let host = domain.trim_start_matches('.');
    ["criterionchannel.com", "criterion.com"]
        .iter()
        .any(|site| host == *site || host.ends_with(&format!(".{site}")))
}

/// Only the Channel's cookies, within the sizes a browser keeps, with its sign-in among them.
pub fn validate(cookies: Vec<Cookie>) -> Result<Vec<Cookie>, &'static str> {
    if cookies.is_empty() || cookies.len() > MAX_COOKIES {
        return Err("cookies must be a list of 1 to 64 cookies");
    }
    let printable = |text: &str| !text.chars().any(char::is_control);
    for cookie in &cookies {
        if cookie.name.is_empty()
            || cookie.name.len() > 256
            || cookie.value.len() > 8192
            || cookie.path.len() > 1024
            || !cookie.path.starts_with('/')
            || !printable(&cookie.name)
            || !printable(&cookie.value)
            || !allowed_domain(&cookie.domain)
        {
            return Err("each cookie must be a Criterion Channel cookie");
        }
        if cookie
            .same_site
            .as_deref()
            .is_some_and(|value| !matches!(value, "Lax" | "Strict" | "None"))
        {
            return Err("sameSite must be Lax, Strict or None");
        }
    }
    if expires(&cookies).is_none() {
        return Err("the cookies must include the Channel's __session sign-in");
    }
    Ok(cookies)
}

/// When the sign-in in `cookies` ends: its `__session` cookie's expiry.
pub fn expires(cookies: &[Cookie]) -> Option<f64> {
    cookies
        .iter()
        .filter(|cookie| cookie.is_session())
        .filter_map(|cookie| cookie.expires)
        .reduce(f64::max)
}

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0., |elapsed| elapsed.as_secs_f64())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub cookies: Vec<Cookie>,
    /// When the background check last found the sign-in accepted (unix seconds).
    #[serde(default)]
    pub checked: Option<f64>,
    /// False once the Channel turned it away; never given to the player then.
    #[serde(default = "yes")]
    pub valid: bool,
}

fn yes() -> bool {
    true
}

impl Session {
    pub fn new(cookies: Vec<Cookie>) -> Self {
        Self {
            cookies,
            checked: None,
            valid: true,
        }
    }

    pub fn expires(&self) -> Option<f64> {
        expires(&self.cookies)
    }

    /// Still usable: accepted when last checked, and not yet expired.
    pub fn usable(&self, now: f64) -> bool {
        self.valid && self.expires().is_some_and(|at| at > now)
    }

    /// What `/api/status` reports. Never the cookies.
    pub fn status(&self) -> Value {
        json!({
            "expires": self.expires(),
            "checked": self.checked,
            "valid": self.valid,
        })
    }
}

/// Whether `candidate` should replace `current`: it is usable and lasts longer, or `current` is
/// missing or no longer usable.
pub fn better(candidate: &[Cookie], current: Option<&Session>, now: f64) -> bool {
    let Some(ends) = expires(candidate).filter(|at| *at > now) else {
        return false;
    };
    match current {
        Some(current) if current.usable(now) => current.expires().is_none_or(|at| ends > at),
        _ => true,
    }
}

// ---- Storage --------------------------------------------------------------------------------

/// The API thread (Home Assistant, the player) and the background check both save.
static FILE: Mutex<()> = Mutex::new(());

fn file() -> std::sync::MutexGuard<'static, ()> {
    FILE.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Beside the Criterion account, beside the pairings.
pub fn path(store: &Path) -> PathBuf {
    store.with_file_name("criterion-session.json")
}

pub fn load(path: &Path) -> Option<Session> {
    let _guard = file();
    read(path)
}

fn read(path: &Path) -> Option<Session> {
    let session: Session = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let cookies = validate(session.cookies).ok()?;
    Some(Session { cookies, ..session })
}

fn write(path: &Path, session: &Session) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec(session).map_err(|e| e.to_string())?;
    crate::home_assistant::write_private(path, &bytes)
}

/// Keeps `cookies` when they are better than what is stored (see `better`). Returns whether it
/// did, and the session now stored.
pub fn offer(
    path: &Path,
    cookies: Vec<Cookie>,
    now: f64,
) -> Result<(bool, Option<Session>), String> {
    let _guard = file();
    let current = read(path);
    if !better(&cookies, current.as_ref(), now) {
        return Ok((false, current));
    }
    let session = Session::new(cookies);
    write(path, &session)?;
    Ok((true, Some(session)))
}

pub fn forget(path: &Path) -> Result<(), String> {
    let _guard = file();
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

// ---- Getting the player ready ---------------------------------------------------------------

/// What `prepare` did, for the log and the player.
#[derive(Debug, PartialEq, Eq)]
pub enum Prepared {
    /// The player already had the longest-lasting sign-in (Luma kept a copy if it was new).
    PlayerCurrent,
    /// The player was given the stored sign-in.
    Restored,
    /// The player was given the Web Browser's sign-in.
    FromBrowser,
    /// No sign-in anywhere: the player signs in on the Channel's page.
    None,
}

/// Before a film: the longest-lasting sign-in among the player's, the stored one and the Web
/// Browser's goes to the player, and Luma keeps it. Blocking, a second or so at most.
pub fn prepare(path: &Path) -> Result<Prepared, String> {
    let now = now();
    let mut devtools = crate::devtools::DevTools::page(crate::criterion::PLAYER)?;
    let reply = devtools.call(
        "Network.getCookies",
        json!({"urls": [
            format!("https://{SITE}/"),
            "https://login.criterion.com/",
            "https://criterion.com/",
        ]}),
    )?;
    let player: Vec<Cookie> = reply
        .get("cookies")
        .and_then(Value::as_array)
        .map(|cookies| cookies.iter().filter_map(Cookie::from_devtools).collect())
        .unwrap_or_default();
    let player = player
        .into_iter()
        .filter(|cookie| allowed_domain(&cookie.domain))
        .collect::<Vec<_>>();
    // The player's own sign-in, renewed as it was used, is kept when it lasts longest.
    if let Ok(cookies) = validate(player.clone())
        && offer(path, cookies, now)?.0
    {
        crate::telemetry::log_info!("criterion", "kept the player's sign-in");
    }
    let player_ends = expires(&player).filter(|at| *at > now);
    let stored = load(path).filter(|session| session.usable(now));
    if let Some(stored) = stored {
        if player_ends.is_some_and(|at| Some(at) >= stored.expires()) {
            return Ok(Prepared::PlayerCurrent);
        }
        give(&mut devtools, &stored.cookies)?;
        return Ok(Prepared::Restored);
    }
    if player_ends.is_some() {
        return Ok(Prepared::PlayerCurrent);
    }
    match browser_cookies() {
        Ok(cookies) => match validate(cookies) {
            Ok(cookies) if expires(&cookies).is_some_and(|at| at > now) => {
                give(&mut devtools, &cookies)?;
                offer(path, cookies, now)?;
                Ok(Prepared::FromBrowser)
            }
            _ => Ok(Prepared::None),
        },
        Err(error) => {
            crate::telemetry::log_info!("criterion", "no Web Browser sign-in to use: {error}");
            Ok(Prepared::None)
        }
    }
}

fn give(devtools: &mut crate::devtools::DevTools, cookies: &[Cookie]) -> Result<(), String> {
    let cookies: Vec<Value> = cookies.iter().map(Cookie::to_devtools).collect();
    devtools
        .call("Network.setCookies", json!({ "cookies": cookies }))
        .map(|_| ())
}

/// The TV's Web Browser keeps its own Chromium profile; its cookie database is SQLite, read with
/// the TV's Python (Luma has no SQLite of its own). Needs root, as Luma has when it is Home.
const BROWSER_COOKIES: &str =
    "/var/palm/jail/com.webos.app.browser/mnt/lg/cache/webbrowser/chrome/Cookies";
const BROWSER_EXPORT: &str = r#"
import json, os, shutil, sqlite3, sys, tempfile
fd, copy = tempfile.mkstemp()
os.close(fd)
try:
    shutil.copy(sys.argv[1], copy)
    db = sqlite3.connect(copy)
    same = {0: "None", 1: "Lax", 2: "Strict"}
    out = []
    for host, name, value, path, expires, secure, http, persistent, samesite in db.execute(
        "select host_key, name, value, path, expires_utc, is_secure, is_httponly, is_persistent, samesite "
        "from cookies where host_key like '%criterion%'"):
        out.append({"name": name, "value": value, "domain": host, "path": path or "/",
                    "expires": expires / 1e6 - 11644473600 if persistent and expires else None,
                    "secure": bool(secure), "httpOnly": bool(http), "sameSite": same.get(samesite)})
    json.dump(out, sys.stdout)
finally:
    os.remove(copy)
"#;

fn browser_cookies() -> Result<Vec<Cookie>, String> {
    if !Path::new(BROWSER_COOKIES).exists() {
        return Err("no Web Browser cookies on this TV (or no access to them)".into());
    }
    let output = std::process::Command::new("python3")
        .args(["-c", BROWSER_EXPORT, BROWSER_COOKIES])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("python3: {e}"))?;
    if !output.status.success() {
        return Err(format!("reading them failed ({})", output.status));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

// ---- Keeping it alive -----------------------------------------------------------------------

/// Applies a reply's `Set-Cookie` header to `cookies`, for a request to `host`.
pub fn apply_set_cookie(cookies: &mut Vec<Cookie>, header: &str, host: &str, now: f64) {
    let mut parts = header.split(';');
    let Some((name, value)) = parts.next().and_then(|pair| pair.split_once('=')) else {
        return;
    };
    let mut cookie = Cookie {
        name: name.trim().into(),
        value: value.trim().into(),
        domain: host.into(),
        path: "/".into(),
        expires: None,
        secure: false,
        http_only: false,
        same_site: None,
    };
    let mut max_age = None;
    for attribute in parts {
        let (key, value) = attribute.split_once('=').unwrap_or((attribute, ""));
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "domain" if !value.is_empty() => {
                cookie.domain = format!(".{}", value.trim_start_matches('.'));
            }
            "path" if value.starts_with('/') => cookie.path = value.into(),
            "expires" => cookie.expires = cookie.expires.or(http_date(value)),
            "max-age" => max_age = value.parse::<f64>().ok(),
            "secure" => cookie.secure = true,
            "httponly" => cookie.http_only = true,
            "samesite" => {
                let mut same_site = value.to_ascii_lowercase();
                if let Some(first) = same_site.get_mut(0..1) {
                    first.make_ascii_uppercase();
                }
                cookie.same_site = Some(same_site);
            }
            _ => {}
        }
    }
    if let Some(max_age) = max_age {
        cookie.expires = Some(now + max_age);
    }
    if !allowed_domain(&cookie.domain) {
        return;
    }
    cookies.retain(|old| {
        !(old.name == cookie.name && old.domain == cookie.domain && old.path == cookie.path)
    });
    if !cookie.expired(now) {
        cookies.push(cookie);
    }
}

/// `Wed, 21 Oct 2026 07:28:00 GMT` as unix seconds.
fn http_date(text: &str) -> Option<f64> {
    let mut parts = text.split([' ', ',', '-', ':']).filter(|p| !p.is_empty());
    let _weekday = parts.next()?;
    let day: i64 = parts.next()?.parse().ok()?;
    let month = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .iter()
    .position(|m| {
        parts
            .clone()
            .next()
            .is_some_and(|p| p.to_ascii_lowercase().starts_with(m))
    })? as i64
        + 1;
    parts.next();
    let mut year: i64 = parts.next()?.parse().ok()?;
    if year < 100 {
        year += if year < 70 { 2000 } else { 1900 };
    }
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = parts.next()?.parse().ok()?;
    // Days from 1970-01-01 to the date (proleptic Gregorian).
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) as f64)
}

/// The `Cookie` header a request to `host` at `path` sends.
pub fn cookie_header(cookies: &[Cookie], host: &str, path: &str, now: f64) -> String {
    cookies
        .iter()
        .filter(|cookie| !cookie.expired(now) && cookie.sent_to(host, path))
        .map(|cookie| format!("{}={}", cookie.name, cookie.value))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Uses the stored sign-in once: the Channel's session endpoints renew it as a browser would.
/// Keeps what the Channel sends back, and marks the sign-in invalid if the Channel turns it away.
pub fn renew(path: &Path, agent: &ureq::Agent) -> Result<Session, String> {
    let now = now();
    let Some(mut session) = load(path) else {
        return Err("no sign-in stored".into());
    };
    let mut signed_in = false;
    for endpoint in ["/auth/access-token", "/auth/profile"] {
        let header = cookie_header(&session.cookies, SITE, endpoint, now);
        let response = agent
            .get(&format!("https://{SITE}{endpoint}"))
            .header("Cookie", &header)
            .header("Accept", "application/json")
            .header(
                "User-Agent",
                "Mozilla/5.0 (Web0S; Linux/SmartTV) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/132.0.6834.207 Safari/537.36",
            )
            .call()
            .map_err(|e| e.to_string())?;
        for value in response.headers().get_all("set-cookie") {
            if let Ok(value) = value.to_str() {
                apply_set_cookie(&mut session.cookies, value, SITE, now);
            }
        }
        if endpoint == "/auth/profile" {
            signed_in = response.status().as_u16() == 200;
        }
    }
    let _guard = file();
    // A sign-in the player or Home Assistant stored meanwhile wins over this one.
    if let Some(current) = read(path)
        && current.expires() > session.expires()
    {
        return Ok(current);
    }
    session.valid = signed_in && session.expires().is_some_and(|at| at > now);
    if session.valid {
        session.checked = Some(now);
    }
    write(path, &session)?;
    Ok(session)
}

/// Runs the background check for as long as Luma runs.
pub fn spawn_keepalive(path: PathBuf) {
    std::thread::Builder::new()
        .name("criterion-session".into())
        .spawn(move || {
            let agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(20)))
                .max_redirects(0)
                .proxy(None)
                .http_status_as_error(false)
                .build()
                .new_agent();
            loop {
                let due = load(&path).is_some_and(|session| {
                    session.usable(now())
                        && session
                            .checked
                            .is_none_or(|at| now() - at >= RENEW_EVERY.as_secs_f64())
                });
                if due {
                    match renew(&path, &agent) {
                        Ok(session) if session.valid => {
                            let days = session.expires().map_or(0., |at| (at - now()) / 86_400.);
                            crate::telemetry::log_info!(
                                "criterion",
                                "sign-in renewed in the background, good for {days:.1} days"
                            );
                        }
                        Ok(_) => crate::telemetry::log_warn!(
                            "criterion",
                            "the Channel no longer accepts the stored sign-in"
                        ),
                        Err(error) => crate::telemetry::log_warn!(
                            "criterion",
                            "background sign-in check failed: {error}"
                        ),
                    }
                }
                std::thread::sleep(WAKE_EVERY);
            }
        })
        .ok();
}

#[cfg(test)]
#[path = "criterion-session-tests.rs"]
mod tests;
