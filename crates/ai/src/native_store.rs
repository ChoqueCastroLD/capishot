//! The desktop and CLI [`ModelStore`]: the models folder's [`DiskCache`] ([`crate::cache::models_dir`]),
//! downloads on one background thread, one file at a time ([`Queue`]), streamed to a temporary
//! file and hashed while they arrive (no 100 MB buffer), then renamed into place. A
//! `<file>.sha256` sidecar records what was stored, so a file from an older catalog shows as
//! [`Status::UpdateAvailable`] without hashing it again.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use crate::cache::DiskCache;
use crate::catalog::ModelInfo;
use crate::models::{Cancelled, ModelStore, Queue, Status, Storage, files};
use crate::{AiError, Result};

/// See the module docs.
pub struct NativeModelStore {
    shared: Arc<Shared>,
}

struct Shared {
    cache: DiskCache,
    base_url: Option<String>,
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    queue: Queue,
    /// Set to stop the running download.
    abort: Option<Arc<AtomicBool>>,
    worker: bool,
    /// The running download is being paused (its bytes stay), not cancelled.
    pausing: bool,
    /// Partial files found on disk were reported once.
    scanned: bool,
}

impl NativeModelStore {
    /// `base_url`: where weight files are downloaded from (`None`: downloads are unavailable).
    pub fn new(cache: DiskCache, base_url: Option<String>) -> Self {
        Self { shared: Arc::new(Shared { cache, base_url, state: Mutex::new(State::default()), wake: Condvar::new() }) }
    }

    /// The shells' default: the cache in `models_dir`, downloads from
    /// [`crate::cache::models_url_from_env`] (as [`crate::native::NativeUpscaler::from_env`]).
    pub fn from_env(models_dir: &Path) -> Self {
        Self::new(DiskCache::in_dir(models_dir), Some(crate::cache::models_url_from_env()))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts the download thread if it isn't running.
    fn ensure_worker(&self, st: &mut State) -> Result<()> {
        if st.worker {
            self.shared.wake.notify_all();
            return Ok(());
        }
        let shared = self.shared.clone();
        std::thread::Builder::new()
            .name("photocraft-ai-downloads".into())
            .spawn(move || worker(&shared))
            .map_err(|e| AiError::Download(format!("could not start the download thread: {e}")))?;
        st.worker = true;
        Ok(())
    }
}

/// An unfinished download's bytes.
fn part_path(dir: &Path, model: &ModelInfo) -> PathBuf {
    dir.join(format!(".{}.part", model.file))
}

fn sidecar(dir: &Path, model: &ModelInfo) -> PathBuf {
    dir.join(format!("{}.sha256", model.file))
}

/// The download thread: takes the next file, downloads it, repeats; exits when the queue is empty.
fn worker(shared: &Shared) {
    loop {
        let (file, abort) = {
            let mut st = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(file) = st.queue.start_next() else {
                st.worker = false;
                return;
            };
            let abort = Arc::new(AtomicBool::new(false));
            st.abort = Some(abort.clone());
            (file, abort)
        };
        let Some(model) = crate::catalog::MODELS.iter().find(|m| m.file == file) else { continue };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fetch_to_cache(shared, model, &abort)))
            .unwrap_or_else(|_| Err(AiError::Download("the download failed with an internal error".into())));
        let mut st = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.abort = None;
        st.pausing = false;
        match r {
            // Cancelled: the queue already dropped it.
            Err(AiError::Cancelled) => {}
            r => st.queue.finish(file, r.map_err(|e| e.to_string())),
        }
    }
}

/// Downloads `model` into the cache folder (streamed, hashed on the fly, renamed into place).
#[cfg(feature = "download")]
fn fetch_to_cache(shared: &Shared, model: &'static ModelInfo, abort: &AtomicBool) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    let base = shared.base_url.as_deref().ok_or_else(|| AiError::Download("model downloads are not configured in this build".into()))?;
    let url = model.url(base);
    let err = |e: &dyn std::fmt::Display| AiError::Download(format!("{url}: {e}"));
    let dir = &shared.cache.dir;
    std::fs::create_dir_all(dir).map_err(|e| AiError::Cache(format!("{}: {e}", dir.display())))?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(20)))
        .timeout_recv_body(Some(std::time::Duration::from_secs(60)))
        .user_agent("Photocraft-dev")
        .build()
        .into();
    let tmp = part_path(dir, model);
    // Resume: hash what is already there, then ask for the rest (HTTP Range).
    let mut hash = Sha256::new();
    let mut done = 0u64;
    if let Ok(mut f) = std::fs::File::open(&tmp) {
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf).map_err(|e| AiError::Cache(e.to_string()))?;
            if n == 0 {
                break;
            }
            hash.update(buf.get(..n).unwrap_or(&[]));
            done = done.saturating_add(n as u64);
        }
        if done > model.bytes {
            let _ = std::fs::remove_file(&tmp);
            hash = Sha256::new();
            done = 0;
        }
    }
    let req = agent.get(&url);
    let req = if done > 0 { req.header("Range", &format!("bytes={done}-")) } else { req };
    let mut resp = req.call().map_err(|e| err(&e))?;
    if done > 0 && resp.status().as_u16() != 206 {
        // The server ignored the range: start over.
        hash = Sha256::new();
        done = 0;
    }
    let mut reader = resp.body_mut().with_config().limit(model.bytes.saturating_add(1)).reader();
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(done > 0)
        .truncate(done == 0)
        .open(&tmp)
        .map_err(|e| AiError::Cache(format!("{}: {e}", tmp.display())))?;
    let cleanup = |e: AiError| {
        let _ = std::fs::remove_file(&tmp);
        e
    };
    let mut buf = vec![0u8; 1 << 16];
    let started = std::time::Instant::now();
    let mut throttle = crate::models::Throttle::default();
    loop {
        if abort.load(Ordering::Relaxed) {
            // Cancelled: the bytes go; paused: they stay for the resume.
            if shared.state.lock().unwrap_or_else(PoisonError::into_inner).pausing {
                let _ = out.flush();
                return Err(AiError::Cancelled);
            }
            return Err(cleanup(AiError::Cancelled));
        }
        // A network error keeps the bytes (the download resumes); a bad file doesn't.
        let n = reader.read(&mut buf).map_err(|e| err(&e))?;
        if n == 0 {
            break;
        }
        let chunk = buf.get(..n).unwrap_or(&[]);
        done = done.saturating_add(n as u64);
        if done > model.bytes {
            return Err(cleanup(err(&"larger than expected")));
        }
        hash.update(chunk);
        out.write_all(chunk).map_err(|e| cleanup(AiError::Cache(e.to_string())))?;
        let now = started.elapsed().as_secs_f64() * 1000.0;
        if throttle.ready(now) {
            shared.state.lock().unwrap_or_else(PoisonError::into_inner).queue.progress(model.file, done, model.bytes, now);
        }
    }
    out.flush().map_err(|e| cleanup(AiError::Cache(e.to_string())))?;
    drop(out);
    let actual: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if done != model.bytes || actual != model.sha256 {
        return Err(cleanup(AiError::Checksum { expected: model.sha256.into(), actual: format!("{actual} ({done} bytes)") }));
    }
    std::fs::rename(&tmp, dir.join(model.file)).map_err(|e| cleanup(AiError::Cache(e.to_string())))?;
    // Best effort: without it the file still counts as downloaded.
    let _ = std::fs::write(sidecar(dir, model), model.sha256);
    Ok(())
}

#[cfg(not(feature = "download"))]
fn fetch_to_cache(_: &Shared, model: &'static ModelInfo, _: &AtomicBool) -> Result<()> {
    Err(AiError::Download(format!("{}: this build can't download models", model.file)))
}

impl ModelStore for NativeModelStore {
    fn available(&self) -> std::result::Result<(), String> {
        if !cfg!(feature = "download") {
            return Err("this build can't download models".into());
        }
        if self.shared.base_url.is_none() {
            return Err("model downloads are not configured in this build (PHOTOCRAFT_AI_MODELS_URL)".into());
        }
        Ok(())
    }

    fn status(&self, model: &ModelInfo) -> Status {
        {
            let mut st = self.lock();
            if !st.scanned {
                st.scanned = true;
                for m in files() {
                    if let Ok(meta) = part_path(&self.shared.cache.dir, m).metadata() {
                        st.queue.mark_partial(m.file, meta.len().min(m.bytes));
                    }
                }
            }
            if let Some(s) = st.queue.status(model) {
                return s;
            }
        }
        let cache = &self.shared.cache;
        let path = cache.dir.join(model.file);
        match path.metadata() {
            Ok(meta) if meta.len() == model.bytes => match std::fs::read_to_string(sidecar(&cache.dir, model)) {
                Ok(sha) if sha.trim() != model.sha256 => Status::UpdateAvailable,
                _ => Status::Downloaded,
            },
            Ok(_) => Status::UpdateAvailable,
            // A developer's local folder (PHOTOCRAFT_AI_MODELS) counts too.
            Err(_) if cache.contains(model) => Status::Downloaded,
            Err(_) => match self.available() {
                Ok(()) => Status::NotDownloaded,
                Err(why) => Status::Unavailable(why),
            },
        }
    }

    fn download(&self, model: &'static ModelInfo) -> Result<()> {
        self.available().map_err(AiError::Download)?;
        let mut st = self.lock();
        st.queue.enqueue(model);
        self.ensure_worker(&mut st)
    }

    fn cancel(&self, model: Option<&ModelInfo>) {
        let mut st = self.lock();
        // Paused bytes go too.
        for m in files().into_iter().filter(|m| model.is_none_or(|x| x.file == m.file)) {
            if matches!(st.queue.status(m), Some(Status::Paused { .. })) {
                let _ = std::fs::remove_file(part_path(&self.shared.cache.dir, m));
            }
        }
        if st.queue.cancel(model) == Cancelled::Active
            && let Some(a) = st.abort.take()
        {
            a.store(true, Ordering::Relaxed);
        }
    }

    fn pause(&self, model: Option<&ModelInfo>) {
        let mut st = self.lock();
        if st.queue.pause(model) == Cancelled::Active
            && let Some(a) = st.abort.take()
        {
            st.pausing = true;
            a.store(true, Ordering::Relaxed);
        }
    }

    fn delete(&self, model: &ModelInfo) -> Result<()> {
        self.cancel(Some(model));
        let _ = std::fs::remove_file(part_path(&self.shared.cache.dir, model));
        self.lock().queue.clear_failure(model.file);
        let _ = std::fs::remove_file(sidecar(&self.shared.cache.dir, model));
        self.shared.cache.remove(model)
    }

    fn storage(&self) -> Storage {
        let dir = &self.shared.cache.dir;
        let models = files().iter().filter_map(|m| dir.join(m.file).metadata().ok()).map(|m| m.len()).sum();
        Storage { models: Some(models), used: None, quota: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{LAMA_ID, model};

    #[test]
    fn status_reads_the_cache_folder_and_its_sidecar() {
        let dir = std::env::temp_dir().join(format!("photocraft-ai-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let s = NativeModelStore::new(DiskCache { dir: dir.clone(), extra: vec![] }, Some("http://127.0.0.1:9/".into()));
        let m = model(LAMA_ID).unwrap();
        let expect_none = if cfg!(feature = "download") { Status::NotDownloaded } else { Status::Unavailable(String::new()) };
        assert_eq!(std::mem::discriminant(&s.status(m)), std::mem::discriminant(&expect_none));
        // A file of the right size: downloaded; with another release's checksum: an update.
        std::fs::File::create(dir.join(m.file)).unwrap().set_len(m.bytes).unwrap();
        assert_eq!(s.status(m), Status::Downloaded);
        std::fs::write(sidecar(&dir, m), "0".repeat(64)).unwrap();
        assert_eq!(s.status(m), Status::UpdateAvailable);
        assert_eq!(s.storage().models, Some(m.bytes));
        s.delete(m).unwrap();
        assert!(!dir.join(m.file).exists() && !sidecar(&dir, m).exists());
        assert_eq!(s.storage().models, Some(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_base_url_downloads_are_unavailable() {
        let dir = std::env::temp_dir().join(format!("photocraft-ai-store-none-{}", std::process::id()));
        let s = NativeModelStore::new(DiskCache { dir, extra: vec![] }, None);
        let m = model(LAMA_ID).unwrap();
        assert!(s.available().is_err());
        assert!(matches!(s.status(m), Status::Unavailable(_)));
        assert!(s.download(m).is_err());
        s.cancel(None);
    }

    #[cfg(feature = "download")]
    #[test]
    fn a_failed_download_reports_why_and_leaves_no_file() {
        let dir = std::env::temp_dir().join(format!("photocraft-ai-store-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Nothing listens on port 9 (discard): the connection is refused at once.
        let s = NativeModelStore::new(DiskCache { dir: dir.clone(), extra: vec![] }, Some("http://127.0.0.1:9/".into()));
        let m = model(crate::catalog::MIGAN_ID).unwrap();
        s.download(m).unwrap();
        let t = std::time::Instant::now();
        while s.status(m).is_pending() && t.elapsed() < std::time::Duration::from_secs(30) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(matches!(s.status(m), Status::Failed(_)), "{:?}", s.status(m));
        assert!(!dir.join(m.file).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
