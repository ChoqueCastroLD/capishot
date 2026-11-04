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
    let profile = match src.run("profile.capture", json!({"sections": ["shortcuts", "theme"]})).unwrap() {
        serde_json::Value::Object(m) => m,
        _ => unreachable!(),
    };
    let mut profile = profile;
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
    assert!(app.session.prefs().shortcuts.get("file.open").is_none());
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
