//! Phone dialogs and the on-screen keyboard: no field takes the focus when a dialog opens, and a
//! focused field keeps it while the keyboard shrinks the window (and when it grows back).

use egui::vec2;
use egui_kittest::Harness;
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::DialogKind;

type H = Harness<'static, PhotocraftApp>;

fn phone() -> H {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "en", "interface.simpleUi": "on"}})).unwrap();
    let mut h = Harness::builder().with_size(vec2(360.0, 780.0)).with_max_steps(64).with_step_dt(1.0 / 30.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    h.run_steps(4);
    h
}

fn desktop() -> H {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "en", "interface.simpleUi": "off"}})).unwrap();
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_max_steps(64).with_step_dt(1.0 / 30.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    h.run_steps(4);
    h
}

fn open_new_document(h: &mut H) {
    let fields = h.state_mut().new_document_fields();
    h.state_mut().ui.open_dialog(DialogKind::NewDocument, fields);
    h.run_steps(4);
}

/// A phone: New Document opens with no field focused (the keyboard stays down until a tap).
#[test]
fn a_phone_dialog_opens_without_a_focused_field() {
    let mut h = phone();
    open_new_document(&mut h);
    assert!(!h.ctx.text_edit_focused(), "no field focused on open: {:?}", h.ctx.memory(|m| m.focused()));
}

/// The desktop keeps Photoshop's behaviour: the dialog opens on its first value, selected.
#[test]
fn a_desktop_dialog_opens_on_its_first_field() {
    let mut h = desktop();
    open_new_document(&mut h);
    assert!(h.ctx.text_edit_focused(), "the first field is focused on open");
}

/// A phone: the command palette's search doesn't take the focus by itself either (it took it
/// back every frame, which bounced the keyboard).
#[test]
fn the_palette_search_waits_for_a_tap_on_a_phone() {
    let mut h = phone();
    h.state_mut().ui.palette_open = true;
    h.run_steps(4);
    assert!(h.state().ui.palette_open);
    assert!(!h.ctx.text_edit_focused(), "the palette's search isn't focused on open");
}

/// A field focused by a tap keeps the focus while the keyboard shrinks the window and gives the
/// room back.
#[test]
fn a_focused_field_keeps_the_focus_through_the_keyboard() {
    let mut h = phone();
    open_new_document(&mut h);
    // Tab moves the focus to the first field, as a tap on it does.
    h.key_press(egui::Key::Tab);
    h.run_steps(2);
    let id = h.ctx.memory(|m| m.focused());
    assert!(h.ctx.text_edit_focused(), "a field has the focus");
    for size in [vec2(360.0, 460.0), vec2(360.0, 780.0)] {
        h.set_size(size);
        h.run_steps(3);
        assert_eq!(h.ctx.memory(|m| m.focused()), id, "at {size:?}");
        assert!(h.ctx.text_edit_focused());
    }
}

/// A phone: Image Size, Canvas Size and the Color Picker open without a focused field too.
#[test]
fn value_dialogs_open_without_a_focused_field_on_a_phone() {
    for command in ["image.imageSize", "image.canvasSize"] {
        let mut h = phone();
        h.state_mut().run("file.new", json!({"width": 64, "height": 48})).unwrap();
        h.run_steps(2);
        crate::sizing::open(h.state_mut(), command).expect("a document");
        h.run_steps(6);
        assert!(!crate::field_tab::fields(&h.ctx).is_empty(), "{command} opened with its fields");
        assert!(!h.ctx.text_edit_focused(), "{command}: no field focused on open: {:?}", h.ctx.memory(|m| m.focused()));
    }
    let mut h = phone();
    crate::color_picker_ui::open(h.state_mut(), "foreground");
    h.run_steps(6);
    assert!(!h.ctx.text_edit_focused(), "colour picker: no field focused on open: {:?}", h.ctx.memory(|m| m.focused()));
}

/// The keyboard slides in over several frames: a phone's dialog waits for the window to settle
/// and then moves once, instead of following each frame of the slide.
#[test]
fn a_phone_dialog_moves_once_while_the_keyboard_slides_in() {
    let mut h = phone();
    open_new_document(&mut h);
    h.key_press(egui::Key::Tab);
    h.run_steps(2);
    let id = h.ctx.memory(|m| m.focused());
    let rect = |h: &H| crate::dialogs::rects(&h.ctx).first().copied().expect("the dialog");
    let mut places = vec![rect(&h).min];
    for step in 1..=8 {
        h.set_size(vec2(360.0, 780.0 - 40.0 * step as f32));
        h.run_steps(1);
        places.push(rect(&h).min);
    }
    h.run_steps(16);
    places.push(rect(&h).min);
    places.dedup_by(|a, b| (*a - *b).length() < 0.5);
    assert!(places.len() <= 2, "the dialog moved more than once: {places:?}");
    assert_eq!(h.ctx.memory(|m| m.focused()), id, "the field kept the focus");
    // The rect includes the frame's outer margin, a few points past the area egui keeps on screen.
    let r = rect(&h);
    assert!(r.bottom() <= h.ctx.content_rect().bottom() + 8.0, "the settled dialog sits above the keyboard: {r:?}");
}
