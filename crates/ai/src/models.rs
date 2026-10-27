//! Model downloads, separate from running models: the AI Model Manager and the `ai.models.*`
//! commands manage which weight files are on this device, and the AI commands only use them.
//!
//! The shell injects a [`ModelStore`] (natively [`crate::native_store::NativeModelStore`] on the
//! config dir's cache; in the browser a store backed by the AI Web Worker, which fetches,
//! verifies and writes Cache Storage off the page's main thread). Downloads are a queue: one at a
//! time, in the order asked, each cancellable ([`Queue`] is that logic, shared by both stores).
//!
//! Weight files, not catalog entries, are what gets downloaded: two entries may share one file
//! (Real-ESRGAN Compact's ×2 and ×4), so [`files`] lists one entry per file and every call here
//! keys on [`ModelInfo::file`].

use std::collections::{HashMap, VecDeque};

use crate::Result;
use crate::catalog::{self, Arch, Content, MODELS, ModelInfo};

/// Progress messages from a download reach the page at most this often (≤ 10 per second), so a
/// fast connection's thousands of chunks never flood the main thread.
pub const PROGRESS_INTERVAL_MS: f64 = 100.0;

/// Where one model's weights stand on this device.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Not on this device.
    NotDownloaded,
    /// Waiting in the download queue.
    Queued,
    /// Downloading now: `done` of `total` bytes, at `bytes_per_sec` (0 until measured).
    Downloading { done: u64, total: u64, bytes_per_sec: f64 },
    /// Stopped part-way (paused, or the page closed): `done` bytes are kept and the download
    /// resumes from there.
    Paused { done: u64, total: u64 },
    /// On this device and verified.
    Downloaded,
    /// A file is there but it isn't the catalog's (an older release): download it again.
    UpdateAvailable,
    /// The last download failed, and why (it can be tried again).
    Failed(String),
    /// Still finding out (the browser's cache answers asynchronously).
    Checking,
    /// Can't be downloaded here, and why.
    Unavailable(String),
}

impl Status {
    /// A short stable id for automation (`ai.models.list`).
    pub fn id(&self) -> &'static str {
        match self {
            Self::NotDownloaded => "notDownloaded",
            Self::Queued => "queued",
            Self::Downloading { .. } => "downloading",
            Self::Paused { .. } => "paused",
            Self::Downloaded => "downloaded",
            Self::UpdateAvailable => "updateAvailable",
            Self::Failed(_) => "failed",
            Self::Checking => "checking",
            Self::Unavailable(_) => "unavailable",
        }
    }

    /// The weights are on this device (an outdated file still runs: it is verified on load).
    pub fn is_downloaded(&self) -> bool {
        matches!(self, Self::Downloaded)
    }

    /// Queued or downloading.
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Queued | Self::Downloading { .. })
    }

    /// Seconds left at the measured speed, if known.
    pub fn eta_secs(&self) -> Option<f64> {
        match self {
            Self::Downloading { done, total, bytes_per_sec } if *bytes_per_sec > 1.0 && total >= done => Some((total - done) as f64 / bytes_per_sec),
            _ => None,
        }
    }
}

/// Bytes of model weights on this device and the browser's quota (`navigator.storage.estimate`:
/// the whole origin's usage and quota), when known.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Storage {
    /// Bytes of downloaded model weights.
    pub models: Option<u64>,
    /// Bytes the site uses in total (browser) or `None`.
    pub used: Option<u64>,
    /// Bytes the site may use (browser) or `None`.
    pub quota: Option<u64>,
}

/// Downloads, deletes and reports model weights (see the module docs). All methods return at
/// once; downloads run in the background, one at a time.
pub trait ModelStore: Send + Sync {
    /// Whether this store can download at all (`Err`: why not, for the UI).
    fn available(&self) -> std::result::Result<(), String>;
    /// Where `model`'s weights stand.
    fn status(&self, model: &ModelInfo) -> Status;
    /// Adds `model`'s file to the download queue (nothing when it is queued or downloading).
    fn download(&self, model: &'static ModelInfo) -> Result<()>;
    /// Cancels `model`'s download (queued or running), or every download with `None`; what was
    /// downloaded so far is thrown away.
    fn cancel(&self, model: Option<&ModelInfo>);
    /// Pauses `model`'s download (queued or running), or every download with `None`, keeping
    /// what was downloaded: [`ModelStore::download`] resumes it. Stores that can't resume cancel.
    fn pause(&self, model: Option<&ModelInfo>) {
        self.cancel(model);
    }
    /// Deletes `model`'s file from this device (cancelling its download first).
    fn delete(&self, model: &ModelInfo) -> Result<()>;
    /// Storage used by models and the quota.
    fn storage(&self) -> Storage;
}

/// One catalog entry per weight file, in catalog order (the Manager's rows).
pub fn files() -> Vec<&'static ModelInfo> {
    let mut out: Vec<&'static ModelInfo> = Vec::new();
    for m in MODELS {
        if !out.iter().any(|o| o.file == m.file) {
            out.push(m);
        }
    }
    out
}

/// Every catalog entry using `model`'s file (Compact ×2 and ×4 share one).
pub fn sharing_file(model: &ModelInfo) -> impl Iterator<Item = &'static ModelInfo> + '_ {
    MODELS.iter().filter(move |m| m.file == model.file)
}

/// The name of `model`'s file as the Manager shows it: the entry's title, without the scale when
/// several entries share the file ("Real-ESRGAN Compact").
pub fn file_title(model: &ModelInfo) -> String {
    if sharing_file(model).count() > 1 {
        let t = model.title;
        let t = t.strip_suffix(" 4x").or_else(|| t.strip_suffix(" 2x")).unwrap_or(t);
        let scales: Vec<String> = sharing_file(model).map(|m| format!("{}x", m.scale)).collect();
        format!("{t} ({})", scales.join(", "))
    } else {
        model.title.to_string()
    }
}

/// What the Manager groups models by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Group {
    Cleaning,
    Upscaling,
    Generative,
}

/// Which group `model` belongs to.
pub fn group(model: &ModelInfo) -> Group {
    match model.content {
        Content::Inpaint => Group::Cleaning,
        Content::Generative => Group::Generative,
        _ => Group::Upscaling,
    }
}

/// The tools (command ids) that use `model`.
pub fn tools(model: &ModelInfo) -> &'static [&'static str] {
    match model.arch {
        Arch::Lama | Arch::Migan => &["edit.inpaintAi", "paint.remove"],
        Arch::Esrgan | Arch::Compact | Arch::Fdat => &["image.upscaleAi"],
        Arch::Generative => &["edit.generativeFill"],
    }
}

/// Where a model runs well.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Runs {
    /// GPU or CPU; best on a GPU (the full models: slow on a CPU).
    BestOnGpu,
    /// GPU or CPU, fine on both (the lite models).
    Both,
}

pub fn runs(model: &ModelInfo) -> Runs {
    if model.lite { Runs::Both } else { Runs::BestOnGpu }
}

/// The files worth downloading on a device: full models with a usable GPU, else lite ones (as
/// [`crate::profile::choose`] picks them automatically). Upscaling with the full models
/// downloads only the ×2 and ×4 IllustrationJaNai and the MangaJaNai heights `page_heights`
/// lists (empty: all of them).
pub fn recommended(profile: &crate::profile::DeviceProfile, page_heights: &[u32]) -> Vec<&'static ModelInfo> {
    let lite = !profile.gpu.usable() || profile.weak();
    files()
        .into_iter()
        .filter(|m| m.lite == lite)
        .filter(|m| match (m.height, page_heights.is_empty()) {
            (Some(h), false) => page_heights.iter().any(|p| catalog_nearest(*p) == Some(h)),
            _ => true,
        })
        .collect()
}

/// The MangaJaNai training height nearest a page height.
fn catalog_nearest(page: u32) -> Option<u32> {
    MODELS.iter().filter_map(|m| m.height).min_by_key(|h| h.abs_diff(page))
}

/// A catalog entry by id or by file name.
pub fn by_id_or_file(key: &str) -> Option<&'static ModelInfo> {
    catalog::model(key).or_else(|| MODELS.iter().find(|m| m.file == key))
}

/// Limits progress messages to one per [`PROGRESS_INTERVAL_MS`] (the first always goes).
#[derive(Clone, Copy, Debug, Default)]
pub struct Throttle {
    last: Option<f64>,
}

impl Throttle {
    /// Should a message go at `now_ms`? Records it when it should.
    pub fn ready(&mut self, now_ms: f64) -> bool {
        match self.last {
            Some(t) if now_ms - t < PROGRESS_INTERVAL_MS && now_ms >= t => false,
            _ => {
                self.last = Some(now_ms);
                true
            }
        }
    }
}

/// The download in progress.
#[derive(Clone, Debug)]
struct Active {
    file: &'static str,
    done: u64,
    total: u64,
    /// Speed estimate (bytes per second, smoothed).
    speed: f64,
    /// Last sample: time (ms) and bytes.
    sample: Option<(f64, u64)>,
}

/// What [`Queue::cancel`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cancelled {
    /// Nothing matched.
    Nothing,
    /// Only queued entries went.
    Queued,
    /// The running download must be stopped too (the store aborts it).
    Active,
}

/// The download queue: one file at a time, in the order asked. Pure bookkeeping (no I/O, time is
/// passed in) so both stores share it and it is tested here.
#[derive(Clone, Debug, Default)]
pub struct Queue {
    waiting: VecDeque<&'static str>,
    active: Option<Active>,
    failed: HashMap<&'static str, String>,
    /// Paused (or interrupted) downloads: bytes kept.
    paused: HashMap<&'static str, u64>,
}

impl Queue {
    /// Queues `model`'s file; false when it is already queued or downloading.
    pub fn enqueue(&mut self, model: &'static ModelInfo) -> bool {
        let f = model.file;
        if self.active.as_ref().is_some_and(|a| a.file == f) || self.waiting.contains(&f) {
            return false;
        }
        self.failed.remove(f);
        self.waiting.push_back(f);
        true
    }

    /// Pauses `model`'s download, or all of them, remembering the bytes done; like
    /// [`Queue::cancel`] says whether the running one must be stopped.
    pub fn pause(&mut self, model: Option<&ModelInfo>) -> Cancelled {
        let mut what = Cancelled::Nothing;
        let keep: Vec<&'static str> = self.waiting.iter().copied().filter(|f| model.is_some_and(|m| m.file != *f)).collect();
        for f in self.waiting.iter().copied().filter(|f| !keep.contains(f)).collect::<Vec<_>>() {
            self.paused.entry(f).or_insert(0);
            what = Cancelled::Queued;
        }
        self.waiting.retain(|f| keep.contains(f));
        if let Some(a) = self.active.take_if(|a| model.is_none_or(|m| m.file == a.file)) {
            self.paused.insert(a.file, a.done);
            what = Cancelled::Active;
        }
        what
    }

    /// Records `bytes` already on this device for `file` (an interrupted download found at start).
    pub fn mark_partial(&mut self, file: &'static str, bytes: u64) {
        if self.active.as_ref().is_none_or(|a| a.file != file) && !self.waiting.contains(&file) {
            self.paused.insert(file, bytes);
        }
    }

    /// Forgets a pause (the partial bytes were deleted).
    pub fn clear_pause(&mut self, file: &str) {
        self.paused.remove(file);
    }

    /// Cancels `model`'s download, or all of them.
    pub fn cancel(&mut self, model: Option<&ModelInfo>) -> Cancelled {
        let mut what = Cancelled::Nothing;
        let before = self.waiting.len();
        match model {
            Some(m) => {
                self.waiting.retain(|f| *f != m.file);
                self.paused.remove(m.file);
            }
            None => {
                self.waiting.clear();
                self.paused.clear();
            }
        }
        if self.waiting.len() != before {
            what = Cancelled::Queued;
        }
        if self.active.as_ref().is_some_and(|a| model.is_none_or(|m| m.file == a.file)) {
            self.active = None;
            what = Cancelled::Active;
        }
        what
    }

    /// Starts the next download when none runs: its file.
    pub fn start_next(&mut self) -> Option<&'static str> {
        if self.active.is_some() {
            return None;
        }
        let file = self.waiting.pop_front()?;
        let total = MODELS.iter().find(|m| m.file == file).map_or(0, |m| m.bytes);
        let done = self.paused.remove(file).unwrap_or(0);
        self.active = Some(Active { file, done, total, speed: 0.0, sample: None });
        Some(file)
    }

    /// The file downloading now.
    pub fn active(&self) -> Option<&'static str> {
        self.active.as_ref().map(|a| a.file)
    }

    /// Bytes `done` of `total` of `file` at `now_ms`; ignored unless `file` is the active one.
    pub fn progress(&mut self, file: &str, done: u64, total: u64, now_ms: f64) {
        let Some(a) = self.active.as_mut().filter(|a| a.file == file) else { return };
        a.done = done.min(total.max(a.total));
        if total > 0 {
            a.total = total;
        }
        match a.sample {
            None => a.sample = Some((now_ms, done)),
            Some((t, d)) if now_ms - t >= 250.0 && done >= d => {
                let inst = (done - d) as f64 * 1000.0 / (now_ms - t);
                a.speed = if a.speed <= 0.0 { inst } else { a.speed * 0.7 + inst * 0.3 };
                a.sample = Some((now_ms, done));
            }
            Some(_) => {}
        }
    }

    /// The active download ended (`Err`: why; not called for cancellations).
    pub fn finish(&mut self, file: &str, result: std::result::Result<(), String>) {
        if self.active.as_ref().is_some_and(|a| a.file == file) {
            self.active = None;
        }
        if let (Err(e), Some(m)) = (result, MODELS.iter().find(|m| m.file == file)) {
            self.failed.insert(m.file, e);
        }
    }

    /// Forgets a failure (the file was deleted or downloaded another way).
    pub fn clear_failure(&mut self, file: &str) {
        self.failed.remove(file);
    }

    /// The queue's view of `model`: queued, downloading or failed; `None` when the queue knows
    /// nothing (the store then says whether the file is there).
    pub fn status(&self, model: &ModelInfo) -> Option<Status> {
        let f = model.file;
        if let Some(a) = self.active.as_ref().filter(|a| a.file == f) {
            return Some(Status::Downloading { done: a.done, total: a.total, bytes_per_sec: a.speed });
        }
        if self.waiting.contains(&f) {
            return Some(Status::Queued);
        }
        if let Some(e) = self.failed.get(f) {
            return Some(Status::Failed(e.clone()));
        }
        self.paused.get(f).map(|d| Status::Paused { done: *d, total: model.bytes })
    }

    /// Files waiting (not counting the active one).
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }

    /// Nothing queued or downloading.
    pub fn idle(&self) -> bool {
        self.active.is_none() && self.waiting.is_empty()
    }

    /// The whole queue's progress: bytes done and total over the active and waiting files.
    pub fn overall(&self) -> (u64, u64) {
        let size = |f: &str| MODELS.iter().find(|m| m.file == f).map_or(0, |m| m.bytes);
        let (mut done, mut total) = self.active.as_ref().map_or((0, 0), |a| (a.done, a.total));
        for f in &self.waiting {
            total = total.saturating_add(size(f));
        }
        done = done.min(total);
        (done, total)
    }
}

/// A [`ModelStore`] for tests: in memory, downloads finish when [`FakeStore::step`] runs (or at
/// once with `instant`).
#[derive(Default)]
pub struct FakeStore {
    pub inner: std::sync::Mutex<FakeInner>,
}

#[derive(Default)]
pub struct FakeInner {
    pub queue: Queue,
    pub downloaded: std::collections::HashSet<&'static str>,
    /// Finish downloads immediately.
    pub instant: bool,
    /// Fail every download with this message.
    pub fail: Option<String>,
    /// `available()` answer.
    pub unavailable: Option<String>,
}

impl FakeStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, FakeInner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Marks `model` downloaded.
    pub fn put(&self, model: &ModelInfo) {
        self.lock().downloaded.insert(model.file);
    }

    /// Finishes the running download (if any) and starts the next; true when something happened.
    pub fn step(&self) -> bool {
        let mut g = self.lock();
        let inner = &mut *g;
        let mut any = false;
        if let Some(f) = inner.queue.active() {
            match inner.fail.clone() {
                Some(e) => inner.queue.finish(f, Err(e)),
                None => {
                    inner.queue.finish(f, Ok(()));
                    inner.downloaded.insert(f);
                }
            }
            any = true;
        }
        any | inner.queue.start_next().is_some()
    }
}

impl ModelStore for FakeStore {
    fn available(&self) -> std::result::Result<(), String> {
        self.lock().unavailable.clone().map_or(Ok(()), Err)
    }

    fn status(&self, model: &ModelInfo) -> Status {
        let g = self.lock();
        if let Some(s) = g.queue.status(model) {
            return s;
        }
        if g.downloaded.contains(model.file) { Status::Downloaded } else { Status::NotDownloaded }
    }

    fn download(&self, model: &'static ModelInfo) -> Result<()> {
        if let Some(e) = self.lock().unavailable.clone() {
            return Err(crate::AiError::Download(e));
        }
        let instant = {
            let mut g = self.lock();
            if g.downloaded.contains(model.file) {
                return Ok(());
            }
            g.queue.enqueue(model);
            if g.queue.active().is_none() {
                g.queue.start_next();
            }
            g.instant
        };
        if instant {
            while self.step() {}
        }
        Ok(())
    }

    fn cancel(&self, model: Option<&ModelInfo>) {
        let mut g = self.lock();
        g.queue.cancel(model);
        g.queue.start_next();
    }

    fn pause(&self, model: Option<&ModelInfo>) {
        let mut g = self.lock();
        g.queue.pause(model);
        g.queue.start_next();
    }

    fn delete(&self, model: &ModelInfo) -> Result<()> {
        let mut g = self.lock();
        g.queue.cancel(Some(model));
        g.queue.clear_failure(model.file);
        g.downloaded.remove(model.file);
        g.queue.start_next();
        Ok(())
    }

    fn storage(&self) -> Storage {
        let g = self.lock();
        let models = files().iter().filter(|m| g.downloaded.contains(m.file)).map(|m| m.bytes).sum();
        Storage { models: Some(models), used: Some(models), quota: Some(1 << 30) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{COMPACT_2X_ID, COMPACT_4X_ID, LAMA_ID, MIGAN_ID, model};

    fn m(id: &str) -> &'static ModelInfo {
        model(id).unwrap()
    }

    #[test]
    fn pause_keeps_bytes_and_resume_starts_there() {
        let mut q = Queue::default();
        let lama = catalog::model(catalog::LAMA_ID).unwrap();
        let migan = catalog::model(catalog::MIGAN_ID).unwrap();
        q.enqueue(lama);
        q.enqueue(migan);
        q.start_next();
        q.progress(lama.file, 500, lama.bytes, 0.0);
        assert_eq!(q.pause(None), Cancelled::Active);
        assert_eq!(q.status(lama), Some(Status::Paused { done: 500, total: lama.bytes }));
        assert_eq!(q.status(migan), Some(Status::Paused { done: 0, total: migan.bytes }));
        assert!(q.idle());
        q.enqueue(lama);
        assert_eq!(q.start_next(), Some(lama.file));
        assert_eq!(q.status(lama), Some(Status::Downloading { done: 500, total: lama.bytes, bytes_per_sec: 0.0 }));
        q.cancel(Some(migan));
        assert_eq!(q.status(migan), None);
    }

    #[test]
    fn throttle_allows_at_most_ten_messages_a_second() {
        let mut t = Throttle::default();
        let sent = (0..1000).filter(|i| t.ready(*i as f64)).count(); // one per ms for 1 s
        assert_eq!(sent, 10);
        // A clock that goes backwards (another time origin) doesn't stall it.
        assert!(t.ready(5.0));
    }

    #[test]
    fn files_are_unique_and_shared_files_get_one_row() {
        let f = files();
        let compact: Vec<_> = f.iter().filter(|m| m.arch == Arch::Compact).collect();
        assert_eq!(compact.len(), 1);
        assert_eq!(file_title(m(COMPACT_2X_ID)), "Real-ESRGAN Compact (4x, 2x)");
        assert_eq!(file_title(m(LAMA_ID)), "Manga LaMa");
        assert_eq!(group(m(LAMA_ID)), Group::Cleaning);
        assert_eq!(group(m(COMPACT_4X_ID)), Group::Upscaling);
        assert_eq!(tools(m(MIGAN_ID)), &["edit.inpaintAi", "paint.remove"]);
        assert_eq!(runs(m(MIGAN_ID)), Runs::Both);
        assert_eq!(runs(m(LAMA_ID)), Runs::BestOnGpu);
        assert_eq!(by_id_or_file("manga-lama-f16.safetensors").map(|m| m.id), Some(LAMA_ID));
    }

    #[test]
    fn recommended_follows_the_device() {
        use crate::profile::{DeviceProfile, GpuClass};
        let gpu = DeviceProfile { gpu: GpuClass::Discrete, cores: Some(8), memory_gb: Some(8.0), ..Default::default() };
        let cpu = DeviceProfile { gpu: GpuClass::None, ..Default::default() };
        let r = recommended(&cpu, &[]);
        assert!(r.iter().all(|m| m.lite) && r.len() == 2, "{r:?}");
        let r = recommended(&gpu, &[]);
        assert!(r.iter().all(|m| !m.lite) && r.iter().any(|m| m.id == LAMA_ID));
        let r = recommended(&gpu, &[1500]);
        assert!(r.iter().filter(|m| m.height.is_some()).all(|m| m.height == Some(1500)));
    }

    #[test]
    fn the_queue_runs_one_at_a_time_in_order_and_cancels() {
        let mut q = Queue::default();
        assert!(q.enqueue(m(LAMA_ID)));
        assert!(q.enqueue(m(MIGAN_ID)));
        assert!(!q.enqueue(m(LAMA_ID)), "already queued");
        assert!(q.enqueue(m(COMPACT_4X_ID)));
        assert!(!q.enqueue(m(COMPACT_2X_ID)), "same file");
        assert_eq!(q.start_next(), Some(m(LAMA_ID).file));
        assert_eq!(q.start_next(), None, "one at a time");
        q.progress(m(LAMA_ID).file, 1000, 0, 0.0);
        q.progress(m(LAMA_ID).file, 3_001_000, 0, 1000.0);
        let s = q.status(m(LAMA_ID)).unwrap();
        let Status::Downloading { done, total, bytes_per_sec } = s.clone() else { panic!("{s:?}") };
        assert_eq!((done, total), (3_001_000, m(LAMA_ID).bytes));
        assert!((bytes_per_sec - 3e6).abs() < 1.0, "{bytes_per_sec}");
        assert!(s.eta_secs().unwrap() > 30.0);
        assert_eq!(q.status(m(MIGAN_ID)), Some(Status::Queued));
        assert_eq!(q.cancel(Some(m(MIGAN_ID))), Cancelled::Queued);
        assert_eq!(q.status(m(MIGAN_ID)), None);
        q.finish(m(LAMA_ID).file, Err("HTTP 404".into()));
        assert_eq!(q.status(m(LAMA_ID)), Some(Status::Failed("HTTP 404".into())));
        assert_eq!(q.start_next(), Some(m(COMPACT_4X_ID).file));
        let (d, t) = q.overall();
        assert!(d <= t && t == m(COMPACT_4X_ID).bytes);
        assert_eq!(q.cancel(None), Cancelled::Active);
        assert!(q.idle());
        // Retrying a failed file clears the failure.
        assert!(q.enqueue(m(LAMA_ID)));
        assert_eq!(q.status(m(LAMA_ID)), Some(Status::Queued));
        // Progress for another file is ignored.
        q.start_next();
        q.progress("nope", 5, 5, 0.0);
        assert!(matches!(q.status(m(LAMA_ID)), Some(Status::Downloading { done: 0, .. })));
    }

    #[test]
    fn the_fake_store_downloads_in_steps() {
        let s = FakeStore::default();
        s.download(m(LAMA_ID)).unwrap();
        s.download(m(MIGAN_ID)).unwrap();
        assert!(matches!(s.status(m(LAMA_ID)), Status::Downloading { .. }));
        assert_eq!(s.status(m(MIGAN_ID)), Status::Queued);
        assert!(s.step());
        assert_eq!(s.status(m(LAMA_ID)), Status::Downloaded);
        assert!(matches!(s.status(m(MIGAN_ID)), Status::Downloading { .. }));
        s.cancel(None);
        assert_eq!(s.status(m(MIGAN_ID)), Status::NotDownloaded);
        s.delete(m(LAMA_ID)).unwrap();
        assert_eq!(s.status(m(LAMA_ID)), Status::NotDownloaded);
        assert_eq!(s.storage().models, Some(0));
    }
}
