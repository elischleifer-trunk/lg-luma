use super::*;
use crate::tmdb_api::SearchMultiResponse;
use serde::de::DeserializeOwned;
use std::path::Path;

fn fixture<T: DeserializeOwned>(dir: &str, name: &str) -> Option<T> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(dir)
        .join(format!("{name}.json"));
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).unwrap();
    Some(serde_json::from_value(record["response"].clone()).unwrap())
}

/// One captured search, enriched the way the search screen does it: a TMDB `search/multi`
/// plus details for every Movie of the Night result.
fn search(slug: &str) -> (Vec<Show>, Enrichment) {
    let shows: Vec<Show> = fixture("streaming", &format!("search-title-{slug}")).unwrap();
    let mut enrichment = Enrichment::default();
    let multi: SearchMultiResponse = fixture("tmdb", &format!("search-multi-{slug}")).unwrap();
    enrichment.add_search(multi.results.as_deref().unwrap_or_default());
    for show in &shows {
        match tmdb_id(show) {
            Some((Kind::Movie, id)) => {
                if let Some(details) = fixture("tmdb", &format!("movie-{id}")) {
                    enrichment.add_movie(&details);
                }
            }
            Some((Kind::Series, id)) => {
                if let Some(details) = fixture("tmdb", &format!("tv-{id}")) {
                    enrichment.add_series(&details);
                }
            }
            None => {}
        }
    }
    (shows, enrichment)
}

fn ranked(slug: &str, query: &str) -> Vec<Ranked> {
    let (shows, enrichment) = search(slug);
    rank(query, &shows, "us", &Services::default(), Some(&enrichment))
}

fn titles(results: &[Ranked]) -> Vec<String> {
    results
        .iter()
        .map(|r| {
            format!(
                "{} ({})",
                r.show.title,
                streaming::year(&r.show).unwrap_or_default()
            )
        })
        .collect()
}

#[test]
fn ghostbusters_lists_the_five_us_films_oldest_first_then_the_series() {
    let results = ranked("ghostbusters", "ghostbusters");
    let movies: Vec<_> = results
        .iter()
        .take_while(|r| r.show.show_type == ShowType::Movie)
        .map(|r| r.show.tmdb_id.as_str())
        .collect();
    assert_eq!(
        movies,
        [
            "movie/620",    // Ghostbusters (1984)
            "movie/2978",   // Ghostbusters II (1989)
            "movie/43074",  // Ghostbusters (2016)
            "movie/425909", // Ghostbusters: Afterlife (2021)
            "movie/967847", // Ghostbusters: Frozen Empire (2024)
        ],
        "{:#?}",
        titles(&results)
    );
    // Spanish-language and little-seen titles are gone; the US cartoons follow the films.
    let all = titles(&results);
    assert!(
        !all.iter().any(|t| t.starts_with("Mexican Ghostbusters")),
        "{all:#?}"
    );
    assert!(
        !all.iter().any(|t| t.starts_with("Return Of The")),
        "{all:#?}"
    );
    assert!(
        all.iter().any(|t| t == "The Real Ghostbusters (1986)"),
        "{all:#?}"
    );
    assert!(
        results[5..]
            .iter()
            .all(|r| r.show.show_type == ShowType::Series),
        "{all:#?}"
    );
}

#[test]
fn foreign_language_titles_from_abroad_are_filtered_out_unless_named() {
    let (shows, enrichment) = search("gho");
    let results = rank("gho", &shows, "us", &Services::default(), Some(&enrichment));
    for result in &results {
        let signals = enrichment.get(&result.show).unwrap();
        assert!(
            !signals.foreign(),
            "{} ({}, {:?})",
            result.show.title,
            signals.language,
            signals.origin
        );
    }
    let all = titles(&results);
    // Marathi, Kannada, and a Japanese anime whose production countries include its US dub.
    for gone in [
        "Shikshanachya Aaicha Gho!",
        "Gho Mala Asala Hawa",
        "Ghost (2023)",
        "Yu Yu Hakusho",
    ] {
        assert!(!all.iter().any(|t| t.starts_with(gone)), "{gone}: {all:#?}");
    }
    assert!(all.contains(&"Ghostbusters (1984)".to_owned()), "{all:#?}");

    // Searching a foreign film by its own title still finds it, first.
    let amelie = titles(&ranked("amelie", "amélie"));
    assert_eq!(
        amelie.first().map(String::as_str),
        Some("Amélie (2001)"),
        "{amelie:#?}"
    );
}

#[test]
fn english_language_work_from_abroad_stays_but_ranks_below_us_work() {
    let results = ranked("gho", "gho");
    let all = titles(&results);
    let us = all.iter().position(|t| t == "Ghosts (2021)");
    let uk = all.iter().position(|t| t == "Ghosts (2019)");
    assert!(uk.is_some(), "BBC Ghosts kept: {all:#?}");
    assert!(us < uk, "{all:#?}");
}

#[test]
fn without_tmdb_data_results_keep_the_availability_order() {
    let (shows, _) = search("gho");
    let services = Services::default();
    let fallback = rank("gho", &shows, "us", &services, None);
    assert_eq!(fallback, streaming::rank(&shows, "us", &services));
}

#[test]
fn availability_breaks_ties_but_never_outranks_relevance() {
    let (shows, enrichment) = search("ghostbusters");
    // A TV with only Tubi installed: the 1984 film and the cartoons stream free there.
    let tubi = Services::new(&["tubi".into()], &[], &[]);
    let with_apps = rank("ghostbusters", &shows, "us", &tubi, Some(&enrichment));
    let without = rank(
        "ghostbusters",
        &shows,
        "us",
        &Services::default(),
        Some(&enrichment),
    );
    assert_eq!(with_apps[0].show.tmdb_id, "movie/620");
    assert_eq!(
        with_apps
            .iter()
            .map(|r| &r.show.tmdb_id)
            .take(5)
            .collect::<Vec<_>>(),
        without
            .iter()
            .map(|r| &r.show.tmdb_id)
            .take(5)
            .collect::<Vec<_>>()
    );
}

#[test]
fn titles_match_by_leading_words_then_prefix_then_word_then_anywhere() {
    assert_eq!(match_tier("ghostbusters", "Ghostbusters: Afterlife"), 0);
    assert_eq!(match_tier("ghostbusters", "Ghostbusters II"), 0);
    assert_eq!(match_tier("star wars", "Star Wars: Andor"), 0);
    assert_eq!(match_tier("gho", "Ghosts"), 1);
    assert_eq!(match_tier("star wa", "Star Wars"), 1);
    assert_eq!(match_tier("ghostbusters", "The Real Ghostbusters"), 2);
    assert_eq!(match_tier("busters", "Ghostbusters"), 3);
    assert_eq!(match_tier("amelie", "Amélie"), 0);
    assert_eq!(match_tier("dune", "Ghosted"), 4);
}

#[test]
fn sequels_share_a_base_title() {
    assert_eq!(base_title("Ghostbusters: Frozen Empire"), "ghostbusters");
    assert_eq!(base_title("Ghostbusters II"), "ghostbusters");
    assert_eq!(base_title("Dune: Part Two"), "dune");
    assert_eq!(base_title("Toy Story 3"), "toy story");
    assert_eq!(base_title("The Office"), "the office");
}

#[test]
fn studio_and_network_lists_have_no_duplicates() {
    for list in [MAJOR_STUDIOS, MAJOR_NETWORKS] {
        let mut ids: Vec<_> = list.iter().map(|(id, _)| id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), list.len());
    }
}

#[test]
fn us_ratings_come_from_appended_release_dates_and_content_ratings() {
    let dune: MovieDetailsResponse = fixture("tmdb", "movie-438631-rated").unwrap();
    // The festival premiere has no certification; the theatrical release is PG-13.
    assert_eq!(us_certification(&dune).as_deref(), Some("PG-13"));
    assert_eq!(Signals::movie(&dune).rating.as_deref(), Some("PG-13"));
    let office: TvSeriesDetailsResponse = fixture("tmdb", "tv-2316-rated").unwrap();
    assert_eq!(us_tv_rating(&office).as_deref(), Some("TV-14"));
    // Details fetched without ratings, as older cached ones were, have none.
    let unrated: MovieDetailsResponse = fixture("tmdb", "movie-438631").unwrap();
    assert_eq!(us_certification(&unrated), None);
}

#[test]
fn ranked_results_carry_the_rating_of_their_details() {
    let (shows, mut enrichment) = search("dune");
    let dune: MovieDetailsResponse = fixture("tmdb", "movie-438631-rated").unwrap();
    enrichment.add_movie(&dune);
    let ranked = rank(
        "dune",
        &shows,
        "us",
        &Services::default(),
        Some(&enrichment),
    );
    let first = ranked
        .iter()
        .find(|r| r.show.tmdb_id == "movie/438631")
        .unwrap();
    assert_eq!(first.rating.as_deref(), Some("PG-13"));
}
