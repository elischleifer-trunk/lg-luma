//! One streaming search, end to end: Movie of the Night for what streams where, TMDB for how
//! prominent each title is, then [`relevance::rank`]. Runs on a worker thread, never the SDL
//! thread. Every reply goes through the disk cache, so a repeated search costs nothing.
//!
//! API calls for a new query, cheapest first:
//! 1. Movie of the Night title search and TMDB `search/multi`, in parallel (one call each).
//! 2. TMDB details for results the search summary did not already rule out (foreign or
//!    little-seen titles are skipped), in parallel. TMDB has no batch endpoint; details are
//!    cached for a month, so each title costs one call.
//! 3. For a franchise the query names, the films the title search missed: the TMDB collection,
//!    then Movie of the Night for each missing film (metered, so capped).
use crate::motn_api::{self, GetShowParams, SearchShowsByTitleParams, Show};
use crate::openapi::Error;
use crate::relevance::{self, Enrichment, Kind, Signals, match_tier, tmdb_id};
use crate::search_cache::{DiskCache, ttl};
use crate::streaming::{self, Ranked, Services};
use crate::streaming_keys::Keys;
use crate::tmdb_api::{
    self, CollectionDetailsParams, CollectionDetailsResponse, MovieDetailsParams,
    MovieDetailsResponse, SearchMultiParams, SearchMultiResponseResult, TvSeriesDetailsParams,
    TvSeriesDetailsResponse,
};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::HashSet, time::SystemTime};

/// Movie of the Night title search returns at most 20 shows; details for all of them.
pub const DETAILS_LIMIT: usize = 20;
/// Franchise films fetched from Movie of the Night per search, at one metered call each.
pub const EXPANSION_LIMIT: usize = 4;
const LANGUAGE: &str = "en-US";

/// Where search data comes from: the live APIs behind the disk cache, or fixtures in tests.
pub trait Source: Sync {
    fn search(&self, query: &str, country: &str) -> Result<Vec<Show>, Error>;
    /// One show by TMDB ID (`movie/2978`), for franchise films the title search missed.
    fn show(&self, id: &str, country: &str) -> Result<Show, Error>;
    /// None without a TMDB token: results then keep Movie of the Night's order.
    fn tmdb_search(&self, query: &str) -> Option<Result<Vec<SearchMultiResponseResult>, Error>>;
    fn movie(&self, id: i64) -> Result<MovieDetailsResponse, Error>;
    fn series(&self, id: i64) -> Result<TvSeriesDetailsResponse, Error>;
    fn collection(&self, id: i64) -> Result<CollectionDetailsResponse, Error>;
}

/// Ranked results for `query`. Fails only when Movie of the Night does; a TMDB failure falls
/// back to ranking by where shows stream.
pub fn search(
    source: &impl Source,
    query: &str,
    country: &str,
    services: &Services,
) -> Result<Vec<Ranked>, Error> {
    let (shows, tmdb) = std::thread::scope(|scope| {
        let tmdb = scope.spawn(|| source.tmdb_search(query));
        let shows = source.search(query, country);
        (shows, tmdb.join().unwrap_or(None))
    });
    let mut shows = shows?;
    let results = match tmdb {
        Some(Ok(results)) => results,
        Some(Err(error)) => {
            eprintln!("Luma streaming search: TMDB unavailable, ranking without it: {error}");
            return Ok(streaming::rank(&shows, country, services));
        }
        None => return Ok(streaming::rank(&shows, country, services)),
    };
    let mut enrichment = Enrichment::default();
    enrichment.add_search(&results);

    let wanted: Vec<(Kind, i64)> = shows
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
        .take(DETAILS_LIMIT)
        .collect();
    fetch_details(source, &mut enrichment, &wanted);

    expand_franchises(source, query, country, &mut shows, &mut enrichment);
    Ok(relevance::rank(
        query,
        &shows,
        country,
        services,
        Some(&enrichment),
    ))
}

/// Details in parallel; a failed request leaves that title with its search summary.
fn fetch_details(source: &impl Source, enrichment: &mut Enrichment, ids: &[(Kind, i64)]) {
    let fetched: Vec<Option<Signals>> = std::thread::scope(|scope| {
        let jobs: Vec<_> = ids
            .iter()
            .map(|&(kind, id)| {
                scope.spawn(move || match kind {
                    Kind::Movie => source.movie(id).ok().map(|d| Signals::movie(&d)),
                    Kind::Series => source.series(id).ok().map(|d| Signals::series(&d)),
                })
            })
            .collect();
        jobs.into_iter()
            .map(|job| job.join().ok().flatten())
            .collect()
    });
    for (id, signals) in ids.iter().zip(fetched) {
        if let Some(signals) = signals {
            enrichment.signals.insert(*id, signals);
        }
    }
}

/// Adds films from the franchises the query names that the title search did not return.
fn expand_franchises(
    source: &impl Source,
    query: &str,
    country: &str,
    shows: &mut Vec<Show>,
    enrichment: &mut Enrichment,
) {
    let present: HashSet<String> = shows.iter().map(|show| show.tmdb_id.clone()).collect();
    let mut collections = Vec::new();
    for show in shows.iter() {
        let Some(id @ (Kind::Movie, _)) = tmdb_id(show) else {
            continue;
        };
        let named = match_tier(query, &show.title) == 0;
        if let Some(signals) = enrichment.signals.get(&id)
            && named
            && signals.keep(Kind::Movie, true)
            && let Some(collection) = signals.collection
            && !collections.contains(&collection)
        {
            collections.push(collection);
        }
    }
    let mut missing = Vec::new();
    for collection in collections {
        let Ok(details) = source.collection(collection) else {
            continue;
        };
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
    missing.truncate(EXPANSION_LIMIT);
    let found: Vec<(Show, Option<MovieDetailsResponse>)> = std::thread::scope(|scope| {
        let jobs: Vec<_> = missing
            .iter()
            .map(|&id| {
                scope.spawn(move || {
                    let show = source.show(&format!("movie/{id}"), country).ok()?;
                    Some((show, source.movie(id).ok()))
                })
            })
            .collect();
        jobs.into_iter()
            .filter_map(|job| job.join().ok().flatten())
            .collect()
    });
    for (show, details) in found {
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
}

impl Live {
    /// None until a Movie of the Night key is stored; TMDB is optional.
    pub fn new(keys: &Keys, cache: DiskCache) -> Option<Self> {
        Some(Self {
            motn: keys.motn()?,
            tmdb: keys.tmdb(),
            cache,
        })
    }
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
        cached(
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

    fn movie(&self, id: i64) -> Result<MovieDetailsResponse, Error> {
        let tmdb = self.tmdb.as_ref().ok_or(Error::NotConfigured("TMDB"))?;
        cached(
            &self.cache,
            &format!("tmdb:movie:{id}"),
            ttl::TMDB_DETAILS,
            || {
                tmdb.movie_details(&MovieDetailsParams {
                    movie_id: id,
                    append_to_response: None,
                    language: Some(LANGUAGE.into()),
                })
            },
        )
    }

    fn series(&self, id: i64) -> Result<TvSeriesDetailsResponse, Error> {
        let tmdb = self.tmdb.as_ref().ok_or(Error::NotConfigured("TMDB"))?;
        cached(
            &self.cache,
            &format!("tmdb:tv:{id}"),
            ttl::TMDB_DETAILS,
            || {
                tmdb.tv_series_details(&TvSeriesDetailsParams {
                    series_id: id,
                    append_to_response: None,
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

#[cfg(test)]
#[path = "streaming-search-tests.rs"]
mod tests;
