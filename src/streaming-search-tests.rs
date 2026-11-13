use super::*;
use crate::motn_api::ShowType;
use std::{path::Path, sync::Mutex};

/// Captured replies (see `pnpm fixtures:streaming`), recording every request made.
#[derive(Default)]
struct Fixtures {
    calls: Mutex<Vec<String>>,
    tmdb_down: bool,
    no_tmdb: bool,
    /// TMDB search works but every details request fails (a timeout or 429).
    details_down: bool,
}

fn missing() -> Error {
    Error::Status {
        status: 404,
        message: "not captured".into(),
    }
}

impl Fixtures {
    fn read<T: DeserializeOwned>(&self, dir: &str, name: &str) -> Result<T, Error> {
        self.calls.lock().unwrap().push(format!("{dir}/{name}"));
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(dir)
            .join(format!("{name}.json"));
        let bytes = std::fs::read(path).map_err(|_| missing())?;
        let record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        Ok(serde_json::from_value(record["response"].clone()).unwrap())
    }

    fn calls(&self, prefix: &str) -> Vec<String> {
        let calls = self.calls.lock().unwrap();
        let mut calls: Vec<_> = calls
            .iter()
            .filter(|call| call.starts_with(prefix))
            .cloned()
            .collect();
        calls.sort();
        calls
    }
}

fn slug(query: &str) -> String {
    match query {
        "amélie" => "amelie".into(),
        other => other.replace(' ', "-"),
    }
}

impl Source for Fixtures {
    fn search(&self, query: &str, _country: &str) -> Result<Vec<Show>, Error> {
        self.read("streaming", &format!("search-title-{}", slug(query)))
    }
    fn show(&self, id: &str, _country: &str) -> Result<Show, Error> {
        self.read("streaming", &format!("show-{}", id.replace('/', "-")))
    }
    fn tmdb_search(&self, query: &str) -> Option<Result<Vec<SearchMultiResponseResult>, Error>> {
        if self.no_tmdb {
            return None;
        }
        if self.tmdb_down {
            return Some(Err(Error::Network("timeout".into())));
        }
        Some(
            self.read::<tmdb_api::SearchMultiResponse>(
                "tmdb",
                &format!("search-multi-{}", slug(query)),
            )
            .map(|reply| reply.results.unwrap_or_default()),
        )
    }
    fn movie(&self, id: i64) -> Result<MovieDetailsResponse, Error> {
        if self.details_down {
            return Err(Error::Status {
                status: 429,
                message: "Too many requests".into(),
            });
        }
        self.read("tmdb", &format!("movie-{id}"))
    }
    fn series(&self, id: i64) -> Result<TvSeriesDetailsResponse, Error> {
        if self.details_down {
            return Err(Error::Network("timeout".into()));
        }
        self.read("tmdb", &format!("tv-{id}"))
    }
    fn collection(&self, id: i64) -> Result<CollectionDetailsResponse, Error> {
        self.read("tmdb", &format!("collection-{id}"))
    }
}

fn ids(results: &[Ranked]) -> Vec<&str> {
    results.iter().map(|r| r.show.tmdb_id.as_str()).collect()
}

#[test]
fn ghostbusters_end_to_end_lists_the_five_films_from_1984() {
    let source = Fixtures::default();
    let results = search(&source, "ghostbusters", "us", &Services::default()).unwrap();
    assert_eq!(
        ids(&results)[..5],
        [
            "movie/620",
            "movie/2978",
            "movie/43074",
            "movie/425909",
            "movie/967847"
        ]
    );
    assert!(
        results[5..]
            .iter()
            .all(|r| r.show.show_type == ShowType::Series)
    );
    // The collection was consulted, and every film in it was already found by the search.
    assert_eq!(source.calls("tmdb/collection"), ["tmdb/collection-2980"]);
    assert!(source.calls("streaming/show-").is_empty());
}

#[test]
fn the_free_search_summary_spares_details_calls_for_titles_that_cannot_rank() {
    let source = Fixtures::default();
    search(&source, "gho", "us", &Services::default()).unwrap();
    let details = source.calls("tmdb/movie-").len() + source.calls("tmdb/tv-").len();
    // Movie of the Night returned 20 shows; foreign and little-seen titles were ruled out by
    // the search summary without a call. Ghost (1990) is in English and was looked up.
    assert!(source.calls("tmdb/movie-251").len() == 1);
    assert!(details < 20, "{details} details calls");
    // Shikshanachya Aaicha Gho! and Gho Mala Asala Hawa: Marathi, ruled out by the summary.
    for skipped in ["tmdb/movie-114564", "tmdb/movie-1222309"] {
        assert!(
            !source.calls("tmdb/").iter().any(|call| call == skipped),
            "{skipped}"
        );
    }
    // Ghost (2023, Kannada) is not on TMDB's first page for "gho", so it has no summary: it
    // is looked up, then filtered out like the others.
    assert_eq!(source.calls("tmdb/movie-1040194").len(), 1);
}

#[test]
fn franchise_films_the_title_search_missed_are_looked_up() {
    let source = Fixtures::default();
    search(&source, "star wars", "us", &Services::default()).unwrap();
    // The Star Wars collection lists films the 20 title results did not include; each is
    // requested from Movie of the Night (not captured here, so they are simply skipped).
    assert!(
        source
            .calls("tmdb/collection")
            .contains(&"tmdb/collection-10".to_owned()),
        "{:?}",
        source.calls("tmdb/collection")
    );
    let shows = source.calls("streaming/show-");
    assert!(
        !shows.is_empty() && shows.len() <= EXPANSION_LIMIT,
        "{shows:?}"
    );
}

#[test]
fn without_tmdb_results_keep_the_order_of_where_they_stream() {
    let services = Services::default();
    let expected = {
        let shows: Vec<Show> = Fixtures::default()
            .read("streaming", "search-title-gho")
            .unwrap();
        streaming::rank(&shows, "us", &services)
    };
    for source in [
        Fixtures {
            tmdb_down: true,
            ..Default::default()
        },
        Fixtures {
            no_tmdb: true,
            ..Default::default()
        },
    ] {
        assert_eq!(search(&source, "gho", "us", &services).unwrap(), expected);
        assert!(source.calls("tmdb/").is_empty());
    }
}

#[test]
fn a_movie_of_the_night_failure_is_the_search_error() {
    let source = Fixtures::default();
    let error = search(&source, "not captured", "us", &Services::default()).unwrap_err();
    assert!(matches!(error, Error::Status { status: 404, .. }));
}

#[test]
fn cached_replies_skip_the_api_and_errors_are_not_cached() {
    let dir = std::env::temp_dir().join(format!("luma-search-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = DiskCache::new(dir.clone(), crate::search_cache::DEFAULT_LIMIT);
    let calls = std::cell::Cell::new(0);
    let fetch = || {
        calls.set(calls.get() + 1);
        Ok(vec!["Ghostbusters".to_owned()])
    };
    let day = std::time::Duration::from_secs(86_400);
    assert_eq!(cached(&cache, "k", day, fetch).unwrap(), ["Ghostbusters"]);
    assert_eq!(cached(&cache, "k", day, fetch).unwrap(), ["Ghostbusters"]);
    assert_eq!(calls.get(), 1);
    let failing = || -> Result<Vec<String>, Error> { Err(Error::Network("down".into())) };
    assert!(cached(&cache, "e", day, failing).is_err());
    assert!(cached(&cache, "e", day, || Ok(vec!["now".to_owned()])).is_ok());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn generated_types_survive_the_cache_round_trip() {
    let source = Fixtures::default();
    let shows = source.search("ghostbusters", "us").unwrap();
    let movie = source.movie(620).unwrap();
    let shows_again: Vec<Show> =
        serde_json::from_slice(&serde_json::to_vec(&shows).unwrap()).unwrap();
    let movie_again: MovieDetailsResponse =
        serde_json::from_slice(&serde_json::to_vec(&movie).unwrap()).unwrap();
    assert_eq!(shows_again, shows);
    assert_eq!(movie_again, movie);
}

#[test]
fn titles_whose_details_fail_are_kept_after_the_ranked_ones() {
    let source = Fixtures {
        details_down: true,
        ..Default::default()
    };
    let results = search(&source, "the office", "us", &Services::default()).unwrap();
    let shows = source.search("the office", "us").unwrap();
    let summary: Vec<String> = source
        .tmdb_search("the office")
        .unwrap()
        .unwrap()
        .iter()
        .filter_map(|r| Some(format!("{}/{}", r.media_type.as_deref()?, r.id?)))
        .collect();
    let undescribed: Vec<&str> = shows
        .iter()
        .map(|show| show.tmdb_id.as_str())
        .filter(|id| !summary.iter().any(|known| known == id))
        .collect();
    assert!(!undescribed.is_empty());
    let found = ids(&results);
    // Every title TMDB could not describe is still offered, at the end in API order.
    assert_eq!(found[found.len() - undescribed.len()..], undescribed[..]);
}
