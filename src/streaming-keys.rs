//! API keys for streaming search: Movie of the Night (where a title streams), TMDB (how
//! prominent it is) and MDBList (its review scores). The owner enters them on a phone through the QR key editor; they are never
//! typed on the TV, bundled in the package, logged or shown beyond their last four characters.
//! Stored in `streaming-keys.json` next to the preferences, readable only by Luma (mode 0600).
use crate::{motn_api, openapi::Http, tmdb_api};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
};

/// Show details with every episode run to a few MB; nothing legitimate comes close to this.
const MOTN_LIMIT: usize = 16 * 1024 * 1024;
const TMDB_LIMIT: usize = 2 * 1024 * 1024;
const MOTN_RAPIDAPI_HOST: &str = "streaming-availability.p.rapidapi.com";

#[derive(Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Keys {
    /// Movie of the Night Developers Platform key (`motn-key-…`) or a RapidAPI key.
    #[serde(default)]
    pub motn: String,
    /// TMDB API Read Access Token, sent as a bearer token.
    #[serde(default)]
    pub tmdb: String,
    /// MDBList API key, for Rotten Tomatoes and IMDb scores.
    #[serde(default)]
    pub mdblist: String,
    /// Home Assistant set these last. It is the source of truth and pushes them again after
    /// a reset; keys entered on the QR page clear this until its next push.
    #[serde(default)]
    pub home_assistant: bool,
}

/// Keys must never reach logs, even through `{:?}`.
impl fmt::Debug for Keys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keys")
            .field("motn", &mask(&self.motn))
            .field("tmdb", &mask(&self.tmdb))
            .field("mdblist", &mask(&self.mdblist))
            .field("home_assistant", &self.home_assistant)
            .finish()
    }
}

/// What the TV and the key page may show about the stored keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub motn: bool,
    pub tmdb: bool,
    pub mdblist: bool,
    pub home_assistant: bool,
}

impl Status {
    #[allow(dead_code)]
    pub fn complete(self) -> bool {
        self.motn && self.tmdb
    }
}

impl Keys {
    pub fn status(&self) -> Status {
        Status {
            motn: !self.motn.is_empty(),
            tmdb: !self.tmdb.is_empty(),
            mdblist: !self.mdblist.is_empty(),
            home_assistant: self.home_assistant,
        }
    }

    /// A Movie of the Night client for whichever kind of key this is: Developers Platform keys
    /// go to api.movieofthenight.com, RapidAPI keys to RapidAPI (their docs' rule).
    pub fn motn(&self) -> Option<motn_api::Client> {
        self.motn_http().map(motn_api::Client::new)
    }

    /// The connection [`Keys::motn`] calls through; clones share its open connections.
    pub fn motn_http(&self) -> Option<Http> {
        if self.motn.is_empty() {
            return None;
        }
        let http = if self.motn.starts_with("motn-key-") {
            Http::new(
                motn_api::SERVERS[0],
                vec![("X-API-Key", self.motn.clone())],
                MOTN_LIMIT,
            )
        } else {
            Http::new(
                motn_api::SERVERS[1],
                vec![
                    ("X-RapidAPI-Key", self.motn.clone()),
                    ("X-RapidAPI-Host", MOTN_RAPIDAPI_HOST.to_owned()),
                ],
                MOTN_LIMIT,
            )
        };
        Some(http)
    }

    pub fn mdblist(&self) -> Option<crate::mdblist::Client> {
        (!self.mdblist.is_empty()).then(|| crate::mdblist::Client::new(&self.mdblist))
    }

    pub fn tmdb(&self) -> Option<tmdb_api::Client> {
        self.tmdb_http().map(tmdb_api::Client::new)
    }

    /// The connection [`Keys::tmdb`] calls through; clones share its open connections.
    pub fn tmdb_http(&self) -> Option<Http> {
        (!self.tmdb.is_empty()).then(|| {
            Http::new(
                tmdb_api::SERVERS[0],
                vec![("Authorization", format!("Bearer {}", self.tmdb))],
                TMDB_LIMIT,
            )
        })
    }
}

/// `…ab12` for a stored key, empty when none is stored.
pub fn mask(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    let tail: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

/// Trims a pasted key and rejects anything that cannot be one: keys are printable ASCII with no
/// spaces, which also keeps them safe to put in an HTTP header.
pub fn clean(key: &str) -> Result<String, &'static str> {
    let key = key.trim();
    let key = key.strip_prefix("Bearer ").unwrap_or(key).trim();
    if key.is_empty() {
        return Ok(String::new());
    }
    if key.len() < 16 || key.len() > 1024 || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("That doesn’t look like an API key. Paste the whole key.");
    }
    Ok(key.to_owned())
}

/// Applies a Home Assistant push: each non-empty key replaces the stored one, and an empty or
/// missing key keeps it, so clearing a field in Home Assistant never erases the TV's key.
pub fn merge_from_home_assistant(
    keys: &Keys,
    motn: Option<&str>,
    tmdb: Option<&str>,
    mdblist: Option<&str>,
) -> Result<Keys, String> {
    let mut next = keys.clone();
    let mut changed = false;
    for (name, value, slot) in [
        ("motn", motn, &mut next.motn),
        ("tmdb", tmdb, &mut next.tmdb),
        ("mdblist", mdblist, &mut next.mdblist),
    ] {
        let value = clean(value.unwrap_or_default()).map_err(|e| format!("{name}: {e}"))?;
        if !value.is_empty() {
            *slot = value;
            changed = true;
        }
    }
    if changed {
        next.home_assistant = true;
    }
    Ok(next)
}

pub fn path(preferences: &Path) -> PathBuf {
    preferences.with_file_name("streaming-keys.json")
}

/// Missing or unreadable files mean no keys; the owner can enter them again.
pub fn load(path: &Path) -> Keys {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            crate::telemetry::log_warn!("streaming", "keys could not be read: {e}");
            Keys::default()
        }),
        Err(_) => Keys::default(),
    }
}

pub fn save(path: &Path, keys: &Keys) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(keys).map_err(|e| e.to_string())?;
    crate::home_assistant::write_private(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_a_key() {
        let keys = Keys {
            motn: "motn-key-secretsecretsecret1234".into(),
            tmdb: "eyJhbGciOiJIUzI1NiJ9.secret.abcd".into(),
            mdblist: "secretsecretsecretwxyz".into(),
            home_assistant: true,
        };
        let debug = format!("{keys:?}");
        assert!(!debug.contains("secret"), "{debug}");
        assert!(
            debug.contains("…1234") && debug.contains("…abcd") && debug.contains("…wxyz"),
            "{debug}"
        );
    }

    #[test]
    fn pasted_keys_are_trimmed_and_checked() {
        assert_eq!(
            clean("  motn-key-0123456789abcdef \n").unwrap(),
            "motn-key-0123456789abcdef"
        );
        assert_eq!(
            clean("Bearer eyJhbGciOiJIUzI1NiJ9.e30.sig").unwrap(),
            "eyJhbGciOiJIUzI1NiJ9.e30.sig"
        );
        assert_eq!(clean("   ").unwrap(), "");
        assert!(clean("short").is_err());
        assert!(clean("motn-key-0123456789 abcdef").is_err());
        assert!(clean("motn-key-0123456789abcdef\r\nX-Evil: 1").is_err());
    }

    #[test]
    fn saved_keys_are_private_and_round_trip() {
        let dir = std::env::temp_dir().join(format!("luma-keys-{}", std::process::id()));
        let file = path(&dir.join("preferences.json"));
        let keys = Keys {
            motn: "motn-key-0123456789abcdef".into(),
            ..Keys::default()
        };
        save(&file, &keys).unwrap();
        assert_eq!(load(&file), keys);
        assert_eq!(
            load(&file).status(),
            Status {
                motn: true,
                tmdb: false,
                mdblist: false,
                home_assistant: false,
            }
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn home_assistant_pushes_never_erase_a_key() {
        let stored = Keys {
            motn: "motn-key-0123456789abcdef".into(),
            tmdb: "eyJhbGciOiJIUzI1NiJ9.e30.oldtoken".into(),
            mdblist: "abcdefghijklmnopqrstuvwxy".into(),
            home_assistant: false,
        };
        let next = merge_from_home_assistant(&stored, None, Some("  "), None).unwrap();
        assert_eq!(next, stored, "empty push changed nothing");
        let next = merge_from_home_assistant(
            &stored,
            Some(""),
            Some("eyJhbGciOiJIUzI1NiJ9.e30.newtoken"),
            Some("zyxwvutsrqponmlkjihgfedcb"),
        )
        .unwrap();
        assert_eq!(next.motn, stored.motn);
        assert_eq!(next.tmdb, "eyJhbGciOiJIUzI1NiJ9.e30.newtoken");
        assert_eq!(next.mdblist, "zyxwvutsrqponmlkjihgfedcb");
        assert!(next.home_assistant);
        assert!(merge_from_home_assistant(&stored, Some("bad key"), None, None).is_err());
    }

    #[test]
    fn missing_or_corrupt_files_mean_no_keys() {
        let dir = std::env::temp_dir().join(format!("luma-keys-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("streaming-keys.json");
        assert_eq!(load(&file), Keys::default());
        std::fs::write(&file, b"{not json").unwrap();
        assert_eq!(load(&file), Keys::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
