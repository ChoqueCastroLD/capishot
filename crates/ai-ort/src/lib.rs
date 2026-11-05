//! ONNX Runtime allows one environment per process, and `ort` hands it only to whoever built it.
//! The upscalers (`photocraft-ai`) and Manga LaMa (`photocraft-ai-lama`) run in one process, so
//! they share the environment here: each building its own made the second one to run fail.
//!
//! ONNX Runtime is not linked: `onnxruntime.dll` (a DirectML build, 1.24 or later) is loaded at
//! run time from `PHOTOCRAFT_AI_ONNXRUNTIME` (the file, or the folder holding it) or from the
//! executable's folder, with `DirectML.dll` from the same folder. On Linux it is the official CPU
//! build's `libonnxruntime.so`, found the same way.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg(any(windows, target_os = "linux"))]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ort::environment::Environment;

/// ONNX Runtime's file name: a DirectML build on Windows, the official CPU build on Linux.
pub const LIBRARY: &str = if cfg!(windows) { "onnxruntime.dll" } else { "libonnxruntime.so" };

/// The ONNX Runtime library to load: `PHOTOCRAFT_AI_ONNXRUNTIME` (a file or a folder), else
/// [`LIBRARY`] next to the executable.
fn library() -> Option<PathBuf> {
    let file = |p: PathBuf| if p.is_dir() { p.join(LIBRARY) } else { p };
    if let Some(p) = std::env::var_os("PHOTOCRAFT_AI_ONNXRUNTIME").filter(|p| !p.is_empty()) {
        return Some(file(PathBuf::from(p)));
    }
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join(LIBRARY);
    p.is_file().then_some(p)
}

static ENV: OnceLock<std::result::Result<Environment, String>> = OnceLock::new();

/// The process's ONNX Runtime environment, loaded on first use, or why it can't be.
pub fn environment() -> std::result::Result<&'static Environment, String> {
    ENV.get_or_init(|| {
        let path = library().ok_or_else(|| format!("{LIBRARY} not found (next to the executable or in PHOTOCRAFT_AI_ONNXRUNTIME)"))?;
        load(&path)
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn load(path: &Path) -> std::result::Result<Environment, String> {
    if !path.is_file() {
        return Err(format!("{} doesn't exist", path.display()));
    }
    // Windows ships an older DirectML.dll in System32: the one next to ONNX Runtime goes first.
    let dml = path.with_file_name("DirectML.dll");
    if cfg!(windows) && dml.is_file() {
        ort::util::preload_dylib(&dml).map_err(|e| format!("{}: {e}", dml.display()))?;
    }
    // A broken or mismatched library must not take the app down.
    std::panic::catch_unwind(|| -> std::result::Result<Environment, String> {
        ort::init_from(path).map_err(|e| e.to_string())?.with_name("capishot").with_telemetry(false).build().map_err(|e| e.to_string())
    })
    .unwrap_or_else(|_| Err(format!("{} failed to initialise", path.display())))
}

/// Whether the library loads, or why not.
pub fn available() -> std::result::Result<(), String> {
    environment().map(|_| ())
}
