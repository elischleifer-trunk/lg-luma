use super::*;
use crate::model::Preferences;
use crate::state::Page;

const PAGE: &str = include_str!("../tests/fixtures/criterion/home.html");
const FILM_PAGE: &str = include_str!("../tests/fixtures/criterion/film.html");

fn film(id: &str, title: &str) -> Film {
    Film {
        id: id.into(),
        title: title.into(),
        year: "1944".into(),
        length: "1 hr 28 min".into(),
        link: Some(format!(
            "{ORIGIN}/films/{id}/{}",
            title.to_lowercase().replace(' ', "-")
        )),
        tmdb: None,
        image: format!("https://img.jwplayer.com/v1/media/{id}/images/regalia_16x9.webp"),
    }
}

fn rows(sizes: &[usize]) -> Vec<Row> {
    sizes
        .iter()
        .enumerate()
        .map(|(r, &n)| Row {
            title: format!("Row {r}"),
            films: (0..n)
                .map(|c| film(&format!("film{r}x{c:02}"), &format!("Film {r} {c}")))
                .collect(),
        })
        .collect()
}

fn opened(sizes: &[usize]) -> State {
    let mut state = State::new(Preferences::default());
    state.sync_custom_tiles();
    // Opened from its tile among the Home favorites, where Back returns.
    state.preferences.favorites = Some(vec![APP.into()]);
    state.page = Page::Home;
    state.focus = Target::App(APP.into());
    assert_eq!(state.activate(), vec![Effect::Launch(APP.into())]);
    state.open_criterion();
    state.accept_criterion(Reply {
        rows: Ok(rows(sizes)),
        stale: false,
    });
    state
}

fn cache(name: &str) -> DiskCache {
    let dir = std::env::temp_dir().join(format!("luma-criterion-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    DiskCache::new(dir, 1024 * 1024)
}

#[test]
fn the_home_page_gives_its_film_rows_in_order() {
    let rows = parse(PAGE);
    let titles: Vec<_> = rows.iter().map(|row| row.title.as_str()).collect();
    // The hero carousel has no heading and Featured Collections has no films.
    assert_eq!(titles, ["Popular Movies", "Leaving November 30"]);
    let popular = &rows[0].films;
    assert_eq!(popular.len(), 10);
    assert_eq!(
        popular[1],
        Film {
            id: "oZkRW7X1".into(),
            title: "Laura".into(),
            year: "1944".into(),
            length: "1 hr 28 min".into(),
            link: Some("https://www.criterionchannel.com/films/oZkRW7X1/laura".into()),
            tmdb: None,
            image: "https://img.jwplayer.com/v1/media/oZkRW7X1/images/regalia_16x9.webp?fallback=default_16x9&width=480".into(),
        }
    );
    assert_eq!(popular[1].details(), "1944 · 1 hr 28 min");
    // Every film appears once per row, with a title.
    for row in &rows {
        for (i, film) in row.films.iter().enumerate() {
            assert!(!film.title.is_empty());
            assert!(row.films[..i].iter().all(|other| other.id != film.id));
        }
    }
}

#[test]
fn card_markup_is_read_defensively() {
    let html = r#"<section><h2 class="a_title">Noir &amp; Neo&#x2d;Noir</h2>
        <li><a href="/films/abcd1234/people-s-hero"></a>
            <h3 id="playlist-card-title-abcd1234">People&#x27;s   Hero</h3>
            <span class="z_metaLeft">1988</span></li>
        <li><a href="/films/abcd1234/people-s-hero"></a></li>
        <li><a href="/films/bad/x"></a><h3 id="playlist-card-title-bad">Short id</h3></li>
        <li><a href="/films/efgh5678/Bad Slug"></a></li>
        <li><a href="/films/ijkl9012/untitled"></a></li>
        <li><a href="/films/mnop3456/m"></a><h3 id="playlist-card-title-mnop3456">M</h3>
            <span class="z_metaRight">1 hr 39 min</span></li>
        </section><section><h2>One film</h2><a href="/films/qrst7890/x"></a>
        <h3 id="playlist-card-title-qrst7890">X</h3></section>"#;
    let rows = parse(html);
    assert_eq!(rows.len(), 1, "a row of one is not a row");
    assert_eq!(rows[0].title, "Noir & Neo-Noir");
    let films: Vec<_> = rows[0]
        .films
        .iter()
        .map(|film| (film.id.as_str(), film.title.as_str(), film.details()))
        .collect();
    assert_eq!(
        films,
        [
            ("abcd1234", "People's Hero", "1988".to_owned()),
            ("mnop3456", "M", "1 hr 39 min".to_owned()),
        ]
    );
    assert!(parse("<html>Service unavailable</html>").is_empty());
    assert_eq!(
        clean("a &bogus; b &#0; &#xD800;"),
        "a &bogus; b &#0; &#xD800;"
    );
}

#[test]
fn the_catalog_uses_a_fresh_copy_and_shows_an_old_one_while_reloading() {
    let cache = cache("load");
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let page = || Ok(parse(PAGE));
    let offline = || Err("offline".to_owned());

    // Nothing stored and no network: an error.
    let replies = load(&cache, &offline, now);
    assert!(matches!(&replies[..], [Reply { rows: Err(_), .. }]));

    let replies = load(&cache, &page, now);
    assert!(matches!(&replies[..], [Reply { rows: Ok(rows), stale: false }] if rows.len() == 2));
    // Fresh: from disk, without the network.
    let replies = load(&cache, &offline, now + Duration::from_secs(3600));
    assert!(matches!(
        &replies[..],
        [Reply {
            rows: Ok(_),
            stale: false
        }]
    ));
    // Old: shown at once, and kept when the network fails.
    let later = now + Duration::from_secs(24 * 3600);
    let replies = load(&cache, &offline, later);
    assert!(matches!(
        &replies[..],
        [Reply {
            rows: Ok(_),
            stale: true
        }]
    ));
    let replies = load(&cache, &page, later);
    assert!(matches!(
        &replies[..],
        [
            Reply { stale: true, .. },
            Reply {
                rows: Ok(_),
                stale: false
            }
        ]
    ));
}

/// TMDB's replies, by path, for `tmdb_rows`.
fn tmdb(
    providers: Value,
    discover: impl Fn(&[(&'static str, String)]) -> Result<Value, String>,
) -> impl Fn(&str, &[(&'static str, String)]) -> Result<Value, String> {
    move |path, query| match path {
        "/3/watch/providers/movie" => Ok(providers.clone()),
        "/3/discover/movie" => discover(query),
        _ => Err(format!("unexpected {path}")),
    }
}

fn movie(id: u64, title: &str) -> Value {
    serde_json::json!({
        "id": id, "title": title, "release_date": "1995-05-31",
        "backdrop_path": format!("/b{id}.jpg"),
    })
}

#[test]
fn tmdb_lists_the_channels_films_by_its_provider_id() {
    let providers = serde_json::json!({"results": [
        {"provider_id": 8, "provider_name": "Netflix"},
        {"provider_id": 258, "provider_name": "Criterion Channel"},
    ]});
    let asked = std::sync::Mutex::new(Vec::new());
    let get = tmdb(providers, |query| {
        asked.lock().unwrap().push(query.to_vec());
        let genre = query.iter().find(|(name, _)| *name == "with_genres");
        Ok(match genre.map(|(_, value)| value.as_str()) {
            // A genre with one film is not a row.
            Some("99") => serde_json::json!({"results": [movie(1, "One")]}),
            _ => serde_json::json!({"results": [
                movie(406, "La Haine"),
                movie(407, "Rope"),
                // No 16:9 image, or an odd path: left out.
                {"id": 408, "title": "No image", "backdrop_path": null},
                {"id": 409, "title": "Odd", "backdrop_path": "/../x.jpg"},
            ]}),
        })
    });
    let rows = tmdb_rows(&get).unwrap();
    assert_eq!(rows.len(), TMDB_ROWS.len() - 1);
    assert_eq!(rows[0].title, "Popular on the Criterion Channel");
    assert_eq!(
        rows[0].films[0],
        Film {
            id: "tmdb406".into(),
            title: "La Haine".into(),
            year: "1995".into(),
            length: String::new(),
            link: None,
            tmdb: Some(406),
            image: "https://image.tmdb.org/t/p/w780/b406.jpg".into(),
        }
    );
    assert_eq!(rows[0].films.len(), 2);
    for query in asked.lock().unwrap().iter() {
        assert!(query.contains(&("with_watch_providers", "258".into())));
        assert!(query.contains(&("watch_region", "US".into())));
    }

    let none = tmdb(serde_json::json!({"results": []}), |_| {
        Ok(serde_json::json!({}))
    });
    assert!(tmdb_rows(&none).is_err(), "no Criterion provider");
    let failing = tmdb(
        serde_json::json!({"results": [{"provider_id": 258, "provider_name": "Criterion Channel"}]}),
        |_| Err("TMDB: offline".into()),
    );
    assert_eq!(tmdb_rows(&failing), Err("TMDB: offline".into()));
}

struct Fixed(
    Option<Result<Option<String>, String>>,
    std::sync::Mutex<u32>,
);

impl Source for Fixed {
    fn rows(&self) -> Result<Vec<Row>, String> {
        Ok(rows(&[2]))
    }
    fn link(&self, _: u64) -> Option<Result<Option<String>, String>> {
        *self.1.lock().unwrap() += 1;
        self.0.clone()
    }
    fn details(&self, film: &Film) -> Result<Details, String> {
        *self.1.lock().unwrap() += 1;
        match film.link {
            Some(_) => Ok(parse_details(FILM_PAGE)),
            None => Err("offline".into()),
        }
    }
}

#[test]
fn a_films_page_comes_from_movie_of_the_night_once_else_the_channels_search() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let page = "https://www.criterionchannel.com/films/3sWhDjsA/la-haine";
    let found = Fixed(Some(Ok(Some(page.into()))), Default::default());
    let cache = cache("links");
    assert_eq!(link(&cache, &found, 406, "La Haine", now), page);
    assert_eq!(link(&cache, &found, 406, "La Haine", now), page);
    assert_eq!(*found.1.lock().unwrap(), 1, "asked once, then from disk");
    let search = "https://www.criterionchannel.com/search?q=La%20Haine%3A%20%C3%A9";
    for source in [
        Fixed(Some(Ok(None)), Default::default()),
        Fixed(Some(Err("quota".into())), Default::default()),
        Fixed(None, Default::default()),
    ] {
        assert_eq!(link(&cache, &source, 1, "La Haine: é", now), search);
    }
    // A search is not kept: the next try asks again.
    let missing = Fixed(Some(Ok(None)), Default::default());
    link(&cache, &missing, 2, "x", now);
    link(&cache, &missing, 2, "x", now);
    assert_eq!(*missing.1.lock().unwrap(), 2);
}

#[test]
fn movie_of_the_night_gives_the_channels_page_for_a_film() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/streaming/search-title-bluey.json"
    ))
    .unwrap();
    let shows: Vec<crate::motn_api::Show> =
        serde_json::from_value(fixture["response"].clone()).unwrap();
    let links: Vec<_> = shows.iter().filter_map(criterion_link).collect();
    assert!(
        links.contains(&"https://www.criterionchannel.com/films/3sWhDjsA/three-colors-blue".into()),
        "{links:?}"
    );
}

#[test]
fn a_tmdb_film_opens_once_its_page_is_found() {
    let mut state = opened(&[2]);
    let mut listed = state.criterion.rows[0].films[1].clone();
    listed.link = None;
    listed.tmdb = Some(406);
    state.criterion.rows[0].films[1] = listed.clone();
    state.direction(Direction::Right, &[]);
    assert_eq!(state.activate(), vec![], "the film's page");
    assert_eq!(
        state.criterion_resolve_request(),
        None,
        "Play not chosen yet"
    );
    assert_eq!(state.activate(), vec![], "Play");
    assert_eq!(state.criterion_resolve_request(), Some(listed.clone()));
    assert_eq!(state.criterion_resolve_request(), None, "asked once");
    // A late answer for another film opens nothing.
    assert_eq!(state.criterion_resolved("other", "x".into()), vec![]);
    let page = "https://www.criterionchannel.com/films/3sWhDjsA/la-haine";
    assert_eq!(
        state.criterion_resolved(&listed.id, page.into()),
        vec![Effect::LaunchTitle {
            app: PLAYER.into(),
            service: SERVICE.into(),
            link: page.into(),
        }]
    );
    // Kept on the film: Play opens it at once next time.
    assert_eq!(state.criterion.rows[0].films[1].link.as_deref(), Some(page));
    assert_eq!(state.activate().len(), 1);
    state.back();
    assert_eq!(state.activate(), vec![]);
    assert_eq!(state.activate().len(), 1);
}

#[test]
fn the_tile_opens_the_screen_and_back_returns_to_it() {
    let mut state = opened(&[6, 3]);
    assert!(state.criterion_open());
    assert_eq!(state.focus, Target::Film(0, 0));
    assert!(state.criterion_load_requested());
    assert!(!state.criterion_load_requested(), "asked once");
    state.back();
    assert!(!state.criterion_open());
    assert_eq!(state.focus, Target::App(APP.into()));
    assert!(!crate::platform::can_uninstall(APP));
}

#[test]
fn arrows_move_along_rows_and_keep_the_place_on_screen_between_rows() {
    let mut state = opened(&[8, 3, 6]);
    for _ in 0..5 {
        state.direction(Direction::Right, &[]);
    }
    assert_eq!(state.focus, Target::Film(0, 5));
    // The row scrolled: cards 2-5 are on screen, the focused one last.
    assert_eq!(state.criterion.first_card(0), 2);
    // Down: the same place on screen, clamped to the shorter row's end. Focus moves into the
    // second row on screen without scrolling; the third scrolls one row.
    state.direction(Direction::Down, &[]);
    assert_eq!(state.focus, Target::Film(1, 2));
    assert_eq!(state.criterion.first_row, 0);
    state.direction(Direction::Down, &[]);
    assert_eq!(state.focus, Target::Film(2, 2));
    assert_eq!(state.criterion.first_row, 1);
    state.direction(Direction::Down, &[]);
    assert_eq!(state.focus, Target::Film(2, 2), "the last row stays");
    // Up returns to the first row where it was scrolled to.
    state.direction(Direction::Up, &[]);
    assert_eq!(state.criterion.first_row, 1, "row 1 is still on screen");
    state.direction(Direction::Up, &[]);
    assert_eq!(state.focus, Target::Film(0, 4));
    assert_eq!(state.criterion.first_row, 0);
    assert_eq!(state.criterion.first_card(0), 2);
    for _ in 0..10 {
        state.direction(Direction::Right, &[]);
    }
    assert_eq!(state.focus, Target::Film(0, 7), "the row's end stays");
    for _ in 0..10 {
        state.direction(Direction::Left, &[]);
    }
    assert_eq!(state.focus, Target::Film(0, 0));
    assert_eq!(state.criterion.first_card(0), 0);
}

#[test]
fn ok_opens_the_focused_film_in_the_player() {
    let mut state = opened(&[3]);
    state.direction(Direction::Right, &[]);
    // OK opens the film's page, with Play focused and its details asked for.
    assert_eq!(state.activate(), vec![]);
    assert_eq!(state.focus, Target::CriterionPlay);
    let detail = state.criterion.detail.as_ref().unwrap();
    assert_eq!(detail.film.id, "film0x01");
    assert_eq!(
        state.criterion_details_request().map(|film| film.id),
        Some("film0x01".into())
    );
    assert_eq!(state.criterion_details_request(), None, "asked once");
    // Arrows stay on Play.
    state.direction(Direction::Down, &[]);
    assert_eq!(state.focus, Target::CriterionPlay);
    assert_eq!(
        state.activate(),
        vec![Effect::LaunchTitle {
            app: PLAYER.into(),
            service: SERVICE.into(),
            link: "https://www.criterionchannel.com/films/film0x01/film-0-1".into(),
        }]
    );
    assert!(
        state.criterion_open(),
        "the screen stays for Back from the film"
    );
}

#[test]
fn a_failed_load_offers_try_again() {
    let mut state = State::new(Preferences::default());
    state.open_criterion();
    assert!(state.criterion.loading);
    assert_eq!(state.focus, Target::CriterionRetry);
    state.criterion_load_requested();
    state.accept_criterion(Reply {
        rows: Err("offline".into()),
        stale: false,
    });
    assert!(!state.criterion.loading);
    assert!(state.criterion.error.is_some());
    assert_eq!(state.activate(), vec![]);
    assert!(state.criterion_load_requested());
    assert!(state.criterion.loading);
}

#[test]
fn new_rows_keep_the_focused_film() {
    let mut state = opened(&[4, 4]);
    state.direction(Direction::Down, &[]);
    state.direction(Direction::Right, &[]);
    assert_eq!(state.focus, Target::Film(1, 1));
    let mut moved = rows(&[4, 4]);
    moved.swap(0, 1);
    state.accept_criterion(Reply {
        rows: Ok(moved),
        stale: false,
    });
    assert_eq!(state.focus, Target::Film(0, 1));
}

fn frame() -> Arc<Frame> {
    Arc::new(Frame {
        key: "x".into(),
        serial: 1,
        pixels: image::RgbaImage::new(1, 1),
        live: false,
    })
}

fn requested(state: &mut State) -> Vec<String> {
    state
        .criterion_image_requests()
        .map(|(_, requests)| requests.into_iter().map(|r| r.show_id).collect())
        .unwrap_or_default()
}

#[test]
fn artwork_loads_only_near_the_screen_and_a_failure_is_tried_again_next_visit() {
    let mut state = opened(&[10, 10, 10, 10, 10, 10, 10, 10]);
    let first = state.criterion_image_requests().unwrap();
    // Five rows from the top, each the cards on screen and two beyond: not all 80 films.
    assert_eq!(first.1.len(), 5 * (VISIBLE_CARDS + 2));
    assert_eq!(first.0.len(), first.1.len());
    assert_eq!(first.1[0].show_id, image_key("film0x00"));
    assert_eq!(first.1[0].size, IMAGE_SIZE);
    assert!(state.criterion_image_requests().is_none(), "asked once");

    state.accept_criterion_image(&image_key("film0x00"), Some(frame()));
    state.accept_criterion_image(&image_key("film0x01"), None);
    state.accept_criterion_image("poster-for-search", None);
    assert!(state.criterion.images.contains_key("film0x00"));
    // A failure is not asked again during this visit.
    assert!(requested(&mut state).is_empty());

    // Moving down drops artwork that left the screen and asks for the new rows.
    for _ in 0..4 {
        state.direction(Direction::Down, &[]);
    }
    let asked = requested(&mut state);
    assert!(asked.contains(&image_key("film5x00")), "{asked:?}");
    assert!(!state.criterion.images.contains_key("film0x00"));

    state.back();
    assert!(state.criterion_image_requests().is_none(), "closed");
    state.open_criterion();
    for _ in 0..4 {
        state.direction(Direction::Up, &[]);
    }
    // The failed one is tried again on the next visit.
    assert!(requested(&mut state).contains(&image_key("film0x01")));
}

#[test]
fn back_from_a_film_keeps_the_grid_but_the_home_key_closes_it() {
    let mut state = opened(&[3, 3]);
    state.direction(Direction::Down, &[]);
    assert_eq!(state.activate(), vec![]);
    assert_eq!(state.activate().len(), 1);
    // Leaving the player relaunches Luma as Home: the film's page is still there.
    state.return_home();
    assert!(state.criterion_open());
    assert_eq!(state.focus, Target::CriterionPlay);
    // Back from the page returns to its card.
    state.back();
    assert!(state.criterion_open());
    assert!(state.criterion.detail.is_none());
    assert_eq!(state.focus, Target::Film(1, 0));
    // Home pressed with Luma in front.
    state.return_home();
    assert!(!state.criterion_open());
}

#[test]
fn a_film_given_up_on_does_not_start_when_its_page_is_found() {
    let page = "https://www.criterionchannel.com/films/3sWhDjsA/la-haine";
    let unlinked = |state: &mut State| {
        let film = &mut state.criterion.rows[0].films[0];
        film.link = None;
        film.tmdb = Some(406);
        film.id.clone()
    };
    // Another film chosen while it was looked up.
    let mut state = opened(&[2]);
    let id = unlinked(&mut state);
    assert_eq!(state.activate(), vec![]);
    assert_eq!(state.activate(), vec![]);
    state.back();
    state.direction(Direction::Right, &[]);
    assert_eq!(state.activate(), vec![]);
    assert_eq!(state.activate().len(), 1);
    assert_eq!(state.criterion_resolve_request(), None);
    assert_eq!(state.criterion_resolved(&id, page.into()), vec![]);
    // Its page is still kept.
    assert_eq!(state.criterion.rows[0].films[0].link.as_deref(), Some(page));

    // The screen closed and opened again while it was looked up.
    let mut state = opened(&[2]);
    let id = unlinked(&mut state);
    assert_eq!(state.activate(), vec![]);
    assert_eq!(state.activate(), vec![]);
    state.back();
    state.back();
    state.open_criterion();
    assert_eq!(state.criterion_resolved(&id, page.into()), vec![]);
}

#[test]
fn a_film_page_gives_its_details() {
    let details = parse_details(FILM_PAGE);
    assert_eq!(details.directors, vec!["Brian De Palma"]);
    assert_eq!(
        details.cast,
        vec!["Nicolas Cage", "Gary Sinise", "Carla Gugino"]
    );
    assert_eq!(details.genres, vec!["Mystery", "Thriller"]);
    assert_eq!(details.countries, vec!["United States"]);
    assert_eq!(details.year, "1998");
    assert_eq!(details.length, "1 hr 38 min");
    // The long synopsis, not the structured data's one line.
    assert!(
        details
            .synopsis
            .starts_with("Opening with a bravura, thirteen-minute Steadicam")
    );
    assert!(details.synopsis.ends_with("contributes the score."));
    assert_eq!(parse_details("<html></html>"), Details::default());
    assert_eq!(iso_minutes("PT1H38M20S"), Some(98));
    assert_eq!(iso_minutes("PT45M"), Some(45));
    assert_eq!(iso_minutes("P1D"), None);
    assert_eq!(running_time(120), "2 hr");
}

#[test]
fn tmdb_gives_details_for_films_it_listed() {
    let get = |path: &str, query: &[(&'static str, String)]| {
        assert_eq!(path, "/3/movie/406");
        assert_eq!(query, &[("append_to_response", "credits".to_owned())]);
        Ok(serde_json::json!({
            "overview": "Three friends drift through a day in the banlieue.",
            "release_date": "1995-05-31",
            "runtime": 98,
            "genres": [{"name": "Drama"}, {"name": "Crime"}],
            "production_countries": [{"name": "France"}],
            "credits": {
                "cast": [{"name": "Vincent Cassel"}, {"name": "Hubert Koundé"}],
                "crew": [{"name": "Someone", "job": "Editor"}, {"name": "Mathieu Kassovitz", "job": "Director"}]
            }
        }))
    };
    let details = tmdb_details(&get, 406).unwrap();
    assert_eq!(details.directors, vec!["Mathieu Kassovitz"]);
    assert_eq!(details.cast, vec!["Vincent Cassel", "Hubert Koundé"]);
    assert_eq!(details.genres, vec!["Drama", "Crime"]);
    assert_eq!(details.year, "1995");
    assert_eq!(details.length, "1 hr 38 min");
    let empty = |_: &str, _: &[(&'static str, String)]| Ok(serde_json::json!({}));
    assert!(tmdb_details(&empty, 1).is_err());
}

#[test]
fn details_are_kept_on_disk_and_reach_the_open_page() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let source = Fixed(None, Default::default());
    let cache = cache("details");
    let channel = film("JEnAxeoh", "Snake Eyes");
    let first = details(&cache, &source, &channel, now).unwrap();
    assert_eq!(details(&cache, &source, &channel, now), Ok(first.clone()));
    assert_eq!(*source.1.lock().unwrap(), 1, "the second came from disk");
    let mut unlinked = channel.clone();
    unlinked.id = "tmdb1".into();
    unlinked.link = None;
    assert!(details(&cache, &source, &unlinked, now).is_err());
    assert!(details(&cache, &source, &unlinked, now).is_err());
    assert_eq!(*source.1.lock().unwrap(), 3, "failures are asked again");

    let mut state = opened(&[2]);
    state.activate();
    let id = state.criterion_details_request().unwrap().id;
    // An answer for a film no longer open is kept for later but changes nothing.
    state.accept_criterion_details(("other".into(), Ok(first.clone())));
    assert_eq!(state.criterion.detail.as_ref().unwrap().details, None);
    state.accept_criterion_details((id.clone(), Ok(first.clone())));
    assert_eq!(
        state.criterion.detail.as_ref().unwrap().details,
        Some(first)
    );
    // Opening it again needs no new request.
    state.back();
    state.activate();
    assert_eq!(state.criterion_details_request(), None);
    assert!(state.criterion.detail.as_ref().unwrap().details.is_some());
}

#[test]
fn the_film_page_loads_its_backdrop_and_drops_it_on_back() {
    let mut state = opened(&[3]);
    state.criterion_image_requests();
    state.activate();
    let (wanted, requests) = state.criterion_image_requests().unwrap();
    let hero = hero_key("film0x00");
    assert!(wanted.contains(&hero));
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].show_id, hero);
    assert_eq!(requests[0].size, HERO_SIZE);
    assert!(
        requests[0]
            .url
            .starts_with("https://img.jwplayer.com/v1/media/film0x00/images/hero_wide"),
        "{}",
        requests[0].url
    );
    assert!(state.criterion_image_requests().is_none(), "asked once");
    state.accept_criterion_image(&hero, Some(frame()));
    assert!(state.criterion.detail.as_ref().unwrap().hero.is_some());
    state.back();
    assert!(state.criterion.detail.is_none());
    // A late backdrop for a page no longer open is ignored.
    state.accept_criterion_image(&hero, Some(frame()));
    assert!(state.criterion.detail.is_none());
}

#[test]
fn rows_glide_to_where_focus_scrolled_them() {
    let mut state = opened(&[8, 8, 8, 8]);
    assert!(!state.criterion.settling());
    for _ in 0..5 {
        state.direction(Direction::Right, &[]);
    }
    state.direction(Direction::Down, &[]);
    state.direction(Direction::Down, &[]);
    assert!(state.criterion.settling());
    assert!(state.animating());
    let start = crate::clock::now();
    assert!(
        state
            .criterion
            .animate(false, start + Duration::from_millis(16))
    );
    let row = state.criterion.scroll_row;
    assert!(row > 0. && row < 1., "{row}");
    state
        .criterion
        .animate(false, start + Duration::from_millis(32));
    assert!(state.criterion.scroll_row > row, "still moving");
    for frame in 3..60 {
        state
            .criterion
            .animate(false, start + Duration::from_millis(16 * frame));
    }
    assert!(!state.criterion.settling());
    assert_eq!(state.criterion.scroll_row, 1.);
    assert_eq!(state.criterion.scroll_card(0), 2.);
    // Reduced motion jumps straight there.
    state.direction(Direction::Down, &[]);
    assert!(
        state
            .criterion
            .animate(true, start + Duration::from_secs(5))
    );
    assert_eq!(state.criterion.scroll_row, 2.);
    assert!(!state.criterion.settling());
}

#[test]
fn artwork_cached_at_the_old_video_host_address_is_fetched_from_the_new_one() {
    let mut film = film("oZkRW7X1", "Laura");
    film.image =
        "https://cdn.jwplayer.com/v2/media/oZkRW7X1/images/regalia_16x9.webp?width=480".into();
    assert_eq!(
        film.card_image(),
        "https://img.jwplayer.com/v1/media/oZkRW7X1/images/regalia_16x9.webp?width=480"
    );
    assert!(
        film.hero()
            .starts_with("https://img.jwplayer.com/v1/media/oZkRW7X1/images/hero_wide")
    );
    film.image = "https://image.tmdb.org/t/p/w780/abc.jpg".into();
    assert_eq!(film.hero(), "https://image.tmdb.org/t/p/w1280/abc.jpg");
}

#[test]
fn back_from_a_film_page_cancels_its_play_lookup() {
    let page = "https://www.criterionchannel.com/films/3sWhDjsA/la-haine";
    let mut state = opened(&[2]);
    let film = &mut state.criterion.rows[0].films[0];
    film.link = None;
    film.tmdb = Some(406);
    let id = film.id.clone();
    assert_eq!(state.activate(), vec![]);
    assert_eq!(state.activate(), vec![], "Play looks up the page");
    assert!(state.criterion_resolve_request().is_some());
    state.back();
    assert!(state.criterion.detail.is_none());
    // The answer only keeps the film's page.
    assert_eq!(state.criterion_resolved(&id, page.into()), vec![]);
    assert_eq!(state.criterion.rows[0].films[0].link.as_deref(), Some(page));
}

#[test]
fn a_film_page_asks_for_its_details_once_while_they_load() {
    let mut state = opened(&[2]);
    state.activate();
    let film = state.criterion_details_request().unwrap();
    state.back();
    state.activate();
    assert_eq!(state.criterion_details_request(), None, "still loading");
    state.accept_criterion_details((film.id.clone(), Err("offline".into())));
    assert!(state.criterion.detail.as_ref().unwrap().failed);
    // After a failure, the next visit asks again.
    state.back();
    state.activate();
    let again = state.criterion_details_request().unwrap();
    state.accept_criterion_details((again.id, Err("offline".into())));
    // A request Back cancels before it was sent does not block the next visit.
    state.back();
    state.activate();
    state.back();
    state.activate();
    assert!(state.criterion_details_request().is_some());
}

#[test]
fn odd_durations_are_rejected_without_panicking() {
    assert_eq!(iso_minutes("PT90分"), None);
    assert_eq!(iso_minutes("PT4294967295H"), None);
    assert_eq!(iso_minutes("PT99999999999M"), None);
    assert_eq!(iso_minutes("PT"), None);
    assert_eq!(iso_minutes("PT1H"), Some(60));
}
