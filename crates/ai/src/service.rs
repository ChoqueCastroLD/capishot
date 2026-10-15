//! The seam between the engine and whatever runs the models: the shell injects an
//! [`Upscaler`] (natively [`crate::native::NativeUpscaler`]; in the browser a bridge to the AI
//! Web Worker), and the engine's `image.upscaleAi` command calls it from a background job.
//!
//! [`Upscaler::run`] reports its result through a callback, possibly later and from another
//! thread or a browser event: the browser can't block for WebGPU. Implementations call `done`
//! exactly once (also on cancellation, with [`AiError::Cancelled`]).

use std::sync::Arc;

use crate::catalog::ModelInfo;
use crate::{AiError, Planar, Result};

/// Whether a model can run right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelState {
    /// Its weights are cached: it runs without a download.
    Ready,
    /// It needs a download of `ModelInfo::bytes` first (ask the user).
    NeedsDownload,
    /// Still finding out (the browser's cache answers asynchronously); ask again.
    Checking,
    /// It can't run in this build or on this machine, and why.
    Unavailable(String),
}

/// One upscale.
#[derive(Clone, Debug)]
pub struct Request {
    pub model: &'static ModelInfo,
    /// Gray (1 channel) or RGB (3), `0..=1`, already preprocessed (see [`crate::mangajanai`]).
    pub input: Planar,
    /// Run the model in its one-channel form (MangaJaNai on grayscale pages).
    pub grayscale: bool,
    /// The user agreed to download the weights if they aren't cached.
    pub allow_download: bool,
}

/// Progress and cancellation from the caller's job.
pub trait Hooks: Send + Sync {
    /// Overall progress `0..=1` and a short status ("Downloading MangaJaNai 4x…").
    fn progress(&self, fraction: f32, status: &str);
    fn cancelled(&self) -> bool;
}

/// Receives the result: the upscaled image (1 channel for grayscale requests, else 3).
pub type Done = Box<dyn FnOnce(Result<Planar>) + Send>;

/// Runs catalog models (see the module docs).
pub trait Upscaler: Send + Sync {
    fn model_state(&self, model: &ModelInfo) -> ModelState;
    /// Where models run, for the UI ("WebGPU", "Vulkan", "CPU"); empty until known.
    fn device(&self) -> String {
        String::new()
    }
    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: Done);
}

/// An [`Upscaler`] for tests: nearest-neighbour scaling, models always ready (or, with
/// `download_needed`, until a request allows the download).
#[derive(Default)]
pub struct FakeUpscaler {
    pub download_needed: std::sync::atomic::AtomicBool,
    pub fail: Option<String>,
}

impl Upscaler for FakeUpscaler {
    fn model_state(&self, _: &ModelInfo) -> ModelState {
        if self.download_needed.load(std::sync::atomic::Ordering::Relaxed) { ModelState::NeedsDownload } else { ModelState::Ready }
    }

    fn device(&self) -> String {
        "test".into()
    }

    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: Done) {
        if hooks.cancelled() {
            return done(Err(AiError::Cancelled));
        }
        if let Some(e) = &self.fail {
            return done(Err(AiError::Inference(e.clone())));
        }
        if self.model_state(req.model) == ModelState::NeedsDownload {
            if !req.allow_download {
                return done(Err(AiError::Download("not allowed".into())));
            }
            self.download_needed.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        let s = req.model.scale as usize;
        let i = &req.input;
        let channels = if req.grayscale { 1 } else { i.channels };
        let src = if req.grayscale { crate::mangajanai::to_gray(i) } else { i.clone() };
        let (w, h) = (i.width * s, i.height * s);
        let mut data = Vec::with_capacity(w * h * channels);
        for c in 0..channels {
            let p = src.plane(c);
            for y in 0..h {
                for x in 0..w {
                    data.push(p.get((y / s) * i.width + x / s).copied().unwrap_or(0.0));
                }
            }
        }
        hooks.progress(1.0, "");
        done(Planar::new(w, h, channels, data));
    }
}
