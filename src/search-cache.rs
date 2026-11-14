//! Disk cache for streaming search replies, so repeated searches cost no API calls and work
//! instantly after a restart. One file per reply under `streaming-cache/` next to the
//! preferences, named by a SHA-256 of its key. Each read states how old a reply may be, so one
//! cache serves short-lived data (where a show streams) and long-lived data (a film's studio).
//! The directory is capped; the oldest replies go first. Any unreadable entry is a miss.
use ring::digest;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// How long each kind of reply stays fresh.
pub mod ttl {
    use std::time::Duration;
    const HOUR: Duration = Duration::from_secs(3600);
    /// Movie of the Night: where shows stream changes daily, and every call costs quota.
    pub const STREAMING: Duration = HOUR.saturating_mul(12);
    /// TMDB search results: popularity moves, slowly.
    pub const TMDB_SEARCH: Duration = HOUR.saturating_mul(24);
    /// TMDB details and collections: studios and budgets never change; votes drift slowly.
    pub const TMDB_DETAILS: Duration = HOUR.saturating_mul(24 * 30);
}

/// Plenty for hundreds of searches; a title search reply is a few hundred KB.
pub const DEFAULT_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct DiskCache {
    dir: PathBuf,
    limit: u64,
}

#[derive(serde::Deserialize)]
struct Entry<T> {
    key: String,
    stored: u64,
    value: T,
}

#[derive(Serialize)]
struct EntryRef<'a, T> {
    key: &'a str,
    stored: u64,
    value: &'a T,
}

fn unix(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl DiskCache {
    pub fn new(dir: PathBuf, limit: u64) -> Self {
        Self { dir, limit }
    }

    /// The cache directory beside the preferences.
    pub fn beside(preferences: &Path) -> Self {
        Self::new(preferences.with_file_name("streaming-cache"), DEFAULT_LIMIT)
    }

    fn file(&self, key: &str) -> PathBuf {
        let hash = digest::digest(&digest::SHA256, key.as_bytes());
        let name: String = hash
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.dir.join(format!("{name}.json"))
    }

    /// The value stored under `key`, if it is younger than `max_age`.
    pub fn get<T: DeserializeOwned>(
        &self,
        key: &str,
        max_age: Duration,
        now: SystemTime,
    ) -> Option<T> {
        let bytes = fs::read(self.file(key)).ok()?;
        let entry: Entry<T> = serde_json::from_slice(&bytes).ok()?;
        let age = unix(now).saturating_sub(entry.stored);
        (entry.key == key && age <= max_age.as_secs()).then_some(entry.value)
    }

    /// Stores a value; failures only cost a future API call, so they are logged and ignored.
    pub fn put<T: Serialize>(&self, key: &str, value: &T, now: SystemTime) {
        let result = (|| {
            fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
            let bytes = serde_json::to_vec(&EntryRef {
                key,
                stored: unix(now),
                value,
            })
            .map_err(|e| e.to_string())?;
            crate::storage::write_atomic(&self.file(key), &bytes)
        })();
        match result {
            Ok(()) => self.evict(),
            Err(error) => crate::telemetry::log_warn!("streaming", "cache: {error}"),
        }
    }

    /// Removes the oldest entries until the directory fits its limit.
    fn evict(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| {
                let metadata = entry.metadata().ok()?;
                Some((metadata.modified().ok()?, metadata.len(), entry.path()))
            })
            .collect();
        let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
        if total <= self.limit {
            return;
        }
        files.sort();
        for (_, size, path) in files {
            if total <= self.limit {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= size;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(name: &str, limit: u64) -> DiskCache {
        let dir = std::env::temp_dir().join(format!("luma-cache-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        DiskCache::new(dir, limit)
    }

    #[test]
    fn values_come_back_until_they_are_too_old() {
        let cache = cache("ttl", DEFAULT_LIMIT);
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        cache.put("tmdb:movie:620", &vec!["Ghostbusters".to_owned()], now);
        let hour = Duration::from_secs(3600);
        assert_eq!(
            cache.get::<Vec<String>>("tmdb:movie:620", hour, now + hour),
            Some(vec!["Ghostbusters".to_owned()])
        );
        assert_eq!(
            cache.get::<Vec<String>>("tmdb:movie:620", hour, now + hour * 2),
            None
        );
        assert_eq!(cache.get::<Vec<String>>("tmdb:movie:2978", hour, now), None);
        // A value of another shape is a miss, not an error.
        assert_eq!(cache.get::<u32>("tmdb:movie:620", hour, now), None);
        fs::remove_dir_all(&cache.dir).unwrap();
    }

    #[test]
    fn the_oldest_entries_are_evicted_to_fit_the_limit() {
        let cache = cache("evict", 1500);
        let now = SystemTime::now();
        let big = "x".repeat(600);
        for key in ["a", "b", "c"] {
            cache.put(key, &big, now);
            // Modification times order eviction; make them distinct.
            std::thread::sleep(Duration::from_millis(20));
        }
        let day = Duration::from_secs(86_400);
        assert_eq!(cache.get::<String>("a", day, now), None);
        assert_eq!(cache.get::<String>("c", day, now), Some(big));
        fs::remove_dir_all(&cache.dir).unwrap();
    }

    #[test]
    fn corrupt_entries_are_misses() {
        let cache = cache("corrupt", DEFAULT_LIMIT);
        fs::create_dir_all(&cache.dir).unwrap();
        fs::write(cache.file("k"), b"{not json").unwrap();
        assert_eq!(
            cache.get::<String>("k", Duration::from_secs(60), SystemTime::now()),
            None
        );
        fs::remove_dir_all(&cache.dir).unwrap();
    }
}
