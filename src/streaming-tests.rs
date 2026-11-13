use super::*;
use crate::motn_api::ShowType;
use serde_json::{Value, json};

/// A service as the spec requires it; only the ID matters to ranking.
fn service(id: &str) -> Value {
    json!({
        "id": id, "name": id, "homePage": format!("https://{id}.example"),
        "themeColorCode": "#000000",
        "imageSet": {"lightThemeImage": "", "darkThemeImage": "", "whiteImage": ""},
    })
}

fn option(service_id: &str, kind: &str, price: Option<&str>) -> Value {
    let mut option = json!({
        "service": service(service_id),
        "type": kind,
        "link": format!("https://{service_id}.example/watch"),
        "audios": [], "subtitles": [], "availableSince": 0, "expiresSoon": false,
    });
    if let Some(amount) = price {
        option["price"] =
            json!({"amount": amount, "currency": "USD", "formatted": format!("{amount} USD")});
    }
    option
}

fn show(id: &str, title: &str, show_type: &str, options: Vec<Value>) -> Show {
    serde_json::from_value(json!({
        "itemType": "show",
        "showType": show_type,
        "id": id,
        "imdbId": "tt0000000",
        "tmdbId": format!("movie/{id}"),
        "title": title,
        "originalTitle": title,
        "overview": "",
        "genres": [],
        "cast": [],
        "releaseYear": 1984,
        "rating": 78,
        "imageSet": {"verticalPoster": vertical(), "horizontalPoster": horizontal()},
        "streamingOptions": {"us": options},
    }))
    .unwrap()
}

fn vertical() -> Value {
    json!({"w240": "", "w360": "", "w480": "", "w600": "", "w720": ""})
}

fn horizontal() -> Value {
    json!({"w360": "", "w480": "", "w720": "", "w1080": "", "w1440": ""})
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// A TV with Netflix, Prime Video, Apple TV and Hulu; Prime used last, then Netflix.
fn services() -> Services {
    Services::new(
        &ids(&[
            "netflix",
            "amazon",
            "com.apple.appletv",
            "hulu",
            "youtube.leanback.v4",
        ]),
        &ids(&["amazon", "netflix"]),
        &ids(&["hulu"]),
    )
}

#[test]
fn most_recently_used_app_wins_between_included_services() {
    let ghosts = show(
        "1",
        "Ghosts",
        "series",
        vec![
            option("hulu", "subscription", None),
            option("netflix", "subscription", None),
            option("prime", "subscription", None),
        ],
    );
    let (pick, also) = pick(&ghosts, "us", &services());
    let pick = pick.unwrap();
    assert_eq!(pick.app_id, "amazon");
    assert_eq!(pick.access, Access::Included);
    // Recent history first, then favorites.
    assert_eq!(also, ids(&["netflix", "hulu"]));
}

#[test]
fn included_access_beats_renting_on_a_more_recent_app() {
    let ghostbusters = show(
        "2",
        "Ghostbusters",
        "movie",
        vec![
            option("prime", "rent", Some("3.99")),
            option("hulu", "subscription", None),
        ],
    );
    let pick = pick(&ghostbusters, "us", &services()).0.unwrap();
    assert_eq!(pick.app_id, "hulu");
    assert_eq!(pick.access, Access::Included);
}

#[test]
fn rentals_are_offered_when_nothing_is_included_preferring_recency_then_price() {
    let movie = show(
        "3",
        "Ghostbusters: Afterlife",
        "movie",
        vec![
            option("apple", "buy", Some("14.99")),
            option("apple", "rent", Some("2.99")),
            option("prime", "rent", Some("3.99")),
        ],
    );
    let pick = pick(&movie, "us", &services()).0.unwrap();
    assert_eq!(pick.app_id, "amazon");
    assert_eq!(pick.access, Access::Rent);
    assert_eq!(pick.price.as_deref(), Some("3.99 USD"));
}

#[test]
fn services_without_an_installed_app_are_ignored() {
    let only_elsewhere = show(
        "4",
        "Ghost",
        "movie",
        vec![option("peacock", "subscription", None)],
    );
    assert_eq!(pick(&only_elsewhere, "us", &services()), (None, vec![]));
    let other_country = show("5", "Ghosts (UK)", "series", vec![]);
    assert!(pick(&other_country, "gb", &services()).0.is_none());
}

#[test]
fn results_rank_included_then_paid_then_unavailable_keeping_relevance_order() {
    let shows = vec![
        show(
            "a",
            "Ghost",
            "movie",
            vec![option("peacock", "subscription", None)],
        ),
        show(
            "b",
            "Ghostbusters",
            "movie",
            vec![option("prime", "rent", Some("3.99"))],
        ),
        show(
            "c",
            "Ghosts",
            "series",
            vec![option("netflix", "subscription", None)],
        ),
        show(
            "d",
            "Ghost Adventures",
            "series",
            vec![option("hulu", "free", None)],
        ),
    ];
    let order: Vec<_> = rank(&shows, "us", &services())
        .into_iter()
        .map(|r| r.show.id)
        .collect();
    assert_eq!(order, ["c", "d", "b", "a"]);
}

#[test]
fn unknown_option_types_and_unknown_fields_do_not_break_parsing() {
    let mut new_type = option("netflix", "subscription", None);
    new_type["type"] = json!("something-new");
    let ghosts = json!({
        "itemType": "show", "showType": "series", "id": "1", "imdbId": "tt1", "tmdbId": "tv/1",
        "title": "Ghosts", "originalTitle": "Ghosts", "overview": "", "genres": [], "cast": [],
        "firstAirYear": 2021, "rating": 74, "newField": true,
        "imageSet": {"verticalPoster": vertical(), "horizontalPoster": horizontal()},
        "streamingOptions": {"us": [option("netflix", "subscription", None), new_type]},
    });
    let reply = json!([ghosts, {"title": "missing id is skipped"}]);
    let shows = parse_search(&reply).unwrap();
    assert_eq!(shows.len(), 1);
    assert_eq!(year(&shows[0]), Some(2021));
    assert_eq!(shows[0].show_type, ShowType::Series);
    assert_eq!(
        pick(&shows[0], "us", &services()).0.unwrap().app_id,
        "netflix"
    );
    // Filter search wraps the list.
    assert_eq!(
        parse_search(&json!({"shows": [], "hasMore": false})).unwrap(),
        vec![]
    );
    assert!(parse_search(&json!({"message": "error"})).is_err());
}

#[test]
fn incremental_search_waits_for_three_characters_and_a_pause() {
    let start = Instant::now();
    let mut search = Incremental::<Show>::default();
    search.set_text("gh", start);
    assert_eq!(search.deadline(), None);
    assert_eq!(search.due(start + DEBOUNCE * 2), None);

    search.set_text("gho", start);
    assert_eq!(search.due(start + DEBOUNCE / 2), None);
    // Typing again restarts the wait.
    search.set_text("ghos", start + DEBOUNCE / 2);
    assert_eq!(search.due(start + DEBOUNCE), None);
    let request = search.due(start + DEBOUNCE / 2 + DEBOUNCE).unwrap();
    assert_eq!(request.query, "ghos");
    assert!(search.searching);
    // Sent once only.
    assert_eq!(search.due(start + DEBOUNCE * 10), None);
}

#[test]
fn stale_replies_are_cached_but_not_shown() {
    let now = Instant::now();
    let mut search = Incremental::<Show>::default();
    search.set_text("gho", now);
    let old = search.due(now + DEBOUNCE).unwrap();
    search.set_text("ghostb", now + DEBOUNCE);
    let shows = vec![
        show("1", "Ghosts", "series", vec![]),
        show("2", "Ghostbusters", "movie", vec![]),
    ];
    assert!(!search.accept(&old, Ok(shows.clone())));
    assert!(search.results.is_empty() || search.provisional);

    // Backspacing to the old text shows the cached reply at once, with no new request.
    search.set_text("  GHO ", now + DEBOUNCE * 2);
    assert_eq!(search.results, shows);
    assert!(!search.provisional);
    assert_eq!(search.deadline(), None);
}

#[test]
fn longer_text_filters_a_cached_prefix_until_its_own_reply_arrives() {
    let now = Instant::now();
    let mut search = Incremental::<Show>::default();
    search.set_text("gho", now);
    let first = search.due(now + DEBOUNCE).unwrap();
    search.accept(
        &first,
        Ok(vec![
            show("1", "Ghosts", "series", vec![]),
            show("2", "Ghostbusters", "movie", vec![]),
        ]),
    );
    search.set_text("ghostbu", now + DEBOUNCE * 2);
    assert!(search.provisional);
    assert_eq!(search.results.len(), 1);
    assert_eq!(search.results[0].title, "Ghostbusters");

    let second = search.due(now + DEBOUNCE * 3).unwrap();
    assert!(search.accept(&second, Err("timeout".into())));
    assert_eq!(search.error.as_deref(), Some("timeout"));
    assert!(!search.searching);

    // Clearing below the minimum empties the list.
    search.set_text("g", now + DEBOUNCE * 4);
    assert!(search.results.is_empty());
}

/// Every captured API reply (see `pnpm fixtures:streaming`) parses without losing shows.
#[test]
fn captured_fixtures_parse() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/streaming");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("search-") && !name.starts_with("popular-") {
            continue;
        }
        let record: Value = serde_json::from_slice(&std::fs::read(entry.path()).unwrap()).unwrap();
        let reply = &record["response"];
        let raw = reply
            .get("shows")
            .unwrap_or(reply)
            .as_array()
            .unwrap()
            .len();
        assert_eq!(parse_search(reply).unwrap().len(), raw, "{name}");
    }
}
