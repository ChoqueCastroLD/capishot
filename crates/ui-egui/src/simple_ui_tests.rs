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

/// On a phone: the compact bar, no toolbar or dock, the favorites and recent-tools bars with the
/// Capi button between them, its sheet with the panels and the way back.
#[test]
fn a_phone_gets_the_simple_interface() {
    for (w, hh) in [(412.0, 915.0), (390.0, 844.0)] {
        let mut a = app();
        a.run("file.new", json!({"width": 600, "height": 800})).unwrap();
        a.sync_views();
        let mut h = harness(a, w, hh);
        assert!(h.state().simple);
        // File, Edit and Image by name, the rest behind "⋯"; no ☰.
        for m in ["File", "Edit", "Image", "More menus"] {
            assert!(h.query_by_label(m).is_some(), "{w}: {m}");
        }
        assert!(h.query_by_label("Menu").is_none() && h.query_by_label("Filter").is_none(), "{w}: no hamburger, Filter is behind ⋯");
        assert!(h.query_by_label("Gradient Tool").is_none(), "{w}: no toolbar column");
        // The two bars: favorites (6) above, recent tools (6) below, Capi centred between.
        let s = inspect(h.state(), &h.ctx);
        let rect = |label: &str| -> egui::Rect {
            let b = s["buttons"].as_array().unwrap().iter().find(|b| b["label"] == label).unwrap_or_else(|| panic!("{w}: no {label}"));
            let r: [f32; 4] = serde_json::from_value(b["rect"].clone()).unwrap();
            egui::Rect::from_min_max(egui::pos2(r[0], r[1]), egui::pos2(r[2], r[3]))
        };
        let favs: Vec<egui::Rect> = DEFAULT_FAVORITES.iter().map(|f| rect(&format!("slot:{f}"))).collect();
        let recents: Vec<egui::Rect> = DEFAULT_RECENT_TOOLS.iter().map(|t| rect(&format!("slot:tool:{t:?}"))).collect();
        let capi = rect("capi");
        assert!((capi.center().x - w / 2.0).abs() < 1.0, "{w}: Capi centred");
        for row in [&favs, &recents] {
            assert!(row[..3].iter().all(|r| r.right() < capi.left()) && row[3..].iter().all(|r| r.left() > capi.right()), "{w}: 3 + 3 around Capi");
            assert!(row.iter().all(|r| r.width() >= 40.0 && r.left() >= 0.0 && r.right() <= w && r.bottom() <= hh));
        }
        assert!(favs[0].bottom() < recents[0].top(), "{w}: favorites above recents");
        assert!(capi.top() < favs[0].bottom() && capi.bottom() > recents[0].top(), "{w}: Capi overlaps both bars");
        // Use two tools from elsewhere: they become the first recent tools.
        h.state_mut().ui.tool = Tool::Gradient;
        h.run_steps(2);
        h.state_mut().ui.tool = Tool::Pen;
        h.run_steps(2);
        let bar = recent_bar(h.state(), &h.ctx);
        assert!(bar.contains(&Tool::Pen) && bar.contains(&Tool::Gradient), "{bar:?}");
        // Using a tool already in the bar leaves every slot where it was.
        h.state_mut().ui.tool = Tool::Gradient;
        h.run_steps(2);
        assert_eq!(recent_bar(h.state(), &h.ctx), bar, "no shuffling under the finger");
        // A favorite never shows among the recents.
        h.state_mut().ui.tool = Tool::Brush;
        h.run_steps(2);
        assert!(!recent_bar(h.state(), &h.ctx).contains(&Tool::Brush));
        // The options row: compact, the rest behind its button, inside the window.
        assert!(h.query_by_label("Opacity:").is_none() && h.query_by_label_contains("Opacity").is_none(), "{w}: no Opacity in the row");
        h.get_by_label("Tool options").click();
        h.run_steps(3);
        h.run_steps(3);
        assert!(h.query_by_label_contains("pressure for opacity").is_some(), "{w}: the options are open");
        // Nothing in the window runs past its edges: the bars, the row, the opened options.
        for node in h.query_all_by(|n| n.label().is_some_and(|l| !l.is_empty())) {
            let a = egui_kittest::kittest::NodeT::accesskit_node(&node);
            let Some(b) = a.bounding_box() else { continue };
            if b.x1 - b.x0 <= 0.0 || b.y0 as f32 > hh {
                continue;
            }
            assert!(b.x0 >= -0.5 && b.x1 as f32 <= w + 0.5, "{w}: {:?} at {b:?} runs past the window", a.label());
        }
        h.get_by_label("Tool options").click();
        h.run_steps(2);
        // The Capi button opens the sheet.
        h.get_by_label("Capi: tools, panels and actions").click();
        h.run_steps(3);
        assert_eq!(sheet(&h.ctx), Sheet::Master);
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
    assert_eq!(favorites(&a).len(), 6);
    toggle_favorite(&mut a, "image.imageSize");
    assert!(is_favorite(&a, "image.imageSize") && is_favorite(&a, "tool:Brush"));
    toggle_favorite(&mut a, "tool:Brush");
    assert!(!is_favorite(&a, "tool:Brush"));
    assert_eq!(command_label("image.imageSize").as_deref(), Some("Image Size"));
}

/// A long press on a tool in the recent-tools bar shows what it does, and its card makes it a
/// favorite (it then leaves that bar).
#[test]
fn a_long_press_makes_a_recent_tool_a_favorite() {
    let mut a = app();
    a.run("file.new", json!({"width": 600, "height": 800})).unwrap();
    a.sync_views();
    let mut h = harness(a, 412.0, 915.0);
    // A secondary click is what a mouse has for the long press (both open the card).
    h.get_by_label("Zoom Tool").click_secondary();
    h.run_steps(3);
    assert_eq!(help_open(&h.ctx).as_deref(), Some("tool:Zoom"));
    h.get_by_label("Tap or drag to zoom in and out");
    assert!(!is_favorite(h.state(), "tool:Zoom"), "only the card's button adds it");
    h.get_by_label("Add to favorites").click();
    h.run_steps(3);
    assert!(help_open(&h.ctx).is_none(), "the card closes");
    assert!(is_favorite(h.state(), "tool:Zoom"));
    assert_eq!(favorites(h.state()).len(), 7);
    assert!(!recent_bar(h.state(), &h.ctx).contains(&Tool::Zoom));
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

/// A tool tapped in the Capi sheet becomes the tool (and the sheet closes).
#[test]
fn a_tool_tapped_in_the_sheet_is_picked() {
    let mut a = app();
    a.run("file.new", json!({"width": 600, "height": 800})).unwrap();
    a.sync_views();
    let mut h = harness(a, 412.0, 915.0);
    h.state_mut().ui.tool = Tool::CloneStamp;
    h.run_steps(2);
    // The scrim closed a sheet once: egui raises a tapped area, and it then sat above the next
    // sheet and took every tap there (the tool never changed).
    h.get_by_label("Capi: tools, panels and actions").click();
    h.run_steps(3);
    let screen = h.ctx.content_rect();
    h.event(egui::Event::PointerMoved(egui::pos2(screen.center().x, screen.top() + 60.0)));
    h.event(egui::Event::PointerButton { pos: egui::pos2(screen.center().x, screen.top() + 60.0), button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: egui::pos2(screen.center().x, screen.top() + 60.0), button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    h.run_steps(3);
    assert_eq!(sheet(&h.ctx), Sheet::Closed, "the scrim closes the sheet");
    h.get_by_label("Capi: tools, panels and actions").click();
    h.run_steps(3);
    h.get_by_label("Pencil Tool").click();
    h.run_steps(3);
    assert_eq!(h.state().ui.tool, Tool::Pencil);
    assert_eq!(sheet(&h.ctx), Sheet::Closed);
}
