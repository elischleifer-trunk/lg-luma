//! The Criterion Channel screen: rows of films on the Channel, browsed with the remote like any
//! other Luma screen. Choosing a film opens it in Luma's Criterion player (`criterion/`), a small
//! web app that loads the Channel's web player full screen and signs in with the account Home
//! Assistant holds (see `criterion_account`).
//!
//! The Channel has no public API, so the rows come from Luma's search partners when their keys
//! are stored (Settings → Search, or Home Assistant):
//! - TMDB, whose quota is generous, lists films on the Channel by its watch-provider data:
//!   Popular, Top Rated, Recently Released and a row per genre, refreshed twice a day.
//! - Movie of the Night, whose quota is small, is asked only when a film is chosen, for the
//!   film's page on the Channel. Links are kept for a month. Without one, the player opens the
//!   Channel's search for the title.
//!
//! Without a TMDB key, a worker reads the rows from the Channel's public home page instead. Its
//! class names are generated and change between deployments; parsing relies only on stable
//! parts: each row's `<h2>` heading, the `/films/<id>/<slug>` card links and the card titles'
//! element IDs. That page's artwork comes from the Channel's video host by film ID.
use crate::cameras::Frame;
use crate::openapi::{Http, Query};
use crate::search_cache::DiskCache;
use crate::state::{Direction, Effect, Overlay, State, Target};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, SystemTime},
};

/// Luma's built-in tile for this screen.
pub const APP: &str = "luma.criterion";
/// The player web app that films open in.
pub const PLAYER: &str = "com.ecklf.luma.criterion";
pub const ORIGIN: &str = "https://www.criterionchannel.com";
/// What `deeplink` calls the Channel.
pub const SERVICE: &str = "criterion";
/// Card artwork, decoded at card size: 16:9.
pub const IMAGE_SIZE: (u32, u32) = (400, 225);
/// The home page is about 1 MB.
const PAGE_LIMIT: u64 = 6 * 1024 * 1024;
/// Rows change a few times a week; the copy on disk is used as is for this long.
const FRESH: Duration = Duration::from_secs(12 * 3600);
/// After that, it still fills the screen while a new copy loads, or when the Channel is away.
const STALE: Duration = Duration::from_secs(30 * 24 * 3600);
const CACHE_KEY: &str = "criterion:rows";
/// A film's page on the Channel, found with Movie of the Night, rarely changes.
const LINK_AGE: Duration = Duration::from_secs(30 * 24 * 3600);
/// Movie of the Night lists Criterion's US catalog.
const COUNTRY: &str = "us";
const TMDB_IMAGES: &str = "https://image.tmdb.org/t/p/w780";
const ROW_LIMIT: usize = 40;
const FILM_LIMIT: usize = 100;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Film {
    /// Unique among the rows: the Channel's 8-character film ID, or `tmdb` and TMDB's ID.
    pub id: String,
    pub title: String,
    /// Year and running time, as far as they are known.
    pub year: String,
    pub length: String,
    /// The film's page on the Channel, when known; TMDB films find it when chosen.
    pub link: Option<String>,
    /// TMDB's ID, for finding the film's page with Movie of the Night.
    pub tmdb: Option<u64>,
    /// 16:9 artwork.
    pub image: String,
}

impl Film {
    /// "1944 · 1 hr 28 min", or whichever of them the card has.
    pub fn details(&self) -> String {
        [self.year.as_str(), self.length.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub title: String,
    pub films: Vec<Film>,
}

// ---- Parsing --------------------------------------------------------------------------------

/// The film rows on the Channel's home page, in page order. Sections without a heading (the
/// hero carousel) or with fewer than two films (collection and director rows) are left out.
pub fn parse(html: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    for section in html.split("<section").skip(1) {
        let Some(title) = heading(section) else {
            continue;
        };
        let mut films: Vec<Film> = Vec::new();
        let mut rest = section;
        while let Some(start) = rest.find(r#"href="/films/"#) {
            rest = &rest[start + r#"href="/films/"#.len()..];
            let Some(end) = rest.find('"') else { break };
            let (id, slug) = rest[..end].split_once('/').unwrap_or(("", ""));
            if !valid_id(id) || !valid_slug(slug) || films.iter().any(|film| film.id == id) {
                continue;
            }
            // The card's own markup runs to the next list item.
            let card = &rest[..rest.find("<li").unwrap_or(rest.len())];
            let Some(title) = card_title(section, id) else {
                continue;
            };
            films.push(Film {
                id: id.into(),
                title,
                year: meta(card, "metaLeft"),
                length: meta(card, "metaRight"),
                link: Some(format!("{ORIGIN}/films/{id}/{slug}")),
                tmdb: None,
                image: format!(
                    "https://cdn.jwplayer.com/v2/media/{id}/images/regalia_16x9.webp?fallback=default_16x9&width=480"
                ),
            });
            if films.len() == FILM_LIMIT {
                break;
            }
        }
        if films.len() >= 2 {
            rows.push(Row { title, films });
            if rows.len() == ROW_LIMIT {
                break;
            }
        }
    }
    rows
}

fn heading(section: &str) -> Option<String> {
    let start = section.find("<h2")?;
    let open = start + section[start..].find('>')? + 1;
    let close = open + section[open..].find("</h2>")?;
    let text = clean(&section[open..close]);
    (!text.is_empty()).then_some(text)
}

/// A card's title: the text of its `playlist-card-title-<id>` element.
fn card_title(section: &str, id: &str) -> Option<String> {
    let marker = format!(r#"id="playlist-card-title-{id}">"#);
    let start = section.find(&marker)? + marker.len();
    let end = start + section[start..].find('<')?;
    let title = clean(&section[start..end]);
    (!title.is_empty()).then_some(title)
}

/// The text of the element whose class ends in `name` (`Xfr99W_metaLeft`).
fn meta(card: &str, name: &str) -> String {
    let marker = format!(r#"_{name}">"#);
    card.find(&marker)
        .and_then(|start| {
            let start = start + marker.len();
            let end = start + card[start..].find('<')?;
            Some(clean(&card[start..end]))
        })
        .unwrap_or_default()
}

fn valid_id(id: &str) -> bool {
    (6..=16).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn valid_slug(slug: &str) -> bool {
    (1..=160).contains(&slug.len())
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Text with HTML entities decoded and whitespace collapsed, at most 200 characters.
fn clean(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&end| end <= 10).and_then(|end| {
            let name = &rest[1..end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => {
                    let number = name.strip_prefix('#')?;
                    let code = match number.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => number.parse().ok()?,
                    };
                    char::from_u32(code).filter(|c| !c.is_control())
                }
            }?;
            Some((c, end + 1))
        });
        match decoded {
            Some((c, length)) => {
                out.push(c);
                rest = &rest[length..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}

// ---- TMDB rows -------------------------------------------------------------------------------

/// The rows built from TMDB, beyond Popular: (title, discover parameters).
const TMDB_ROWS: &[(&str, &[(&str, &str)])] = &[
    (
        "Popular on the Criterion Channel",
        &[("sort_by", "popularity.desc")],
    ),
    (
        "Top Rated",
        &[("sort_by", "vote_average.desc"), ("vote_count.gte", "300")],
    ),
    (
        "Recently Released",
        &[
            ("sort_by", "primary_release_date.desc"),
            ("vote_count.gte", "20"),
        ],
    ),
    (
        "Drama",
        &[("sort_by", "popularity.desc"), ("with_genres", "18")],
    ),
    (
        "Comedy",
        &[("sort_by", "popularity.desc"), ("with_genres", "35")],
    ),
    (
        "Crime",
        &[("sort_by", "popularity.desc"), ("with_genres", "80")],
    ),
    (
        "Romance",
        &[("sort_by", "popularity.desc"), ("with_genres", "10749")],
    ),
    (
        "Horror",
        &[("sort_by", "popularity.desc"), ("with_genres", "27")],
    ),
    (
        "Science Fiction",
        &[("sort_by", "popularity.desc"), ("with_genres", "878")],
    ),
    (
        "Documentary",
        &[("sort_by", "popularity.desc"), ("with_genres", "99")],
    ),
    (
        "Animation",
        &[("sort_by", "popularity.desc"), ("with_genres", "16")],
    ),
];

/// A TMDB GET: path and query in, JSON out.
pub type TmdbGet<'a> = dyn Fn(&str, &[(&'static str, String)]) -> Result<Value, String> + 'a;

/// The Channel's TMDB watch-provider ID, found by name: TMDB does not document its IDs.
fn provider(get: &TmdbGet) -> Result<u64, String> {
    let list = get(
        "/3/watch/providers/movie",
        &[("watch_region", COUNTRY.to_uppercase())],
    )?;
    list["results"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|provider| {
            provider["provider_name"]
                .as_str()
                .is_some_and(|name| name.to_lowercase().contains("criterion"))
        })
        .and_then(|provider| provider["provider_id"].as_u64())
        .ok_or_else(|| "TMDB does not list the Criterion Channel".to_owned())
}

/// Rows of films TMDB lists on the Channel. Rows that fail or come back short are left out.
pub fn tmdb_rows(get: &TmdbGet) -> Result<Vec<Row>, String> {
    let provider = provider(get)?.to_string();
    let mut rows = Vec::new();
    let mut last_error = None;
    for (title, parameters) in TMDB_ROWS {
        let mut query = vec![
            ("watch_region", COUNTRY.to_uppercase()),
            ("with_watch_providers", provider.clone()),
            ("with_watch_monetization_types", "flatrate".to_owned()),
            ("include_adult", "false".to_owned()),
        ];
        query.extend(
            parameters
                .iter()
                .map(|(name, value)| (*name, (*value).to_owned())),
        );
        match get("/3/discover/movie", &query) {
            Ok(reply) => {
                let films: Vec<Film> = reply["results"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(tmdb_film)
                    .take(FILM_LIMIT)
                    .collect();
                if films.len() >= 2 {
                    rows.push(Row {
                        title: (*title).into(),
                        films,
                    });
                }
            }
            Err(error) => last_error = Some(error),
        }
    }
    match (rows.is_empty(), last_error) {
        (true, Some(error)) => Err(error),
        (true, None) => Err("TMDB lists no films on the Criterion Channel".into()),
        _ => Ok(rows),
    }
}

/// A discover result with a title and a 16:9 image.
fn tmdb_film(movie: &Value) -> Option<Film> {
    let id = movie["id"].as_u64()?;
    let title = clean(movie["title"].as_str()?);
    let backdrop = movie["backdrop_path"].as_str()?;
    // Image paths are `/` and a file name; anything else is not fetched.
    if title.is_empty()
        || !backdrop.starts_with('/')
        || !backdrop[1..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return None;
    }
    let year = movie["release_date"]
        .as_str()
        .and_then(|date| date.get(..4))
        .filter(|year| year.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or_default();
    Some(Film {
        id: format!("tmdb{id}"),
        title,
        year: year.into(),
        length: String::new(),
        link: None,
        tmdb: Some(id),
        image: format!("{TMDB_IMAGES}{backdrop}"),
    })
}

// ---- Finding a film's page ------------------------------------------------------------------

/// The film's page on the Channel among Movie of the Night's streaming options.
pub fn criterion_link(show: &crate::motn_api::Show) -> Option<String> {
    show.streaming_options
        .get(COUNTRY)?
        .iter()
        .filter(|option| option.service.id == SERVICE)
        .map(|option| option.link.as_str())
        .find(|link| {
            link.strip_prefix(ORIGIN)
                .is_some_and(|path| path.starts_with("/films/"))
        })
        .map(str::to_owned)
}

/// The Channel's search for a title, when its page is not known.
pub fn search_link(title: &str) -> String {
    let mut query = String::new();
    for byte in title.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            query.push(byte as char);
        } else {
            query.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("{ORIGIN}/search?q={query}")
}

// ---- Catalog worker -------------------------------------------------------------------------

/// The rows, or why there are none. `stale` rows are an old copy shown while a new one loads.
#[derive(Debug)]
pub struct Reply {
    pub rows: Result<Vec<Row>, String>,
    pub stale: bool,
}

/// A film's page, for the film `id` asked about.
pub type Resolved = (String, String);

enum Request {
    Rows,
    Link {
        id: String,
        tmdb: u64,
        title: String,
    },
}

pub struct Service {
    sender: mpsc::Sender<Request>,
    pub receiver: mpsc::Receiver<Reply>,
    pub resolved: mpsc::Receiver<Resolved>,
}

/// Where the worker gets rows and links: the network, or a test's fixtures.
pub trait Source: Send + 'static {
    fn rows(&self) -> Result<Vec<Row>, String>;
    /// None when no Movie of the Night key is stored.
    fn link(&self, tmdb: u64) -> Option<Result<Option<String>, String>>;
}

/// Luma's search keys, read again for every request so a key set later is used at once.
struct Network {
    keys: Option<PathBuf>,
    agent: ureq::Agent,
}

impl Network {
    fn keys(&self) -> crate::streaming_keys::Keys {
        self.keys
            .as_deref()
            .map(crate::streaming_keys::load)
            .unwrap_or_default()
    }
}

impl Source for Network {
    fn rows(&self) -> Result<Vec<Row>, String> {
        if let Some(http) = self.keys().tmdb_http() {
            let get = |path: &str, query: &[(&'static str, String)]| {
                http.get::<Value>(path, &Query(query.to_vec()))
                    .map_err(|e| format!("TMDB: {e}"))
            };
            match tmdb_rows(&get) {
                Ok(rows) => return Ok(rows),
                Err(error) => crate::telemetry::log_warn!("criterion", "{error}"),
            }
        }
        let rows = parse(&fetch(&self.agent)?);
        if rows.is_empty() {
            Err("The Criterion Channel page had no films".to_owned())
        } else {
            Ok(rows)
        }
    }

    fn link(&self, tmdb: u64) -> Option<Result<Option<String>, String>> {
        let http: Http = self.keys().motn_http()?;
        let client = crate::motn_api::Client::new(http);
        Some(
            client
                .get_show(&crate::motn_api::GetShowParams {
                    id: format!("movie/{tmdb}"),
                    country: Some(COUNTRY.into()),
                    series_granularity: None,
                    output_language: None,
                })
                .map(|show| criterion_link(&show))
                .map_err(|e| format!("Movie of the Night: {e}")),
        )
    }
}

impl Service {
    /// `keys`: where Luma keeps its search keys (`streaming_keys::path`).
    pub fn new(cache: DiskCache, keys: Option<PathBuf>) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .max_redirects(0)
            .proxy(None)
            .http_status_as_error(false)
            .build()
            .new_agent();
        Self::with_source(cache, Network { keys, agent })
    }

    fn with_source(cache: DiskCache, source: impl Source) -> Self {
        let (sender, requests) = mpsc::channel::<Request>();
        let (replies, receiver) = mpsc::channel();
        let (links, resolved) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(request) = requests.recv() {
                match request {
                    Request::Rows => {
                        for reply in load(&cache, &|| source.rows(), SystemTime::now()) {
                            if replies.send(reply).is_err() {
                                return;
                            }
                        }
                    }
                    Request::Link { id, tmdb, title } => {
                        let link = link(&cache, &source, tmdb, &title, SystemTime::now());
                        if links.send((id, link)).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Self {
            sender,
            receiver,
            resolved,
        }
    }

    pub fn load(&self) {
        let _ = self.sender.send(Request::Rows);
    }

    /// Finds the page of a film TMDB listed; the answer arrives on `resolved`.
    pub fn resolve(&self, film: &Film) {
        if let Some(tmdb) = film.tmdb {
            let _ = self.sender.send(Request::Link {
                id: film.id.clone(),
                tmdb,
                title: film.title.clone(),
            });
        }
    }
}

/// A film's page: from disk, else from Movie of the Night, else the Channel's search.
fn link(cache: &DiskCache, source: &dyn Source, tmdb: u64, title: &str, now: SystemTime) -> String {
    let key = format!("criterion:link:{tmdb}");
    if let Some(link) = cache.get::<String>(&key, LINK_AGE, now) {
        return link;
    }
    match source.link(tmdb) {
        Some(Ok(Some(link))) => {
            cache.put(&key, &link, now);
            link
        }
        Some(Ok(None)) => {
            crate::telemetry::log_info!("criterion", "Movie of the Night has no page for {tmdb}");
            search_link(title)
        }
        Some(Err(error)) => {
            crate::telemetry::log_warn!("criterion", "{error}");
            search_link(title)
        }
        None => search_link(title),
    }
}

/// A fresh copy from disk; else any copy from disk at once, then new rows from the network.
fn load(
    cache: &DiskCache,
    fetch: &dyn Fn() -> Result<Vec<Row>, String>,
    now: SystemTime,
) -> Vec<Reply> {
    if let Some(rows) = cache.get::<Vec<Row>>(CACHE_KEY, FRESH, now) {
        return vec![Reply {
            rows: Ok(rows),
            stale: false,
        }];
    }
    let mut replies = Vec::new();
    let stale = cache.get::<Vec<Row>>(CACHE_KEY, STALE, now);
    if let Some(rows) = &stale {
        replies.push(Reply {
            rows: Ok(rows.clone()),
            stale: true,
        });
    }
    match fetch() {
        Ok(rows) => {
            cache.put(CACHE_KEY, &rows, now);
            replies.push(Reply {
                rows: Ok(rows),
                stale: false,
            });
        }
        Err(error) => {
            crate::telemetry::log_warn!("criterion", "{error}");
            // The old copy stays on screen; with none, the screen says it could not load.
            if stale.is_none() {
                replies.push(Reply {
                    rows: Err(error),
                    stale: false,
                });
            }
        }
    }
    replies
}

fn fetch(agent: &ureq::Agent) -> Result<String, String> {
    let mut response = agent
        .get(&format!("{ORIGIN}/"))
        .header("User-Agent", "Mozilla/5.0 (Web0S; Linux) LumaNative")
        .header("Accept", "text/html")
        .call()
        .map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("The Criterion Channel answered HTTP {status}"));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(PAGE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > PAGE_LIMIT {
        return Err("The Criterion Channel page is too large".into());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

// ---- Screen state ---------------------------------------------------------------------------

/// Cards per row on screen at once; the row scrolls to keep the focused card inside them.
pub const VISIBLE_CARDS: usize = 4;

#[derive(Default)]
pub struct Browser {
    pub rows: Vec<Row>,
    /// Waiting for the first rows of this visit.
    pub loading: bool,
    pub error: Option<String>,
    /// The main loop asks the catalog worker for rows.
    pub requested: bool,
    /// The row at the top of the screen.
    pub first_row: usize,
    /// Per row, the first card on screen.
    pub first_card: Vec<usize>,
    /// Artwork by film ID, only for cards on or near the screen: each is about 360 KB.
    pub images: HashMap<String, Arc<Frame>>,
    /// Artwork asked for and not yet answered.
    pending: HashSet<String>,
    /// Artwork that could not be fetched this visit; the next visit tries again.
    failed: HashSet<String>,
    /// A film was opened in the player: the relaunch that brings Luma back keeps this screen.
    pub playing: bool,
    /// The film chosen whose page is being looked up; it opens when the page is found.
    pub resolving: Option<String>,
    /// A film whose page the main loop asks the catalog worker for.
    resolve_request: Option<Film>,
}

/// Rows kept around the screen's: the one above and the two below the three on screen.
const ROWS_ABOVE: usize = 1;
const ROWS_BELOW: usize = 5;
/// Cards kept either side of a row's visible ones.
const CARDS_AROUND: usize = 2;

impl Browser {
    /// Films whose artwork should be loaded: cards on screen and just beyond it, top row first.
    fn nearby(&self) -> Vec<&Film> {
        let rows = self.first_row.saturating_sub(ROWS_ABOVE)
            ..(self.first_row + ROWS_BELOW).min(self.rows.len());
        let mut films = Vec::new();
        for index in rows {
            let first = self.first_card(index);
            let row = &self.rows[index].films;
            let cards = first.saturating_sub(CARDS_AROUND)
                ..(first + VISIBLE_CARDS + CARDS_AROUND).min(row.len());
            films.extend(&row[cards]);
        }
        films
    }

    pub fn film(&self, row: usize, card: usize) -> Option<&Film> {
        self.rows.get(row)?.films.get(card)
    }

    pub fn first_card(&self, row: usize) -> usize {
        self.first_card.get(row).copied().unwrap_or(0)
    }

    /// Scrolls so that `card` of `row` is on screen, with the row at the top.
    fn reveal(&mut self, row: usize, card: usize) {
        self.first_row = row;
        if self.first_card.len() < self.rows.len() {
            self.first_card.resize(self.rows.len(), 0);
        }
        if let Some(first) = self.first_card.get_mut(row) {
            if card < *first {
                *first = card;
            } else if card >= *first + VISIBLE_CARDS {
                *first = card + 1 - VISIBLE_CARDS;
            }
        }
    }
}

impl State {
    pub fn criterion_open(&self) -> bool {
        matches!(self.overlay, Some(Overlay::Criterion { .. }))
    }

    /// Opens the screen, from its tile.
    pub fn open_criterion(&mut self) {
        if self.criterion_open() {
            return;
        }
        let restore = self.focus.clone();
        self.overlay = Some(Overlay::Criterion { restore });
        self.moving = None;
        self.pressed = None;
        let browser = &mut self.criterion;
        browser.requested = true;
        browser.error = None;
        browser.loading = browser.rows.is_empty();
        browser.failed.clear();
        self.focus = if browser.rows.is_empty() {
            Target::CriterionRetry
        } else {
            let (row, card) = (browser.first_row.min(browser.rows.len() - 1), 0);
            let card = browser.first_card(row).max(card);
            Target::Film(row, card)
        };
        self.reconcile_criterion_focus();
        self.dirty = true;
    }

    pub fn close_criterion(&mut self) {
        if let Some(Overlay::Criterion { restore }) = self.overlay.take() {
            self.focus = restore;
            self.reconcile_focus();
        }
        self.pressed = None;
        self.dirty = true;
    }

    /// Rows from the catalog worker.
    pub fn accept_criterion(&mut self, reply: Reply) {
        let open = self.criterion_open();
        let browser = &mut self.criterion;
        match reply.rows {
            Ok(rows) => {
                if rows != browser.rows {
                    // Keep the focused film when it is still listed; else the top of the screen.
                    let focused = match self.focus {
                        Target::Film(row, card) => browser.film(row, card).map(|f| f.id.clone()),
                        _ => None,
                    };
                    browser.rows = rows;
                    browser.first_card = vec![0; browser.rows.len()];
                    browser.first_row = 0;
                    if open {
                        let found = focused.and_then(|id| {
                            browser.rows.iter().enumerate().find_map(|(r, row)| {
                                let c = row.films.iter().position(|film| film.id == id)?;
                                Some((r, c))
                            })
                        });
                        let (row, card) = found.unwrap_or((0, 0));
                        browser.reveal(row, card);
                        self.focus = Target::Film(row, card);
                    }
                }
                let browser = &mut self.criterion;
                browser.loading = reply.stale && browser.rows.is_empty();
                browser.error = None;
            }
            Err(error) => {
                browser.loading = false;
                browser.error = Some(error);
            }
        }
        self.reconcile_criterion_focus();
        self.dirty = true;
    }

    fn reconcile_criterion_focus(&mut self) {
        if !self.criterion_open() {
            return;
        }
        let browser = &self.criterion;
        let valid = match self.focus {
            Target::Film(row, card) => browser.film(row, card).is_some(),
            Target::CriterionRetry => browser.rows.is_empty(),
            _ => false,
        };
        if !valid {
            self.focus = if browser.rows.is_empty() {
                Target::CriterionRetry
            } else {
                Target::Film(0, 0)
            };
        }
    }

    pub fn criterion_direction(&mut self, direction: Direction) -> Vec<Effect> {
        self.criterion.playing = false;
        let Target::Film(row, card) = self.focus else {
            return vec![];
        };
        let browser = &mut self.criterion;
        let (row, card) = match direction {
            Direction::Left => (row, card.saturating_sub(1)),
            Direction::Right => {
                let last = browser.rows[row].films.len() - 1;
                (row, (card + 1).min(last))
            }
            Direction::Up | Direction::Down => {
                let next = match direction {
                    Direction::Up if row > 0 => row - 1,
                    Direction::Down if row + 1 < browser.rows.len() => row + 1,
                    _ => return vec![],
                };
                // The card in the same place on screen, or the row's last card.
                let place = card.saturating_sub(browser.first_card(row));
                let first = browser.first_card(next);
                (
                    next,
                    (first + place).min(browser.rows[next].films.len() - 1),
                )
            }
        };
        browser.reveal(row, card);
        self.focus = Target::Film(row, card);
        self.dirty = true;
        vec![]
    }

    pub fn criterion_activate(&mut self) -> Vec<Effect> {
        match self.focus {
            Target::Film(row, card) => match self.criterion.film(row, card).cloned() {
                Some(Film {
                    link: Some(link), ..
                }) => {
                    self.criterion.playing = true;
                    vec![Effect::LaunchTitle {
                        app: PLAYER.into(),
                        service: SERVICE.into(),
                        link,
                    }]
                }
                // Listed by TMDB: its page on the Channel is looked up first.
                Some(film) => {
                    self.criterion.resolving = Some(film.id.clone());
                    self.criterion.resolve_request = Some(film);
                    self.notify(self.preferences.language.t("opening"));
                    self.dirty = true;
                    vec![]
                }
                None => vec![],
            },
            Target::CriterionRetry => {
                let browser = &mut self.criterion;
                browser.requested = true;
                browser.loading = true;
                browser.error = None;
                self.dirty = true;
                vec![]
            }
            _ => vec![],
        }
    }

    /// A film whose page to look up, once.
    pub fn criterion_resolve_request(&mut self) -> Option<Film> {
        self.criterion.resolve_request.take()
    }

    /// A film's page was found: it is kept on the film, and the film opens if it is still the
    /// one chosen and the screen is still open.
    pub fn criterion_resolved(&mut self, id: &str, link: String) -> Vec<Effect> {
        let browser = &mut self.criterion;
        let finds_page = link
            .strip_prefix(ORIGIN)
            .is_some_and(|path| path.starts_with("/films/"));
        if finds_page {
            for film in browser.rows.iter_mut().flat_map(|row| &mut row.films) {
                if film.id == id {
                    film.link = Some(link.clone());
                }
            }
        }
        if browser.resolving.as_deref() != Some(id) {
            return vec![];
        }
        browser.resolving = None;
        if !self.criterion_open() {
            return vec![];
        }
        self.criterion.playing = true;
        vec![Effect::LaunchTitle {
            app: PLAYER.into(),
            service: SERVICE.into(),
            link,
        }]
    }

    pub fn criterion_back(&mut self) -> Vec<Effect> {
        self.criterion.playing = false;
        self.close_criterion();
        vec![]
    }

    /// Whether to ask the catalog worker for rows now.
    pub fn criterion_load_requested(&mut self) -> bool {
        std::mem::take(&mut self.criterion.requested)
    }

    /// Artwork to fetch for the cards on and near the screen, with every image the worker should
    /// still load (`posters::Service::want`), or None when nothing changed. Artwork that moved
    /// away from the screen is dropped.
    pub fn criterion_image_requests(
        &mut self,
    ) -> Option<(Vec<String>, Vec<crate::posters::Request>)> {
        if !self.criterion_open() {
            return None;
        }
        let browser = &mut self.criterion;
        let nearby: Vec<Film> = browser.nearby().into_iter().cloned().collect();
        let before = browser.images.len();
        browser
            .images
            .retain(|id, _| nearby.iter().any(|film| &film.id == id));
        let mut requests = Vec::new();
        for film in &nearby {
            if browser.images.contains_key(&film.id)
                || browser.pending.contains(&film.id)
                || browser.failed.contains(&film.id)
            {
                continue;
            }
            browser.pending.insert(film.id.clone());
            requests.push(crate::posters::Request {
                show_id: image_key(&film.id),
                url: film.image.clone(),
                size: IMAGE_SIZE,
            });
        }
        // Requests for films no longer nearby are skipped by the worker; forget them here.
        browser
            .pending
            .retain(|id| nearby.iter().any(|film| &film.id == id));
        if requests.is_empty() && browser.images.len() == before {
            return None;
        }
        let wanted = nearby.iter().map(|film| image_key(&film.id)).collect();
        Some((wanted, requests))
    }

    pub fn accept_criterion_image(&mut self, key: &str, image: Option<Arc<Frame>>) {
        let Some(id) = key.strip_prefix(IMAGE_PREFIX) else {
            return;
        };
        let browser = &mut self.criterion;
        if !browser.pending.remove(id) {
            return;
        }
        match image {
            Some(image) => {
                browser.images.insert(id.to_owned(), image);
            }
            None => {
                browser.failed.insert(id.to_owned());
            }
        }
        self.dirty = true;
    }
}

const IMAGE_PREFIX: &str = "criterion:";

/// The image worker's ID for a film's artwork, unique among Luma's posters.
pub fn image_key(id: &str) -> String {
    format!("{IMAGE_PREFIX}{id}")
}

#[cfg(test)]
#[path = "criterion-tests.rs"]
mod tests;
