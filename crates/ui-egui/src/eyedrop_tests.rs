//! The colour controls' eyedropper: armed, a press on the image sets the control's colour to the
//! composite there; cancelled (Esc, the button again), nothing changes.

use serde_json::json;

use super::*;
use crate::color_picker_ui;

/// A 64 x 64 document filled with `color`.
fn app(color: &str) -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 64, "height": 64, "background": color})).unwrap();
    app.sync_views();
    app
}

fn picker_rgb(app: &PhotocraftApp, id: u64) -> [f32; 3] {
    color_picker_ui::current(&app.ui.dialogs.iter().find(|d| d.id == id).unwrap().fields).rgb
}

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() < 0.01)
}

/// One pass of `ctx` with `events`, running `f` in it.
fn pass(ctx: &Context, events: Vec<egui::Event>, mut f: impl FnMut(&Context)) {
    let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 800.0))), events, ..Default::default() };
    ctx.run_ui(raw, |ui| f(ui.ctx())).textures_delta.clear();
}

fn press(at: Pos2) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(at),
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
    ]
}

#[test]
fn a_pick_sets_the_color_picker_from_the_image_pixel_and_disarms() {
    let mut app = app("#ff0000");
    let ctx = Context::default();
    let id = color_picker_ui::open_for_command(&mut app, "Color Picker", [0.0, 0.0, 1.0], "edit.fill", json!({}));
    toggle(&ctx, Target::ColorPicker { id });
    assert_eq!(armed(&ctx), Some(Target::ColorPicker { id }));
    assert!(pick_at(&mut app, &ctx, 10.0, 10.0));
    assert!(close(picker_rgb(&app, id), [1.0, 0.0, 0.0]), "{:?}", picker_rgb(&app, id));
    assert_eq!(armed(&ctx), None, "one shot");
    assert!(!pick_at(&mut app, &ctx, 10.0, 10.0), "not armed any more");
}

#[test]
fn cancelling_changes_nothing() {
    let mut app = app("#ff0000");
    let ctx = Context::default();
    let id = color_picker_ui::open_for_command(&mut app, "Color Picker", [0.0, 0.0, 1.0], "edit.fill", json!({}));
    // The button again disarms.
    toggle(&ctx, Target::ColorPicker { id });
    toggle(&ctx, Target::ColorPicker { id });
    assert_eq!(armed(&ctx), None);
    // Esc disarms, and the dialog doesn't see the Esc.
    toggle(&ctx, Target::ColorPicker { id });
    pass(&ctx, vec![egui::Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }], |ctx| {
        begin_frame(&app, ctx);
        assert!(!ctx.input(|i| i.key_pressed(Key::Escape)), "consumed");
    });
    assert_eq!(armed(&ctx), None);
    assert!(!pick_at(&mut app, &ctx, 10.0, 10.0));
    assert!(close(picker_rgb(&app, id), [0.0, 0.0, 1.0]), "colour kept");
    // Off the image nothing is picked, and it stays armed.
    toggle(&ctx, Target::ColorPicker { id });
    assert!(!pick_at(&mut app, &ctx, -5.0, 500.0));
    assert!(armed(&ctx).is_some());
    // Its dialog closed: forgotten.
    app.ui.dialogs.clear();
    pass(&ctx, Vec::new(), |ctx| begin_frame(&app, ctx));
    assert_eq!(armed(&ctx), None);
}

#[test]
fn a_press_on_the_canvas_picks_and_its_drag_reaches_no_tool() {
    let mut app = app("#00ff00");
    let ctx = Context::default();
    let id = color_picker_ui::open_for_command(&mut app, "Color Picker", [0.0, 0.0, 1.0], "edit.fill", json!({}));
    let canvas = Rect::from_min_size(Pos2::ZERO, egui::vec2(64.0, 64.0));
    let to_doc = |p: Pos2| [f64::from(p.x), f64::from(p.y)];
    // Not armed: the canvas is the tool's.
    pass(&ctx, press(egui::pos2(20.0, 20.0)), |ctx| assert!(!canvas_frame(&mut app, ctx, canvas, to_doc)));
    pass(&ctx, vec![egui::Event::PointerButton { pos: egui::pos2(20.0, 20.0), button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }], |_| {});
    toggle(&ctx, Target::ColorPicker { id });
    // A press outside the canvas isn't a pick.
    pass(&ctx, press(egui::pos2(300.0, 300.0)), |ctx| assert!(!canvas_frame(&mut app, ctx, canvas, to_doc)));
    pass(&ctx, vec![egui::Event::PointerButton { pos: egui::pos2(300.0, 300.0), button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }], |_| {});
    assert!(armed(&ctx).is_some());
    pass(&ctx, press(egui::pos2(20.0, 20.0)), |ctx| assert!(canvas_frame(&mut app, ctx, canvas, to_doc)));
    assert!(close(picker_rgb(&app, id), [0.0, 1.0, 0.0]));
    assert!(owns_pointer(&ctx), "the held press stays the eyedropper's");
    pass(&ctx, vec![egui::Event::PointerMoved(egui::pos2(30.0, 30.0))], |ctx| assert!(canvas_frame(&mut app, ctx, canvas, to_doc)));
    pass(&ctx, vec![egui::Event::PointerButton { pos: egui::pos2(30.0, 30.0), button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }], |_| {});
    pass(&ctx, Vec::new(), |ctx| assert!(!canvas_frame(&mut app, ctx, canvas, to_doc)));
    assert!(!owns_pointer(&ctx));
}

#[test]
fn a_phone_dialog_steps_aside_while_armed() {
    let mut app = app("#ff0000");
    let ctx = Context::default();
    let id = color_picker_ui::open_for_command(&mut app, "Color Picker", [0.0, 0.0, 1.0], "edit.fill", json!({}));
    toggle(&ctx, Target::ColorPicker { id });
    let phone = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(360.0, 780.0))), ..Default::default() };
    ctx.run_ui(phone.clone(), |ui| assert!(hides_dialog(ui.ctx(), id))).textures_delta.clear();
    assert!(pick_at(&mut app, &ctx, 1.0, 1.0));
    ctx.run_ui(phone, |ui| assert!(!hides_dialog(ui.ctx(), id), "back after the pick")).textures_delta.clear();
    toggle(&ctx, Target::ColorPicker { id });
    pass(&ctx, Vec::new(), |ctx| assert!(!hides_dialog(ctx, id), "a desktop's stays"));
}
