//! The seam between the engine and whatever runs the models: the shell injects an
//! [`Upscaler`] (natively [`crate::native::NativeUpscaler`]; in the browser a bridge to the AI
//! Web Worker), and the engine's `image.upscaleAi` command calls it from a background job.
//!
//! [`Upscaler::run`] reports its result through a callback, possibly later and from another
//! thread or a browser event: the browser can't block for WebGPU. Implementations call `done`
//! exactly once (also on cancellation, with [`AiError::Cancelled`]).

use std::sync::Arc;

use crate::catalog::ModelInfo;
use crate::profile::DeviceProfile;
use crate::tile::TileOptions;
use crate::{AiError, Planar, Result};

/// How a request runs: the dialog's choices (Device) and Advanced settings; the defaults are
/// automatic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunOptions {
    /// Skip the GPU.
    pub cpu: bool,
    /// Tiling; `None`: the model's default ([`crate::tile::default_tiles`]).
    pub tiles: Option<TileOptions>,
    /// CPU threads; 0: one per core.
    pub threads: usize,
    /// Free the model after the run instead of keeping it loaded for the next one.
    pub unload: bool,
    /// Which GPU runtime (natively; ignored for CPU runs and in the browser).
    pub backend: GpuBackend,
}

/// The native GPU runtimes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GpuBackend {
    /// DirectML where it can run the model (Windows, ESRGAN, ONNX Runtime present), else wgpu.
    #[default]
    Auto,
    /// burn on wgpu (Vulkan, DX12, Metal).
    Wgpu,
    /// ONNX Runtime on DirectML (Windows; the ESRGAN models): an error where it can't run.
    DirectMl,
}

impl GpuBackend {
    /// The `device` param values that pick a GPU runtime: `directml`, `wgpu`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "directml" => Some(Self::DirectMl),
            "wgpu" => Some(Self::Wgpu),
            _ => None,
        }
    }
}

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
    pub opts: RunOptions,
}

/// Progress and cancellation from the caller's job.
pub trait Hooks: Send + Sync {
    /// Overall progress `0..=1` and a short status ("Downloading MangaJaNai 4x…").
    fn progress(&self, fraction: f32, status: &str);
    fn cancelled(&self) -> bool;
    /// An automatic request left the backend it was headed for and runs on another (DirectML
    /// to wgpu, or no usable GPU: the CPU). Called before the model runs there, once per request.
    fn fell_back(&self, _fallback: &Fallback) {}
}

/// Where an automatic request ended instead of where it was headed ([`Hooks::fell_back`]; as
/// `photocraft_ai_lama::service::Fallback`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fallback {
    /// What it left: `"DirectML"` or `"GPU"`.
    pub from: String,
    /// The device it runs on instead (as [`Upscaler::device`]: `"GPU, DiscreteGpu(0)"`, `"CPU"`).
    pub to: String,
    /// `to` is the CPU (burn's CPU backend, or a software GPU adapter).
    pub cpu: bool,
    /// Why (the error, or "software adapter …").
    pub why: String,
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
    /// What the shell knows about the machine, for the automatic model and device choice
    /// ([`crate::profile::choose`]).
    fn profile(&self) -> DeviceProfile {
        DeviceProfile::default()
    }
    /// Bytes of downloaded model weights on this device, if the shell can tell.
    fn storage_used(&self) -> Option<u64> {
        None
    }
    /// Deletes every downloaded model (they download again, with consent, when next used).
    fn clear_downloads(&self) -> Result<()> {
        Err(AiError::Cache("clearing downloaded models isn't supported here".into()))
    }
    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: Done);
}

/// An [`Upscaler`] for tests: nearest-neighbour scaling, models always ready (or, with
/// `download_needed`, until a request allows the download).
#[derive(Default)]
pub struct FakeUpscaler {
    pub download_needed: std::sync::atomic::AtomicBool,
    pub fail: Option<String>,
    /// Fail every request that may use the GPU (not `opts.cpu`) with [`AiError::Gpu`] and this
    /// message.
    pub gpu_fail: Option<String>,
    /// The last request's model id.
    pub last_model: std::sync::Mutex<Option<&'static str>>,
    /// What [`Upscaler::profile`] reports.
    pub profile: std::sync::Mutex<DeviceProfile>,
    /// The last request's options.
    pub last_opts: std::sync::Mutex<Option<RunOptions>>,
    /// Report this [`Hooks::fell_back`] for every request that may use the GPU (not `opts.cpu`).
    pub fallback: Option<Fallback>,
}

impl Upscaler for FakeUpscaler {
    fn model_state(&self, _: &ModelInfo) -> ModelState {
        if self.download_needed.load(std::sync::atomic::Ordering::Relaxed) { ModelState::NeedsDownload } else { ModelState::Ready }
    }

    fn device(&self) -> String {
        "test".into()
    }

    fn profile(&self) -> DeviceProfile {
        self.profile.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    fn storage_used(&self) -> Option<u64> {
        Some(if self.download_needed.load(std::sync::atomic::Ordering::Relaxed) { 0 } else { 1 })
    }

    fn clear_downloads(&self) -> Result<()> {
        self.download_needed.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: Done) {
        *self.last_opts.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(req.opts);
        *self.last_model.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(req.model.id);
        if hooks.cancelled() {
            return done(Err(AiError::Cancelled));
        }
        if let Some(e) = &self.fail {
            return done(Err(AiError::Inference(e.clone())));
        }
        if let Some(e) = self.gpu_fail.as_ref().filter(|_| !req.opts.cpu) {
            return done(Err(AiError::Gpu(e.clone())));
        }
        if let Some(f) = self.fallback.as_ref().filter(|_| !req.opts.cpu) {
            hooks.fell_back(f);
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
