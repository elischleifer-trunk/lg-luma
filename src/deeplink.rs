//! Launch parameters that open a title inside a streaming app, built from the web link Movie of
//! the Night returns for that service. Each app reads its own format, passed as webOS launch
//! `params` (see `platform::launch_with_params`). A service without a known format, or a link
//! that fails validation, opens the app's home screen instead.
//!
//! Formats marked `Unverified` are best-known community formats that have not been confirmed on
//! a TV. Confirm them with `pnpm deeplink:probe` (tools/deeplink-probe.mjs) and flip them to
//! `Verified` here; the probe's candidate list is where alternatives are tried.
use serde_json::{Value, json};

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
        "disney" | "hbo" | "hulu" | "peacock" | "paramount" => {
            unverified(json!({ "contentTarget": link }))
        }
        // Prime Video's appinfo.json declares `{"contentTarget":"$CONTENTID"}`, and LG's own
        // search fills it with the title's GTI (`amzn1.dv.gti.<uuid>`), captured with
        // ls-monitor on a TV (2026-11). A GTI converted from a Movie of the Night link opened
        // the title on a TV; web links and the compact ID open its home screen.
        "prime" => {
            let compact = segment_after(path, &["detail", "dp"])?;
            Some(Launch {
                params: json!({ "contentTarget": gti_from_compact(compact)? }),
                confidence: Confidence::Verified,
            })
        }
        _ => None,
    }
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
        let link = "https://www.hulu.com/series/ghosts-2b0a7c1e-91ab-4c7e-9a3a-2e6f1c1d1b2f";
        let result = launch("hulu", link).unwrap();
        assert_eq!(result.params, json!({"contentTarget": link}));
        assert_eq!(result.confidence, Confidence::Unverified);
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
