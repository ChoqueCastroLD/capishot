//! Commands run for someone else (Visoria's live multiplayer): on an open document that isn't the
//! active one, without the user seeing a document switch (`document`), and outside the user's undo
//! history (`history: "none"`), so their Ctrl+Z never takes back a collaborator's change.
//!
//! `history: "none"` runs the command as usual, then puts the history back as it was and carries
//! the layers the command changed into every stored state ([`LayerPatch`]): undo and redo then
//! step through the user's own changes with the collaborator's change kept in each. The patch is
//! per layer: a layer the command changed is replaced (a group keeps each state's own children),
//! a layer it added is added, one it deleted is deleted and one it moved is moved, in every state
//! that has (or lacks) it; a layer whose visibility is all that changed only has its visibility
//! set (each state keeps its own pixels). Layer comps (and so `document.views`) are carried too.
//! Other document-wide changes (canvas size, mode, guides, selection, channels) are not carried:
//! they stay in the current document only.

use std::collections::HashMap;

use photocraft_doc::{DocId, Document, Layer, LayerContent, LayerId};

use super::{Arc, EngineError, Result, Session, Value};

/// How a command's change enters the document's undo history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryMode {
    /// As always: the command's own history step.
    #[default]
    Step,
    /// No step: the change is folded into every history state, so undo and redo keep it.
    None,
}

impl HistoryMode {
    /// `history`: `"step"` (default), `"none"`, or `"silent"` (the same as `"none"`).
    pub fn parse(v: Option<&Value>) -> std::result::Result<HistoryMode, String> {
        match v {
            None | Some(Value::Null) => Ok(HistoryMode::Step),
            Some(Value::String(s)) => match s.as_str() {
                "step" | "normal" => Ok(HistoryMode::Step),
                "none" | "silent" => Ok(HistoryMode::None),
                other => Err(format!("unknown history mode `{other}` (step, none, silent)")),
            },
            Some(_) => Err("`history` must be \"step\", \"none\" or \"silent\"".into()),
        }
    }
}

/// What [`Session::begin_scope`] changed, for [`Session::end_scope`] to put back.
#[derive(Debug)]
pub struct ExecScope {
    /// The document active before (`None`: there was none), made active again at the end.
    previous: Option<DocId>,
    /// The document the command runs on.
    target: DocId,
    fold: Option<Fold>,
}

#[derive(Debug)]
struct Fold {
    before: Arc<Document>,
    history: photocraft_ops::History,
    coalesce: Option<String>,
}

impl ExecScope {
    /// The document the scope runs on.
    pub fn target(&self) -> DocId {
        self.target
    }
}

impl Session {
    /// The index of the open document `id`.
    pub fn index_of_document(&self, id: DocId) -> Option<usize> {
        self.docs.iter().position(|st| st.doc.id == id)
    }

    /// Make document `target` (`None`: the active one) the one commands act on, quietly: no active
    /// document change is announced (the active generation stays), and [`Self::end_scope`] makes
    /// the previous one active again. With [`HistoryMode::None`] the history is kept to be put back.
    pub fn begin_scope(&mut self, target: Option<DocId>, history: HistoryMode) -> Result<ExecScope> {
        let previous = self.active().map(|st| st.doc.id);
        let index = match target {
            Some(id) => self.index_of_document(id).ok_or_else(|| EngineError::Other(format!("no open document {}", id.0)))?,
            None => self.active.ok_or(EngineError::NoDocument)?,
        };
        let st = self.docs.get(index).ok_or(EngineError::NoDocument)?;
        let target = st.doc.id;
        let fold = (history == HistoryMode::None).then(|| Fold { before: st.doc.clone(), history: st.history.clone(), coalesce: st.coalesce.clone() });
        // Quiet: `note_active_change` isn't called, so nothing sees a switch.
        self.active = Some(index);
        Ok(ExecScope { previous, target, fold })
    }

    /// End a [`Self::begin_scope`]: fold the change into the history when asked, and make the
    /// previously active document active again. Returns how many history states couldn't be
    /// rewritten (archived states that failed to load; they keep the document as it was).
    pub fn end_scope(&mut self, scope: ExecScope) -> usize {
        let mut unreadable = 0;
        if let (Some(fold), Some(i)) = (scope.fold, self.index_of_document(scope.target))
            && let Some(st) = self.docs.get_mut(i)
        {
            let patch = LayerPatch::between(&fold.before, &st.doc);
            let mut history = fold.history;
            if !patch.is_empty() {
                unreadable = history.map_states(|d| patch.apply(d));
            }
            history.clear_current_archive();
            st.history = history;
            st.coalesce = fold.coalesce;
            st.trim_history();
        }
        let back = scope.previous.and_then(|id| self.index_of_document(id));
        match back {
            Some(i) => self.active = Some(i),
            // No document was active, or the previous one closed: whatever is active now is
            // announced as usual.
            None => {
                // Never a hidden document (the scope's target may be one).
                let shown = self.visible_count();
                if self.active.is_some_and(|a| a >= shown) {
                    self.active = shown.checked_sub(1);
                }
                self.note_active_change();
            }
        }
        unreadable
    }

    /// [`Self::execute`] on document `target` (`None`: the active one) with `history`, the
    /// active document staying as it is ([`Self::begin_scope`]).
    pub fn execute_scoped(&mut self, id: &str, params: Value, target: Option<DocId>, history: HistoryMode) -> Result<Value> {
        if target.is_none() && history == HistoryMode::Step {
            return self.execute(id, params);
        }
        let scope = self.begin_scope(target, history)?;
        let r = self.execute(id, params);
        self.end_scope(scope);
        r
    }
}

/// Where a layer sits: its parent group (`None`: the top level) and the sibling right below it
/// (`None`: it is the bottom one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Place {
    parent: Option<LayerId>,
    below: Option<LayerId>,
}

fn places(doc: &Document) -> HashMap<LayerId, Place> {
    fn rec(layers: &[Layer], parent: Option<LayerId>, out: &mut HashMap<LayerId, Place>) {
        let mut below = None;
        for l in layers {
            out.insert(l.id, Place { parent, below });
            if let Some(ch) = l.children() {
                rec(ch, Some(l.id), out);
            }
            below = Some(l.id);
        }
    }
    let mut out = HashMap::new();
    rec(&doc.layers, None, &mut out);
    out
}

/// A group without its children (they are compared and carried one by one).
fn shallow(l: &Layer) -> Layer {
    let mut c = l.clone();
    if let LayerContent::Group(g) = &mut c.content {
        g.children.clear();
    }
    c
}

fn same_shallow(a: &Layer, b: &Layer) -> bool {
    match (&a.content, &b.content) {
        (LayerContent::Group(_), LayerContent::Group(_)) => shallow(a) == shallow(b),
        _ => a == b,
    }
}

/// The per-layer difference between two versions of a document ([module docs](self)).
#[derive(Debug, Default)]
pub(crate) struct LayerPatch {
    removed: Vec<LayerId>,
    /// Changed layers (groups without children), by id.
    changed: Vec<Layer>,
    /// Layers whose visibility is all that changed: shown (`true`) or hidden.
    shown: Vec<(LayerId, bool)>,
    /// The layer comps, the last applied one and the Last Document State, when they changed.
    comps: Option<(Vec<photocraft_doc::LayerComp>, Option<u32>, Option<photocraft_doc::LayerComp>)>,
    /// Added (with their children) or moved layers, bottom to top, outer before inner.
    placed: Vec<(LayerId, Place, Option<Layer>)>,
}

impl LayerPatch {
    pub(crate) fn between(before: &Document, after: &Document) -> LayerPatch {
        let (pb, pa) = (places(before), places(after));
        let old: HashMap<LayerId, &Layer> = before.walk().into_iter().map(|(_, _, l)| (l.id, l)).collect();
        let mut patch = LayerPatch { removed: old.keys().filter(|id| !pa.contains_key(id)).copied().collect(), ..Default::default() };
        if before.layer_comps != after.layer_comps
            || before.last_applied_comp != after.last_applied_comp
            || before.last_document_state != after.last_document_state
        {
            patch.comps = Some((after.layer_comps.clone(), after.last_applied_comp, after.last_document_state.clone()));
        }
        for (_, _, l) in after.walk() {
            let Some(place) = pa.get(&l.id).copied() else { continue };
            match old.get(&l.id) {
                None => {
                    // Children of an added group come with it.
                    if place.parent.is_none_or(|p| pb.contains_key(&p)) {
                        patch.placed.push((l.id, place, Some(l.clone())));
                    }
                }
                Some(o) => {
                    if o.visible != l.visible && same_shallow(&Layer { visible: l.visible, ..shallow(o) }, &shallow(l)) {
                        patch.shown.push((l.id, l.visible));
                    } else if !same_shallow(o, l) {
                        patch.changed.push(shallow(l));
                    }
                    if pb.get(&l.id) != Some(&place) {
                        patch.placed.push((l.id, place, None));
                    }
                }
            }
        }
        patch
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.changed.is_empty() && self.shown.is_empty() && self.placed.is_empty() && self.comps.is_none()
    }

    /// `doc` with the patch applied (`None`: nothing to change in it).
    pub(crate) fn apply(&self, doc: &Document) -> Option<Document> {
        let mut d = doc.clone();
        let mut touched = false;
        for id in &self.removed {
            touched |= d.remove(*id).is_some();
        }
        for new in &self.changed {
            let Some(slot) = d.layer_mut(new.id) else { continue };
            let mut l = new.clone();
            if let (Some(kids), Some(own)) = (l.children_mut(), slot.children_mut()) {
                *kids = std::mem::take(own);
            }
            *slot = l;
            touched = true;
        }
        for (id, visible) in &self.shown {
            if let Some(l) = d.layer_mut(*id) {
                touched |= l.visible != *visible;
                l.visible = *visible;
            }
        }
        if let Some((comps, last, state)) = &self.comps {
            d.layer_comps = comps.clone();
            d.last_applied_comp = *last;
            d.last_document_state = state.clone();
            touched = true;
        }
        for (id, place, layer) in &self.placed {
            let layer = match layer {
                // Added: only where it isn't yet.
                Some(l) if d.layer(*id).is_none() => l.clone(),
                Some(_) => continue,
                // Moved: take it out and put it back where it is now.
                None => match d.remove(*id) {
                    Some(l) => l,
                    None => continue,
                },
            };
            place_layer(&mut d, *place, layer);
            touched = true;
        }
        touched.then_some(d)
    }
}

/// Put `layer` at `place`: above its lower sibling when the document has it, else at the bottom
/// of its parent group, else (the group is gone in this state) at the top of the document.
fn place_layer(d: &mut Document, place: Place, layer: Layer) {
    if let Some(below) = place.below.filter(|b| d.layer(*b).is_some()) {
        d.insert_above(Some(below), layer);
        return;
    }
    match place.parent {
        None => {
            d.insert_below(d.layers.first().map(|l| l.id), layer);
        }
        Some(p) => match d.layer_mut(p).and_then(Layer::children_mut) {
            Some(kids) => kids.insert(0, layer),
            None => d.layers.push(layer),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn new_layer(s: &mut Session) -> u64 {
        s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap()
    }

    fn name(s: &Session, id: u64) -> Option<String> {
        s.active().and_then(|st| st.doc.layer(LayerId(id))).map(|l| l.name.clone())
    }

    #[test]
    fn history_modes_parse() {
        assert_eq!(HistoryMode::parse(None), Ok(HistoryMode::Step));
        assert_eq!(HistoryMode::parse(Some(&json!("none"))), Ok(HistoryMode::None));
        assert_eq!(HistoryMode::parse(Some(&json!("silent"))), Ok(HistoryMode::None));
        assert!(HistoryMode::parse(Some(&json!("later"))).is_err());
        assert!(HistoryMode::parse(Some(&json!(1))).is_err());
    }

    #[test]
    fn a_command_runs_on_another_document_without_switching() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        let a = s.active().unwrap().doc.id;
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        let b = s.active().unwrap().doc.id;
        let generation = s.active_generation();
        let layers = |s: &Session, id: DocId| s.documents().iter().find(|st| st.doc.id == id).unwrap().doc.layer_count();
        let before = (layers(&s, a), layers(&s, b));
        s.execute_scoped("layer.new.layer", json!({}), Some(a), HistoryMode::Step).unwrap();
        assert_eq!((layers(&s, a), layers(&s, b)), (before.0 + 1, before.1));
        assert_eq!(s.active().unwrap().doc.id, b, "the active document stays");
        assert_eq!(s.active_generation(), generation, "no switch is announced");
        // An error on the other document leaves things as they were too.
        assert!(s.execute_scoped("layer.delete", json!({"layer": 999_999}), Some(a), HistoryMode::Step).is_err());
        assert_eq!(s.active().unwrap().doc.id, b);
        assert!(s.execute_scoped("layer.new.layer", json!({}), Some(DocId(u64::MAX)), HistoryMode::Step).is_err(), "no such document");
        assert_eq!(s.active().unwrap().doc.id, b);
    }

    #[test]
    fn hidden_documents_are_never_shown_or_made_active() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        let shown = s.active().unwrap().doc.id;
        let generation = s.active_generation();
        let r = s.execute("file.new", json!({"width": 4, "height": 4, "hidden": true})).unwrap();
        let hidden = DocId(r["id"].as_u64().unwrap());
        assert!(s.execute("file.new", json!({"hidden": 1})).is_err());
        assert_eq!((s.active().unwrap().doc.id, s.active_generation()), (shown, generation));
        assert_eq!((s.visible_count(), s.documents().len()), (1, 2));
        // A shown document opens before the hidden ones, and is made active as always.
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        let second = s.active().unwrap().doc.id;
        assert_eq!(s.visible_count(), 2);
        assert_eq!(s.documents().last().unwrap().doc.id, hidden, "hidden documents stay last");
        assert!(!s.set_active(2), "a hidden document isn't activated");
        assert_eq!(s.move_document(2, 0), None, "nor moved among the tabs");
        assert_eq!(s.move_document(0, 5), Some(1), "a tab stays before the hidden ones");
        assert_eq!(s.documents().last().unwrap().doc.id, hidden);
        // Copying layers between documents by id activates nothing.
        let layer = s.execute_scoped("layer.new.layer", json!({}), Some(hidden), HistoryMode::None).unwrap()["layer"].as_u64().unwrap();
        let generation = s.active_generation();
        let r = s.execute("layer.copyToDocument", json!({"fromDocument": hidden.0, "toDocument": shown.0, "layers": [layer]})).unwrap();
        assert_eq!(r["id"], json!(shown.0));
        assert_eq!((s.active().unwrap().doc.id, s.active_generation()), (second, generation));
        assert!(s.execute("layer.copyToDocument", json!({"fromDocument": 123_456_789, "toDocument": shown.0})).is_err());
        assert_eq!(s.active().unwrap().doc.id, second);
        // Closing the shown documents never activates a hidden one; Close All leaves it open.
        s.execute("file.closeAll", json!({})).unwrap();
        assert_eq!((s.active_index(), s.documents().len()), (None, 1));
        // Commands still reach it, and leave no document active.
        s.execute_scoped("layer.new.layer", json!({}), Some(hidden), HistoryMode::None).unwrap();
        assert_eq!(s.active_index(), None);
        assert!(s.close(0).is_some());
    }

    #[test]
    fn showing_a_hidden_document_swaps_it_with_the_active_one() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        let a = s.active().unwrap().doc.id;
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        let b = s.active().unwrap().doc.id;
        let h = DocId(s.execute("file.new", json!({"width": 4, "height": 4, "hidden": true})).unwrap()["id"].as_u64().unwrap());
        let generation = s.active_generation();
        let ids = |s: &Session| s.documents().iter().map(|st| (st.doc.id, st.hidden)).collect::<Vec<_>>();
        // The helper is shown and active; the page shown before becomes a helper, kept open.
        assert_eq!(s.show_document(h, true), Some(1));
        assert_eq!(ids(&s), [(a, false), (h, false), (b, true)]);
        assert_eq!(s.active().unwrap().doc.id, h);
        assert_ne!(s.active_generation(), generation);
        // Back: the same swap the other way; without hiding, both stay shown.
        assert_eq!(s.show_document(b, true), Some(1));
        assert_eq!(ids(&s), [(a, false), (b, false), (h, true)]);
        assert_eq!(s.show_document(a, false), Some(0));
        assert_eq!(ids(&s), [(a, false), (b, false), (h, true)]);
        assert_eq!(s.show_document(a, true), Some(0), "showing the active document hides nothing");
        assert_eq!(s.show_document(DocId(u64::MAX), true), None);
        assert_eq!(s.visible_count(), 2);
    }

    #[test]
    fn a_change_without_a_step_survives_the_users_undo_and_redo() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        let mine = new_layer(&mut s);
        let theirs = new_layer(&mut s);
        s.execute("layer.setProps", json!({"layer": mine, "name": "mine 1"})).unwrap();
        let steps = s.active().unwrap().history.entries().len();
        // A collaborator renames their layer, adds one and deletes nothing of mine.
        s.execute_scoped("layer.setProps", json!({"layer": theirs, "name": "theirs!"}), None, HistoryMode::None).unwrap();
        let added = s.execute_scoped("layer.new.layer", json!({"name": "remote"}), None, HistoryMode::None).unwrap()["layer"].as_u64().unwrap();
        assert_eq!(s.active().unwrap().history.entries().len(), steps, "no new steps");
        assert_eq!(name(&s, theirs).as_deref(), Some("theirs!"));
        // My undo takes back my rename only.
        assert!(s.undo());
        assert_eq!(name(&s, mine).as_deref(), Some("Layer 1"));
        assert_eq!(name(&s, theirs).as_deref(), Some("theirs!"), "their rename is kept");
        assert!(name(&s, added).is_some(), "their layer is kept");
        // Back to before both of my layers existed: theirs stays (it was folded in), mine goes.
        assert!(s.undo() && s.undo());
        assert!(name(&s, mine).is_none());
        assert_eq!(name(&s, added).as_deref(), Some("remote"));
        while s.redo() {}
        assert_eq!(name(&s, mine).as_deref(), Some("mine 1"));
        assert_eq!(name(&s, theirs).as_deref(), Some("theirs!"));
        // A collaborator's delete stays deleted through undo.
        s.execute_scoped("layer.delete", json!({"layer": added}), None, HistoryMode::None).unwrap();
        assert!(s.undo());
        assert!(name(&s, added).is_none());
    }

    #[test]
    fn a_move_and_a_group_change_are_carried_per_layer() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        let a = new_layer(&mut s);
        let b = new_layer(&mut s);
        let before = s.active().unwrap().doc.clone();
        let mut after = (*before).clone();
        after.shift(LayerId(b), -1);
        if let Some(l) = after.layer_mut(LayerId(a)) {
            l.opacity = 0.5;
        }
        let patch = LayerPatch::between(&before, &after);
        let mut old = (*before).clone();
        old.remove(LayerId(b));
        // A state without `b` gets `a`'s change; `b`'s move needs `b`.
        let patched = patch.apply(&old).unwrap();
        assert_eq!(patched.layer(LayerId(a)).unwrap().opacity, 0.5);
        assert!(patched.layer(LayerId(b)).is_none());
        let patched = patch.apply(&before).unwrap();
        let order: Vec<u64> = patched.layers.iter().map(|l| l.id.0).collect();
        let want: Vec<u64> = after.layers.iter().map(|l| l.id.0).collect();
        assert_eq!(order, want);
        assert!(LayerPatch::between(&before, &before).is_empty());
    }
}
