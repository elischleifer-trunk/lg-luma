//! Title search backed by Movie of the Night (Streaming Availability API v4): reply parsing,
//! ranking against the TV's installed apps, and incremental-search timing. Pure logic: no
//! network, files or SDL, so it is tested against captured fixtures.
//!
//! Ranking rules:
//! 1. Only services whose app is installed on this TV count as "ours". Shows we can watch on
//!    one of them come first; shows with no option on an installed app come last.
//! 2. When several of our services carry a show, the most recently used app wins (system recent
//!    history first, then favorites order, then any other installed app).
//! 3. Included access (subscription or free) beats paying more (rent, buy), even on a less
//!    recently used app. Rentals and purchases are still offered when nothing is included.
//! 4. Add-on channels (a paid channel inside another app: Shout! TV on Prime Video) are not a
//!    way to watch: the household has none. A service sold that way that it does have (Hulu
//!    inside Disney+) is listed under its own app too. A title only on add-on channels is on
//!    none of the TV's apps. To support one later, let the owner say which channels they have.
use crate::branding;
pub use crate::motn_api::{Show, StreamingOptionType};
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

/// Shorter queries are not sent: they match too much and waste quota.
pub const MIN_QUERY_CHARS: usize = 3;
/// Quiet time after the last keystroke before a request is sent.
pub const DEBOUNCE: Duration = Duration::from_millis(350);
/// Distinct queries whose replies are kept for instant redisplay (e.g. after Backspace).
const CACHE_QUERIES: usize = 32;
/// How long a remembered reply is shown again without asking: no longer than the disk cache
/// keeps where a show streams (`search_cache::ttl::STREAMING`), as Luma runs for days.
pub const CACHE_AGE: Duration = Duration::from_secs(12 * 3600);

/// Movie of the Night service IDs and the curated brand whose TV app plays them.
const SERVICE_BRANDS: &[(&str, &str)] = &[
    ("netflix", "netflix"),
    ("prime", "prime-video"),
    ("disney", "disney-plus"),
    ("hbo", "hbo-max"),
    ("hulu", "hulu"),
    ("peacock", "peacock"),
    ("paramount", "paramount-plus"),
    ("apple", "apple-tv"),
    ("crunchyroll", "crunchyroll"),
    ("now", "now-tv"),
    ("iplayer", "bbc-iplayer"),
    ("itvx", "itvx"),
    ("all4", "channel-4"),
    ("plex", "plex"),
];

/// Release year for movies, first air year for series.
pub fn year(show: &Show) -> Option<i64> {
    show.release_year.or(show.first_air_year)
}

/// What watching costs, cheapest first: the derived order is the preference order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    /// Covered by a subscription or free with ads: no extra money.
    Included,
    Rent,
    Buy,
}

fn access(kind: StreamingOptionType) -> Option<Access> {
    match kind {
        StreamingOptionType::Subscription | StreamingOptionType::Free => Some(Access::Included),
        StreamingOptionType::Rent => Some(Access::Rent),
        StreamingOptionType::Buy => Some(Access::Buy),
        // An add-on channel is another subscription to buy: see rule 4.
        StreamingOptionType::Addon | StreamingOptionType::Unknown => None,
    }
}

/// Which Movie of the Night services this TV can play, and how recently each app was used.
#[derive(Clone, Debug, Default)]
pub struct Services {
    /// Service ID → (installed app ID, recency rank; lower is more recent).
    apps: HashMap<&'static str, (String, usize)>,
}

impl Services {
    /// `installed`: every app on the TV. `recent`: system recent history, most recent first.
    /// `favorites`: Home favorites, which Luma also keeps most-recently-launched first.
    pub fn new(installed: &[String], recent: &[String], favorites: &[String]) -> Self {
        let rank = |id: &str| {
            recent
                .iter()
                .position(|r| r == id)
                .or_else(|| {
                    favorites
                        .iter()
                        .position(|f| f == id)
                        .map(|p| recent.len() + p)
                })
                .unwrap_or(usize::MAX)
        };
        let mut apps = HashMap::new();
        for &(service, brand) in SERVICE_BRANDS {
            let Some(brand) = branding::for_key(brand) else {
                continue;
            };
            // A brand can have several app IDs; use the installed one used most recently.
            if let Some(app) = installed
                .iter()
                .filter(|id| brand.ids.contains(&id.as_str()))
                .min_by_key(|id| rank(id))
            {
                apps.insert(service, (app.clone(), rank(app)));
            }
        }
        Self { apps }
    }

    fn app(&self, service: &str) -> Option<&(String, usize)> {
        self.apps.get(service)
    }
}

/// The option a result card shows and launches.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    pub app_id: String,
    pub service_id: String,
    pub access: Access,
    pub link: String,
    pub price: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ranked {
    pub show: Show,
    /// None when no installed app carries the show in this country.
    pub pick: Option<Pick>,
    /// Other installed apps where it is included, most recent first, excluding `pick`.
    pub also_included: Vec<String>,
    /// The US rating (PG-13, TV-14) when TMDB details were fetched for the show.
    pub rating: Option<String>,
}

/// Picks the best installed option for one show.
pub fn pick(show: &Show, country: &str, services: &Services) -> (Option<Pick>, Vec<String>) {
    let mut candidates: Vec<_> = show
        .streaming_options
        .get(country)
        .into_iter()
        .flatten()
        .filter_map(|option| {
            let access = access(option.r#type)?;
            let (app, rank) = services.app(&option.service.id)?;
            let cents = option.price.as_ref().map_or(0, |p| price_cents(&p.amount));
            Some((access, *rank, cents, app, option))
        })
        .collect();
    candidates.sort_by_key(|c| (c.0, c.1, c.2));
    let Some(&(access, _, _, app, option)) = candidates.first() else {
        return (None, Vec::new());
    };
    let mut also = Vec::new();
    for (other_access, _, _, other_app, _) in &candidates[1..] {
        if *other_access == Access::Included && *other_app != app && !also.contains(*other_app) {
            also.push((*other_app).clone());
        }
    }
    let pick = Pick {
        app_id: app.clone(),
        service_id: option.service.id.clone(),
        access,
        link: option.link.clone(),
        price: option.price.as_ref().map(|p| p.formatted.clone()),
    };
    (Some(pick), also)
}

/// Orders API results: included on our apps, then paid on our apps, then not on our apps.
/// Within each group the API's relevance order is kept.
pub fn rank(shows: &[Show], country: &str, services: &Services) -> Vec<Ranked> {
    let mut ranked: Vec<_> = shows
        .iter()
        .map(|show| {
            let (pick, also_included) = pick(show, country, services);
            Ranked {
                show: show.clone(),
                pick,
                also_included,
                rating: None,
            }
        })
        .collect();
    ranked.sort_by_key(|r| match &r.pick {
        Some(p) if p.access == Access::Included => 0,
        Some(_) => 1,
        None => 2,
    });
    ranked
}

fn price_cents(amount: &str) -> u64 {
    let (whole, fraction) = amount.split_once('.').unwrap_or((amount, "0"));
    let fraction: String = fraction.chars().chain("00".chars()).take(2).collect();
    whole.parse::<u64>().unwrap_or(u64::MAX / 200) * 100 + fraction.parse::<u64>().unwrap_or(0)
}

/// Parses a `/shows/search/title` or `/shows/search/filters` reply.
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse_search(reply: &serde_json::Value) -> Result<Vec<Show>, String> {
    // Title search returns an array; filter search wraps it in {"shows": [...], "hasMore", ...}.
    let shows = reply.get("shows").unwrap_or(reply);
    let list = shows.as_array().ok_or("Invalid search reply")?;
    Ok(list
        .iter()
        .filter_map(|show| serde_json::from_value(show.clone()).ok())
        .collect())
}

/// Lowercased, trimmed, single-spaced: the cache key and the text actually sent.
pub fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub generation: u64,
    pub query: String,
}

/// A search result with a title, which the provisional prefix filter matches against.
pub trait Titled: Clone {
    fn title(&self) -> &str;
}

impl Titled for Show {
    fn title(&self) -> &str {
        &self.title
    }
}

impl Titled for Ranked {
    fn title(&self) -> &str {
        &self.show.title
    }
}

/// Search-as-you-type: waits for a pause in typing, never sends short queries, answers repeats
/// from a small cache and drops replies for text the user has since changed.
#[derive(Debug)]
pub struct Incremental<T = Show> {
    query: String,
    changed_at: Option<Instant>,
    generation: u64,
    /// Query, when its reply arrived, and the reply.
    cache: VecDeque<(String, Instant, Vec<T>)>,
    pub results: Vec<T>,
    /// The results are a local filter of an earlier, shorter query while the real reply loads.
    pub provisional: bool,
    pub searching: bool,
    pub error: Option<String>,
}

impl<T> Default for Incremental<T> {
    fn default() -> Self {
        Self {
            query: String::new(),
            changed_at: None,
            generation: 0,
            cache: VecDeque::new(),
            results: Vec::new(),
            provisional: false,
            searching: false,
            error: None,
        }
    }
}

impl<T: Titled> Incremental<T> {
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Call on every edit of the text box.
    pub fn set_text(&mut self, text: &str, now: Instant) {
        let query = normalize(text);
        if query == self.query {
            return;
        }
        self.query = query;
        self.error = None;
        // Any reply still in flight is now for old text.
        self.generation += 1;
        self.searching = false;
        if self.query.chars().count() < MIN_QUERY_CHARS {
            self.changed_at = None;
            self.results.clear();
            self.provisional = false;
            return;
        }
        self.cache
            .retain(|(_, at, _)| now.saturating_duration_since(*at) <= CACHE_AGE);
        if let Some(cached) = self.cached(&self.query) {
            self.results = cached.to_vec();
            self.provisional = false;
            self.changed_at = None;
            return;
        }
        self.changed_at = Some(now);
        // Narrow the longest cached prefix locally so typing feels instant.
        let prefix = self
            .cache
            .iter()
            .filter(|(q, _, _)| self.query.starts_with(q.as_str()))
            .max_by_key(|(q, _, _)| q.len());
        if let Some((_, _, shows)) = prefix {
            let query = &self.query;
            self.results = shows
                .iter()
                .filter(|s| s.title().to_lowercase().contains(query.as_str()))
                .cloned()
                .collect();
            self.provisional = true;
        } else {
            // Results for other text must not pass for this text's while it loads.
            self.results.clear();
            self.provisional = false;
        }
    }

    /// Forgets remembered replies, whose picks were made for the TV's apps at the time, and
    /// searches the current text again. Call when the TV's apps change.
    pub fn refresh(&mut self, now: Instant) {
        self.cache.clear();
        if self.query.chars().count() >= MIN_QUERY_CHARS {
            self.generation += 1;
            self.searching = false;
            self.changed_at = Some(now);
        }
    }

    /// When the event loop should next call `due`, if a search is waiting on the debounce.
    pub fn deadline(&self) -> Option<Instant> {
        self.changed_at.map(|at| at + DEBOUNCE)
    }

    /// The request to send now, once typing has paused long enough.
    pub fn due(&mut self, now: Instant) -> Option<Request> {
        if now < self.deadline()? {
            return None;
        }
        self.changed_at = None;
        self.searching = true;
        Some(Request {
            generation: self.generation,
            query: self.query.clone(),
        })
    }

    /// Accepts a worker reply. Returns whether the visible results changed.
    pub fn accept(&mut self, request: &Request, reply: Result<Vec<T>, String>) -> bool {
        self.accept_at(request, reply, Instant::now())
    }

    /// `accept` with the time the reply arrived, which starts its age in the cache.
    pub fn accept_at(
        &mut self,
        request: &Request,
        reply: Result<Vec<T>, String>,
        now: Instant,
    ) -> bool {
        let current = request.generation == self.generation;
        match reply {
            Ok(shows) => {
                // Cache even stale replies: the user may come back to that text.
                self.cache.retain(|(q, _, _)| *q != request.query);
                if self.cache.len() == CACHE_QUERIES {
                    self.cache.pop_front();
                }
                self.cache
                    .push_back((request.query.clone(), now, shows.clone()));
                if !current {
                    return false;
                }
                self.results = shows;
                self.provisional = false;
                self.error = None;
            }
            Err(error) => {
                if !current {
                    return false;
                }
                self.error = Some(error);
            }
        }
        self.searching = false;
        true
    }

    fn cached(&self, query: &str) -> Option<&[T]> {
        self.cache
            .iter()
            .find(|(q, _, _)| q == query)
            .map(|(_, _, shows)| shows.as_slice())
    }
}

#[cfg(test)]
#[path = "streaming-tests.rs"]
mod tests;
