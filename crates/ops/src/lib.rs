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

/// Rebuilds what a stored state left out to save memory: type layers' rendered pixels, which the
/// engine renders again from their text and style (installed by the engine, [`set_rebuild`]).
/// `None`: nothing was left out.
pub type RebuildFn = fn(&Document) -> Option<Document>;

static REBUILD: std::sync::OnceLock<RebuildFn> = std::sync::OnceLock::new();

/// Install the [`RebuildFn`] every document handed out of the history goes through (once).
pub fn set_rebuild(f: RebuildFn) {
    let _ = REBUILD.set(f);
}

fn rebuilt(doc: Arc<Document>) -> Arc<Document> {
    match REBUILD.get().and_then(|f| f(&doc)) {
        Some(d) => Arc::new(d),
        None => doc,
    }
}

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
    /// The layers whose visibility the step that created this state changed, as stepping to this
    /// state from the one before it in the list must apply it; see [`History::current_visibility`].
    pub visibility: Vec<LayerId>,
    /// Stable id within its [`History`] (History panel rows and the History Brush's source
    /// follow it while older states fall off). Never reused.
    id: u64,
}

impl HistoryState {
    /// Build a resident recovery state without copying pixel data.
    pub fn from_document(label: impl Into<String>, document: Arc<Document>) -> Self {
        Self {
            label: label.into(),
            has_selection: document.selection.is_some(),
            document: StoredDocument::Resident(document),
            layers: LayerTarget::default(),
            visibility: Vec::new(),
            id: 0,
        }
    }

    /// Build a lazily restored recovery state. `has_selection` is metadata for
    /// selection menus, which must not read storage merely to draw themselves.
    pub fn from_archive(label: impl Into<String>, has_selection: bool, archive: Arc<dyn ArchivedDocument>) -> Self {
        Self { label: label.into(), has_selection, document: StoredDocument::Archived(archive), layers: LayerTarget::default(), visibility: Vec::new(), id: 0 }
    }

    /// May read storage for a cold state; recovery writers call this on a worker.
    pub fn load_document(&self) -> Result<Arc<Document>, String> {
        self.document.load()
    }

    pub fn has_selection(&self) -> bool {
        self.has_selection
    }

    /// Stable id within its [`History`] (never reused there).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Identity of the stored document (its allocation): with [`Self::id`], tells a step a
    /// recovery writer encoded before from another History's step of the same id.
    pub fn source_key(&self) -> (u64, usize) {
        let ptr = match &self.document {
            StoredDocument::Resident(doc) => Arc::as_ptr(doc) as *const () as usize,
            StoredDocument::Archived(source) => Arc::as_ptr(source) as *const () as usize,
        };
        (self.id, ptr)
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

/// What [`History::rollback`] needs to take the most recent [`History::record`] back.
#[derive(Clone, Debug)]
struct LastRecord {
    /// The document it replaced (the state a rollback makes current again).
    before: Arc<Document>,
    /// How many former redo states it moved onto the undo side (always 0 in linear history).
    moved: usize,
    /// The states it (and the [`History::trim`] after it) dropped for the state limit or the
    /// byte budget, in the order they were taken off, each with its index in the undo list at
    /// that moment (not always the front: the History Brush's source is never dropped). They are
    /// only held until the next change to the list, so a rollback can put them back where they
    /// were.
    evicted: Vec<(usize, HistoryState)>,
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
    /// [`HistoryState::visibility`] of the current state.
    current_visibility: Vec<LayerId>,
    /// History Options › Allow Non-Linear History: a step recorded while later states exist
    /// (after stepping back) keeps them and goes to the end of the list, instead of discarding
    /// them; see [`History::record`] and [`History::remove`].
    pub non_linear: bool,
    /// The most recent [`History::record`], while it is still the latest change to the list;
    /// `None` once anything else changed it. [`History::rollback`] needs it to take the step back
    /// exactly.
    last_record: Option<LastRecord>,
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
            current_visibility: Vec::new(),
            non_linear: false,
            last_record: None,
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
            current_visibility: Vec::new(),
            non_linear: false,
            last_record: None,
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

    /// Record that `before` was replaced by a new current document via step `label`, which left
    /// `layers` targeted.
    ///
    /// Linear history (the default) discards the redo states, as Photoshop does when you edit
    /// after selecting an earlier state. With [`History::non_linear`] they are kept: the list
    /// reads `…, before, the former redo states (oldest first), the new step`, so the new step is
    /// appended at the end and every earlier state can still be stepped to.
    pub fn record(&mut self, label: impl Into<String>, before: Arc<Document>, layers: LayerTarget) {
        self.clear_current_archive();
        // A new step forgets what Clear History took off the list.
        self.cleared = None;
        let prev_label = std::mem::replace(&mut self.current_label, label.into());
        let prev_layers = std::mem::replace(&mut self.current_layers, layers);
        let prev_visibility = std::mem::take(&mut self.current_visibility);
        let prev_id = std::mem::replace(&mut self.current_id, self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        self.undo.push_back(HistoryState {
            label: prev_label,
            has_selection: before.selection.is_some(),
            document: StoredDocument::Resident(before.clone()),
            layers: prev_layers,
            visibility: prev_visibility,
            id: prev_id,
        });
        let mut moved = 0;
        if self.non_linear {
            // `redo` holds the next state last; the list order is oldest first.
            while let Some(state) = self.redo.pop() {
                // The new step follows the last of them in the list but was made from `before`:
                // stepping back to them walks back over their steps.
                merge_ids(&mut self.current_visibility, &state.visibility);
                self.undo.push_back(state);
                moved += 1;
            }
        } else {
            self.redo.clear();
        }
        let mut evicted = Vec::new();
        while self.undo.len() > self.max_states {
            // The History Brush's source doesn't fall off (as in Photoshop): the next oldest does.
            let pinned = self.max_states > 1 && self.undo.front().is_some_and(|s| Some(s.id) == self.brush_source);
            let at = usize::from(pinned);
            let Some(state) = self.undo.remove(at) else { break };
            evicted.push((at, state));
        }
        self.last_record = Some(LastRecord { before, moved, evicted });
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

    /// The layers whose visibility the step that produced the current document changed, as the
    /// session reported them with [`History::add_current_visibility`]. With History Options ›
    /// Make Layer Visibility Changes Undoable off, showing or hiding a layer is not a step, so
    /// stepping through history keeps every other layer's visibility as it is; these layers take
    /// the visibility of the state stepped to, since a recorded step changed them.
    pub fn current_visibility(&self) -> &[LayerId] {
        &self.current_visibility
    }

    /// Note layers whose visibility the current step changed (after [`History::record`], and
    /// again when a later change coalesces into the step).
    pub fn add_current_visibility(&mut self, ids: &[LayerId]) {
        merge_ids(&mut self.current_visibility, ids);
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
        let restored = rebuilt(prev.document.load()?);
        let Some(prev) = self.undo.pop_back() else { return Ok(None) };
        self.last_record = None;
        let label = std::mem::replace(&mut self.current_label, prev.label);
        let layers = std::mem::replace(&mut self.current_layers, prev.layers.clone());
        let visibility = std::mem::replace(&mut self.current_visibility, prev.visibility);
        let has_selection = current.selection.is_some();
        let document = self.take_current_document(current);
        let id = std::mem::replace(&mut self.current_id, prev.id);
        self.redo.push(HistoryState { label, has_selection, document, layers, visibility, id });
        self.current_archive = match prev.document {
            StoredDocument::Archived(archive) => Some(archive),
            _ => None,
        };
        self.current_archive_document = self.current_archive.as_ref().map(|_| Arc::downgrade(&restored));
        Ok(Some((restored, prev.layers)))
    }

    pub fn try_redo(&mut self, current: Arc<Document>) -> Result<Option<(Arc<Document>, LayerTarget)>, String> {
        let Some(next) = self.redo.last() else { return Ok(None) };
        let restored = rebuilt(next.document.load()?);
        let Some(next) = self.redo.pop() else { return Ok(None) };
        self.last_record = None;
        let label = std::mem::replace(&mut self.current_label, next.label);
        let layers = std::mem::replace(&mut self.current_layers, next.layers.clone());
        let visibility = std::mem::replace(&mut self.current_visibility, next.visibility);
        let has_selection = current.selection.is_some();
        let document = self.take_current_document(current);
        let id = std::mem::replace(&mut self.current_id, next.id);
        self.undo.push_back(HistoryState { label, has_selection, document, layers, visibility, id });
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
            Some(k) => self.redo.iter().rev().nth(k - 1).and_then(|s| s.document.load().ok()).map(rebuilt),
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
        self.last_record = None;
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
        self.undo.get(i).map(|state| state.document.load().map(rebuilt)).transpose()
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
        self.last_record = None;
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
        self.last_record = None;
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
        self.last_record = None;
        if self.cleared.take().is_some() {
            return true;
        }
        let Some(state) = self.undo.pop_back() else { return false };
        // The current step now starts where the purged one did.
        merge_ids(&mut self.current_visibility, &state.visibility);
        true
    }

    /// Delete the past state at entry `i` (indexed like [`History::entries`]) and nothing else,
    /// as the History panel's Delete does in non-linear history. The current state is not held
    /// here (the session steps back first) and the oldest state can't be removed this way
    /// either, since every later state would lose its starting point in the list. Returns
    /// whether a state was removed.
    pub fn remove(&mut self, i: usize) -> bool {
        if i == 0 {
            return false;
        }
        let Some(state) = self.undo.remove(i) else { return false };
        self.last_record = None;
        // The state after it now follows the one before it.
        match self.undo.get_mut(i) {
            Some(next) => merge_ids(&mut next.visibility, &state.visibility),
            None => merge_ids(&mut self.current_visibility, &state.visibility),
        }
        true
    }

    /// Delete the later (redo) state `j` (0 = the next redo, as [`History::redo_labels`] lists
    /// them) and nothing else: the History panel's Delete on a dimmed state in non-linear
    /// history. Returns whether a state was removed.
    pub fn remove_later(&mut self, j: usize) -> bool {
        let Some(k) = self.redo.len().checked_sub(j.saturating_add(1)) else { return false };
        let state = self.redo.remove(k);
        self.last_record = None;
        // The state after it in the list (the one before it in `redo`) now follows the state
        // before it.
        if let Some(next) = k.checked_sub(1).and_then(|n| self.redo.get_mut(n)) {
            merge_ids(&mut next.visibility, &state.visibility);
        }
        true
    }

    /// Step back like [`History::undo`] but drop the current document instead of making it
    /// redoable: the current state is deleted while the redo states (later states in the
    /// list) stay. Returns the document to make current and the layers it targeted; `None`
    /// (changing nothing) when there is no earlier state or it can't be read back.
    pub fn drop_current(&mut self) -> Option<(Arc<Document>, LayerTarget)> {
        let restored = rebuilt(self.undo.back()?.document.load().ok()?);
        let prev = self.undo.pop_back()?;
        self.last_record = None;
        self.clear_current_archive();
        self.current_label = prev.label;
        self.current_layers = prev.layers.clone();
        self.current_id = prev.id;
        let dropped = std::mem::replace(&mut self.current_visibility, prev.visibility);
        // The next later state now follows `prev`.
        if let Some(next) = self.redo.last_mut() {
            merge_ids(&mut next.visibility, &dropped);
        }
        Some((restored, prev.layers))
    }

    /// Take back the most recent [`History::record`] exactly, for a compound command whose later
    /// part failed: the document it replaced becomes current again, the current document is
    /// dropped (nothing to redo), and in non-linear history the states that record moved from
    /// the redo side go back there, so the list reads as before the command, with the oldest
    /// states the state limit or the byte budget dropped for it back too. Returns `None`
    /// (changing nothing) when something else changed the list since that record, e.g. an undo;
    /// the caller then falls back to [`History::undo`].
    pub fn rollback(&mut self) -> Option<(Arc<Document>, LayerTarget)> {
        let held = self.last_record.as_ref()?;
        if self.undo.len().saturating_add(held.evicted.len()) <= held.moved {
            return None;
        }
        let LastRecord { before, moved, evicted } = self.last_record.take()?;
        // Put each back where it was taken off, the last one taken off first.
        for (at, state) in evicted.into_iter().rev() {
            self.undo.insert(at.min(self.undo.len()), state);
        }
        for _ in 0..moved {
            let state = self.undo.pop_back()?;
            self.redo.push(state);
        }
        let prev = self.undo.pop_back()?;
        self.clear_current_archive();
        self.current_label = prev.label;
        self.current_layers = prev.layers.clone();
        self.current_visibility = prev.visibility;
        self.current_id = prev.id;
        // The state is the document the record replaced (held resident for this).
        Some((rebuilt(before), prev.layers))
    }

    /// The document the most recent [`History::record`] replaced, while [`History::rollback`]
    /// can still take that record back.
    pub fn recorded_before(&self) -> Option<Arc<Document>> {
        self.last_record.as_ref().map(|held| held.before.clone())
    }

    /// Forget the redo states, e.g. after undoing half of a compound step that failed.
    pub fn clear_redo(&mut self) {
        if !self.redo.is_empty() {
            self.last_record = None;
        }
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
        self.last_record = None;
        self.current_visibility.clear();
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
        for state in self.undo.drain(..drop) {
            // Held for a rollback of the step that pushed them out: each was the front when
            // taken off, so putting them back at the front, the last first, restores the order.
            if let Some(held) = &mut self.last_record {
                held.evicted.push((0, state));
            }
        }
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

/// Add the ids of `from` that `into` lacks.
fn merge_ids(into: &mut Vec<LayerId>, from: &[LayerId]) {
    for id in from {
        if !into.contains(id) {
            into.push(*id);
        }
    }
}

#[cfg(test)]
mod tests;
