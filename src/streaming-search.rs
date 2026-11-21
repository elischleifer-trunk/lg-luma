//! One streaming search, end to end: Movie of the Night for what streams where, TMDB for how
//! prominent each title is, then [`relevance::rank`]. Runs on a worker thread, never the SDL
//! thread. Every reply goes through the disk cache, so a repeated search costs nothing.
//!
//! API calls for a new query:
//! 1. TMDB `search/multi` (free). If the text ends mid-word, it is completed to the word the
//!    best-known matching titles use ([`relevance::complete`]) and the search continues as if
//!    that had been typed: "ghostb", "ghostbu" … are all the one search for "ghostbusters".
//!    If it ends in a whole word of some titles, TMDB lists only those, so its keyword search
//!    suggests longer words, each with its own free search; one far better known is searched
//!    as well ([`relevance::longer_word`]): "kin" is also "king".
//! 2. Movie of the Night title search (metered: one call, two with a longer word). The one
//!    for the text starts at once, beside the longer-word checks.
//! 3. TMDB details for results the search summary did not already rule out (foreign or
//!    little-seen titles are skipped), a few at a time over connections that stay open. TMDB
//!    has no batch endpoint; details are cached for a month, so each title costs one call.
//! 4. When a title search came back full, the best-known titles TMDB found that it left out:
//!    "king" returns twenty films called *King* and not *King Kong* (metered, so capped).
//! 5. For a franchise the query names or has begun to name ("gho" → *Ghostbusters*), the films
//!    the title search missed: the TMDB collection and, for half-typed text, TMDB's free
//!    search for the franchise's title, then Movie of the Night for each missing film
//!    (metered, so capped).
//!
//! A search the user has typed past is dropped between steps, before it spends more calls.
use crate::motn_api::{self, GetShowParams, SearchShowsByTitleParams, Show};
use crate::openapi::{CONNECTIONS, Error, Http};
use crate::relevance::{self, Enrichment, Kind, Signals, match_tier, tmdb_id};
use crate::search_cache::{DiskCache, ttl};
use crate::streaming::{self, Ranked, Request, Services};
use crate::streaming_keys::{self, Keys};
use crate::tmdb_api::{
    self, CollectionDetailsParams, CollectionDetailsResponse, MovieDetailsParams,
    MovieDetailsResponse, SearchKeywordParams, SearchMultiParams, SearchMultiResponseResult,
    TvSeriesDetailsParams, TvSeriesDetailsResponse,
};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Instant, SystemTime},
};

/// Movie of the Night title search returns at most this many shows.
pub const SEARCH_PAGE: usize = 20;
/// Details for every show a title search returns.
pub const DETAILS_LIMIT: usize = SEARCH_PAGE;
/// Well-known titles a full title search left out, fetched from Movie of the Night per search
/// at one metered call each.
pub const MISSED_LIMIT: usize = 4;
/// Franchise films fetched from Movie of the Night per search, at one metered call each.
pub const EXPANSION_LIMIT: usize = 4;
/// Franchise films fetched at once. A request on an open connection takes a tenth of the time
/// of opening another, so two connections beat four.
const EXPANSION_CONNECTIONS: usize = 2;
const LANGUAGE: &str = "en-US";
/// Ranking is tuned for US commercial work, so searches ask about US availability.
pub const COUNTRY: &str = "us";

/// Where search data comes from: the live APIs behind the disk cache, or fixtures in tests.
pub trait Source: Sync {
    fn search(&self, query: &str, country: &str) -> Result<Vec<Show>, Error>;
    /// One show by TMDB ID (`movie/2978`), for franchise films the title search missed.
    fn show(&self, id: &str, country: &str) -> Result<Show, Error>;
    /// TMDB keywords like the text, for longer words it may be the start of. None without a
    /// TMDB token.
    fn tmdb_keywords(&self, query: &str) -> Option<Result<Vec<String>, Error>>;
    /// None without a TMDB token: results then keep Movie of the Night's order.
    fn tmdb_search(&self, query: &str) -> Option<Result<Vec<SearchMultiResponseResult>, Error>>;
    fn movie(&self, id: i64) -> Result<MovieDetailsResponse, Error>;
    fn series(&self, id: i64) -> Result<TvSeriesDetailsResponse, Error>;
    fn collection(&self, id: i64) -> Result<CollectionDetailsResponse, Error>;
}

/// Ranked results for `query`. Fails only when Movie of the Night does; a TMDB failure falls
/// back to ranking by where shows stream.
// The worker calls `search_while`; tests have no newer search to give way to.
#[cfg_attr(not(test), allow(dead_code))]
pub fn search(
    source: &impl Source,
    query: &str,
    country: &str,
    services: &Services,
) -> Result<Vec<Ranked>, Error> {
    search_while(source, query, country, services, &|| true).unwrap_or_else(|| Ok(Vec::new()))
}

/// [`search`] that gives up once `wanted` says the user has typed something else: None, with
/// every reply fetched so far kept in the cache.
pub fn search_while(
    source: &impl Source,
    query: &str,
    country: &str,
    services: &Services,
    wanted: &dyn Fn() -> bool,
) -> Option<Result<Vec<Ranked>, Error>> {
    // TMDB first: it is free and answers in a fraction of the time, and it says whether the
    // text is worth a metered search as typed.
    let mut tmdb = source.tmdb_search(query);
    let mut query = query.to_owned();
    let mut completed = false;
    if let Some(Ok(results)) = &tmdb
        && let Some(text) = relevance::complete(&query, results)
    {
        tmdb = source.tmdb_search(&text);
        query = text;
        completed = true;
    }
    let query = query.as_str();
    if !wanted() {
        return None;
    }
    // The title search for the text runs whatever the longer-word checks decide, so it starts
    // at once and they run beside it; a longer word's search starts as soon as it is chosen.
    let (first, longer, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| source.search(query, country));
        let longer = match &tmdb {
            Some(Ok(results)) if !completed => longer_word(source, query, results, wanted),
            _ => None,
        };
        let second = longer
            .as_ref()
            .filter(|_| wanted())
            .map(|(text, _)| source.search(text, country));
        (first.join(), longer, second)
    });
    let mut shows = match first {
        Ok(Ok(shows)) => shows,
        Ok(Err(error)) => return Some(Err(error)),
        Err(_) => return Some(Err(Error::Network("search failed".into()))),
    };
    if !wanted() {
        return None;
    }
    // Each text searched, and whether Movie of the Night's reply was full.
    let mut searched = vec![(query.to_owned(), shows.len() >= SEARCH_PAGE)];
    let longer = match (longer, second) {
        (Some((text, results)), Some(Ok(more))) => {
            searched.push((text, more.len() >= SEARCH_PAGE));
            shows.extend(more);
            Some(results)
        }
        (Some((text, _)), Some(Err(error))) => {
            crate::telemetry::log_warn!(
                "streaming",
                "search: longer word \"{text}\" failed: {error}"
            );
            None
        }
        _ => None,
    };
    let mut seen = HashSet::new();
    shows.retain(|show| seen.insert(show.tmdb_id.clone()));
    let results = match tmdb {
        Some(Ok(results)) => results,
        Some(Err(error)) => {
            crate::telemetry::log_warn!(
                "streaming",
                "search: TMDB unavailable, ranking without it: {error}"
            );
            return Some(Ok(streaming::rank(&shows, country, services)));
        }
        None => return Some(Ok(streaming::rank(&shows, country, services))),
    };
    let mut enrichment = Enrichment::default();
    enrichment.add_search(&results);
    let mut tmdb_results = vec![results];
    if let Some(more) = longer {
        enrichment.add_search(&more);
        tmdb_results.push(more);
    }

    let wanted_details: Vec<(Kind, i64)> = shows
        .iter()
        .filter_map(|show| {
            let id = tmdb_id(show)?;
            let named = match_tier(query, &show.title) == 0;
            // The free search summary already rules out foreign and little-seen titles.
            match enrichment.signals.get(&id) {
                Some(summary) if !summary.keep(id.0, named) => None,
                _ => Some(id),
            }
        })
        .take(DETAILS_LIMIT * searched.len())
        .collect();
    if !wanted() {
        return None;
    }
    fetch_details(source, &mut enrichment, &wanted_details);

    if !wanted() {
        return None;
    }
    let searched: Vec<(&str, bool, &[SearchMultiResponseResult])> = searched
        .iter()
        .zip(&tmdb_results)
        .map(|((text, full), results)| (text.as_str(), *full, results.as_slice()))
        .collect();
    // Titles already asked of Movie of the Night one by one, found or not.
    let mut tried = HashSet::new();
    look_up_missed(
        source,
        country,
        &searched,
        &mut shows,
        &mut enrichment,
        &mut tried,
        wanted,
    );
    if !wanted() {
        return None;
    }
    expand_franchises(
        source,
        query,
        country,
        &mut shows,
        &mut enrichment,
        &tried,
        wanted,
    );
    Some(Ok(relevance::rank(
        query,
        &shows,
        country,
        services,
        Some(&enrichment),
    )))
}

/// The text with its last word lengthened, and that text's TMDB results, when the word typed
/// names some titles but a longer one is far better known ([`relevance::longer_word`]):
/// "kin" is also searched as "king". Free: TMDB's keyword search, then a TMDB search for each
/// longer word it suggests.
fn longer_word(
    source: &impl Source,
    query: &str,
    results: &[SearchMultiResponseResult],
    wanted: &dyn Fn() -> bool,
) -> Option<(String, Vec<SearchMultiResponseResult>)> {
    if relevance::word_votes(query, results) == 0 {
        return None;
    }
    let keywords = source.tmdb_keywords(&relevance::last_word(query)?)?.ok()?;
    let texts = relevance::longer_words(query, &keywords);
    if texts.is_empty() || !wanted() {
        return None;
    }
    let candidates: Vec<(String, Vec<SearchMultiResponseResult>)> =
        parallel(&texts, CONNECTIONS, |text| source.tmdb_search(text))
            .into_iter()
            .zip(&texts)
            .filter_map(|(reply, text)| Some((text.clone(), reply??.ok()?)))
            .collect();
    relevance::longer_word(query, results, &candidates).cloned()
}

/// Adds the best-known titles TMDB found for a searched text that Movie of the Night's reply
/// left out. It returns at most [`SEARCH_PAGE`] shows, and fills them with every exact match
/// first: "king" brings back a dozen films called *King* and not *King Kong*. Only a full reply
/// can have left a title out. The most seen first, one metered call each, at most
/// [`MISSED_LIMIT`]; TMDB's summary has already ruled out foreign and little-seen titles, so
/// no call is spent on them.
fn look_up_missed(
    source: &impl Source,
    country: &str,
    searched: &[(&str, bool, &[SearchMultiResponseResult])],
    shows: &mut Vec<Show>,
    enrichment: &mut Enrichment,
    tried: &mut HashSet<String>,
    wanted: &dyn Fn() -> bool,
) {
    let present: HashSet<String> = shows.iter().map(|show| show.tmdb_id.clone()).collect();
    // (how it matches the query, votes, TMDB identity)
    let mut missed: Vec<(i64, (Kind, i64), String)> = Vec::new();
    for (text, _, results) in searched.iter().filter(|(_, full, _)| *full) {
        for result in *results {
            let (kind, prefix) = match result.media_type.as_deref() {
                Some("movie") => (Kind::Movie, "movie"),
                Some("tv") => (Kind::Series, "tv"),
                _ => continue,
            };
            let (Some(id), Some(title)) = (
                result.id,
                result.title.as_deref().or(result.name.as_deref()),
            ) else {
                continue;
            };
            let signals = Signals::from_search(result);
            let key = format!("{prefix}/{id}");
            if present.contains(&key)
                || missed.iter().any(|(_, _, known)| *known == key)
                || match_tier(text, title) > 2
                || !signals.keep(kind, false)
            {
                continue;
            }
            missed.push((signals.votes, (kind, id), key));
        }
    }
    missed.sort_by_key(|(votes, _, _)| std::cmp::Reverse(*votes));
    missed.truncate(MISSED_LIMIT);
    // The metered step: not for a search nobody is waiting for.
    if missed.is_empty() || !wanted() {
        return;
    }
    tried.extend(missed.iter().map(|(_, _, key)| key.clone()));
    let found = parallel(&missed, EXPANSION_CONNECTIONS, |(_, (kind, id), key)| {
        let (kind, id) = (*kind, *id);
        let show = source.show(key, country).ok()?;
        let signals = match kind {
            Kind::Movie => source.movie(id).ok().map(|d| Signals::movie(&d)),
            Kind::Series => source.series(id).ok().map(|d| Signals::series(&d)),
        };
        Some((show, (kind, id), signals))
    });
    for (show, id, signals) in found.into_iter().flatten().flatten() {
        if let Some(signals) = signals {
            enrichment.signals.insert(id, signals);
        }
        shows.push(show);
    }
}

/// `work` for each item on at most `limit` threads, in the items' order. Each thread keeps
/// one connection busy, so `limit` is also how many connections a step opens. A panic in
/// `work` leaves None.
fn parallel<T: Sync, R: Send>(
    items: &[T],
    limit: usize,
    work: impl Fn(&T) -> R + Sync,
) -> Vec<Option<R>> {
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<R>>> = items.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..limit.min(items.len()))
            .map(|_| {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(index) else { break };
                        let result = work(item);
                        if let Ok(mut slot) = results[index].lock() {
                            *slot = Some(result);
                        }
                    }
                })
            })
            .collect();
        for worker in workers {
            let _ = worker.join();
        }
    });
    results
        .into_iter()
        .map(|slot| slot.into_inner().ok().flatten())
        .collect()
}

/// Details a few at a time; a failed request leaves that title with its search summary.
fn fetch_details(source: &impl Source, enrichment: &mut Enrichment, ids: &[(Kind, i64)]) {
    let fetched = parallel(ids, CONNECTIONS, |&(kind, id)| match kind {
        Kind::Movie => source.movie(id).ok().map(|d| Signals::movie(&d)),
        Kind::Series => source.series(id).ok().map(|d| Signals::series(&d)),
    });
    for (id, signals) in ids.iter().zip(fetched) {
        if let Some(signals) = signals.flatten() {
            enrichment.signals.insert(*id, signals);
        }
    }
}

/// Adds films from the franchises the query names, or has begun to name, that the title
/// search did not return. Movie of the Night matches whole words, so "gho" brings back
/// *Ghostbusters* and one sequel but not the rest. A franchise is a film whose title starts
/// with the text and that belongs to a TMDB collection; its other films are the collection's
/// parts and, for half-typed text, films TMDB lists under the same base title (the 2016
/// *Ghostbusters* is in no collection).
fn expand_franchises(
    source: &impl Source,
    query: &str,
    country: &str,
    shows: &mut Vec<Show>,
    enrichment: &mut Enrichment,
    tried: &HashSet<String>,
    wanted: &dyn Fn() -> bool,
) {
    let present: HashSet<String> = shows
        .iter()
        .map(|show| show.tmdb_id.clone())
        .chain(tried.iter().cloned())
        .collect();
    // (how the lead matches, its collection, its base title), named franchises first.
    let mut franchises: Vec<(u8, i64, String)> = Vec::new();
    for show in shows.iter() {
        let Some(id @ (Kind::Movie, _)) = tmdb_id(show) else {
            continue;
        };
        let tier = match_tier(query, &show.title);
        if let Some(signals) = enrichment.signals.get(&id)
            && tier <= 1
            && signals.keep(Kind::Movie, tier == 0)
            && let Some(collection) = signals.collection
            && !franchises.iter().any(|(_, known, _)| *known == collection)
        {
            franchises.push((tier, collection, relevance::base_title(&show.title)));
        }
    }
    franchises.sort_by_key(|(tier, _, _)| *tier);
    let mut missing = Vec::new();
    let collections = parallel(&franchises, CONNECTIONS, |(_, id, _)| {
        source.collection(*id).ok()
    });
    for details in collections.into_iter().flatten().flatten() {
        for part in details.parts.iter().flatten() {
            let Some(id) = part.id else { continue };
            let key = format!("movie/{id}");
            // Only films released and seen enough to be kept anyway; skips announced sequels.
            if !present.contains(&key)
                && !missing.contains(&id)
                && part.vote_count.unwrap_or(0) >= relevance::MIN_MOVIE_VOTES
            {
                missing.push(id);
            }
        }
    }
    // Typed in full, the title search itself returns the films that share the title.
    for (_, _, base) in franchises.iter().filter(|(tier, _, _)| *tier == 1) {
        let Some(Ok(results)) = source.tmdb_search(base) else {
            continue;
        };
        for result in &results {
            let (Some("movie"), Some(id), Some(title)) =
                (result.media_type.as_deref(), result.id, &result.title)
            else {
                continue;
            };
            if relevance::base_title(title) == *base
                && Signals::from_search(result).keep(Kind::Movie, false)
                && !present.contains(&format!("movie/{id}"))
                && !missing.contains(&id)
            {
                missing.push(id);
            }
        }
    }
    missing.truncate(EXPANSION_LIMIT);
    // The metered step: not for a search nobody is waiting for.
    if missing.is_empty() || !wanted() {
        return;
    }
    let found = parallel(&missing, EXPANSION_CONNECTIONS, |&id| {
        let show = source.show(&format!("movie/{id}"), country).ok()?;
        Some((show, source.movie(id).ok()))
    });
    for (show, details) in found.into_iter().flatten().flatten() {
        if let Some(details) = details {
            enrichment.add_movie(&details);
        }
        shows.push(show);
    }
}

/// The live APIs, each reply cached on disk for as long as it stays useful.
pub struct Live {
    motn: motn_api::Client,
    tmdb: Option<tmdb_api::Client>,
    cache: DiskCache,
    /// The origins the clients call, for [`Live::warm`].
    origins: Vec<Http>,
}

impl Live {
    /// None until a Movie of the Night key is stored; TMDB is optional. Keep it between
    /// searches: it holds the open connections.
    pub fn new(keys: &Keys, cache: DiskCache) -> Option<Self> {
        let motn = keys.motn_http()?;
        let tmdb = keys.tmdb_http();
        Some(Self {
            motn: motn_api::Client::new(motn.clone()),
            tmdb: tmdb.clone().map(tmdb_api::Client::new),
            cache,
            origins: std::iter::once(motn).chain(tmdb).collect(),
        })
    }

    /// Connects to each API ahead of the first search (see [`Http::warm`]).
    pub fn warm(&self) {
        parallel(&self.origins, self.origins.len(), Http::warm);
    }
}

/// [`cached`] that also remembers, for `max_age`, that the API has no such item (HTTP 404):
/// asking again would spend a metered call on the same answer. Other errors (network, rate
/// limits) are not remembered, so the next search retries them.
pub fn cached_or_missing<T: Serialize + DeserializeOwned>(
    cache: &DiskCache,
    key: &str,
    max_age: std::time::Duration,
    fetch: impl FnOnce() -> Result<T, Error>,
) -> Result<T, Error> {
    let missing = format!("{key}:missing");
    if cache
        .get::<bool>(&missing, max_age, SystemTime::now())
        .is_some()
    {
        return Err(Error::Status {
            status: 404,
            message: "not found (remembered)".into(),
        });
    }
    let reply = cached(cache, key, max_age, fetch);
    if let Err(Error::Status { status: 404, .. }) = &reply {
        cache.put(&missing, &true, SystemTime::now());
    }
    reply
}

/// A cached reply, or a fresh one that is then cached. Errors are never cached.
pub fn cached<T: Serialize + DeserializeOwned>(
    cache: &DiskCache,
    key: &str,
    max_age: std::time::Duration,
    fetch: impl FnOnce() -> Result<T, Error>,
) -> Result<T, Error> {
    let now = SystemTime::now();
    if let Some(value) = cache.get(key, max_age, now) {
        return Ok(value);
    }
    let value = fetch()?;
    cache.put(key, &value, now);
    Ok(value)
}

impl Source for Live {
    fn search(&self, query: &str, country: &str) -> Result<Vec<Show>, Error> {
        cached(
            &self.cache,
            &format!("motn:search:{country}:{query}"),
            ttl::STREAMING,
            || {
                self.motn.search_shows_by_title(&SearchShowsByTitleParams {
                    country: country.into(),
                    title: query.into(),
                    series_granularity: Some(motn_api::GetGranularity::Show),
                    output_language: Some(motn_api::OutputLanguage::En),
                    ..Default::default()
                })
            },
        )
    }

    fn show(&self, id: &str, country: &str) -> Result<Show, Error> {
        // Titles are looked up by the TMDB ID that TMDB's search gave, and Movie of the Night
        // does not have every one; each repeat of the search would ask again.
        cached_or_missing(
            &self.cache,
            &format!("motn:show:{country}:{id}"),
            ttl::STREAMING,
            || {
                self.motn.get_show(&GetShowParams {
                    id: id.into(),
                    country: Some(country.into()),
                    series_granularity: Some(motn_api::GetGranularity::Show),
                    output_language: Some(motn_api::OutputLanguage::En),
                })
            },
        )
    }

    fn tmdb_search(&self, query: &str) -> Option<Result<Vec<SearchMultiResponseResult>, Error>> {
        let tmdb = self.tmdb.as_ref()?;
        Some(cached(
            &self.cache,
            &format!("tmdb:search:{query}"),
            ttl::TMDB_SEARCH,
            || {
                tmdb.search_multi(&SearchMultiParams {
                    query: query.into(),
                    include_adult: Some(false),
                    language: Some(LANGUAGE.into()),
                    page: None,
                })
                .map(|reply| reply.results.unwrap_or_default())
            },
        ))
    }

    fn tmdb_keywords(&self, query: &str) -> Option<Result<Vec<String>, Error>> {
        let tmdb = self.tmdb.as_ref()?;
        Some(cached(
            &self.cache,
            &format!("tmdb:keywords:{query}"),
            ttl::TMDB_SEARCH,
            || {
                tmdb.search_keyword(&SearchKeywordParams {
                    query: query.into(),
                    page: None,
                })
                .map(|reply| {
                    reply
                        .results
                        .into_iter()
                        .flatten()
                        .filter_map(|keyword| keyword.name)
                        .collect()
                })
            },
        ))
    }

    fn movie(&self, id: i64) -> Result<MovieDetailsResponse, Error> {
        let tmdb = self.tmdb.as_ref().ok_or(Error::NotConfigured("TMDB"))?;
        cached(
            &self.cache,
            // With its US rating and alternative titles appended; details cached before
            // those were asked for are not reused.
            &format!("tmdb:movie:{id}:titled"),
            ttl::TMDB_DETAILS,
            || {
                tmdb.movie_details(&MovieDetailsParams {
                    movie_id: id,
                    append_to_response: Some("release_dates,alternative_titles".into()),
                    language: Some(LANGUAGE.into()),
                })
            },
        )
    }

    fn series(&self, id: i64) -> Result<TvSeriesDetailsResponse, Error> {
        let tmdb = self.tmdb.as_ref().ok_or(Error::NotConfigured("TMDB"))?;
        cached(
            &self.cache,
            // With its US rating and alternative titles appended; details cached before
            // those were asked for are not reused.
            &format!("tmdb:tv:{id}:titled"),
            ttl::TMDB_DETAILS,
            || {
                tmdb.tv_series_details(&TvSeriesDetailsParams {
                    series_id: id,
                    append_to_response: Some("content_ratings,alternative_titles".into()),
                    language: Some(LANGUAGE.into()),
                })
            },
        )
    }

    fn collection(&self, id: i64) -> Result<CollectionDetailsResponse, Error> {
        let tmdb = self.tmdb.as_ref().ok_or(Error::NotConfigured("TMDB"))?;
        cached(
            &self.cache,
            &format!("tmdb:collection:{id}"),
            ttl::TMDB_DETAILS,
            || {
                tmdb.collection_details(&CollectionDetailsParams {
                    collection_id: id,
                    language: Some(LANGUAGE.into()),
                })
            },
        )
    }
}

/// One search for the worker: the query and the TV's apps at the time it was typed.
pub struct Job {
    pub request: Request,
    pub services: Services,
}

enum Message {
    Search(Job),
    Warm,
}

pub type Reply = (Request, Result<Vec<Ranked>, String>);

/// The search worker: keeps network and disk off the SDL thread. Keys are read from disk for
/// each search, so keys entered on a phone or pushed by Home Assistant apply at once; the
/// connections are kept until the keys change.
pub struct Service {
    sender: mpsc::Sender<Message>,
    pub receiver: mpsc::Receiver<Reply>,
    /// The generation of the newest search sent: an older one in progress gives up.
    newest: Arc<AtomicU64>,
}

impl Service {
    pub fn new(keys: PathBuf, cache: DiskCache) -> Self {
        let (sender, jobs) = mpsc::channel::<Message>();
        let (replies, receiver) = mpsc::channel();
        let newest = Arc::new(AtomicU64::new(0));
        let latest = newest.clone();
        std::thread::spawn(move || {
            let mut live: Option<(Keys, Live)> = None;
            while let Ok(mut message) = jobs.recv() {
                // Typing outpaced the network: only the newest query is still wanted.
                while let Ok(next) = jobs.try_recv() {
                    if matches!(next, Message::Search(_)) || matches!(message, Message::Warm) {
                        message = next;
                    }
                }
                let stored = streaming_keys::load(&keys);
                if live.as_ref().is_none_or(|(keys, _)| *keys != stored) {
                    live = Live::new(&stored, cache.clone()).map(|live| (stored, live));
                }
                let live = live.as_ref().map(|(_, live)| live);
                let job = match message {
                    Message::Search(job) => job,
                    Message::Warm => {
                        if let Some(live) = live {
                            live.warm();
                        }
                        continue;
                    }
                };
                let generation = job.request.generation;
                let wanted = || latest.load(Ordering::Relaxed) == generation;
                let started = Instant::now();
                let result = match live {
                    Some(live) => {
                        match search_while(
                            live,
                            &job.request.query,
                            COUNTRY,
                            &job.services,
                            &wanted,
                        ) {
                            Some(result) => result.map_err(|error| {
                                crate::telemetry::log_warn!("streaming", "search: {error}");
                                error.to_string()
                            }),
                            // Typed past: its reply would be dropped anyway.
                            None => continue,
                        }
                    }
                    None => Err("No Movie of the Night key".into()),
                };
                if let Ok(results) = &result {
                    crate::telemetry::log_info!(
                        "streaming",
                        "search: {} results in {} ms",
                        results.len(),
                        started.elapsed().as_millis()
                    );
                }
                if replies.send((job.request, result)).is_err() {
                    break;
                }
            }
        });
        Self {
            sender,
            receiver,
            newest,
        }
    }

    pub fn send(&self, job: Job) {
        self.newest.store(job.request.generation, Ordering::Relaxed);
        let _ = self.sender.send(Message::Search(job));
    }

    /// Connects to the APIs before the first search, while the user is still typing.
    pub fn warm(&self) {
        let _ = self.sender.send(Message::Warm);
    }
}

#[cfg(test)]
#[path = "streaming-search-tests.rs"]
mod tests;
