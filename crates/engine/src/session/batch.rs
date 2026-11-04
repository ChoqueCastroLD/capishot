//! Several commands as one transaction (`document.batch`, the embed API's `batch`): run in order
//! on the active document (`engine.execute {document}` scopes it to another one), all or nothing,
//! in one history step (`history: "one"`) or a step each (`"step"`). The host sees one revision
//! change at the end of the frame, so the canvas is drawn once.
//!
//! A failing step puts the document, its history and its layer selection back as they were
//! before the batch ([`Batch::rollback`]) and fails the batch with that step's error.

use serde_json::{Value, json};

use super::{DocState, EngineError, Result, Session, type_cmds, visibility_changes};
use crate::commands::CommandSpec;

const ID: &str = "document.batch";

/// At most this many steps in one batch.
pub const MAX_STEPS: usize = 10_000;

/// How a batch enters the undo history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BatchHistory {
    /// One step for the whole batch, named after its first step.
    #[default]
    One,
    /// Each command its own step, as if run one by one.
    Step,
}

impl BatchHistory {
    /// `"one"` (default) or `"step"`.
    pub fn parse(v: Option<&Value>) -> std::result::Result<BatchHistory, String> {
        match v {
            None | Some(Value::Null) => Ok(BatchHistory::One),
            Some(Value::String(s)) if s == "one" => Ok(BatchHistory::One),
            Some(Value::String(s)) if s == "step" => Ok(BatchHistory::Step),
            Some(v) => Err(format!("`history` must be \"one\" or \"step\", got {v}")),
        }
    }
}

/// A batch in progress on the active document: what [`Session::end_batch`] folds or puts back.
#[derive(Debug)]
pub struct Batch {
    saved: DocState,
    history: BatchHistory,
}

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: ID.into(), msg: msg.into() }
}

/// Commands a batch refuses: they open, close or switch documents (the batch is on one), undo
/// or redo (the batch is one step), or nest batches.
fn refused(id: &str) -> bool {
    id.starts_with("file.")
        || id.starts_with("window.")
        || matches!(id, ID | "edit.undo" | "edit.redo" | "edit.stepForward" | "edit.stepBackward" | "document.activate")
}

/// `[{command, params?}, …]` → the steps, checked before any runs.
pub fn parse_steps(v: Option<&Value>) -> std::result::Result<Vec<(String, Value)>, String> {
    let list = v.and_then(Value::as_array).ok_or("`commands` must be a list of {command, params}")?;
    if list.len() > MAX_STEPS {
        return Err(format!("at most {MAX_STEPS} commands in one batch"));
    }
    list.iter()
        .enumerate()
        .map(|(i, step)| {
            let id = step.get("command").or_else(|| step.get("id")).and_then(Value::as_str).ok_or_else(|| format!("command {i}: missing `command`"))?;
            if crate::commands::find(id).is_none() {
                return Err(format!("command {i}: unknown command `{id}`"));
            }
            if refused(id) {
                return Err(format!("command {i}: `{id}` can't run in a batch (it opens, closes or switches documents, undoes or nests a batch)"));
            }
            let params = match step.get("params") {
                None | Some(Value::Null) => json!({}),
                Some(p @ Value::Object(_)) => p.clone(),
                Some(_) => return Err(format!("command {i}: `params` must be an object")),
            };
            Ok((id.to_string(), params))
        })
        .collect()
}

impl Session {
    /// Start a batch on the active document.
    pub fn begin_batch(&self, history: BatchHistory) -> Result<Batch> {
        let saved = self.active().ok_or(EngineError::NoDocument)?.clone();
        Ok(Batch { saved, history })
    }

    /// Whether step `i` of `b` left things as a batch needs them: the same active document, no
    /// document opened or closed. `Err` names the step.
    pub fn batch_step_check(&self, b: &Batch, i: usize, id: &str) -> Result<()> {
        if self.active().map(|st| st.doc.id) != Some(b.saved.doc.id) {
            return Err(EngineError::Other(format!("command {i} (`{id}`) switched documents; the batch was undone")));
        }
        Ok(())
    }

    /// End a batch: with `ok`, fold its steps into one history step (`history: "one"`); without,
    /// put the document back as it was before the batch.
    pub fn end_batch(&mut self, b: Batch, ok: bool) {
        let Some(i) = self.index_of_document(b.saved.doc.id) else { return };
        let poll = {
            let Some(st) = self.docs.get_mut(i) else { return };
            if !ok {
                b.rollback(st);
                true
            } else if b.history == BatchHistory::One && !std::sync::Arc::ptr_eq(&b.saved.doc, &st.doc) {
                b.fold(st);
                true
            } else {
                false
            }
        };
        if poll {
            self.history_cache.dirty = true;
            self.poll_history_cache();
        }
    }

    /// Run `steps` as one batch on the active document ([module docs](self)): each step's result,
    /// or the first failure (nothing applied then).
    pub fn execute_batch(&mut self, steps: Vec<(String, Value)>, history: BatchHistory) -> Result<Vec<Value>> {
        let batch = self.begin_batch(history)?;
        let mut results = Vec::with_capacity(steps.len());
        for (i, (id, params)) in steps.into_iter().enumerate() {
            let r = self
                .execute(&id, params)
                .map_err(|e| EngineError::Other(format!("command {i} (`{id}`): {e}; the batch was undone")))
                .and_then(|v| self.batch_step_check(&batch, i, &id).map(|()| v));
            match r {
                Ok(v) => results.push(v),
                Err(e) => {
                    self.end_batch(batch, false);
                    return Err(e);
                }
            }
        }
        self.end_batch(batch, true);
        Ok(results)
    }
}

impl Batch {
    /// The document, history and layer selection before the batch, as a new revision (the
    /// canvas redraws), clean if it was clean.
    fn rollback(self, st: &mut DocState) {
        let revision = st.revision;
        let pixel_revs = st.pixel_revs.clone();
        let was_clean = self.saved.revision == self.saved.saved_revision;
        *st = self.saved;
        st.pixel_revs = pixel_revs;
        st.revision = revision;
        st.last_damage = None;
        st.bump_revision(was_clean);
    }

    /// The batch's steps as one history step: the history from before the batch, then one
    /// state, named after the batch's first step, from the document before it to the one now.
    fn fold(self, st: &mut DocState) {
        let before = self.saved.doc.clone();
        let past = self.saved.history.past_len();
        let label = st.history.entries().get(past + 1).cloned().unwrap_or_else(|| "Batch".into());
        let prior = self.saved.layer_target();
        let mut history = self.saved.history;
        history.clear_current_archive();
        history.set_current_layers(prior);
        let recorded = type_cmds::strip_rebuildable(&before, &st.doc);
        history.record(label, recorded.clone(), st.layer_target());
        history.add_current_visibility(&visibility_changes(&recorded, &st.doc));
        st.history = history;
        st.coalesce = None;
        st.trim_history();
    }
}

/// `document.batch {commands: [{command, params}], history?, inspect?}`.
fn batch(s: &mut Session, p: &Value) -> Result<Value> {
    let steps = parse_steps(p.get("commands")).map_err(bad)?;
    let history = BatchHistory::parse(p.get("history")).map_err(bad)?;
    let inspect = match p.get("inspect") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => None,
        Some(Value::Bool(true)) => Some(json!({})),
        Some(o @ Value::Object(_)) => Some(o.clone()),
        Some(v) => return Err(bad(format!("`inspect` must be true, false or document.inspect's params, got {v}"))),
    };
    let results = s.execute_batch(steps, history)?;
    let mut out = json!({ "results": results });
    if let Some(params) = inspect {
        out["inspect"] = s.execute("document.inspect", params)?;
    }
    Ok(out)
}

/// `layer.setPropsMany {layers: [{layer, visible?, opacity?, …}]}`: `layer.setProps` for each,
/// as one batch (one step, all or nothing).
fn set_props_many(s: &mut Session, p: &Value) -> Result<Value> {
    const MANY: &str = "layer.setPropsMany";
    let bad = |msg: String| EngineError::BadParams { cmd: MANY.into(), msg };
    let list = p.get("layers").and_then(Value::as_array).ok_or_else(|| bad("`layers` must be a list of {layer, …layer.setProps fields}".into()))?;
    if list.len() > MAX_STEPS {
        return Err(bad(format!("at most {MAX_STEPS} layers")));
    }
    let steps = list
        .iter()
        .enumerate()
        .map(|(i, item)| match item {
            Value::Object(o) if o.get("layer").is_some_and(Value::is_u64) => Ok(("layer.setProps".to_string(), item.clone())),
            _ => Err(bad(format!("layers[{i}] must be {{layer: id, …}}"))),
        })
        .collect::<Result<Vec<_>>>()?;
    let history = BatchHistory::parse(p.get("history")).map_err(bad)?;
    let results = s.execute_batch(steps, history)?;
    Ok(json!({ "layers": results.len() }))
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: ID,
            label: "Batch",
            menu: &[],
            shortcut: None,
            params: r##"{"commands":[{"command":id,"params":{}}],"history":"one|step"="one","inspect":bool|{document.inspect params}=false} → {results:[…], inspect?}: the commands in order on the document as one transaction: one history step ("one") or one each ("step"); a failing command undoes the whole batch and fails it. File, window, undo/redo and nested batches are refused. With engine.execute {document, history:"none"} it runs on another open document with no undo step"##,
            enabled: has_doc,
            journal: false,
            run: batch,
        },
        CommandSpec {
            id: "layer.setPropsMany",
            label: "Layer Properties",
            menu: &[],
            shortcut: None,
            params: r##"{"layers":[{"layer":id,…layer.setProps fields (visible, opacity, fill, blend, name, locks…)}],"history":"one|step"="one"} → {layers}: layer.setProps for each as one document.batch (one history step, all or nothing)"##,
            enabled: has_doc,
            journal: false,
            run: set_props_many,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> (Session, u64, u64) {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        let a = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap();
        let b = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap();
        (s, a, b)
    }

    fn opacity(s: &Session, id: u64) -> f32 {
        s.active().unwrap().doc.layer(photocraft_doc::LayerId(id)).unwrap().opacity
    }

    #[test]
    fn a_batch_is_one_undo_step() {
        let (mut s, a, b) = session();
        let steps = s.active().unwrap().history.entries().len();
        let r = s
            .execute(
                ID,
                json!({"commands": [
                    {"command": "layer.setProps", "params": {"layer": a, "opacity": 0.5}},
                    {"command": "layer.setProps", "params": {"layer": b, "visible": false}},
                    {"command": "layer.new.layer", "params": {"name": "added"}},
                ], "inspect": true}),
            )
            .unwrap();
        assert_eq!(r["results"].as_array().unwrap().len(), 3);
        assert!(r["inspect"]["layers"].is_array(), "{r}");
        assert_eq!(s.active().unwrap().history.entries().len(), steps + 1, "one step");
        assert_ne!(s.active().unwrap().history.undo_label(), Some("Batch"), "named after its first step");
        assert!(s.undo());
        assert_eq!(opacity(&s, a), 1.0);
        assert!(s.active().unwrap().doc.layer(photocraft_doc::LayerId(b)).unwrap().visible);
        assert_eq!(s.active().unwrap().doc.layer_count(), 3, "the added layer goes with the same undo");
        assert!(s.redo());
        assert_eq!(opacity(&s, a), 0.5);
    }

    #[test]
    fn a_failing_step_undoes_the_batch() {
        let (mut s, a, _) = session();
        let st = s.active().unwrap();
        let (steps, revision, doc) = (st.history.entries().len(), st.revision, st.doc.clone());
        let e = s.execute(
            ID,
            json!({"commands": [
                {"command": "layer.setProps", "params": {"layer": a, "opacity": 0.25}},
                {"command": "layer.setProps", "params": {"layer": 999_999, "opacity": 0.5}},
            ]}),
        );
        assert!(e.is_err());
        let st = s.active().unwrap();
        assert_eq!(opacity(&s, a), 1.0, "the first step was undone");
        assert_eq!(st.history.entries().len(), steps);
        assert!(st.revision > revision, "a new revision: the canvas redraws");
        assert_eq!(*st.doc, *doc);
    }

    #[test]
    fn step_history_keeps_a_step_each_and_set_props_many_wraps_a_batch() {
        let (mut s, a, b) = session();
        let steps = s.active().unwrap().history.entries().len();
        s.execute(ID, json!({"history": "step", "commands": [{"command": "layer.setProps", "params": {"layer": a, "opacity": 0.5}}, {"command": "layer.setProps", "params": {"layer": b, "opacity": 0.5}}]})).unwrap();
        assert_eq!(s.active().unwrap().history.entries().len(), steps + 2);
        let r = s.execute("layer.setPropsMany", json!({"layers": [{"layer": a, "opacity": 0.1}, {"layer": b, "opacity": 0.2, "visible": false}]})).unwrap();
        assert_eq!(r["layers"], 2);
        assert_eq!(s.active().unwrap().history.entries().len(), steps + 3);
        assert!((opacity(&s, a) - 0.1).abs() < 1e-6 && (opacity(&s, b) - 0.2).abs() < 1e-6);
    }

    #[test]
    fn bad_batches_are_refused_before_anything_runs() {
        let (mut s, a, _) = session();
        let rev = s.active().unwrap().revision;
        for bad in [
            json!({}),
            json!({"commands": 3}),
            json!({"commands": [{"params": {}}]}),
            json!({"commands": [{"command": "no.such"}]}),
            json!({"commands": [{"command": "file.new"}]}),
            json!({"commands": [{"command": ID, "params": {"commands": []}}]}),
            json!({"commands": [{"command": "edit.undo"}]}),
            json!({"commands": [{"command": "layer.setProps", "params": [1]}]}),
            json!({"commands": [], "history": "later"}),
            json!({"commands": [], "inspect": 3}),
        ] {
            assert!(s.execute(ID, bad.clone()).is_err(), "{bad}");
        }
        for bad in [json!({}), json!({"layers": [1]}), json!({"layers": [{"opacity": 1}]}), json!({"layers": [{"layer": a}], "history": 1})] {
            assert!(s.execute("layer.setPropsMany", bad.clone()).is_err(), "{bad}");
        }
        assert_eq!(s.active().unwrap().revision, rev);
        assert_eq!(s.execute(ID, json!({"commands": []})).unwrap()["results"], json!([]));
    }

    #[test]
    fn a_batch_on_a_hidden_document_leaves_the_active_one_alone() {
        let (mut s, _, _) = session();
        let shown = s.active().unwrap().doc.clone();
        let generation = s.active_generation();
        let hidden = s.execute("file.new", json!({"width": 4, "height": 4, "hidden": true})).unwrap()["id"].as_u64().unwrap();
        assert_eq!(s.active().unwrap().doc.id, shown.id, "a hidden document isn't made active");
        let r = s.execute_scoped(
            ID,
            json!({"commands": [{"command": "layer.new.layer", "params": {}}]}),
            Some(photocraft_doc::DocId(hidden)),
            crate::HistoryMode::None,
        );
        assert!(r.is_ok(), "{r:?}");
        assert!(std::sync::Arc::ptr_eq(&s.active().unwrap().doc, &shown));
        assert_eq!(s.active_generation(), generation);
        let h = s.documents().iter().find(|st| st.doc.id.0 == hidden).unwrap();
        assert_eq!(h.doc.layer_count(), 2);
        assert_eq!(h.history.entries().len(), 1, "no undo step");
    }
}
