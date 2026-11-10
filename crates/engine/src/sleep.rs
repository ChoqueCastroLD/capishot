//! Pages put to sleep: a document no one is looking at gives back its memory. Its pixels and
//! undo history go into the compressed form crash recovery writes ([`ObjectAutosaver`]), held in
//! memory, and the document left in the session is a stub of the same id, name, size and mode
//! with no layers, so the tab bar, the session's lists and its saved state stay as they were.
//! Waking it decodes that form back ([`recover_object_checkpoint`]): the same document, layers,
//! selection and undo steps. Both run on a worker ([`encode`], [`decode`]); the session only
//! swaps the result in ([`Session::put_to_sleep`], [`Session::wake`]), and only if the document
//! didn't change meanwhile.
//!
//! The active document is never asleep for long: activating a sleeping one wakes it, and until
//! then commands that need it fail ([`Session::refuse_asleep`]) instead of reading the stub.

use std::collections::HashMap;
use std::sync::Arc;

use photocraft_doc::{DocId, Document};
use photocraft_format::{HistoryLimits, ObjectAutosaver, RecoveryInfo};
use photocraft_ops::{History, HistoryCheckpoint};

use crate::{EngineError, Result, Session};

/// A document's sleeping form.
#[derive(Debug)]
pub struct Sleeping {
    descriptor: Vec<u8>,
    objects: Arc<HashMap<String, Vec<u8>>>,
    /// What it holds, compressed.
    pub bytes: usize,
}

/// Commands that run while the active document sleeps: they don't read it.
const AWAKE_NOT_NEEDED: &[&str] = &[
    "prefs.", "tool.", "tools.", "document.activate", "document.move", "document.list", "document.recent", "file.new", "file.open", "file.close", "file.recover",
    "window.", "help.", "engine.", "jobs.", "fonts.", "ai.models", "edit.preferences", "edit.colorSettings", "workspace.",
];

/// The sleeping form of `doc` and its undo history (on a worker: seconds for a big page on a phone).
pub fn encode(doc: &Document, history: &HistoryCheckpoint) -> Result<Sleeping> {
    let info = RecoveryInfo { key: format!("sleep-{}", doc.id.0), document_name: doc.name.clone(), original_path: None, saved_at: 0, revision: 0 };
    let limits = HistoryLimits { max_states: usize::MAX, max_bytes: u64::MAX };
    let checkpoint = ObjectAutosaver::default().checkpoint_with_history(doc, Some(history), limits, info, serde_json::Value::Null).map_err(|e| EngineError::Other(e.to_string()))?;
    let bytes = checkpoint.descriptor.len() + checkpoint.objects.iter().map(|(_, b)| b.len()).sum::<usize>();
    Ok(Sleeping { descriptor: checkpoint.descriptor, objects: Arc::new(checkpoint.objects.into_iter().collect()), bytes })
}

/// The document and undo history `s` holds (on a worker).
pub fn decode(s: &Sleeping) -> Result<(Document, HistoryCheckpoint)> {
    let (doc, history, _) = photocraft_format::recover_object_checkpoint(&s.descriptor, Arc::clone(&s.objects)).map_err(|e| EngineError::Other(e.to_string()))?;
    Ok((doc, history))
}

/// What stays of a sleeping document: its id, name, size, mode, depth and resolution, no layers.
fn stub(doc: &Document) -> Document {
    let mut s = Document::new(doc.name.clone(), doc.size, doc.mode, doc.depth);
    s.id = doc.id;
    s.resolution_dpi = doc.resolution_dpi;
    s.icc_profile = doc.icc_profile.clone();
    s
}

impl Session {
    /// Whether document `index` may sleep: not active, not a host's hidden helper, not mid-gesture
    /// (a floating selection), not linked to a smart object being edited, not already asleep.
    pub fn can_sleep(&self, index: usize) -> bool {
        let Some(st) = self.documents().get(index) else { return false };
        let id = st.doc.id;
        Some(index) != self.active_index()
            && !st.hidden
            && st.sleeping.is_none()
            && st.floating.is_none()
            && !self.smart_links.iter().any(|l| l.child == id || l.parent == id)
    }

    /// Swaps in the sleeping form encoded at `revision`; false (nothing changes) when the document
    /// changed, closed, became active or can't sleep any more.
    pub fn put_to_sleep(&mut self, id: DocId, revision: u64, sleeping: Sleeping) -> bool {
        let Some(index) = self.documents().iter().position(|st| st.doc.id == id) else { return false };
        if !self.can_sleep(index) || self.documents()[index].revision != revision {
            return false;
        }
        let st = &mut self.docs[index];
        let max_states = st.history.max_states;
        st.doc = Arc::new(stub(&st.doc));
        let mut history = History::new(max_states);
        history.max_bytes = 0;
        st.history = history;
        st.snapshots.clear();
        st.sleeping = Some(Arc::new(sleeping));
        // What Text Selection found on it: a label per pixel.
        self.text_detections.retain(|(d, _)| *d != id);
        true
    }

    /// Wakes document `id` with what [`decode`] gave back.
    pub fn wake(&mut self, id: DocId, mut doc: Document, history: HistoryCheckpoint) -> Result<()> {
        let st = self.docs.iter_mut().find(|st| st.doc.id == id).ok_or(EngineError::NoDocument)?;
        if st.sleeping.is_none() {
            return Ok(());
        }
        doc.id = id;
        let (max_states, non_linear) = (st.history.max_states, st.history.non_linear);
        let mut history = History::from_checkpoint(history).map_err(EngineError::Other)?;
        history.remap_document_id(id);
        history.max_states = max_states;
        history.max_bytes = 0;
        history.non_linear = non_linear;
        st.doc = Arc::new(doc);
        st.history = history;
        st.sleeping = None;
        st.view_revision = st.view_revision.wrapping_add(1);
        Ok(())
    }

    /// Whether document `index` is asleep.
    pub fn is_asleep(&self, index: usize) -> bool {
        self.documents().get(index).is_some_and(|st| st.sleeping.is_some())
    }

    /// The error a command `id` that needs the active document gets while it sleeps.
    pub(crate) fn refuse_asleep(&self, id: &str) -> Result<()> {
        if self.active().is_some_and(|st| st.sleeping.is_some()) && !AWAKE_NOT_NEEDED.iter().any(|p| id.starts_with(p)) {
            return Err(EngineError::Other("the page is still loading".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session_with_two() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 64, "height": 48, "name": "a"})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.execute("select.rect", json!({"x": 4, "y": 4, "width": 20, "height": 10})).unwrap();
        s.execute("edit.fill", json!({"color": "#ff0000"})).unwrap();
        s.execute("file.new", json!({"width": 32, "height": 32, "name": "b"})).unwrap();
        s
    }

    #[test]
    fn a_page_sleeps_and_wakes_with_its_layers_selection_and_undo() {
        let mut s = session_with_two();
        let st = &s.documents()[0];
        let (id, revision, layers, undo) = (st.doc.id, st.revision, st.doc.layer_count(), st.history.entries().len());
        let pixels = crate::sleep::tests::pixels(&st.doc);
        let sleeping = encode(&st.doc, &st.history.checkpoint()).unwrap();
        assert!(s.can_sleep(0) && !s.can_sleep(1), "the active one doesn't");
        assert!(s.put_to_sleep(id, revision, sleeping));
        assert!(s.is_asleep(0));
        assert_eq!(s.documents()[0].doc.layer_count(), 0);
        assert_eq!((s.documents()[0].doc.name.as_str(), s.documents()[0].doc.size.width), ("a", 64));
        let (doc, history) = decode(s.documents()[0].sleeping.as_ref().unwrap()).unwrap();
        s.wake(id, doc, history).unwrap();
        let st = &s.documents()[0];
        assert!(!s.is_asleep(0));
        assert_eq!((st.doc.id, st.doc.layer_count(), st.history.entries().len()), (id, layers, undo));
        assert!(st.doc.selection.is_some(), "the selection");
        assert_eq!(pixels, crate::sleep::tests::pixels(&st.doc), "the same pixels");
        s.set_active(0);
        s.execute("history.undo", json!({})).unwrap();
    }

    #[test]
    fn a_page_that_changed_meanwhile_stays_awake() {
        let mut s = session_with_two();
        let st = &s.documents()[0];
        let (id, revision) = (st.doc.id, st.revision);
        let sleeping = encode(&st.doc, &st.history.checkpoint()).unwrap();
        s.set_active(0);
        s.execute("edit.fill", json!({"color": "#00ff00"})).unwrap();
        s.set_active(1);
        assert!(!s.put_to_sleep(id, revision, sleeping));
        assert!(!s.is_asleep(0));
    }

    #[test]
    fn commands_wait_for_a_sleeping_active_page() {
        let mut s = session_with_two();
        let st = &s.documents()[0];
        let (id, revision) = (st.doc.id, st.revision);
        let sleeping = encode(&st.doc, &st.history.checkpoint()).unwrap();
        assert!(s.put_to_sleep(id, revision, sleeping));
        s.set_active(0);
        assert!(s.execute("edit.fill", json!({"color": "#0000ff"})).is_err());
        assert!(s.execute("document.activate", json!({"document": 1})).is_ok());
    }

    #[test]
    fn a_sleeping_page_exports_as_it_was() {
        let mut s = session_with_two();
        let page = |s: &Session| {
            let export = crate::zip_export_cmds::prepare(s, &json!({"format": "png"})).unwrap();
            export.render(&export.pages[0]).unwrap()
        };
        let awake = page(&s);
        let st = &s.documents()[0];
        let (id, revision) = (st.doc.id, st.revision);
        let sleeping = encode(&st.doc, &st.history.checkpoint()).unwrap();
        assert!(s.put_to_sleep(id, revision, sleeping));
        assert!(page(&s) == awake, "the same file");
        assert!(s.is_asleep(0), "and it stays asleep");
    }

    pub(super) fn pixels(doc: &Document) -> Vec<u8> {
        let b = photocraft_compose::flatten(doc);
        b.px.iter().flat_map(|p| p.iter().map(|v| (v * 255.0).round() as u8)).collect()
    }
}
