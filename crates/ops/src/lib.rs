//! Undo/redo history.
//!
//! Because pixel tiles are `Arc`-shared copy-on-write (see `photocraft-raster`), a full
//! [`Document`] clone costs O(layers + tiles) pointer copies, not pixel copies. History therefore
//! stores whole-document snapshots per transaction, which is simple, obviously correct, and the
//! same approach Photoshop's History panel exposes to users (one state per step).
//!
//! Memory is bounded by `max_states` plus an approximate byte budget. Accounting deduplicates shared tiles; archived snapshots do not retain resident pixels.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use photocraft_doc::{DocId, Document, LayerId};

/// The layers a state targeted when it was created: the active ("key") layer and every selected
/// layer. Undo and redo bring them back with the state's document, as the reference app does;
/// selecting layers is not a step of its own, so it doesn't change a state's target.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayerTarget {
    pub active: Option<LayerId>,
    pub selected: Vec<LayerId>,
}

#[derive(Clone, Debug)]
pub struct HistoryState {
    pub label: String,
    document: StoredDocument,
    has_selection: bool,
    /// The layers targeted when this state was created.
    pub layers: LayerTarget,
    /// Stable id within its [`History`] (History panel rows and the History Brush's source
    /// follow it while older states fall off). Never reused.
    id: u64,
}

impl HistoryState {
    /// Build a resident recovery state without copying pixel data.
    pub fn from_document(label: impl Into<String>, document: Arc<Document>) -> Self {
        Self { label: label.into(), has_selection: document.selection.is_some(), document: StoredDocument::Resident(document), layers: LayerTarget::default(), id: 0 }
    }

    /// Build a lazily restored recovery state. `has_selection` is metadata for
    /// selection menus, which must not read storage merely to draw themselves.
    pub fn from_archive(label: impl Into<String>, has_selection: bool, archive: Arc<dyn ArchivedDocument>) -> Self {
        Self { label: label.into(), has_selection, document: StoredDocument::Archived(archive), layers: LayerTarget::default(), id: 0 }
    }

    /// May read storage for a cold state; recovery writers call this on a worker.
    pub fn load_document(&self) -> Result<Arc<Document>, String> {
        self.document.load()
    }

    pub fn has_selection(&self) -> bool {
        self.has_selection
    }
}

/// Immutable handles and navigation metadata captured alongside the current
/// document. Undo is oldest first; redo's last entry is the next redo state.
/// Capturing this value never hydrates archived documents.
#[derive(Clone, Debug)]
pub struct HistoryCheckpoint {
    pub undo: Vec<HistoryState>,
    pub redo: Vec<HistoryState>,
    pub current_label: String,
    pub max_states: usize,
    pub max_bytes: usize,
    pub current_layers: LayerTarget,
}

/// Engine-owned immutable scratch archive. Loading must preserve the complete document.
pub trait ArchivedDocument: std::fmt::Debug + Send + Sync {
    fn load(&self) -> Result<Arc<Document>, String>;
    fn metadata_bytes(&self) -> usize {
        0
    }
    /// Resident backing for compact replay states; counted once, never mistaken for disk.
    fn resident(&self) -> Option<&Arc<Document>> {
        None
    }
}

#[derive(Debug)]
struct RemappedDocument {
    source: Arc<dyn ArchivedDocument>,
    id: DocId,
}
impl ArchivedDocument for RemappedDocument {
    fn metadata_bytes(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(self.source.metadata_bytes())
    }
    fn resident(&self) -> Option<&Arc<Document>> {
        self.source.resident()
    }
    fn load(&self) -> Result<Arc<Document>, String> {
        let mut doc = self.source.load()?;
        Arc::make_mut(&mut doc).id = self.id;
        Ok(doc)
    }
}

#[derive(Clone, Debug)]
enum StoredDocument {
    Resident(Arc<Document>),
    Archived(Arc<dyn ArchivedDocument>),
}

impl StoredDocument {
    fn load(&self) -> Result<Arc<Document>, String> {
        match self {
            Self::Resident(doc) => Ok(doc.clone()),
            Self::Archived(archive) => archive.load(),
        }
    }
    fn resident(&self) -> Option<&Arc<Document>> {
        match self {
            Self::Resident(doc) => Some(doc),
            Self::Archived(archive) => archive.resident(),
        }
    }
}

/// What History panel › Clear History took off the list, kept for its undo.
#[derive(Clone, Debug)]
struct Cleared {
    undo: VecDeque<HistoryState>,
    redo: Vec<HistoryState>,
    brush_source: Option<u64>,
}

/// The undo label of the step [`History::clear_states`] adds (Edit › Undo Clear History).
pub const CLEAR_HISTORY_LABEL: &str = "Clear History";

#[derive(Clone, Debug)]
pub struct History {
    /// Past states; the last one is the state *before* the current document.
    undo: VecDeque<HistoryState>,
    redo: Vec<HistoryState>,
    pub max_states: usize,
    /// Pixel memory budget in bytes for the current document plus the tiles only history holds
    /// (0 = unlimited). [`History::trim`] drops the oldest states beyond it.
    pub max_bytes: usize,
    /// Label of the step that produced the current document.
    current_label: String,
    /// The layers targeted when the current document was created (opened or edited).
    current_layers: LayerTarget,
    current_archive: Option<Arc<dyn ArchivedDocument>>,
    current_archive_document: Option<std::sync::Weak<Document>>,
    /// The states History panel › Clear History took off the list, kept so the next undo puts
    /// them back (Clear History is itself undoable). Any new step drops them.
    cleared: Option<Box<Cleared>>,
    /// [`HistoryState::id`] of the current state.
    current_id: u64,
    /// The id the next state gets: never reused, so a discarded state's id names no other.
    next_id: u64,
    /// The History Brush's source state, set in the History panel (`None`: the default).
    brush_source: Option<u64>,
}

impl Default for History {
    fn default() -> Self {
        Self::new(50)
    }
}

impl History {
    pub fn checkpoint(&self) -> HistoryCheckpoint {
        HistoryCheckpoint {
            undo: self.undo.iter().cloned().collect(),
            redo: self.redo.clone(),
            current_label: self.current_label.clone(),
            current_layers: self.current_layers.clone(),
            max_states: self.max_states,
            max_bytes: self.max_bytes,
        }
    }

    /// Restore the exact navigation cursor without reading document storage.
    /// Recovery metadata has independent admission limits; preference limits
    /// may subsequently be applied explicitly through `enforce_state_limit`.
    pub fn from_checkpoint(checkpoint: HistoryCheckpoint) -> Result<Self, String> {
        const MAX_RECOVERY_STATES: usize = 10_000;
        const MAX_LABEL_BYTES: usize = 4096;
        if checkpoint.undo.len().saturating_add(checkpoint.redo.len()) > MAX_RECOVERY_STATES {
            return Err("Recovery history exceeds 10000 states".into());
        }
        if checkpoint.current_label.len() > MAX_LABEL_BYTES
            || checkpoint.undo.iter().chain(checkpoint.redo.iter()).any(|state| state.label.len() > MAX_LABEL_BYTES)
        {
            return Err("Recovery history label exceeds 4096 bytes".into());
        }
        if checkpoint.max_states == 0 || checkpoint.max_states > MAX_RECOVERY_STATES {
            return Err("Recovery history state limit must be between 1 and 10000".into());
        }
        // Ids are per History: number the recovered states afresh, oldest first.
        let (mut undo, mut redo) = (checkpoint.undo, checkpoint.redo);
        let mut next_id = 0u64;
        for state in undo.iter_mut() {
            state.id = next_id;
            next_id += 1;
        }
        let current_id = next_id;
        next_id += 1;
        for state in redo.iter_mut().rev() {
            state.id = next_id;
            next_id += 1;
        }
        Ok(Self {
            undo: undo.into(),
            redo,
            cleared: None,
            current_id,
            next_id,
            brush_source: None,
            current_label: checkpoint.current_label,
            current_layers: checkpoint.current_layers,
            current_archive: None,
            current_archive_document: None,
            max_states: checkpoint.max_states,
            max_bytes: checkpoint.max_bytes,
        })
    }

    pub fn new(max_states: usize) -> Self {
        Self {
            undo: VecDeque::new(),
            redo: Vec::new(),
            max_states: max_states.max(1),
            max_bytes: 0,
            current_label: "Open".into(),
            current_layers: LayerTarget::default(),
            current_archive: None,
            current_archive_document: None,
            cleared: None,
            current_id: 0,
            next_id: 1,
            brush_source: None,
        }
    }

    /// Metadata-only stamp for logical history changes (including Purge).
    pub fn checkpoint_signature(&self) -> (usize, usize, &str) {
        (self.undo.len(), self.redo.len(), &self.current_label)
    }

    /// A duplicate recovered tab can be admitted under a fresh identity.
    /// Cold states are remapped only when restored, without reading disk here.
    pub fn remap_document_id(&mut self, id: DocId) {
        for state in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            match &mut state.document {
                StoredDocument::Resident(doc) => {
                    if doc.id != id {
                        Arc::make_mut(doc).id = id;
                    }
                }
                StoredDocument::Archived(source) => {
                    *source = Arc::new(RemappedDocument { source: source.clone(), id });
                }
            }
        }
    }

    /// Record that `before` was replaced by a new current document via step `label`.
    pub fn record(&mut self, label: impl Into<String>, before: Arc<Document>, layers: LayerTarget) {
        self.clear_current_archive();
        // A new step forgets what Clear History took off the list.
        self.cleared = None;
        let prev_label = std::mem::replace(&mut self.current_label, label.into());
        let prev_layers = std::mem::replace(&mut self.current_layers, layers);
        let prev_id = std::mem::replace(&mut self.current_id, self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        self.undo.push_back(HistoryState {
            label: prev_label,
            has_selection: before.selection.is_some(),
            document: StoredDocument::Resident(before),
            layers: prev_layers,
            id: prev_id,
        });
        self.redo.clear();
        while self.undo.len() > self.max_states {
            // The History Brush's source doesn't fall off (as in Photoshop): the next oldest does.
            let pinned = self.max_states > 1 && self.undo.front().is_some_and(|s| Some(s.id) == self.brush_source);
            self.undo.remove(usize::from(pinned));
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty() || self.cleared.is_some()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo_label(&self) -> Option<&str> {
        if self.cleared.is_some() {
            return Some(CLEAR_HISTORY_LABEL);
        }
        self.can_undo().then_some(self.current_label.as_str())
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }
    /// Labels of the redo states, the next redo first (the History panel's greyed rows).
    pub fn redo_labels(&self) -> impl Iterator<Item = &str> {
        self.redo.iter().rev().map(|s| s.label.as_str())
    }

    pub fn set_current_layers(&mut self, layers: LayerTarget) {
        self.current_layers = layers;
    }

    /// Restore first: a failed archive read never moves either history stack.
    pub fn try_undo(&mut self, current: Arc<Document>) -> Result<Option<(Arc<Document>, LayerTarget)>, String> {
        // Undo Clear History: the cleared states come back; the document doesn't change.
        if let Some(cleared) = self.cleared.take() {
            let Cleared { undo, redo, brush_source } = *cleared;
            self.undo = undo;
            self.redo = redo;
            self.brush_source = brush_source;
            return Ok(Some((current, self.current_layers.clone())));
        }
        let Some(prev) = self.undo.back() else { return Ok(None) };
        let restored = prev.document.load()?;
        let Some(prev) = self.undo.pop_back() else { return Ok(None) };
        let label = std::mem::replace(&mut self.current_label, prev.label);
        let layers = std::mem::replace(&mut self.current_layers, prev.layers.clone());
        let has_selection = current.selection.is_some();
        let document = self.take_current_document(current);
        let id = std::mem::replace(&mut self.current_id, prev.id);
        self.redo.push(HistoryState { label, has_selection, document, layers, id });
        self.current_archive = match prev.document {
            StoredDocument::Archived(archive) => Some(archive),
            _ => None,
        };
        self.current_archive_document = self.current_archive.as_ref().map(|_| Arc::downgrade(&restored));
        Ok(Some((restored, prev.layers)))
    }

    pub fn try_redo(&mut self, current: Arc<Document>) -> Result<Option<(Arc<Document>, LayerTarget)>, String> {
        let Some(next) = self.redo.last() else { return Ok(None) };
        let restored = next.document.load()?;
        let Some(next) = self.redo.pop() else { return Ok(None) };
        let label = std::mem::replace(&mut self.current_label, next.label);
        let layers = std::mem::replace(&mut self.current_layers, next.layers.clone());
        let has_selection = current.selection.is_some();
        let document = self.take_current_document(current);
        let id = std::mem::replace(&mut self.current_id, next.id);
        self.undo.push_back(HistoryState { label, has_selection, document, layers, id });
        self.current_archive = match next.document {
            StoredDocument::Archived(archive) => Some(archive),
            _ => None,
        };
        self.current_archive_document = self.current_archive.as_ref().map(|_| Arc::downgrade(&restored));
        Ok(Some((restored, next.layers)))
    }

    pub fn undo(&mut self, current: Arc<Document>) -> Option<(Arc<Document>, LayerTarget)> {
        self.try_undo(current).ok().flatten()
    }
    pub fn redo(&mut self, current: Arc<Document>) -> Option<(Arc<Document>, LayerTarget)> {
        self.try_redo(current).ok().flatten()
    }

    /// Entries for a History panel: past labels oldest→newest, then the current label.
    pub fn entries(&self) -> Vec<String> {
        self.undo.iter().map(|s| s.label.clone()).chain(std::iter::once(self.current_label.clone())).collect()
    }

    /// Document of past entry `i`, indexed like [`History::entries`] (0 = oldest). The last entry is
    /// the current document, which the history does not hold, so it (and any index past it) is `None`.
    pub fn state(&self, i: usize) -> Option<Arc<Document>> {
        self.try_state(i).ok().flatten()
    }

    /// Id of History panel row `i`: the [`History::entries`] (0 = oldest, then the current state),
    /// then the redo states, the next redo first ([`History::redo_labels`]). `None` past the end.
    pub fn row_id(&self, i: usize) -> Option<u64> {
        let past = self.undo.len();
        match i.checked_sub(past) {
            None => self.undo.get(i).map(|s| s.id),
            Some(0) => Some(self.current_id),
            Some(k) => self.redo.iter().rev().nth(k - 1).map(|s| s.id),
        }
    }

    /// The History panel row of the state with id `id`, if it is still held.
    pub fn row_of(&self, id: u64) -> Option<usize> {
        let past = self.undo.len();
        if id == self.current_id {
            return Some(past);
        }
        if let Some(i) = self.undo.iter().position(|s| s.id == id) {
            return Some(i);
        }
        self.redo.iter().rev().position(|s| s.id == id).map(|k| past + 1 + k)
    }

    /// Document of History panel row `i` (see [`History::row_id`]); `current` is the current
    /// document, which the history does not hold. May read scratch storage for a cold state.
    pub fn row_doc(&self, i: usize, current: &Arc<Document>) -> Option<Arc<Document>> {
        let past = self.undo.len();
        match i.checked_sub(past) {
            None => self.state(i),
            Some(0) => Some(current.clone()),
            Some(k) => self.redo.iter().rev().nth(k - 1).and_then(|s| s.document.load().ok()),
        }
    }

    /// Make row `i` the History Brush's source (the History panel's source column). It stays on
    /// that state while older states fall off, and the state itself is kept when it would fall off
    /// the history by count; `false` if there is no such row.
    pub fn set_brush_source(&mut self, i: usize) -> bool {
        let Some(id) = self.row_id(i) else { return false };
        self.brush_source = Some(id);
        true
    }

    /// The History Brush's source row: the oldest state held by default, else the state set with
    /// [`History::set_brush_source`]. `None` once that state is gone.
    pub fn brush_source(&self) -> Option<usize> {
        match self.brush_source {
            None => Some(0),
            Some(id) => self.row_of(id),
        }
    }

    /// Make the History Brush's source the default again (the oldest state held), pinning no state.
    pub fn reset_brush_source(&mut self) {
        self.brush_source = None;
    }

    /// History panel › Clear History: take every state but the current one off the list without
    /// changing the document. Unlike [`Self::clear`] (Edit › Purge › Histories) it is undoable:
    /// the next [`Self::undo`] puts the states back, and the next recorded step forgets them.
    /// Returns false (and changes nothing) when the current state is the only one.
    pub fn clear_states(&mut self) -> bool {
        if self.undo.is_empty() && self.redo.is_empty() {
            return false;
        }
        let undo = std::mem::take(&mut self.undo);
        let redo = std::mem::take(&mut self.redo);
        let brush_source = self.brush_source.take();
        self.cleared = Some(Box::new(Cleared { undo, redo, brush_source }));
        true
    }

    /// Whether the last change to the list was Clear History (so undo restores the states).
    pub fn is_cleared(&self) -> bool {
        self.cleared.is_some()
    }

    /// Every state history holds, cleared ones included while Clear History can be undone.
    fn held_states(&self) -> impl Iterator<Item = &HistoryState> {
        let cleared = self.cleared.as_deref().into_iter().flat_map(|c| c.undo.iter().chain(c.redo.iter()));
        self.undo.iter().chain(self.redo.iter()).chain(cleared)
    }

    /// Metadata-only query for menus; never hydrates a cold snapshot.
    pub fn has_past_selection(&self) -> bool {
        self.undo.iter().any(|state| state.has_selection)
    }

    /// Most recent selection-bearing state, found without reading scratch storage.
    pub fn latest_selection_state(&self) -> Option<usize> {
        self.undo.iter().enumerate().rev().find_map(|(index, state)| state.has_selection.then_some(index))
    }

    pub fn resident_state(&self, i: usize) -> Option<Arc<Document>> {
        self.undo.get(i).and_then(|state| match &state.document {
            StoredDocument::Resident(doc) => Some(doc.clone()),
            _ => None,
        })
    }

    pub fn try_state(&self, i: usize) -> Result<Option<Arc<Document>>, String> {
        self.undo.get(i).map(|state| state.document.load()).transpose()
    }

    /// Resident snapshots only; this never reads scratch storage.
    pub fn resident_states(&self) -> Vec<Arc<Document>> {
        let mut seen = HashSet::new();
        self.held_states()
            .filter_map(|state| state.document.resident().cloned())
            .chain(self.current_archive.iter().filter_map(|archive| archive.resident().cloned()))
            .filter(|doc| seen.insert(Arc::as_ptr(doc) as usize))
            .collect()
    }

    /// Concrete snapshots eligible for scratch serialization. Compact replay backing stays resident.
    pub fn spillable_states(&self) -> Vec<Arc<Document>> {
        self.undo
            .iter()
            .chain(self.redo.iter())
            .filter_map(|state| match &state.document {
                StoredDocument::Resident(doc) => Some(doc.clone()),
                _ => None,
            })
            .collect()
    }

    /// State descriptors and compact replay records, separate from pixel payload accounting.
    pub fn metadata_bytes(&self) -> usize {
        let mut total = self.current_label.capacity().saturating_add(self.current_layers.selected.capacity().saturating_mul(std::mem::size_of::<LayerId>()));
        for state in self.undo.iter().chain(self.redo.iter()) {
            total = total
                .saturating_add(std::mem::size_of::<HistoryState>())
                .saturating_add(state.label.capacity())
                .saturating_add(state.layers.selected.capacity().saturating_mul(std::mem::size_of::<LayerId>()));
            if let StoredDocument::Archived(archive) = &state.document {
                total = total.saturating_add(archive.metadata_bytes());
            }
        }
        if let Some(archive) = &self.current_archive {
            total = total.saturating_add(archive.metadata_bytes());
        }
        total
    }

    pub fn archived_states(&self) -> usize {
        self.undo
            .iter()
            .chain(self.redo.iter())
            .filter(|state| matches!(&state.document, StoredDocument::Archived(archive) if archive.resident().is_none()))
            .count()
    }

    /// Adjacent resident snapshots pinned for immediate undo/redo.
    pub fn hot_states(&self) -> Vec<Arc<Document>> {
        self.undo
            .back()
            .into_iter()
            .chain(self.redo.last())
            .filter_map(|state| state.document.resident().cloned())
            .chain(self.current_archive.iter().filter_map(|archive| archive.resident().cloned()))
            .collect()
    }

    /// Whether this allocation is the immediate undo or redo state.
    /// Used to revalidate asynchronous spill results after intervening navigation.
    pub fn is_hot(&self, doc: &Arc<Document>) -> bool {
        self.undo.back().and_then(|state| state.document.resident()).is_some_and(|hot| Arc::ptr_eq(doc, hot))
            || self.redo.last().and_then(|state| state.document.resident()).is_some_and(|hot| Arc::ptr_eq(doc, hot))
    }

    /// Oldest eligible snapshot, keeping the immediate undo and redo states hot.
    pub fn spill_candidate(&self) -> Option<Arc<Document>> {
        self.undo
            .iter()
            .take(self.undo.len().saturating_sub(1))
            .chain(self.redo.iter().take(self.redo.len().saturating_sub(1)))
            .filter_map(|state| state.document.resident())
            .find(|doc| {
                !self.undo.back().and_then(|state| state.document.resident()).is_some_and(|hot| Arc::ptr_eq(doc, hot))
                    && !self.redo.last().and_then(|state| state.document.resident()).is_some_and(|hot| Arc::ptr_eq(doc, hot))
            })
            .cloned()
    }

    /// Replace only the allocation actually archived; stale worker results are harmless.
    pub fn replace_resident(&mut self, doc: &Arc<Document>, archive: Arc<dyn ArchivedDocument>) -> bool {
        let mut replaced = false;
        for state in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            if matches!(&state.document, StoredDocument::Resident(resident) if Arc::ptr_eq(resident, doc)) {
                state.document = StoredDocument::Archived(archive.clone());
                replaced = true;
            }
        }
        replaced
    }

    /// Budget fallback, preserving the immediate undo and redo steps.
    pub fn drop_oldest(&mut self) -> bool {
        if self.undo.len() > 1 {
            self.undo.pop_front();
            true
        } else if self.redo.len() > 1 {
            self.redo.remove(0);
            true
        } else {
            false
        }
    }

    /// Explicit preference reduction may retire even the final undo/redo state.
    /// The current document is never owned by these stacks.
    pub fn discard_oldest(&mut self) -> bool {
        if self.undo.pop_front().is_some() {
            true
        } else if !self.redo.is_empty() {
            self.redo.remove(0);
            true
        } else {
            false
        }
    }

    /// Enforce a lowered history-count preference without reading scratch storage.
    pub fn enforce_state_limit(&mut self) -> usize {
        let limit = self.max_states.max(1);
        let mut removed = 0;
        while self.undo.len().saturating_add(self.redo.len()) > limit {
            if !self.discard_oldest() {
                break;
            }
            removed += 1;
        }
        removed
    }

    /// Number of past states (entries before the current one).
    pub fn past_len(&self) -> usize {
        self.undo.len()
    }

    /// Rename the current step (e.g. after folding several steps into one with [`Self::purge_last`]).
    pub fn set_current_label(&mut self, label: impl Into<String>) {
        self.current_label = label.into();
    }

    /// Forget the most recent undo state and every redo state (Edit › Purge › Undo): the
    /// last step can no longer be undone and its pixels are released.
    pub fn purge_last(&mut self) -> bool {
        self.redo.clear();
        if self.cleared.take().is_some() {
            return true;
        }
        self.undo.pop_back().is_some()
    }

    /// Forget the redo states, e.g. after undoing half of a compound step that failed.
    pub fn clear_redo(&mut self) {
        self.redo.clear();
    }

    pub fn clear_current_archive(&mut self) {
        self.current_archive = None;
        self.current_archive_document = None;
    }

    pub fn set_current_archive(&mut self, current: &Arc<Document>, archive: Arc<dyn ArchivedDocument>) {
        self.current_archive_document = Some(Arc::downgrade(current));
        self.current_archive = Some(archive);
    }

    fn take_current_document(&mut self, current: Arc<Document>) -> StoredDocument {
        let matches = self.current_archive_document.take().and_then(|bound| bound.upgrade()).is_some_and(|bound| Arc::ptr_eq(&bound, &current));
        match self.current_archive.take() {
            Some(archive) if matches => StoredDocument::Archived(archive),
            _ => StoredDocument::Resident(current),
        }
    }

    /// Forget every past and redo state (Edit › Purge › Histories). The History Brush's source
    /// goes back to the default.
    pub fn clear(&mut self) {
        self.clear_current_archive();
        self.undo.clear();
        self.redo.clear();
        self.cleared = None;
        self.reset_brush_source();
    }

    /// Bytes only `extra` documents hold (History panel snapshots): not `current`'s and not any
    /// resident state's. 0 without `extra`, without walking the states.
    pub fn extra_bytes(&self, current: &Document, extra: &[&Document]) -> usize {
        if extra.is_empty() {
            return 0;
        }
        let mut seen = HashSet::new();
        document_bytes(current, &mut seen);
        for doc in self.resident_states() {
            document_bytes(&doc, &mut seen);
        }
        extra.iter().fold(0usize, |n, d| n.saturating_add(document_bytes(d, &mut seen)))
    }

    /// [`History::unique_bytes`] less the bytes `keep` documents also hold: what clearing the
    /// history frees while they stay (Edit › Purge › Histories keeps the snapshots).
    pub fn unique_bytes_besides(&self, current: &Document, keep: &[&Document]) -> usize {
        let mut seen = HashSet::new();
        document_bytes(current, &mut seen);
        for d in keep {
            document_bytes(d, &mut seen);
        }
        self.resident_states().iter().fold(0usize, |n, doc| n.saturating_add(document_bytes(doc, &mut seen)))
    }

    /// [`History::trim`] with the bytes only `pinned` documents hold (History panel snapshots)
    /// counted against the budget too.
    pub fn trim_with(&mut self, current: &Document, pinned: &[&Document]) -> usize {
        if pinned.is_empty() {
            return self.trim(current);
        }
        let mut seen = HashSet::new();
        document_bytes(current, &mut seen);
        let extra = pinned.iter().fold(0usize, |n, d| n.saturating_add(document_bytes(d, &mut seen)));
        let budget = self.max_bytes;
        if budget == 0 {
            return 0;
        }
        self.max_bytes = budget.saturating_sub(extra).max(1);
        let n = self.trim(current);
        self.max_bytes = budget;
        n
    }

    /// Approximate unique tile and shared binary-payload bytes held by history.
    pub fn unique_bytes(&self, current: &Document) -> usize {
        let mut seen = HashSet::new();
        document_bytes(current, &mut seen);
        self.resident_states().iter().fold(0usize, |n, doc| n.saturating_add(document_bytes(doc, &mut seen)))
    }

    /// Managed bytes of `current` plus the allocations only history holds (what [`History::trim`]
    /// bounds by [`History::max_bytes`]).
    pub fn pixel_bytes(&self, current: &Document) -> usize {
        let mut seen = HashSet::new();
        let own = document_bytes(current, &mut seen);
        self.resident_states().iter().fold(own, |n, doc| n.saturating_add(document_bytes(doc, &mut seen)))
    }

    /// Keep pixel memory within [`History::max_bytes`]: the current document's tiles plus the
    /// tiles only history holds (newest states first). The oldest undo states that don't fit are
    /// dropped; the most recent one is always kept so the last step can be undone. Returns how
    /// many states were dropped.
    pub fn trim(&mut self, current: &Document) -> usize {
        if self.max_bytes == 0 || self.undo.len() <= 1 {
            return 0;
        }
        let mut seen = HashSet::new();
        let mut total = document_bytes(current, &mut seen);
        for s in self.redo.iter().rev() {
            if let Some(doc) = s.document.resident() {
                total = total.saturating_add(document_bytes(doc, &mut seen));
            }
        }
        let mut keep = 0;
        for s in self.undo.iter().rev() {
            if let Some(doc) = s.document.resident() {
                total = total.saturating_add(document_bytes(doc, &mut seen));
            }
            if total > self.max_bytes && keep >= 1 {
                break;
            }
            keep += 1;
        }
        let drop = self.undo.len() - keep;
        self.undo.drain(..drop);
        drop
    }
}

/// Managed tile and shared binary-payload bytes not already in `seen`.
/// Metadata containers, strings, renderer caches and allocator overhead are additional memory.
pub fn document_bytes(doc: &Document, seen: &mut HashSet<usize>) -> usize {
    let mut total = 0usize;
    for_each_payload(doc, &mut |ptr, bytes| {
        if seen.insert(ptr) {
            total = total.saturating_add(bytes);
        }
    });
    total
}

/// Every managed payload of `doc` that [`document_bytes`] counts: `f(address, bytes)` per tile and
/// shared binary payload, in the same order, a payload shared within the document once per use.
/// Distinct live payloads have distinct addresses (across payload types too).
pub fn for_each_payload(doc: &Document, f: &mut dyn FnMut(usize, usize)) {
    let mut add = |surface: &photocraft_doc::Surface| {
        for (_, tile) in surface.tiles() {
            f(Arc::as_ptr(tile) as usize, tile.bytes().len());
        }
    };
    let layers = doc.walk();
    for (_, _, layer) in &layers {
        if let Some(surface) = layer.surface() {
            add(surface);
        }
        if let Some(mask) = &layer.mask {
            add(&mask.surface);
        }
        if let Some(cache) = &layer.fill_cache {
            add(&cache.surface);
        }
        if let photocraft_doc::LayerContent::Smart(smart) = &layer.content
            && let Some(mask) = &smart.filter_mask
        {
            add(&mask.surface);
        }
        if let Some(video) = &layer.video {
            for frame in &video.frames {
                add(frame);
            }
        }
    }
    for channel in doc.channels.iter().chain(&doc.quick_mask) {
        add(&channel.surface);
    }
    if let Some(selection) = &doc.selection {
        add(selection);
    }
    for pattern in &doc.patterns {
        add(&pattern.surface);
    }
    // Distinct live Arc allocations cannot have the same address, including across
    // payload types. A caller keeps all traversed documents alive while deduplicating.
    let mut blob = |bytes: &Arc<Vec<u8>>| f(Arc::as_ptr(bytes) as usize, bytes.capacity());
    for bytes in doc.icc_profile.iter().chain(doc.metadata.exif.iter()) {
        blob(bytes);
    }
    for (_, _, bytes) in &doc.metadata.psd_resources {
        blob(bytes);
    }
    for (_, _, bytes) in &doc.metadata.psd_global_blocks {
        blob(bytes);
    }
    for path in &doc.paths {
        if let Some(bytes) = &path.psd_raw {
            blob(bytes);
        }
    }
    for (_, _, layer) in &layers {
        for (_, bytes) in &layer.psd_blocks {
            blob(bytes);
        }
        if let Some(bytes) = &layer.effects.psd_raw {
            blob(bytes);
        }
        match &layer.content {
            photocraft_doc::LayerContent::Text(text) => {
                if let Some(bytes) = &text.psd_raw {
                    blob(bytes);
                }
            }
            photocraft_doc::LayerContent::Shape(shape) => {
                if let Some(bytes) = &shape.psd_raw {
                    blob(bytes);
                }
            }
            photocraft_doc::LayerContent::Smart(smart) => {
                if let Some(bytes) = &smart.psd_raw {
                    blob(bytes);
                }
                if let photocraft_doc::SmartSource::Embedded { bytes, .. } = &smart.source {
                    blob(bytes);
                }
            }
            _ => {}
        }
    }
    for (_, _, layer) in &layers {
        if let photocraft_doc::LayerContent::Adjustment(photocraft_doc::Adjustment::ColorLookup { lut: Some(lut), .. }) = &layer.content {
            f(Arc::as_ptr(lut) as usize, lut.capacity().saturating_mul(std::mem::size_of::<f32>()));
        }
    }
}

/// [`document_bytes`] over a set of history snapshots kept up to date between calls: a command
/// adds one snapshot and retires another, so only those two are walked instead of every
/// snapshot again (walking all of history after each command cost 10-50 ms on 20-layer manga
/// pages in the browser). Snapshots must not change while counted (history states are immutable; a document
/// changed in place is a different set member only once it is a new allocation).
///
/// Each snapshot is held weakly, so a dropped one is noticed (its address can't be reused while
/// the memo remembers it) and its payloads are released by reference count.
#[derive(Default)]
pub struct PayloadMemo {
    /// Counted snapshots and their distinct payload addresses.
    states: Vec<(std::sync::Weak<Document>, Vec<usize>)>,
    /// Payload address → (bytes, number of counted snapshots holding it).
    counts: std::collections::HashMap<usize, (usize, u32)>,
    total: usize,
}

impl PayloadMemo {
    /// Count exactly `states`; returns their unique payload bytes, as [`document_bytes`] over all
    /// of them with one `seen` set would.
    pub fn sync(&mut self, states: &[Arc<Document>]) -> usize {
        let want: HashSet<usize> = states.iter().map(|d| Arc::as_ptr(d) as usize).collect();
        let mut kept = Vec::with_capacity(self.states.len());
        for (state, payloads) in std::mem::take(&mut self.states) {
            if state.strong_count() > 0 && want.contains(&(state.as_ptr() as usize)) {
                kept.push((state, payloads));
                continue;
            }
            for p in payloads {
                if let std::collections::hash_map::Entry::Occupied(mut e) = self.counts.entry(p) {
                    let (bytes, n) = e.get_mut();
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        self.total = self.total.saturating_sub(*bytes);
                        e.remove();
                    }
                }
            }
        }
        self.states = kept;
        let mut have: HashSet<usize> = self.states.iter().map(|(s, _)| s.as_ptr() as usize).collect();
        for doc in states {
            if !have.insert(Arc::as_ptr(doc) as usize) {
                continue;
            }
            let (mut local, mut payloads) = (HashSet::new(), Vec::new());
            let (counts, total) = (&mut self.counts, &mut self.total);
            for_each_payload(doc, &mut |p, bytes| {
                if !local.insert(p) {
                    return;
                }
                payloads.push(p);
                let e = counts.entry(p).or_insert((bytes, 0));
                if e.1 == 0 {
                    *total = total.saturating_add(bytes);
                }
                e.1 = e.1.saturating_add(1);
            });
            self.states.push((Arc::downgrade(doc), payloads));
        }
        self.total
    }

    /// Bytes of `doc`'s payloads that neither the counted snapshots nor `seen` hold (added to
    /// `seen`): the current document on top of [`Self::sync`]'s total.
    pub fn extra_bytes(&self, doc: &Document, seen: &mut HashSet<usize>) -> usize {
        let mut total = 0usize;
        for_each_payload(doc, &mut |p, bytes| {
            if !self.counts.contains_key(&p) && seen.insert(p) {
                total = total.saturating_add(bytes);
            }
        });
        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::{Color, ColorMode, Layer, SampleType, Size};

    fn base() -> Document {
        Document::with_background("h", Size::new(64, 64), ColorMode::Rgb, SampleType::U8, Color::WHITE)
    }

    /// Apply an edit the way the engine does: snapshot, mutate a clone, record.
    fn edit(h: &mut History, cur: &mut Arc<Document>, label: &str, f: impl FnOnce(&mut Document)) {
        let before = cur.clone();
        let mut d = (**cur).clone();
        f(&mut d);
        *cur = Arc::new(d);
        h.record(label, before, LayerTarget::default());
    }

    #[test]
    fn payload_memo_counts_like_one_walk_and_keeps_no_snapshot_alive() {
        let mut a = base();
        if let Some(s) = a.layers[0].surface_mut() {
            s.write_pixel(1, 1, &[1.0, 0.0, 0.0, 1.0]);
        }
        let a = Arc::new(a);
        let mut b = (*a).clone();
        b.insert_above(None, Layer::raster("L", b.pixel_format()));
        if let Some(s) = b.layers[1].surface_mut() {
            s.write_pixel(3, 3, &[0.0, 1.0, 0.0, 1.0]);
        }
        let b = Arc::new(b);
        let walked = |docs: &[Arc<Document>]| {
            let mut seen = HashSet::new();
            docs.iter().map(|d| document_bytes(d, &mut seen)).sum::<usize>()
        };
        let mut memo = PayloadMemo::default();
        assert_eq!(memo.sync(&[a.clone(), b.clone()]), walked(&[a.clone(), b.clone()]));
        assert_eq!(memo.sync(std::slice::from_ref(&b)), walked(std::slice::from_ref(&b)), "a retired snapshot releases only its own tiles");
        assert_eq!(memo.sync(&[a.clone(), b.clone()]), walked(&[a.clone(), b.clone()]));
        assert_eq!(Arc::strong_count(&a), 1, "the memo holds snapshots weakly");
        drop(a);
        assert_eq!(memo.sync(std::slice::from_ref(&b)), walked(std::slice::from_ref(&b)), "a dropped snapshot is noticed");
        let mut seen = HashSet::new();
        assert_eq!(memo.extra_bytes(&b, &mut seen), 0, "everything in b is counted already");
    }

    #[test]
    fn undo_redo_cycle() {
        let mut h = History::default();
        let mut cur = Arc::new(base());
        edit(&mut h, &mut cur, "New Layer", |d| {
            d.insert_above(None, Layer::raster("L", d.pixel_format()));
        });
        assert_eq!(cur.layers.len(), 2);
        assert_eq!(h.undo_label(), Some("New Layer"));
        cur = h.undo(cur).unwrap().0;
        assert_eq!(cur.layers.len(), 1);
        assert_eq!(h.redo_label(), Some("New Layer"));
        cur = h.redo(cur).unwrap().0;
        assert_eq!(cur.layers.len(), 2);
        assert!(h.redo(cur.clone()).is_none());
    }

    #[test]
    fn undo_and_redo_return_the_layers_each_state_targeted_when_created() {
        let mut h = History::default();
        let open = Arc::new(base());
        let bg = open.layers[0].id;
        let target = |id: LayerId| LayerTarget { active: Some(id), selected: vec![id] };
        h.set_current_layers(target(bg));
        // New Layer targets the new layer; the background is then selected (not a step) and filled.
        let mut d = (*open).clone();
        let new = d.insert_above(None, Layer::raster("L", d.pixel_format()));
        let layered = Arc::new(d);
        h.record("New Layer", open, target(new));
        let mut d = (*layered).clone();
        d.name = "filled".into();
        h.record("Fill", layered, target(bg));
        let (d, layers) = h.undo(Arc::new(d)).unwrap();
        assert_eq!(layers, target(new), "New Layer's state targeted the new layer when created");
        let (d, layers) = h.undo(d).unwrap();
        assert_eq!(layers, target(bg), "the opened document's target");
        let (d, layers) = h.redo(d).unwrap();
        assert_eq!(layers, target(new));
        let (_, layers) = h.redo(d).unwrap();
        assert_eq!(layers, target(bg), "Fill's state targeted the background");
    }

    #[test]
    fn state_accessor_matches_entries() {
        let mut h = History::default();
        let mut cur = Arc::new(base());
        let open = cur.clone();
        edit(&mut h, &mut cur, "A", |d| d.name = "a".into());
        edit(&mut h, &mut cur, "B", |d| d.name = "b".into());
        assert_eq!(h.entries().len(), h.past_len() + 1);
        assert!(Arc::ptr_eq(&h.state(0).unwrap(), &open));
        assert_eq!(h.state(1).unwrap().name, "a");
        assert!(h.state(2).is_none(), "the current document is not held");
        cur = h.undo(cur).unwrap().0;
        h.undo(cur).unwrap();
        assert_eq!(h.redo_labels().collect::<Vec<_>>(), ["A", "B"], "next redo first");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut h = History::default();
        let mut cur = Arc::new(base());
        edit(&mut h, &mut cur, "A", |d| d.name = "a".into());
        cur = h.undo(cur).unwrap().0;
        edit(&mut h, &mut cur, "B", |d| d.name = "b".into());
        assert!(!h.can_redo());
        assert_eq!(h.entries(), vec!["Open".to_string(), "B".to_string()]);
    }

    #[test]
    fn max_states_bound() {
        let mut h = History::new(3);
        let mut cur = Arc::new(base());
        for i in 0..10 {
            edit(&mut h, &mut cur, &format!("step {i}"), |d| d.name = format!("{i}"));
        }
        let mut n = 0;
        while let Some((d, _)) = h.undo(cur.clone()) {
            cur = d;
            n += 1;
        }
        assert_eq!(n, 3);
        assert_eq!(cur.name, "6");
    }

    #[test]
    fn byte_budget_drops_oldest_states_but_keeps_one() {
        let mut h = History::new(50);
        let mut cur = Arc::new(base());
        let tile = 256 * 256 * 4;
        // Each step repaints the background's tile: one unique tile per state.
        for i in 0..6 {
            edit(&mut h, &mut cur, &format!("paint {i}"), |d| {
                let id = d.layers[0].id;
                d.layer_mut(id).unwrap().surface_mut().unwrap().write_pixel(1, 1, &[i as f32 / 8.0, 0.0, 0.0, 1.0]);
            });
        }
        assert_eq!(h.trim(&cur), 0, "unlimited by default");
        assert_eq!(h.past_len(), 6);
        // The current tile plus three history tiles fit.
        h.max_bytes = 4 * tile;
        assert_eq!(h.trim(&cur), 3);
        assert_eq!(h.past_len(), 3);
        assert_eq!(h.unique_bytes(&cur), 3 * tile);
        assert_eq!(h.pixel_bytes(&cur), 4 * tile, "the current tile plus history's");
        assert_eq!(h.entries()[0], "paint 2");
        // A budget smaller than one state still keeps the last step undoable.
        h.max_bytes = 1;
        h.trim(&cur);
        assert_eq!(h.past_len(), 1);
        assert!(h.undo(cur.clone()).is_some());
    }

    #[test]
    fn history_shares_untouched_tiles() {
        let mut h = History::default();
        let mut cur = Arc::new(base());
        assert_eq!(h.unique_bytes(&cur), 0);
        // rename only: no pixel changes, history holds no unique tiles
        edit(&mut h, &mut cur, "Rename", |d| d.name = "x".into());
        assert_eq!(h.unique_bytes(&cur), 0);
        // paint one pixel: exactly one tile becomes unique to history
        edit(&mut h, &mut cur, "Paint", |d| {
            let id = d.layers[0].id;
            d.layer_mut(id).unwrap().surface_mut().unwrap().write_pixel(1, 1, &[0.0, 0.0, 0.0, 1.0]);
        });
        assert_eq!(h.unique_bytes(&cur), 256 * 256 * 4);
    }
    #[derive(Debug)]
    struct Archive(Result<Arc<Document>, String>);
    impl ArchivedDocument for Archive {
        fn load(&self) -> Result<Arc<Document>, String> {
            self.0.clone()
        }
    }

    #[test]
    fn recovery_checkpoint_preserves_cursor_labels_and_both_stack_orders() {
        let mut history = History::new(37);
        history.max_bytes = 1234;
        let mut current = Arc::new(base());
        for name in ["A", "B", "C", "D"] {
            edit(&mut history, &mut current, name, |doc| doc.name = name.into());
        }
        current = history.try_undo(current).unwrap().unwrap().0;
        current = history.try_undo(current).unwrap().unwrap().0;
        assert_eq!(current.name, "B");
        let checkpoint = history.checkpoint();
        assert_eq!(checkpoint.current_label, "B");
        assert_eq!(checkpoint.undo.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["Open", "A"]);
        assert_eq!(checkpoint.redo.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["D", "C"]);
        let mut recovered = History::from_checkpoint(checkpoint).unwrap();
        assert_eq!(recovered.max_states, 37);
        assert_eq!(recovered.max_bytes, 1234);
        for expected in ["C", "D"] {
            current = recovered.try_redo(current).unwrap().unwrap().0;
            assert_eq!(current.name, expected);
        }
        for expected in ["C", "B", "A"] {
            current = recovered.try_undo(current).unwrap().unwrap().0;
            assert_eq!(current.name, expected);
        }
        assert_eq!(recovered.redo_label(), Some("B"));
    }

    #[test]
    fn recovery_checkpoint_keeps_cold_states_lazy_and_failed_navigation_atomic() {
        let mut checkpoint = History::new(50).checkpoint();
        checkpoint.current_label = "Current".into();
        checkpoint.undo.push(HistoryState::from_archive("Before", true, Arc::new(Archive(Err("unreadable recovery".into())))));
        checkpoint.redo.push(HistoryState::from_document("After", Arc::new(base())));
        let mut history = History::from_checkpoint(checkpoint).unwrap();
        let captured = history.checkpoint();
        assert!(captured.undo.first().unwrap().has_selection());
        assert!(captured.undo.first().unwrap().load_document().is_err());
        assert_eq!(history.archived_states(), 1);
        assert!(history.has_past_selection());
        assert!(history.try_undo(Arc::new(base())).is_err());
        assert_eq!(history.entries(), ["Before", "Current"]);
        assert_eq!(history.redo_label(), Some("After"));
        assert_eq!(history.past_len(), 1);
    }

    #[test]
    fn recovered_identity_remaps_resident_and_cold_states_without_changing_sources() {
        let original = Arc::new(base());
        let old_id = original.id;
        let new_id = DocId::fresh();
        let mut checkpoint = History::default().checkpoint();
        checkpoint.undo.push(HistoryState::from_archive("Cold", false, Arc::new(Archive(Ok(original.clone())))));
        checkpoint.redo.push(HistoryState::from_document("Resident", original.clone()));
        let mut history = History::from_checkpoint(checkpoint).unwrap();
        history.remap_document_id(new_id);
        assert_eq!(history.archived_states(), 1, "remapping does not hydrate cold history");
        let checkpoint = history.checkpoint();
        assert_eq!(checkpoint.undo[0].load_document().unwrap().id, new_id);
        assert_eq!(checkpoint.redo[0].load_document().unwrap().id, new_id);
        assert_eq!(original.id, old_id, "other tabs retain their identities");
    }

    #[test]
    fn recovery_checkpoint_preserves_targeting_across_undo_and_redo() {
        let before = Arc::new(base());
        let first = before.layers.first().unwrap().id;
        let target = LayerTarget { active: Some(first), selected: vec![first] };
        let mut history = History::default();
        history.set_current_layers(target.clone());
        history.record("Move", before.clone(), target.clone());
        let mut restored = History::from_checkpoint(history.checkpoint()).unwrap();
        let (doc, layers) = restored.try_undo(before).unwrap().unwrap();
        assert_eq!(layers, target);
        assert_eq!(restored.try_redo(doc).unwrap().unwrap().1, target);
    }

    #[test]
    fn recovery_checkpoint_rejects_oversized_metadata() {
        let mut checkpoint = History::new(50).checkpoint();
        checkpoint.current_label = "x".repeat(4097);
        assert!(History::from_checkpoint(checkpoint).is_err());
        let mut checkpoint = History::new(50).checkpoint();
        let state = HistoryState::from_document("State", Arc::new(base()));
        checkpoint.undo = vec![state.clone(); 10_000];
        checkpoint.redo.push(state);
        assert!(History::from_checkpoint(checkpoint).is_err());
        let mut checkpoint = History::new(50).checkpoint();
        checkpoint.max_states = 0;
        assert!(History::from_checkpoint(checkpoint).is_err());
    }

    #[test]
    fn cold_restore_and_failed_read_preserve_stacks() {
        let mut history = History::new(50);
        let mut current = Arc::new(base());
        let original = current.clone();
        edit(&mut history, &mut current, "A", |doc| doc.name = "a".into());
        let labels = history.entries();
        assert!(history.replace_resident(&original, Arc::new(Archive(Err("corrupt scratch blob".into())))));
        assert_eq!(history.archived_states(), 1);
        assert!(history.try_state(0).is_err());
        assert!(history.try_undo(current.clone()).is_err());
        assert_eq!(history.entries(), labels);
        assert!(!history.can_redo());
        history.clear();
        history.record("A", original.clone(), LayerTarget::default());
        assert!(history.replace_resident(&original, Arc::new(Archive(Ok(original.clone())))));
        let restored = history.try_undo(current.clone()).unwrap().unwrap().0;
        assert!(Arc::ptr_eq(&restored, &original));
        assert!(Arc::ptr_eq(&history.try_redo(restored).unwrap().unwrap().0, &current));
    }

    #[test]
    fn replacement_deduplicates_and_stale_results_are_ignored() {
        let mut history = History::new(50);
        let original = Arc::new(base());
        history.record("A", original.clone(), LayerTarget::default());
        history.record("B", original.clone(), LayerTarget::default());
        assert!(history.spill_candidate().is_none(), "shared immediate undo allocation stays hot");
        assert!(history.replace_resident(&original, Arc::new(Archive(Ok(original.clone())))));
        assert_eq!(history.archived_states(), 2);
        assert!(history.resident_states().is_empty());
        assert!(!history.replace_resident(&original, Arc::new(Archive(Ok(original.clone())))));
        assert!(history.drop_oldest());
        assert!(!history.drop_oldest());
        history.clear();
        assert_eq!(history.archived_states(), 0);
    }

    #[test]
    fn selection_patterns_and_video_pixels_are_accounted_once() {
        let mut doc = base();
        let format = doc.pixel_format();
        let mut surface = photocraft_doc::Surface::new(format);
        surface.write_pixel(0, 0, &[0.25, 0.5, 0.75, 1.0]);
        doc.selection = Some(surface.clone());
        doc.patterns.push(photocraft_doc::Pattern::new("P", surface.clone(), 1, 1));
        doc.layers[0].video = Some(photocraft_doc::VideoData::new(vec![surface], 24.0));
        let mut seen = HashSet::new();
        assert_eq!(document_bytes(&doc, &mut seen), 2 * 256 * 256 * 4);
        assert_eq!(document_bytes(&doc, &mut seen), 0);
    }

    #[test]
    fn cold_redo_failure_keeps_the_next_step() {
        let mut history = History::new(50);
        let mut current = Arc::new(base());
        edit(&mut history, &mut current, "A", |doc| doc.name = "a".into());
        let future = current.clone();
        current = history.try_undo(current).unwrap().unwrap().0;
        assert!(history.replace_resident(&future, Arc::new(Archive(Err("read failed".into())))));
        let label = history.redo_label().map(str::to_owned);
        assert!(history.try_redo(current).is_err());
        assert_eq!(history.redo_label(), label.as_deref());
        assert!(!history.can_undo());
    }
    #[test]
    fn menu_queries_never_load_a_cold_selection() {
        let mut history = History::new(50);
        let mut doc = base();
        doc.selection = Some(photocraft_doc::Surface::new(doc.pixel_format()));
        let doc = Arc::new(doc);
        history.record("Select", doc.clone(), LayerTarget::default());
        assert!(history.replace_resident(&doc, Arc::new(Archive(Err("must not read".into())))));
        assert!(history.has_past_selection());
        assert_eq!(history.latest_selection_state(), Some(0));
        history.record("Clear selection", Arc::new(base()), LayerTarget::default());
        assert_eq!(history.latest_selection_state(), Some(0), "skip newer no-selection states without loading archives");
        assert!(history.resident_state(0).is_none());
        assert!(history.try_state(0).is_err());
    }

    #[test]
    fn spill_and_drop_keep_adjacent_steps_hot_and_contiguous() {
        let mut history = History::new(50);
        let mut current = Arc::new(base());
        let oldest = current.clone();
        for label in ["A", "B", "C"] {
            edit(&mut history, &mut current, label, |doc| doc.name = label.into());
        }
        assert!(Arc::ptr_eq(&history.spill_candidate().unwrap(), &oldest));
        assert!(history.drop_oldest());
        assert_eq!(history.entries(), ["A", "B", "C"]);
        current = history.try_undo(current).unwrap().unwrap().0;
        current = history.try_undo(current).unwrap().unwrap().0;
        assert_eq!(current.name, "A");
        assert!(history.drop_oldest(), "discard farthest redo");
        assert_eq!(history.redo_label(), Some("B"));
        assert!(!history.drop_oldest());
        assert_eq!(history.try_redo(current).unwrap().unwrap().0.name, "B");
    }
    #[test]
    fn explicit_budget_reduction_can_discard_final_cold_state() {
        let mut history = History::new(50);
        let doc = Arc::new(base());
        history.record("A", doc.clone(), LayerTarget::default());
        history.replace_resident(&doc, Arc::new(Archive(Err("must not load".into()))));
        assert!(!history.drop_oldest());
        assert!(history.discard_oldest());
        assert!(!history.can_undo());
        assert!(!history.discard_oldest());
    }

    #[test]
    fn lowered_count_bounds_both_stacks_without_loading() {
        let mut history = History::new(50);
        let mut current = Arc::new(base());
        for label in ["A", "B", "C"] {
            edit(&mut history, &mut current, label, |doc| doc.name = label.into());
        }
        current = history.undo(current).unwrap().0;
        history.max_states = 1;
        assert_eq!(history.enforce_state_limit(), 2);
        assert_eq!(history.past_len(), 0);
        assert_eq!(history.redo_label(), Some("C"));
        assert_eq!(history.redo(current).unwrap().0.name, "C");
    }
    #[test]
    fn asynchronous_candidate_can_become_hot_after_navigation() {
        let mut history = History::new(50);
        let mut current = Arc::new(base());
        let oldest = current.clone();
        for label in ["A", "B", "C"] {
            edit(&mut history, &mut current, label, |doc| doc.name = label.into());
        }
        assert!(!history.is_hot(&oldest));
        current = history.undo(current).unwrap().0;
        let _current = history.undo(current).unwrap().0;
        assert!(history.is_hot(&oldest));
        assert!(history.spill_candidate().is_none_or(|candidate| !Arc::ptr_eq(&candidate, &oldest)));
    }
    #[test]
    fn large_shared_binary_payloads_are_counted_once() {
        let mut doc = base();
        let bytes = Arc::new(vec![17; 4096]);
        doc.icc_profile = Some(bytes.clone());
        doc.metadata.exif = Some(bytes.clone());
        doc.metadata.psd_resources.push((1, "resource".into(), bytes.clone()));
        doc.metadata.psd_global_blocks.push((*b"8BIM", *b"lnk2", bytes.clone()));
        doc.layers[0].psd_blocks.push((*b"test", bytes));
        let baseline = 256 * 256 * 4;
        let mut seen = HashSet::new();
        assert_eq!(document_bytes(&doc, &mut seen), baseline + 4096);
        let mut clone = doc.clone();
        clone.metadata.exif = Some(Arc::new(vec![4; 1024]));
        assert_eq!(document_bytes(&clone, &mut seen), 1024);
    }
}
