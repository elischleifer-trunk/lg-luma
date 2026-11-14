//! Hand-written runtime for the API clients that `pnpm openapi` generates from OpenAPI specs
//! (`motn-api.rs`, `tmdb-api.rs`). Blocking ureq/rustls with the same rules as weather: fixed
//! origins, timeouts, no proxy or redirects and bounded replies. Call it from a worker thread,
//! never the SDL thread.
//!
//! Keep an `Http` alive between calls: it owns the connection pool. A new TLS connection to
//! Movie of the Night takes several times longer than a request on an open one, and replies
//! are asked for gzipped (TMDB's are a quarter of the size; Movie of the Night sends plain).
use serde::de::DeserializeOwned;
use std::{fmt, io::Read, time::Duration};

/// Why a request failed. `Display` text is short enough for the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// DNS, TLS, timeout or connection failure.
    Network(String),
    /// The server answered with a non-2xx status; `message` comes from its JSON error body.
    Status { status: u16, message: String },
    /// The reply exceeded the client's size limit.
    TooLarge,
    /// The reply was not the JSON the spec describes.
    Decode(String),
    /// The API cannot be called: its key is not set.
    NotConfigured(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Network(e) => write!(f, "Network error: {e}"),
            Error::Status { status, message } if message.is_empty() => write!(f, "HTTP {status}"),
            Error::Status { status, message } => write!(f, "HTTP {status}: {message}"),
            Error::TooLarge => f.write_str("The reply is too large"),
            Error::Decode(e) => write!(f, "Unreadable reply: {e}"),
            Error::NotConfigured(api) => write!(f, "{api} key is not set"),
        }
    }
}

/// Connections one search opens to an API at once; also how many stay open afterwards.
pub const CONNECTIONS: usize = 6;
/// How long an unused connection is kept for the next search. Typing a title on a remote takes
/// a few seconds a letter; a closed connection is replaced, never an error.
const IDLE: Duration = Duration::from_secs(60);

/// One API origin plus its credentials.
#[derive(Clone)]
pub struct Http {
    agent: ureq::Agent,
    base: String,
    headers: Vec<(&'static str, String)>,
    limit: usize,
}

impl Http {
    /// `base` is a server URL from the spec (no trailing slash). `headers` carry the API key.
    pub fn new(base: &str, headers: Vec<(&'static str, String)>, limit: usize) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .max_redirects(0)
            .proxy(None)
            .http_status_as_error(false)
            .max_idle_connections_per_host(CONNECTIONS)
            .max_idle_age(IDLE)
            .build()
            .new_agent();
        Self {
            agent,
            base: base.trim_end_matches('/').to_owned(),
            headers,
            limit,
        }
    }

    /// Opens a connection for the requests that follow, so the first search does not wait for
    /// DNS and TLS. Sent without credentials: the origin refuses it, which costs no quota, and
    /// the connection stays open. Failure only means the first request connects itself.
    pub fn warm(&self) {
        let Ok(mut response) = self
            .agent
            .get(format!("{}/", self.base))
            .header("User-Agent", "LumaNative")
            .call()
        else {
            return;
        };
        // A connection is reused only once its reply has been read to the end.
        let mut sink = Vec::new();
        let _ = response
            .body_mut()
            .as_reader()
            .take(64 * 1024)
            .read_to_end(&mut sink);
    }

    /// GETs `path` (already percent-encoded) and decodes the JSON reply.
    pub fn get<T: DeserializeOwned>(&self, path: &str, query: &Query) -> Result<T, Error> {
        let mut request = self
            .agent
            .get(format!("{}{path}", self.base))
            .header("Accept", "application/json")
            .header("User-Agent", "LumaNative");
        for (name, value) in &self.headers {
            request = request.header(*name, value);
        }
        for (name, value) in &query.0 {
            request = request.query(*name, value);
        }
        let mut response = request.call().map_err(|e| Error::Network(e.to_string()))?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(self.limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| Error::Network(e.to_string()))?;
        if bytes.len() > self.limit {
            return Err(Error::TooLarge);
        }
        if !(200..300).contains(&status) {
            return Err(Error::Status {
                status,
                message: error_message(&bytes),
            });
        }
        serde_json::from_slice(&bytes).map_err(|e| Error::Decode(e.to_string()))
    }
}

/// Movie of the Night sends `{"message"}`, TMDB `{"status_message"}`.
fn error_message(body: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return String::new();
    };
    ["message", "status_message"]
        .iter()
        .find_map(|key| value.get(key)?.as_str())
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect()
}

/// Query parameters in the order the spec lists them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Query(pub Vec<(&'static str, String)>);

impl Query {
    pub fn add<V: QueryValue + ?Sized>(&mut self, name: &'static str, value: &V) {
        self.0.push((name, value.to_query()));
    }
}

/// How a parameter is written into the query string.
pub trait QueryValue {
    fn to_query(&self) -> String;
}

impl QueryValue for str {
    fn to_query(&self) -> String {
        self.to_owned()
    }
}

impl QueryValue for String {
    fn to_query(&self) -> String {
        self.clone()
    }
}

impl QueryValue for i64 {
    fn to_query(&self) -> String {
        self.to_string()
    }
}

impl QueryValue for bool {
    fn to_query(&self) -> String {
        self.to_string()
    }
}

/// Arrays use OpenAPI `style: form, explode: false`: one comma-separated value.
impl<T: QueryValue> QueryValue for [T] {
    fn to_query(&self) -> String {
        self.iter()
            .map(QueryValue::to_query)
            .collect::<Vec<_>>()
            .join(",")
    }
}

impl<T: QueryValue> QueryValue for Vec<T> {
    fn to_query(&self) -> String {
        self.as_slice().to_query()
    }
}

/// Percent-encodes a path parameter. `/` is kept: Movie of the Night show IDs can be TMDB IDs
/// such as `movie/597`, which the spec's own examples put in the path unescaped.
pub fn encode_path(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_parameters_keep_slashes_and_escape_the_rest() {
        assert_eq!(encode_path("movie/597"), "movie/597");
        assert_eq!(encode_path("tt0068646"), "tt0068646");
        assert_eq!(encode_path("a b?&é"), "a%20b%3F%26%C3%A9");
    }

    #[test]
    fn arrays_are_comma_separated() {
        let mut query = Query::default();
        query.add(
            "catalogs",
            &vec!["netflix".to_owned(), "prime.subscription".into()],
        );
        query.add("rating_min", &70_i64);
        assert_eq!(
            query.0,
            vec![
                ("catalogs", "netflix,prime.subscription".to_owned()),
                ("rating_min", "70".to_owned()),
            ]
        );
    }

    /// Every captured Movie of the Night reply decodes into the type generated for its endpoint,
    /// so the spec's required fields match what the API really sends.
    #[test]
    fn captured_motn_replies_decode_into_generated_types() {
        use crate::motn_api::{Country, Genre, SearchResult, Show};
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/streaming");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let file = entry.unwrap().path();
            if file.file_name().unwrap() == "index.json" {
                continue;
            }
            let record: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            let path = record["request"]["path"].as_str().unwrap();
            let reply = record["response"].clone();
            let result = match path {
                "/genres" => serde_json::from_value::<Vec<Genre>>(reply).map(drop),
                "/shows/search/title" => serde_json::from_value::<Vec<Show>>(reply).map(drop),
                "/shows/search/filters" => serde_json::from_value::<SearchResult>(reply).map(drop),
                p if p.starts_with("/countries/") => {
                    serde_json::from_value::<Country>(reply).map(drop)
                }
                p if p.starts_with("/shows/") => serde_json::from_value::<Show>(reply).map(drop),
                other => panic!("{}: no type for {other}", file.display()),
            };
            result.unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            checked += 1;
        }
        assert_eq!(checked, 22);
    }

    /// Every captured TMDB reply decodes into the generated type for its endpoint. This is how
    /// the spec's wrong season rating type was found (see `patches` in tools/openapi.mjs).
    #[test]
    fn captured_tmdb_replies_decode_into_generated_types() {
        use crate::tmdb_api::{
            CollectionDetailsResponse, MovieDetailsResponse, SearchMultiResponse,
            TvSeriesDetailsResponse,
        };
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tmdb");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let file = entry.unwrap().path();
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            if name == "index.json" {
                continue;
            }
            let record: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            let reply = record["response"].clone();
            let result = match name.split('-').next().unwrap() {
                "search" => serde_json::from_value::<SearchMultiResponse>(reply).map(drop),
                "movie" => serde_json::from_value::<MovieDetailsResponse>(reply).map(drop),
                "tv" => serde_json::from_value::<TvSeriesDetailsResponse>(reply).map(drop),
                "collection" => {
                    serde_json::from_value::<CollectionDetailsResponse>(reply).map(drop)
                }
                other => panic!("{name}: no type for {other}"),
            };
            result.unwrap_or_else(|e| panic!("{name}: {e}"));
            checked += 1;
        }
        assert!(checked > 200, "{checked}");
    }

    #[test]
    fn error_messages_come_from_either_api() {
        assert_eq!(error_message(br#"{"message":"bad"}"#), "bad");
        assert_eq!(
            error_message(br#"{"status_code":7,"status_message":"Invalid API key"}"#),
            "Invalid API key"
        );
        assert_eq!(error_message(b"<html>"), "");
    }
}
