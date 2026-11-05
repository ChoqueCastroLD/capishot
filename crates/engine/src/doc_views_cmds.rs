//! Named views of a document (`document.views.*`): which layers a host shows for a page ("raw",
//! "cleaned", "typeset", …), switched in one step that is no history step.
//!
//! A view is a layer comp (Window › Layer Comps, [`photocraft_doc::LayerComp`]) that restores
//! visibility only, for the layers it lists: it is saved in the PSD and `.pcraft` like any comp
//! and shows in the Layer Comps panel. Defining and applying one run with `history: "none"`
//! ([`crate::HistoryMode::None`]): the user's undo and redo never take a view back, and each
//! history state keeps its own pixels (only the visibility is carried into it). Applying changes
//! visibility only, so the canvas recomposites the layers it already holds: no type is laid out
//! or rasterized again.
//!
//! A host can also show a view without applying it ([`view_document`]): a copy of the document
//! with the view's visibility, sharing every layer's pixels, for a canvas of its own beside the
//! document as it is. The document's own `layer.visible` stays as it was.

use photocraft_doc::comps::next_comp_id;
use photocraft_doc::{CompLayerState, Document, LayerComp, LayerId};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, HistoryMode, Result, Session};

/// The most layers one view may list.
const MAX_LAYERS: usize = 100_000;

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn name_param<'a>(p: &'a Value, cmd: &str) -> Result<&'a str> {
    match p.get("name") {
        Some(Value::String(n)) if !n.trim().is_empty() => Ok(n),
        Some(Value::String(_)) => Err(bad(cmd, "`name` must not be empty")),
        _ => Err(bad(cmd, "missing `name` (the view's name)")),
    }
}

/// Each layer's slash-joined path of names from the top level (`"Text/Bubble 3"`), with its id.
fn paths(doc: &Document) -> Vec<(String, LayerId)> {
    fn rec(layers: &[photocraft_doc::Layer], prefix: &str, out: &mut Vec<(String, LayerId)>, depth: usize) {
        if depth > photocraft_doc::MAX_GROUP_DEPTH {
            return;
        }
        // Top-down, so a name matches the topmost layer that has it.
        for l in layers.iter().rev() {
            let path = if prefix.is_empty() { l.name.clone() } else { format!("{prefix}/{}", l.name) };
            out.push((path.clone(), l.id));
            if let Some(ch) = l.children() {
                rec(ch, &path, out, depth + 1);
            }
        }
    }
    let mut out = Vec::new();
    rec(&doc.layers, "", &mut out, 0);
    out
}

/// The layers `key` lists: ids, or names (a layer's own name, or its group path `"Group/Name"`;
/// a group stands for itself, so its children show or hide with it).
fn layer_list(doc: &Document, p: &Value, key: &str, cmd: &str, paths: &[(String, LayerId)]) -> Result<Vec<LayerId>> {
    let items = match p.get(key) {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) if a.len() <= MAX_LAYERS => a,
        Some(Value::Array(_)) => return Err(bad(cmd, format!("`{key}` lists more than {MAX_LAYERS} layers"))),
        Some(_) => return Err(bad(cmd, format!("`{key}` must be a list of layer ids or names"))),
    };
    items
        .iter()
        .map(|v| match v {
            Value::Number(n) => {
                let id = n.as_u64().map(LayerId).ok_or_else(|| bad(cmd, format!("layer id {n} is not a non-negative integer")))?;
                doc.layer(id).map(|_| id).ok_or_else(|| bad(cmd, format!("no layer {n}")))
            }
            Value::String(s) => paths
                .iter()
                .find(|(path, _)| path == s)
                .or_else(|| paths.iter().find(|(path, _)| path.rsplit('/').next() == Some(s.as_str())))
                .map(|(_, id)| *id)
                .ok_or_else(|| bad(cmd, format!("no layer named \"{s}\""))),
            other => Err(bad(cmd, format!("`{key}` items must be layer ids or names, not {other}"))),
        })
        .collect()
}

/// Run `f` on the active document as an edit without a history step (see the module docs).
fn quiet_edit<R>(s: &mut Session, label: &str, f: impl FnOnce(&mut Document) -> Result<R>) -> Result<R> {
    let scope = s.begin_scope(None, HistoryMode::None)?;
    let r = s.edit(label, |doc, _| f(doc));
    s.end_scope(scope);
    r
}

/// A number that changes when view `name` of `doc` shows other layers; `None` without such a view.
pub fn view_signature(doc: &Document, name: &str) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let comp = doc.layer_comps.iter().find(|c| c.name == name)?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for st in &comp.states {
        (st.layer.0, st.visible).hash(&mut h);
    }
    Some(h.finish())
}

/// `doc` as view `name` shows it: a copy with the view's visibility (layers it doesn't list keep
/// theirs, as `document.views.apply` leaves them), sharing the pixels. `doc` isn't touched and
/// nothing reaches the history. `None` without such a view.
pub fn view_document(doc: &Document, name: &str) -> Option<Document> {
    let comp = doc.layer_comps.iter().find(|c| c.name == name)?;
    let mut shown = doc.clone();
    for st in &comp.states {
        if let (Some(v), Some(l)) = (st.visible, shown.layer_mut(st.layer)) {
            l.visible = v;
        }
    }
    Some(shown)
}

fn define(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "document.views.define";
    let name = name_param(p, CMD)?.to_string();
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let paths = paths(&d.doc);
    let visible = layer_list(&d.doc, p, "visible", CMD, &paths)?;
    let hidden = layer_list(&d.doc, p, "hidden", CMD, &paths)?;
    if visible.is_empty() && hidden.is_empty() {
        return Err(bad(CMD, "a view lists layers in `visible` or `hidden`"));
    }
    if let Some(both) = visible.iter().find(|id| hidden.contains(id)) {
        return Err(bad(CMD, format!("layer {} is in both `visible` and `hidden`", both.0)));
    }
    let state = |layer: LayerId, v: bool| CompLayerState { layer, visible: Some(v), position: None, appearance: None };
    let states: Vec<CompLayerState> = visible.iter().map(|id| state(*id, true)).chain(hidden.iter().map(|id| state(*id, false))).collect();
    let layers = states.len();
    let id = quiet_edit(s, "Define View", |doc| {
        let comp = match doc.layer_comps.iter_mut().find(|c| c.name == name) {
            Some(c) => c,
            None => {
                let id = next_comp_id(doc);
                doc.layer_comps.push(LayerComp {
                    id,
                    name: name.clone(),
                    comment: String::new(),
                    apply_visibility: true,
                    apply_position: false,
                    apply_appearance: false,
                    states: Vec::new(),
                });
                doc.layer_comps.last_mut().ok_or_else(|| EngineError::Other("the view was not added".into()))?
            }
        };
        comp.apply_visibility = true;
        comp.apply_position = false;
        comp.apply_appearance = false;
        comp.states = states;
        Ok(comp.id)
    })?;
    Ok(json!({"name": name, "comp": id, "layers": layers}))
}

fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "document.views.apply";
    let name = name_param(p, CMD)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let comp = d.doc.layer_comps.iter().find(|c| c.name == name).cloned().ok_or_else(|| bad(CMD, format!("no view named \"{name}\"")))?;
    let changed = comp.states.iter().filter(|st| st.visible.is_some_and(|v| d.doc.layer(st.layer).is_some_and(|l| l.visible != v))).count();
    let missing = comp.missing_layers(&d.doc).len();
    if changed > 0 || d.doc.last_applied_comp != Some(comp.id) {
        quiet_edit(s, "Apply View", |doc| {
            // Visibility only, whatever the comp's other options say (a view never moves layers).
            for st in &comp.states {
                if let (Some(v), Some(l)) = (st.visible, doc.layer_mut(st.layer)) {
                    l.visible = v;
                }
            }
            doc.last_applied_comp = Some(comp.id);
            Ok(())
        })?;
    }
    Ok(json!({"name": comp.name, "comp": comp.id, "changed": changed, "missingLayers": missing}))
}

fn list(s: &mut Session, _p: &Value) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let doc = &d.doc;
    let views: Vec<Value> = doc
        .layer_comps
        .iter()
        .filter(|c| c.apply_visibility && !c.apply_position && !c.apply_appearance)
        .map(|c| {
            let of = |v: bool| c.states.iter().filter(|st| st.visible == Some(v)).map(|st| st.layer.0).collect::<Vec<_>>();
            json!({"name": c.name, "comp": c.id, "visible": of(true), "hidden": of(false), "applied": doc.last_applied_comp == Some(c.id)})
        })
        .collect();
    Ok(json!({"views": views}))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "document.views.delete";
    let name = name_param(p, CMD)?.to_string();
    let d = s.active().ok_or(EngineError::NoDocument)?;
    if !d.doc.layer_comps.iter().any(|c| c.name == name) {
        return Err(bad(CMD, format!("no view named \"{name}\"")));
    }
    quiet_edit(s, "Delete View", |doc| {
        let gone: Vec<u32> = doc.layer_comps.iter().filter(|c| c.name == name).map(|c| c.id).collect();
        doc.layer_comps.retain(|c| c.name != name);
        if doc.last_applied_comp.is_some_and(|id| gone.contains(&id)) {
            doc.last_applied_comp = None;
        }
        Ok(())
    })?;
    Ok(json!({"name": name}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "document.views.define",
            label: "Define View",
            menu: &[],
            shortcut: None,
            params: r##"{"name":str,"visible":[layer id | name | "Group/Name"]?,"hidden":[…]?} → {name, comp, layers}: a named view, kept as a layer comp that restores the visibility of the layers it lists (a group stands for its contents); replaces a view of the same name. No history step"##,
            enabled: has_doc,
            journal: false,
            run: define,
        },
        CommandSpec {
            id: "document.views.apply",
            label: "Apply View",
            menu: &[],
            shortcut: None,
            params: r##"{"name":str} → {name, comp, changed, missingLayers}: shows and hides the view's layers in one change, no history step (undo and redo keep it); visibility only, so nothing is re-rasterized"##,
            enabled: has_doc,
            journal: false,
            run: apply,
        },
        CommandSpec {
            id: "document.views.list",
            label: "List Views",
            menu: &[],
            shortcut: None,
            params: r##"{} → {views:[{name, comp, visible:[id], hidden:[id], applied}]}: the document's views (layer comps that restore visibility only)"##,
            enabled: has_doc,
            journal: false,
            run: list,
        },
        CommandSpec {
            id: "document.views.delete",
            label: "Delete View",
            menu: &[],
            shortcut: None,
            params: r##"{"name":str} → {name}: removes the view (its layer comp). No history step"##,
            enabled: has_doc,
            journal: false,
            run: delete,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> (Session, u64, u64, u64) {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 64, "height": 64})).expect("a document");
        let raw = s.execute("layer.new.layer", json!({"name": "raw"})).expect("a layer")["layer"].as_u64().expect("an id");
        let group = s.execute("layer.new.group", json!({"name": "Text"})).expect("a group")["layer"].as_u64().expect("an id");
        let text = s.execute("type.create", json!({"text": "Hi", "x": 4, "y": 30, "size": 12})).expect("type")["layer"].as_u64().expect("an id");
        (s, raw, group, text)
    }

    fn visible(s: &Session, id: u64) -> bool {
        s.active().and_then(|d| d.doc.layer(LayerId(id))).is_some_and(|l| l.visible)
    }

    #[test]
    fn a_view_document_shows_the_view_and_leaves_the_document_alone() {
        let (mut s, raw, group, _) = session();
        s.execute("document.views.define", json!({"name": "raw", "visible": [raw], "hidden": ["Text"]})).expect("define");
        s.execute("document.views.define", json!({"name": "typeset", "hidden": [raw]})).expect("define");
        let revision = s.active().map(|d| d.revision);
        let doc = s.active().expect("a document").doc.clone();
        let shown = view_document(&doc, "raw").expect("the view");
        assert!(shown.layer(LayerId(raw)).is_some_and(|l| l.visible));
        assert!(shown.layer(LayerId(group)).is_some_and(|l| !l.visible));
        assert!(visible(&s, group), "the document's own visibility stays");
        assert_eq!(s.active().map(|d| d.revision), revision, "no edit");
        assert!(view_document(&doc, "nope").is_none() && view_signature(&doc, "nope").is_none());
        assert_ne!(view_signature(&doc, "raw"), view_signature(&doc, "typeset"));
        s.execute("document.views.define", json!({"name": "raw", "visible": ["Text"]})).expect("redefine");
        let redefined = s.active().expect("a document").doc.clone();
        assert_ne!(view_signature(&doc, "raw"), view_signature(&redefined, "raw"), "a redefined view draws again");
    }

    #[test]
    fn views_switch_visibility_without_a_history_step() {
        let (mut s, raw, group, text) = session();
        let undo_before = s.active().map(|d| d.history.entries().len());
        s.execute("document.views.define", json!({"name": "raw", "visible": [raw], "hidden": ["Text"]})).expect("define");
        s.execute("document.views.define", json!({"name": "typeset", "visible": ["Text"], "hidden": ["raw"]})).expect("define");
        let r = s.execute("document.views.apply", json!({"name": "raw"})).expect("apply");
        assert_eq!(r["changed"], 1);
        assert!(visible(&s, raw) && !visible(&s, group));
        let cache = |s: &Session| match &s.active().and_then(|d| d.doc.layer(LayerId(text))).expect("the type").content {
            photocraft_doc::LayerContent::Text(t) => t.cache.as_ref().map(|c| c.tiles().map(|(_, t)| std::sync::Arc::as_ptr(t) as usize).collect::<Vec<_>>()),
            _ => None,
        };
        let raster = cache(&s);
        s.execute("document.views.apply", json!({"name": "typeset"})).expect("apply");
        assert!(!visible(&s, raw) && visible(&s, group));
        assert_eq!(cache(&s), raster, "the type isn't rasterized again");
        assert_eq!(s.active().map(|d| d.history.entries().len()), undo_before, "no history step");
        // Undo takes back the type layer, not the view.
        s.execute("edit.undo", json!({})).expect("undo");
        assert!(!visible(&s, raw), "undo keeps the view");
        let list = s.execute("document.views.list", json!({})).expect("list");
        assert_eq!(list["views"].as_array().map(Vec::len), Some(2), "{list}");
        s.execute("edit.redo", json!({})).expect("redo");
        assert_eq!(s.execute("document.views.list", json!({})).expect("list")["views"][1]["applied"], true);
    }

    #[test]
    fn a_view_apply_keeps_each_history_state_s_pixels() {
        let (mut s, raw, ..) = session();
        s.execute("layer.select", json!({"layer": raw})).expect("select");
        s.execute("edit.fill", json!({"contents": "black"})).expect("fill");
        s.execute("document.views.define", json!({"name": "clean", "hidden": [raw]})).expect("define");
        s.execute("document.views.apply", json!({"name": "clean"})).expect("apply");
        s.execute("edit.undo", json!({})).expect("undo the fill");
        let d = s.active().expect("a document");
        let l = d.doc.layer(LayerId(raw)).expect("the layer");
        assert!(!l.visible, "the view stays applied");
        assert!(l.surface().is_none_or(|sf| sf.content_bounds().is_empty()), "the fill is undone");
    }

    #[test]
    fn bad_view_params_are_errors() {
        let (mut s, raw, ..) = session();
        for p in [
            json!({}),
            json!({"name": ""}),
            json!({"name": "v"}),
            json!({"name": "v", "visible": "raw"}),
            json!({"name": "v", "visible": [9999]}),
            json!({"name": "v", "visible": [-1]}),
            json!({"name": "v", "visible": ["nope"]}),
            json!({"name": "v", "visible": [true]}),
            json!({"name": "v", "visible": [raw], "hidden": [raw]}),
        ] {
            assert!(s.execute("document.views.define", p.clone()).is_err(), "{p}");
        }
        assert!(s.execute("document.views.apply", json!({"name": "none"})).is_err());
        assert!(s.execute("document.views.delete", json!({"name": "none"})).is_err());
        s.execute("document.views.define", json!({"name": "v", "visible": [raw]})).expect("define");
        s.execute("document.views.delete", json!({"name": "v"})).expect("delete");
        assert!(s.execute("document.views.apply", json!({"name": "v"})).is_err());
    }
}
