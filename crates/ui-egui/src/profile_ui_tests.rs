use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use photocraft_engine::Session;
use photocraft_engine::font_projects as fp;
use serde_json::json;

use crate::PhotocraftApp;
use crate::theme::{ThemeKind, Tokens};

fn new_app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "en"}})).unwrap();
    app
}

const CSS: &str = ":root { --base: studio; --accent: #ff8800; }\nbutton:hover { --bg: #102030; }";

/// The custom theme in use is what the app shows, after a restart too; choosing a built-in
/// theme goes back to it.
#[test]
fn the_custom_theme_in_use_is_shown_and_restored() {
    let ctx = egui::Context::default();
    let mut app = new_app();
    app.run("theme.custom.save", json!({"name": "Tinta", "css": CSS})).unwrap();
    crate::prefs_ui::tick(&mut app, &ctx);
    let t = Tokens::get(&ctx);
    assert_eq!(t.accent, egui::Color32::from_rgb(255, 136, 0));
    assert_eq!(app.ui.theme, ThemeKind::Studio, "the layout follows its base");
    assert_eq!(ctx.global_style().visuals.widgets.hovered.bg_fill, egui::Color32::from_rgb(16, 32, 48));
    // A restart: the saved preferences bring it back.
    let saved = app.session.prefs_to_json();
    let mut next = Session::new();
    next.load_prefs_json(&saved).unwrap();
    let mut app2 = PhotocraftApp::new(next, crate::Services::default());
    let ctx2 = egui::Context::default();
    crate::prefs_ui::tick(&mut app2, &ctx2);
    assert_eq!(Tokens::get(&ctx2).accent, egui::Color32::from_rgb(255, 136, 0));
    app2.set_theme(&ctx2, ThemeKind::Classic);
    crate::prefs_ui::tick(&mut app2, &ctx2);
    assert_eq!(Tokens::get(&ctx2), Tokens::for_kind(ThemeKind::Classic));
    assert_eq!(app2.session.prefs().interface.custom_theme, "");
}

/// The editor's preview shows at once and ends with the dialog.
#[test]
fn the_editor_preview_ends_when_the_dialog_closes() {
    let ctx = egui::Context::default();
    let mut app = new_app();
    let d = super::open(&mut app);
    let mut preview = Tokens::for_kind(ThemeKind::Pro);
    preview.card = egui::Color32::from_rgb(1, 2, 3);
    app.prefs_rt.theme_preview = Some(preview);
    crate::prefs_ui::tick(&mut app, &ctx);
    assert_eq!(Tokens::get(&ctx).card, egui::Color32::from_rgb(1, 2, 3));
    app.ui.dialogs.retain(|x| x.id != d);
    crate::prefs_ui::tick(&mut app, &ctx);
    assert!(app.prefs_rt.theme_preview.is_none());
    assert_ne!(Tokens::get(&ctx).card, egui::Color32::from_rgb(1, 2, 3));
}

/// Edit › Preferences › Profile & Themes… opens the dialog, and each tab draws.
#[test]
fn the_dialog_opens_from_the_menu_and_draws_every_tab() {
    let mut app = new_app();
    app.run("theme.custom.save", json!({"name": "Tinta", "css": CSS})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 900.0)).with_max_steps(32).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    h.run_steps(2);
    let ctx = h.ctx.clone();
    crate::menus::invoke(h.state_mut(), &ctx, "edit.preferences.profile", json!({})).unwrap();
    h.run_steps(3);
    h.get_by_label("Tinta");
    h.get_by_label("Built-in themes");
    h.get_all_by_label("Edit…").next().unwrap().click();
    h.run_steps(3);
    h.get_by_label("Edit as text");
    assert!(h.state().prefs_rt.theme_preview.is_some(), "the editor previews live");
    h.get_by_label("Discard changes").click();
    h.run_steps(3);
    assert!(h.state().prefs_rt.theme_preview.is_none(), "Cancel ends the preview");
    h.get_by_label("Export profile").click();
    h.run_steps(3);
    h.get_by_label("Keyboard shortcuts, menus and toolbar");
    h.get_by_label("Import profile").click();
    h.run_steps(3);
    h.get_by_label("Undo last profile import");
}

/// A profile opened is shown first; applying it keeps the profile for Undo, and Undo brings
/// it back, the shell's Bubble Type styles too.
#[test]
fn an_opened_profile_applies_after_a_preview_and_undoes() {
    let mut src = new_app();
    src.run("prefs.set", json!({"path": "shortcuts.file.open", "value": "F2"})).unwrap();
    src.run("theme.custom.save", json!({"name": "Tinta", "css": CSS})).unwrap();
    src.ui.tool_options.bubble_styles.bubble.size = 31.0;
    let mut profile = src.run("profile.capture", json!({"sections": ["shortcuts", "theme"]})).unwrap().as_object().cloned().unwrap();
    profile.insert("bubble".into(), serde_json::to_value(&src.ui.tool_options.bubble_styles).unwrap());
    let out = fp::export(&fp::Pack { name: "Equipo".into(), profile, ..Default::default() }).unwrap();

    let mut app = new_app();
    let before = app.ui.tool_options.bubble_styles.clone();
    app.open_bytes("Equipo.proj.capishot", &out.bytes).unwrap();
    assert!(app.font_projects.profile_import.is_some(), "shown, not applied yet");
    assert!(app.session.prefs().interface.custom_themes.is_empty());
    let (_, imp) = app.font_projects.profile_import.take().unwrap();
    let mut found = super::sections_of(&imp);
    found.sort();
    assert_eq!(found, ["bubble", "shortcuts", "theme"]);
    // Only the shortcuts and Bubble Type, merged.
    super::apply_import(&mut app, imp, &["shortcuts".into(), "bubble".into()], true).unwrap();
    assert_eq!(app.session.prefs().shortcuts.get("file.open").map(String::as_str), Some("F2"));
    assert!(app.session.prefs().interface.custom_themes.is_empty(), "the theme section wasn't chosen");
    assert_eq!(app.ui.tool_options.bubble_styles.bubble.size, 31.0);
    super::undo_import(&mut app).unwrap();
    assert!(!app.session.prefs().shortcuts.contains_key("file.open"));
    assert_eq!(app.ui.tool_options.bubble_styles, before);
    assert!(super::undo_import(&mut app).is_err(), "one undo");
}

/// A `.theme.capishot` opened is added and used.
#[test]
fn opening_a_theme_file_adds_and_uses_it() {
    let mut src = new_app();
    let id = src.run("theme.custom.save", json!({"name": "Tinta", "css": CSS})).unwrap()["id"].as_str().unwrap().to_string();
    let out = src.run("theme.package.export", json!({"id": id})).unwrap();
    let bytes = photocraft_engine::paint::tile::b64_decode(out["data"].as_str().unwrap()).unwrap();
    let mut app = new_app();
    app.open_bytes("Tinta.theme.capishot", &bytes).unwrap();
    let t = app.session.prefs().interface.active_custom_theme().cloned().unwrap();
    assert_eq!((t.name.as_str(), t.css.as_str()), ("Tinta", CSS));
}

#[test]
fn an_icon_is_cropped_and_scaled_to_256() {
    let data: Vec<u8> = (0..600 * 300).flat_map(|i| if i % 600 < 300 { [255, 0, 0, 255] } else { [0, 0, 255, 255] }).collect();
    let img = photocraft_codecs::Image::from_raw(600, 300, photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8, data).unwrap();
    let png = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap();
    let icon = super::icon_png(&png).unwrap();
    assert_eq!(photocraft_engine::package::check_png(&icon, 256), Ok((256, 256)));
    assert!(super::icon_png(b"not an image").is_err());
}

/// The items of Window › Theme, as (id, path below it).
fn theme_rows(app: &PhotocraftApp) -> Vec<(String, Vec<String>)> {
    crate::menus::menu_items(app).into_iter().filter(|i| i.path.starts_with(&["Window".to_string(), "Theme".to_string()])).map(|i| (i.id, i.path[2..].to_vec())).collect()
}

fn invoke(app: &mut PhotocraftApp, id: &str) -> Result<serde_json::Value, String> {
    crate::menus::invoke(app, &egui::Context::default(), id, json!({}))
}

fn opened_tab(app: &PhotocraftApp) -> Option<&str> {
    app.ui.dialogs.iter().find(|d| d.fields.get("__prefsui").and_then(serde_json::Value::as_str) == Some(super::KIND))?.fields.get("tab")?.as_str()
}

fn editing(app: &PhotocraftApp) -> serde_json::Value {
    app.ui.dialogs.iter().find_map(|d| d.fields.get("edit").cloned()).expect("the Theme editor is open")
}

/// My Themes shows only with themes, between the built-in ones and the actions; the actions end
/// the submenu with a separator before them.
#[test]
fn my_themes_is_listed_only_when_there_are_themes() {
    let mut app = new_app();
    let none = theme_rows(&app);
    assert!(!none.iter().any(|(_, path)| path.first().is_some_and(|p| p == "My Themes")));
    let tail: Vec<&str> = none.iter().rev().take(5).rev().map(|(id, _)| id.as_str()).collect();
    assert_eq!(tail, ["window.theme.system", "---", "window.theme.manage", "window.theme.create", "window.theme.load"]);
    app.run("theme.custom.save", json!({"name": "Tinta", "css": CSS})).unwrap();
    app.run("theme.custom.save", json!({"name": "Pro", "css": CSS})).unwrap();
    let rows = theme_rows(&app);
    let mine: Vec<&str> = rows.iter().filter(|(_, path)| path.first().is_some_and(|p| p == "My Themes")).map(|(id, _)| id.as_str()).collect();
    assert_eq!(mine, ["window.theme.custom.tinta", "window.theme.custom.pro"]);
    let at = |id: &str| rows.iter().position(|(i, _)| i == id).unwrap();
    assert!(at("window.theme.softLight") < at(mine[0]) && at(mine[1]) < at("window.theme.system"));
    assert_eq!(rows.iter().filter(|(id, _)| id == "window.theme.manage").count(), 1, "one row despite the catalogue and the shell listing it");
    let lang = crate::i18n::Lang::all().find(|l| l.code() == "es").unwrap();
    assert_eq!(crate::i18n::tr(lang, "My Themes"), "Mis temas");
}

/// Exactly one check mark: choosing a theme of the user's unchecks the built-in ones, and a
/// built-in on the same base goes back to it.
#[test]
fn the_check_mark_follows_the_theme_in_use() {
    let ctx = egui::Context::default();
    let mut app = new_app();
    app.run("theme.custom.save", json!({"name": "Tinta", "css": CSS, "use": false})).unwrap();
    let marked = |app: &PhotocraftApp| -> Vec<String> { crate::menus::menu_items(app).into_iter().filter(|i| i.id.starts_with("window.theme.") && i.checked == Some(true)).map(|i| i.id).collect() };
    let builtin = marked(&app);
    assert_eq!(builtin.len(), 1, "{builtin:?}");
    crate::menus::invoke(&mut app, &ctx, "window.theme.custom.tinta", json!({})).unwrap();
    crate::prefs_ui::tick(&mut app, &ctx);
    assert_eq!(Tokens::get(&ctx).accent, egui::Color32::from_rgb(255, 136, 0));
    assert_eq!(marked(&app), ["window.theme.custom.tinta"]);
    // The slot already holds this built-in, so `prefs.set` alone would change nothing.
    crate::menus::invoke(&mut app, &ctx, &builtin[0], json!({})).unwrap();
    crate::prefs_ui::tick(&mut app, &ctx);
    assert_eq!(marked(&app), builtin);
    assert_ne!(Tokens::get(&ctx).accent, egui::Color32::from_rgb(255, 136, 0));
    // Sync with system leaves the custom theme too, already on automatic or not.
    for _ in 0..2 {
        crate::menus::invoke(&mut app, &ctx, "window.theme.custom.tinta", json!({})).unwrap();
        crate::menus::invoke(&mut app, &ctx, "window.theme.system", json!({})).unwrap();
        assert_eq!(marked(&app), ["window.theme.system"]);
    }
    assert!(crate::menus::invoke(&mut app, &ctx, "window.theme.custom.gone", json!({})).is_err());
}

/// A theme's id comes from the name the user gave it: usage statistics never see it.
#[test]
fn a_custom_themes_id_stays_out_of_the_statistics() {
    assert_eq!(crate::telemetry::sanitize_command_id("window.theme.custom.tinta"), "window.other");
    assert_eq!(crate::telemetry::sanitize_command_id("window.theme.manage"), "window.theme.manage");
}

/// A change to the themes reaches a menu kept open and the native menu bar.
#[test]
fn the_menu_state_hash_sees_the_themes() {
    let mut app = new_app();
    let before = crate::native_menu::state_hash(&app, None);
    app.run("theme.custom.save", json!({"name": "Tinta", "css": CSS, "use": false})).unwrap();
    let listed = crate::native_menu::state_hash(&app, None);
    assert_ne!(before, listed);
    app.run("theme.custom.use", json!({"id": "tinta"})).unwrap();
    assert_ne!(listed, crate::native_menu::state_hash(&app, None));
}

#[test]
fn manage_themes_opens_the_themes_tab() {
    let mut app = new_app();
    assert!(invoke(&mut app, "window.theme.manage").unwrap()["dialog"].is_u64());
    assert_eq!(opened_tab(&app), Some("themes"));
    // Already open on another tab: the same dialog comes back to it.
    for d in &mut app.ui.dialogs {
        d.fields.insert("tab".into(), json!("export"));
    }
    invoke(&mut app, "window.theme.manage").unwrap();
    assert_eq!(opened_tab(&app), Some("themes"));
    assert_eq!(app.ui.dialogs.len(), 1);
}

/// Create Theme… starts from what is on screen, as a new theme, and leaves an edit in progress.
#[test]
fn create_theme_starts_from_the_current_theme() {
    let mut app = new_app();
    app.run("prefs.set", json!({"path": "interface.theme", "value": "studio"})).unwrap();
    let ctx = egui::Context::default();
    crate::prefs_ui::tick(&mut app, &ctx);
    invoke(&mut app, "window.theme.create").unwrap();
    let e = editing(&app);
    assert_eq!(e["id"], "");
    assert_eq!(crate::theme::css::parse(e["css"].as_str().unwrap()).tokens, Tokens::for_kind(ThemeKind::Studio));
    assert_eq!(opened_tab(&app), Some("themes"));

    let mut app = new_app();
    app.run("theme.custom.save", json!({"name": "Tinta", "css": CSS})).unwrap();
    invoke(&mut app, "window.theme.create").unwrap();
    let e = editing(&app);
    assert_eq!((e["id"].as_str(), e["css"].as_str(), e["name"].as_str()), (Some(""), Some(CSS), Some("My Tinta")));
    // Unsaved edits stay.
    for d in &mut app.ui.dialogs {
        if let Some(edit) = d.fields.get_mut("edit") {
            edit["css"] = json!(":root { --accent: #000000; }");
        }
    }
    invoke(&mut app, "window.theme.create").unwrap();
    assert_eq!(editing(&app)["css"], ":root { --accent: #000000; }");
    assert_eq!(app.ui.dialogs.len(), 1);
}

/// Load Theme… asks for a theme file, adds it, uses it and reports what it didn't read; anything
/// else is refused with the theme in use kept.
#[test]
fn load_theme_imports_with_the_existing_validation() {
    use crate::file_dialog::{FileDialogAnswer, FileDialogRequest};
    let mut source = new_app();
    source.run("theme.custom.save", json!({"name": "Tinta", "css": ":root { --base: studio; --accent: #ff8800; --nope: 1; }"})).unwrap();
    let out = source.run("theme.package.export", json!({"id": "tinta"})).unwrap();
    let bytes = photocraft_engine::paint::tile::b64_decode(out["data"].as_str().unwrap()).unwrap();

    let ctx = egui::Context::default();
    let mut app = new_app();
    let answers = vec![Some(FileDialogAnswer::Contents("x.theme.capishot".into(), bytes)), Some(FileDialogAnswer::Contents("x.theme.capishot".into(), b"not a theme".to_vec()))];
    let (show, asked) = crate::file_dialog::fake(answers);
    app.services.file_dialog = Some(show);
    assert!(crate::menus::invoke(&mut app, &ctx, "window.theme.load", json!({})).unwrap()["fileDialog"].is_string());
    while app.file_dialog.is_some() {
        app.poll_file_dialog(&ctx, None);
    }
    assert!(matches!(&asked.borrow()[0], FileDialogRequest::Open { extensions: Some(e), multiple: false, .. } if e == &["capishot"]));
    assert_eq!(app.session.prefs().interface.active_custom_theme().map(|t| t.name.as_str()), Some("Tinta"));
    crate::prefs_ui::tick(&mut app, &ctx);
    assert_eq!(Tokens::get(&ctx).accent, egui::Color32::from_rgb(255, 136, 0));
    assert!(app.ui.notices.iter().any(|n| n.lines.iter().any(|l| l.contains("nope"))), "the unknown property is reported");

    crate::menus::invoke(&mut app, &ctx, "window.theme.load", json!({})).unwrap();
    while app.file_dialog.is_some() {
        app.poll_file_dialog(&ctx, None);
    }
    assert_eq!(app.session.prefs().interface.custom_themes.len(), 1);
    assert_eq!(app.session.prefs().interface.active_custom_theme().map(|t| t.name.as_str()), Some("Tinta"));
}

/// The three actions are real commands: known to search, assignable a shortcut and labelled for
/// the phone's favourites.
#[test]
fn the_theme_actions_are_searchable_and_can_take_a_shortcut() {
    let mut app = new_app();
    let items = crate::menus::menu_items(&app);
    let lang = crate::i18n::Lang::all().find(|l| l.code() == "es").unwrap();
    for (id, en, es) in [("window.theme.manage", "manage themes", "administrar temas"), ("window.theme.create", "create theme", "crear tema"), ("window.theme.load", "load theme", "cargar tema")] {
        assert!(crate::menus::is_live(id));
        assert!(crate::menu_catalog::CATALOG.iter().any(|c| c.3 == id), "{id}");
        assert!(crate::menus::search_items(&items, en, crate::i18n::Lang::EN).iter().any(|i| i.id == id), "{id}");
        assert!(crate::menus::search_items(&items, es, lang).iter().any(|i| i.id == id), "{id}");
        assert!(crate::simple_ui::command_label(id).is_some(), "{id}");
    }
    app.session.prefs.edit(|p| p.shortcuts.insert("window.theme.create".into(), "Cmd+Alt+T".into()));
    assert!(crate::shortcut_dispatch::bindings(&app).iter().any(|(id, _)| id == "window.theme.create"));
    assert_eq!(crate::menus::menu_items(&app).iter().find(|i| i.id == "window.theme.create").unwrap().shortcut.as_deref(), Some("Cmd+Alt+T"));
}

/// On a phone the simple interface keeps Window under "⋯", with Window › Theme whole.
#[test]
fn the_simple_interface_has_the_theme_items() {
    let mut app = new_app();
    app.run("theme.custom.save", json!({"name": "Tinta", "css": CSS})).unwrap();
    app.simple = true;
    let rows = theme_rows(&app);
    for id in ["window.theme.custom.tinta", "window.theme.manage", "window.theme.create", "window.theme.load"] {
        assert!(rows.iter().any(|(i, _)| i == id), "{id}");
    }
    invoke(&mut app, "window.theme.create").unwrap();
    assert!(editing(&app).is_object());
}
