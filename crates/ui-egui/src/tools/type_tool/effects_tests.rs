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
