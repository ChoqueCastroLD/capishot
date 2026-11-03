//! Commands for hosts that sync a document live between several people and agents (Visoria's
//! multiplayer editing over the embed API and `capishot-cli serve`): cheap change detection
//! (`layer.pixelRevision`).

use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document".into())
}

fn object<'a>(p: &'a Value, cmd: &str) -> Result<Option<&'a serde_json::Map<String, Value>>> {
    match p {
        Value::Object(o) => Ok(Some(o)),
        Value::Null => Ok(None),
        _ => Err(bad(cmd, "params must be an object")),
    }
}

/// `layer.pixelRevision {layer?}` → `{layer, pixelRev}`; without `layer`, every layer's.
fn pixel_revision(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "layer.pixelRevision";
    let o = object(p, ID)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    match o.and_then(|o| o.get("layer")).filter(|v| !v.is_null()) {
        Some(v) => {
            let id = photocraft_doc::LayerId(v.as_u64().ok_or_else(|| bad(ID, "`layer` must be a layer id"))?);
            let rev = st.pixel_revs.revision(&st.doc, id).ok_or(EngineError::NoLayer(id))?;
            Ok(json!({"layer": id.0, "pixelRev": rev}))
        }
        None => {
            let revs = st.pixel_revs.revisions(&st.doc);
            let layers: Vec<Value> =
                st.doc.walk().into_iter().map(|(_, _, l)| json!({"layer": l.id.0, "pixelRev": revs.get(&l.id).copied().unwrap_or(0)})).collect();
            Ok(json!({"layers": layers}))
        }
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "layer.pixelRevision",
        label: "Layer Pixel Revision",
        menu: &[],
        shortcut: None,
        params: r##"{"layer":id?} → {layer, pixelRev} ({layers:[{layer, pixelRev}]} without `layer`): a counter that moves whenever the layer's pixels or layer mask change (strokes, fills, Remove, clears, undo, redo, jobs); also `pixelRev` on every layer of document.inspect"##,
        enabled: has_doc,
        journal: false,
        run: pixel_revision,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_revision_moves_with_a_stroke_and_with_undo() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
        let layer = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap();
        let rev = |s: &mut Session| s.execute("layer.pixelRevision", json!({"layer": layer})).unwrap()["pixelRev"].as_u64().unwrap();
        let r0 = rev(&mut s);
        s.execute("edit.fill", json!({"contents": "black"})).unwrap();
        let r1 = rev(&mut s);
        assert_ne!(r0, r1, "a fill moves it");
        let inspected = s.execute("document.inspect", json!({})).unwrap();
        assert_eq!(inspected["layers"][0]["pixelRev"], json!(r1), "inspect reports the same number");
        s.execute("layer.renameLayer", json!({"layer": layer, "name": "x"})).unwrap();
        assert_eq!(rev(&mut s), r1, "a rename doesn't");
        assert!(s.undo() && s.undo());
        assert_ne!(rev(&mut s), r1, "undoing the fill does");
        let all = s.execute("layer.pixelRevision", json!({})).unwrap();
        assert_eq!(all["layers"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn inspect_with_text_style_gives_runs_paragraphs_and_effect_settings() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 200, "height": 100})).unwrap();
        let layer = s.execute("type.create", json!({"text": "HI", "x": 10, "y": 50, "size": 24})).unwrap()["layer"].as_u64().unwrap();
        s.execute("layer.layerStyle.stroke", json!({"layer": layer, "size": 3, "color": "#ffffff", "add": true})).unwrap();
        let plain = s.execute("document.inspect", json!({})).unwrap();
        let node = plain["layers"].as_array().unwrap().iter().find(|l| l["id"] == json!(layer)).unwrap().clone();
        assert!(node["text"].get("style").is_none(), "opt-in only");
        assert!(node["effects"]["items"][0].get("fx").is_none());
        let full = s.execute("document.inspect", json!({"textStyle": true})).unwrap();
        let node = full["layers"].as_array().unwrap().iter().find(|l| l["id"] == json!(layer)).unwrap().clone();
        let style = &node["text"]["style"];
        assert_eq!(style["runs"][0]["start"], 0);
        assert_eq!(style["runs"][0]["end"], 2);
        assert!(style["runs"][0]["style"].is_object() && style["paragraphs"][0]["style"].is_object(), "{style}");
        let info = s.execute("type.info", json!({"layer": layer})).unwrap();
        assert_eq!(style["runs"], info["runs"], "the same runs as type.info");
        assert_eq!(node["effects"]["items"][0]["id"], "stroke");
        assert!(node["effects"]["items"][0]["fx"].is_object());
        assert!(s.execute("document.inspect", json!({"textStyle": 1})).is_err());
    }

    #[test]
    fn pixel_revision_refuses_bad_params() {
        let mut s = Session::new();
        assert!(s.execute("layer.pixelRevision", json!({})).is_err(), "no document");
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        assert!(s.execute("layer.pixelRevision", json!({"layer": "x"})).is_err());
        assert!(s.execute("layer.pixelRevision", json!({"layer": 999_999})).is_err());
        assert!(s.execute("layer.pixelRevision", json!([1])).is_err());
    }
}
