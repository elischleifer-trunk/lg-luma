use super::*;
use crate::model::Preferences;
use crate::state::Page;

const PAGE: &str = include_str!("../tests/fixtures/criterion/home.html");

fn film(id: &str, title: &str) -> Film {
    Film {
        id: id.into(),
        slug: title.to_lowercase().replace(' ', "-"),
        title: title.into(),
        year: "1944".into(),
        length: "1 hr 28 min".into(),
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
            slug: "laura".into(),
            title: "Laura".into(),
            year: "1944".into(),
            length: "1 hr 28 min".into(),
        }
    );
    assert_eq!(popular[1].details(), "1944 · 1 hr 28 min");
    assert_eq!(
        popular[1].link(),
        "https://www.criterionchannel.com/films/oZkRW7X1/laura"
    );
    assert!(
        popular[1]
            .image_url()
            .starts_with("https://cdn.jwplayer.com/v2/media/oZkRW7X1/images/regalia_16x9.webp")
    );
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
    let page = || Ok(PAGE.to_owned());
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
    // A page without films is an error, not an empty screen.
    let empty = || Ok("<html></html>".to_owned());
    let replies = load(&self::cache("empty"), &empty, now);
    assert!(matches!(&replies[..], [Reply { rows: Err(_), .. }]));
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
    // Down: the same place on screen, clamped to the shorter row's end.
    state.direction(Direction::Down, &[]);
    assert_eq!(state.focus, Target::Film(1, 2));
    assert_eq!(state.criterion.first_row, 1);
    state.direction(Direction::Down, &[]);
    assert_eq!(state.focus, Target::Film(2, 2));
    state.direction(Direction::Down, &[]);
    assert_eq!(state.focus, Target::Film(2, 2), "the last row stays");
    // Up returns to the first row where it was scrolled to.
    state.direction(Direction::Up, &[]);
    state.direction(Direction::Up, &[]);
    assert_eq!(state.focus, Target::Film(0, 4));
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

#[test]
fn artwork_is_asked_for_once_per_film_and_only_while_open() {
    let mut state = opened(&[2, 2]);
    let requests = state.criterion_image_requests().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].show_id, image_key("film0x00"));
    assert_eq!(requests[0].size, IMAGE_SIZE);
    assert!(state.criterion_image_requests().is_none());
    state.accept_criterion_image(&image_key("film0x00"), None);
    state.accept_criterion_image("poster-for-search", None);
    state.back();
    state.open_criterion();
    // Reopened: only films without a request so far (none) are asked for.
    assert_eq!(state.criterion_image_requests().map(|r| r.len()), Some(0));
    state.back();
    assert!(state.criterion_image_requests().is_none());
}
