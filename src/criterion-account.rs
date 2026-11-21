//! The Criterion Channel account the Criterion player signs in with, so nobody types a password
//! with the remote. The owner enters it in the Home Assistant integration's options; Home
//! Assistant sends it to the TV over the paired API (`PUT /api/criterion-account`), and the TV
//! keeps it beside the pairings in a file only Luma can read. It is never logged, and never
//! put in launch parameters, which webOS may log.
//!
//! The player gets it like this: each time Luma opens a film, it hands the player a one-time
//! code in its launch parameters. When the Channel shows its sign-in page, the player's script
//! trades the code for the account at `POST /api/criterion/sign-in` on Luma's own API, from the
//! TV itself only, and fills in the form. A code works for a few minutes and a few sign-ins.
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

/// The Channel's sign-in pages (Auth0), the only origin that may ask for the account.
pub const SIGN_IN_ORIGIN: &str = "https://login.criterion.com";
/// How long a code handed to the player works: long enough to load a film and its sign-in page.
const CODE_LIFETIME: Duration = Duration::from_secs(10 * 60);
/// A wrong password must not turn into a sign-in loop.
const CODE_USES: u8 = 3;
/// Codes for films opened one after another; older ones are dropped.
const CODES_KEPT: usize = 4;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    pub password: String,
    /// Home Assistant's random fingerprint for this account, reported back in `/api/status` so
    /// it can tell whether the TV holds the account it has now. Not derived from the account.
    #[serde(default)]
    pub fingerprint: String,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account").finish_non_exhaustive()
    }
}

impl Account {
    /// An account as Home Assistant sends it: an email address and a password, each without
    /// control characters, within the lengths a sign-in form takes.
    pub fn new(email: &str, password: &str) -> Result<Self, String> {
        let email = email.trim();
        let printable = |text: &str| !text.chars().any(char::is_control);
        if !(3..=254).contains(&email.chars().count())
            || !email.contains('@')
            || email.contains(char::is_whitespace)
            || !printable(email)
        {
            return Err("email must be an email address".into());
        }
        if !(1..=256).contains(&password.chars().count()) || !printable(password) {
            return Err("password must be 1 to 256 characters".into());
        }
        Ok(Self {
            email: email.into(),
            password: password.into(),
            fingerprint: String::new(),
        })
    }

    /// With Home Assistant's fingerprint: up to 128 letters and digits, or none.
    pub fn with_fingerprint(mut self, fingerprint: &str) -> Result<Self, String> {
        if fingerprint.len() > 128 || !fingerprint.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err("fingerprint must be up to 128 letters and digits".into());
        }
        self.fingerprint = fingerprint.into();
        Ok(self)
    }
}

/// Beside the pairings, which sit beside the preferences.
pub fn path(store: &Path) -> PathBuf {
    store.with_file_name("criterion-account.json")
}

/// Where this run of Luma keeps the account, set once at startup so the platform worker can tell
/// whether there is one to sign in with before it hands the player a code.
static STORE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn set_store(store: &Path) {
    let _ = STORE.set(path(store));
}

/// Whether an account is stored: without one, the player gets no code and leaves the Channel's
/// sign-in to the viewer.
pub fn stored() -> bool {
    STORE.get().is_some_and(|file| load(file).is_some())
}

pub fn load(path: &Path) -> Option<Account> {
    let bytes = std::fs::read(path).ok()?;
    let account: Account = serde_json::from_slice(&bytes).ok()?;
    Account::new(&account.email, &account.password)
        .and_then(|valid| valid.with_fingerprint(&account.fingerprint))
        .ok()
}

pub fn save(path: &Path, account: &Account) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec(account).map_err(|e| e.to_string())?;
    crate::home_assistant::write_private(path, &bytes)
}

pub fn forget(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

// ---- One-time codes -------------------------------------------------------------------------

struct Code {
    value: String,
    issued: Instant,
    uses: u8,
}

/// Shared by the platform worker, which launches the player, and the API thread, which answers it.
static CODES: Mutex<Vec<Code>> = Mutex::new(Vec::new());

fn codes() -> std::sync::MutexGuard<'static, Vec<Code>> {
    CODES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A new code for the player's launch parameters.
pub fn issue(now: Instant) -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| e.to_string())?;
    let value: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut codes = codes();
    codes.retain(|code| now.duration_since(code.issued) < CODE_LIFETIME);
    codes.push(Code {
        value: value.clone(),
        issued: now,
        uses: 0,
    });
    let excess = codes.len().saturating_sub(CODES_KEPT);
    codes.drain(..excess);
    Ok(value)
}

/// Compared without stopping at the first difference, so timing says nothing about a code.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

/// Whether `value` is a current code, counting this use.
pub fn redeem(value: &str, now: Instant) -> bool {
    if value.len() != 32 {
        return false;
    }
    let mut codes = codes();
    codes.retain(|code| now.duration_since(code.issued) < CODE_LIFETIME && code.uses < CODE_USES);
    let Some(code) = codes
        .iter_mut()
        .find(|code| same(code.value.as_bytes(), value.as_bytes()))
    else {
        return false;
    };
    code.uses += 1;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_are_checked_and_never_printed() {
        let account = Account::new(" me@example.com ", "hunter2 with spaces").unwrap();
        assert_eq!(account.email, "me@example.com");
        assert_eq!(account.password, "hunter2 with spaces");
        let debug = format!("{account:?}");
        // Failure messages name the case, never the values.
        assert!(!debug.contains("hunter2") && !debug.contains("example"));
        for (case, (email, password)) in [
            ("not-an-email", "x"),
            ("a b@example.com", "x"),
            ("me@example.com", ""),
            ("me@example.com", "line\nbreak"),
            ("me\u{0}@example.com", "x"),
        ]
        .into_iter()
        .enumerate()
        {
            assert!(Account::new(email, password).is_err(), "case {case}");
        }
        assert!(Account::new("me@example.com", &"x".repeat(257)).is_err());
    }

    #[test]
    fn the_account_file_is_private_and_round_trips() {
        let dir =
            std::env::temp_dir().join(format!("luma-criterion-account-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = path(&dir.join("home-assistant.json"));
        assert_eq!(file, dir.join("criterion-account.json"));
        assert_eq!(load(&file), None);
        let account = Account::new("me@example.com", "secret")
            .unwrap()
            .with_fingerprint("abc123")
            .unwrap();
        assert!(account.clone().with_fingerprint("a-b").is_err());
        save(&file, &account).unwrap();
        assert_eq!(load(&file), Some(account));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }
        forget(&file).unwrap();
        assert_eq!(load(&file), None);
        forget(&file).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_player_gets_a_code_only_when_an_account_is_stored() {
        let dir =
            std::env::temp_dir().join(format!("luma-criterion-stored-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = dir.join("home-assistant.json");
        // The only test that sets the store; Luma sets it once at startup.
        set_store(&store);
        assert!(
            !stored(),
            "no account: no code, so the viewer signs in by hand"
        );
        save(&path(&store), &Account::new("me@example.com", "x").unwrap()).unwrap();
        assert!(stored());
        forget(&path(&store)).unwrap();
        assert!(!stored());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn codes_work_a_few_times_for_a_few_minutes() {
        let start = Instant::now();
        let code = issue(start).unwrap();
        assert_eq!(code.len(), 32);
        assert!(!redeem("0".repeat(32).as_str(), start));
        assert!(!redeem("short", start));
        for _ in 0..CODE_USES {
            assert!(redeem(&code, start));
        }
        assert!(!redeem(&code, start), "used up");
        let code = issue(start).unwrap();
        assert!(!redeem(&code, start + CODE_LIFETIME), "expired");
        // Only the newest few codes are kept.
        let first = issue(start).unwrap();
        for _ in 0..CODES_KEPT {
            issue(start).unwrap();
        }
        assert!(!redeem(&first, start));
    }
}
