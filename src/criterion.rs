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
//!
//! OK on a film opens its detail page. Its details come from the film's Channel page
//! (schema.org data and the synopsis) when that is known, else from TMDB; Play opens the player.
use crate::cameras::Frame;
use crate::openapi::{Http, Query};
use crate::relevance::Words;
use crate::search_cache::DiskCache;
use crate::state::{Direction, Effect, Overlay, State, Target};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant, SystemTime},
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
/// The detail page's backdrop: TMDB's larger size of the same image.
const TMDB_HERO: &str = "https://image.tmdb.org/t/p/w1280";
/// The Channel's video host. Its older `cdn.jwplayer.com/v2` address now redirects here, and
/// images are fetched without following redirects.
const JW_IMAGES: &str = "https://img.jwplayer.com/v1/media";
const JW_IMAGES_OLD: &str = "https://cdn.jwplayer.com/v2/media";
/// The detail page's backdrop, decoded at most this size and stretched to the screen.
pub const HERO_SIZE: (u32, u32) = (1280, 720);
/// A film's details rarely change.
const DETAILS_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
const SYNOPSIS_LIMIT: usize = 1200;
const ROW_LIMIT: usize = 40;
const FILM_LIMIT: usize = 100;
const CATALOG_KEY: &str = "criterion:catalog";
/// The whole catalog is 160-odd TMDB pages, so it is fetched at most once a day.
const CATALOG_FRESH: Duration = Duration::from_secs(24 * 3600);
/// TMDB serves no page past 500.
const CATALOG_PAGES: u64 = 500;
/// Search results shown, best first.
pub const RESULT_LIMIT: usize = 48;

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

    /// Wide artwork for the detail page's backdrop.
    pub fn hero(&self) -> String {
        if let Some(path) = self.image.strip_prefix(TMDB_IMAGES) {
            return format!("{TMDB_HERO}{path}");
        }
        if valid_id(&self.id) && self.card_image().starts_with(JW_IMAGES) {
            return format!(
                "{JW_IMAGES}/{}/images/hero_wide.webp?fallback=default_16x9&width=1280",
                self.id
            );
        }
        self.card_image()
    }

    /// The card's artwork, at the video host's current address: rows cached before it moved
    /// still name the old one.
    pub fn card_image(&self) -> String {
        match self.image.strip_prefix(JW_IMAGES_OLD) {
            Some(rest) => format!("{JW_IMAGES}{rest}"),
            None => self.image.clone(),
        }
    }
}

/// What the detail page says about a film beyond its card.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Details {
    pub directors: Vec<String>,
    pub cast: Vec<String>,
    pub genres: Vec<String>,
    pub countries: Vec<String>,
    pub synopsis: String,
    /// Filled in when the card lacked them.
    pub year: String,
    pub length: String,
}

impl Details {
    fn is_empty(&self) -> bool {
        self.synopsis.is_empty() && self.directors.is_empty() && self.cast.is_empty()
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
                    "{JW_IMAGES}/{id}/images/regalia_16x9.webp?fallback=default_16x9&width=480"
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
    clean_up_to(text, 200)
}

/// Like [`clean`], at most `limit` characters.
fn clean_up_to(text: &str, limit: usize) -> String {
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
        .take(limit)
        .collect()
}

/// Markup with its tags removed.
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            return out;
        };
        out.push(' ');
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

// ---- Film details ---------------------------------------------------------------------------

/// A film page's details: schema.org data in its `application/ld+json` scripts, which search
/// engines read and so stay put, and the long synopsis in the element whose class ends in
/// `_description` (the structured data has only a one-line one).
pub fn parse_details(html: &str) -> Details {
    let mut details = Details::default();
    let mut rest = html;
    while let Some(start) = rest.find("application/ld+json") {
        rest = &rest[start..];
        let Some(open) = rest.find('>') else { break };
        let Some(close) = rest.find("</script>") else {
            break;
        };
        if open < close
            && let Ok(json) = serde_json::from_str::<Value>(&rest[open + 1..close])
        {
            let graph = json["@graph"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| vec![json]);
            for item in &graph {
                read_schema(item, &mut details);
            }
        }
        rest = &rest[close..];
    }
    if let Some(long) = element_text(html, r#"_description">"#, "</p>") {
        details.synopsis = long;
    }
    if details.directors.is_empty()
        && let Some(director) = element_text(html, r#"_secondLine">"#, "</div>")
    {
        details.directors.push(director);
    }
    details
}

fn read_schema(item: &Value, details: &mut Details) {
    let kind = item["@type"].as_str().unwrap_or_default();
    if kind != "Movie" && kind != "VideoObject" {
        return;
    }
    let names = |value: &Value| -> Vec<String> {
        let list = match value {
            Value::Array(list) => list.clone(),
            Value::Null => vec![],
            other => vec![other.clone()],
        };
        list.iter()
            .filter_map(|entry| entry.as_str().or_else(|| entry["name"].as_str()))
            .map(clean)
            .filter(|name| !name.is_empty())
            .take(6)
            .collect()
    };
    let fill = |field: &mut Vec<String>, value: &Value| {
        if field.is_empty() {
            *field = names(value);
        }
    };
    fill(&mut details.directors, &item["director"]);
    fill(&mut details.cast, &item["actor"]);
    fill(&mut details.genres, &item["genre"]);
    fill(&mut details.countries, &item["countryOfOrigin"]);
    if details.synopsis.is_empty()
        && let Some(text) = item["description"].as_str()
    {
        details.synopsis = clean_up_to(text, SYNOPSIS_LIMIT);
    }
    if details.year.is_empty()
        && let Some(year) = item["datePublished"]
            .as_str()
            .and_then(|date| date.get(..4))
        && year.bytes().all(|b| b.is_ascii_digit())
    {
        details.year = year.into();
    }
    if details.length.is_empty()
        && let Some(minutes) = item["duration"].as_str().and_then(iso_minutes)
    {
        details.length = running_time(minutes);
    }
}

/// The text of the first element whose opening tag ends in `marker`, up to `end`.
fn element_text(html: &str, marker: &str, end: &str) -> Option<String> {
    let start = html.find(marker)? + marker.len();
    let stop = start + html[start..].find(end)?;
    let text = clean_up_to(&strip_tags(&html[start..stop]), SYNOPSIS_LIMIT);
    (!text.is_empty()).then_some(text)
}

/// Whole minutes in an ISO 8601 duration such as `PT1H38M20S`.
fn iso_minutes(duration: &str) -> Option<u32> {
    let mut rest = duration.strip_prefix("PT")?;
    let mut minutes: u32 = 0;
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !c.is_ascii_digit())?;
        let value: u32 = rest[..digits].parse().ok()?;
        // The page is remote: a unit may be any character, and numbers any size.
        let unit = rest[digits..].chars().next()?;
        let add = match unit {
            'H' => value.checked_mul(60)?,
            'M' => value,
            'S' => 0,
            _ => return None,
        };
        minutes = minutes.checked_add(add)?;
        rest = &rest[digits + unit.len_utf8()..];
    }
    (minutes > 0).then_some(minutes)
}

/// "1 hr 38 min", as the Channel writes running times.
fn running_time(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} hr"),
        (h, m) => format!("{h} hr {m} min"),
    }
}

/// A film's details from TMDB, for films TMDB listed whose Channel page is not known yet.
pub fn tmdb_details(get: &TmdbGet, id: u64) -> Result<Details, String> {
    let movie = get(
        &format!("/3/movie/{id}"),
        &[("append_to_response", "credits".to_owned())],
    )?;
    let names = |list: &Value, filter: &dyn Fn(&Value) -> bool, limit: usize| -> Vec<String> {
        list.as_array()
            .into_iter()
            .flatten()
            .filter(|entry| filter(entry))
            .filter_map(|entry| entry["name"].as_str())
            .map(clean)
            .filter(|name| !name.is_empty())
            .take(limit)
            .collect()
    };
    let details = Details {
        directors: names(
            &movie["credits"]["crew"],
            &|entry| entry["job"] == "Director",
            3,
        ),
        cast: names(&movie["credits"]["cast"], &|_| true, 4),
        genres: names(&movie["genres"], &|_| true, 3),
        countries: names(&movie["production_countries"], &|_| true, 2),
        synopsis: clean_up_to(
            movie["overview"].as_str().unwrap_or_default(),
            SYNOPSIS_LIMIT,
        ),
        year: movie["release_date"]
            .as_str()
            .and_then(|date| date.get(..4))
            .filter(|year| year.bytes().all(|b| b.is_ascii_digit()))
            .unwrap_or_default()
            .into(),
        length: movie["runtime"]
            .as_u64()
            .filter(|&minutes| minutes > 0)
            .map(|minutes| running_time(minutes as u32))
            .unwrap_or_default(),
    };
    if details.is_empty() {
        Err(format!("TMDB has no details for {id}"))
    } else {
        Ok(details)
    }
}

/// Whether a link is a film's page on the Channel.
fn film_page(link: &str) -> bool {
    link.strip_prefix(ORIGIN)
        .is_some_and(|path| path.starts_with("/films/"))
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
    tmdb_listed_film(movie).filter(|film| !film.image.is_empty())
}

/// A discover result with a title; its image is empty when TMDB has no usable 16:9 one, and
/// the card then shows the title.
fn tmdb_listed_film(movie: &Value) -> Option<Film> {
    let id = movie["id"].as_u64()?;
    let title = clean(movie["title"].as_str()?);
    // Image paths are `/` and a file name; anything else is not fetched.
    let backdrop = movie["backdrop_path"].as_str().filter(|backdrop| {
        backdrop.starts_with('/')
            && backdrop[1..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    });
    if title.is_empty() {
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
        image: backdrop
            .map(|backdrop| format!("{TMDB_IMAGES}{backdrop}"))
            .unwrap_or_default(),
    })
}

// ---- Search ---------------------------------------------------------------------------------

/// A film in the Channel's whole catalog, as search matches it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Listed {
    pub film: Film,
    /// The title in its own language: "Shichinin no samurai" finds *Seven Samurai*.
    pub original: String,
    pub popularity: f64,
}

/// The Channel's films as read from TMDB.
#[derive(Debug, PartialEq)]
pub struct Catalog {
    pub films: Vec<Listed>,
    /// Every page was read. A partial list is searched this visit but not kept.
    pub complete: bool,
}

/// Every film TMDB lists on the Channel, most popular first: about 3,200 films, 20 to a page.
/// A page that fails ends the list there; the films before it are still worth searching.
pub fn tmdb_catalog(get: &TmdbGet) -> Result<Catalog, String> {
    let provider = provider(get)?.to_string();
    let mut films: Vec<Listed> = Vec::new();
    let mut seen = HashSet::new();
    let mut page = 1;
    let mut pages = 1;
    let mut complete = true;
    while page <= pages.min(CATALOG_PAGES) {
        let query = vec![
            ("watch_region", COUNTRY.to_uppercase()),
            ("with_watch_providers", provider.clone()),
            ("with_watch_monetization_types", "flatrate".to_owned()),
            ("include_adult", "false".to_owned()),
            ("sort_by", "popularity.desc".to_owned()),
            ("page", page.to_string()),
        ];
        let reply = match get("/3/discover/movie", &query) {
            Ok(reply) => reply,
            Err(error) if films.is_empty() => return Err(error),
            Err(error) => {
                crate::telemetry::log_warn!(
                    "criterion",
                    "catalog: stopped at page {page} of {pages}: {error}"
                );
                complete = false;
                break;
            }
        };
        pages = reply["total_pages"].as_u64().unwrap_or(0);
        for movie in reply["results"].as_array().into_iter().flatten() {
            let Some(film) = tmdb_listed_film(movie) else {
                continue;
            };
            // Popularity shifts while the pages are read: a film can appear on two.
            if !seen.insert(film.id.clone()) {
                continue;
            }
            films.push(Listed {
                original: movie["original_title"]
                    .as_str()
                    .map(clean)
                    .unwrap_or_default(),
                popularity: movie["popularity"].as_f64().unwrap_or(0.),
                film,
            });
        }
        page += 1;
    }
    if films.is_empty() {
        Err("TMDB lists no films on the Criterion Channel".into())
    } else {
        Ok(Catalog { films, complete })
    }
}

/// A catalog film with its titles split into words once, not at every keystroke: matching
/// the whole catalog then takes a few milliseconds even on the TV.
#[derive(Clone, Debug, PartialEq)]
pub struct Indexed {
    pub listed: Listed,
    title: Words,
    /// Empty when it is the title again.
    original: Words,
}

/// Films ready for [`search`].
pub fn index(films: impl IntoIterator<Item = Listed>) -> Vec<Indexed> {
    films
        .into_iter()
        .map(|listed| Indexed {
            title: Words::new(&listed.film.title),
            original: if listed.original == listed.film.title {
                Words::default()
            } else {
                Words::new(&listed.original)
            },
            listed,
        })
        .collect()
}

/// The films in `rows`, once each, in their order: what search covers without a catalog.
fn index_rows(rows: &[Row]) -> Vec<Indexed> {
    let mut seen = HashSet::new();
    index(
        rows.iter()
            .flat_map(|row| &row.films)
            .filter(|film| seen.insert(film.id.clone()))
            .map(|film| Listed {
                film: film.clone(),
                original: String::new(),
                popularity: 0.,
            }),
    )
}

/// How a title matches the query, lower first: it starts with the query (whole words, then
/// the last only begun), the query's whole words are in it, a word in it begins with the
/// query, then the query is anywhere in it. The Search tab's tiers
/// ([`crate::relevance::match_words`]) with a word begun mid-title above text inside a word:
/// "umbr" finds *The Umbrellas of Cherbourg* before titles with "umbr" in a word.
fn tier(query: &Words, title: &Words) -> u8 {
    match crate::relevance::match_words(query, title) {
        3 if crate::relevance::starts_a_word(query, title) => 3,
        3 => 4,
        4 => 5,
        tier => tier,
    }
}

/// Films whose title (or original title) matches `query`, best first by [`tier`], then the
/// most popular. Accents, punctuation and spacing do not matter.
pub fn search(query: &str, films: &[Indexed]) -> Vec<Film> {
    let query = Words::new(query);
    let mut found: Vec<(u8, f64, usize)> = films
        .iter()
        .enumerate()
        .filter_map(|(index, film)| {
            let mut rank = tier(&query, &film.title);
            if rank > 0 && !film.original.is_empty() {
                rank = rank.min(tier(&query, &film.original));
            }
            (rank < 5).then_some((rank, film.listed.popularity, index))
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.total_cmp(&a.1)).then(a.2.cmp(&b.2)));
    found
        .into_iter()
        .take(RESULT_LIMIT)
        .map(|(_, _, index)| films[index].listed.film.clone())
        .collect()
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

/// A film's details, for the film `id` asked about.
pub type Described = (String, Result<Details, String>);

enum Request {
    Rows,
    Link {
        id: String,
        tmdb: u64,
        title: String,
    },
    Details(Film),
}

pub struct Service {
    sender: mpsc::Sender<Request>,
    catalog: mpsc::Sender<()>,
    pub receiver: mpsc::Receiver<Reply>,
    /// The whole catalog for search; empty when it cannot be had.
    pub indexed: mpsc::Receiver<Indexing>,
    pub resolved: mpsc::Receiver<Resolved>,
    pub described: mpsc::Receiver<Described>,
}

/// Where the worker gets rows and links: the network, or a test's fixtures.
pub trait Source: Send + Sync + 'static {
    fn rows(&self) -> Result<Vec<Row>, String>;
    /// Every film on the Channel, for search. Fails without a TMDB token.
    fn catalog(&self) -> Result<Catalog, String> {
        Err("No catalog".into())
    }
    /// None when no Movie of the Night key is stored.
    fn link(&self, tmdb: u64) -> Option<Result<Option<String>, String>>;
    fn details(&self, film: &Film) -> Result<Details, String>;
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
        let rows = parse(&fetch(&self.agent, &format!("{ORIGIN}/"))?);
        if rows.is_empty() {
            Err("The Criterion Channel page had no films".to_owned())
        } else {
            Ok(rows)
        }
    }

    fn catalog(&self) -> Result<Catalog, String> {
        let http = self
            .keys()
            .tmdb_http()
            .ok_or("No TMDB token: search covers the rows on screen")?;
        let get = |path: &str, query: &[(&'static str, String)]| {
            http.get::<Value>(path, &Query(query.to_vec()))
                .map_err(|e| format!("TMDB: {e}"))
        };
        tmdb_catalog(&get)
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

    /// From the film's page on the Channel when it is known, else from TMDB.
    fn details(&self, film: &Film) -> Result<Details, String> {
        if let Some(link) = film.link.as_deref().filter(|link| film_page(link)) {
            let details = parse_details(&fetch(&self.agent, link)?);
            return if details.is_empty() {
                Err(format!(
                    "The Criterion Channel page for {} had no details",
                    film.id
                ))
            } else {
                Ok(details)
            };
        }
        let (Some(id), Some(http)) = (film.tmdb, self.keys().tmdb_http()) else {
            return Err(format!("No details for {}", film.id));
        };
        let get = |path: &str, query: &[(&'static str, String)]| {
            http.get::<Value>(path, &Query(query.to_vec()))
                .map_err(|e| format!("TMDB: {e}"))
        };
        tmdb_details(&get, id)
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
        let source = Arc::new(source);
        let (sender, requests) = mpsc::channel::<Request>();
        let (replies, receiver) = mpsc::channel();
        let (links, resolved) = mpsc::channel();
        let (descriptions, described) = mpsc::channel();
        // The catalog takes a minute to read on the TV: its own thread, so Play is not kept
        // waiting behind it.
        let (catalog, catalog_requests) = mpsc::channel::<()>();
        let (catalogs, indexed) = mpsc::channel();
        {
            let (source, cache) = (source.clone(), cache.clone());
            std::thread::spawn(move || {
                while catalog_requests.recv().is_ok() {
                    // Requests made while one was read are answered by it.
                    while catalog_requests.try_recv().is_ok() {}
                    // Each copy goes as soon as it is had: an old one from disk is searched
                    // while TMDB is read again, which takes a minute on the TV.
                    load_catalog(
                        &cache,
                        &|| source.catalog(),
                        SystemTime::now(),
                        &mut |films, fresh| {
                            // Split into words here, not on the SDL thread. Once Luma is
                            // quitting, nothing receives the reply.
                            let _ = catalogs.send(Indexing {
                                films: index(films),
                                fresh,
                            });
                        },
                    );
                }
            });
        }
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
                        let link = link(&cache, &*source, tmdb, &title, SystemTime::now());
                        if links.send((id, link)).is_err() {
                            return;
                        }
                    }
                    Request::Details(film) => {
                        let details = details(&cache, &*source, &film, SystemTime::now());
                        if descriptions.send((film.id, details)).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Self {
            sender,
            catalog,
            receiver,
            indexed,
            resolved,
            described,
        }
    }

    pub fn load(&self) {
        let _ = self.sender.send(Request::Rows);
    }

    /// Asks for the whole catalog, for search; it arrives on `indexed`.
    pub fn load_catalog(&self) {
        let _ = self.catalog.send(());
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

    /// Asks for a film's details; the answer arrives on `described`.
    pub fn describe(&self, film: &Film) {
        let _ = self.sender.send(Request::Details(film.clone()));
    }
}

/// A film's details: from disk, else from the network. Failures are not kept.
fn details(
    cache: &DiskCache,
    source: &dyn Source,
    film: &Film,
    now: SystemTime,
) -> Result<Details, String> {
    let key = format!("criterion:details:{}", film.id);
    if let Some(details) = cache.get::<Details>(&key, DETAILS_AGE, now) {
        return Ok(details);
    }
    let details = source.details(film);
    match &details {
        Ok(details) => cache.put(&key, details, now),
        Err(error) => crate::telemetry::log_warn!("criterion", "{error}"),
    }
    details
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

/// The catalog for search, split into words, and whether it can be kept for a day: a fresh
/// copy needs no new request until then.
#[derive(Debug)]
pub struct Indexing {
    pub films: Vec<Indexed>,
    pub fresh: bool,
}

/// The catalog, each with whether it is fresh: a copy from disk less than a day old; else any
/// copy from disk at once, then a new one from TMDB. Empty when there is none, so search covers
/// the rows on screen. A partial list (a page failed) is not kept, so the next visit tries
/// again, and it is offered only when it is longer than the copy already had.
fn load_catalog(
    cache: &DiskCache,
    fetch: &dyn Fn() -> Result<Catalog, String>,
    now: SystemTime,
    reply: &mut dyn FnMut(Vec<Listed>, bool),
) {
    if let Some(films) = cache.get::<Vec<Listed>>(CATALOG_KEY, CATALOG_FRESH, now) {
        reply(films, true);
        return;
    }
    let stale = cache.get::<Vec<Listed>>(CATALOG_KEY, STALE, now);
    let stale_len = stale.as_ref().map(Vec::len);
    if let Some(films) = stale {
        reply(films, false);
    }
    match fetch() {
        Ok(Catalog {
            films,
            complete: true,
        }) => {
            cache.put(CATALOG_KEY, &films, now);
            reply(films, true);
        }
        Ok(Catalog { films, .. }) => {
            if stale_len.is_none_or(|stale| films.len() > stale) {
                reply(films, false);
            }
        }
        Err(error) => {
            crate::telemetry::log_info!("criterion", "catalog: {error}");
            if stale_len.is_none() {
                reply(Vec::new(), false);
            }
        }
    }
}

fn fetch(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    let mut response = agent
        .get(url)
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
/// Rows wholly on screen, with the next one's top showing below. Focus moves down through them
/// before the rows scroll, as on Apple TV and Google TV, rather than every row jumping to the top.
pub const VISIBLE_ROWS: usize = 2;
/// How quickly scrolling settles: about 95% of the way in 0.2 s.
const SCROLL_SPEED: f32 = 15.;

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
    /// Where the rows are drawn, gliding toward `first_row` and `first_card`.
    pub scroll_row: f32,
    scroll_cards: Vec<f32>,
    /// The last animation step, or None while nothing moves.
    animated: Option<Instant>,
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
    /// The film whose detail page is open over the rows.
    pub detail: Option<Detail>,
    /// A film whose details the main loop asks the catalog worker for.
    details_request: Option<Film>,
    /// Details found this run, by film ID.
    described: HashMap<String, Details>,
    /// Films whose details the catalog worker has been asked for and not yet answered.
    describing: HashSet<String>,
    /// What is typed in the search field. While it holds anything, `rows` are its results.
    pub query: String,
    /// Every film on the Channel, for search; empty until loaded, or without a TMDB token.
    pub catalog: Vec<Indexed>,
    /// The main loop asks the catalog worker for the whole catalog.
    catalog_requested: bool,
    /// When a fresh catalog arrived: until it is a day old, visits ask for none.
    catalog_fresh: Option<Instant>,
    /// The Channel's rows, where they were scrolled and what was focused, put aside while
    /// search results take their place.
    channel: Option<Channel>,
    /// The keyboard is open over the screen, which comes back with focus `restore` (Back's
    /// target for the whole screen) when it closes.
    pub typing: Option<Target>,
    /// The card Up left for the search field, which Down returns to.
    above: Option<(usize, usize)>,
}

/// The Channel's rows as they were when a search began.
struct Channel {
    rows: Vec<Row>,
    first_row: usize,
    first_card: Vec<usize>,
    focus: Target,
}

/// A film's page: its backdrop, details and Play, like a streaming service's title page.
pub struct Detail {
    pub film: Film,
    /// Where the film was chosen, for Back. Moves with the film when new rows arrive.
    pub row: usize,
    pub card: usize,
    /// None while loading, or when they could not be found (`failed`).
    pub details: Option<Details>,
    pub failed: bool,
    pub hero: Option<Arc<Frame>>,
    hero_pending: bool,
    hero_failed: bool,
    /// Fades in from 0 when opened.
    pub shown: f32,
}

/// Rows kept around the screen's: the one above and those below the ones on screen.
const ROWS_ABOVE: usize = 1;
const ROWS_BELOW: usize = 5;
/// Cards kept either side of a row's visible ones.
const CARDS_AROUND: usize = 2;

impl Browser {
    /// Search results are on screen in place of the Channel's rows.
    pub fn searching(&self) -> bool {
        self.channel.is_some()
    }

    /// The Channel's own rows, whether on screen or put aside for search results.
    fn channel_rows(&self) -> &[Row] {
        self.channel
            .as_ref()
            .map_or(&self.rows, |channel| &channel.rows)
    }

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

    /// Where a row's cards are drawn, in cards: gliding toward [`Self::first_card`].
    pub fn scroll_card(&self, row: usize) -> f32 {
        self.scroll_cards
            .get(row)
            .copied()
            .unwrap_or_else(|| self.first_card(row) as f32)
    }

    /// Scrolls as little as keeps `card` of `row` on screen.
    fn reveal(&mut self, row: usize, card: usize) {
        if self.animated.is_none() {
            self.animated = Some(crate::clock::now());
        }
        if row < self.first_row {
            self.first_row = row;
        } else if row >= self.first_row + VISIBLE_ROWS {
            self.first_row = row + 1 - VISIBLE_ROWS;
        }
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

    /// Drops a details request the main loop has not sent yet.
    fn cancel_details_request(&mut self) {
        if let Some(film) = self.details_request.take() {
            self.describing.remove(&film.id);
        }
    }

    /// Puts the rows where they are going at once.
    fn settle(&mut self) {
        self.scroll_row = self.first_row as f32;
        self.scroll_cards = (0..self.rows.len())
            .map(|row| self.first_card(row) as f32)
            .collect();
        if let Some(detail) = &mut self.detail {
            detail.shown = 1.;
        }
        self.animated = None;
    }

    /// Whether anything is still gliding or fading in.
    pub fn settling(&self) -> bool {
        self.scroll_row != self.first_row as f32
            || (0..self.rows.len()).any(|row| self.scroll_card(row) != self.first_card(row) as f32)
            || self.detail.as_ref().is_some_and(|detail| detail.shown < 1.)
    }

    /// One animation step; whether anything moved.
    pub fn animate(&mut self, reduced: bool, now: Instant) -> bool {
        if !self.settling() {
            self.animated = None;
            return false;
        }
        if reduced {
            self.settle();
            return true;
        }
        let last = self.animated.replace(now).unwrap_or(now);
        let dt = now.saturating_duration_since(last).as_secs_f32().min(0.1);
        let step = 1. - (-SCROLL_SPEED * dt).exp();
        let approach = |current: &mut f32, target: f32| {
            if (*current - target).abs() < 0.002 {
                *current = target;
            } else {
                *current += (target - *current) * step;
            }
        };
        approach(&mut self.scroll_row, self.first_row as f32);
        if self.scroll_cards.len() != self.rows.len() {
            self.scroll_cards.resize(self.rows.len(), 0.);
        }
        for (row, scroll) in self.scroll_cards.iter_mut().enumerate() {
            let target = self.first_card.get(row).copied().unwrap_or(0) as f32;
            approach(scroll, target);
        }
        if let Some(detail) = &mut self.detail {
            detail.shown = (detail.shown + dt / 0.22).min(1.);
        }
        true
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
        browser.catalog_requested = browser
            .catalog_fresh
            .is_none_or(|at| crate::clock::now().saturating_duration_since(at) >= CATALOG_FRESH);
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
        self.criterion.settle();
        self.reconcile_criterion_focus();
        self.dirty = true;
    }

    /// The screen is on screen: open, or behind the keyboard typing a search.
    pub fn criterion_visible(&self) -> bool {
        self.criterion_open() || self.criterion.typing.is_some()
    }

    pub fn close_criterion(&mut self) {
        // A lookup answered after this only keeps the film's page; it opens nothing.
        self.criterion.resolving = None;
        self.criterion.resolve_request = None;
        self.criterion.detail = None;
        self.criterion.cancel_details_request();
        if let Some(restore) = self.criterion.typing.take() {
            // The keyboard took the screen's place; it goes with it.
            self.overlay = Some(Overlay::Criterion { restore });
            self.panel_started = None;
        }
        // The next visit starts on the Channel's rows.
        self.criterion.query.clear();
        self.end_criterion_search();
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
        // Search results are on screen: the new rows wait behind them, scrolled to the top.
        if let Some(channel) = &mut browser.channel {
            match reply.rows {
                Ok(rows) => {
                    if rows != channel.rows {
                        channel.first_card = vec![0; rows.len()];
                        channel.first_row = 0;
                        channel.focus = Target::Film(0, 0);
                        channel.rows = rows;
                    }
                    browser.loading = false;
                    browser.error = None;
                }
                Err(error) => {
                    browser.loading = false;
                    browser.error = Some(error);
                }
            }
            if browser.catalog.is_empty() {
                // Without a catalog, search covers these rows.
                self.criterion_search_changed();
            }
            self.dirty = true;
            return;
        }
        match reply.rows {
            Ok(rows) => {
                if rows != browser.rows {
                    // Keep the focused film when it is still listed; else the top of the screen.
                    let focused = match (&browser.detail, &self.focus) {
                        (Some(detail), _) => Some(detail.film.id.clone()),
                        (None, Target::Film(row, card)) => {
                            browser.film(*row, *card).map(|f| f.id.clone())
                        }
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
                        match &mut browser.detail {
                            Some(detail) => (detail.row, detail.card) = (row, card),
                            None => self.focus = Target::Film(row, card),
                        }
                    }
                    browser.settle();
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
            _ if browser.detail.is_some() => self.focus == Target::CriterionPlay,
            Target::Film(row, card) => browser.film(row, card).is_some(),
            Target::CriterionRetry => browser.rows.is_empty() && !browser.searching(),
            Target::CriterionSearch => !browser.rows.is_empty() || browser.searching(),
            _ => false,
        };
        if !valid {
            self.focus = if browser.detail.is_some() {
                Target::CriterionPlay
            } else if browser.searching() {
                Target::CriterionSearch
            } else if browser.rows.is_empty() {
                Target::CriterionRetry
            } else {
                Target::Film(0, 0)
            };
        }
    }

    pub fn criterion_direction(&mut self, direction: Direction) -> Vec<Effect> {
        self.criterion.playing = false;
        if self.focus == Target::CriterionSearch {
            // Down from the search field: the top row on screen, where it is scrolled to.
            let browser = &mut self.criterion;
            if matches!(direction, Direction::Down) && !browser.rows.is_empty() {
                let (row, card) = browser
                    .above
                    .filter(|&(row, card)| browser.film(row, card).is_some())
                    .unwrap_or_else(|| {
                        let row = browser.first_row.min(browser.rows.len() - 1);
                        (
                            row,
                            browser
                                .first_card(row)
                                .min(browser.rows[row].films.len() - 1),
                        )
                    });
                browser.reveal(row, card);
                self.focus = Target::Film(row, card);
                self.dirty = true;
            }
            return vec![];
        }
        // The detail page has only Play.
        let Target::Film(row, card) = self.focus else {
            return vec![];
        };
        if row == 0 && matches!(direction, Direction::Up) {
            self.criterion.above = Some((row, card));
            self.focus = Target::CriterionSearch;
            self.dirty = true;
            return vec![];
        }
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
            Target::Film(row, card) => {
                self.open_film(row, card);
                vec![]
            }
            Target::CriterionPlay => match self.criterion.detail.as_ref().map(|d| d.film.clone()) {
                Some(film) => self.play(film),
                None => vec![],
            },
            Target::CriterionSearch => {
                self.open_criterion_search();
                vec![]
            }
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

    /// Opens a film's detail page, with Play focused.
    fn open_film(&mut self, row: usize, card: usize) {
        let browser = &mut self.criterion;
        let Some(film) = browser.film(row, card).cloned() else {
            return;
        };
        let details = browser.described.get(&film.id).cloned();
        // One request per film at a time: reopening a page still loading asks again for nothing.
        if details.is_none() && browser.describing.insert(film.id.clone()) {
            browser.details_request = Some(film.clone());
        }
        browser.detail = Some(Detail {
            film,
            row,
            card,
            details,
            failed: false,
            hero: None,
            hero_pending: false,
            hero_failed: false,
            shown: if self.preferences.reduced_motion {
                1.
            } else {
                0.
            },
        });
        if browser.animated.is_none() {
            browser.animated = Some(crate::clock::now());
        }
        self.focus = Target::CriterionPlay;
        self.dirty = true;
    }

    /// Back on the detail page: the rows again, with the film focused.
    fn close_film(&mut self) {
        let Some(detail) = self.criterion.detail.take() else {
            return;
        };
        self.criterion.cancel_details_request();
        // A lookup for Play answered after this only keeps the film's page; it opens nothing.
        self.criterion.resolving = None;
        self.criterion.resolve_request = None;
        self.focus = Target::Film(detail.row, detail.card);
        self.criterion.reveal(detail.row, detail.card);
        self.reconcile_criterion_focus();
        self.dirty = true;
    }

    /// Opens a film in the player, once its page on the Channel is known.
    fn play(&mut self, film: Film) -> Vec<Effect> {
        match film.link {
            Some(link) => {
                // A film still being looked up is not opened after this one.
                self.criterion.resolving = None;
                self.criterion.resolve_request = None;
                self.criterion.playing = true;
                vec![Effect::LaunchTitle {
                    app: PLAYER.into(),
                    service: SERVICE.into(),
                    link,
                }]
            }
            // Listed by TMDB: its page on the Channel is looked up first.
            None => {
                self.criterion.resolving = Some(film.id.clone());
                self.criterion.resolve_request = Some(film);
                self.notify(self.preferences.language.t("opening"));
                self.dirty = true;
                vec![]
            }
        }
    }

    /// A film whose details to look up, once.
    pub fn criterion_details_request(&mut self) -> Option<Film> {
        self.criterion.details_request.take()
    }

    /// A film's details from the catalog worker.
    pub fn accept_criterion_details(&mut self, (id, details): Described) {
        let browser = &mut self.criterion;
        browser.describing.remove(&id);
        if let Ok(details) = &details {
            browser.described.insert(id.clone(), details.clone());
        }
        if let Some(detail) = browser.detail.as_mut().filter(|d| d.film.id == id) {
            match details {
                Ok(details) => detail.details = Some(details),
                Err(_) => detail.failed = true,
            }
            self.dirty = true;
        }
    }

    /// One animation step of the rows and the detail page.
    pub fn criterion_tick(&mut self) {
        if self.criterion_open() {
            self.dirty |= self
                .criterion
                .animate(self.preferences.reduced_motion, crate::clock::now());
        }
    }

    pub fn criterion_animating(&self) -> bool {
        self.criterion_open() && self.criterion.settling()
    }

    /// A film whose page to look up, once.
    pub fn criterion_resolve_request(&mut self) -> Option<Film> {
        self.criterion.resolve_request.take()
    }

    /// A film's page was found: it is kept on the film, and the film opens if it is still the
    /// one chosen and the screen is still open.
    pub fn criterion_resolved(&mut self, id: &str, link: String) -> Vec<Effect> {
        let browser = &mut self.criterion;
        if film_page(&link) {
            let detail = browser.detail.as_mut().map(|detail| &mut detail.film);
            let channel = browser
                .channel
                .as_mut()
                .map(|channel| &mut channel.rows)
                .into_iter()
                .flatten();
            for film in browser
                .rows
                .iter_mut()
                .chain(channel)
                .flat_map(|row| &mut row.films)
                .chain(browser.catalog.iter_mut().map(|film| &mut film.listed.film))
                .chain(detail)
            {
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
        if self.criterion.detail.is_some() {
            self.close_film();
            return vec![];
        }
        if self.criterion.searching() {
            if self.focus == Target::CriterionSearch {
                // Back from the field ends the search: the Channel's rows, as they were left.
                self.criterion.query.clear();
                self.end_criterion_search();
            } else {
                // Back from a result returns to the field, which keeps the search.
                self.focus = Target::CriterionSearch;
            }
            self.reconcile_criterion_focus();
            self.dirty = true;
            return vec![];
        }
        self.close_criterion();
        vec![]
    }

    /// OK on the search field: the keyboard comes up over the screen, which stays drawn
    /// behind it, its results following what is typed.
    pub fn open_criterion_search(&mut self) {
        let Some(Overlay::Criterion { restore }) = self.overlay.clone() else {
            return;
        };
        self.criterion.typing = Some(restore);
        self.open_keyboard(crate::state::TextField::Criterion);
    }

    /// The keyboard closed by any route: the screen comes back, focused on the search field.
    pub(crate) fn commit_criterion_search(&mut self) {
        let Some(restore) = self.criterion.typing.take() else {
            return;
        };
        self.overlay = Some(Overlay::Criterion { restore });
        self.panel_started = None;
        self.focus = Target::CriterionSearch;
        self.reconcile_criterion_focus();
        self.dirty = true;
    }

    /// Enter on the keyboard: the first result, when there is one.
    pub(crate) fn focus_first_criterion_result(&mut self) {
        if self.criterion_open() && self.criterion.searching() && !self.criterion.rows.is_empty() {
            self.focus = Target::Film(0, 0);
            self.criterion.reveal(0, 0);
            self.criterion.settle();
        }
    }

    /// The search text changed: its results replace the rows, or, once it is empty again,
    /// the Channel's rows come back as they were left.
    pub(crate) fn criterion_search_changed(&mut self) {
        let browser = &mut self.criterion;
        if browser.query.trim().is_empty() {
            self.end_criterion_search();
            return;
        }
        let films = if browser.catalog.is_empty() {
            search(&browser.query, &index_rows(browser.channel_rows()))
        } else {
            search(&browser.query, &browser.catalog)
        };
        let language = self.preferences.language;
        let heading = if films.is_empty() {
            String::new()
        } else {
            language.format("criterionResults", &[("query", browser.query.trim())])
        };
        let rows: Vec<Row> = films
            .chunks(VISIBLE_CARDS)
            .enumerate()
            .map(|(index, films)| Row {
                title: if index == 0 {
                    heading.clone()
                } else {
                    String::new()
                },
                films: films.to_vec(),
            })
            .collect();
        if browser.channel.is_none() {
            browser.channel = Some(Channel {
                rows: std::mem::take(&mut browser.rows),
                first_row: browser.first_row,
                first_card: std::mem::take(&mut browser.first_card),
                focus: match (&self.focus, browser.above) {
                    (focus @ Target::Film(..), _) => focus.clone(),
                    (_, Some((row, card))) => Target::Film(row, card),
                    _ => Target::Film(browser.first_row, browser.first_card(browser.first_row)),
                },
            });
        }
        browser.above = None;
        browser.first_row = 0;
        browser.first_card = vec![0; rows.len()];
        browser.rows = rows;
        browser.settle();
        // Results replace each other as typing goes on: a focused one may no longer exist.
        if self.criterion_open() {
            self.reconcile_criterion_focus();
        }
        self.dirty = true;
    }

    /// Puts the Channel's rows back where they were before the search.
    fn end_criterion_search(&mut self) {
        let browser = &mut self.criterion;
        let Some(channel) = browser.channel.take() else {
            return;
        };
        browser.above = None;
        browser.rows = channel.rows;
        browser.first_row = channel.first_row;
        browser.first_card = channel.first_card;
        browser.settle();
        if self.criterion_open() && self.criterion.detail.is_none() {
            self.focus = channel.focus;
            self.reconcile_criterion_focus();
        }
        self.dirty = true;
    }

    /// Whether to ask the catalog worker for the whole catalog now.
    pub fn criterion_catalog_requested(&mut self) -> bool {
        std::mem::take(&mut self.criterion.catalog_requested)
    }

    /// The whole catalog from the catalog worker. An empty one (no TMDB token, or TMDB away)
    /// does not replace one already had.
    pub fn accept_criterion_catalog(&mut self, Indexing { films, fresh }: Indexing) {
        if fresh {
            self.criterion.catalog_fresh = Some(crate::clock::now());
        }
        if films.is_empty() && !self.criterion.catalog.is_empty() {
            return;
        }
        self.criterion.catalog = films;
        if self.criterion.searching() {
            self.criterion_search_changed();
        }
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
        if !self.criterion_visible() {
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
            // TMDB has no 16:9 image for some films: their cards show the title.
            if film.image.is_empty() {
                browser.failed.insert(film.id.clone());
                continue;
            }
            browser.pending.insert(film.id.clone());
            requests.push(crate::posters::Request {
                show_id: image_key(&film.id),
                url: film.card_image(),
                size: IMAGE_SIZE,
            });
        }
        // Requests for films no longer nearby are skipped by the worker; forget them here.
        browser
            .pending
            .retain(|id| nearby.iter().any(|film| &film.id == id));
        let mut wanted: Vec<String> = nearby.iter().map(|film| image_key(&film.id)).collect();
        let mut hero_changed = false;
        if let Some(detail) = &mut browser.detail {
            wanted.push(hero_key(&detail.film.id));
            if detail.film.image.is_empty() {
                detail.hero_failed = true;
            }
            if detail.hero.is_none() && !detail.hero_pending && !detail.hero_failed {
                detail.hero_pending = true;
                hero_changed = true;
                requests.push(crate::posters::Request {
                    show_id: hero_key(&detail.film.id),
                    url: detail.film.hero(),
                    size: HERO_SIZE,
                });
            }
        }
        if requests.is_empty() && browser.images.len() == before && !hero_changed {
            return None;
        }
        Some((wanted, requests))
    }

    pub fn accept_criterion_image(&mut self, key: &str, image: Option<Arc<Frame>>) {
        if let Some(id) = key.strip_prefix(HERO_PREFIX) {
            if let Some(detail) = self
                .criterion
                .detail
                .as_mut()
                .filter(|detail| detail.film.id == id && detail.hero_pending)
            {
                detail.hero_pending = false;
                match image {
                    Some(image) => detail.hero = Some(image),
                    None => detail.hero_failed = true,
                }
                self.dirty = true;
            }
            return;
        }
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
const HERO_PREFIX: &str = "criterion-hero:";

/// The image worker's ID for a film's detail-page backdrop.
pub fn hero_key(id: &str) -> String {
    format!("{HERO_PREFIX}{id}")
}

/// The image worker's ID for a film's artwork, unique among Luma's posters.
pub fn image_key(id: &str) -> String {
    format!("{IMAGE_PREFIX}{id}")
}

#[cfg(test)]
#[path = "criterion-tests.rs"]
mod tests;
