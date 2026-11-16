//! Launch parameters that open a title inside a streaming app, built from the web link Movie of
//! the Night returns for that service. Each app reads its own format, passed as webOS launch
//! `params` (see `platform::launch_with_params`). A service without a known format, or a link
//! that fails validation, opens the app's home screen instead.
//!
//! Formats marked `Unverified` are best-known community formats that have not been confirmed on
//! a TV. Confirm them with `pnpm deeplink:probe` (tools/deeplink-probe.mjs) and flip them to
//! `Verified` here; the probe's candidate list is where alternatives are tried.
//!
//! Hulu's ID is not in its links: `resolve` reads it from the title's web page, so it blocks
//! and must run on a worker thread. `launch` never touches the network.
use serde_json::{Value, json};
use std::{io::Read, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    Verified,
    Unverified,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Launch {
    pub params: Value,
    pub confidence: Confidence,
}

/// Web hosts each service's links may use. Links come from the network, so anything else is
/// refused rather than handed to an app.
const HOSTS: &[(&str, &[&str])] = &[
    ("netflix", &["www.netflix.com", "netflix.com"]),
    (
        "prime",
        &[
            "www.amazon.com",
            "amazon.com",
            "www.primevideo.com",
            "primevideo.com",
            "app.primevideo.com",
        ],
    ),
    ("disney", &["www.disneyplus.com", "disneyplus.com"]),
    ("hbo", &["play.max.com", "play.hbomax.com", "www.max.com"]),
    ("hulu", &["www.hulu.com", "hulu.com"]),
    ("apple", &["tv.apple.com"]),
    ("peacock", &["www.peacocktv.com", "peacocktv.com"]),
    ("paramount", &["www.paramountplus.com", "paramountplus.com"]),
];

/// Launch parameters for `link` on `service`, or None to open the app normally.
pub fn launch(service: &str, link: &str) -> Option<Launch> {
    let path = validated_path(service, link)?;
    let unverified = |params| {
        Some(Launch {
            params,
            confidence: Confidence::Unverified,
        })
    };
    match service {
        // Netflix's DIAL launch payload, which its TV app also accepts as contentTarget.
        "netflix" => {
            let id = numeric_after(path, &["title", "watch"])?;
            unverified(json!({
                "contentTarget": format!("m=https://www.netflix.com/title/{id}&source_type=4")
            }))
        }
        // Apple TV takes the title's web URL as its contentTarget (confirmed on a TV, 2026-11).
        "apple" => Some(Launch {
            params: json!({ "contentTarget": link }),
            confidence: Confidence::Verified,
        }),
        // Apps believed to take the title's web URL as their DIAL/contentTarget.
        "disney" | "hbo" | "peacock" | "paramount" => unverified(json!({ "contentTarget": link })),
        // Prime Video's appinfo.json declares `{"contentTarget":"$CONTENTID"}`, and LG's own
        // search fills it with the title's GTI (`amzn1.dv.gti.<uuid>`), captured with
        // ls-monitor on a TV (2026-11). A GTI converted from a Movie of the Night link opened
        // the title on a TV; web links and the compact ID open its home screen.
        // Titles included with Prime link to app.primevideo.com/detail?gti=<GTI>; rentals and
        // purchases to amazon.com/gp/video/detail/<compact GTI>.
        "prime" => {
            let gti = match query_value(path, "gti") {
                Some(gti) => valid_gti(gti)?,
                None => gti_from_compact(segment_after(path, &["detail", "dp"])?)?,
            };
            Some(Launch {
                params: json!({ "contentTarget": gti }),
                confidence: Confidence::Verified,
            })
        }
        // Hulu ignores web links and its own UUIDs (both open its home screen); see `resolve`.
        _ => None,
    }
}

/// Like `launch`, but may read the title's web page first (Hulu). Blocking: call it from a
/// worker thread, never the SDL thread.
pub fn resolve(service: &str, link: &str) -> Option<Launch> {
    if service != "hulu" {
        return launch(service, link);
    }
    let page_url = hulu_page(link)?;
    let page = match fetch_page(&page_url) {
        Ok(page) => page,
        Err(error) => {
            crate::telemetry::log_warn!("deeplink", "Hulu page for {page_url}: {error}");
            return None;
        }
    };
    let result = hulu_launch(&page, link);
    if result.is_none() {
        crate::telemetry::log_warn!("deeplink", "No Hulu content ID on {page_url}");
    }
    result
}

/// Hulu's webOS app declares `{"contentTarget":"$CONTENTID"}`, and LG's own search fills it with
/// `<numeric id>_content` (captured with ls-monitor on a TV, 2026-11). The numeric ID is the
/// middle field of the title's `EAB::<uuid>::<id>::<asset>` ID on its hulu.com page.
/// A movie page carries its own EAB ID. A series page has none for the series, only for its
/// episodes, so a series opens its first listed episode (not yet confirmed on a TV). Episodes
/// count only when their entry links back to this series: other shows' episodes may be on the page.
fn hulu_launch(page: &str, link: &str) -> Option<Launch> {
    let (kind, entity) = hulu_entity(validated_path("hulu", link)?)?;
    let (id, confidence) = if kind == "movie" {
        (
            eab_ids(page).find(|eab| eab.entity == entity)?.id,
            Confidence::Verified,
        )
    } else {
        // Each episode entry is `…"href":"/series/<slug>-<series uuid>","eabId":"EAB::…`.
        let own = format!("{entity}\",\"eabId\":\"");
        let episode = page
            .match_indices(&own)
            .filter_map(|(at, _)| eab_ids(&page[at + own.len()..]).next())
            .find(|eab| eab.kind == Some("episode"))?;
        (episode.id, Confidence::Unverified)
    };
    Some(Launch {
        params: json!({ "contentTarget": format!("{id}_content") }),
        confidence,
    })
}

/// `/movie/<slug>-<uuid>` or `/series/<slug>-<uuid>` → (kind, uuid).
fn hulu_entity(path: &str) -> Option<(&str, &str)> {
    let path = path.split(['?', '#']).next()?;
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    let kind = segments
        .next()
        .filter(|k| matches!(*k, "movie" | "series"))?;
    let slug = segments.next()?;
    let entity = slug.get(slug.len().checked_sub(36)?..)?;
    is_uuid(entity).then_some((kind, entity))
}

/// The page to read for a Hulu link, on Hulu's canonical host (the bare host redirects).
fn hulu_page(link: &str) -> Option<String> {
    let path = validated_path("hulu", link)?;
    let (kind, _) = hulu_entity(path)?;
    let slug = path
        .split(['?', '#'])
        .next()?
        .trim_matches('/')
        .split('/')
        .nth(1)?;
    Some(format!("https://www.hulu.com/{kind}/{slug}"))
}

/// A lowercase UUID, as Hulu writes entity IDs.
fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        })
}

struct Eab<'a> {
    entity: &'a str,
    id: &'a str,
    /// The `"type"` written right after the ID in the page's JSON, when there is one.
    kind: Option<&'a str>,
}

/// Every `EAB::<uuid>::<numeric id>::…` ID on a page, in page order.
fn eab_ids(page: &str) -> impl Iterator<Item = Eab<'_>> {
    page.match_indices("EAB::").filter_map(|(at, _)| {
        let rest = &page[at + 5..];
        let entity = rest.get(..36).filter(|e| is_uuid(e))?;
        let rest = rest[36..].strip_prefix("::")?;
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if !(1..=12).contains(&digits) || !rest[digits..].starts_with("::") {
            return None;
        }
        let kind = rest[digits..]
            .split_once('"')
            .and_then(|(_, after)| after.strip_prefix(",\"type\":\""))
            .and_then(|after| after.split('"').next());
        Some(Eab {
            entity,
            id: &rest[..digits],
            kind,
        })
    })
}

/// A hulu.com page: no proxy or redirects, a short timeout and a bounded reply.
fn fetch_page(url: &str) -> Result<String, String> {
    const LIMIT: u64 = 3 * 1024 * 1024;
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .max_redirects(0)
        .proxy(None)
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .header("User-Agent", "LumaNative")
        .header("Accept", "text/html")
        .call()
        .map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("HTTP {status}"));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("the page is too large".into());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The path of an https link on one of the service's own hosts, with no userinfo, port or odd
/// characters.
fn validated_path<'a>(service: &str, link: &'a str) -> Option<&'a str> {
    if link.len() > 512 || !link.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let rest = link.strip_prefix("https://")?;
    let (host, path) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    let hosts = HOSTS.iter().find(|(s, _)| *s == service)?.1;
    hosts.contains(&host).then_some(path)
}

/// The value of `key` in a path's query string: `/detail?gti=x&ref=y` → `x`.
fn query_value<'a>(path: &'a str, key: &str) -> Option<&'a str> {
    let query = path.split('#').next()?.split_once('?')?.1;
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
}

/// A GTI exactly as Prime Video writes it: `amzn1.dv.gti.` and a lowercase UUID.
fn valid_gti(gti: &str) -> Option<String> {
    let uuid = gti.strip_prefix("amzn1.dv.gti.")?;
    let shape = uuid.len() == 36
        && uuid.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        });
    shape.then(|| gti.to_owned())
}

/// The path segment following one of `markers`: `/gp/video/detail/0O2P…/` → `0O2P…`.
fn segment_after<'a>(path: &'a str, markers: &[&str]) -> Option<&'a str> {
    let path = path.split(['?', '#']).next()?;
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    while let Some(segment) = segments.next() {
        if markers.contains(&segment) {
            return segments.next();
        }
    }
    None
}

/// Amazon's full title ID from the compact one in its web links. The compact ID is the GTI's
/// 128-bit UUID plus 2^128, in uppercase base 36: `0O2PTZNG3GJQ205Y02FHPY7HGD` is
/// `amzn1.dv.gti.96a9f6e0-1128-c70c-e520-1986b41e8a1d` (checked against 164 pairs that
/// amazon.com pages list side by side as `compactGTI` and `gti`). Older ASIN-style IDs
/// (`B0FV8FQK6M`) are not GTIs and give None.
fn gti_from_compact(compact: &str) -> Option<String> {
    if compact.len() != 26 {
        return None;
    }
    // Base 36 into 2^128 · high + low, exactly: high must come out as 1.
    let (mut high, mut low) = (0u128, 0u128);
    for c in compact.chars() {
        let digit = u128::from(c.to_digit(36).filter(|_| !c.is_ascii_lowercase())?);
        // low · 36 + digit, carrying into high; 36 < 2^6, so split low to avoid overflow.
        let (lo, hi) = (low & u128::from(u64::MAX), low >> 64);
        let lo = lo * 36 + digit;
        let hi = hi * 36 + (lo >> 64);
        low = (lo & u128::from(u64::MAX)) | (hi << 64);
        high = high.checked_mul(36)?.checked_add(hi >> 64)?;
    }
    if high != 1 {
        return None;
    }
    let hex = format!("{low:032x}");
    Some(format!(
        "amzn1.dv.gti.{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

/// The numeric segment following one of `markers` in a URL path: `/title/81223232/` → 81223232.
fn numeric_after(path: &str, markers: &[&str]) -> Option<String> {
    let path = path.split(['?', '#']).next()?;
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    while let Some(segment) = segments.next() {
        if markers.contains(&segment) {
            let id = segments.next()?;
            return (id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit())).then(|| id.into());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netflix_links_become_its_dial_payload() {
        for link in [
            "https://www.netflix.com/title/81223232/",
            "https://www.netflix.com/title/81223232?trkid=1",
            "https://www.netflix.com/watch/81223232",
        ] {
            assert_eq!(
                launch("netflix", link).unwrap().params,
                json!({"contentTarget": "m=https://www.netflix.com/title/81223232&source_type=4"}),
                "{link}"
            );
        }
        assert!(launch("netflix", "https://www.netflix.com/browse").is_none());
        assert!(launch("netflix", "https://www.netflix.com/title/8122x/").is_none());
    }

    #[test]
    fn other_services_pass_their_own_web_link() {
        let link = "https://www.disneyplus.com/browse/entity-2b0a7c1e-91ab-4c7e-9a3a-2e6f1c1d1b2f";
        let result = launch("disney", link).unwrap();
        assert_eq!(result.params, json!({"contentTarget": link}));
        assert_eq!(result.confidence, Confidence::Unverified);
    }

    /// Trimmed from hulu.com/movie/ghostbusters-0a7184af-… (2026-11): the movie's own EAB ID
    /// comes after other titles' in "You May Also Like".
    const GHOSTBUSTERS_PAGE: &str = concat!(
        r#"{"id":"7790712b-297b-4898-8fc0-f4dee57c068f","href":"/movie/batman-returns-7790712b-297b-4898-8fc0-f4dee57c068f","eabId":null,"type":"movie"},"#,
        r#"{"eabId":"EAB::9ff68316-f6e5-475f-874c-7732f0c4dcf5::169127190::346067506","type":"movie","name":"Ghostbusters II"},"#,
        r#"{"id":"0a7184af-7705-4884-bcef-908c27edb1af","eabId":"EAB::0a7184af-7705-4884-bcef-908c27edb1af::169126830::353249532","type":"movie","name":"Ghostbusters"}"#,
    );
    const GHOSTBUSTERS: &str =
        "https://www.hulu.com/movie/ghostbusters-0a7184af-7705-4884-bcef-908c27edb1af";

    #[test]
    fn hulu_movies_open_with_their_numeric_content_id() {
        assert!(launch("hulu", GHOSTBUSTERS).is_none(), "needs the page");
        assert_eq!(
            hulu_page(GHOSTBUSTERS).as_deref(),
            Some(GHOSTBUSTERS),
            "already canonical"
        );
        assert_eq!(
            hulu_launch(GHOSTBUSTERS_PAGE, GHOSTBUSTERS).unwrap(),
            Launch {
                params: json!({"contentTarget": "169126830_content"}),
                confidence: Confidence::Verified,
            }
        );
        // Percent-encoded slugs, the bare host and query strings resolve to the same page.
        let spanish = "https://hulu.com/movie/ghostbusters-%28en-espa%C3%B1ol%29-da941add-b21a-4a5a-b991-96de6e0f2188?x=1";
        assert_eq!(
            hulu_page(spanish).as_deref(),
            Some(
                "https://www.hulu.com/movie/ghostbusters-%28en-espa%C3%B1ol%29-da941add-b21a-4a5a-b991-96de6e0f2188"
            )
        );
        // Each movie takes its own ID, wherever it is on the page; a page without it opens the
        // app plainly, never another title.
        let other =
            "https://www.hulu.com/movie/ghostbusters-ii-9ff68316-f6e5-475f-874c-7732f0c4dcf5";
        assert_eq!(
            hulu_launch(GHOSTBUSTERS_PAGE, other).unwrap().params,
            json!({"contentTarget": "169127190_content"})
        );
        let missing = "https://www.hulu.com/movie/x-11111111-2222-3333-4444-555555555555";
        assert!(hulu_launch(GHOSTBUSTERS_PAGE, missing).is_none());
    }

    #[test]
    fn hulu_series_open_their_first_listed_episode() {
        // Trimmed from hulu.com/series/the-handmaid%27s-tale-565d8976-… (2026-11).
        let page = concat!(
            r#"{"id":"565d8976-9d26-4e63-866c-40f8a137ce5f","name":"The Handmaid's Tale"},"#,
            // Another show's episode, as a recommendation rail could list it, comes first.
            r#"{"href":"/series/ghosts-2b0a7c1e-91ab-4c7e-9a3a-2e6f1c1d1b2f","eabId":"EAB::11111111-2222-3333-4444-555555555555::99999999::1","type":"episode"},"#,
            r#"{"href":"/series/the-handmaids-tale-565d8976-9d26-4e63-866c-40f8a137ce5f","eabId":"EAB::671f8878-bdba-40bd-ae6d-f85c55cecbb4::60873890::3450585","type":"episode","name":"Offred"},"#,
            r#"{"href":"/series/the-handmaids-tale-565d8976-9d26-4e63-866c-40f8a137ce5f","eabId":"EAB::8aad3f97-4531-47ab-b5d3-18daa78da42d::61095299::24055531","type":"episode","name":"June"}"#,
        );
        let link = "https://www.hulu.com/series/the-handmaid%27s-tale-565d8976-9d26-4e63-866c-40f8a137ce5f";
        assert_eq!(
            hulu_launch(page, link).unwrap(),
            Launch {
                params: json!({"contentTarget": "60873890_content"}),
                confidence: Confidence::Unverified,
            }
        );
        assert!(
            hulu_launch(GHOSTBUSTERS_PAGE, link).is_none(),
            "no episodes listed"
        );
    }

    #[test]
    fn hulu_links_without_a_title_id_are_not_fetched() {
        for link in [
            "https://www.hulu.com/hub/movies",
            "https://www.hulu.com/watch/0a7184af-7705-4884-bcef-908c27edb1af",
            "https://www.hulu.com/movie/ghostbusters-0A7184AF-7705-4884-bcef-908c27edb1af",
            "https://www.hulu.com/movie/0a7184af-7705",
            "https://www.hulu.com.evil.example/movie/x-0a7184af-7705-4884-bcef-908c27edb1af",
        ] {
            assert!(hulu_page(link).is_none(), "{link}");
        }
        // Malformed EAB IDs are skipped.
        let page = r#""EAB::0a7184af-7705-4884-bcef-908c27edb1af::12x::3" "EAB::0a7184af-7705-4884-bcef-908c27edb1af::::3""#;
        assert_eq!(eab_ids(page).count(), 0);
    }

    #[test]
    fn apple_tv_links_are_confirmed() {
        let link = "https://tv.apple.com/us/movie/f1-the-movie/umc.cmc.3t6dvnnr87zwd4wmvpdx5came";
        let result = launch("apple", link).unwrap();
        assert_eq!(result.params, json!({"contentTarget": link}));
        assert_eq!(result.confidence, Confidence::Verified);
    }

    #[test]
    fn prime_video_links_become_the_titles_gti() {
        // The Hobbit: The Desolation of Smaug, as Movie of the Night links it.
        let link = "https://www.amazon.com/gp/video/detail/0O2PTZNG3GJQ205Y02FHPY7HGD?ref_=atv_dp";
        assert_eq!(
            launch("prime", link).unwrap(),
            Launch {
                params: json!({"contentTarget": "amzn1.dv.gti.96a9f6e0-1128-c70c-e520-1986b41e8a1d"}),
                confidence: Confidence::Verified,
            }
        );
        // Titles included with Prime carry the GTI itself.
        assert_eq!(
            launch(
                "prime",
                "https://app.primevideo.com/detail?gti=amzn1.dv.gti.72e041dc-573e-4d1d-87a0-c79ddb7004bd"
            )
            .unwrap()
            .params,
            json!({"contentTarget": "amzn1.dv.gti.72e041dc-573e-4d1d-87a0-c79ddb7004bd"})
        );
        for link in [
            "https://app.primevideo.com/detail?gti=amzn1.dv.gti.72E041DC-573e-4d1d-87a0-c79ddb7004bd",
            "https://app.primevideo.com/detail?gti=amzn1.dv.gti.72e041dc",
            "https://app.primevideo.com/detail?gti=amzn1.dv.gti.72e041dc-573e-4d1d-87a0-c79ddb7004bd%22",
        ] {
            assert!(launch("prime", link).is_none(), "{link}");
        }
        // More pairs from amazon.com, including GTIs with leading zero bytes.
        for (compact, gti) in [
            (
                "0F5R4WB5W63RKJCQBDRNVJCQ4O",
                "0011545b-608c-4507-baf9-1f6aabbe0e48",
            ),
            (
                "0FO2OYUWTRL41A3G59F5N4361I",
                "08a9f721-4b59-1944-0a92-71d807cb9156",
            ),
            (
                "0FSC5A2NHS3R0RTLU08QC7XXDD",
                "0aa9f77e-2dfd-6b0a-1e7f-3c33792cde31",
            ),
        ] {
            assert_eq!(
                gti_from_compact(compact).as_deref(),
                Some(format!("amzn1.dv.gti.{gti}").as_str())
            );
        }
        // ASIN-style IDs, wrong lengths, lowercase and out-of-range values open the app plainly.
        for link in [
            "https://www.amazon.com/gp/video/detail/B0FV8FQK6M",
            "https://www.amazon.com/gp/video/detail/0O2PTZNG3GJQ205Y02FHPY7HG",
            "https://www.amazon.com/gp/video/detail/0o2ptzng3gjq205y02fhpy7hgd",
            "https://www.amazon.com/gp/video/detail/00000000000000000000000000",
            "https://www.amazon.com/gp/video/detail/ZZZZZZZZZZZZZZZZZZZZZZZZZZ",
            "https://www.amazon.com/gp/video/storefront",
        ] {
            assert!(launch("prime", link).is_none(), "{link}");
        }
    }

    #[test]
    fn links_off_the_services_own_hosts_or_malformed_are_refused() {
        for (service, link) in [
            ("hulu", "http://www.hulu.com/watch/1"),
            ("hulu", "https://www.hulu.com.evil.example/watch/1"),
            ("hulu", "https://user@www.hulu.com/watch/1"),
            ("hulu", "https://www.hulu.com:8443/watch/1"),
            ("hulu", "https://www.hulu.com/watch/1 2"),
            ("netflix", "https://www.hulu.com/title/1"),
            ("unknown", "https://example.com/"),
        ] {
            assert!(launch(service, link).is_none(), "{service} {link}");
        }
        let long = format!("https://www.hulu.com/{}", "a".repeat(600));
        assert!(launch("hulu", &long).is_none());
    }
}
