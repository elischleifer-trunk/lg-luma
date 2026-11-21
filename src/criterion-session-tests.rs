use super::*;

const NOW: f64 = 1_800_000_000.;
const DAY: f64 = 86_400.;

fn cookie(name: &str, domain: &str, expires: Option<f64>) -> Cookie {
    Cookie {
        name: name.into(),
        value: format!("{name}-value"),
        domain: domain.into(),
        path: "/".into(),
        expires,
        secure: true,
        http_only: true,
        same_site: Some("Lax".into()),
    }
}

fn signed_in(days: f64) -> Vec<Cookie> {
    vec![
        cookie(
            SESSION_COOKIE,
            "www.criterionchannel.com",
            Some(NOW + days * DAY),
        ),
        cookie("auth0", "login.criterion.com", Some(NOW + 3. * DAY)),
        cookie(
            "OptanonConsent",
            ".criterionchannel.com",
            Some(NOW + 365. * DAY),
        ),
    ]
}

#[test]
fn only_the_channels_cookies_with_its_sign_in_are_accepted() {
    assert!(validate(signed_in(30.)).is_ok());
    assert!(validate(vec![]).is_err());
    assert!(
        validate(signed_in(30.)[1..].to_vec()).is_err(),
        "no __session"
    );
    let mut foreign = signed_in(30.);
    foreign.push(cookie("x", ".criterionchannel.com.evil.example", None));
    assert!(validate(foreign).is_err());
    let mut lookalike = signed_in(30.);
    lookalike.push(cookie("x", "notcriterion.com", None));
    assert!(validate(lookalike).is_err());
    let mut bad_same_site = signed_in(30.);
    bad_same_site[0].same_site = Some("lax; evil".into());
    assert!(validate(bad_same_site).is_err());
    assert!(validate(vec![cookie("x", "www.criterionchannel.com", None); 65]).is_err());
    // A __session on another site of the Channel's is not its sign-in.
    assert_eq!(
        expires(&[cookie(SESSION_COOKIE, "login.criterion.com", Some(NOW))]),
        None
    );
}

#[test]
fn the_longest_lasting_usable_sign_in_wins() {
    let stored = Session::new(signed_in(10.));
    assert!(better(&signed_in(20.), Some(&stored), NOW));
    assert!(!better(&signed_in(5.), Some(&stored), NOW));
    assert!(
        !better(&signed_in(10.), Some(&stored), NOW),
        "same expiry: nothing to do"
    );
    assert!(better(&signed_in(5.), None, NOW));
    assert!(!better(&signed_in(-1.), None, NOW), "already expired");
    let turned_away = Session {
        valid: false,
        ..Session::new(signed_in(25.))
    };
    assert!(
        better(&signed_in(30.), Some(&turned_away), NOW),
        "a sign-in the Channel turned away is replaced by a newer one"
    );
    assert!(
        !better(&signed_in(25.), Some(&turned_away), NOW),
        "but not by another copy of itself, which would look usable again"
    );
    assert!(!turned_away.usable(NOW));
    assert!(!Session::new(signed_in(-1.)).usable(NOW));
}

#[test]
fn the_status_never_carries_the_cookies() {
    let status = Session::new(signed_in(30.)).status();
    assert_eq!(status["expires"], json!(NOW + 30. * DAY));
    assert_eq!(status["valid"], json!(true));
    assert!(!status.to_string().contains("value"));
    assert!(!format!("{:?}", Session::new(signed_in(30.))).contains("-value"));
}

#[test]
fn offering_keeps_only_a_better_sign_in_in_a_private_file() {
    let dir = std::env::temp_dir().join(format!("luma-session-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let file = path(&dir.join("home-assistant.json"));
    let now = now();
    let fresh = |days: f64| {
        let mut cookies = signed_in(0.);
        cookies[0].expires = Some(now + days * DAY);
        cookies
    };
    assert!(offer(&file, fresh(10.), now).unwrap().0);
    assert!(!offer(&file, fresh(5.), now).unwrap().0);
    let (kept, session) = offer(&file, fresh(20.), now).unwrap();
    assert!(kept);
    assert_eq!(load(&file), session);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    forget(&file).unwrap();
    assert_eq!(load(&file), None);
    forget(&file).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn devtools_cookies_round_trip_keeping_host_and_domain_cookies_apart() {
    let host = cookie(SESSION_COOKIE, "www.criterionchannel.com", Some(NOW + DAY));
    let sent = host.to_devtools();
    assert_eq!(sent["url"], json!("https://www.criterionchannel.com/"));
    assert!(sent.get("domain").is_none());
    let domain = cookie("OptanonConsent", ".criterionchannel.com", None).to_devtools();
    assert_eq!(domain["domain"], json!(".criterionchannel.com"));
    assert!(domain.get("expires").is_none());
    let read = Cookie::from_devtools(&json!({
        "name": "__session", "value": "v", "domain": "www.criterionchannel.com", "path": "/",
        "expires": NOW + DAY, "httpOnly": true, "secure": true, "session": false, "sameSite": "Lax"
    }))
    .unwrap();
    assert_eq!(read.expires, Some(NOW + DAY));
    assert!(read.http_only);
    let browser_session = Cookie::from_devtools(&json!({
        "name": "x", "value": "v", "domain": "a.criterion.com", "path": "/", "expires": -1,
        "session": true
    }))
    .unwrap();
    assert_eq!(browser_session.expires, None);
}

#[test]
fn renewed_cookies_replace_the_old_ones() {
    let mut cookies = signed_in(1.);
    apply_set_cookie(
        &mut cookies,
        "__session=new; Path=/; Max-Age=2592000; Secure; HttpOnly; SameSite=lax",
        "www.criterionchannel.com",
        NOW,
    );
    let session: Vec<_> = cookies
        .iter()
        .filter(|c| c.name == SESSION_COOKIE)
        .collect();
    assert_eq!(session.len(), 1);
    assert_eq!(session[0].value, "new");
    assert_eq!(session[0].expires, Some(NOW + 30. * DAY));
    assert_eq!(session[0].same_site.as_deref(), Some("Lax"));
    assert!(session[0].http_only && session[0].secure);
    // Expires instead of Max-Age, and a domain cookie.
    apply_set_cookie(
        &mut cookies,
        "seen=1; Domain=criterionchannel.com; Expires=Wed, 21 Oct 2037 07:28:00 GMT",
        "www.criterionchannel.com",
        NOW,
    );
    let seen = cookies.iter().find(|c| c.name == "seen").unwrap();
    assert_eq!(seen.domain, ".criterionchannel.com");
    assert_eq!(seen.expires, Some(2139722880.));
    // Max-Age=0 removes; another site's cookie is ignored.
    apply_set_cookie(
        &mut cookies,
        "seen=; Domain=criterionchannel.com; Max-Age=0",
        "www.criterionchannel.com",
        NOW,
    );
    assert!(!cookies.iter().any(|c| c.name == "seen"));
    apply_set_cookie(
        &mut cookies,
        "x=1; Domain=evil.example",
        "www.criterionchannel.com",
        NOW,
    );
    assert!(!cookies.iter().any(|c| c.name == "x"));
}

#[test]
fn requests_carry_only_the_cookies_for_their_host() {
    let header = cookie_header(
        &signed_in(1.),
        "www.criterionchannel.com",
        "/auth/profile",
        NOW,
    );
    assert!(header.contains("__session=__session-value"));
    assert!(
        header.contains("OptanonConsent="),
        "domain cookies go to subdomains"
    );
    assert!(
        !header.contains("auth0"),
        "login.criterion.com's stay there"
    );
    let expired = vec![cookie(
        SESSION_COOKIE,
        "www.criterionchannel.com",
        Some(NOW - 1.),
    )];
    assert_eq!(
        cookie_header(&expired, "www.criterionchannel.com", "/", NOW),
        ""
    );
}

#[test]
fn http_dates_parse() {
    assert_eq!(http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0.));
    assert_eq!(
        http_date("Sat, 21 Nov 2026 08:36:43 GMT"),
        Some(1_795_250_203.)
    );
    assert_eq!(
        http_date("Sunday, 06-Nov-94 08:49:37 GMT"),
        Some(784_111_777.)
    );
    assert_eq!(http_date("garbage"), None);
}

#[test]
fn an_empty_sign_in_is_no_sign_in() {
    let mut blank = signed_in(30.);
    blank[0].value.clear();
    assert_eq!(expires(&blank), None);
    assert!(validate(blank).is_err());
}

#[test]
fn the_web_browsers_cookies_keep_only_the_channels_sign_in_first() {
    let mut cookies = vec![cookie("x", "criterion.example.com", None)];
    cookies.extend((0..70).map(|i| cookie(&format!("t{i}"), ".criterionchannel.com", None)));
    cookies.extend(signed_in(30.));
    let kept = channel_cookies(cookies);
    assert_eq!(kept.len(), 64);
    assert!(kept.iter().all(|c| allowed_domain(&c.domain)));
    assert!(
        validate(kept.clone()).is_ok(),
        "the sign-in survives the trim"
    );
    assert!(kept.iter().any(|c| c.name == "auth0"));
}

#[test]
fn forgetting_marks_the_players_sign_in_for_removal_until_a_new_one_is_kept() {
    let dir = std::env::temp_dir().join(format!("luma-session-forget-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = path(&dir.join("home-assistant.json"));
    let now = now();
    let mut cookies = signed_in(0.);
    cookies[0].expires = Some(now + 10. * DAY);
    offer(&file, cookies.clone(), now).unwrap();
    forget(&file).unwrap();
    assert_eq!(load(&file), None);
    assert!(
        forgotten(&file).exists(),
        "the player is signed out at its next film"
    );
    cookies[0].expires = Some(now + 20. * DAY);
    assert!(offer(&file, cookies, now).unwrap().0);
    assert!(!forgotten(&file).exists(), "signing in again is wanted");
    let _ = std::fs::remove_dir_all(&dir);
}
