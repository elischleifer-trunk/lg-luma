//! The Criterion Channel screen: the Channel's rows of films as 16:9 cards, gliding to keep the
//! focused card on screen, and a film's detail page with Play. State and navigation live in
//! [`crate::criterion`].
use super::*;
use crate::criterion::{Detail, IMAGE_SIZE, VISIBLE_CARDS};

const BACKDROP: u32 = 0x0d1013;
const RADIUS: f32 = 14.;
const LEFT: f32 = 96.;
/// The first row's heading.
const TOP: f32 = 150.;
const CARD_W: f32 = IMAGE_SIZE.0 as f32;
const CARD_H: f32 = IMAGE_SIZE.1 as f32;
const GAP: f32 = 32.;
const HEADING: f32 = 48.;
const CAPTION: f32 = 72.;
const ROW_H: f32 = HEADING + CARD_H + CAPTION;

/// Where a card is drawn, given where its row is scrolled to (in cards) and the row's top.
fn card_rect(card: usize, scroll: f32, top: f32) -> Rect {
    let place = card as f32 - scroll;
    Rect::new(LEFT + place * (CARD_W + GAP), top + HEADING, CARD_W, CARD_H)
}

pub fn render(out: &mut Scene, state: &State, assets: &Assets) {
    let language = state.preferences.language;
    let browser = &state.criterion;
    out.shape(Rect::new(0., 0., 1920., 1080.), 0., c(BACKDROP, 1.));
    if let Some(detail) = &browser.detail {
        detail_page(out, state, assets, detail);
        return;
    }
    let title_style = text(40., 600, -0.02);
    out.text(
        "Criterion Channel",
        Rect::new(LEFT, 58., 900., 52.),
        title_style,
        WHITE,
        false,
    );
    if !browser.rows.is_empty() || browser.searching() {
        let field = search_field(
            out,
            state,
            assets,
            LEFT + assets.width("Criterion Channel", title_style) + 48.,
        );
        if !browser.rows.is_empty() {
            let hint_x = field.x + field.w + 40.;
            out.text(
                language.t("criterionHint"),
                Rect::new(hint_x, 70., 1920. - LEFT - hint_x, 28.),
                text(19.2, 500, 0.).right(),
                c(0xf5f5f5, 0.56),
                false,
            );
            rows(out, state);
        } else {
            message(
                out,
                &language.format("criterionNoResults", &[("query", browser.query.trim())]),
            );
        }
        typing(out, state, assets);
        return;
    }
    if browser.loading || browser.error.is_none() {
        message(out, language.t("criterionLoading"));
        return;
    }
    message(out, language.t("criterionError"));
    let label = language.t("retry");
    let width = assets.width(label, text(21., 550, -0.01)) + 74.;
    let rect = Rect::new((1920. - width) / 2., 580., width, 60.);
    let focused = state.focus == Target::CriterionRetry;
    out.pill(rect, focused, false);
    let color = if focused {
        c(0x20232a, 1.)
    } else {
        c(0xffffff, 0.8)
    };
    out.icon(
        assets,
        "refresh",
        Rect::new(rect.x + 20., rect.y + 19., 22., 22.),
        color,
    );
    out.text(
        label,
        Rect::new(rect.x + 54., rect.y + 16.5, rect.w - 74., 32.),
        text(21., 550, -0.01),
        color,
        false,
    );
    out.control(Target::CriterionRetry, rect, false);
}

/// The search field in the header, after the title: what is typed, or what it searches. OK
/// brings up the keyboard. Returns where it is drawn.
fn search_field(out: &mut Scene, state: &State, assets: &Assets, x: f32) -> Rect {
    let browser = &state.criterion;
    let rect = Rect::new(x, 54., 560., 60.);
    let focused = state.focus == Target::CriterionSearch && browser.typing.is_none();
    out.pill(rect, focused, browser.typing.is_some());
    let ink = if focused {
        c(0x20232a, 1.)
    } else {
        c(0xffffff, 0.8)
    };
    out.icon(
        assets,
        "search",
        Rect::new(rect.x + 22., rect.y + 19., 22., 22.),
        ink,
    );
    let style = text(22., 500, 0.);
    let (label, color) = if browser.query.is_empty() && browser.typing.is_none() {
        (
            state.preferences.language.t("criterionSearch").to_owned(),
            if focused {
                c(0x20232a, 0.7)
            } else {
                c(0xffffff, 0.5)
            },
        )
    } else if browser.typing.is_some() {
        (format!("{}|", browser.query), WHITE)
    } else {
        (browser.query.clone(), ink)
    };
    let width = rect.w - 84.;
    out.text(
        clamp_lines(assets, &label, style, width, 1),
        Rect::new(rect.x + 58., rect.y + 15., width, 32.),
        style,
        color,
        false,
    );
    out.control(Target::CriterionSearch, rect, false);
    rect
}

/// Luma's own keyboard over the screen, while typing a search off the TV. On the TV, LG's
/// keyboard covers the bottom of the screen and the results show above it as they change.
fn typing(out: &mut Scene, state: &State, assets: &Assets) {
    if state.criterion.typing.is_none() {
        return;
    }
    // The keyboard is modal: only its keys take the pointer.
    out.controls.clear();
    if matches!(state.overlay, Some(Overlay::Keyboard)) {
        out.shape(Rect::new(0., 0., 1920., 1080.), 0., c(0x080b12, 0.6));
        keyboard(out, state, assets);
    }
}

fn message(out: &mut Scene, label: &str) {
    out.text(
        label,
        Rect::new(LEFT, 480., 1920. - 2. * LEFT, 40.),
        text(28., 500, 0.),
        c(0xf5f5f5, 0.72),
        true,
    );
}

fn rows(out: &mut Scene, state: &State) {
    let browser = &state.criterion;
    // Focus rings reach past the cards; keep them, not the header, inside the clip.
    out.draw.push(Draw::Clip(Some(Rect::new(
        0.,
        TOP - 12.,
        1920.,
        1080. - TOP + 12.,
    ))));
    // The row above the top one may still be sliding off.
    let from = browser.scroll_row.floor().max(0.) as usize;
    for (index, row) in browser.rows.iter().enumerate().skip(from) {
        let top = TOP + (index as f32 - browser.scroll_row) * ROW_H;
        if top >= 1080. {
            break;
        }
        if top + ROW_H <= TOP - 12. {
            continue;
        }
        out.text(
            row.title.as_str(),
            Rect::new(LEFT, top, 1920. - 2. * LEFT, 34.),
            text(26., 600, -0.01),
            c(0xffffff, 0.9),
            false,
        );
        let scroll = browser.scroll_card(index);
        let first = scroll.floor().max(0.) as usize;
        // One card past the last full one shows the row goes on.
        let shown = first.saturating_sub(1)..(first + VISIBLE_CARDS + 2).min(row.films.len());
        for card in shown {
            let film = &row.films[card];
            let rect = card_rect(card, scroll, top);
            if rect.x + rect.w < 0. || rect.x > 1920. {
                continue;
            }
            let focused = state.focus == Target::Film(index, card);
            if focused {
                out.shadow(rect, RADIUS, 40., 12., 0.5);
            }
            out.shape(rect, RADIUS, c(0x1b1f24, 1.));
            match browser.images.get(&film.id) {
                Some(image) => {
                    let shade = if focused { 1. } else { 0.82 };
                    out.draw.push(Draw::Camera(
                        image.clone(),
                        rect,
                        RADIUS,
                        false,
                        [shade, shade, shade, 1.],
                    ));
                }
                // Loading or no artwork: the title on a plain card.
                _ => out.text(
                    film.title.as_str(),
                    Rect::new(rect.x + 20., rect.y + rect.h / 2. - 16., rect.w - 40., 32.),
                    text(22., 600, 0.),
                    c(0xffffff, 0.7),
                    true,
                ),
            }
            out.border(rect, RADIUS, 1., c(0xffffff, 0.08));
            if focused {
                out.ring(rect, RADIUS);
            }
            out.text(
                film.title.as_str(),
                Rect::new(rect.x + 4., rect.y + rect.h + 14., rect.w - 8., 28.),
                text(21., if focused { 600 } else { 500 }, 0.),
                if focused { WHITE } else { c(0xeeeeee, 0.73) },
                false,
            );
            let details = film.details();
            if !details.is_empty() {
                out.text(
                    details,
                    Rect::new(rect.x + 4., rect.y + rect.h + 44., rect.w - 8., 24.),
                    text(17., 450, 0.),
                    c(0xeeeeee, if focused { 0.7 } else { 0.5 }),
                    false,
                );
            }
            out.control(
                Target::Film(index, card),
                Rect::new(rect.x, rect.y, rect.w, rect.h + CAPTION),
                false,
            );
        }
    }
    out.draw.push(Draw::Clip(None));
}

// ---- Detail page ----------------------------------------------------------------------------

/// The text column's width, over the backdrop's darkened left side.
const DETAIL_W: f32 = 860.;

/// A film's page, after the Channel's own (director above the title, year and running time,
/// the synopsis, then who stars) laid out like HBO Max's and Netflix's title pages on TV: the
/// film's still across the screen, darkened behind the text, and Play first.
fn detail_page(out: &mut Scene, state: &State, assets: &Assets, detail: &Detail) {
    let language = state.preferences.language;
    let from = out.draw.len();
    let film = &detail.film;
    let screen = Rect::new(0., 0., 1920., 1080.);
    // The backdrop, or the card's smaller artwork until it arrives.
    let still = detail
        .hero
        .as_ref()
        .or_else(|| state.criterion.images.get(&film.id));
    if let Some(image) = still {
        out.draw.push(Draw::Camera(
            image.clone(),
            screen,
            0.,
            false,
            [0.9, 0.9, 0.9, 1.],
        ));
    }
    // Shade the left side for the text, and the bottom edge into the backdrop colour.
    side_scrim(out, 0., 1340., 0.96, 0.);
    out.draw.push(Draw::RoundedGradient(
        Rect::new(0., 620., 1920., 460.),
        0.,
        c(BACKDROP, 0.),
        c(BACKDROP, 0.94),
    ));
    out.draw.push(Draw::RoundedGradient(
        Rect::new(0., 0., 1920., 220.),
        0.,
        c(BACKDROP, 0.55),
        c(BACKDROP, 0.),
    ));
    out.draw.push(Draw::TintedImage(
        assets.path("logos/criterion.lvec"),
        Rect::new(LEFT, 60., 46., 46.),
        0.,
        true,
        c(0xffffff, 0.92),
    ));

    let details = detail.details.as_ref();
    let x = LEFT;
    let title_style = text(76., 650, -0.025).leading(1.05);
    let title_lines = assets.lines(&film.title, title_style, DETAIL_W).min(2);
    let title_h = title_lines as f32 * title_style.size * title_style.line_height;
    // The block ends near the bottom; the title sits higher when it takes two lines.
    let mut y = 330. - (title_h - 80.).max(0.);

    // Director, small and spaced, above the title as on the Channel.
    if let Some(director) = details.filter(|d| !d.directors.is_empty()) {
        out.text(
            director.directors.join(", ").to_uppercase(),
            Rect::new(x, y, DETAIL_W, 30.),
            text(21., 600, 0.14),
            c(0xffffff, 0.72),
            false,
        );
    }
    y += 44.;
    out.text(
        clamp_lines(assets, &film.title, title_style, DETAIL_W, 2),
        Rect::new(x, y, DETAIL_W, title_h + 8.),
        title_style,
        WHITE,
        false,
    );
    y += title_h + 22.;

    // Year · running time · genres · country.
    let year = Some(film.year.as_str())
        .filter(|year| !year.is_empty())
        .or(details.map(|d| d.year.as_str()));
    let length = Some(film.length.as_str())
        .filter(|length| !length.is_empty())
        .or(details.map(|d| d.length.as_str()));
    let mut facts: Vec<String> = [year, length]
        .into_iter()
        .flatten()
        .filter(|fact| !fact.is_empty())
        .map(str::to_owned)
        .collect();
    if let Some(details) = details {
        if !details.genres.is_empty() {
            facts.push(
                details
                    .genres
                    .iter()
                    .take(2)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        if let Some(country) = details.countries.first() {
            facts.push(country.clone());
        }
    }
    out.text(
        facts.join("  ·  "),
        Rect::new(x, y, DETAIL_W, 32.),
        text(23., 500, 0.),
        c(0xffffff, 0.78),
        false,
    );
    y += 64.;

    // Play: the white button streaming services lead with.
    let label = language.t("play");
    let style = text(24., 650, -0.01);
    let width = assets.width(label, style) + 128.;
    let play = Rect::new(x, y, width.max(210.), 68.);
    let focused = state.focus == Target::CriterionPlay;
    if focused {
        out.shadow(play, 14., 30., 8., 0.45);
    }
    out.shape(
        play,
        14.,
        if focused {
            c(0xffffff, 1.)
        } else {
            c(0xffffff, 0.16)
        },
    );
    if focused {
        out.ring(play, 14.);
    }
    let ink = if focused { c(0x111317, 1.) } else { WHITE };
    out.icon(
        assets,
        "play",
        Rect::new(play.x + 34., play.y + 20., 28., 28.),
        ink,
    );
    out.text(
        label,
        Rect::new(play.x + 76., play.y + 18., play.w - 96., 34.),
        style,
        ink,
        false,
    );
    out.control(Target::CriterionPlay, play, false);
    y += play.h + 44.;

    // The synopsis, then who stars in it.
    let body = text(24., 450, 0.).leading(1.42);
    match details {
        Some(details) if !details.synopsis.is_empty() => {
            let lines = if details.cast.is_empty() { 6 } else { 5 };
            let synopsis = clamp_lines(assets, &details.synopsis, body, DETAIL_W, lines);
            let shown = assets.lines(&synopsis, body, DETAIL_W).min(lines);
            let h = shown as f32 * body.size * body.line_height;
            out.text(
                synopsis,
                Rect::new(x, y, DETAIL_W, h + 4.),
                body,
                c(0xffffff, 0.84),
                false,
            );
            y += h + 22.;
        }
        _ if detail.details.is_none() && !detail.failed => {
            // Loading: faint lines where the synopsis will be.
            for (line, w) in [1., 0.94, 0.62].into_iter().enumerate() {
                out.shape(
                    Rect::new(x, y + 8. + line as f32 * 34., DETAIL_W * w, 16.),
                    8.,
                    c(0xffffff, 0.07),
                );
            }
        }
        _ => {}
    }
    if let Some(details) = details.filter(|d| !d.cast.is_empty()) {
        let label = language.t("starring");
        let label_style = text(21., 600, 0.);
        let label_w = assets.width(label, label_style) + 12.;
        out.text(
            label,
            Rect::new(x, y + 1., label_w, 30.),
            label_style,
            c(0xffffff, 0.5),
            false,
        );
        out.text(
            clamp_lines(
                assets,
                &details.cast.join(", "),
                text(21., 500, 0.),
                DETAIL_W - label_w,
                1,
            ),
            Rect::new(x + label_w, y + 1., DETAIL_W - label_w, 30.),
            text(21., 500, 0.),
            c(0xffffff, 0.78),
            false,
        );
    }
    if detail.shown < 1. {
        // Ease out: quick at first, settling gently.
        let t = 1. - (1. - detail.shown).powi(3);
        out.fade(from, t, 1.);
    }
}

/// A horizontal fade in the backdrop colour from `alpha_from` at `x0` to `alpha_to` at `x1`,
/// drawn as narrow bands: the renderer's gradients run top to bottom.
fn side_scrim(out: &mut Scene, x0: f32, x1: f32, alpha_from: f32, alpha_to: f32) {
    // Whole-pixel bands that neither overlap nor leave gaps once snapped.
    const BAND: f32 = 20.;
    let bands = ((x1 - x0) / BAND).ceil() as usize;
    for index in 0..bands {
        let t = (index as f32 + 0.5) / bands as f32;
        // Smoothstep, so the fade has no visible edge at either end.
        let eased = t * t * (3. - 2. * t);
        let alpha = alpha_from + (alpha_to - alpha_from) * eased;
        out.shape(
            Rect::new(x0 + index as f32 * BAND, 0., BAND, 1080.),
            0.,
            c(BACKDROP, alpha),
        );
    }
}
