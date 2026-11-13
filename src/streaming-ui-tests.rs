//! Movies and shows under the Apps search: when searches are sent, how results are drawn and
//! reached with the remote, and what OK opens.
use super::{controls, ready, scene};
use crate::renderer::Draw;
use crate::state::{Direction, Effect, State, TITLE_COLUMNS, Target};
use crate::streaming::{self, DEBOUNCE, Show};
use std::time::Instant;

fn fixture(name: &str) -> Vec<Show> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/fixtures/streaming/search-title-{name}.json"));
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    streaming::parse_search(&record["response"]).unwrap()
}

/// The Apps page with `query` typed and a Movie of the Night key stored.
fn typed(query: &str) -> State {
    let mut state = ready();
    state.open(1);
    state.streaming_keys.motn = true;
    state.query = query.into();
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
    assert_eq!(request.query, state.query.to_lowercase());
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
fn without_a_key_the_apps_search_stays_apps_only() {
    let mut state = typed("dune");
    state.streaming_keys.motn = false;
    let now = Instant::now();
    assert!(state.sync_titles(now).is_none());
    assert!(state.sync_titles(now + DEBOUNCE * 4).is_none());
    assert!(!shows_text(&state, "Movies & shows"));
    state.query.clear();
    assert!(shows_text(&state, "Find an app"));
    state.streaming_keys.motn = true;
    assert!(shows_text(&state, "Apps, movies, shows"));
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
fn results_replace_the_no_apps_empty_state_and_take_focus_from_the_search_field() {
    let mut state = typed("Dune");
    state.sync_titles(Instant::now());
    assert!(shows_text(&state, "Searching…"));
    answer(&mut state, "dune");
    assert!(!state.titles.results.is_empty());
    assert!(!shows_text(&state, "No matches"));
    assert!(!shows_text(&state, "Searching…"));
    let titles = controls(&state)
        .iter()
        .filter(|control| matches!(control.target, Target::Title(_)))
        .count();
    assert_eq!(titles, state.titles.results.len());
    assert_eq!(state.first_content(), Target::Title(0));

    // The field is at the right, so Down lands on the first row's right-hand card.
    state.focus = Target::Search;
    state.direction(Direction::Down, &controls(&state));
    assert_eq!(state.focus, Target::Title(TITLE_COLUMNS - 1));
    state.direction(Direction::Left, &controls(&state));
    state.direction(Direction::Left, &controls(&state));
    assert_eq!(state.focus, Target::Title(0));
    state.direction(Direction::Right, &controls(&state));
    assert_eq!(state.focus, Target::Title(1));
    state.direction(Direction::Down, &controls(&state));
    assert_eq!(state.focus, Target::Title(1 + TITLE_COLUMNS));
    state.direction(Direction::Up, &controls(&state));
    state.direction(Direction::Up, &controls(&state));
    assert_eq!(
        state.focus,
        Target::Search,
        "no apps: Up returns to the field"
    );
}

#[test]
fn remote_moves_between_the_app_grid_and_the_results() {
    let mut state = typed("netflix");
    let netflix = Target::App("netflix".into());
    assert_eq!(state.first_content(), netflix);
    answer(&mut state, "dune");
    state.focus = netflix.clone();
    state.direction(Direction::Down, &controls(&state));
    assert_eq!(state.focus, Target::Title(0));
    state.direction(Direction::Up, &controls(&state));
    assert_eq!(state.focus, netflix);
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
    state.query = "the office".into();
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
    state.query.clear();
    state.sync_titles(Instant::now());
    assert!(state.titles.results.is_empty());
    assert!(!matches!(state.focus, Target::Title(_)));
    assert!(!shows_text(&state, "Movies & shows"));
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
