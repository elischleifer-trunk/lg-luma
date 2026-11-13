//! The Cameras gallery: a full-screen grid of Home Assistant's cameras, one enlarged camera,
//! and the options and settings sheets. Layout lives in [`crate::cameras`], shared with focus.
use super::*;
use crate::cameras::{CAPTION, GEAR, GRID, Layout, SHEET, SHEET_ROW, SHEET_ROWS, SHEET_TOP, Sheet};

const BACKDROP: u32 = 0x0d1013;
const RADIUS: f32 = 18.;

pub fn render(out: &mut Scene, state: &State, assets: &Assets) {
    let gallery = &state.cameras;
    out.shape(Rect::new(0., 0., 1920., 1080.), 0., c(BACKDROP, 1.));
    if let Some(id) = &gallery.enlarged {
        enlarged(out, state, assets, id);
        return;
    }
    header(out, state, assets);
    grid(out, state);
    if let Some((id, _)) = &gallery.renaming {
        // The keyboard is modal: only its keys take the pointer.
        out.controls.clear();
        if matches!(state.overlay, Some(Overlay::Keyboard)) {
            out.shape(Rect::new(0., 0., 1920., 1080.), 0., c(0x080b12, 0.6));
            keyboard(out, state, assets);
        } else {
            rename(out, state, id);
        }
        return;
    }
    // Like Luma's other popups, a sheet takes every pointer target: hovering the grid does not
    // move focus out of it, and a click outside it closes it.
    if gallery.sheet.is_some() {
        out.controls.clear();
    }
    match &gallery.sheet {
        Some(Sheet::Options(id)) => options(out, state, assets, id),
        Some(Sheet::Settings) => settings(out, state, assets),
        None => {}
    }
}

fn header(out: &mut Scene, state: &State, assets: &Assets) {
    let language = state.preferences.language;
    let gallery = &state.cameras;
    out.text(
        language.t("cameras"),
        Rect::new(96., 58., 900., 52.),
        text(40., 600, -0.02),
        WHITE,
        false,
    );
    let hint = language.t(if gallery.moving {
        "camerasMoveHint"
    } else {
        "camerasHint"
    });
    out.text(
        hint,
        Rect::new(900., 70., GEAR.x - 24. - 900., 28.),
        text(19.2, 500, 0.).right(),
        c(0xf5f5f5, if gallery.moving { 0.9 } else { 0.56 }),
        false,
    );
    let focused = state.focus == Target::CameraGear && gallery.sheet.is_none();
    if focused {
        out.shadow(GEAR, GEAR.h / 2., 30., 10., 0.5);
        out.draw.push(Draw::RoundedGradient(
            GEAR,
            GEAR.h / 2.,
            c(0xfafafa, 1.),
            c(0xe5e5e5, 1.),
        ));
    } else {
        out.shape(GEAR, GEAR.h / 2., c(0xffffff, 0.08));
    }
    out.icon(
        assets,
        "settings",
        GEAR.inset(20.),
        if focused {
            c(0x0d1013, 1.)
        } else {
            c(0xf5f5f5, 0.8)
        },
    );
    out.control(Target::CameraGear, GEAR, false);
}

fn status(out: &mut Scene, label: &str, rect: Rect, size: f32) {
    let style = text(size, 500, 0.);
    let height = style.size * style.line_height;
    out.text(
        label,
        Rect::new(rect.x, rect.y + (rect.h - height) / 2., rect.w, height),
        style,
        c(0xf5f5f5, 0.72),
        true,
    );
}

fn grid(out: &mut Scene, state: &State) {
    let language = state.preferences.language;
    let gallery = &state.cameras;
    let visible = gallery.visible();
    if visible.is_empty() {
        status(out, language.t("camerasAllHidden"), GRID, 28.);
        return;
    }
    let layout = Layout::new(visible.len());
    // Focus rings reach past the tiles; keep them, not scrolled-off rows, inside the clip.
    out.draw.push(Draw::Clip(Some(Rect::new(
        0.,
        GRID.y - 12.,
        1920.,
        GRID.h + 24.,
    ))));
    let rows = gallery.first_row..gallery.first_row + layout.visible_rows;
    for (index, camera) in visible.iter().enumerate() {
        if !rows.contains(&(index / layout.cols)) {
            continue;
        }
        let frame = layout.frame(index, gallery.first_row);
        let focused = state.focus == Target::Camera(camera.id.clone()) && gallery.sheet.is_none();
        if focused {
            out.shadow(frame, RADIUS, 40., 12., 0.5);
        }
        out.shape(frame, RADIUS, c(0x000000, 1.));
        match gallery.frames.get(&camera.id) {
            Some(picture) => {
                out.draw
                    .push(Draw::Camera(picture.clone(), frame, RADIUS, false, [1.; 4]))
            }
            None => status(
                out,
                language.t(if gallery.unavailable.contains(&camera.id) {
                    "cameraUnavailable"
                } else {
                    "cameraConnecting"
                }),
                frame,
                20.8,
            ),
        }
        out.border(frame, RADIUS, 1., c(0xffffff, 0.08));
        if focused && gallery.moving {
            out.border(frame.inset(-4.), RADIUS + 4., 5., c(0x64b5ff, 1.));
        } else if focused {
            out.ring(frame, RADIUS);
        }
        out.text(
            camera.title(),
            Rect::new(frame.x + 4., frame.y + frame.h + 12., frame.w - 8., 28.),
            text(22.4, if focused { 600 } else { 500 }, 0.),
            if focused { WHITE } else { c(0xeeeeee, 0.73) },
            false,
        );
        out.control(
            Target::Camera(camera.id.clone()),
            Rect::new(frame.x, frame.y, frame.w, frame.h + CAPTION),
            false,
        );
    }
    out.draw.push(Draw::Clip(None));
    // More rows above or below: a soft edge says the grid scrolls.
    if gallery.first_row > 0 {
        out.draw.push(Draw::RoundedGradient(
            Rect::new(0., GRID.y - 12., 1920., 40.),
            0.,
            c(BACKDROP, 1.),
            c(BACKDROP, 0.),
        ));
    }
    if gallery.first_row + layout.visible_rows < layout.rows {
        out.draw.push(Draw::RoundedGradient(
            Rect::new(0., GRID.y + GRID.h - 28., 1920., 40.),
            0.,
            c(BACKDROP, 0.),
            c(BACKDROP, 1.),
        ));
    }
}

fn enlarged(out: &mut Scene, state: &State, assets: &Assets, id: &str) {
    let language = state.preferences.language;
    let gallery = &state.cameras;
    let screen = Rect::new(0., 0., 1920., 1080.);
    out.shape(screen, 0., c(0x000000, 1.));
    let picture = gallery.frames.get(id);
    match picture {
        Some(picture) => out
            .draw
            .push(Draw::Camera(picture.clone(), screen, 0., true, [1.; 4])),
        None => status(
            out,
            language.t(if gallery.unavailable.iter().any(|item| item == id) {
                "cameraUnavailable"
            } else {
                "cameraConnecting"
            }),
            screen,
            28.,
        ),
    }
    out.control(Target::Camera(id.into()), screen, false);
    // Measured text plus room for tracking and rounding, and the red dot when live.
    let inner = |label: &str, style: TextStyle, dot: bool| {
        assets.width(label, style).ceil() + 8. + if dot { 22. } else { 0. }
    };
    let pill = |out: &mut Scene, label: &str, x: f32, y: f32, style: TextStyle, dot: bool| {
        let height = style.size * style.line_height;
        let inner = inner(label, style, dot);
        let rect = Rect::new(x, y, inner + 40., height + 20.);
        out.shape(rect, 14., c(0x171c20, 0.78));
        if dot {
            out.shape(
                Rect::new(rect.x + 20., rect.y + rect.h / 2. - 6., 12., 12.),
                6.,
                c(0xff453a, 1.),
            );
        }
        out.text(
            label,
            Rect::new(
                rect.x + 20. + if dot { 22. } else { 0. },
                rect.y + 10.,
                inner,
                height,
            ),
            style,
            WHITE,
            false,
        );
        rect
    };
    if let Some(camera) = gallery.camera(id) {
        let style = text(25.6, 600, 0.);
        pill(out, camera.title(), 96., 1080. - 54. - 52., style, false);
    }
    if let Some(picture) = picture {
        let label = language.t(if picture.live {
            "cameraLive"
        } else {
            "cameraSnapshots"
        });
        let style = text(19.2, 650, 0.06);
        let width = inner(label, style, picture.live) + 40.;
        pill(out, label, 1824. - width, 54., style, picture.live);
    }
}

/// The right-hand sheet both menus use, over the dimmed grid.
fn sheet(out: &mut Scene, title: &str) {
    out.shape(Rect::new(0., 0., 1920., 1080.), 0., c(0x080b12, 0.6));
    out.modal = Some(SHEET);
    out.shadow(SHEET, 0., 80., 0., 0.5);
    out.draw.push(Draw::RoundedGradient(
        SHEET,
        0.,
        c(0x24282d, 1.),
        c(0x1b1f23, 1.),
    ));
    out.text(
        title,
        Rect::new(SHEET.x + 48., 72., SHEET.w - 96., 44.),
        text(32., 600, -0.02),
        WHITE,
        false,
    );
}

struct SheetRow<'a> {
    target: Target,
    label: &'a str,
    icon: Option<&'static str>,
    value: Option<(&'a str, bool)>,
    y: f32,
}

fn sheet_row(out: &mut Scene, state: &State, assets: &Assets, row: SheetRow<'_>) {
    let rect = Rect::new(SHEET.x + 32., row.y, SHEET.w - 64. - 64., 67.2);
    let focused = state.focus == row.target;
    let color = if focused { c(0x242424, 1.) } else { WHITE };
    out.shape(rect, 12., c(0xffffff, if focused { 0.96 } else { 0.10 }));
    let style = text(20.8, 600, -0.02);
    let height = style.size * style.line_height;
    let value_width = row.value.map_or(0., |(value, _)| {
        assets.width(value, text(16.8, 650, 0.)) + 28.
    });
    out.text(
        row.label,
        Rect::new(
            rect.x + 24.,
            rect.y + (rect.h - height) / 2.,
            rect.w - 72. - value_width,
            height,
        ),
        style,
        color,
        false,
    );
    if let Some(icon) = row.icon {
        out.icon(
            assets,
            icon,
            Rect::new(rect.x + rect.w - 45.6, rect.y + 22.8, 21.6, 21.6),
            color,
        );
    }
    if let Some((value, on)) = row.value {
        let pill = Rect::new(
            rect.x + rect.w - 20. - value_width,
            rect.y + 18.,
            value_width,
            31.2,
        );
        out.shape(
            pill,
            10.,
            if on {
                c(0x2f8f4e, 1.)
            } else if focused {
                c(0x000000, 0.12)
            } else {
                c(0xffffff, 0.12)
            },
        );
        out.text(
            value,
            Rect::new(pill.x, pill.y + 5., pill.w, 21.),
            text(16.8, 650, 0.),
            if on || !focused {
                WHITE
            } else {
                c(0x242424, 1.)
            },
            true,
        );
    }
    out.control(row.target, rect, false);
}

fn options(out: &mut Scene, state: &State, assets: &Assets, id: &str) {
    let language = state.preferences.language;
    let name = state.cameras.camera(id).map_or(id, |camera| camera.title());
    sheet(out, name);
    for (index, target) in state.cameras.options().into_iter().enumerate() {
        let (label, icon) = match target {
            Target::CameraMove => (language.t("move"), "grid"),
            Target::CameraRename => (language.t("cameraRename"), "type"),
            Target::CameraNameReset => (language.t("cameraNameReset"), "refresh"),
            Target::CameraHide => (language.t("cameraHide"), "eye-off"),
            _ => (language.t("cameraSettings"), "settings"),
        };
        sheet_row(
            out,
            state,
            assets,
            SheetRow {
                target,
                label,
                icon: Some(icon),
                value: None,
                y: SHEET_TOP + index as f32 * SHEET_ROW,
            },
        );
    }
}

fn settings(out: &mut Scene, state: &State, assets: &Assets) {
    let language = state.preferences.language;
    let gallery = &state.cameras;
    sheet(out, language.t("cameraSettings"));
    let rows = gallery.settings();
    let first = gallery.sheet_first.min(rows.len().saturating_sub(1));
    // Camera rows sit below a section label.
    let section = 44.;
    for (index, target) in rows.into_iter().enumerate().skip(first).take(SHEET_ROWS) {
        let mut y = SHEET_TOP + (index - first) as f32 * SHEET_ROW;
        if index >= 1 {
            y += section;
        }
        if index == 1 || (index == first && index > 1) {
            out.text(
                language.t("camerasShownSection"),
                Rect::new(SHEET.x + 56., y - 32., SHEET.w - 112., 20.),
                text(15.2, 650, 0.08),
                c(0xf5f5f5, 0.5),
                false,
            );
        }
        let row = match &target {
            Target::CameraShown(id) => {
                let Some(camera) = gallery.camera(id) else {
                    continue;
                };
                SheetRow {
                    label: camera.title(),
                    icon: None,
                    value: Some(if camera.hidden {
                        (language.t("hiddenLabel"), false)
                    } else {
                        (language.t("cameraShown"), true)
                    }),
                    target: target.clone(),
                    y,
                }
            }
            _ => SheetRow {
                label: language.t("cameraLiveVideo"),
                icon: None,
                value: Some(if gallery.live {
                    (language.t("stateOn"), true)
                } else {
                    (language.t("stateOff"), false)
                }),
                target: target.clone(),
                y,
            },
        };
        sheet_row(out, state, assets, row);
    }
}

/// Renaming with the TV's own keyboard: a sheet shows the name as it is typed.
fn rename(out: &mut Scene, state: &State, id: &str) {
    let language = state.preferences.language;
    let gallery = &state.cameras;
    sheet(out, language.t("cameraName"));
    let field = Rect::new(SHEET.x + 32., SHEET_TOP, SHEET.w - 128., 67.2);
    out.shape(field, 12., c(0xffffff, 0.96));
    let style = text(20.8, 600, -0.02);
    let height = style.size * style.line_height;
    let draft = &gallery.name_draft;
    let (label, color) = if draft.is_empty() {
        (
            gallery
                .camera(id)
                .map_or(id, |camera| camera.title())
                .to_owned(),
            c(0x242424, 0.45),
        )
    } else {
        (format!("{draft}|"), c(0x242424, 1.))
    };
    out.text(
        label,
        Rect::new(
            field.x + 24.,
            field.y + (field.h - height) / 2.,
            field.w - 48.,
            height,
        ),
        style,
        color,
        false,
    );
}
