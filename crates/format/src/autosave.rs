//! Durable document/history checkpoints and bounded background autosave.
//! A sidecar is the single atomic publication point. Immutable objects are
//! shared across checkpoints; a failed save never replaces that descriptor.
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use photocraft_doc::Document;
use photocraft_ops::{ArchivedDocument, History, HistoryCheckpoint, HistoryState};
use photocraft_raster::Tile;
use serde::{Deserialize, Serialize};

use crate::store::{self, ByteSource, write_atomic};
use crate::{FormatError, LoadOptions, PcraftWriter, Result, SaveOptions, SaveStats};

const MAX_DESCRIPTOR: u64 = 256 << 20;
/// Hard admission cap; full recovery storage fails visibly rather than growing indefinitely.
const MAX_RECOVERY_BYTES: u64 = 8 << 30;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryInfo {
    pub key: String,
    pub document_name: String,
    pub original_path: Option<String>,
    pub saved_at: u64,
    pub revision: u64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryEntry {
    pub info: RecoveryInfo,
    pub bundle: PathBuf,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredLayerTarget {
    active: Option<photocraft_doc::LayerId>,
    selected: Vec<photocraft_doc::LayerId>,
}
impl From<photocraft_ops::LayerTarget> for StoredLayerTarget {
    fn from(value: photocraft_ops::LayerTarget) -> Self {
        Self { active: value.active, selected: value.selected }
    }
}
impl From<StoredLayerTarget> for photocraft_ops::LayerTarget {
    fn from(value: StoredLayerTarget) -> Self {
        Self { active: value.active, selected: value.selected }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredState {
    label: String,
    has_selection: bool,
    manifest: String,
    #[serde(default)]
    layers: StoredLayerTarget,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Checkpoint {
    version: u32,
    current: String,
    undo: Vec<StoredState>,
    redo: Vec<StoredState>,
    current_label: String,
    #[serde(default)]
    current_layers: StoredLayerTarget,
    max_states: usize,
    max_bytes: usize,
}
#[derive(Serialize, Deserialize)]
struct Descriptor {
    #[serde(flatten)]
    info: RecoveryInfo,
    #[serde(default)]
    checkpoint: Option<Checkpoint>,
    #[serde(default)]
    context: serde_json::Value,
}
struct Job {
    snapshot: Arc<Document>,
    history: HistoryCheckpoint,
    info: RecoveryInfo,
    opts: SaveOptions,
    context: serde_json::Value,
}
#[derive(Debug, Clone)]
pub struct AutosaveCompletion {
    pub revision: u64,
    pub result: std::result::Result<SaveStats, String>,
}
#[derive(Default)]
struct Mailbox {
    pending: Option<Job>,
    completion: Option<AutosaveCompletion>,
    last: Option<AutosaveCompletion>,
    stopped: bool,
    discard: bool,
    failure: Option<String>,
}
/// Exactly one in-flight snapshot and one newest pending snapshot are retained.
pub struct Autosaver {
    dir: PathBuf,
    key: String,
    shared: Arc<(Mutex<Mailbox>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}
fn sanitize(key: &str) -> String {
    let s: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if s.is_empty() { "untitled".into() } else { s }
}
fn safe_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 200 && key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
impl Autosaver {
    pub fn new(recovery_dir: impl Into<PathBuf>, key: &str) -> Self {
        let dir = recovery_dir.into();
        let key = sanitize(key);
        let shared = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
        let state = shared.clone();
        let bundle = dir.join(format!("{key}.pcraft"));
        let sidecar = dir.join(format!("{key}.json"));
        let spawned = std::thread::Builder::new().name(format!("autosave-{key}")).spawn(move || worker(state, bundle, sidecar));
        let handle = match spawned {
            Ok(handle) => Some(handle),
            Err(error) => {
                shared.0.lock().unwrap_or_else(PoisonError::into_inner).failure = Some(format!("Could not start autosave worker: {error}"));
                None
            }
        };
        Self { dir, key, shared, handle }
    }
    pub fn bundle_path(&self) -> PathBuf {
        self.dir.join(format!("{}.pcraft", self.key))
    }
    /// Queue acceptance is not durable completion; poll [`Self::take_completion`].
    pub fn request(&self, snapshot: Arc<Document>, revision: u64, original_path: Option<String>, opts: SaveOptions) -> std::result::Result<(), String> {
        self.request_checkpoint(snapshot, History::default().checkpoint(), revision, original_path, opts)
    }
    pub fn request_checkpoint(
        &self,
        snapshot: Arc<Document>,
        history: HistoryCheckpoint,
        revision: u64,
        original_path: Option<String>,
        opts: SaveOptions,
    ) -> std::result::Result<(), String> {
        self.request_checkpoint_with_context(snapshot, history, revision, original_path, opts, serde_json::Value::Null)
    }
    pub fn request_checkpoint_with_context(
        &self,
        snapshot: Arc<Document>,
        history: HistoryCheckpoint,
        revision: u64,
        original_path: Option<String>,
        opts: SaveOptions,
        context: serde_json::Value,
    ) -> std::result::Result<(), String> {
        let context_bytes = serde_json::to_vec(&context).map_err(|e| e.to_string())?;
        if context_bytes.len() > 64 << 10 {
            return Err("Recovery view context exceeds 64 KiB".into());
        }
        let mut state = self.shared.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.stopped || self.handle.as_ref().is_none_or(JoinHandle::is_finished) {
            return Err("Autosave worker has stopped".into());
        }
        if !safe_key(&self.key) {
            return Err("Recovery key exceeds its safe filename limit".into());
        }
        let info = RecoveryInfo {
            key: self.key.clone(),
            document_name: snapshot.name.clone(),
            original_path,
            saved_at: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
            revision,
        };
        state.pending = Some(Job { snapshot, history, info, opts, context });
        self.shared.1.notify_one();
        Ok(())
    }
    /// Retire on the worker without blocking the caller. Poll after `is_finished`.
    pub fn begin_discard(&self) -> std::result::Result<(), String> {
        let mut state = self.shared.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        state.pending = None;
        state.discard = true;
        state.stopped = true;
        self.shared.1.notify_one();
        Ok(())
    }
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }
    pub fn take_completion(&self) -> Option<AutosaveCompletion> {
        self.shared.0.lock().unwrap_or_else(PoisonError::into_inner).completion.take()
    }
    pub fn last_result(&self) -> Option<std::result::Result<SaveStats, String>> {
        self.shared.0.lock().unwrap_or_else(PoisonError::into_inner).last.as_ref().map(|c| c.result.clone())
    }
    pub fn flush(mut self) -> Option<std::result::Result<SaveStats, String>> {
        self.shutdown();
        self.last_result()
    }
    pub fn discard(mut self) -> Result<()> {
        self.shutdown();
        remove_entry(&self.dir, &self.key)
    }
    fn shutdown(&mut self) {
        self.shared.0.lock().unwrap_or_else(PoisonError::into_inner).stopped = true;
        self.shared.1.notify_one();
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            let mut state = self.shared.0.lock().unwrap_or_else(PoisonError::into_inner);
            let revision = state.last.as_ref().map_or(0, |c| c.revision);
            let c = AutosaveCompletion { revision, result: Err("Autosave worker terminated unexpectedly".into()) };
            state.last = Some(c.clone());
            state.completion = Some(c);
        }
    }
}
impl Drop for Autosaver {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn worker(shared: Arc<(Mutex<Mailbox>, Condvar)>, bundle: PathBuf, sidecar: PathBuf) {
    let mut writer = PcraftWriter::new();
    let mut verified = HashMap::new();
    loop {
        let job = {
            let mut state = shared.0.lock().unwrap_or_else(PoisonError::into_inner);
            while state.pending.is_none() && !state.stopped {
                state = shared.1.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
            let Some(job) = state.pending.take() else {
                let discard = state.discard;
                let revision = state.last.as_ref().map_or(0, |c| c.revision);
                drop(state);
                if discard {
                    let result = match (bundle.parent(), bundle.file_stem().and_then(|s| s.to_str())) {
                        (Some(dir), Some(key)) => remove_entry(dir, key).map(|_| SaveStats::default()).map_err(|e| e.to_string()),
                        _ => Err("Invalid recovery location".into()),
                    };
                    let c = AutosaveCompletion { revision, result };
                    let mut state = shared.0.lock().unwrap_or_else(PoisonError::into_inner);
                    state.last = Some(c.clone());
                    state.completion = Some(c);
                }
                return;
            };
            job
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| save_checkpoint(&mut writer, &mut verified, &job, &bundle, &sidecar)))
            .map_err(|_| "Autosave could not complete because its worker panicked; your document remains open".to_string())
            .and_then(|r| r.map_err(|e| e.to_string()));
        let completion = AutosaveCompletion { revision: job.info.revision, result };
        let mut state = shared.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.last = Some(completion.clone());
        state.completion = Some(completion);
    }
}
fn bundle_bytes(root: &Path) -> Result<u64> {
    let mut total = 0u64;
    for sub in ["tiles", "blobs"] {
        let path = root.join(sub);
        if !path.exists() {
            continue;
        }
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(FormatError::corrupt("unexpected recovery object directory"));
            }
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}
fn prepare_state(
    writer: &mut PcraftWriter,
    verified: &mut HashMap<PathBuf, FileFingerprint>,
    doc: &Document,
    opts: &SaveOptions,
    bundle: &Path,
    bytes: &mut u64,
    stats: &mut SaveStats,
) -> Result<String> {
    let prepared = writer.prepare(doc, opts)?;
    if prepared.manifest.len() as u64 > MAX_DESCRIPTOR
        || prepared.decoded_bytes > LoadOptions::default().max_total_bytes
        || prepared.objects.values().any(|object| matches!(object, store::Object::Blob(blob) if blob.len() > LoadOptions::default().max_blob_bytes))
    {
        return Err(FormatError::LimitExceeded("recovery state exceeds restore limits".into()));
    }
    for (name, object) in prepared.objects {
        let path = bundle.join(&name);
        if std::fs::symlink_metadata(&path).is_ok() {
            ensure_file(&path)?;
            let fingerprint = file_fingerprint(&path)?;
            if verified.get(&path) != Some(&fingerprint) {
                // First reuse verifies canonical lossless bytes. Later saves
                // reuse unchanged file identity without rehashing large tiles.
                let expected = object.compressed();
                let matches = fingerprint.len == expected.len() as u64 && read_bounded(&path, expected.len() as u64)? == expected;
                if !matches {
                    let next = bytes.saturating_sub(fingerprint.len).saturating_add(expected.len() as u64);
                    if next > MAX_RECOVERY_BYTES {
                        return Err(FormatError::LimitExceeded("recovery storage is full".into()));
                    }
                    write_atomic(&path, &expected)?;
                    *bytes = next;
                }
                verified.insert(path.clone(), file_fingerprint(&path)?);
            }
            if matches!(object, store::Object::Tile(..)) {
                stats.tiles_reused += 1;
            }
            continue;
        }
        let compressed = object.compressed();
        let next = bytes.saturating_add(compressed.len() as u64);
        if next > MAX_RECOVERY_BYTES {
            return Err(FormatError::LimitExceeded("recovery storage is full (8 GiB); save your document and free recovery storage".into()));
        }
        write_atomic(&path, &compressed)?;
        verified.insert(path.clone(), file_fingerprint(&path)?);
        *bytes = next;
        match object {
            store::Object::Tile(..) => stats.tiles_written += 1,
            store::Object::Blob(..) => stats.blobs_written += 1,
        }
    }
    String::from_utf8(prepared.manifest).map_err(|_| FormatError::corrupt("invalid UTF-8 manifest"))
}
fn save_checkpoint(writer: &mut PcraftWriter, verified: &mut HashMap<PathBuf, FileFingerprint>, job: &Job, bundle: &Path, sidecar: &Path) -> Result<SaveStats> {
    // Serialize publication and deferred cleanup so a retired root cannot be
    // deleted while a replacement checkpoint is being written.
    let root_registry = roots().lock().unwrap_or_else(PoisonError::into_inner);
    History::from_checkpoint(job.history.clone()).map_err(FormatError::corrupt)?;
    if let Some(parent) = bundle.parent() {
        create_directory(parent)?;
    }
    create_directory(bundle)?;
    for sub in ["tiles", "blobs"] {
        create_directory(&bundle.join(sub))?;
    }
    let mut bytes = bundle_bytes(bundle)?;
    let mut stats = SaveStats::default();
    let current = prepare_state(writer, verified, &job.snapshot, &job.opts, bundle, &mut bytes, &mut stats)?;
    let mut save_states = |states: &[HistoryState]| -> Result<Vec<StoredState>> {
        let mut stored = Vec::with_capacity(states.len());
        for state in states {
            let doc = state.load_document().map_err(FormatError::corrupt)?;
            let manifest = prepare_state(writer, verified, &doc, &SaveOptions::default(), bundle, &mut bytes, &mut stats)?;
            stored.push(StoredState { label: state.label.clone(), has_selection: state.has_selection(), manifest, layers: state.layers.clone().into() });
        }
        Ok(stored)
    };
    let undo = save_states(&job.history.undo)?;
    let redo = save_states(&job.history.redo)?;
    let checkpoint = Checkpoint {
        version: 1,
        current,
        undo,
        redo,
        current_label: job.history.current_label.clone(),
        current_layers: job.history.current_layers.clone().into(),
        max_states: job.history.max_states,
        max_bytes: job.history.max_bytes,
    };
    let descriptor = serde_json::to_vec(&Descriptor { info: job.info.clone(), checkpoint: Some(checkpoint), context: job.context.clone() })?;
    if descriptor.len() as u64 > MAX_DESCRIPTOR || bytes.saturating_add(descriptor.len() as u64) > MAX_RECOVERY_BYTES {
        return Err(FormatError::LimitExceeded("recovery checkpoint is too large".into()));
    }
    stats.manifest_bytes = descriptor.len();
    if std::fs::symlink_metadata(sidecar).is_ok() {
        ensure_file(sidecar)?;
    }
    write_atomic(sidecar, &descriptor)?;
    // A recovered lazy history pins the entire root conservatively. This keeps
    // cold states valid after another checkpoint replaces the descriptor.
    let pinned = root_registry.get(bundle).and_then(Weak::upgrade).is_some();
    if !pinned {
        let mut hashes = std::collections::HashSet::new();
        let published: Descriptor = match serde_json::from_slice(&descriptor) {
            Ok(published) => published,
            Err(_) => return Ok(stats),
        };
        if let Some(checkpoint) = published.checkpoint {
            for manifest in std::iter::once(&checkpoint.current).chain(checkpoint.undo.iter().chain(checkpoint.redo.iter()).map(|s| &s.manifest)) {
                let value: serde_json::Value = match serde_json::from_str(manifest) {
                    Ok(value) => value,
                    Err(_) => return Ok(stats),
                };
                collect_hashes(&value, &mut hashes);
            }
        }
        for sub in ["tiles", "blobs"] {
            if let Ok(entries) = std::fs::read_dir(bundle.join(sub)) {
                for entry in entries.flatten() {
                    if entry.path().file_stem().and_then(|s| s.to_str()).is_some_and(|h| !hashes.contains(h)) && std::fs::remove_file(entry.path()).is_ok() {
                        stats.objects_removed += 1;
                    }
                }
            }
        }
        verified.retain(|path, _| path.file_stem().and_then(|stem| stem.to_str()).is_some_and(|hash| hashes.contains(hash)));
    }
    Ok(stats)
}

#[derive(Debug)]
struct RootLease {
    root: PathBuf,
    retired: AtomicBool,
    tiles: Mutex<HashMap<String, Weak<Tile>>>,
    blobs: Mutex<HashMap<String, Weak<Vec<u8>>>>,
}
impl Drop for RootLease {
    fn drop(&mut self) {
        // Lazy history handles can drop on the UI thread: cleanup is queued.
        if self.retired.load(Ordering::Acquire)
            && std::fs::symlink_metadata(self.root.with_extension("json")).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            && let Some(tx) = cleanup_sender()
        {
            let _ = tx.send(self.root.clone());
        }
    }
}
type Roots = Mutex<HashMap<PathBuf, Weak<RootLease>>>;
fn roots() -> &'static Roots {
    static ROOTS: OnceLock<Roots> = OnceLock::new();
    ROOTS.get_or_init(Mutex::default)
}
fn root_lease(root: &Path) -> Arc<RootLease> {
    let mut roots = roots().lock().unwrap_or_else(PoisonError::into_inner);
    roots.retain(|_, lease| lease.strong_count() > 0);
    if let Some(lease) = roots.get(root).and_then(Weak::upgrade) {
        return lease;
    }
    let lease = Arc::new(RootLease { root: root.to_path_buf(), retired: AtomicBool::new(false), tiles: Mutex::default(), blobs: Mutex::default() });
    roots.insert(root.to_path_buf(), Arc::downgrade(&lease));
    lease
}
fn cleanup_sender() -> Option<&'static mpsc::Sender<PathBuf>> {
    static CLEANUP: OnceLock<Option<mpsc::Sender<PathBuf>>> = OnceLock::new();
    CLEANUP
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<PathBuf>();
            std::thread::Builder::new()
                .name("recovery-cleanup".into())
                .spawn(move || {
                    while let Ok(root) = rx.recv() {
                        let registry = roots().lock().unwrap_or_else(PoisonError::into_inner);
                        let pinned = registry.get(&root).and_then(Weak::upgrade).is_some();
                        if !pinned
                            && std::fs::symlink_metadata(root.with_extension("json")).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                            && ensure_directory(&root).is_ok()
                        {
                            let _ = std::fs::remove_dir_all(root);
                        }
                    }
                })
                .ok()
                .map(|_| tx)
        })
        .as_ref()
}
#[derive(Debug)]
struct RecoverySource {
    lease: Arc<RootLease>,
    manifest: String,
}
impl ByteSource for RecoverySource {
    fn get(&self, path: &str, max: usize) -> Result<Vec<u8>> {
        if path == store::MANIFEST {
            if self.manifest.len() > max {
                return Err(FormatError::LimitExceeded("recovery manifest too large".into()));
            }
            return Ok(self.manifest.as_bytes().to_vec());
        }
        let relative = Path::new(path);
        if relative.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
            return Err(FormatError::corrupt("unsafe recovery object path"));
        }
        ensure_directory(&self.lease.root)?;
        if let Some(parent) = relative.parent() {
            ensure_directory(&self.lease.root.join(parent))?;
        }
        read_bounded(&self.lease.root.join(relative), max as u64)
    }
}
impl RecoverySource {
    fn load_native(&self) -> Result<Document> {
        // Reuse the undo archive's native decoder cache. Weak entries share
        // unchanged live pixels/blobs without retaining a second RAM copy.
        let mut tiles = self.lease.tiles.lock().unwrap_or_else(PoisonError::into_inner);
        let mut blobs = self.lease.blobs.lock().unwrap_or_else(PoisonError::into_inner);
        tiles.retain(|_, value| value.strong_count() > 0);
        blobs.retain(|_, value| value.strong_count() > 0);
        store::load_cached(self, &LoadOptions::default(), Some(&mut tiles), Some(&mut blobs))
    }
}
impl ArchivedDocument for RecoverySource {
    fn load(&self) -> std::result::Result<Arc<Document>, String> {
        self.load_native().map(Arc::new).map_err(|e| e.to_string())
    }
}

fn descriptor(entry: &RecoveryEntry) -> Result<Descriptor> {
    if !safe_key(&entry.info.key) || entry.bundle.file_name().and_then(|n| n.to_str()) != Some(format!("{}.pcraft", entry.info.key).as_str()) {
        return Err(FormatError::corrupt("unsafe recovery key"));
    }
    if let Some(parent) = entry.bundle.parent() {
        ensure_directory(parent)?;
    }
    let path = entry.bundle.with_extension("json");
    ensure_directory(&entry.bundle)?;
    let result: Descriptor = serde_json::from_slice(&read_bounded(&path, MAX_DESCRIPTOR)?)?;
    if result.info.key != entry.info.key {
        return Err(FormatError::corrupt("recovery descriptor key mismatch"));
    }
    Ok(result)
}
/// Discovery errors are reported without deleting suspect files.
pub fn list_recovery_checked(recovery_dir: &Path) -> (Vec<RecoveryEntry>, Vec<String>) {
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    if std::fs::symlink_metadata(recovery_dir).is_ok()
        && let Err(e) = ensure_directory(recovery_dir)
    {
        return (entries, vec![e.to_string()]);
    }
    let rd = match std::fs::read_dir(recovery_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (entries, errors),
        Err(e) => return (entries, vec![format!("Could not inspect recovery storage: {e}")]),
    };
    for item in rd {
        let item = match item {
            Ok(item) => item,
            Err(e) => {
                errors.push(e.to_string());
                continue;
            }
        };
        let path = item.path();
        if !path.extension().is_some_and(|ext| ext == "json") {
            continue;
        }
        let read = (|| -> Result<Descriptor> { Ok(serde_json::from_slice(&read_bounded(&path, MAX_DESCRIPTOR)?)?) })();
        match read {
            Ok(d) if safe_key(&d.info.key) && path.file_stem().and_then(|n| n.to_str()) == Some(d.info.key.as_str()) => {
                let bundle = recovery_dir.join(format!("{}.pcraft", d.info.key));
                if ensure_directory(&bundle).is_ok() && (d.checkpoint.is_some() || bundle.join(store::MANIFEST).is_file()) {
                    entries.push(RecoveryEntry { info: d.info, bundle });
                } else {
                    errors.push(format!("Recovery bundle is missing: {}", path.display()));
                }
            }
            Ok(_) => errors.push(format!("Unsafe recovery key in {}", path.display())),
            Err(e) => errors.push(format!("Could not read recovery {}: {e}", path.display())),
        }
    }
    entries.sort_by(|a, b| b.info.saved_at.cmp(&a.info.saved_at).then(a.info.key.cmp(&b.info.key)));
    (entries, errors)
}
pub fn list_recovery(recovery_dir: &Path) -> Vec<RecoveryEntry> {
    list_recovery_checked(recovery_dir).0
}
pub fn recover_checkpoint(entry: &RecoveryEntry) -> Result<(Document, HistoryCheckpoint)> {
    recover_checkpoint_with_context(entry).map(|(doc, history, _)| (doc, history))
}
pub fn recover_checkpoint_with_context(entry: &RecoveryEntry) -> Result<(Document, HistoryCheckpoint, serde_json::Value)> {
    if !safe_key(&entry.info.key) || entry.bundle.file_name().and_then(|n| n.to_str()) != Some(format!("{}.pcraft", entry.info.key).as_str()) {
        return Err(FormatError::corrupt("unsafe recovery key"));
    }
    // Pin before reading publication: a concurrent commit must not collect
    // objects named by the descriptor that this recovery reader observes.
    let lease = root_lease(&entry.bundle);
    let descriptor = descriptor(entry)?;
    if serde_json::to_vec(&descriptor.context)?.len() > 64 << 10 {
        return Err(FormatError::LimitExceeded("recovery context exceeds 64 KiB".into()));
    }
    let Some(checkpoint) = descriptor.checkpoint else {
        let bytes = read_bounded(&entry.bundle.join(store::MANIFEST), LoadOptions::default().max_manifest_bytes as u64)?;
        let manifest = String::from_utf8(bytes).map_err(|_| FormatError::corrupt("invalid legacy manifest UTF-8"))?;
        let doc = store::load(&RecoverySource { lease: lease.clone(), manifest }, &LoadOptions::default())?;
        return Ok((doc, History::default().checkpoint(), descriptor.context));
    };
    if checkpoint.version != 1 {
        return Err(FormatError::Unsupported("newer recovery checkpoint version".into()));
    }
    if checkpoint.undo.len().saturating_add(checkpoint.redo.len()) > 10_000 {
        return Err(FormatError::LimitExceeded("too many recovery history states".into()));
    }
    let current = RecoverySource { lease: lease.clone(), manifest: checkpoint.current }.load_native()?;
    let restore = |states: Vec<StoredState>| -> Result<Vec<HistoryState>> {
        states
            .into_iter()
            .map(|state| {
                let source = RecoverySource { lease: lease.clone(), manifest: state.manifest };
                // Validate manifest syntax now; object reads remain lazy and fallible.
                store::read_manifest(&source, &LoadOptions::default())?;
                let mut restored = HistoryState::from_archive(state.label, state.has_selection, Arc::new(source));
                restored.layers = state.layers.into();
                Ok(restored)
            })
            .collect()
    };
    let history = HistoryCheckpoint {
        undo: restore(checkpoint.undo)?,
        redo: restore(checkpoint.redo)?,
        current_label: checkpoint.current_label,
        current_layers: checkpoint.current_layers.into(),
        max_states: checkpoint.max_states,
        max_bytes: checkpoint.max_bytes,
    };
    History::from_checkpoint(history.clone()).map_err(FormatError::corrupt)?;
    Ok((current, history, descriptor.context))
}
pub fn recover(entry: &RecoveryEntry) -> Result<Document> {
    recover_checkpoint(entry).map(|(doc, _)| doc)
}
pub fn discard_recovery(recovery_dir: &Path, entry: &RecoveryEntry) -> Result<()> {
    if !safe_key(&entry.info.key) || entry.bundle != recovery_dir.join(format!("{}.pcraft", entry.info.key)) {
        return Err(FormatError::corrupt("unsafe recovery key"));
    }
    remove_entry(recovery_dir, &entry.info.key)
}
fn remove_entry(dir: &Path, key: &str) -> Result<()> {
    if !safe_key(key) {
        return Err(FormatError::corrupt("unsafe recovery key"));
    }
    let registry = roots().lock().unwrap_or_else(PoisonError::into_inner);
    ensure_directory(dir)?;
    let root = dir.join(format!("{key}.pcraft"));
    if std::fs::symlink_metadata(&root).is_ok() {
        ensure_directory(&root)?;
    }
    let sidecar = dir.join(format!("{key}.json"));
    if std::fs::symlink_metadata(&sidecar).is_ok() {
        ensure_file(&sidecar)?;
        std::fs::remove_file(sidecar)?;
    }
    let active = registry.get(&root).and_then(Weak::upgrade);
    if let Some(active) = active {
        active.retired.store(true, Ordering::Release);
    } else if root.exists() {
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

fn collect_hashes(value: &serde_json::Value, out: &mut std::collections::HashSet<String>) {
    match value {
        serde_json::Value::String(s) if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) => {
            out.insert(s.clone());
        }
        serde_json::Value::Array(a) => {
            for v in a {
                collect_hashes(v, out);
            }
        }
        serde_json::Value::Object(o) => {
            for v in o.values() {
                collect_hashes(v, out);
            }
        }
        _ => {}
    }
}

fn ensure_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(FormatError::corrupt("recovery directory is a symlink or not a directory"));
    }
    Ok(())
}
fn create_directory(path: &Path) -> Result<()> {
    if std::fs::symlink_metadata(path).is_err() {
        std::fs::create_dir_all(path)?;
    }
    ensure_directory(path)
}
fn ensure_file(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(FormatError::corrupt("recovery file is a symlink or not a regular file"));
    }
    Ok(())
}
fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    ensure_file(path)?;
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > max {
        return Err(FormatError::LimitExceeded("recovery file exceeds read limit".into()));
    }
    ensure_file(path)?;
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(FormatError::LimitExceeded("recovery file exceeds read limit".into()));
    }
    Ok(bytes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}
fn file_fingerprint(path: &Path) -> Result<FileFingerprint> {
    let metadata = std::fs::symlink_metadata(path)?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(FileFingerprint {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    })
}

// ---------------------------------------------------------------------------
// Checkpoints as named objects (storage without a filesystem: the browser's)
// ---------------------------------------------------------------------------

/// A recovery checkpoint as named objects, for storage that has no directories or atomic
/// renames (the browser's origin-private file system or IndexedDB). Same descriptor and objects
/// as a recovery bundle ([`Autosaver`]): the compressed tiles and blobs by name (`tiles/<hash>`,
/// `blobs/<hash>`), then the descriptor, which publishes the checkpoint and so is stored last.
pub struct ObjectCheckpoint {
    /// The descriptor (the bundle's `<key>.json` sidecar).
    pub descriptor: Vec<u8>,
    /// The compressed objects the storage doesn't hold yet.
    pub objects: Vec<(String, Vec<u8>)>,
    /// Every object the descriptor names: once it is stored, any other object of the key can go.
    pub live: Vec<String>,
}

/// How much undo history an [`ObjectCheckpoint`] keeps: the newest `max_states` steps (redo
/// steps count too) whose own objects (tiles and blobs the current state doesn't hold) take at
/// most `max_bytes` compressed. Older steps are left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryLimits {
    pub max_states: usize,
    pub max_bytes: u64,
}

impl HistoryLimits {
    /// No history: the current state only.
    pub const NONE: Self = Self { max_states: 0, max_bytes: 0 };
}

/// One history step as encoded for a checkpoint, kept for the next one (steps don't change).
struct EncodedState {
    stored: StoredState,
    /// The objects its manifest names (handles to the step's own tiles and blobs).
    objects: BTreeMap<String, store::Object>,
}

/// Encodes recovery checkpoints of one document as [`ObjectCheckpoint`]s: the current state and
/// the newest undo steps within [`HistoryLimits`] (the browser keeps its storage small).
/// Objects stored by an earlier checkpoint ([`Self::committed`]) aren't encoded again, so a
/// checkpoint after a small edit writes a few tiles and the descriptor; a history step encoded
/// once is reused by id.
#[derive(Default)]
pub struct ObjectAutosaver {
    writer: PcraftWriter,
    stored: std::collections::HashSet<String>,
    /// History steps encoded by an earlier checkpoint, by [`HistoryState::source_key`].
    states: HashMap<(u64, usize), EncodedState>,
}

impl ObjectAutosaver {
    /// The checkpoint of `doc` described by `info` (its key, name, time and revision) and the
    /// view `context`, without history.
    pub fn checkpoint(&mut self, doc: &Document, info: RecoveryInfo, context: serde_json::Value) -> Result<ObjectCheckpoint> {
        self.checkpoint_with_history(doc, None, HistoryLimits::NONE, info, context)
    }

    /// [`Self::checkpoint`] keeping the newest steps of `history` within `limits`, so Undo works
    /// in the recovered document. A step costs the bytes of the objects the current state and
    /// the newer steps kept don't already hold (uncompressed, so the cap is conservative).
    pub fn checkpoint_with_history(
        &mut self,
        doc: &Document,
        history: Option<&HistoryCheckpoint>,
        limits: HistoryLimits,
        info: RecoveryInfo,
        context: serde_json::Value,
    ) -> Result<ObjectCheckpoint> {
        if !safe_key(&info.key) {
            return Err(FormatError::corrupt("unsafe recovery key"));
        }
        if serde_json::to_vec(&context)?.len() > 64 << 10 {
            return Err(FormatError::LimitExceeded("recovery context exceeds 64 KiB".into()));
        }
        let prepared = self.writer.prepare(doc, &SaveOptions::default())?;
        let restorable = |p: &store::Prepared| {
            p.manifest.len() as u64 <= MAX_DESCRIPTOR
                && p.decoded_bytes <= LoadOptions::default().max_total_bytes
                && !p.objects.values().any(|object| matches!(object, store::Object::Blob(blob) if blob.len() > LoadOptions::default().max_blob_bytes))
        };
        if !restorable(&prepared) {
            return Err(FormatError::LimitExceeded("recovery state exceeds restore limits".into()));
        }
        let current = String::from_utf8(prepared.manifest).map_err(|_| FormatError::corrupt("invalid UTF-8 manifest"))?;
        let mut all: BTreeMap<String, store::Object> = prepared.objects;
        // History: newest undo steps first, then the nearest redo steps, while the limits allow.
        let empty = History::default().checkpoint();
        let history = history.unwrap_or(&empty);
        let (mut undo, mut redo) = (Vec::new(), Vec::new());
        let mut spent = 0u64;
        let mut keep = std::collections::HashSet::new();
        let steps = history.undo.iter().rev().map(|s| (true, s)).chain(history.redo.iter().rev().map(|s| (false, s)));
        for (is_undo, state) in steps.take(limits.max_states) {
            if !self.states.contains_key(&state.source_key()) {
                let Ok(d) = state.load_document() else { break };
                let p = self.writer.prepare(&d, &SaveOptions::default())?;
                if !restorable(&p) {
                    break;
                }
                let manifest = String::from_utf8(p.manifest).map_err(|_| FormatError::corrupt("invalid UTF-8 manifest"))?;
                let stored = StoredState { label: state.label.clone(), has_selection: state.has_selection(), manifest, layers: state.layers.clone().into() };
                self.states.insert(state.source_key(), EncodedState { stored, objects: p.objects });
            }
            let e = &self.states[&state.source_key()];
            let cost: u64 = e.objects.iter().filter(|(n, _)| !all.contains_key(*n)).map(|(_, o)| o.raw_len()).sum();
            if spent.saturating_add(cost) > limits.max_bytes {
                break;
            }
            spent += cost;
            for (n, o) in &e.objects {
                all.entry(n.clone()).or_insert_with(|| o.clone());
            }
            keep.insert(state.source_key());
            if is_undo {
                undo.push(e.stored.clone());
            } else {
                redo.push(e.stored.clone());
            }
        }
        self.states.retain(|id, _| keep.contains(id));
        undo.reverse();
        redo.reverse();
        let live: Vec<String> = all.keys().cloned().collect();
        let objects = all.iter().filter(|(name, _)| !self.stored.contains(*name)).map(|(name, object)| (name.clone(), object.compressed())).collect();
        let checkpoint = Checkpoint {
            version: 1,
            current,
            undo,
            redo,
            current_label: history.current_label.clone(),
            current_layers: history.current_layers.clone().into(),
            max_states: history.max_states,
            max_bytes: history.max_bytes,
        };
        let descriptor = serde_json::to_vec(&Descriptor { info, checkpoint: Some(checkpoint), context })?;
        if descriptor.len() as u64 > MAX_DESCRIPTOR {
            return Err(FormatError::LimitExceeded("recovery checkpoint is too large".into()));
        }
        Ok(ObjectCheckpoint { descriptor, objects, live })
    }

    /// A checkpoint's objects and descriptor are stored: later checkpoints skip `live`.
    pub fn committed(&mut self, live: Vec<String>) {
        self.stored = live.into_iter().collect();
    }

    /// What the storage holds is unknown (a write failed): the next checkpoint writes everything.
    pub fn forget(&mut self) {
        self.stored.clear();
    }
}

/// What a stored [`ObjectCheckpoint`] descriptor describes (for listing; nothing is decoded).
pub fn object_checkpoint_info(descriptor: &[u8]) -> Result<RecoveryInfo> {
    if descriptor.len() as u64 > MAX_DESCRIPTOR {
        return Err(FormatError::LimitExceeded("recovery descriptor too large".into()));
    }
    #[derive(Deserialize)]
    struct Head {
        #[serde(flatten)]
        info: RecoveryInfo,
    }
    let head: Head = serde_json::from_slice(descriptor)?;
    if !safe_key(&head.info.key) {
        return Err(FormatError::corrupt("unsafe recovery key"));
    }
    Ok(head.info)
}

/// A stored [`ObjectCheckpoint`]'s manifest over its objects in memory.
struct MemorySource {
    manifest: String,
    objects: Arc<HashMap<String, Vec<u8>>>,
}

impl ByteSource for MemorySource {
    fn get(&self, path: &str, max: usize) -> Result<Vec<u8>> {
        let bytes = if path == store::MANIFEST { self.manifest.as_bytes() } else { self.objects.get(path).ok_or_else(|| FormatError::corrupt(format!("missing `{path}`")))? };
        if bytes.len() > max {
            return Err(FormatError::LimitExceeded(format!("`{path}` exceeds its size limit")));
        }
        Ok(bytes.to_vec())
    }
}

impl std::fmt::Debug for MemorySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemorySource").field("objects", &self.objects.len()).finish()
    }
}

/// A recovered history step: decoded from memory when Undo reaches it.
impl ArchivedDocument for MemorySource {
    fn load(&self) -> std::result::Result<Arc<Document>, String> {
        store::load(self, &LoadOptions::default()).map(Arc::new).map_err(|e| e.to_string())
    }
}

/// Loads the document of a stored [`ObjectCheckpoint`] from its descriptor and its objects (by
/// name), with its history (the steps decoded lazily from `objects`) and view context.
pub fn recover_object_checkpoint(descriptor: &[u8], objects: Arc<HashMap<String, Vec<u8>>>) -> Result<(Document, HistoryCheckpoint, serde_json::Value)> {
    if descriptor.len() as u64 > MAX_DESCRIPTOR {
        return Err(FormatError::LimitExceeded("recovery descriptor too large".into()));
    }
    let descriptor: Descriptor = serde_json::from_slice(descriptor)?;
    if !safe_key(&descriptor.info.key) {
        return Err(FormatError::corrupt("unsafe recovery key"));
    }
    if serde_json::to_vec(&descriptor.context)?.len() > 64 << 10 {
        return Err(FormatError::LimitExceeded("recovery context exceeds 64 KiB".into()));
    }
    let checkpoint = descriptor.checkpoint.ok_or_else(|| FormatError::corrupt("recovery descriptor has no checkpoint"))?;
    if checkpoint.version != 1 {
        return Err(FormatError::Unsupported("newer recovery checkpoint version".into()));
    }
    if checkpoint.undo.len().saturating_add(checkpoint.redo.len()) > 10_000 {
        return Err(FormatError::LimitExceeded("too many recovery history states".into()));
    }
    let doc = store::load(&MemorySource { manifest: checkpoint.current, objects: objects.clone() }, &LoadOptions::default())?;
    let restore = |states: Vec<StoredState>| -> Result<Vec<HistoryState>> {
        states
            .into_iter()
            .map(|state| {
                let source = MemorySource { manifest: state.manifest, objects: objects.clone() };
                // Validate the manifest now; its objects are read when Undo reaches the step.
                store::read_manifest(&source, &LoadOptions::default())?;
                let mut restored = HistoryState::from_archive(state.label, state.has_selection, Arc::new(source));
                restored.layers = state.layers.into();
                Ok(restored)
            })
            .collect()
    };
    let history = HistoryCheckpoint {
        undo: restore(checkpoint.undo)?,
        redo: restore(checkpoint.redo)?,
        current_label: checkpoint.current_label,
        current_layers: checkpoint.current_layers.into(),
        max_states: checkpoint.max_states,
        max_bytes: checkpoint.max_bytes,
    };
    History::from_checkpoint(history.clone()).map_err(FormatError::corrupt)?;
    Ok((doc, history, descriptor.context))
}

#[cfg(test)]
mod object_tests {
    use super::*;
    use photocraft_doc::{Color, ColorMode, Layer, SampleType, Size};

    fn info(key: &str, revision: u64) -> RecoveryInfo {
        RecoveryInfo { key: key.into(), document_name: "Page 1".into(), original_path: None, saved_at: 1_700_000_000, revision }
    }

    fn doc() -> Document {
        let mut doc = Document::new("Page 1", Size::new(300, 200), ColorMode::Rgb, SampleType::U8);
        let mut l = Layer::raster("ink", doc.pixel_format());
        l.surface_mut().unwrap().fill_rect(photocraft_geom::Rect::new(10, 10, 120, 90), &[0.2, 0.4, 0.8, 1.0]);
        doc.layers.push(l);
        doc.layers.push(Layer::raster("empty", doc.pixel_format()));
        let _ = Color::BLACK;
        doc
    }

    /// Store objects then descriptor, read them back: the same layers and pixels, and the
    /// second checkpoint after a small edit writes only the changed tile.
    #[test]
    fn object_checkpoints_round_trip_and_write_only_new_objects() {
        let mut saver = ObjectAutosaver::default();
        let mut d = doc();
        let first = saver.checkpoint(&d, info("web-a-1", 3), serde_json::json!({"active": true})).unwrap();
        assert_eq!(first.objects.len(), first.live.len(), "a first checkpoint writes every object");
        let mut store: HashMap<String, Vec<u8>> = first.objects.into_iter().collect();
        saver.committed(first.live);
        assert_eq!(object_checkpoint_info(&first.descriptor).unwrap(), info("web-a-1", 3));
        let (back, history, context) = recover_object_checkpoint(&first.descriptor, Arc::new(store.clone())).unwrap();
        assert_eq!(back.layers.len(), 2);
        assert_eq!(back.layers[0].surface().unwrap().pixel(20, 20), d.layers[0].surface().unwrap().pixel(20, 20));
        assert!(history.undo.is_empty());
        assert_eq!(context["active"], true);
        // A dab in one corner: one tile changes.
        d.layers[0].surface_mut().unwrap().fill_rect(photocraft_geom::Rect::new(0, 0, 4, 4), &[1.0, 0.0, 0.0, 1.0]);
        let second = saver.checkpoint(&d, info("web-a-1", 4), serde_json::Value::Null).unwrap();
        assert_eq!(second.objects.len(), 1, "{:?}", second.objects.iter().map(|o| &o.0).collect::<Vec<_>>());
        store.extend(second.objects);
        store.retain(|name, _| second.live.contains(name));
        let (back, _, _) = recover_object_checkpoint(&second.descriptor, Arc::new(store.clone())).unwrap();
        assert_eq!(back.layers[0].surface().unwrap().pixel(1, 1), d.layers[0].surface().unwrap().pixel(1, 1));
        // A missing object is an error, not a blank layer.
        store.clear();
        assert!(recover_object_checkpoint(&second.descriptor, Arc::new(store.clone())).is_err());
    }

    /// Undo history survives a checkpoint: the newest steps within the step and byte limits,
    /// each step restored lazily and undoable; a later checkpoint reuses steps encoded before.
    #[test]
    fn object_checkpoints_keep_the_newest_undo_steps_within_limits() {
        use photocraft_ops::{History, LayerTarget};
        let mut history = History::new(50);
        let mut page = Document::new("Page 1", Size::new(512, 512), ColorMode::Rgb, SampleType::U8);
        page.layers.push(Layer::raster("ink", page.pixel_format()));
        page.layers.push(Layer::raster("dabs", page.pixel_format()));
        let mut d = Arc::new(page);
        // 30 edits, each a dab in its own tile: every step owns one tile the others don't.
        for i in 0..30 {
            let before = d.clone();
            let mut next = (*d).clone();
            let x = (i % 8) * 64;
            let y = (i / 8) * 64;
            next.layers[1].surface_mut().unwrap().fill_rect(photocraft_geom::Rect::new(x, y, x + 8, y + 8), &[1.0, 0.0, 0.0, 1.0]);
            d = Arc::new(next);
            history.record(format!("Dab {i}"), before, LayerTarget::default());
        }
        let mut saver = ObjectAutosaver::default();
        let limits = HistoryLimits { max_states: 20, max_bytes: 150 << 20 };
        let cp = saver.checkpoint_with_history(&d, Some(&history.checkpoint()), limits, info("web-h-1", 30), serde_json::Value::Null).unwrap();
        let store: HashMap<String, Vec<u8>> = cp.objects.iter().cloned().collect();
        assert_eq!(store.len(), cp.live.len());
        let (back, h, _) = recover_object_checkpoint(&cp.descriptor, Arc::new(store)).unwrap();
        assert_eq!(h.undo.len(), 20, "the newest 20 steps");
        // A step is labelled with the edit that made it current: the newest is before Dab 29.
        assert_eq!(h.undo.last().unwrap().label, "Dab 28");
        assert_eq!(h.undo.first().unwrap().label, "Dab 9");
        // Undo works in the recovered document: each step brings back the pixels before it.
        let mut restored = History::from_checkpoint(h).unwrap();
        let mut cur = Arc::new(back);
        let (prev, _) = restored.try_undo(cur.clone()).unwrap().unwrap();
        let (x, y) = ((29 % 8) * 64, (29 / 8) * 64);
        assert_eq!(cur.layers[1].surface().unwrap().pixel(x + 1, y + 1)[0], 1.0);
        assert_eq!(prev.layers[1].surface().unwrap().pixel(x + 1, y + 1)[3], 0.0, "undone dab gone");
        cur = prev;
        for _ in 0..19 {
            cur = restored.try_undo(cur.clone()).unwrap().unwrap().0;
        }
        assert!(restored.try_undo(cur).unwrap().is_none(), "only the kept steps");
        // A byte cap keeps fewer steps: each step owns one RGBA8 storage tile.
        let tile = (photocraft_geom::TILE_SIZE * photocraft_geom::TILE_SIZE * 4) as u64;
        let small = HistoryLimits { max_states: 20, max_bytes: 5 * tile + tile / 2 };
        let mut s2 = ObjectAutosaver::default();
        let cp = s2.checkpoint_with_history(&d, Some(&history.checkpoint()), small, info("web-h-2", 30), serde_json::Value::Null).unwrap();
        let (_, h, _) = recover_object_checkpoint(&cp.descriptor, Arc::new(cp.objects.iter().cloned().collect())).unwrap();
        assert!((1..20).contains(&h.undo.len()), "{}", h.undo.len());
        // The next checkpoint after one more edit encodes only the new step and the new tile.
        saver.committed(cp_live(&d, &history, limits));
        let before = d.clone();
        let mut next = (*d).clone();
        next.layers[0].surface_mut().unwrap().fill_rect(photocraft_geom::Rect::new(250, 150, 260, 160), &[0.0, 1.0, 0.0, 1.0]);
        d = Arc::new(next);
        history.record("Dab 30", before, LayerTarget::default());
        let cp = saver.checkpoint_with_history(&d, Some(&history.checkpoint()), limits, info("web-h-1", 31), serde_json::Value::Null).unwrap();
        assert!(cp.objects.len() <= 2, "{:?}", cp.objects.iter().map(|o| &o.0).collect::<Vec<_>>());
    }

    /// What a checkpoint of `d` with `history` names (objects are named by content, so any
    /// saver's names match; to mark them stored).
    fn cp_live(d: &Document, history: &photocraft_ops::History, limits: HistoryLimits) -> Vec<String> {
        let mut probe = ObjectAutosaver::default();
        probe.checkpoint_with_history(d, Some(&history.checkpoint()), limits, info("web-h-9", 1), serde_json::Value::Null).unwrap().live
    }

    #[test]
    fn unsafe_keys_are_refused() {
        let mut saver = ObjectAutosaver::default();
        assert!(saver.checkpoint(&doc(), info("../x", 1), serde_json::Value::Null).is_err());
    }
}
