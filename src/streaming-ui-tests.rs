//! The Search tab: how it opens, when searches are sent, how the selected title and the
//! results are drawn and reached with the remote, and what OK opens.
use super::{controls, ready, scene};
use crate::renderer::Draw;
use crate::state::{Direction, Effect, Overlay, Page, SEARCH_TAB, State, TITLE_COLUMNS, Target};
use crate::streaming::{self, DEBOUNCE, Show};
use std::time::Instant;

fn fixture(name: &str) -> Vec<Show> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/fixtures/streaming/search-title-{name}.json"));
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    streaming::parse_search(&record["response"]).unwrap()
}

/// The Search page with `query` typed, the keyboard closed and a Movie of the Night key stored.
fn typed(query: &str) -> State {
    let mut state = ready();
    state.streaming_keys.motn = true;
    state.open(SEARCH_TAB);
    state.text(query);
    state.back();
    assert_eq!(state.focus, Target::Nav(SEARCH_TAB));
    state
}

/// Lets the debounce pass and answers the search with a captured reply.
fn answer(state: &mut State, reply: &str) {
    let now = Instant::now();
    assert!(
        state.sync_titles(now).is_none(),
        "waits for typing to pause"
    );
    let request = state
        .sync_titles(now + DEBOUNCE)
        .expect("searches after the pause");
    assert_eq!(request.query, state.title_query.to_lowercase());
    let ranked = streaming::rank(&fixture(reply), "us", &state.streaming_services());
    state.accept_titles(&request, Ok(ranked));
}

fn shows_text(state: &State, value: &str) -> bool {
    scene(state)
        .draw
        .iter()
        .any(|draw| matches!(draw, Draw::Text(text, ..) if text == value))
}

#[test]
fn the_search_tab_sits_left_of_home_and_opens_with_the_keyboard() {
    let mut state = ready();
    state.focus = Target::Nav(0);
    state.direction(Direction::Left, &controls(&state));
    assert_eq!(state.page, Page::Search);
    assert!(state.text_input_active(), "the keyboard comes up");
    assert!(matches!(state.overlay, Some(Overlay::Keyboard)));
    state.text("dune");
    assert_eq!(state.title_query, "dune");
    assert!(state.query.is_empty(), "the Apps search is separate");
    state.back();
    assert_eq!(state.focus, Target::Nav(SEARCH_TAB));
    // The tab is now the field, wider than any other tab.
    let field = controls(&state)
        .into_iter()
        .find(|control| control.target == Target::Nav(SEARCH_TAB))
        .unwrap();
    let home = controls(&state)
        .into_iter()
        .find(|control| control.target == Target::Nav(0))
        .unwrap();
    assert!(
        field.rect.w > home.rect.w * 3.,
        "{} vs {}",
        field.rect.w,
        home.rect.w
    );
    assert!(field.rect.w <= crate::view::SEARCH_FIELD_WIDTH);
    assert!(field.rect.x >= 300., "clear of the clock");
    assert!(shows_text(&state, "dune"));
    // OK on the field brings the keyboard back; Right goes on to Home.
    state.activate();
    assert!(state.text_input_active());
    state.back();
    state.direction(Direction::Right, &controls(&state));
    assert_eq!(state.page, Page::Home);
    // Leftmost tab: Left goes nowhere.
    state.open(SEARCH_TAB);
    state.back();
    state.direction(Direction::Left, &controls(&state));
    assert_eq!(state.page, Page::Search);
    assert!(
        state.title_query.is_empty(),
        "the field starts empty each time"
    );
}

#[test]
fn without_a_key_the_search_page_says_how_to_add_one() {
    let mut state = typed("dune");
    state.streaming_keys.motn = false;
    let now = Instant::now();
    assert!(state.sync_titles(now).is_none());
    assert!(state.sync_titles(now + DEBOUNCE * 4).is_none());
    assert!(shows_text(
        &state,
        "Add a Movie of the Night key in Settings → System → Streaming search keys to search movies and shows."
    ));
    // The Apps search finds apps only.
    state.open(1);
    assert!(shows_text(&state, "Find an app"));
}

#[test]
fn short_queries_ask_for_more_typing_and_send_nothing() {
    let mut state = typed("du");
    let now = Instant::now();
    assert!(state.sync_titles(now).is_none());
    assert!(state.sync_titles(now + DEBOUNCE * 4).is_none());
    assert!(shows_text(&state, "Movies & shows"));
    assert!(shows_text(&state, "Keep typing to search movies and shows"));
}

#[test]
fn results_are_reached_from_the_field_and_the_first_is_described_on_top() {
    let mut state = typed("Dune");
    state.sync_titles(Instant::now());
    assert!(shows_text(&state, "Searching…"));
    answer(&mut state, "dune");
    assert!(!state.titles.results.is_empty());
    assert!(!shows_text(&state, "Searching…"));
    let titles = controls(&state)
        .iter()
        .filter(|control| matches!(control.target, Target::Title(_)))
        .count();
    assert_eq!(titles, state.titles.results.len());
    assert_eq!(state.first_content(), Target::Title(0));

    // The first result is described on top until another is selected.
    let first = state.titles.results[0].show.title.clone();
    let second = state.titles.results[1].show.title.clone();
    let large = |state: &State, title: &str| {
        scene(state).draw.iter().any(|draw| {
            matches!(draw, Draw::Text(text, _, style, ..) if text == title && style.size >= 40.)
        })
    };
    assert!(large(&state, &first));

    state.direction(Direction::Down, &controls(&state));
    assert_eq!(state.focus, Target::Title(0));
    state.direction(Direction::Right, &controls(&state));
    assert_eq!(state.focus, Target::Title(1));
    assert!(
        large(&state, &second),
        "the selected title moves to the top"
    );
    state.direction(Direction::Down, &controls(&state));
    assert_eq!(state.focus, Target::Title(1 + TITLE_COLUMNS));
    state.direction(Direction::Up, &controls(&state));
    state.direction(Direction::Up, &controls(&state));
    assert_eq!(
        state.focus,
        Target::Nav(SEARCH_TAB),
        "Up returns to the field"
    );

    // Back from a result returns to the field, then to Home.
    state.focus = Target::Title(2);
    state.back();
    assert_eq!(state.focus, Target::Nav(SEARCH_TAB));
    state.back();
    assert_eq!(state.page, Page::Home);
}

#[test]
fn the_selected_title_shows_its_us_rating_length_and_where_it_plays() {
    let mut state = typed("dune");
    answer(&mut state, "dune");
    let index = state
        .titles
        .results
        .iter()
        .position(|r| r.show.tmdb_id == "movie/438631")
        .unwrap();
    state.titles.results[index].rating = Some("PG-13".into());
    state.focus = Target::Title(index);
    for text in ["2021", "PG-13", "2h 35m", "Movie"] {
        assert!(shows_text(&state, text), "{text}");
    }
}

#[test]
fn ok_opens_the_title_in_its_app_or_explains_why_it_cannot() {
    let mut state = typed("dune");
    answer(&mut state, "dune");
    let (index, pick) = state
        .titles
        .results
        .iter()
        .enumerate()
        .find_map(|(index, result)| Some((index, result.pick.clone()?)))
        .expect("a result on one of the demo TV's apps");
    state.focus = Target::Title(index);
    assert_eq!(
        state.activate(),
        vec![Effect::LaunchTitle {
            app: pick.app_id.clone(),
            params: crate::deeplink::launch(&pick.service_id, &pick.link).map(|l| l.params),
        }]
    );
    let missing = state
        .titles
        .results
        .iter()
        .position(|result| result.pick.is_none())
        .expect("a result on none of the demo TV's apps");
    state.focus = Target::Title(missing);
    assert!(state.activate().is_empty());
    assert_eq!(
        state.notice.as_ref().map(|(text, _)| text.as_str()),
        Some("Not on your TV’s apps")
    );
}

#[test]
fn editing_the_query_drops_stale_replies_and_keeps_focus_on_a_result() {
    let mut state = typed("dune");
    let now = Instant::now();
    state.sync_titles(now);
    let stale = state.sync_titles(now + DEBOUNCE).unwrap();
    state.title_query = "the office".into();
    state.sync_titles(now);
    state.accept_titles(
        &stale,
        Ok(streaming::rank(
            &fixture("dune"),
            "us",
            &state.streaming_services(),
        )),
    );
    assert!(
        state.titles.results.is_empty(),
        "reply was for the old text"
    );

    answer(&mut state, "the-office");
    let last = state.titles.results.len() - 1;
    state.focus = Target::Title(last);
    state.title_query.clear();
    state.sync_titles(Instant::now());
    assert!(state.titles.results.is_empty());
    assert!(!matches!(state.focus, Target::Title(_)));
    assert!(shows_text(
        &state,
        "Type a title to see where it streams on your TV’s apps."
    ));
}

#[test]
fn search_failures_are_shown_without_dropping_the_field() {
    let mut state = typed("dune");
    let now = Instant::now();
    state.sync_titles(now);
    let request = state.sync_titles(now + DEBOUNCE).unwrap();
    state.accept_titles(&request, Err("offline".into()));
    assert!(shows_text(
        &state,
        "Movie and show search is unavailable right now."
    ));
}

#[test]
fn posters_are_requested_once_per_result_and_dropped_with_it() {
    let mut state = typed("dune");
    assert!(state.poster_requests().is_none(), "nothing to load yet");
    answer(&mut state, "dune");
    let requests = state
        .poster_requests()
        .expect("posters for the new results");
    assert_eq!(requests.len(), state.titles.results.len());
    assert!(
        requests
            .iter()
            .all(|r| r.url.starts_with("https://cdn.movieofthenight.com/"))
    );
    assert!(state.poster_requests().is_none(), "already loading");

    // A loaded poster replaces the plain title card.
    let first = state.titles.results[0].show.id.clone();
    let poster = std::sync::Arc::new(crate::cameras::Frame {
        key: crate::posters::texture_key(&first),
        serial: 1,
        pixels: image::RgbaImage::new(2, 3),
        live: false,
    });
    state.accept_poster(&first, Some(poster));
    let posters = scene(&state)
        .draw
        .iter()
        .filter(|draw| matches!(draw, Draw::Camera(frame, ..) if frame.key == format!("poster:{first}")))
        .count();
    assert_eq!(posters, 1);
    // A late poster for a show no longer listed is ignored.
    state.accept_poster("gone", None);
    assert!(!state.posters.contains_key("gone"));

    state.title_query.clear();
    state.sync_titles(Instant::now());
    assert_eq!(state.poster_requests().map(|r| r.len()), Some(0));
    assert!(state.posters.is_empty(), "decoded posters are freed");
}

fn icons(state: &State) -> Vec<String> {
    scene(state)
        .draw
        .iter()
        .filter_map(|draw| match draw {
            Draw::TintedImage(path, ..) => Some(path.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn scores_are_fetched_in_one_batch_only_with_an_mdblist_key() {
    let mut state = typed("dune");
    answer(&mut state, "dune");
    assert!(state.score_requests().is_none(), "no MDBList key");
    state.streaming_keys.mdblist = true;
    let ids = state.score_requests().expect("one batch for the results");
    assert_eq!(ids.len(), state.titles.results.len());
    assert!(state.score_requests().is_none(), "already loading");

    // Dune (2021): fresh critics and audience scores on its poster.
    let dune = crate::mdblist::Scores {
        critics: Some(83),
        audience: Some(90),
        imdb: Some(8.0),
    };
    state.accept_scores(vec![("tt1160419".into(), dune)]);
    assert!(shows_text(&state, "83%") && shows_text(&state, "90%"));
    let drawn = icons(&state);
    assert!(
        drawn
            .iter()
            .any(|path| path.ends_with("ui-icons/tomato.lvec"))
    );
    assert!(
        drawn
            .iter()
            .any(|path| path.ends_with("ui-icons/popcorn.lvec"))
    );

    // Without Rotten Tomatoes scores the IMDb rating stands in; with none, no badge.
    let index = state
        .titles
        .results
        .iter()
        .position(|r| r.show.imdb_id != "tt1160419")
        .unwrap();
    let other = state.titles.results[index].show.imdb_id.clone();
    state.accept_scores(vec![(
        other.clone(),
        crate::mdblist::Scores {
            imdb: Some(5.75),
            ..Default::default()
        },
    )]);
    assert!(shows_text(&state, "5.8"));
    state.accept_scores(vec![(other, crate::mdblist::Scores::default())]);
    assert!(!shows_text(&state, "5.8"));
    // Late scores for a show no longer listed are ignored.
    state.accept_scores(vec![("tt0000001".into(), dune)]);
    assert!(!state.scores.contains_key("tt0000001"));

    state.title_query.clear();
    state.sync_titles(Instant::now());
    assert!(state.score_requests().is_none());
    assert!(
        state.scores.is_empty(),
        "scores are dropped with their results"
    );
}

#[test]
fn two_rows_of_posters_fit_under_the_selected_title() {
    let mut state = typed("dune");
    answer(&mut state, "dune");
    assert!(state.titles.results.len() >= TITLE_COLUMNS * 2);
    let second_row = controls(&state)
        .into_iter()
        .find(|control| control.target == Target::Title(TITLE_COLUMNS))
        .unwrap();
    assert!(
        second_row.rect.y + second_row.rect.h <= 1068.,
        "{:?}",
        second_row.rect
    );
    state.focus = Target::Title(TITLE_COLUMNS);
    assert!(
        !state.reveal_focus(&controls(&state)),
        "no scrolling to reach it"
    );
}

#[test]
fn a_failed_search_says_so_even_with_results_on_screen() {
    let mut state = typed("dune");
    answer(&mut state, "dune");
    // "dune drifter" narrows "dune" locally while its own search runs, then fails.
    state.title_query = "dune drifter".into();
    let now = Instant::now();
    state.sync_titles(now);
    assert!(!state.titles.results.is_empty(), "narrowed locally");
    assert!(shows_text(&state, "Searching…"));
    let request = state.sync_titles(now + DEBOUNCE).unwrap();
    state.accept_titles(&request, Err("offline".into()));
    assert!(shows_text(
        &state,
        "Movie and show search is unavailable right now."
    ));
}

#[test]
fn installing_or_removing_an_app_searches_the_current_text_again() {
    let mut state = typed("dune");
    answer(&mut state, "dune");
    assert!(state.titles.deadline().is_none());
    let mut inventory = state.inventory.clone();
    inventory.apps.pop();
    state.set_inventory(inventory.clone(), None);
    assert!(
        state.titles.deadline().is_some(),
        "picks were made for the old apps"
    );
    // The same apps again change nothing.
    let request = state.sync_titles(Instant::now() + DEBOUNCE).unwrap();
    state.accept_titles(&request, Ok(state.titles.results.clone()));
    state.set_inventory(inventory, None);
    assert!(state.titles.deadline().is_none());
}
