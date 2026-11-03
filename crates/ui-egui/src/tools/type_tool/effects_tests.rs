//! The type bars' Efectos: gradient, shadow and glow made, edited (never duplicated), hidden and
//! shown again on the type target, one undo step per popover session.

use photocraft_doc::{Effect, LayerId};
use serde_json::json;

use super::{KINDS, PRESETS, set_effect, shown};
use crate::PhotocraftApp;

fn new_app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 400, "height": 200})).unwrap();
    app.sync_views();
    app.ui.tool = crate::state::Tool::Type;
    app
}

fn type_new(app: &mut PhotocraftApp, s: &str) -> LayerId {
    super::super::pointer_up(app, [50.0, 100.0], [50.0, 100.0]);
    super::super::insert(app, s);
    LayerId(app.ui.text_edit.as_ref().unwrap().layer)
}

fn effects(app: &PhotocraftApp, id: LayerId) -> Vec<Effect> {
    app.session.active().unwrap().doc.layer(id).unwrap().effects.items.clone()
}

fn count(app: &PhotocraftApp, id: LayerId, kind: &str) -> usize {
    effects(app, id).iter().filter(|e| photocraft_engine::layer_style::kind_of(e) == kind).count()
}

#[test]
fn each_effect_is_made_edited_hidden_and_shown_in_one_undo_step() {
    let mut app = new_app();
    let id = type_new(&mut app, "¡Fuego!");
    let past = app.session.active().unwrap().history.past_len();
    for (kind, _) in KINDS {
        let (_, mut p, on) = shown(&app, kind).unwrap();
        assert!(!on, "{kind}: none yet");
        set_effect(&mut app, kind, &p, true, "fx1").unwrap();
        assert_eq!(count(&app, id, kind), 1, "{kind}: made");
        p["opacity"] = json!(40);
        set_effect(&mut app, kind, &p, true, "fx1").unwrap();
        assert_eq!(count(&app, id, kind), 1, "{kind}: edited, not duplicated");
        let (_, shown_now, on) = shown(&app, kind).unwrap();
        assert!(on && shown_now["opacity"].as_f64() == Some(40.0), "{kind}: {shown_now}");
        set_effect(&mut app, kind, &shown_now, false, "fx1").unwrap();
        assert!(!shown(&app, kind).unwrap().2, "{kind}: off hides it");
        assert_eq!(count(&app, id, kind), 1, "{kind}: hidden keeps its settings");
        set_effect(&mut app, kind, &shown_now, true, "fx1").unwrap();
        assert!(shown(&app, kind).unwrap().2, "{kind}: on again");
    }
    assert_eq!(app.session.active().unwrap().history.past_len(), past + 1, "one undo step for the session");
    app.run("edit.undo", json!({})).unwrap();
    assert!(effects(&app, id).is_empty(), "one undo takes all three back");
}

#[test]
fn a_gradient_preset_sets_both_colours_and_the_shadow_keeps_its_own_angle() {
    let mut app = new_app();
    let id = type_new(&mut app, "Océano");
    let (_, mut p, _) = shown(&app, "gradientOverlay").unwrap();
    p["from"] = json!(PRESETS[1].1);
    p["to"] = json!(PRESETS[1].2);
    p["style"] = json!("radial");
    set_effect(&mut app, "gradientOverlay", &p, true, "g").unwrap();
    let (_, g, _) = shown(&app, "gradientOverlay").unwrap();
    assert_eq!((g["from"].as_str(), g["to"].as_str(), g["style"].as_str()), (Some("#0a3d91"), Some("#36d1dc"), Some("radial")), "{g}");
    let (_, mut d, _) = shown(&app, "dropShadow").unwrap();
    d["angle"] = json!(45);
    set_effect(&mut app, "dropShadow", &d, true, "d").unwrap();
    let shadow = effects(&app, id).into_iter().find_map(|e| if let Effect::DropShadow(s) = e { Some(s) } else { None }).unwrap();
    assert!(!shadow.use_global_light && (shadow.angle - 45.0).abs() < 0.01, "{shadow:?}");
}

/// Runs `f` in one frame at `time` (s), the pointer held down or not.
fn frame(ctx: &egui::Context, time: f64, down: bool, f: impl FnOnce(&egui::Ui)) {
    let mut input = egui::RawInput { time: Some(time), ..Default::default() };
    input.events.push(egui::Event::PointerMoved(egui::pos2(10.0, 10.0)));
    input.events.push(egui::Event::PointerButton { pos: egui::pos2(10.0, 10.0), button: egui::PointerButton::Primary, pressed: down, modifiers: Default::default() });
    let mut f = Some(f);
    ctx.run_ui(input, |ui| {
        if let Some(f) = f.take() {
            f(ui);
        }
    })
    .textures_delta
    .clear();
}

#[test]
fn a_drag_applies_at_most_every_120_ms_and_its_last_value_on_release() {
    use super::{pending, throttle};
    let ctx = egui::Context::default();
    let id = egui::Id::unique("fx-throttle-test");
    let mut applied = Vec::new();
    // Pointer held: the first change applies, the next ones within 120 ms wait.
    for (i, t) in [0.0, 0.03, 0.06, 0.09, 0.13, 0.16].into_iter().enumerate() {
        frame(&ctx, t, true, |ui| {
            if throttle(ui, id, &0, &(i + 1)) {
                applied.push(i + 1);
            }
        });
    }
    assert_eq!(applied, vec![1, 5], "one apply per 120 ms while dragging");
    frame(&ctx, 0.17, true, |ui| assert_eq!(pending::<usize>(ui, id), Some(6), "the last value waits"));
    // Released: the waiting value applies at once.
    frame(&ctx, 0.18, false, |ui| {
        let p = pending::<usize>(ui, id).unwrap();
        assert!(throttle(ui, id, &5, &p));
        assert_eq!(pending::<usize>(ui, id), None);
    });
}

/// Efectos' eyedroppers: a gradient stop, the shadow and the glow take the image's colour where it
/// was pressed, the effect made and shown; a cancelled one leaves the effect alone.
#[test]
fn the_effect_eyedroppers_take_the_image_colour() {
    let mut app = new_app();
    app.run("edit.fill", json!({"color": "#22aa44"})).unwrap();
    let id = type_new(&mut app, "Hola");
    let ctx = egui::Context::default();
    for (kind, field) in [("gradientOverlay", "to"), ("dropShadow", "color"), ("outerGlow", "color")] {
        crate::eyedrop::toggle(&ctx, crate::eyedrop::Target::TypeEffect { kind: kind.into(), field: field.into(), key: "fx-drop".into() });
        assert!(crate::eyedrop::pick_at(&mut app, &ctx, 300.0, 20.0), "{kind}");
        let (_, p, on) = shown(&app, kind).unwrap();
        assert!(on, "{kind}: shown");
        assert_eq!(p[field].as_str(), Some("#22aa44"), "{kind}: {p}");
        assert_eq!(count(&app, id, kind), 1);
    }
    let before = effects(&app, id);
    crate::eyedrop::toggle(&ctx, crate::eyedrop::Target::TypeEffect { kind: "dropShadow".into(), field: "color".into(), key: "fx-drop".into() });
    crate::eyedrop::cancel(&ctx);
    assert!(!crate::eyedrop::pick_at(&mut app, &ctx, 300.0, 20.0));
    assert_eq!(effects(&app, id), before, "cancel restores (nothing changed)");
}
