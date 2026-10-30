//! The touch interface end to end in the real app: finger events go through the router
//! (`touch_ui::route`, as `raw_input_hook` does) and the frame runs.

use egui::{Event, Pos2, TouchDeviceId, TouchId, TouchPhase, pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;

type H = Harness<'static, PhotocraftApp>;

fn harness() -> H {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "en", "interface.simpleUi": "on"}})).unwrap();
    app.run("file.new", json!({"width": 600, "height": 800, "background": "white"})).unwrap();
    app.run("layer.new.layer", json!({})).unwrap();
    app.sync_views();
    let mut h = Harness::builder().with_size(vec2(412.0, 915.0)).with_max_steps(64).with_step_dt(1.0 / 30.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    h.run_steps(4);
    h
}

/// One frame with these events, routed like the browser's (each touch preceded by eframe's
/// pointer emulation for the first finger, which the router replaces).
fn frame(h: &mut H, events: Vec<Event>) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    raw.events.extend(events);
    let t = ctx.input(|i| i.time) + 1.0 / 30.0;
    raw.time = Some(t);
    crate::touch_ui::route(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    h.step();
}

fn touch(id: u64, phase: TouchPhase, p: Pos2) -> Event {
    Event::Touch { device_id: TouchDeviceId(1), id: TouchId(id), phase, pos: p, force: None }
}

/// A finger drawn along `pts` (screen points), one frame per point.
fn finger_drag(h: &mut H, pts: &[Pos2]) {
    frame(h, vec![touch(1, TouchPhase::Start, pts[0])]);
    for p in &pts[1..] {
        frame(h, vec![touch(1, TouchPhase::Move, *p)]);
    }
    frame(h, vec![touch(1, TouchPhase::End, *pts.last().unwrap())]);
    h.run_steps(3);
}

fn history(h: &mut H) -> Vec<String> {
    let v = h.state_mut().run("document.inspect", json!({})).unwrap();
    serde_json::from_value(v["history"].clone()).unwrap()
}

fn canvas_center(h: &H) -> Pos2 {
    h.state().last_canvas_rect.center()
}

#[test]
fn a_finger_lasso_selects() {
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Lasso;
    h.run_steps(2);
    let c = canvas_center(&h);
    let pts: Vec<Pos2> = (0..=16).map(|k| {
        let a = k as f32 / 16.0 * std::f32::consts::TAU;
        c + vec2(80.0 * a.cos(), 60.0 * a.sin())
    }).collect();
    finger_drag(&mut h, &pts);
    assert!(h.state().session.active().unwrap().doc.selection.is_some(), "history: {:?}", history(&mut h));
    assert!(h.state().ui.canvas_tool_menu.is_none(), "no context menu");
}

#[test]
fn two_fingers_tapped_undo_and_never_paint() {
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Brush;
    h.run_steps(2);
    let c = canvas_center(&h);
    finger_drag(&mut h, &[c, c + vec2(40.0, 10.0), c + vec2(80.0, 20.0)]);
    let n = history(&mut h).len();
    assert_eq!(history(&mut h).last().map(String::as_str), Some("Brush Tool"));
    // Two fingers down and up quickly: undo, and the first finger's dab is gone, not committed.
    frame(&mut h, vec![touch(1, TouchPhase::Start, c + vec2(0.0, 100.0))]);
    frame(&mut h, vec![touch(2, TouchPhase::Start, c + vec2(60.0, 100.0))]);
    frame(&mut h, vec![touch(1, TouchPhase::End, c + vec2(0.0, 100.0)), touch(2, TouchPhase::End, c + vec2(60.0, 100.0))]);
    h.run_steps(4);
    assert!(h.state().session.active().unwrap().history.can_redo(), "undone: {:?}", history(&mut h));
    let after = history(&mut h);
    assert_eq!(after.len(), n - 1, "the tap undid one step and painted nothing: {after:?}");
    // Three fingers: redo.
    let f: Vec<Pos2> = (0..3).map(|k| c + vec2(40.0 * k as f32, 150.0)).collect();
    frame(&mut h, f.iter().enumerate().map(|(k, p)| touch(10 + k as u64, TouchPhase::Start, *p)).collect());
    frame(&mut h, f.iter().enumerate().map(|(k, p)| touch(10 + k as u64, TouchPhase::End, *p)).collect());
    h.run_steps(4);
    assert!(!h.state().session.active().unwrap().history.can_redo(), "redone");
}

#[test]
fn four_fingers_hide_the_interface() {
    let mut h = harness();
    let c = canvas_center(&h);
    let f: Vec<Pos2> = (0..4).map(|k| c + vec2(30.0 * k as f32, 0.0)).collect();
    frame(&mut h, f.iter().enumerate().map(|(k, p)| touch(20 + k as u64, TouchPhase::Start, *p)).collect());
    frame(&mut h, f.iter().enumerate().map(|(k, p)| touch(20 + k as u64, TouchPhase::End, *p)).collect());
    h.run_steps(3);
    assert!(h.state().ui_hidden);
    let s = crate::simple_ui::inspect(h.state(), &h.ctx);
    assert!(s["buttons"].as_array().unwrap().iter().all(|b| b["label"] != "capi"), "no Capi button while hidden");
    let t = crate::touch_ui::inspect(h.state(), &h.ctx);
    assert!(t["rects"].as_array().unwrap().iter().any(|r| r["label"] == "showUi"));
}

#[test]
fn a_long_press_with_the_brush_picks_a_color_and_paints_nothing() {
    let mut h = harness();
    h.state_mut().run("select.rect", json!({"x": 0, "y": 0, "width": 600, "height": 800})).unwrap();
    h.state_mut().run("edit.fill", json!({"contents": "color", "color": "#ff0000"})).unwrap();
    h.state_mut().run("select.deselect", json!({})).unwrap();
    h.state_mut().run("tools.setColors", json!({"foreground": "#000000"})).ok();
    h.state_mut().ui.tool = Tool::Brush;
    h.run_steps(3);
    let n = history(&mut h).len();
    let c = canvas_center(&h);
    frame(&mut h, vec![touch(1, TouchPhase::Start, c)]);
    for _ in 0..20 {
        frame(&mut h, vec![]);
    }
    assert!(crate::touch_ui::picking(&h.ctx), "the long press picks");
    let t = crate::touch_ui::inspect(h.state(), &h.ctx);
    assert!(t["rects"].as_array().unwrap().iter().any(|r| r["label"] == "loupe"), "the loupe shows");
    frame(&mut h, vec![touch(1, TouchPhase::End, c)]);
    h.run_steps(4);
    let fg = h.state().session.tools.foreground;
    assert!(fg[0] > 0.9 && fg[1] < 0.1, "picked red: {fg:?}");
    let after = history(&mut h);
    assert_eq!(after.len(), n, "nothing painted: {after:?}");
}

#[test]
fn the_modifier_button_adds_to_a_marquee_selection() {
    let mut h = harness();
    h.state_mut().ui.tool = Tool::RectMarquee;
    h.run_steps(3);
    let c = canvas_center(&h);
    finger_drag(&mut h, &[c - vec2(100.0, 100.0), c - vec2(60.0, 60.0), c - vec2(20.0, 20.0)]);
    let bounds = |h: &mut H| -> [f64; 4] {
        let v = h.state_mut().run("document.inspect", json!({})).unwrap();
        serde_json::from_value(v["selectionBounds"].clone()).expect("a selection")
    };
    let first = bounds(&mut h);
    let button = h.state().touch.button.center.expect("the modifier button is placed");
    // Hold the button, drag a second rectangle elsewhere: it adds.
    frame(&mut h, vec![touch(5, TouchPhase::Start, button)]);
    finger_drag(&mut h, &[c + vec2(20.0, 20.0), c + vec2(60.0, 60.0), c + vec2(100.0, 100.0)]);
    frame(&mut h, vec![touch(5, TouchPhase::End, button)]);
    h.run_steps(2);
    let both = bounds(&mut h);
    assert!(both[2] > first[2] + 20.0 && both[0] <= first[0] + 1.0, "the second rectangle joined the first: {first:?} → {both:?}");
}

#[test]
fn the_size_slider_sets_the_brush_size_with_a_preview() {
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Brush;
    h.run_steps(3);
    let t = crate::touch_ui::inspect(h.state(), &h.ctx);
    let r = t["rects"].as_array().unwrap().iter().find(|r| r["label"] == "sizeSlider").expect("size slider")["rect"].clone();
    let r: [f32; 4] = serde_json::from_value(r).unwrap();
    assert!(r[2] - r[0] >= 44.0, "a 44 pt target");
    let x = (r[0] + r[2]) / 2.0;
    frame(&mut h, vec![touch(1, TouchPhase::Start, pos2(x, r[3] - 10.0))]);
    frame(&mut h, vec![touch(1, TouchPhase::Move, pos2(x, (r[1] + r[3]) / 2.0))]);
    let t = crate::touch_ui::inspect(h.state(), &h.ctx);
    assert!(t["rects"].as_array().unwrap().iter().any(|r| r["label"] == "brushPreview"), "the brush shows while dragging");
    frame(&mut h, vec![touch(1, TouchPhase::Move, pos2(x, r[1] + 6.0))]);
    frame(&mut h, vec![touch(1, TouchPhase::End, pos2(x, r[1] + 6.0))]);
    h.run_steps(3);
    assert!(h.state().session.tools.brush.size > 500.0, "{}", h.state().session.tools.brush.size);
}

#[test]
fn a_selection_brings_its_action_bar() {
    let mut h = harness();
    h.state_mut().run("select.rect", json!({"x": 50, "y": 50, "width": 200, "height": 200})).unwrap();
    h.run_steps(3);
    let t = crate::touch_ui::inspect(h.state(), &h.ctx);
    let rects = t["rects"].as_array().unwrap();
    for id in ["sel:select.deselect", "sel:select.inverse", "sel:edit.fill", "sel:edit.inpaintAi", "sel:edit.copy", "sel:edit.cut", "sel:edit.freeTransform", "sel:select.modify.feather"] {
        let r = rects.iter().find(|r| r["label"] == id).unwrap_or_else(|| panic!("{id}"));
        let r: [f32; 4] = serde_json::from_value(r["rect"].clone()).unwrap();
        assert!(r[2] - r[0] >= 44.0 && r[3] - r[1] >= 44.0, "{id} is a 44 pt target");
    }
    let r: [f32; 4] = serde_json::from_value(rects.iter().find(|r| r["label"] == "sel:select.inverse").unwrap()["rect"].clone()).unwrap();
    let p = pos2((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
    frame(&mut h, vec![touch(1, TouchPhase::Start, p)]);
    frame(&mut h, vec![touch(1, TouchPhase::End, p)]);
    h.run_steps(3);
    assert_eq!(history(&mut h).last().map(String::as_str), Some("Inverse"));
}

#[test]
fn transform_has_done_and_cancel_and_a_pinch_inside_scales_it() {
    let mut h = harness();
    h.state_mut().run("select.rect", json!({"x": 100, "y": 100, "width": 400, "height": 400})).unwrap();
    h.state_mut().run("edit.fill", json!({"contents": "color", "color": "#00aa00"})).unwrap();
    h.state_mut().run("select.deselect", json!({})).unwrap();
    let ctx = h.ctx.clone();
    crate::menus::invoke(h.state_mut(), &ctx, "edit.freeTransform", json!({})).unwrap();
    h.run_steps(3);
    let q0 = h.state().ui.transform.as_ref().expect("transforming").quad;
    // Two fingers inside the box spread apart: the box grows (the view's zoom doesn't change).
    let zoom0 = h.state().ui.views[0].zoom;
    let c = canvas_center(&h);
    frame(&mut h, vec![touch(1, TouchPhase::Start, c - vec2(30.0, 0.0)), touch(2, TouchPhase::Start, c + vec2(30.0, 0.0))]);
    for k in 1..=6 {
        let d = 30.0 + 10.0 * k as f32;
        frame(&mut h, vec![touch(1, TouchPhase::Move, c - vec2(d, 0.0)), touch(2, TouchPhase::Move, c + vec2(d, 0.0))]);
    }
    frame(&mut h, vec![touch(1, TouchPhase::End, c - vec2(90.0, 0.0)), touch(2, TouchPhase::End, c + vec2(90.0, 0.0))]);
    h.run_steps(3);
    let q1 = h.state().ui.transform.as_ref().expect("still transforming").quad;
    let w = |q: [[f64; 2]; 4]| q[1][0] - q[0][0];
    assert!(w(q1) > w(q0) * 1.5, "{q0:?} → {q1:?}");
    assert!((h.state().ui.views[0].zoom - zoom0).abs() < 1e-6, "the view didn't zoom");
    let t = crate::touch_ui::inspect(h.state(), &h.ctx);
    let done: [f32; 4] = serde_json::from_value(t["rects"].as_array().unwrap().iter().find(|r| r["label"] == "transformDone").expect("Done")["rect"].clone()).unwrap();
    assert!(done[3] - done[1] >= 44.0);
    let p = pos2((done[0] + done[2]) / 2.0, (done[1] + done[3]) / 2.0);
    frame(&mut h, vec![touch(1, TouchPhase::Start, p)]);
    frame(&mut h, vec![touch(1, TouchPhase::End, p)]);
    h.run_steps(8);
    assert!(h.state().ui.transform.is_none(), "committed");
}

#[test]
fn once_a_pen_is_used_a_finger_pans() {
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Brush;
    h.state_mut().touch.pen_seen = true;
    h.run_steps(2);
    let n = history(&mut h).len();
    let center0 = h.state().ui.views[0].center;
    let c = canvas_center(&h);
    finger_drag(&mut h, &[c, c + vec2(30.0, 0.0), c + vec2(60.0, 0.0), c + vec2(90.0, 0.0)]);
    assert_eq!(history(&mut h).len(), n, "the finger painted nothing");
    assert_ne!(h.state().ui.views[0].center, center0, "it panned");
}

#[test]
fn the_selection_mode_chips_set_the_mode() {
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Lasso;
    h.run_steps(3);
    let s = crate::simple_ui::inspect(h.state(), &h.ctx);
    let r: [f32; 4] = serde_json::from_value(s["buttons"].as_array().unwrap().iter().find(|b| b["label"] == "mode:2").expect("Subtract chip")["rect"].clone()).unwrap();
    let p = pos2((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
    frame(&mut h, vec![touch(1, TouchPhase::Start, p)]);
    frame(&mut h, vec![touch(1, TouchPhase::End, p)]);
    h.run_steps(2);
    assert_eq!(h.state().ui.selection_mode, 2);
}



#[test]
fn a_long_press_with_the_clone_stamp_sets_its_source() {
    let mut h = harness();
    h.state_mut().run("select.rect", json!({"x": 0, "y": 0, "width": 600, "height": 400})).unwrap();
    h.state_mut().run("edit.fill", json!({"contents": "color", "color": "#ff0000"})).unwrap();
    h.state_mut().run("select.deselect", json!({})).unwrap();
    h.state_mut().ui.tool = Tool::CloneStamp;
    h.run_steps(3);
    let c = canvas_center(&h);
    frame(&mut h, vec![touch(1, TouchPhase::Start, c)]);
    for k in 0..20 {
        frame(&mut h, vec![]);
        let _ = k;
    }
    frame(&mut h, vec![touch(1, TouchPhase::End, c)]);
    h.run_steps(3);
    assert!(h.state().ui.clone_source.is_some(), "status: {:?}", h.state().ui.status);
    // Then a stroke clones.
    finger_drag(&mut h, &[c + vec2(-60.0, 120.0), c + vec2(-30.0, 125.0), c + vec2(0.0, 130.0), c + vec2(30.0, 135.0)]);
    assert_eq!(history(&mut h).last().map(String::as_str), Some("Clone Stamp"));
}

/// Dialogs fit a phone's width: the buttons stay on the screen (they ran off its right edge).
#[test]
fn dialogs_fit_the_phone() {
    use egui_kittest::kittest::Queryable;
    let mut h = harness();
    let ctx = h.ctx.clone();
    let mut f = serde_json::Map::new();
    f.insert("__label".into(), json!("MI-GAN isn't downloaded"));
    f.insert("message".into(), json!("This type layer must be rasterized before proceeding. Its text will no longer be editable. Rasterize the type? A long sentence that would run past a narrow window."));
    h.state_mut().ui.open_dialog(crate::state::DialogKind::Command, f);
    let _ = ctx;
    h.run_steps(6);
    for label in ["OK", "Cancel"] {
        if let Some(n) = h.query_by_label(label) {
            let r = n.rect();
            assert!(r.left() >= 0.0 && r.right() <= 412.0, "{label} at {r:?} is off the screen");
        }
    }
    // The model gate's dialog too.
    let mut h = harness();
    h.state_mut().run("select.rect", json!({"x": 10, "y": 10, "width": 50, "height": 50})).unwrap();
    let ctx = h.ctx.clone();
    let _ = crate::menus::invoke(h.state_mut(), &ctx, "edit.inpaintAi", json!({}));
    h.run_steps(6);
    for label in ["Open Model Manager", "Cancel"] {
        if let Some(n) = h.query_by_label(label) {
            let r = n.rect();
            assert!(r.left() >= 0.0 && r.right() <= 412.0, "{label} at {r:?} is off the screen");
        }
    }
}

/// The simple interface has no status bar: a status message (an error, a tool's hint) shows as a
/// toast instead of nowhere.
#[test]
fn status_messages_show_as_a_toast() {
    let mut h = harness();
    h.state_mut().ui.status = "Alt-click to define a source point to clone from".into();
    h.run_steps(2);
    let t = crate::touch_ui::inspect(h.state(), &h.ctx);
    assert!(t["rects"].as_array().unwrap().iter().any(|r| r["label"] == "toast"));
}

/// The scrollbars only covered the canvas's edges on a phone (fingers pan).
#[test]
fn no_scrollbars_with_the_touch_interface() {
    let mut h = harness();
    h.state_mut().run("view.zoomIn", json!({})).ok();
    h.state_mut().run("view.zoomIn", json!({})).ok();
    h.run_steps(3);
    let over = h.ctx.data(|d| d.get_temp::<bool>(egui::Id::new(("pc-canvas-bars-hover", 0usize))));
    assert_ne!(over, Some(true));
    assert!(!h.state().ui.view.shows_scrollbars() || crate::touch_ui::active(h.state()));
}

/// The Layers sheet: rows and footer buttons a finger can hit.
#[test]
fn the_layers_sheet_has_finger_sized_rows_and_buttons() {
    use egui_kittest::kittest::Queryable;
    let mut h = harness();
    crate::simple_ui::set_sheet(&h.ctx, crate::simple_ui::Sheet::Panel(crate::dock::Group::Layers, 0));
    h.run_steps(4);
    for label in ["Delete layer", "Create a new layer"] {
        let n = h.query_by_label_contains(label);
        assert!(n.is_some(), "{label} in the sheet");
        if let Some(n) = n {
            let r = n.rect();
            assert!(r.width() >= 40.0 && r.height() >= 40.0, "{label}: {r:?}");
        }
    }
}

/// Notices sat bottom right, over the Capi button and the tool bars.
#[test]
fn notices_leave_the_capi_button_free() {
    use egui_kittest::kittest::Queryable;
    let mut h = harness();
    crate::notices::post(h.state_mut(), "Exported Page.png with 1 warning", vec!["3 layer(s) flattened".into()], false, None);
    h.run_steps(3);
    let notice = h.get_by_label("Exported Page.png with 1 warning").rect();
    let s = crate::simple_ui::inspect(h.state(), &h.ctx);
    let r: [f32; 4] = serde_json::from_value(s["buttons"].as_array().unwrap().iter().find(|b| b["label"] == "capi").unwrap()["rect"].clone()).unwrap();
    assert!(notice.bottom() < r[1], "notice {notice:?} over Capi {r:?}");
}

/// A long press on a layer row in the Layers sheet opens the layer's menu, and not the canvas's
/// menu under the sheet.
#[test]
fn a_long_press_on_a_layer_row_opens_its_menu_not_the_canvas_s() {
    use egui_kittest::kittest::Queryable;
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Lasso;
    crate::simple_ui::set_sheet(&h.ctx, crate::simple_ui::Sheet::Panel(crate::dock::Group::Layers, 0));
    h.run_steps(4);
    let row = h.get_by_label("Layer 1").rect().center();
    frame(&mut h, vec![touch(1, TouchPhase::Start, row)]);
    for _ in 0..40 {
        frame(&mut h, vec![]);
    }
    frame(&mut h, vec![touch(1, TouchPhase::End, row)]);
    h.run_steps(3);
    assert!(h.state().ui.canvas_tool_menu.is_none(), "the canvas menu opened under the sheet");
    assert!(h.query_by_label_contains("Duplicate Layer").is_some() || h.query_by_label_contains("Delete Layer").is_some(), "the layer menu is open");
}

/// A notice and the selection's action bar both show: the bar's buttons stay tappable.
#[test]
fn a_notice_leaves_the_selection_bar_free() {
    use egui_kittest::kittest::Queryable;
    let mut h = harness();
    h.state_mut().run("select.rect", json!({"x": 50, "y": 50, "width": 200, "height": 200})).unwrap();
    crate::notices::post(h.state_mut(), "MI-GAN downloaded", vec![], false, None);
    h.run_steps(3);
    let notice = h.get_by_label("MI-GAN downloaded").rect();
    let t = crate::touch_ui::inspect(h.state(), &h.ctx);
    let bar: [f32; 4] = serde_json::from_value(t["rects"].as_array().unwrap().iter().find(|r| r["label"] == "selectionBar").unwrap()["rect"].clone()).unwrap();
    assert!(notice.top() >= bar[3], "notice {notice:?} over the bar {bar:?}");
}

/// A pinch whose fingers land outside a small box but are centred on it still scales the box.
#[test]
fn a_pinch_centred_on_a_small_box_scales_it() {
    let mut h = harness();
    h.state_mut().run("select.rect", json!({"x": 280, "y": 380, "width": 40, "height": 40})).unwrap();
    h.state_mut().run("edit.fill", json!({"contents": "color", "color": "#00aa00"})).unwrap();
    let ctx = h.ctx.clone();
    crate::menus::invoke(h.state_mut(), &ctx, "edit.freeTransform", json!({})).unwrap();
    h.run_steps(3);
    let q0 = h.state().ui.transform.as_ref().expect("transforming").quad;
    let xf = crate::canvas::ViewXform::active(h.state()).unwrap();
    let c = xf.to_screen(300.0, 400.0);
    frame(&mut h, vec![touch(1, TouchPhase::Start, c - vec2(40.0, 0.0)), touch(2, TouchPhase::Start, c + vec2(40.0, 0.0))]);
    for k in 1..=6 {
        let d = 40.0 + 12.0 * k as f32;
        frame(&mut h, vec![touch(1, TouchPhase::Move, c - vec2(d, 0.0)), touch(2, TouchPhase::Move, c + vec2(d, 0.0))]);
    }
    frame(&mut h, vec![touch(1, TouchPhase::End, c - vec2(112.0, 0.0)), touch(2, TouchPhase::End, c + vec2(112.0, 0.0))]);
    h.run_steps(3);
    let q1 = h.state().ui.transform.as_ref().expect("still transforming").quad;
    assert!(q1[1][0] - q1[0][0] > (q0[1][0] - q0[0][0]) * 1.5, "{q0:?} -> {q1:?}");
}

/// The action bar's buttons have names under their icons, and at 390 pt the bar scrolls with a
/// fade at the edge that has more.
#[test]
fn the_selection_bar_names_its_buttons_and_hints_that_it_scrolls() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "es", "interface.simpleUi": "on"}})).unwrap();
    app.run("file.new", json!({"width": 600, "height": 800, "background": "white"})).unwrap();
    app.run("select.rect", json!({"x": 50, "y": 50, "width": 200, "height": 200})).unwrap();
    app.sync_views();
    let mut h = Harness::builder().with_size(vec2(390.0, 844.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    h.run_steps(4);
    let t = crate::touch_ui::inspect(h.state(), &h.ctx);
    let rects = t["rects"].as_array().unwrap();
    assert!(rects.iter().any(|r| r["label"] == "selectionBarMore"), "a fade says there is more");
    for r in rects.iter().filter(|r| r["label"].as_str().is_some_and(|l| l.starts_with("sel:"))) {
        let r: [f32; 4] = serde_json::from_value(r["rect"].clone()).unwrap();
        assert!(r[2] - r[0] >= 44.0 && r[3] - r[1] >= 44.0);
    }
    use egui_kittest::kittest::Queryable;
    assert!(h.query_by_label("Invertir").is_some(), "names in the UI language");
}

fn phone_harness(w: f32, h: f32) -> H {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "en", "interface.simpleUi": "on"}})).unwrap();
    app.run("file.new", json!({"width": 600, "height": 800, "background": "white"})).unwrap();
    app.sync_views();
    let mut hh = Harness::builder().with_size(vec2(w, h)).with_max_steps(64).with_step_dt(1.0 / 30.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    hh.run_steps(4);
    hh
}

fn tap(h: &mut H, p: Pos2) {
    frame(h, vec![touch(7, TouchPhase::Start, p)]);
    frame(h, vec![touch(7, TouchPhase::End, p)]);
    h.run_steps(3);
}

fn noted(h: &H, label: &str) -> Option<egui::Rect> {
    crate::touch_ui::noted_rect(&h.ctx, label).or_else(|| crate::simple_ui::noted_rect(&h.ctx, label))
}

/// Start typing on the phone: the Type tool, a tap on the canvas, some letters.
fn start_typing(h: &mut H) {
    h.state_mut().ui.tool = Tool::Type;
    h.run_steps(2);
    let c = canvas_center(h);
    tap(h, c);
    frame(h, vec![Event::Text("Hola".into())]);
    h.run_steps(2);
    assert!(h.state().ui.text_edit.is_some(), "typing");
}

#[test]
fn done_then_move_selects_move() {
    let mut h = phone_harness(360.0, 780.0);
    start_typing(&mut h);
    let done = noted(&h, "typeDone").expect("Done");
    tap(&mut h, done.center());
    assert!(h.state().ui.text_edit.is_none(), "Done leaves typing");
    let mv = noted(&h, "slot:tool:Move").expect("the favourites bar is back with Move");
    tap(&mut h, mv.center());
    assert_eq!(h.state().ui.tool, Tool::Move);
}

/// Every way out of typing on a phone keeps the text, brings the tool bars back and lets the
/// next tool be chosen: a tool in the Capi sheet (reachable above the type bar), a tap outside
/// the text, and Back (Escape).
#[test]
fn every_way_out_of_typing_brings_the_tools_back() {
    // The Capi sheet while typing.
    let mut h = phone_harness(360.0, 780.0);
    start_typing(&mut h);
    let capi = noted(&h, "capi").expect("the Capi button stays while typing");
    tap(&mut h, capi.center());
    let mv = noted(&h, "tool:Move").expect("the sheet's tools");
    tap(&mut h, mv.center());
    assert_eq!(h.state().ui.tool, Tool::Move);
    assert!(h.state().ui.text_edit.is_none(), "the tool ended the typing");
    assert!(history(&mut h).iter().any(|s| s.contains("Type")), "the text was kept: {:?}", history(&mut h));
    // A tap outside the text.
    let mut h = phone_harness(360.0, 780.0);
    start_typing(&mut h);
    let c = canvas_center(&h);
    tap(&mut h, c + vec2(0.0, 150.0));
    assert!(h.state().ui.text_edit.is_none(), "a tap outside ends the typing");
    let mv = noted(&h, "slot:tool:Move").expect("the bars are back");
    tap(&mut h, mv.center());
    assert_eq!(h.state().ui.tool, Tool::Move);
    // Back (the phone's Back sends Escape): keeps the text.
    let mut h = phone_harness(360.0, 780.0);
    start_typing(&mut h);
    let n = history(&mut h).len();
    frame(&mut h, vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
    h.run_steps(3);
    assert!(h.state().ui.text_edit.is_none(), "Back ends the typing");
    assert!(history(&mut h).len() >= n, "and keeps the text");
    let mv = noted(&h, "slot:tool:Move").expect("the bars are back");
    tap(&mut h, mv.center());
    assert_eq!(h.state().ui.tool, Tool::Move);
}
