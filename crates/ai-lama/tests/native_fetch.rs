//! `allow_download` on the native inpainter: missing weights are fetched before the model loads.
#![cfg(feature = "flex")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use photocraft_ai_lama::LamaError;
use photocraft_ai_lama::native::{NativeInpainter, WEIGHTS_FILE};
use photocraft_ai_lama::service::{GpuBackend, Hooks, Inpainter, LAMA, Request};
use photocraft_ai_lama::tiles::TileInput;

#[derive(Default)]
struct NoHooks {
    cancel: bool,
}

impl Hooks for NoHooks {
    fn progress(&self, _: f32, _: &str) {}
    fn cancelled(&self) -> bool {
        self.cancel
    }
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lama-fetch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(i: &NativeInpainter, allow_download: bool, hooks: NoHooks) -> Result<(), LamaError> {
    let tile = TileInput { rgb: vec![0.0; 3 * 8 * 8], mask: vec![0.0; 8 * 8], w: 8, h: 8 };
    let req = Request { tile, allow_download, model: LAMA, cpu: true, threads: 1, unload: true, backend: GpuBackend::Auto };
    let out = Arc::new(Mutex::new(None));
    let sink = out.clone();
    i.run(req, Arc::new(hooks), Box::new(move |r| *sink.lock().unwrap() = Some(r.map(|_| ()))));
    out.lock().unwrap().take().unwrap()
}

/// Before the fix a request with `allow_download` failed with "weights not found": nothing
/// fetched them. Now the fetcher runs, and the file it left is what the model loads (these bytes
/// aren't weights: the error is about them, not about a missing file).
#[test]
fn allow_download_fetches_missing_weights() {
    let dir = folder("fetches");
    let calls = Arc::new(AtomicUsize::new(0));
    let (count, target) = (calls.clone(), dir.join(WEIGHTS_FILE));
    let i = NativeInpainter::new(dir.join(WEIGHTS_FILE)).with_downloads(dir.clone()).with_fetch(Arc::new(move |id, _, _| {
        assert_eq!(id, LAMA);
        count.fetch_add(1, Ordering::SeqCst);
        std::fs::write(&target, b"not weights").map_err(|e| e.to_string())
    }));
    let e = run(&i, true, NoHooks::default()).unwrap_err().to_string();
    assert!(!e.contains("not found"), "{e}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // Here now: no second fetch.
    let _ = run(&i, true, NoHooks::default());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn no_fetch_without_allow_download() {
    let dir = folder("asked");
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let i = NativeInpainter::new(dir.join(WEIGHTS_FILE)).with_downloads(dir.clone()).with_fetch(Arc::new(move |_, _, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));
    assert!(run(&i, false, NoHooks::default()).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let _ = std::fs::remove_dir_all(dir);
}

/// A failed fetch reaches the caller with its reason; a cancelled one is a cancel.
#[test]
fn a_failed_fetch_says_why() {
    let dir = folder("fails");
    let i = NativeInpainter::new(dir.join(WEIGHTS_FILE)).with_downloads(dir.clone()).with_fetch(Arc::new(|_, _, _| Err("the server said no".into())));
    let e = run(&i, true, NoHooks::default()).unwrap_err().to_string();
    assert!(e.contains("the server said no"), "{e}");
    // Cancelled before it started: `run` stops at once.
    assert!(matches!(run(&i, true, NoHooks { cancel: true }), Err(LamaError::Cancelled)));
    let _ = std::fs::remove_dir_all(dir);
}
