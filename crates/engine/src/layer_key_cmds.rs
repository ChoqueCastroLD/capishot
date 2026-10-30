//! Photoshop's Layers-panel keys that have no menu item (#217): ⇧⌘⌥N makes a new layer without the
//! New Layer dialog, and `/` toggles Lock Transparent Pixels on the selected layers. Like the brush
//! keys (`brush_key_cmds`) they are commands with default shortcuts, so they can be rebound in
//! Edit › Keyboard Shortcuts, recorded in actions and driven by agents.

use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document".into())
}

fn has_layer(s: &Session) -> std::result::Result<(), String> {
    let st = s.active().ok_or("no document")?;
    st.active_layer.filter(|id| st.doc.layer(*id).is_some()).map(|_| ()).ok_or_else(|| "no active layer".into())
}

fn object(p: &Value, cmd: &str) -> Result<()> {
    match p {
        Value::Object(_) | Value::Null => Ok(()),
        _ => Err(EngineError::BadParams { cmd: cmd.into(), msg: "expected an object".into() }),
    }
}

/// `layer.new.layerSilent`: Layer › New › Layer with the dialog's defaults.
fn new_layer_silent(s: &mut Session, p: &Value) -> Result<Value> {
    object(p, "layer.new.layerSilent")?;
    let new = crate::commands::find("layer.new.layer").ok_or_else(|| EngineError::Other("Layer › New › Layer is missing".into()))?;
    (new.run)(s, &json!({}))
}

/// `layer.toggleLockTransparency`: locks the selected layers' transparent pixels, or unlocks them
/// when every one is already locked. The Background (always locked) is left alone.
fn toggle_lock_transparency(s: &mut Session, p: &Value) -> Result<Value> {
    object(p, "layer.toggleLockTransparency")?;
    // An explicit `layer` (one layer), else the selected layers.
    let sel = match p.get("layer") {
        Some(_) => vec![crate::commands::layer_param(s, p)?],
        None => crate::layer_multi_cmds::selected(s),
    };
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let bg = st.doc.layers.first().filter(|l| st.doc.is_background(l.id)).map(|l| l.id);
    let ids: Vec<_> = sel.into_iter().filter(|id| Some(*id) != bg && st.doc.layer(*id).is_some()).collect();
    if ids.is_empty() {
        return Err(EngineError::Other("the Background's transparency can't be locked".into()));
    }
    let lock = !ids.iter().all(|id| st.doc.layer(*id).is_some_and(|l| l.locks.transparency));
    s.edit(if lock { "Lock Transparent Pixels" } else { "Unlock Transparent Pixels" }, |doc, _| {
        for id in &ids {
            doc.layer_mut(*id).ok_or(EngineError::NoLayer(*id))?.locks.transparency = lock;
        }
        Ok(())
    })?;
    Ok(json!({ "locked": lock, "layers": ids.len() }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "layer.new.layerSilent",
            label: "New Layer (No Dialog)",
            menu: &[],
            shortcut: Some("Cmd+Alt+Shift+N"),
            params: "{} → {layer} (a new empty layer above the active one, skipping the New Layer dialog)",
            enabled: has_doc,
            journal: true,
            run: new_layer_silent,
        },
        CommandSpec {
            id: "layer.toggleLockTransparency",
            label: "Toggle Lock Transparent Pixels",
            menu: &[],
            shortcut: Some("/"),
            params: "{layer:id?} → {locked, layers} (the layer, else the selected layers; unlocks when all are locked)",
            enabled: has_layer,
            journal: true,
            run: toggle_lock_transparency,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::LayerId;

    fn session() -> (Session, LayerId) {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 16, "height": 16})).unwrap();
        let id = LayerId(s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap());
        (s, id)
    }

    fn locked(s: &Session, id: LayerId) -> bool {
        s.active().unwrap().doc.layer(id).unwrap().locks.transparency
    }

    #[test]
    fn slash_toggles_lock_transparency_on_the_selected_layers() {
        let (mut s, a) = session();
        let b = LayerId(s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap());
        s.execute("layer.select", json!({"layer": a.0})).unwrap();
        s.execute("layer.lockLayers", json!({"transparency": true})).unwrap();
        assert!(locked(&s, a) && !locked(&s, b));
        s.execute("layer.select", json!({"layer": b.0, "mode": "add"})).unwrap();
        // Mixed: lock both. All locked: unlock both. One undo step each.
        let r = s.execute("layer.toggleLockTransparency", json!({})).unwrap();
        assert_eq!(r["locked"], true);
        assert!(locked(&s, a) && locked(&s, b));
        s.execute("layer.toggleLockTransparency", json!({})).unwrap();
        assert!(!locked(&s, a) && !locked(&s, b));
        assert!(s.undo());
        assert!(locked(&s, a) && locked(&s, b));
    }

    #[test]
    fn new_layer_silent_adds_a_layer_and_bad_input_fails() {
        let (mut s, a) = session();
        let r = s.execute("layer.new.layerSilent", json!({})).unwrap();
        let id = LayerId(r["layer"].as_u64().unwrap());
        assert_ne!(id, a);
        assert_eq!(s.active().unwrap().active_layer, Some(id));
        assert_eq!(crate::commands::find("layer.new.layerSilent").unwrap().shortcut, Some("Cmd+Alt+Shift+N"));
        for bad in [json!([1]), json!("x"), json!(3)] {
            assert!(s.execute("layer.new.layerSilent", bad.clone()).is_err(), "{bad}");
            assert!(s.execute("layer.toggleLockTransparency", bad.clone()).is_err(), "{bad}");
        }
        // Only the Background selected: nothing to lock.
        let bg = s.active().unwrap().doc.layers[0].id;
        s.execute("layer.select", json!({"layer": bg.0})).unwrap();
        assert!(s.execute("layer.toggleLockTransparency", json!({})).is_err());
        let mut empty = Session::new();
        assert!(empty.execute("layer.new.layerSilent", json!({})).is_err());
        assert!(empty.execute("layer.toggleLockTransparency", json!({})).is_err());
    }
}
