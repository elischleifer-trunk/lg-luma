//! The Criterion Channel screen: the Channel's own rows of films (Popular Movies, Leaving Soon,
//! its featured collections), browsed with the remote like any other Luma screen. Choosing a
//! film opens it in Luma's Criterion player (`criterion/`), a small web app that loads the
//! Channel's web player full screen and keeps the viewer's own sign-in.
//!
//! The Channel has no public API. Its home page is public and server-rendered, so a worker reads
//! the rows from its HTML and keeps the last good copy on disk. The page's class names are
//! generated and change between deployments; parsing relies only on stable parts: each row's
//! `<h2>` heading, the `/films/<id>/<slug>` card links and the card titles' element IDs.
//! Artwork comes from the Channel's video host by film ID.
use crate::cameras::Frame;
use crate::search_cache::DiskCache;
use crate::state::{Direction, Effect, Overlay, State, Target};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::Read,
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
const FRESH: Duration = Duration::from_secs(6 * 3600);
/// After that, it still fills the screen while a new copy loads, or when the Channel is away.
const STALE: Duration = Duration::from_secs(30 * 24 * 3600);
const CACHE_KEY: &str = "criterion:home";
const ROW_LIMIT: usize = 40;
const FILM_LIMIT: usize = 100;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Film {
    /// The Channel's 8-character film ID, which is also its artwork's ID.
    pub id: String,
    pub slug: String,
    pub title: String,
    /// Year and running time, as the Channel shows them under the card.
    pub year: String,
    pub length: String,
}

impl Film {
    /// The film's page; the player starts it (see `deeplink`).
    pub fn link(&self) -> String {
        format!("{ORIGIN}/films/{}/{}", self.id, self.slug)
    }

    pub fn image_url(&self) -> String {
        format!(
            "https://cdn.jwplayer.com/v2/media/{}/images/regalia_16x9.webp?fallback=default_16x9&width=480",
            self.id
        )
    }

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
                slug: slug.into(),
                title,
                year: meta(card, "metaLeft"),
                length: meta(card, "metaRight"),
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

// ---- Catalog worker -------------------------------------------------------------------------

/// The rows, or why there are none. `stale` rows are an old copy shown while a new one loads.
#[derive(Debug)]
pub struct Reply {
    pub rows: Result<Vec<Row>, String>,
    pub stale: bool,
}

pub struct Service {
    sender: mpsc::Sender<()>,
    pub receiver: mpsc::Receiver<Reply>,
}

impl Service {
    pub fn new(cache: DiskCache) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .max_redirects(0)
            .proxy(None)
            .http_status_as_error(false)
            .build()
            .new_agent();
        Self::with_fetch(cache, move || fetch(&agent))
    }

    fn with_fetch(
        cache: DiskCache,
        fetch: impl Fn() -> Result<String, String> + Send + 'static,
    ) -> Self {
        let (sender, requests) = mpsc::channel::<()>();
        let (replies, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            while requests.recv().is_ok() {
                // Requests made while one was loading are answered by it.
                while requests.try_recv().is_ok() {}
                for reply in load(&cache, &fetch, SystemTime::now()) {
                    if replies.send(reply).is_err() {
                        return;
                    }
                }
            }
        });
        Self { sender, receiver }
    }

    pub fn load(&self) {
        let _ = self.sender.send(());
    }
}

/// A fresh copy from disk; else any copy from disk at once, then the page from the network.
fn load(
    cache: &DiskCache,
    fetch: &dyn Fn() -> Result<String, String>,
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
    let rows = fetch().and_then(|html| {
        let rows = parse(&html);
        if rows.is_empty() {
            Err("The Criterion Channel page had no films".to_owned())
        } else {
            Ok(rows)
        }
    });
    match rows {
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
    /// Artwork by film ID: None while loading or when unavailable.
    pub images: HashMap<String, Option<Arc<Frame>>>,
    /// Films whose artwork was asked for since the screen opened.
    image_requests_sent: bool,
}

impl Browser {
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
        browser.image_requests_sent = false;
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
                    browser.images.retain(|id, _| {
                        rows.iter()
                            .any(|row| row.films.iter().any(|film| &film.id == id))
                    });
                    browser.rows = rows;
                    browser.first_card = vec![0; browser.rows.len()];
                    browser.first_row = 0;
                    browser.image_requests_sent = false;
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
            Target::Film(row, card) => match self.criterion.film(row, card) {
                Some(film) => vec![Effect::LaunchTitle {
                    app: PLAYER.into(),
                    service: SERVICE.into(),
                    link: film.link(),
                }],
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

    pub fn criterion_back(&mut self) -> Vec<Effect> {
        self.close_criterion();
        vec![]
    }

    /// Whether to ask the catalog worker for rows now.
    pub fn criterion_load_requested(&mut self) -> bool {
        std::mem::take(&mut self.criterion.requested)
    }

    /// Artwork to fetch for the open screen, once per film, or None when nothing changed.
    pub fn criterion_image_requests(&mut self) -> Option<Vec<crate::posters::Request>> {
        let open = self.criterion_open();
        let browser = &mut self.criterion;
        if !open || browser.image_requests_sent || browser.rows.is_empty() {
            return None;
        }
        browser.image_requests_sent = true;
        let mut requests = Vec::new();
        // Top rows first: they are on screen.
        for film in browser.rows.iter().flat_map(|row| &row.films) {
            if browser.images.contains_key(&film.id) {
                continue;
            }
            browser.images.insert(film.id.clone(), None);
            requests.push(crate::posters::Request {
                show_id: image_key(&film.id),
                url: film.image_url(),
                size: IMAGE_SIZE,
            });
        }
        Some(requests)
    }

    pub fn accept_criterion_image(&mut self, key: &str, image: Option<Arc<Frame>>) {
        let Some(id) = key.strip_prefix(IMAGE_PREFIX) else {
            return;
        };
        if let Some(slot) = self.criterion.images.get_mut(id) {
            *slot = image;
            self.dirty = true;
        }
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
