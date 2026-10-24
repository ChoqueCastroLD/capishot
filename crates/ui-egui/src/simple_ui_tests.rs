use egui::vec2;
use egui_kittest::{Harness, kittest::Queryable};
use serde_json::json;

use super::*;

fn app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "en"}})).unwrap();
    app
}

fn harness(app: PhotocraftApp, w: f32, h: f32) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(w, h)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    h.run_steps(4);
    h
}

#[test]
fn auto_is_on_for_phones_and_the_preference_overrides_it() {
    let mut a = app();
    let phone = egui::Context::default();
    phone.run_ui(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(412.0, 915.0))), ..Default::default() }, |_| {}).textures_delta.clear();
    let desk = egui::Context::default();
    desk.run_ui(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(1280.0, 800.0))), ..Default::default() }, |_| {}).textures_delta.clear();
    assert!(active(&a, &phone) && !active(&a, &desk));
    a.run("prefs.set", json!({"values": {"interface.simpleUi": "off"}})).unwrap();
    assert!(!active(&a, &phone));
    a.run("prefs.set", json!({"values": {"interface.simpleUi": "on"}})).unwrap();
    assert!(active(&a, &desk));
}

/// On a phone: the compact bar, no toolbar or dock, the favorites bar, and the Capi button's
/// sheet with the tools, the panels and the way back to the full interface.
#[test]
fn a_phone_gets_the_simple_interface() {
    for (w, hh) in [(412.0, 915.0), (390.0, 844.0)] {
        let mut a = app();
        a.run("file.new", json!({"width": 600, "height": 800})).unwrap();
        a.sync_views();
        let mut h = harness(a, w, hh);
        assert!(h.state().simple);
        assert!(h.query_by_label("Undo").is_some() && h.query_by_label_contains("Search commands").is_some(), "{w}: the compact bar");
        assert!(h.query_by_label("Move Tool").is_some() && h.query_by_label("Gradient Tool").is_none(), "{w}: no toolbar column");
        // The favorites bar (the last Brush Tool button: the options bar shows the tool too).
        let brush = h.query_all_by_label("Brush Tool").last().unwrap().rect();
        assert!(brush.width() >= 40.0 && brush.bottom() <= hh, "{w}: {brush:?}");
        h.get_by_label("Eraser Tool").click();
        h.run_steps(2);
        assert_eq!(h.state().ui.tool, Tool::Eraser);
        // The Capi button opens the sheet.
        h.get_by_label("Capi: tools, panels and actions").click();
        h.run_steps(3);
        assert_eq!(sheet(&h.ctx), Sheet::Master);
        assert!(h.query_by_label("Full interface").is_some());
        for b in ["Layers", "Full interface"] {
            let r = h.get_by_label(b).rect();
            assert!(r.right() <= w && r.left() >= 0.0, "{w}: {b} at {r:?}");
        }
        h.get_by_label("Layers").click();
        h.run_steps(3);
        assert!(matches!(sheet(&h.ctx), Sheet::Panel(Group::Layers, _)), "{:?}", sheet(&h.ctx));
        h.get_by_label("Back").click();
        h.run_steps(2);
        assert_eq!(sheet(&h.ctx), Sheet::Master);
        h.get_by_label("Full interface").click();
        h.run_steps(3);
        assert!(!h.state().simple, "{w}: back to the full interface");
        assert_eq!(h.state().session.prefs().interface.simple_ui, SimpleUi::Off);
    }
}

#[test]
fn favorites_can_be_added_and_removed() {
    let mut a = app();
    assert_eq!(favorites(&a).len(), DEFAULT_FAVORITES.len());
    toggle_favorite(&mut a, "image.imageSize");
    assert!(is_favorite(&a, "image.imageSize") && is_favorite(&a, "tool:Brush"));
    toggle_favorite(&mut a, "tool:Brush");
    assert!(!is_favorite(&a, "tool:Brush"));
    assert_eq!(command_label("image.imageSize").as_deref(), Some("Image Size"));
}

/// The palette finds commands by their names in Spanish, Portuguese and English and by everyday
/// words, whatever the UI language, and spells out quick actions.
#[test]
fn the_palette_searches_three_languages_and_runs_quick_actions() {
    let mut a = app();
    let ctx = egui::Context::default();
    let first = |a: &PhotocraftApp, q: &str| crate::palette::hits_for(a, &ctx, q).first().map(|h| h.1.clone());
    assert_eq!(first(&a, "borrar").as_deref(), Some("tool:Eraser"));
    assert_eq!(first(&a, "desfazer").as_deref(), Some("edit.undo"));
    assert_eq!(first(&a, "tamano de imagen").as_deref(), Some("image.imageSize"), "Spanish, without the accent, in an English UI");
    assert_eq!(first(&a, "zoom 200").as_deref(), Some("quick:"));
    crate::palette::run_quick(&mut a, &ctx, "nuevo 300x200").unwrap();
    let d = &a.session.active().unwrap().doc;
    assert_eq!((d.size.width, d.size.height), (300, 200));
    crate::palette::run_quick(&mut a, &ctx, "#ff0000").unwrap();
    assert_eq!(a.session.tools.foreground, [1.0, 0.0, 0.0, 1.0]);
    crate::palette::run_quick(&mut a, &ctx, "zoom 200").unwrap();
    assert_eq!(a.ui.views[a.session.active_index().unwrap()].zoom, 2.0);
    crate::palette::run_quick(&mut a, &ctx, "rotar 90").unwrap();
    let d = &a.session.active().unwrap().doc;
    assert_eq!((d.size.width, d.size.height), (200, 300));
}
