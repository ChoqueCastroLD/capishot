//! TypeR panel: it renders, a gutter click makes a line current, a style click selects it, the
//! settings dialog saves, the workspace persists and restores, and the TypeR keymap is off by
//! default and never takes a Photoshop key.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

use super::keymap;
use crate::PhotocraftApp;
use crate::shortcut_dispatch::{set_dry_run, take_log};

const LIBRARY: &str = r##"{
  "folders": [{"id": "f1", "name": "Series A", "parentId": null, "order": 0}],
  "styles": [
    {"id": "reg", "name": "Regular", "folder": "f1", "prefixes": ["R:"], "prefixColor": "#CDFDD3"},
    {"id": "sfx", "name": "Shout", "folder": "f1", "prefixes": ["SFX:"], "prefixColor": "#FFA6A4",
     "sizePresets": [30, 24], "textProps": {"layerText": {"textStyleRange": [{"textStyle": {"fontName": "Inter", "size": 30}}],
       "paragraphStyleRange": [{"paragraphStyle": {"alignment": "center"}}]}, "typeUnit": "pixelsUnit"}}
  ]
}"##;

const SCRIPT: &str = "Page 1\nR: Hello there.\n## a note\nSFX: BOOM!\n//...and more.\nPage 2\nLast line.";

fn app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("typer.library.import", json!({"json": LIBRARY})).unwrap();
    app.run("typer.script.set", json!({"text": SCRIPT})).unwrap();
    app
}

/// The panel alone in a 320 × 560 column (TypeR's minimum is 250 × 300), with the dialogs and the
/// shortcut handler, as in the app.
fn harness(app: PhotocraftApp, size: egui::Vec2) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(size).with_step_dt(1.0 / 60.0).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            let ctx = ui.ctx().clone();
            if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            crate::shortcuts::handle(app, &ctx);
            egui::CentralPanel::default().show(ui, |ui| {
                let rect = ui.max_rect();
                ui.set_min_height(rect.height());
                super::panel(app, ui);
            });
            super::windows(app, &ctx);
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.run_steps(4);
    h
}

fn click_at(h: &mut Harness<'static, PhotocraftApp>, p: Pos2) {
    h.input_mut().events.push(Event::PointerMoved(p));
    h.step();
    h.input_mut().events.push(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.input_mut().events.push(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
}

#[test]
fn panel_renders_the_three_blocks() {
    let h = harness(app(), vec2(320.0, 560.0));
    h.get_by_label("Paste");
    h.get_by_label("Align");
    assert!(h.get_all_by_label("Regular").count() >= 1, "the style row (and the preview's style name)");
    h.get_by_label("Shout");
    let (gutter, rects) = super::script_editor::last_layout(&h.ctx).expect("the script editor was drawn");
    assert_eq!(rects.len(), SCRIPT.lines().count(), "one rect per raw line");
    assert!(gutter.width() > 20.0 && rects.windows(2).all(|w| w[0].top() < w[1].top()));
    // The current line starts on the first usable one, with its tag's style.
    let state = &h.state().session.typer.state;
    assert_eq!(state.current_line_index, 1);
    assert_eq!(state.current_style().map(|s| s.name.as_str()), Some("Regular"));
}

#[test]
fn gutter_click_makes_the_line_current() {
    let mut h = harness(app(), vec2(320.0, 560.0));
    let (gutter, rects) = super::script_editor::last_layout(&h.ctx).unwrap();
    click_at(&mut h, Pos2::new(gutter.center().x, rects[3].center().y));
    let state = &h.state().session.typer.state;
    assert_eq!(state.current_line_index, 3, "SFX: BOOM!");
    assert_eq!(state.current_style().map(|s| s.name.as_str()), Some("Shout"), "the line's tag picks its style");
    // A page marker moves to the first line of its page.
    let (gutter, rects) = super::script_editor::last_layout(&h.ctx).unwrap();
    click_at(&mut h, Pos2::new(gutter.center().x, rects[5].center().y));
    assert_eq!(h.state().session.typer.state.current_line_index, 6);
}

#[test]
fn next_and_previous_buttons_move_the_line() {
    let mut h = harness(app(), vec2(320.0, 560.0));
    h.get_by_label("Next line").click();
    h.run_steps(2);
    assert_eq!(h.state().session.typer.state.current_line_index, 3, "the ## note is skipped");
    h.get_by_label("Previous line").click();
    h.run_steps(2);
    assert_eq!(h.state().session.typer.state.current_line_index, 1);
}

#[test]
fn style_click_selects_and_size_preset_chips_switch() {
    let mut h = harness(app(), vec2(320.0, 560.0));
    h.get_by_label("Shout").click();
    h.run_steps(2);
    assert_eq!(h.state().session.typer.state.current_style().map(|s| s.name.as_str()), Some("Shout"));
    // The preset chip 24 makes 24 the active size.
    let id = h.state().session.typer.state.current_style_id.clone().unwrap();
    h.state_mut().run("typer.style.setSizePreset", json!({"id": id.to_value(), "size": 24, "now": 1_700_000_000_000u64})).unwrap();
    let style = h.state().session.typer.state.library.style(&id).cloned().unwrap();
    assert_eq!(style.size(), Some(24.0));
    assert_eq!(style.edited.as_ref().and_then(serde_json::Number::as_u64), Some(1_700_000_000_000));
}

#[test]
fn settings_dialog_saves_its_draft() {
    let mut h = harness(app(), vec2(700.0, 700.0));
    super::settings::open(h.state_mut());
    h.run_steps(2);
    {
        let d = h.state_mut().ui.typer.settings.as_mut().unwrap();
        d.values["internalPadding"] = json!(25);
        d.values["interpretMarkdown"] = json!(false);
        d.ignore_prefixes = "##\nNOTE:".into();
    }
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(3);
    let app = h.state();
    assert!(app.ui.typer.settings.is_none(), "Save closes the dialog");
    let s = &app.session.typer.state.settings;
    assert_eq!(s.internal_padding, 25.0);
    assert!(!s.interpret_markdown);
    assert_eq!(s.ignore_line_prefixes, ["##", "NOTE:"]);
}

#[test]
fn style_editor_creates_a_style_with_presets() {
    let mut app = app();
    super::style_editor::open_new(&mut app);
    {
        let d = app.ui.typer.style_editor.as_mut().unwrap();
        d.style["name"] = json!("Whisper");
        d.prefixes = "W:; (w)".into();
        d.style["sizePresets"] = json!([10, 12]);
        d.style["textProps"]["layerText"]["textStyleRange"][0]["textStyle"]["size"] = json!(12);
    }
    app.services.clock = Some(|| 1_234_567.0);
    let mut h = harness(app, vec2(800.0, 900.0));
    // The window sizes itself over a few frames.
    h.run_steps(10);
    h.get_by_label("Save").click();
    h.run_steps(3);
    let app = h.state();
    assert!(app.ui.typer.style_editor.is_none(), "{:?}", app.ui.typer.style_editor.as_ref().map(|d| &d.message));
    let lib = &app.session.typer.state.library;
    let s = lib.styles.iter().find(|s| s.name == "Whisper").expect("saved");
    assert_eq!(s.prefixes, ["W:", "(w)"]);
    assert_eq!(photocraft_typer::presets::normalize_presets(s), [12.0, 10.0], "the active size comes first");
    assert_eq!(s.edited.as_ref().and_then(serde_json::Number::as_u64), Some(1_234_567), "stamped by the shell's clock");
    assert_eq!(app.session.typer.state.current_style().map(|s| s.name.as_str()), Some("Whisper"), "a new style becomes current");
}

#[test]
fn workspace_persists_and_restores() {
    use std::sync::{Arc, Mutex};
    let store: Arc<Mutex<Option<String>>> = Arc::default();
    let services = |store: &Arc<Mutex<Option<String>>>| {
        let (a, b) = (store.clone(), store.clone());
        crate::Services {
            load_typer: Some(Box::new(move || a.lock().unwrap().clone())),
            save_typer: Some(Box::new(move |text: &str| {
                *b.lock().unwrap() = Some(text.to_string());
                Ok(())
            })),
            ..Default::default()
        }
    };
    let mut first = app();
    first.services = services(&store);
    let ctx = egui::Context::default();
    super::persist(&mut first, &ctx);
    assert!(store.lock().unwrap().is_none(), "nothing saved until something changes");
    first.run("typer.line.next", json!({})).unwrap();
    // A second later the change is saved.
    let mut out = ctx.run_ui(egui::RawInput { time: Some(2.0), ..Default::default() }, |_| {});
    out.textures_delta.clear();
    super::persist(&mut first, &ctx);
    let saved = store.lock().unwrap().clone().expect("saved");
    assert!(saved.contains("BOOM"), "{saved}");
    // A new session restores it on its first frame.
    let mut second = PhotocraftApp::new(photocraft_engine::Session::new(), services(&store));
    super::persist(&mut second, &egui::Context::default());
    assert_eq!(second.session.typer, first.session.typer);
    assert_eq!(second.session.typer.state.current_line_index, 3);
    // Garbage is reported, not applied.
    *store.lock().unwrap() = Some("{\"state\": {\"settings\": {\"internalPadding\": \"wide\"}}}".into());
    let mut third = PhotocraftApp::new(photocraft_engine::Session::new(), services(&store));
    super::persist(&mut third, &egui::Context::default());
    assert_eq!(third.session.typer, photocraft_engine::typer_state::TyperSession::default());
    assert!(third.ui.status_error);
}

#[test]
fn markdown_keys_in_the_script_editor_skip_the_menus() {
    let mut h = harness(app(), vec2(320.0, 560.0));
    h.ctx.memory_mut(|m| m.request_focus(super::script_editor::editor_id()));
    h.run_steps(2);
    // Select "Hello" on line 1 (characters 10..15 of the script).
    let mut st = egui::TextEdit::load_state(&h.ctx, super::script_editor::editor_id()).unwrap_or_default();
    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(10), egui::text::CCursor::new(15))));
    st.store(&h.ctx, super::script_editor::editor_id());
    set_dry_run(&h.ctx, true);
    h.input_mut().events.push(Event::Key { key: Key::B, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND });
    h.run_steps(3);
    assert!(take_log(&h.ctx).is_empty(), "⌘B didn't reach Image › Adjustments › Color Balance");
    assert!(h.state().session.typer.state.text.contains("R: **Hello** there."), "{}", h.state().session.typer.state.text);
}

/// Photoshop's defaults, from the shortcut parity table, in Capishot's notation for this
/// platform (`Cmd` = ⌘ / Ctrl).
fn photoshop_keys() -> Vec<String> {
    let mac = cfg!(target_os = "macos");
    include_str!("../shortcut_parity/photoshop_defaults.tsv")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| l.split('\t').nth(if mac { 3 } else { 2 }).map(str::to_string))
        .filter_map(|keys| {
            let keys = keys.replace("Command", "Cmd").replace("Option", "Alt").replace("Page Up", "PageUp").replace("Page Down", "PageDown");
            let keys = if mac { keys.replace("Control", "Ctrl") } else { keys.replace("Ctrl", "Cmd") };
            photocraft_engine::prefs::normalize_shortcut(&keys)
        })
        .collect()
}

#[test]
fn typer_keymap_is_off_by_default_and_never_takes_a_photoshop_key() {
    let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    // Off by default: no TypeR command has a key, from the registry or the preferences.
    assert!(photocraft_engine::command_specs().iter().filter(|c| c.id.starts_with("typer.")).all(|c| c.shortcut.is_none()));
    for (id, ..) in keymap::ENTRIES {
        assert_eq!(crate::shortcuts::effective_shortcut(&app, id, crate::shortcuts::default_shortcut(id).as_deref()), None, "{id}");
    }
    assert!(!keymap::is_on(&app.session.prefs().shortcuts));
    // The preset's keys: none is in Photoshop's table, none is pressed by a Capishot default.
    let ps = photoshop_keys();
    assert!(ps.len() > 150, "the parity table was read ({})", ps.len());
    let defaults = crate::shortcut_dispatch::bindings(&app);
    for (id, key) in keymap::preset() {
        assert!(!ps.contains(&key), "{id}: {key} is a Photoshop default");
        let sc = crate::shortcuts::parse(&key).unwrap_or_else(|| panic!("{key} parses"));
        let taken: Vec<&str> =
            defaults.iter().filter(|(_, b)| crate::shortcuts::key_matches(b, sc.logical_key, sc.modifiers)).map(|(i, _)| i.as_str()).collect();
        assert!(taken.is_empty(), "{id}: {key} already runs {taken:?}");
    }
    // TypeR's own keys that collide stay Photoshop's.
    for (id, typer_key) in [("typer.line.next", "Cmd+Enter"), ("typer.line.previous", "Ctrl+Tab"), ("typer.toggleCleaningLayers", "Cmd+H")] {
        let preset = keymap::preset().into_iter().find(|(i, _)| *i == id).map(|(_, k)| k);
        assert_ne!(preset.as_deref(), photocraft_engine::prefs::normalize_shortcut(typer_key).as_deref(), "{id}");
    }
}

#[test]
fn typer_keymap_applies_dispatches_and_reverts() {
    let mut h = harness(app(), vec2(320.0, 560.0));
    let before = h.state().session.prefs().shortcuts.clone();
    keymap::apply(h.state_mut(), true).unwrap();
    assert!(keymap::is_on(&h.state().session.prefs().shortcuts));
    // Photoshop's keys are untouched.
    for id in ["window.nextDocument", "view.extras", "edit.undo", "type.panels.character"] {
        let app = h.state();
        assert_eq!(
            crate::shortcuts::effective_shortcut(app, id, crate::shortcuts::default_shortcut(id).as_deref()),
            crate::shortcuts::default_shortcut(id),
            "{id}"
        );
    }
    // Ctrl+Alt+Enter runs Next Line; Ctrl+Alt+B the panel's Paste.
    h.input_mut().events.push(Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND | Modifiers::ALT });
    h.run_steps(2);
    assert_eq!(h.state().session.typer.state.current_line_index, 3);
    set_dry_run(&h.ctx, true);
    h.input_mut().events.push(Event::Key { key: Key::B, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND | Modifiers::ALT });
    h.run_steps(2);
    let log = take_log(&h.ctx);
    assert!(log.iter().any(|(id, _)| id == "typer.ui.paste"), "{log:?}");
    set_dry_run(&h.ctx, false);
    // A key the user moved stays when the preset goes off.
    h.state_mut().run("edit.keyboardShortcuts", json!({"set": {"typer.align": "F9"}, "allowUnknown": true})).unwrap();
    keymap::apply(h.state_mut(), false).unwrap();
    let after = h.state().session.prefs().shortcuts.clone();
    assert_eq!(after.get("typer.align").map(String::as_str), Some("F9"));
    let mut expected = before;
    expected.insert("typer.align".into(), "F9".into());
    assert_eq!(after, expected, "only the preset's keys went");
}

#[test]
fn the_keyboard_shortcuts_dialog_lists_the_typer_commands() {
    let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let items = crate::prefs_ui::shortcut_items(&app);
    for (id, ..) in keymap::ENTRIES {
        let it = items.iter().find(|i| i.0 == *id).unwrap_or_else(|| panic!("{id} listed"));
        assert_eq!(it.2.first().map(String::as_str), Some("Type"), "{id}");
        assert_eq!(it.3, None, "{id}: no default key");
    }
    let mut overrides = std::collections::BTreeMap::new();
    keymap::set(&mut overrides, true);
    assert!(keymap::is_on(&overrides));
    keymap::set(&mut overrides, false);
    assert!(overrides.is_empty());
}

#[test]
fn panel_commands_route_like_typer() {
    let mut app = app();
    assert_eq!(super::target(&app, "typer.ui.paste").map(|t| t.0), Some("typer.paste"));
    app.run("typer.settings.set", json!({"key": "textSizeIncrement", "value": 3})).unwrap();
    assert_eq!(super::target(&app, "typer.ui.decreaseSize").map(|t| t.1), Some(json!({"delta": -3.0})));
    assert!(super::handles("typer.ui.insertText") && !super::handles("typer.paste"));
    // Unknown ids aren't ours; a stub placement reports why it can't run.
    assert!(super::invoke(&mut app, "typer.nope", &json!({})).is_none());
    assert_eq!(super::is_enabled(&app, "typer.ui.paste"), Some(app.session.is_enabled("typer.paste")));
}

/// Window › TypeR shows the panel in its own dock group, and choosing it again hides it.
#[test]
fn window_menu_toggles_the_dock_group() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let ctx = egui::Context::default();
    assert!(!app.ui.panels.typer, "not in the default workspace");
    crate::menus::invoke(&mut app, &ctx, "window.panel.typer", json!({})).unwrap();
    assert!(app.ui.panels.typer && crate::dock::Group::TypeR.shown(&app.ui.panels));
    assert!(crate::menus::menu_items(&app).iter().any(|i| i.id == "window.panel.typer" && i.path == ["Window"] && i.checked == Some(true)));
    crate::menus::invoke(&mut app, &ctx, "window.panel.typer", json!({})).unwrap();
    assert!(!app.ui.panels.typer);
    // The layout round-trips with the new group.
    let v = serde_json::to_value(&app.ui.dock).unwrap();
    let back: crate::dock::DockLayout = serde_json::from_value(v).unwrap();
    assert!(back.order().contains(&crate::dock::Group::TypeR));
}

/// A document with a white page, so the bubble opening finds the selected area.
fn app_with_page() -> PhotocraftApp {
    let mut app = app();
    app.run("file.new", json!({"width": 800, "height": 600, "background": "white"})).unwrap();
    app.sync_views();
    app
}

fn text_layers(app: &PhotocraftApp) -> usize {
    let doc = &app.session.active().unwrap().doc;
    doc.walk().into_iter().filter(|(_, _, l)| matches!(l.content, photocraft_doc::LayerContent::Text(_))).count()
}

#[test]
fn paste_button_typesets_the_current_line_into_the_selection() {
    let mut app = app_with_page();
    app.run("select.rect", json!({"x": 100, "y": 100, "width": 300, "height": 200})).unwrap();
    let mut h = harness(app, vec2(320.0, 560.0));
    assert_eq!(text_layers(h.state()), 0);
    h.get_by_label("Paste").click();
    h.run_steps(3);
    assert_eq!(text_layers(h.state()), 1, "{}", h.state().ui.typer.message);
    assert_eq!(h.state().session.typer.state.current_line_index, 3, "Paste advances to the next line");
}

#[test]
fn multi_bubble_mode_captures_each_new_selection() {
    let mut app = app_with_page();
    app.run("typer.multiBubble.toggle", json!({"on": true})).unwrap();
    let ctx = egui::Context::default();
    super::persist(&mut app, &ctx);
    app.run("select.rect", json!({"x": 50, "y": 50, "width": 200, "height": 120})).unwrap();
    super::persist(&mut app, &ctx);
    assert_eq!(app.session.typer.stored_selections.len(), 1);
    assert_eq!(app.session.typer.state.current_line_index, 3, "the line advances with each bubble");
    // Another edit with the same selection stores nothing more.
    app.run("typer.style.next", json!({})).unwrap();
    app.run("layer.new.layer", json!({})).unwrap();
    super::persist(&mut app, &ctx);
    assert_eq!(app.session.typer.stored_selections.len(), 1);
    app.run("select.rect", json!({"x": 400, "y": 300, "width": 200, "height": 120})).unwrap();
    super::persist(&mut app, &ctx);
    assert_eq!(app.session.typer.stored_selections.len(), 2);
    // Off: nothing is captured.
    app.run("typer.multiBubble.toggle", json!({"on": false})).unwrap();
    app.run("select.rect", json!({"x": 10, "y": 10, "width": 100, "height": 100})).unwrap();
    super::persist(&mut app, &ctx);
    assert!(app.session.typer.stored_selections.is_empty());
}

/// Window › Workspace › Typesetting: TypeR on top with most of the column, Character and Layers
/// below; the other presets (Essentials) are unchanged and hide TypeR again.
#[test]
fn typesetting_workspace_shows_typer_character_and_layers() {
    use crate::dock::Group;
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, "window.workspace.essentials", json!({})).unwrap();
    let essentials = (app.ui.panels.clone(), app.ui.dock.clone());
    crate::menus::invoke(&mut app, &ctx, "window.workspace.typesetting", json!({})).unwrap();
    assert_eq!(app.ui.workspace, "Typesetting");
    assert!(crate::menus::menu_items(&app).iter().any(|i| i.id == "window.workspace.typesetting" && i.checked == Some(true)));
    let shown: Vec<Group> = app.ui.dock.order().into_iter().filter(|g| g.shown(&app.ui.panels)).collect();
    assert_eq!(shown, [Group::TypeR, Group::Character, Group::Layers]);
    let hs = app.ui.dock.heights_for(&shown, 830.0, 28.0);
    assert!(hs[0].1 >= 500.0, "TypeR gets most of a 900-pt screen's column: {hs:?}");
    // A 900-px screen in a browser: TypeR keeps a usable height, the others give way first.
    let hs = app.ui.dock.heights_for(&shown, 680.0, 28.0);
    assert!(hs[0].1 >= 420.0 && hs[2].1 >= Group::Layers.min_height(), "{hs:?}");
    // Reset brings Typesetting back; Essentials is what it was.
    app.ui.dock.heights.insert(Group::TypeR, 200.0);
    crate::menus::invoke(&mut app, &ctx, "window.workspace.resetWorkspace", json!({})).unwrap();
    assert_eq!(app.ui.dock.height(Group::TypeR), crate::menus::TYPESETTING_TYPER_HEIGHT);
    crate::menus::invoke(&mut app, &ctx, "window.workspace.essentials", json!({})).unwrap();
    assert!(!app.ui.panels.typer);
    assert_eq!((app.ui.panels.clone(), app.ui.dock.clone()), essentials);
    assert!(
        crate::menus::menu_items(&app).iter().any(|i| i.id == "window.workspace.typesetting" && i.path == ["Window", "Workspace"] && i.checked == Some(false))
    );
}

/// TypeR shown in Essentials on a short column: Color and Properties give way before it.
#[test]
fn typer_group_gives_way_last_in_a_short_column() {
    use crate::dock::{DockLayout, Group};
    let shown = [Group::Color, Group::Properties, Group::TypeR, Group::Layers];
    let hs = DockLayout::default().heights_for(&shown, 680.0, 28.0);
    let h = |g: Group| hs.iter().find(|(x, _)| *x == g).map_or(0.0, |(_, h)| *h);
    assert_eq!(h(Group::Color), Group::Color.min_height());
    assert_eq!(h(Group::Properties), Group::Properties.min_height());
    assert!(h(Group::TypeR) >= 340.0, "{hs:?}");
    assert!(h(Group::Layers) >= Group::Layers.min_height());
    let total: f32 = hs.iter().map(|(_, h)| h).sum::<f32>() + crate::dock::GAP * 3.0;
    assert!((total - 680.0).abs() < 0.5, "{hs:?}");
}

/// Folding the script gives the style list its room, and folding the preview leaves the line as
/// one row of text.
#[test]
fn panel_blocks_fold_and_give_their_room_away() {
    let h = harness(app(), vec2(320.0, 520.0));
    let styles_h = |h: &Harness<'static, PhotocraftApp>| h.get_by_label("Shout").rect().top();
    let before = styles_h(&h);
    let (_, rects) = super::script_editor::last_layout(&h.ctx).expect("the script is drawn");
    assert!(!rects.is_empty());
    let mut app = h.into_state();
    super::toggle_fold(&mut app, "script");
    super::toggle_fold(&mut app, "preview");
    let h = harness(app, vec2(320.0, 520.0));
    assert!(styles_h(&h) < before - 100.0, "the style list moves up: {} → {}", before, styles_h(&h));
    h.get_by_label("Script");
    h.get_by_label("Regular: Hello there.");
    let mut app = h.into_state();
    assert!(super::is_folded(&app, "script"));
    super::toggle_fold(&mut app, "script");
    assert!(!super::is_folded(&app, "script") && super::is_folded(&app, "preview"));
    // The fold state is view state: it round-trips with the UI state.
    let back: super::TyperUi = serde_json::from_value(serde_json::to_value(&app.ui.typer).unwrap()).unwrap();
    assert_eq!(back.folded, ["preview"]);
}
