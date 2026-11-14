//! Review scores for the Movies & shows results from MDBList: the Rotten Tomatoes critics
//! score (Tomatometer) and audience score (Popcornmeter), with the IMDb rating as a fallback.
//! One batch request covers a whole search, by IMDb ID, which Movie of the Night returns for
//! every show; each title's scores are then cached on disk for a week. Like the other APIs:
//! a fixed origin, a timeout, no proxy or redirects, a bounded reply, and a worker thread.
//! MDBList takes its key as a query parameter, so error text is scrubbed of it.
use crate::openapi::Error;
use crate::search_cache::DiskCache;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::Read,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, SystemTime},
};

const ORIGIN: &str = "https://api.mdblist.com";
/// A 20-title batch is about 90 KB.
const LIMIT: u64 = 4 * 1024 * 1024;
/// Titles per batch request; a search returns at most 20.
const BATCH: usize = 50;
/// Scores move slowly; a week keeps a repeated search free.
pub const TTL: Duration = Duration::from_secs(7 * 24 * 3600);

/// What a card shows. Every field is optional: obscure titles have no Rotten Tomatoes scores.
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct Scores {
    /// Tomatometer, 0–100.
    pub critics: Option<u8>,
    /// Popcornmeter, 0–100.
    pub audience: Option<u8>,
    /// IMDb rating, 0–10.
    pub imdb: Option<f32>,
}

#[derive(Deserialize)]
struct Item {
    #[serde(default)]
    ids: Ids,
    #[serde(default)]
    ratings: Vec<Rating>,
}

#[derive(Default, Deserialize)]
struct Ids {
    imdb: Option<String>,
}

#[derive(Deserialize)]
struct Rating {
    source: String,
    /// An integer for percentages, a float for IMDb, null when the source has no rating.
    value: Option<f64>,
}

fn percent(value: Option<f64>) -> Option<u8> {
    value
        .filter(|v| v.is_finite() && (0. ..=100.).contains(v))
        .map(|v| v.round() as u8)
}

/// Scores by IMDb ID from a batch reply. Titles MDBList does not know are simply absent.
pub fn parse(reply: &serde_json::Value) -> Result<HashMap<String, Scores>, String> {
    let items: Vec<serde_json::Value> =
        serde_json::from_value(reply.clone()).map_err(|_| "Invalid MDBList reply".to_owned())?;
    let mut scores = HashMap::new();
    // One malformed title does not lose the others.
    for item in items {
        let Ok(item) = serde_json::from_value::<Item>(item) else {
            continue;
        };
        let Some(imdb) = item.ids.imdb else { continue };
        let mut entry = Scores::default();
        for rating in item.ratings {
            match rating.source.as_str() {
                "tomatoes" => entry.critics = percent(rating.value),
                "popcorn" => entry.audience = percent(rating.value),
                "imdb" => {
                    entry.imdb = rating
                        .value
                        .filter(|v| v.is_finite() && (0. ..=10.).contains(v))
                        .map(|v| v as f32)
                }
                _ => {}
            }
        }
        scores.insert(imdb, entry);
    }
    Ok(scores)
}

/// IMDb IDs are `tt` and digits; anything else from the network is not sent.
pub fn valid_imdb_id(id: &str) -> bool {
    id.len() > 2
        && id.len() <= 16
        && id.starts_with("tt")
        && id[2..].bytes().all(|b| b.is_ascii_digit())
}

pub struct Client {
    agent: ureq::Agent,
    key: String,
}

impl Client {
    pub fn new(key: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .max_redirects(0)
            .proxy(None)
            .http_status_as_error(false)
            .build()
            .new_agent();
        Self {
            agent,
            key: key.to_owned(),
        }
    }

    /// Never let the key reach a log or the key page through an error message.
    fn scrub(&self, text: String) -> String {
        if self.key.is_empty() {
            text
        } else {
            text.replace(&self.key, "…")
        }
    }

    fn read(&self, response: ureq::http::Response<ureq::Body>) -> Result<Vec<u8>, Error> {
        let mut response = response;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| Error::Network(self.scrub(e.to_string())))?;
        if bytes.len() as u64 > LIMIT {
            return Err(Error::TooLarge);
        }
        if !(200..300).contains(&status) {
            let message = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|v| {
                    v.get("error")?
                        .as_str()
                        .map(|s| s.chars().take(200).collect())
                })
                .unwrap_or_default();
            return Err(Error::Status {
                status,
                message: self.scrub(message),
            });
        }
        Ok(bytes)
    }

    /// Checks the key without spending a lookup.
    pub fn check(&self) -> Result<(), Error> {
        let response = self
            .agent
            .get(format!("{ORIGIN}/user"))
            .query("apikey", &self.key)
            .header("User-Agent", "LumaNative")
            .call()
            .map_err(|e| Error::Network(self.scrub(e.to_string())))?;
        self.read(response).map(drop)
    }

    /// Scores for movies and shows by IMDb ID, in one request.
    pub fn scores(&self, imdb_ids: &[String]) -> Result<HashMap<String, Scores>, Error> {
        let response = self
            .agent
            .post(format!("{ORIGIN}/imdb/any"))
            .query("apikey", &self.key)
            .header("User-Agent", "LumaNative")
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .send(serde_json::json!({ "ids": imdb_ids }).to_string())
            .map_err(|e| Error::Network(self.scrub(e.to_string())))?;
        let bytes = self.read(response)?;
        let reply: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| Error::Decode(e.to_string()))?;
        parse(&reply).map_err(Error::Decode)
    }
}

/// Where scores come from: MDBList behind the disk cache, or fixtures in tests.
pub trait Source: Send + 'static {
    fn scores(&self, imdb_ids: &[String]) -> Option<Result<HashMap<String, Scores>, Error>>;
}

/// The live API with the key read from disk for each batch, so a new key applies at once. The
/// client, and so its open connection, is kept until the key changes.
pub struct Live {
    keys: PathBuf,
    client: std::sync::Mutex<Option<(String, Client)>>,
}

impl Live {
    pub fn new(keys: PathBuf) -> Self {
        Self {
            keys,
            client: std::sync::Mutex::new(None),
        }
    }
}

impl Source for Live {
    fn scores(&self, imdb_ids: &[String]) -> Option<Result<HashMap<String, Scores>, Error>> {
        let key = crate::streaming_keys::load(&self.keys).mdblist;
        if key.is_empty() {
            return None;
        }
        let mut client = self.client.lock().ok()?;
        if client.as_ref().is_none_or(|(used, _)| *used != key) {
            *client = Some((key.clone(), Client::new(&key)));
        }
        let (_, client) = client.as_ref()?;
        Some(client.scores(imdb_ids))
    }
}

/// Cached scores, then one request for the rest. Titles the reply leaves out are cached as
/// having no scores, so they are not asked for again all week. Errors are not cached.
pub fn lookup(
    source: &impl Source,
    cache: &DiskCache,
    imdb_ids: &[String],
    now: SystemTime,
) -> Vec<(String, Scores)> {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for id in imdb_ids.iter().filter(|id| valid_imdb_id(id)) {
        match cache.get::<Scores>(&format!("mdblist:imdb:{id}"), TTL, now) {
            Some(scores) => found.push((id.clone(), scores)),
            None => missing.push(id.clone()),
        }
    }
    for batch in missing.chunks(BATCH) {
        match source.scores(batch) {
            None => break,
            Some(Err(error)) => {
                eprintln!("Luma scores: {error}");
                break;
            }
            Some(Ok(mut scores)) => {
                for id in batch {
                    let entry = scores.remove(id).unwrap_or_default();
                    cache.put(&format!("mdblist:imdb:{id}"), &entry, now);
                    found.push((id.clone(), entry));
                }
            }
        }
    }
    found
}

pub type Reply = Vec<(String, Scores)>;

/// The scores worker: one batch per search, off the SDL thread.
pub struct Service {
    sender: mpsc::Sender<Vec<String>>,
    pub receiver: mpsc::Receiver<Reply>,
}

impl Service {
    pub fn new(source: impl Source, cache: DiskCache) -> Self {
        let (sender, jobs) = mpsc::channel::<Vec<String>>();
        let (replies, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(ids) = jobs.recv() {
                let found = lookup(&source, &cache, &ids, SystemTime::now());
                if replies.send(found).is_err() {
                    return;
                }
            }
        });
        Self { sender, receiver }
    }

    pub fn send(&self, imdb_ids: Vec<String>) {
        let _ = self.sender.send(imdb_ids);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn fixture(name: &str) -> serde_json::Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("tests/fixtures/mdblist/batch-{name}.json"));
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        record["response"].clone()
    }

    #[test]
    fn captured_batches_parse_movies_and_series() {
        let dune = parse(&fixture("dune")).unwrap();
        assert_eq!(
            dune["tt1160419"],
            Scores {
                critics: Some(83),
                audience: Some(90),
                imdb: Some(8.0),
            },
            "Dune (2021)"
        );
        // Obscure titles have an IMDb rating but no Rotten Tomatoes scores.
        assert!(
            dune.values()
                .any(|s| s.critics.is_none() && s.imdb.is_some())
        );
        let office = parse(&fixture("the-office")).unwrap();
        let us = office["tt0386676"];
        assert!(us.critics.is_some() && us.audience.is_some(), "{us:?}");
    }

    #[test]
    fn malformed_titles_and_values_are_skipped() {
        let reply = serde_json::json!([
            {"ids": {"imdb": "tt1"}, "ratings": [
                {"source": "tomatoes", "value": 140},
                {"source": "popcorn", "value": null},
                {"source": "imdb", "value": 7.25}
            ]},
            {"ids": {"imdb": "tt2"}, "ratings": "not a list"},
            {"ratings": []}
        ]);
        let scores = parse(&reply).unwrap();
        assert_eq!(scores.len(), 1);
        assert_eq!(
            scores["tt1"],
            Scores {
                critics: None,
                audience: None,
                imdb: Some(7.25),
            }
        );
        assert!(parse(&serde_json::json!({"error": "Invalid API key"})).is_err());
    }

    #[test]
    fn only_imdb_ids_are_sent() {
        assert!(valid_imdb_id("tt1160419"));
        for bad in [
            "",
            "tt",
            "nm0000123",
            "tt12a",
            "tt1&apikey=x",
            "tt12345678901234567",
        ] {
            assert!(!valid_imdb_id(bad), "{bad}");
        }
    }

    #[test]
    fn errors_never_contain_the_key() {
        let client = Client::new("secretsecretsecret12345");
        assert_eq!(
            client.scrub("GET https://api.mdblist.com/user?apikey=secretsecretsecret12345".into()),
            "GET https://api.mdblist.com/user?apikey=…"
        );
    }

    struct Fake {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        reply: Option<Result<HashMap<String, Scores>, Error>>,
    }

    impl Source for Fake {
        fn scores(&self, ids: &[String]) -> Option<Result<HashMap<String, Scores>, Error>> {
            self.calls.lock().unwrap().push(ids.to_vec());
            self.reply.clone()
        }
    }

    fn cache(name: &str) -> DiskCache {
        let dir = std::env::temp_dir().join(format!("luma-scores-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        DiskCache::new(dir, crate::search_cache::DEFAULT_LIMIT)
    }

    #[test]
    fn one_request_per_search_then_the_cache_answers() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let dune = parse(&fixture("dune")).unwrap();
        let source = Fake {
            calls: calls.clone(),
            reply: Some(Ok(dune.clone())),
        };
        let cache = cache("lookup");
        let ids: Vec<String> = ["tt1160419", "tt0087182", "tt9999999", "bogus"]
            .map(String::from)
            .to_vec();
        let now = SystemTime::now();
        let found: HashMap<_, _> = lookup(&source, &cache, &ids, now).into_iter().collect();
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(
            calls.lock().unwrap()[0],
            ids[..3],
            "invalid IDs are not sent"
        );
        assert_eq!(found["tt1160419"], dune["tt1160419"]);
        assert_eq!(
            found["tt9999999"],
            Scores::default(),
            "unknown titles have no scores"
        );

        let again: HashMap<_, _> = lookup(&source, &cache, &ids, now).into_iter().collect();
        assert_eq!(calls.lock().unwrap().len(), 1, "answered from the cache");
        assert_eq!(again, found);
        assert!(
            lookup(&source, &cache, &ids, now + TTL * 2).len() == 3
                && calls.lock().unwrap().len() == 2,
            "stale after a week"
        );
    }

    #[test]
    fn failures_and_a_missing_key_are_not_cached() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let cache = cache("failure");
        let ids = vec!["tt1160419".to_owned()];
        let now = SystemTime::now();
        for reply in [None, Some(Err(Error::Network("offline".into())))] {
            let source = Fake {
                calls: calls.clone(),
                reply,
            };
            assert!(lookup(&source, &cache, &ids, now).is_empty());
        }
        let source = Fake {
            calls: calls.clone(),
            reply: Some(Ok(HashMap::new())),
        };
        assert_eq!(lookup(&source, &cache, &ids, now).len(), 1);
        assert_eq!(calls.lock().unwrap().len(), 3, "each attempt asked again");
    }
}
