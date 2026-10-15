//! The seam between the engine and whatever runs LaMa: the shell injects an [`Inpainter`]
//! (natively [`crate::native::NativeInpainter`]; in the browser a bridge to the AI Web Worker
//! module), and the engine's `edit.inpaintAi` command calls it once per tile from a background
//! job.
//!
//! Shaped like `photocraft_ai::service::Upscaler` (the shared AI runtime): [`Inpainter::run`]
//! reports through a callback, possibly later and from another thread or a browser event, because
//! the browser can't block on WebGPU. Implementations call `done` exactly once (also on
//! cancellation, with [`LamaError::Cancelled`]). When the shared runtime lands, [`ModelState`]
//! and [`Hooks`] become its types and this trait moves next to `Upscaler`.

use std::sync::Arc;

use crate::tiles::TileInput;
use crate::{LamaError, Result};

/// Whether the model can run right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelState {
    /// Its weights are available: it runs without a download.
    Ready,
    /// It needs a download of `bytes` first (ask the user).
    NeedsDownload {
        /// Download size.
        bytes: u64,
    },
    /// Still finding out (the browser's cache answers asynchronously); ask again.
    Checking,
    /// It can't run in this build or on this machine, and why.
    Unavailable(String),
}

/// One tile to inpaint.
#[derive(Clone, Debug)]
pub struct Request {
    /// The model input (see [`crate::tiles::extract`]).
    pub tile: TileInput,
    /// The user agreed to download the weights if they aren't cached.
    pub allow_download: bool,
    /// The model's catalog id (`photocraft_ai::catalog`): [`LAMA`] or [`MIGAN`].
    pub model: &'static str,
    /// Skip the GPU.
    pub cpu: bool,
    /// CPU threads; 0: one per core.
    pub threads: usize,
    /// Free the model after the run instead of keeping it loaded.
    pub unload: bool,
}

/// Catalog id of Manga LaMa (as `photocraft_ai::catalog::LAMA_ID`).
pub const LAMA: &str = "manga-lama";
/// Catalog id of MI-GAN (as `photocraft_ai::catalog::MIGAN_ID`).
pub const MIGAN: &str = "migan-512";

impl Request {
    /// Manga LaMa on `tile`, automatic device.
    pub fn lama(tile: TileInput, allow_download: bool) -> Self {
        Self { tile, allow_download, model: LAMA, cpu: false, threads: 0, unload: false }
    }
}

/// Progress and cancellation from the caller's job.
pub trait Hooks: Send + Sync {
    /// Progress of this request `0..=1` and a short status ("Downloading Manga LaMa…").
    fn progress(&self, fraction: f32, status: &str);
    /// The caller gave up; finish with [`LamaError::Cancelled`] as soon as possible.
    fn cancelled(&self) -> bool;
}

/// Receives the raw generator output: interleaved RGB, `tile.w·tile.h·3`, 0..=1.
pub type Done = Box<dyn FnOnce(Result<Vec<f32>>) + Send>;

/// Runs LaMa (see the module docs).
pub trait Inpainter: Send + Sync {
    /// Whether a Manga LaMa request can run now.
    fn model_state(&self) -> ModelState;
    /// Whether a request for catalog model `model` ([`LAMA`], [`MIGAN`]) can run now.
    fn model_state_of(&self, model: &str) -> ModelState {
        if model == LAMA { self.model_state() } else { ModelState::Unavailable(format!("{model} isn't available here")) }
    }
    /// Where the model runs, for the UI ("WebGPU", "Vulkan", "CPU"); empty until known.
    fn device(&self) -> String {
        String::new()
    }
    /// Inpaints one tile and calls `done` exactly once.
    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: Done);
}

/// An [`Inpainter`] for tests: fills masked pixels with the mean colour of the unmasked ones
/// (or mid-grey), synchronously.
#[derive(Debug, Default)]
pub struct FakeInpainter {
    /// Fail every request with this message.
    pub fail: Option<String>,
    /// Report this state instead of `Ready`.
    pub state: Option<ModelState>,
    /// The last request's model id and CPU flag.
    pub last: std::sync::Mutex<Option<(&'static str, bool)>>,
}

impl Inpainter for FakeInpainter {
    fn model_state(&self) -> ModelState {
        self.state.clone().unwrap_or(ModelState::Ready)
    }

    fn model_state_of(&self, _model: &str) -> ModelState {
        self.model_state()
    }

    fn device(&self) -> String {
        "test".into()
    }

    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: Done) {
        *self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((req.model, req.cpu));
        if hooks.cancelled() {
            return done(Err(LamaError::Cancelled));
        }
        if let Some(e) = &self.fail {
            return done(Err(LamaError::Device(e.clone())));
        }
        if let ModelState::NeedsDownload { .. } = self.model_state_of(req.model)
            && !req.allow_download
        {
            return done(Err(LamaError::Unavailable("the model needs a download".into())));
        }
        let t = &req.tile;
        let (mut sum, mut n) = ([0f64; 3], 0usize);
        for (px, m) in t.rgb.as_chunks::<3>().0.iter().zip(&t.mask) {
            if *m <= 0.0 {
                for (s, v) in sum.iter_mut().zip(px) {
                    *s += f64::from(*v);
                }
                n += 1;
            }
        }
        let mean = sum.map(|s| if n > 0 { (s / n as f64) as f32 } else { 0.5 });
        let mut out = t.rgb.clone();
        for (px, m) in out.as_chunks_mut::<3>().0.iter_mut().zip(&t.mask) {
            if *m > 0.0 {
                *px = mean;
            }
        }
        hooks.progress(1.0, "");
        done(Ok(out));
    }
}
