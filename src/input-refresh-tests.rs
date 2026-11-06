use super::{controls, ready, scene};
use crate::{
    i18n::Language,
    input_refresh::Refresh,
    model::{InputMode, Preferences, demo_inventory},
    platform::normalize_inputs,
    renderer::Draw,
    state::{Overlay, Subpage, Target, WIDGETS_SECTION},
    view::input_status,
};
use serde_json::json;
use std::time::{Duration, Instant};

#[test]
fn signal_never_overrides_the_cable_state_and_auto_hides_sources_without_signal() {
    let cases = [
        (
            json!({"connected":true,"hdmiPlugIn":false,"hdmiSignalExist":true,"activate":true}),
            Some(false),
            Some(true),
            "noDevice",
        ),
        (
            json!({"connected":false,"hdmiPlugIn":true,"hdmiSignalExist":true}),
            Some(true),
            Some(true),
            "connected",
        ),
        (
            json!({"connected":true,"hdmiPlugIn":true,"hdmiSignalExist":false,"activate":false}),
            Some(true),
            Some(false),
            "noSignal",
        ),
        (
            json!({"hdmiPlugIn":true,"hdmiSignalExist":false,"activate":true}),
            Some(true),
            Some(false),
            "noSignal",
        ),
        (
            json!({"connected":true,"hdmiSignalExist":false}),
            Some(true),
            Some(false),
            "noSignal",
        ),
        (
            json!({"connected":false,"hdmiPlugIn":"false","hdmiSignalExist":true}),
            Some(false),
            Some(true),
            "noDevice",
        ),
        (
            json!({"hdmiSignalExist":true}),
            None,
            Some(true),
            "externalInput",
        ),
        (
            json!({"hdmiPlugIn":true,"hdmiSignalExist":"false"}),
            Some(true),
            None,
            "connected",
        ),
        (json!({"hdmiPlugIn":true}), Some(true), None, "connected"),
        (json!({"connected":false}), Some(false), None, "noDevice"),
        (json!({}), None, None, "externalInput"),
    ];
    for (mut raw, cable, signal, key) in cases {
        raw["appId"] = json!("com.webos.app.hdmi1");
        let input = normalize_inputs(&json!([raw])).unwrap().remove(0);
        assert_eq!((input.connected, input.signal), (cable, signal));
        let mut preferences = Preferences::default();
        // Auto gives a card only to a plugged-in source that is not reporting "no signal".
        assert_eq!(
            preferences.input_visible(&input),
            cable == Some(true) && signal != Some(false)
        );
        for language in Language::ALL {
            assert_eq!(input_status(&input, false, language), language.t(key));
        }
        preferences
            .input_modes
            .insert(input.id.clone(), InputMode::On);
        assert!(preferences.input_visible(&input));
        preferences
            .input_modes
            .insert(input.id.clone(), InputMode::Off);
        assert!(!preferences.input_visible(&input));
    }
}

#[test]
fn live_signal_keeps_card_geometry_names_apps_and_preferences() {
    let mut state = ready();
    let id = state.inventory.inputs[0].id.clone();
    state.preferences.set_input_name(&id, "Mac");
    // Pinned On, the card stays through a signal change and only its badge updates.
    state
        .preferences
        .input_modes
        .insert(id.clone(), InputMode::On);
    state.focus = Target::App(id.clone());
    let previous = controls(&state);
    let preferences = serde_json::to_value(&state.preferences).unwrap();
    let apps = serde_json::to_value(&state.inventory.apps).unwrap();
    let mut inputs = state.inventory.inputs.clone();
    inputs[0].signal = Some(false);
    state.set_inputs(inputs.clone());
    assert_eq!(state.focus, Target::App(id));
    let next = controls(&state);
    for before in &previous {
        let after = next.iter().find(|c| c.target == before.target).unwrap();
        assert_eq!(
            (before.rect.x, before.rect.y, before.rect.w, before.rect.h),
            (after.rect.x, after.rect.y, after.rect.w, after.rect.h)
        );
    }
    assert!(
        scene(&state)
            .draw
            .iter()
            .any(|d| matches!(d, Draw::Text(t, ..) if t.ends_with("HDMI 1 · No signal")))
    );
    state.open(2);
    state.section = WIDGETS_SECTION;
    state.subpage = Some(Subpage::Inputs);
    inputs[0].signal = Some(true);
    state.set_inputs(inputs.clone());
    assert!(
        scene(&state)
            .draw
            .iter()
            .any(|d| matches!(d, Draw::Text(t, ..) if t.ends_with("HDMI 1 · Connected")))
    );
    assert_eq!(
        serde_json::to_value(&state.preferences).unwrap(),
        preferences
    );
    assert_eq!(serde_json::to_value(&state.inventory.apps).unwrap(), apps);
    state.dirty = false;
    state.set_inputs(inputs);
    assert!(!state.dirty, "unchanged polls must not redraw");
}

#[test]
fn disconnect_reconciles_home_focus_and_preserves_visibility_overrides() {
    let mut state = ready();
    let id = state.inventory.inputs[0].id.clone();
    state.focus = Target::App(id.clone());
    let mut inputs = state.inventory.inputs.clone();
    inputs[0].connected = Some(false);
    inputs[0].signal = Some(true);
    state.set_inputs(inputs.clone());
    assert_ne!(state.focus, Target::App(id.clone()));
    assert!(
        !controls(&state)
            .iter()
            .any(|c| c.target == Target::App(id.clone()))
    );
    state
        .preferences
        .input_modes
        .insert(id.clone(), InputMode::On);
    assert!(
        controls(&state)
            .iter()
            .any(|c| c.target == Target::App(id.clone()))
    );
    state
        .preferences
        .input_modes
        .insert(id.clone(), InputMode::Off);
    inputs[0].connected = Some(true);
    state.set_inputs(inputs);
    assert!(
        !controls(&state)
            .iter()
            .any(|c| c.target == Target::App(id.clone()))
    );
}

#[test]
fn a_removed_port_restores_valid_input_settings_focus() {
    for detail in [false, true] {
        let mut state = ready();
        let id = state.inventory.inputs[0].id.clone();
        state.open(2);
        state.section = WIDGETS_SECTION;
        state.subpage = Some(if detail {
            Subpage::Input(id.clone())
        } else {
            Subpage::Inputs
        });
        state.focus = if detail {
            Target::Choice(2)
        } else {
            Target::InputMode(id)
        };
        let mut inputs = state.inventory.inputs.clone();
        inputs.remove(0);
        let next = inputs[0].id.clone();
        state.set_inputs(inputs);
        assert_eq!(state.subpage, Some(Subpage::Inputs));
        assert_eq!(state.focus, Target::InputMode(next));
        state.set_inputs(vec![]);
        assert_eq!(state.focus, Target::Sidebar(WIDGETS_SECTION));
    }
}

#[test]
fn polling_only_runs_on_visible_focused_tv_input_screens_outside_interactions() {
    let mut state = ready();
    let active = |s: &crate::state::State| crate::input_refresh_active(s, true, true, true);
    assert!(active(&state));
    assert!(!crate::input_refresh_active(&state, false, true, true));
    assert!(!crate::input_refresh_active(&state, true, false, true));
    assert!(!crate::input_refresh_active(&state, true, true, false));
    state.loading = true;
    assert!(!active(&state));
    state.loading = false;
    state.overlay = Some(Overlay::Recents {
        restore: Target::Nav(0),
    });
    assert!(!active(&state));
    state.overlay = None;
    state.recents.visibility = 0.5;
    assert!(!active(&state));
    state.recents.visibility = 0.;
    state.moving = Some(state.inventory.apps[0].id.clone());
    assert!(!active(&state));
    state.moving = None;
    state.pressed = Some(Target::App(state.inventory.inputs[0].id.clone()));
    assert!(!active(&state));
    state.pressed = None;
    state.open(1);
    assert!(!active(&state));
    state.open(2);
    assert!(!active(&state));
    state.subpage = Some(Subpage::Inputs);
    assert!(active(&state));
    state.subpage = Some(Subpage::Input(state.inventory.inputs[0].id.clone()));
    assert!(active(&state));
    state.subpage = Some(Subpage::Clock);
    assert!(!active(&state));
}

#[test]
fn slow_reads_are_bounded_and_errors_preserve_the_last_valid_snapshot() {
    let now = Instant::now();
    let mut refresh = Refresh::default();
    assert!(!refresh.due(now, true));
    assert!(refresh.due(now + Duration::from_secs(2), true));
    assert!(!refresh.due(now + Duration::from_secs(60), true));
    let inputs = demo_inventory().inputs;
    refresh
        .completed(now + Duration::from_secs(60), Ok(inputs.clone()))
        .unwrap();
    assert!(!refresh.due(now + Duration::from_secs(61), true));
    assert!(refresh.due(now + Duration::from_secs(62), true));
    assert!(
        refresh
            .completed(now + Duration::from_secs(63), Err("unavailable".into()))
            .is_err()
    );
    assert!(!refresh.due(now + Duration::from_secs(72), true));
    assert_eq!(refresh.take_pending(true).unwrap(), inputs);
    assert!(refresh.due(now + Duration::from_secs(73), true));
}

#[test]
fn snapshots_wait_for_release_and_yield_to_a_newer_full_inventory() {
    let now = Instant::now();
    let mut refresh = Refresh::default();
    let mut inputs = demo_inventory().inputs;
    refresh.completed(now, Ok(inputs.clone())).unwrap();
    assert!(refresh.take_pending(false).is_none());
    inputs[0].connected = Some(false);
    refresh.completed(now, Ok(inputs.clone())).unwrap();
    assert_eq!(refresh.take_pending(true).unwrap(), inputs);
    assert!(refresh.take_pending(true).is_none());
    refresh.completed(now, Ok(inputs)).unwrap();
    refresh.superseded(now);
    assert!(refresh.take_pending(true).is_none());
    assert!(!refresh.due(now + Duration::from_secs(1), true));
    assert!(refresh.due(now + Duration::from_secs(2), true));
}

#[test]
fn a_disconnect_snapshot_cannot_move_the_pressed_card_before_release() {
    let mut state = ready();
    let id = state.inventory.inputs[0].id.clone();
    state.pressed = Some(Target::App(id.clone()));
    let mut inputs = state.inventory.inputs.clone();
    inputs[0].connected = Some(false);
    let mut refresh = Refresh::default();
    refresh.completed(Instant::now(), Ok(inputs)).unwrap();
    assert!(
        refresh
            .take_pending(crate::input_refresh_active(&state, true, true, true))
            .is_none()
    );
    assert!(
        controls(&state)
            .iter()
            .any(|c| c.target == Target::App(id.clone()))
    );
    state.pressed = None;
    state.set_inputs(
        refresh
            .take_pending(crate::input_refresh_active(&state, true, true, true))
            .unwrap(),
    );
    assert!(
        !controls(&state)
            .iter()
            .any(|c| c.target == Target::App(id.clone()))
    );
}

#[test]
fn hidden_screens_cancel_scheduled_reads_without_duplicating_in_flight_requests() {
    let now = Instant::now();
    let mut refresh = Refresh::default();
    assert!(!refresh.due(now, false));
    assert!(!refresh.due(now + Duration::from_secs(10), true));
    assert!(!refresh.due(now + Duration::from_secs(11), false));
    assert!(!refresh.due(now + Duration::from_secs(20), true));
    assert!(refresh.due(now + Duration::from_secs(22), true));
    assert!(!refresh.due(now + Duration::from_secs(30), false));
    assert!(!refresh.due(now + Duration::from_secs(40), true));
    refresh
        .completed(now + Duration::from_secs(41), Ok(vec![]))
        .unwrap();
    assert!(!refresh.due(now + Duration::from_secs(42), true));
    assert!(refresh.due(now + Duration::from_secs(43), true));
}

#[test]
fn consoles_are_recognized_from_spd_and_cec_reports() {
    use crate::model::Console;
    let cases = [
        (
            json!({"spdVendorName":"MSFT","spdProductDescription":"Xbox One"}),
            Some(Console::Xbox),
        ),
        (json!({"label":"Xbox One"}), Some(Console::Xbox)),
        (
            json!({"spdVendorName":"Nintendo","spdProductDescription":"Switch"}),
            Some(Console::NintendoSwitch),
        ),
        (
            json!({"label":"HDMI1","cecDeviceName":"NintendoSwitch"}),
            Some(Console::NintendoSwitch),
        ),
        (json!({"label":"PS5"}), Some(Console::PlayStation)),
        // SPD describes the source that is on now, even if the TV kept an older label.
        (
            json!({"label":"Xbox One","spdVendorName":"Nintendo"}),
            Some(Console::NintendoSwitch),
        ),
        (json!({"label":"HDMI1"}), None),
        (json!({"label":"Sony soundbar"}), None),
        (json!({"label":"Switchbox AVR"}), None),
    ];
    for (mut raw, device) in cases {
        raw["appId"] = json!("com.webos.app.hdmi1");
        let input = normalize_inputs(&json!([raw])).unwrap().remove(0);
        assert_eq!(input.device, device, "{raw}");
    }
}

#[test]
fn inputs_lead_the_home_strip_and_follow_the_console_on_a_shared_port() {
    use crate::model::Console;
    let mut state = ready();
    let id = state.inventory.inputs[0].id.clone();
    let apps: Vec<_> = state.apps().iter().map(|app| app.id.clone()).collect();
    assert_eq!(apps[..2], ["com.webos.app.hdmi1", "com.webos.app.hdmi2"]);
    assert!(!apps.contains(&"com.webos.app.hdmi3".to_string()));
    // The desktop demo's HDMI 1 reports an Xbox, remembered by the first inventory.
    assert_eq!(state.preferences.input_devices[&id], Console::Xbox);
    let mut inputs = state.inventory.inputs.clone();
    let face = state.input_app(&id).unwrap();
    assert_eq!(
        (face.title.as_str(), face.brand.as_str()),
        ("Xbox", "input-xbox")
    );
    // The console switches off: its card leaves the strip and takes no slot, and the
    // remembered identity is kept without saving again.
    let favorite_x = |state: &crate::state::State| {
        controls(state)
            .iter()
            .find(|c| c.target == Target::App("netflix".into()))
            .map(|c| c.rect.x)
            .unwrap()
    };
    let before = favorite_x(&state);
    inputs[0].device = None;
    inputs[0].signal = Some(false);
    assert!(!state.set_inputs(inputs.clone()));
    assert!(!state.apps().iter().any(|app| app.id == id));
    assert!(favorite_x(&state) < before);
    assert_eq!(
        state.apps()[0].id,
        "com.webos.app.hdmi2",
        "the next card moves into the freed slot"
    );
    assert_eq!(state.input_app(&id).unwrap().brand, "input-xbox");
    // The Switch wakes on the same port.
    inputs[0].device = Some(Console::NintendoSwitch);
    inputs[0].signal = Some(true);
    assert!(state.set_inputs(inputs));
    let face = state.input_app(&id).unwrap();
    assert_eq!(
        (face.title.as_str(), face.brand.as_str()),
        ("Nintendo Switch", "input-switch")
    );
    assert_eq!(
        state.preferences.input_devices[&id],
        Console::NintendoSwitch
    );
    // A custom name wins for the title.
    state.preferences.set_input_name(&id, "Game room");
    assert_eq!(state.input_app(&id).unwrap().title, "Game room");
}

#[test]
fn holding_an_input_card_opens_options_to_remove_it_from_favorites() {
    use crate::state::Effect;
    let mut state = ready();
    let id = state.inventory.inputs[0].id.clone();
    let next = state.inventory.inputs[1].id.clone();
    state.focus = Target::App(id.clone());
    // OK still opens the input; holding opens options instead of reordering.
    assert_eq!(state.activate(), vec![Effect::Launch(id.clone())]);
    state.hold();
    assert!(state.moving.is_none());
    assert!(matches!(&state.overlay, Some(Overlay::Options { id: current, .. }) if current == &id));
    assert_eq!(state.focus, Target::OpenApp);
    // Inputs offer Open and the favorites toggle only: no hide or delete.
    let targets: Vec<_> = controls(&state).into_iter().map(|c| c.target).collect();
    assert!(targets.contains(&Target::OpenApp) && targets.contains(&Target::Option(0)));
    assert!(!targets.contains(&Target::Option(1)) && !targets.contains(&Target::Option(2)));
    state.focus = Target::Option(1);
    assert!(state.activate().is_empty(), "hide does nothing for inputs");
    state.focus = Target::Option(0);
    assert_eq!(state.activate(), vec![Effect::Save]);
    assert_eq!(state.preferences.input_modes[&id], InputMode::Apps);
    assert!(state.overlay.is_none());
    assert!(!state.apps().iter().any(|app| app.id == id));
    assert_eq!(
        state.focus,
        Target::App(next),
        "focus moves to the card that took its slot"
    );

    // On the Apps page it opens options like any app, and can be added back.
    state.open(1);
    assert_eq!(state.apps()[0].id, id);
    state.focus = Target::App(id.clone());
    state.activate();
    assert_eq!(state.focus, Target::OpenApp);
    state.focus = Target::Option(0);
    assert_eq!(state.activate(), vec![Effect::Save]);
    assert_eq!(state.preferences.input_modes[&id], InputMode::Auto);
    assert!(!state.apps().iter().any(|app| app.id == id));
    state.open(0);
    assert_eq!(
        state.apps()[0].id,
        id,
        "back at the front of the Home strip"
    );
    // Open from the options launches the input.
    state.focus = Target::App(id.clone());
    state.options();
    assert_eq!(state.activate(), vec![Effect::Launch(id)]);
}

#[test]
fn apps_only_inputs_skip_home_and_list_on_the_apps_page_while_live() {
    let mut state = ready();
    // HDMI 2 is a soundbar: keep it off Home, but reachable from Apps.
    let id = state.inventory.inputs[1].id.clone();
    state.open(2);
    state.section = WIDGETS_SECTION;
    state.subpage = Some(Subpage::Inputs);
    state.focus = Target::InputMode(id.clone());
    state.activate();
    assert_eq!(state.subpage, Some(Subpage::Input(id.clone())));
    // "Apps only" is the fourth choice, after On, Off and Auto.
    state.focus = Target::Choice(3);
    state.activate();
    assert_eq!(state.preferences.input_modes[&id], InputMode::Apps);

    state.open(0);
    assert!(!state.apps().iter().any(|app| app.id == id));
    assert_eq!(
        state.apps()[0].id,
        state.inventory.inputs[0].id,
        "other cards keep their slots"
    );

    state.open(1);
    assert_eq!(
        state.apps()[0].id,
        id,
        "live Apps-only inputs lead the All tab"
    );
    // Category tabs list apps only; search matches the card title.
    state.filter = 1;
    assert!(!state.apps().iter().any(|app| app.id == id));
    state.filter = 0;
    state.query = "nothing like it".into();
    assert!(!state.apps().iter().any(|app| app.id == id));
    for query in ["hdmi 2", "apple"] {
        state.query = query.into();
        assert!(state.apps().iter().any(|app| app.id == id), "{query}");
    }
    state.query.clear();

    // Switched off: it leaves the Apps page too.
    let mut inputs = state.inventory.inputs.clone();
    inputs[1].signal = Some(false);
    state.set_inputs(inputs);
    assert!(!state.apps().iter().any(|app| app.id == id));

    // The mode survives a save/load round trip.
    let saved = serde_json::to_string(&state.preferences).unwrap();
    let loaded: Preferences = serde_json::from_str(&saved).unwrap();
    assert_eq!(loaded.input_modes[&id], InputMode::Apps);

    // Builds before Apps only parse input_modes as Auto, On or Off and refuse the whole file
    // otherwise. They must still read it, and keep the input off the Home strip.
    #[derive(Debug, serde::Deserialize, PartialEq)]
    enum OlderMode {
        Auto,
        On,
        Off,
    }
    #[derive(serde::Deserialize)]
    struct OlderPreferences {
        input_modes: std::collections::BTreeMap<String, OlderMode>,
    }
    let older: OlderPreferences = serde_json::from_str(&saved).expect("older builds read it");
    assert_eq!(older.input_modes[&id], OlderMode::Off);
}

#[test]
fn input_modes_read_older_newer_and_unknown_values() {
    let preferences: Preferences = serde_json::from_value(serde_json::json!({
        "input_modes": {
            "com.webos.app.hdmi1": "On",
            // Written by builds that stored Apps only inline.
            "com.webos.app.hdmi2": "Apps",
            "com.webos.app.hdmi3": "Off",
            "com.webos.app.hdmi4": "SomethingNewer",
        },
        "apps_only_inputs": ["com.webos.app.hdmi3", "com.webos.app.hdmi1"],
        "show_home_names": true,
    }))
    .unwrap();
    assert_eq!(
        preferences.input_modes["com.webos.app.hdmi1"],
        InputMode::On
    );
    assert_eq!(
        preferences.input_modes["com.webos.app.hdmi2"],
        InputMode::Apps
    );
    assert_eq!(
        preferences.input_modes["com.webos.app.hdmi3"],
        InputMode::Apps
    );
    assert!(!preferences.input_modes.contains_key("com.webos.app.hdmi4"));
    assert!(preferences.show_home_names, "other fields still load");

    let saved = serde_json::to_value(&preferences).unwrap();
    assert_eq!(saved["input_modes"]["com.webos.app.hdmi2"], "Off");
    assert_eq!(
        saved["apps_only_inputs"],
        serde_json::json!(["com.webos.app.hdmi2", "com.webos.app.hdmi3"])
    );
    let empty = serde_json::to_value(Preferences::default()).unwrap();
    assert_eq!(empty["input_modes"], serde_json::json!({}));
    assert!(empty.get("apps_only_inputs").is_none());
}
