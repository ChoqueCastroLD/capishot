//! Contextual bars never block the canvas: with the Generative Fill bar showing under a
//! selection (desktop) or the selection action bar (phone), a lasso drag on the canvas makes a new
//! selection at once, a click outside deselects, and the Move tool still moves. Real popovers
//! still swallow the press that closes them (`touch_ui_tests::closing_the_options_popover…`).

use std::sync::Arc;

use egui::{Event, Modifiers, PointerButton, Pos2, TouchDeviceId, TouchId, TouchPhase, vec2};
use egui_kittest::Harness;
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;

type H = Harness<'static, PhotocraftApp>;

fn app(lang: &str, simple: &str) -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.session.set_genfill(Some(Arc::new(photocraft_ai_sd::service::FakeGenFill::default())));
    app.run("prefs.set", json!({"values": {"interface.language": lang, "interface.simpleUi": simple}})).unwrap();
    app.run("file.new", json!({"width": 800, "height": 600, "background": "white"})).unwrap();
    app.run("select.rect", json!({"x": 100, "y": 100, "width": 200, "height": 150})).unwrap();
    app.ui.tool = Tool::Lasso;
    app.sync_views();
    app
}

fn harness(app: PhotocraftApp, w: f32, h: f32) -> H {
    let mut h = Harness::builder().with_size(vec2(w, h)).with_max_steps(64).with_step_dt(1.0 / 30.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    for _ in 0..6 {
        frame(&mut h, vec![]);
    }
    h
}

fn frame(h: &mut H, events: Vec<Event>) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    raw.events.extend(events);
    raw.time = Some(ctx.input(|i| i.time) + 1.0 / 30.0);
    crate::touch_ui::route(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    h.step();
}

fn settle(h: &mut H) {
    for _ in 0..4 {
        frame(h, vec![]);
    }
}

fn screen(h: &H, x: f32, y: f32) -> Pos2 {
    crate::canvas::ViewXform::active(h.state()).unwrap().to_screen(x, y)
}

fn mouse_drag(h: &mut H, pts: &[Pos2]) {
    let m = Modifiers::NONE;
    frame(h, vec![Event::PointerMoved(pts[0])]);
    frame(h, vec![Event::PointerButton { pos: pts[0], button: PointerButton::Primary, pressed: true, modifiers: m }]);
    for p in &pts[1..] {
        frame(h, vec![Event::PointerMoved(*p)]);
    }
    let last = *pts.last().unwrap();
    frame(h, vec![Event::PointerButton { pos: last, button: PointerButton::Primary, pressed: false, modifiers: m }]);
    settle(h);
}

fn finger_drag(h: &mut H, pts: &[Pos2]) {
    let t = |phase, p| Event::Touch { device_id: TouchDeviceId(1), id: TouchId(3), phase, pos: p, force: None };
    frame(h, vec![t(TouchPhase::Start, pts[0])]);
    for p in &pts[1..] {
        frame(h, vec![t(TouchPhase::Move, *p)]);
    }
    frame(h, vec![t(TouchPhase::End, *pts.last().unwrap())]);
    settle(h);
}

fn selection(h: &H) -> Option<photocraft_geom::Rect> {
    h.state().session.active().unwrap().doc.selection.as_ref().map(|s| s.content_bounds())
}

fn bar_shown(h: &H) -> bool {
    let id = egui::Id::unique("genfill-prompt-bar");
    h.ctx.memory(|m| m.areas().visible_layer_ids()).iter().any(|l| l.id == id)
}

/// A lasso loop around document points (cx, cy), about `r` px.
fn around(h: &H, cx: f32, cy: f32, r: f32) -> Vec<Pos2> {
    (0..=12)
        .map(|k| {
            let a = k as f32 / 12.0 * std::f32::consts::TAU;
            screen(h, cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

#[test]
fn with_the_generative_fill_bar_up_the_canvas_still_works_on_desktop() {
    let mut h = harness(app("en", "off"), 1280.0, 860.0);
    assert!(bar_shown(&h), "the Generative Fill bar is up");
    assert!(!crate::overlay_guard::overlay_open(h.state(), &h.ctx), "and is no overlay to dismiss");
    // A new lasso elsewhere: a new selection at once.
    let pts = around(&h, 550.0, 400.0, 60.0);
    mouse_drag(&mut h, &pts);
    let s = selection(&h).expect("a selection");
    assert!(s.x0 > 400 && s.y0 > 280, "the new lasso's selection: {s:?}");
    // The bar follows it; a click outside deselects.
    assert!(bar_shown(&h), "the bar moved to the new selection");
    let p = screen(&h, 150.0, 500.0);
    mouse_drag(&mut h, &[p]);
    assert!(selection(&h).is_none(), "a click outside deselects");
}

#[test]
fn with_the_generative_fill_bar_up_the_move_tool_moves() {
    let mut a = app("en", "off");
    // Something to move: a red patch on its own layer, selected.
    a.run("layer.new.layer", json!({})).unwrap();
    a.run("edit.fill", json!({"color": "#ff0000"})).unwrap();
    a.run("select.rect", json!({"x": 100, "y": 100, "width": 200, "height": 150})).unwrap();
    let mut h = harness(a, 1280.0, 860.0);
    assert!(bar_shown(&h));
    // Picking the Move tool and dropping the selection with the bar still drawn this frame.
    h.state_mut().ui.tool = Tool::Move;
    h.state_mut().run("select.deselect", json!({})).unwrap();
    settle(&mut h);
    let id = h.state().session.active().unwrap().active_layer.unwrap();
    let before = h.state().session.active().unwrap().history.entries().len();
    let (p0, p1) = (screen(&h, 200.0, 170.0), screen(&h, 260.0, 230.0));
    mouse_drag(&mut h, &[p0, p0 + (p1 - p0) * 0.3, p0 + (p1 - p0) * 0.7, p1]);
    let st = h.state().session.active().unwrap();
    assert!(st.history.entries().len() > before, "the move made a step: {:?}", st.history.entries());
    assert!(st.doc.layer(id).is_some());
}

#[test]
fn with_the_selection_bar_up_a_phone_lasso_selects_and_a_tap_deselects() {
    let mut h = harness(app("es", "on"), 390.0, 844.0);
    let bar = crate::touch_ui::inspect(h.state(), &h.ctx)["rects"].as_array().unwrap().iter().any(|r| r["label"] == "selectionBar");
    assert!(bar, "the selection bar is up");
    let pts = around(&h, 550.0, 400.0, 70.0);
    finger_drag(&mut h, &pts);
    let s = selection(&h).expect("a selection");
    assert!(s.x0 > 400, "the new lasso's selection: {s:?}");
    let p = screen(&h, 150.0, 520.0);
    finger_drag(&mut h, &[p]);
    assert!(selection(&h).is_none(), "a tap outside deselects");
}
