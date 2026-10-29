//! The type bar's Contorno (the edited layer's Stroke layer effect) and styles while typing.

use photocraft_doc::{Effect, LayerId};
use serde_json::json;

use crate::PhotocraftApp;

fn new_app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 400, "height": 200})).unwrap();
    app.sync_views();
    app.ui.tool = crate::state::Tool::Type;
    app
}

fn type_new(app: &mut PhotocraftApp, s: &str) -> LayerId {
    super::pointer_up(app, [50.0, 100.0], [50.0, 100.0]);
    super::insert(app, s);
    LayerId(app.ui.text_edit.as_ref().unwrap().layer)
}

pub(super) fn strokes(app: &PhotocraftApp, id: LayerId) -> Vec<photocraft_doc::effects::StrokeFx> {
    app.session
        .active()
        .unwrap()
        .doc
        .layer(id)
        .unwrap()
        .effects
        .items
        .iter()
        .filter_map(|e| if let Effect::Stroke(s) = e { Some(s.clone()) } else { None })
        .collect()
}

#[test]
fn a_style_applied_while_typing_stays() {
    let mut app = new_app();
    let id = type_new(&mut app, "Hola");
    app.run("style.presets.apply", json!({"preset": "Black Stroke"})).unwrap();
    assert_eq!(strokes(&app, id).len(), 1, "the style's stroke");
    super::insert(&mut app, " mundo");
    let s = strokes(&app, id);
    assert_eq!(s.len(), 1, "typing keeps the style's stroke: {s:?}");
    assert_eq!(s[0].size, 3.0);
}

use super::{TypeStroke, set_type_stroke, shown_stroke};
use photocraft_doc::effects::StrokePosition;

fn on(size: f32, rgb: [f32; 3]) -> TypeStroke {
    TypeStroke { on: true, size, position: StrokePosition::Outside, color: rgb }
}

fn visible(app: &PhotocraftApp, id: LayerId) -> bool {
    strokes(app, id).first().is_some_and(|s| s.common.enabled)
}

#[test]
fn the_control_makes_edits_and_hides_one_stroke_in_one_undo_step() {
    let mut app = new_app();
    let id = type_new(&mut app, "Hola");
    let past = app.session.active().unwrap().history.past_len();
    let (_, s) = shown_stroke(&app).unwrap();
    assert!(!s.on, "none yet");
    set_type_stroke(&mut app, on(3.0, [1.0, 1.0, 1.0]), "k1").unwrap();
    assert_eq!(strokes(&app, id).len(), 1, "made");
    set_type_stroke(&mut app, on(7.5, [1.0, 0.0, 0.0]), "k1").unwrap();
    set_type_stroke(&mut app, TypeStroke { position: StrokePosition::Inside, ..on(7.5, [1.0, 0.0, 0.0]) }, "k1").unwrap();
    let s = strokes(&app, id);
    assert_eq!(s.len(), 1, "edited, not duplicated");
    assert_eq!(s[0].size, 7.5);
    assert_eq!(s[0].position, StrokePosition::Inside);
    let (_, shown) = shown_stroke(&app).unwrap();
    assert!(shown.on && (shown.color[0] - 1.0).abs() < 0.01 && shown.color[1] < 0.01, "{shown:?}");
    set_type_stroke(&mut app, TypeStroke { on: false, ..shown }, "k1").unwrap();
    assert!(!visible(&app, id), "off hides it");
    assert_eq!(strokes(&app, id).len(), 1, "and keeps its settings");
    set_type_stroke(&mut app, TypeStroke { on: true, ..shown }, "k1").unwrap();
    assert!(visible(&app, id), "on again");
    assert_eq!(app.session.active().unwrap().history.past_len(), past + 1, "one undo step for the popover session");
    app.run("edit.undo", json!({})).unwrap();
    assert!(strokes(&app, id).is_empty(), "one undo takes it all back");
}

/// A stroke a style made (or Bubble Type's) is the one the control edits; its opacity stays.
#[test]
fn an_existing_stroke_is_edited_and_keeps_what_the_control_doesnt_show() {
    let mut app = new_app();
    let id = type_new(&mut app, "Hola");
    app.run("layer.layerStyle.stroke", json!({"layer": id.0, "size": 4, "color": "#000000", "opacity": 60})).unwrap();
    let (_, s) = shown_stroke(&app).unwrap();
    assert!(s.on && s.size == 4.0);
    set_type_stroke(&mut app, TypeStroke { size: 9.0, ..s }, "k2").unwrap();
    let st = strokes(&app, id);
    assert_eq!(st.len(), 1);
    assert_eq!(st[0].size, 9.0);
    assert!((st[0].common.opacity - 0.6).abs() < 0.01, "opacity kept: {}", st[0].common.opacity);
}

/// The user's "the stroke changes every moment": a style applied while typing, then the
/// stroke edited, then more typing: the edit stays.
#[test]
fn a_style_then_an_edit_survive_typing() {
    let mut app = new_app();
    let id = type_new(&mut app, "Hola");
    app.run("style.presets.apply", json!({"preset": "White Stroke"})).unwrap();
    super::insert(&mut app, " otra");
    let (_, s) = shown_stroke(&app).unwrap();
    assert!(s.on && s.size == 3.0 && s.color[0] > 0.99, "{s:?}");
    set_type_stroke(&mut app, TypeStroke { size: 6.0, color: [0.0, 0.0, 0.0], ..s }, "k3").unwrap();
    super::insert(&mut app, " vez");
    let st = strokes(&app, id);
    assert_eq!(st.len(), 1);
    assert_eq!(st[0].size, 6.0, "the edit stays while typing");
    super::commit(&mut app);
    let st = strokes(&app, id);
    assert_eq!(st[0].size, 6.0, "and after Done");
}

/// The phone's type bar at 360 px: the outline button is there and Done stays in view.
#[test]
fn the_phone_bar_has_the_outline_and_keeps_done_in_view() {
    let mut app = new_app();
    app.run("prefs.set", json!({"values": {"interface.language": "en"}})).unwrap();
    type_new(&mut app, "Hola");
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(360.0, 780.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    h.run_steps(4);
    let rects = crate::touch_ui::inspect(h.state(), &h.ctx).to_string();
    let v: serde_json::Value = serde_json::from_str(&rects).unwrap();
    let find = |l: &str| v["rects"].as_array().and_then(|a| a.iter().find(|r| r["label"] == l)).map(|r| r["rect"].clone());
    let done = find("typeDone").unwrap_or_else(|| panic!("no Done: {rects}"));
    assert!(done[2].as_f64().unwrap() <= 360.0, "Done in view: {done}");
    assert!(find("typeStroke").is_some(), "the outline button: {rects}");
}

/// A tap on the phone bar's outline button opens its popover; a quick colour applies it.
#[test]
fn the_phone_popover_opens_and_a_chip_applies() {
    let mut app = new_app();
    app.run("prefs.set", json!({"values": {"interface.language": "en"}})).unwrap();
    let id = type_new(&mut app, "Hola");
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(360.0, 780.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    h.run_steps(4);
    let rect = |h: &egui_kittest::Harness<'static, PhotocraftApp>, l: &str| -> Option<egui::Rect> {
        let v = crate::touch_ui::inspect(h.state(), &h.ctx);
        let r = v["rects"].as_array()?.iter().rev().find(|r| r["label"] == l)?["rect"].clone();
        let f = |i: usize| r[i].as_f64().unwrap() as f32;
        Some(egui::Rect::from_min_max(egui::pos2(f(0), f(1)), egui::pos2(f(2), f(3))))
    };
    let tap = |h: &mut egui_kittest::Harness<'static, PhotocraftApp>, r: egui::Rect| {
        let p = egui::pos2(r.left() + 6.0, r.center().y);
        h.event(egui::Event::PointerMoved(p));
        h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        h.run_steps(3);
    };
    let b = rect(&h, "typeStroke").expect("the button");
    tap(&mut h, b);
    let chip = rect(&h, "typeStrokeChip0").expect("the popover opened");
    tap(&mut h, chip);
    let s = strokes(h.state(), id);
    assert_eq!(s.len(), 1, "a white stroke");
}
