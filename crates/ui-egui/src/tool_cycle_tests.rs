//! Tool letters: from another group a letter picks that group's tool; repeated, it steps to the
//! next tool of the group and ⇧+letter to the previous one, both wrapping (Repeat the Letter to
//! Switch Tools, on by default); off, Photoshop's classic keys. A ⇧+letter bound to a command
//! keeps running it. The toast shows the previous, current and next tool and animates. And no
//! single-key shortcut ever fires while text has the keyboard: the letters go into the text.

use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;

type H = Harness<'static, PhotocraftApp>;

fn app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "en"}})).unwrap();
    app.run("file.new", json!({"width": 600, "height": 400, "background": "white"})).unwrap();
    app.sync_views();
    app
}

fn harness(app: PhotocraftApp) -> H {
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 860.0)).with_max_steps(64).with_step_dt(1.0 / 30.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    for _ in 0..4 {
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

fn wait(h: &mut H, secs: f64) {
    for _ in 0..(secs * 30.0).ceil() as usize {
        frame(h, vec![]);
    }
}

/// A letter typed (its key press and release, and its text), with ⇧ or not.
fn letter(c: char, shift: bool) -> Vec<Event> {
    let key = Key::from_name(&c.to_ascii_uppercase().to_string()).unwrap();
    let modifiers = if shift { Modifiers::SHIFT } else { Modifiers::NONE };
    let text = if shift { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() };
    vec![
        Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers },
        Event::Text(text.to_string()),
        Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers },
    ]
}

fn press(h: &mut H, c: char, shift: bool) {
    if shift {
        frame(h, vec![Event::ModifiersChanged(Modifiers::SHIFT)]);
    }
    frame(h, letter(c, shift));
    if shift {
        frame(h, vec![Event::ModifiersChanged(Modifiers::NONE)]);
    }
    frame(h, vec![]);
}

const J: [Tool; 8] =
    [Tool::Remove, Tool::RemoveAi, Tool::BubbleTextRemover, Tool::SpotHealing, Tool::Healing, Tool::Patch, Tool::ContentAwareMove, Tool::RedEye];

#[test]
fn j_repeated_steps_through_the_group_and_wraps_and_shift_j_goes_back() {
    assert!(photocraft_engine::Session::new().prefs().tools.repeat_letter_cycles, "on by default");
    let mut h = harness(app());
    h.state_mut().ui.tool = Tool::Remove;
    frame(&mut h, vec![]);
    for k in 1..=J.len() {
        press(&mut h, 'j', false);
        assert_eq!(h.state().ui.tool, J[k % J.len()], "J #{k}");
    }
    assert_eq!(h.state().ui.tool, Tool::Remove, "wrapped round");
    press(&mut h, 'j', true);
    assert_eq!(h.state().ui.tool, Tool::RedEye, "⇧J goes back, wrapping");
    press(&mut h, 'j', true);
    assert_eq!(h.state().ui.tool, Tool::ContentAwareMove);
}

#[test]
fn the_first_letter_from_another_group_picks_that_groups_tool() {
    let mut h = harness(app());
    h.state_mut().ui.tool = Tool::BubbleTextRemover;
    frame(&mut h, vec![]);
    press(&mut h, 'b', false);
    assert_eq!(h.state().ui.tool, Tool::Brush);
    press(&mut h, 'j', false);
    assert_eq!(h.state().ui.tool, Tool::BubbleTextRemover, "the J group's last tool, not the next");
    press(&mut h, 'b', true);
    assert_eq!(h.state().ui.tool, Tool::Brush, "⇧B from another group picks too, without stepping");
}

#[test]
fn with_the_preference_off_the_classic_keys_apply() {
    let mut h = harness(app());
    h.state_mut().session.edit_prefs(|p| p.tools.repeat_letter_cycles = false);
    h.state_mut().ui.tool = Tool::Remove;
    frame(&mut h, vec![]);
    // Photoshop's default, Use Shift Key for Tool Switch: J keeps the tool, ⇧J steps forward.
    press(&mut h, 'j', false);
    assert_eq!(h.state().ui.tool, Tool::Remove);
    press(&mut h, 'j', true);
    assert_eq!(h.state().ui.tool, Tool::RemoveAi, "⇧J forward");
    // And without it, J steps forward.
    h.state_mut().session.edit_prefs(|p| p.tools.use_shift_key_for_tool_switch = false);
    press(&mut h, 'j', false);
    assert_eq!(h.state().ui.tool, Tool::BubbleTextRemover);
}

/// The letters whose bare ⇧ combination is a shortcut by default (listed in the report).
#[test]
fn shift_letters_bound_to_commands_are_listed_and_skipped() {
    let a = app();
    let conflicts: Vec<char> = Tool::ALL
        .iter()
        .map(|t| t.key())
        .filter(|c| *c != '\0')
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|c| crate::shortcuts::shift_conflicts(&a, crate::spring_tools::key_of(*c).unwrap()))
        .collect();
    eprintln!("tool letters whose bare Shift combination is bound: {conflicts:?}");
    // A command on ⇧J: ⇧J runs it and the J group doesn't step back.
    let mut h = harness(app());
    h.state_mut().session.prefs.edit(|p| p.shortcuts.insert("select.all".into(), "Shift+J".into()));
    assert!(crate::shortcuts::shift_conflicts(h.state(), Key::J));
    h.state_mut().ui.tool = Tool::Patch;
    frame(&mut h, vec![]);
    press(&mut h, 'j', true);
    assert_eq!(h.state().ui.tool, Tool::Patch, "no step back");
    assert!(h.state().session.active().unwrap().doc.selection.is_some(), "the command ran");
    press(&mut h, 'j', false);
    assert_eq!(h.state().ui.tool, Tool::ContentAwareMove, "J still steps forward");
}

#[test]
fn the_toast_shows_the_neighbours_and_animates() {
    let mut h = harness(app());
    h.state_mut().ui.tool = Tool::RemoveAi;
    frame(&mut h, vec![]);
    assert!(crate::tool_toast::view(&h.ctx).is_none(), "nothing yet");
    press(&mut h, 'j', false);
    let v = crate::tool_toast::view(&h.ctx).expect("the toast");
    assert_eq!(v.current.as_deref(), Some("Bubble Text Remover Tool"));
    assert_eq!(v.prev.as_deref(), Some("Remove with AI Tool"));
    assert_eq!(v.next.as_deref(), Some("Spot Healing Brush Tool"));
    assert!(v.opacity < 1.0 && v.rise > 0.0, "fading and sliding in: {v:?}");
    wait(&mut h, 0.4);
    let v = crate::tool_toast::view(&h.ctx).unwrap();
    assert!(v.opacity > 0.99 && v.rise < 0.1, "in place: {v:?}");
    // Again while it shows: a sideways change, from the old tool, forward.
    press(&mut h, 'j', false);
    let v = crate::tool_toast::view(&h.ctx).unwrap();
    assert_eq!(v.current.as_deref(), Some("Spot Healing Brush Tool"));
    assert!(v.change < 1.0 && v.dir > 0.0 && v.changing_from.as_deref() == Some("Bubble Text Remover Tool"), "crossfading forward: {v:?}");
    assert!(v.opacity > 0.99, "it stays up while changing");
    press(&mut h, 'j', true);
    assert!(crate::tool_toast::view(&h.ctx).unwrap().dir < 0.0, "backwards slides the other way");
    wait(&mut h, 0.5);
    assert_eq!(crate::tool_toast::view(&h.ctx).unwrap().change, 1.0, "the change settles");
    // Idle: it fades out and goes.
    wait(&mut h, 1.1);
    let v = crate::tool_toast::view(&h.ctx).unwrap();
    assert!(v.opacity < 1.0, "fading out: {v:?}");
    wait(&mut h, 0.5);
    assert!(crate::tool_toast::view(&h.ctx).is_none(), "gone");
}

#[test]
fn with_animations_off_the_toast_shows_at_once() {
    let mut h = harness(app());
    h.state_mut().session.edit_prefs(|p| p.interface.enable_animations = false);
    h.state_mut().ui.tool = Tool::RemoveAi;
    frame(&mut h, vec![]);
    frame(&mut h, vec![]);
    press(&mut h, 'j', false);
    let v = crate::tool_toast::view(&h.ctx).unwrap();
    assert!(v.opacity == 1.0 && v.rise == 0.0, "{v:?}");
    wait(&mut h, 1.6);
    assert!(crate::tool_toast::view(&h.ctx).is_none());
}

/// Every place text can be typed: J, ⇧J, B, V, T and M change no tool (nor run anything) and
/// go into the text where it can be read back.
#[test]
fn letters_typed_into_text_never_switch_tools() {
    let keys: [(char, bool); 6] = [('j', false), ('j', true), ('b', false), ('v', false), ('t', false), ('m', false)];
    let typed = "jJbvtm";
    let type_all = |h: &mut H| {
        for (c, shift) in keys {
            press(h, c, shift);
        }
    };
    // 1. A type layer being edited.
    {
        let mut a = app();
        a.run("type.create", json!({"text": "x", "x": 50, "y": 100, "size": 40})).unwrap();
        a.ui.tool = Tool::Type;
        crate::type_tool::edit_active(&mut a).unwrap();
        let mut h = harness(a);
        type_all(&mut h);
        assert_eq!(h.state().ui.tool, Tool::Type, "type editing: no tool switch");
        let st = h.state().session.active().unwrap();
        let id = st.active_layer.unwrap();
        let text = match &st.doc.layer(id).unwrap().content {
            photocraft_doc::LayerContent::Text(t) => t.text.clone(),
            _ => panic!(),
        };
        assert_eq!(text, typed, "the letters went into the type");
    }
    // 2. The palette's search field.
    {
        let mut h = harness(app());
        h.state_mut().ui.tool = Tool::Brush;
        h.state_mut().ui.palette_open = true;
        frame(&mut h, vec![]);
        frame(&mut h, vec![]);
        type_all(&mut h);
        assert_eq!(h.state().ui.tool, Tool::Brush, "palette: no tool switch");
        let q: String = h.ctx.data(|d| d.get_temp(egui::Id::unique("palette-query"))).unwrap_or_default();
        assert_eq!(q, typed, "the letters went into the search");
    }
    // 3. An input method composing (a pre-edit under way).
    {
        let mut h = harness(app());
        h.state_mut().ui.tool = Tool::Brush;
        frame(&mut h, vec![Event::Ime(egui::ImeEvent::Preedit { text: "ka".into(), active_range_chars: None })]);
        type_all(&mut h);
        assert_eq!(h.state().ui.tool, Tool::Brush, "IME: no tool switch");
        frame(&mut h, vec![Event::Ime(egui::ImeEvent::Commit("か".into()))]);
        press(&mut h, 'v', false);
        assert_eq!(h.state().ui.tool, Tool::Move, "after the commit the letters are tools again");
    }
    // 4. A text input outside the app (the web page's own field).
    {
        let mut h = harness(app());
        h.state_mut().ui.tool = Tool::Brush;
        h.state_mut().external_text_focus = true;
        type_all(&mut h);
        assert_eq!(h.state().ui.tool, Tool::Brush, "a page field: no tool switch");
    }
    // 5. Any egui text field with focus (a dialog's, a panel's, a rename, Preferences…).
    {
        let mut a = app();
        a.ui.tool = Tool::Brush;
        let mut h = Harness::builder().with_size(egui::vec2(800.0, 600.0)).with_accessibility_check(false).build_ui_state(
            |ui, app: &mut PhotocraftApp| {
                let id = egui::Id::unique("a-field");
                let mut s: String = ui.data(|d| d.get_temp(id)).unwrap_or_default();
                let r = ui.add(egui::TextEdit::singleline(&mut s).id_salt("field"));
                if !ui.data(|d| d.get_temp::<bool>(id.with("once")).unwrap_or(false)) {
                    r.request_focus();
                    ui.data_mut(|d| d.insert_temp(id.with("once"), true));
                }
                ui.data_mut(|d| d.insert_temp(id, s));
                crate::shortcuts::handle(app, ui.ctx());
            },
            a,
        );
        h.run_steps(3);
        for (c, shift) in keys {
            for e in letter(c, shift) {
                h.event(e);
            }
            h.run_steps(2);
        }
        assert_eq!(h.state().ui.tool, Tool::Brush, "a text field: no tool switch");
        let s: String = h.ctx.data(|d| d.get_temp(egui::Id::unique("a-field"))).unwrap_or_default();
        assert_eq!(s, typed, "the letters went into the field");
    }
}

/// On a phone, picking a tool from the bars shows the toast too: above the bars, clear of every
/// other floating control, inside the screen.
#[test]
fn on_a_phone_a_bar_pick_shows_the_toast_clear_of_the_controls() {
    let mut a = app();
    a.run("prefs.set", json!({"values": {"interface.simpleUi": "on"}})).unwrap();
    a.run("select.rect", json!({"x": 50, "y": 50, "width": 200, "height": 100})).unwrap();
    a.ui.tool = Tool::Move;
    let mut h = Harness::builder().with_size(egui::vec2(390.0, 844.0)).with_max_steps(64).with_step_dt(1.0 / 30.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        a
    });
    for _ in 0..4 {
        frame(&mut h, vec![]);
    }
    let s = crate::simple_ui::inspect(h.state(), &h.ctx);
    let slot = s["buttons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["label"].as_str().is_some_and(|l| l.starts_with("slot:tool:") && l != "slot:tool:Move"))
        .unwrap()
        .clone();
    let r: [f32; 4] = serde_json::from_value(slot["rect"].clone()).unwrap();
    let p = egui::pos2((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
    let t = |phase| Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(4), phase, pos: p, force: None };
    frame(&mut h, vec![t(egui::TouchPhase::Start)]);
    frame(&mut h, vec![t(egui::TouchPhase::End)]);
    wait(&mut h, 0.4);
    let v = crate::tool_toast::view(&h.ctx).expect("the toast on a bar pick");
    assert!(v.current.is_some(), "{v:?}");
    let toast = crate::touch_ui::noted_rect(&h.ctx, "toolToast").expect("noted");
    let screen = h.ctx.content_rect();
    assert!(screen.contains_rect(toast), "inside the screen: {toast:?}");
    let tch = crate::touch_ui::inspect(h.state(), &h.ctx);
    let s = crate::simple_ui::inspect(h.state(), &h.ctx);
    for b in s["buttons"].as_array().into_iter().flatten().chain(tch["rects"].as_array().into_iter().flatten()) {
        let l = b["label"].as_str().unwrap_or_default();
        if l == "toolToast" || l.starts_with("sel:") || l.starts_with("layer:") || matches!(l, "selectionBarMore" | "brushPreview" | "loupe" | "sliderBubble") {
            continue;
        }
        let v: [f32; 4] = serde_json::from_value(b["rect"].clone()).unwrap();
        let r = egui::Rect::from_min_max(egui::pos2(v[0], v[1]), egui::pos2(v[2], v[3]));
        assert!(!r.intersects(toast), "the toast {toast:?} overlaps {l} {r:?}");
    }
    let capi = crate::simple_ui::noted_rect(&h.ctx, "capi").unwrap();
    assert!(toast.bottom() <= capi.top(), "above the bars");
}
