//! The Criterion Channel screen: the Channel's rows of films as 16:9 cards, the focused row at
//! the top. State and navigation live in [`crate::criterion`].
use super::*;
use crate::criterion::{IMAGE_SIZE, VISIBLE_CARDS};

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

/// Where a card is drawn, given the first card on screen in its row and the row's top.
fn card_rect(card: usize, first_card: usize, top: f32) -> Rect {
    let place = card as f32 - first_card as f32;
    Rect::new(LEFT + place * (CARD_W + GAP), top + HEADING, CARD_W, CARD_H)
}

pub fn render(out: &mut Scene, state: &State, assets: &Assets) {
    let language = state.preferences.language;
    let browser = &state.criterion;
    out.shape(Rect::new(0., 0., 1920., 1080.), 0., c(BACKDROP, 1.));
    out.text(
        "Criterion Channel",
        Rect::new(LEFT, 58., 900., 52.),
        text(40., 600, -0.02),
        WHITE,
        false,
    );
    if !browser.rows.is_empty() {
        out.text(
            language.t("criterionHint"),
            Rect::new(900., 70., 1920. - LEFT - 900., 28.),
            text(19.2, 500, 0.).right(),
            c(0xf5f5f5, 0.56),
            false,
        );
        rows(out, state);
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
    for (index, row) in browser.rows.iter().enumerate().skip(browser.first_row) {
        let top = TOP + (index - browser.first_row) as f32 * ROW_H;
        if top >= 1080. {
            break;
        }
        out.text(
            row.title.as_str(),
            Rect::new(LEFT, top, 1920. - 2. * LEFT, 34.),
            text(26., 600, -0.01),
            c(0xffffff, 0.9),
            false,
        );
        let first = browser.first_card(index);
        // One card past the last full one shows the row goes on.
        let shown = first.saturating_sub(1)..(first + VISIBLE_CARDS + 1).min(row.films.len());
        for card in shown {
            let film = &row.films[card];
            let rect = card_rect(card, first, top);
            if rect.x + rect.w < 0. || rect.x > 1920. {
                continue;
            }
            let focused = state.focus == Target::Film(index, card);
            if focused {
                out.shadow(rect, RADIUS, 40., 12., 0.5);
            }
            out.shape(rect, RADIUS, c(0x1b1f24, 1.));
            match browser.images.get(&film.id) {
                Some(Some(image)) => {
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
