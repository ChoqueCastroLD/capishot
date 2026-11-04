//! AI in-process: Remove with AI (Manga LaMa), Clean Selected Area with AI, the Bubble Text
//! Remover, Upscale with AI (MangaJaNai / IllustrationJaNai) and Generative Fill, run by the same
//! commands the control protocol has (`edit.removeAi`, `edit.inpaintAi`, `edit.removeBubbleText`,
//! `image.upscaleAi`, `edit.generativeFill`, `ai.models.*`).
//!
//! # Setting it up
//!
//! [`install`] gives a session its AI services once, with the host's [`AiConfig`]: where models
//! are downloaded and cached, where they are downloaded from, and which device runs them.
//!
//! - **Native** (feature `ai-native`): burn on the GPU through wgpu (Vulkan, Metal, DX12), Manga
//!   LaMa and the upscalers on DirectML on Windows when ONNX Runtime's DirectML build
//!   (`onnxruntime.dll`, `DirectML.dll`) is next to the executable, and the CPU when there is no
//!   usable GPU or the GPU fails mid-run. Models live in [`AiConfig::models_dir`].
//! - **wasm** (feature `ai-web`): the AI Web Worker (`apps/photocraft-ai-worker`, built by
//!   `packaging/web-ai/build.sh` and served by the host next to its page) runs the models on
//!   WebGPU, or on the CPU (the worker's threads variant on a cross-origin isolated page). Models
//!   live in the browser's Cache Storage; [`AiConfig::worker_url`] says where the worker is.
//!
//! Enable the feature per target in the host's manifest:
//!
//! ```toml
//! [target.'cfg(not(target_arch = "wasm32"))'.dependencies]
//! capishot-host = { git = "…", rev = "…", features = ["ai-native"] }
//! [target.'cfg(target_arch = "wasm32")'.dependencies]
//! capishot-host = { git = "…", rev = "…", features = ["ai-web"] }
//! ```
//!
//! Without the feature for the target being built, [`install`] says AI isn't available and the AI
//! commands fail with an error saying so (never a panic).
//!
//! # Running
//!
//! Every AI command is a background job (natively on a worker thread; in the browser it waits
//! for the AI Web Worker between frames): start it as a job (progress, cancel, the job's end
//! event), with the params the typed builders here make ([`RemoveAi`], [`InpaintAi`],
//! [`RemoveBubbleText`], [`UpscaleAi`]). [`AiReply::from_value`] reads the result: where it ran
//! (`device`, `cpu`), whether it fell back (`fellBack`, `notices`: the GPU failed and the CPU
//! finished it, DirectML couldn't load it, no usable GPU) and how long it took.
//!
//! **One undo step.** Remove with AI (strokes or the selection), Clean Selected Area with AI and
//! the Bubble Text Remover each land as exactly one undo step on the document, whatever they
//! paint into (the layer's pixels, its layer mask, an alpha channel, the Quick Mask) and
//! whichever target the build runs on (native jobs, the browser's stepped jobs): one Ctrl+Z
//! takes the whole result back. A failed or cancelled run leaves no step.
//!
//! # Models
//!
//! Runs only use downloaded models unless their params allow the download (`allowDownload` /
//! `download`). [`model_status`], [`download_model`] and the `ai.models.*` commands manage them;
//! [`prepare_remove_ai`] starts Manga LaMa's download (about 100 MB) the first time, as the
//! editor's Remove with AI tool does.

use std::path::PathBuf;
use std::sync::Arc;

use photocraft_ai::catalog::{self, ModelInfo};
use photocraft_ai::models::Status;
use photocraft_engine::Session;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::LayerId;
use crate::error::{Error, Result, bad};

/// Remove with AI: what was painted, else the selection, erased and filled by Manga LaMa.
pub const REMOVE_AI: &str = photocraft_engine::remove_ai_cmds::ID;
/// Clean Selected Area with AI: the selection filled by Manga LaMa (or the lite MI-GAN).
pub const INPAINT_AI: &str = photocraft_engine::ai_inpaint_cmds::ID;
/// The Bubble Text Remover's click: the lettering in a plain speech bubble removed, without AI.
pub const REMOVE_BUBBLE_TEXT: &str = photocraft_engine::bubble_text_cmds::ID;
/// Upscale with AI: MangaJaNai for grayscale pages, IllustrationJaNai for colour.
pub const UPSCALE_AI: &str = photocraft_engine::ai_cmds::COMMAND;
/// Generative Fill: Stable Diffusion inpainting of the selection (params: the control protocol's).
pub const GENERATIVE_FILL: &str = "edit.generativeFill";

/// The download base URL Capishot's own builds use ([`AiConfig::models_url`] `None`).
pub const DEFAULT_MODELS_URL: &str = photocraft_ai::cache::DEFAULT_MODELS_URL;

/// Which device runs the models.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AiDevice {
    /// A real GPU when there is one (DirectML or wgpu natively, WebGPU in the browser), else the
    /// CPU; a GPU failure mid-run finishes on the CPU (the reply's `fellBack`).
    #[default]
    Auto,
    /// The GPU, even a software adapter (lavapipe, SwiftShader): for testing.
    Gpu,
    /// Always the CPU.
    Cpu,
}

/// What the host decides about AI ([`install`]). The same fields on every target; each target
/// reads its own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AiConfig {
    /// Native: the folder model files are downloaded to and loaded from (created if missing).
    /// `None`: no AI. Ignored in the browser (Cache Storage).
    pub models_dir: Option<PathBuf>,
    /// Where model files are downloaded from (a folder URL; files are verified by checksum).
    /// `None`: [`DEFAULT_MODELS_URL`]. `Some("")`: no downloads, only models already here run.
    pub models_url: Option<String>,
    /// Which device runs the models.
    pub device: AiDevice,
    /// wasm: the AI Web Worker's wasm-bindgen glue (`ai/photocraft_ai_worker-<hash>.js` from
    /// `packaging/web-ai/build.sh`), relative to the page or absolute. `None`: no AI in the
    /// browser. Ignored natively.
    pub worker_url: Option<String>,
    /// wasm: the worker's threads variant (`ai/<hash>/photocraft_ai_worker_mt.js`), used for CPU
    /// runs on a cross-origin isolated page. Ignored natively.
    pub worker_threads_url: Option<String>,
    /// Also install Generative Fill (Stable Diffusion; gigabytes of models, downloaded only when
    /// asked).
    pub generative_fill: bool,
}

/// What [`install`] set up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiAvailability {
    /// Remove with AI and Clean Selected Area with AI.
    pub inpaint: bool,
    /// Upscale with AI.
    pub upscale: bool,
    /// Generative Fill.
    pub generative_fill: bool,
    /// `ai.models.*`: model downloads and their status.
    pub models: bool,
    /// Why something is missing (for people), if anything is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl AiAvailability {
    fn none(reason: impl Into<String>) -> Self {
        Self { reason: Some(reason.into()), ..Self::default() }
    }
}

/// Gives `session` its AI services as `config` says, replacing any it had. Never fails: what
/// can't be set up is left out and [`AiAvailability::reason`] says why; the AI commands then fail
/// with an "unavailable" error. The Bubble Text Remover needs no AI and always works.
pub fn install(session: &mut Session, config: &AiConfig) -> AiAvailability {
    install_impl(session, config)
}

/// Removes the AI services (the models already loaded are freed when no job holds them).
pub fn uninstall(session: &mut Session) {
    session.set_inpainter(None);
    session.set_ai_upscaler(None);
    session.set_genfill(None);
    session.set_ai_models(None);
}

/// The download base `config` asks for (`None`: no downloads).
fn models_base(config: &AiConfig) -> Option<String> {
    match config.models_url.as_deref().map(str::trim) {
        None => Some(DEFAULT_MODELS_URL.to_string()),
        Some("") => None,
        Some(u) => Some(if u.ends_with('/') { u.to_string() } else { format!("{u}/") }),
    }
}

#[cfg(all(feature = "ai-native", not(target_arch = "wasm32")))]
fn install_impl(session: &mut Session, config: &AiConfig) -> AiAvailability {
    use photocraft_ai::cache::DiskCache;
    use photocraft_ai::native::{DeviceChoice, NativeUpscaler};
    use photocraft_ai::native_store::NativeModelStore;
    use photocraft_ai_lama::native::{MIGAN_FILE, NativeInpainter, WEIGHTS_FILE};

    let Some(dir) = config.models_dir.as_deref() else {
        return AiAvailability::none("no models folder set (AiConfig::models_dir)");
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        return AiAvailability::none(format!("the models folder {} can't be created: {e}", dir.display()));
    }
    let base = models_base(config);
    let choice = match config.device {
        AiDevice::Auto => DeviceChoice::Auto,
        AiDevice::Gpu => DeviceChoice::Gpu,
        AiDevice::Cpu => DeviceChoice::Cpu,
    };
    let cpu = config.device == AiDevice::Cpu;
    // The upscaler's device profile is what the engine picks the device of every AI run from
    // (`DeviceChoice::Cpu` reports no GPU), so a CPU-only host runs everything on the CPU.
    session.set_ai_upscaler(Some(Arc::new(NativeUpscaler::new(DiskCache::in_dir(dir), base.clone(), choice))));
    session.set_ai_models(Some(Arc::new(NativeModelStore::new(DiskCache::in_dir(dir), base))));
    let inpainter = NativeInpainter::new(dir.join(WEIGHTS_FILE)).with_migan(dir.join(MIGAN_FILE)).with_downloads(dir.to_path_buf());
    session.set_inpainter(Some(cpu_inpainter(Arc::new(inpainter), cpu)));
    if config.generative_fill {
        let genfill = photocraft_ai_sd::native::NativeGenFill::new(vec![dir.to_path_buf()], true);
        session.set_genfill(Some(cpu_genfill(Arc::new(genfill), cpu)));
    } else {
        session.set_genfill(None);
    }
    AiAvailability { inpaint: true, upscale: true, generative_fill: config.generative_fill, models: true, reason: None }
}

#[cfg(all(feature = "ai-web", target_arch = "wasm32"))]
fn install_impl(session: &mut Session, config: &AiConfig) -> AiAvailability {
    let Some(worker_url) = config.worker_url.clone().filter(|u| !u.trim().is_empty()) else {
        return AiAvailability::none("no AI worker set (AiConfig::worker_url)");
    };
    let web = photocraft_ai_web::WebAiConfig { worker_url, worker_threads_url: config.worker_threads_url.clone(), models_url: models_base(config) };
    let Some(ai) = photocraft_ai_web::from_config(&web) else {
        return AiAvailability::none(format!("the AI worker URL `{}` isn't a URL on this page", web.worker_url));
    };
    let cpu = config.device == AiDevice::Cpu;
    session.set_ai_upscaler(Some(cpu_upscaler(ai.upscaler, cpu)));
    session.set_inpainter(Some(cpu_inpainter(ai.inpainter, cpu)));
    session.set_genfill(config.generative_fill.then(|| cpu_genfill(ai.genfill, cpu)));
    session.set_ai_models(Some(ai.models));
    AiAvailability { inpaint: true, upscale: true, generative_fill: config.generative_fill, models: true, reason: None }
}

#[cfg(not(any(all(feature = "ai-native", not(target_arch = "wasm32")), all(feature = "ai-web", target_arch = "wasm32"))))]
fn install_impl(_session: &mut Session, _config: &AiConfig) -> AiAvailability {
    let feature = if cfg!(target_arch = "wasm32") { "ai-web" } else { "ai-native" };
    AiAvailability::none(format!("capishot-host was built without its `{feature}` feature"))
}

/// What is installed now (also what a host that injected its own services has).
pub fn availability(session: &Session) -> AiAvailability {
    let a = AiAvailability {
        inpaint: session.inpainter().is_some(),
        upscale: session.ai_upscaler().is_some(),
        generative_fill: session.genfill().is_some(),
        models: session.ai_models().is_some(),
        reason: None,
    };
    if a.inpaint || a.upscale || a.models { a } else { AiAvailability { reason: Some("AI isn't installed (capishot_host::ai::install)".into()), ..a } }
}

// ---------------------------------------------------------------------------------------------
// The CPU-only wrappers: every request skips the GPU.

/// `inner`, or with `cpu` every request on the CPU.
pub(crate) fn cpu_inpainter(inner: Arc<dyn photocraft_ai_lama::service::Inpainter>, cpu: bool) -> Arc<dyn photocraft_ai_lama::service::Inpainter> {
    if cpu { Arc::new(CpuOnly(inner)) } else { inner }
}

/// `inner`, or with `cpu` every request on the CPU.
pub(crate) fn cpu_genfill(inner: Arc<dyn photocraft_ai_sd::service::GenFill>, cpu: bool) -> Arc<dyn photocraft_ai_sd::service::GenFill> {
    if cpu { Arc::new(CpuOnly(inner)) } else { inner }
}

/// `inner`, or with `cpu` every request on the CPU and a device profile without a GPU (the
/// engine picks every AI run's device from it).
#[cfg_attr(not(all(feature = "ai-web", target_arch = "wasm32")), allow(dead_code))]
pub(crate) fn cpu_upscaler(inner: Arc<dyn photocraft_ai::service::Upscaler>, cpu: bool) -> Arc<dyn photocraft_ai::service::Upscaler> {
    if cpu { Arc::new(CpuOnly(inner)) } else { inner }
}

/// A service whose requests all skip the GPU.
struct CpuOnly<T: ?Sized>(Arc<T>);

impl photocraft_ai_lama::service::Inpainter for CpuOnly<dyn photocraft_ai_lama::service::Inpainter> {
    fn model_state(&self) -> photocraft_ai_lama::service::ModelState {
        self.0.model_state()
    }

    fn model_state_of(&self, model: &str) -> photocraft_ai_lama::service::ModelState {
        self.0.model_state_of(model)
    }

    fn device(&self) -> String {
        self.0.device()
    }

    fn run(&self, req: photocraft_ai_lama::service::Request, hooks: Arc<dyn photocraft_ai_lama::service::Hooks>, done: photocraft_ai_lama::service::Done) {
        self.0.run(photocraft_ai_lama::service::Request { cpu: true, ..req }, hooks, done);
    }
}

impl photocraft_ai_sd::service::GenFill for CpuOnly<dyn photocraft_ai_sd::service::GenFill> {
    fn model_state(&self, tier: photocraft_ai_sd::service::Tier) -> photocraft_ai_sd::service::ModelState {
        self.0.model_state(tier)
    }

    fn device(&self) -> String {
        self.0.device()
    }

    fn run(&self, req: photocraft_ai_sd::service::Request, hooks: Arc<dyn photocraft_ai_sd::service::Hooks>, done: photocraft_ai_sd::service::Done) {
        self.0.run(photocraft_ai_sd::service::Request { cpu: true, ..req }, hooks, done);
    }
}

impl photocraft_ai::service::Upscaler for CpuOnly<dyn photocraft_ai::service::Upscaler> {
    fn model_state(&self, model: &ModelInfo) -> photocraft_ai::service::ModelState {
        self.0.model_state(model)
    }

    fn device(&self) -> String {
        self.0.device()
    }

    fn profile(&self) -> photocraft_ai::profile::DeviceProfile {
        photocraft_ai::profile::DeviceProfile { gpu: photocraft_ai::profile::GpuClass::None, gpu_name: String::new(), ..self.0.profile() }
    }

    fn storage_used(&self) -> Option<u64> {
        self.0.storage_used()
    }

    fn clear_downloads(&self) -> photocraft_ai::Result<()> {
        self.0.clear_downloads()
    }

    fn run(&self, mut req: photocraft_ai::service::Request, hooks: Arc<dyn photocraft_ai::service::Hooks>, done: photocraft_ai::service::Done) {
        req.opts.cpu = true;
        self.0.run(req, hooks, done);
    }
}

// ---------------------------------------------------------------------------------------------
// Models

/// A model by catalog id (`manga-lama`, `migan-512`, the upscalers' ids: `ai.models.list`).
pub fn model(id: &str) -> Option<&'static ModelInfo> {
    photocraft_ai::models::by_id_or_file(id)
}

/// Manga LaMa, the model Remove with AI always runs.
pub fn lama() -> Option<&'static ModelInfo> {
    catalog::model(catalog::LAMA_ID)
}

/// A model's state on this device (`None`: no model service installed).
pub fn model_status(session: &Session, model: &ModelInfo) -> Option<Status> {
    Some(session.ai_models()?.status(model))
}

/// Queues a model's download (it resumes a paused one). Progress: [`model_status`] (or
/// `ai.models.status`), polled.
pub fn download_model(session: &Session, model: &'static ModelInfo) -> Result<()> {
    let store = session.ai_models().ok_or_else(|| Error::Engine(photocraft_engine::ai_models_cmds::UNAVAILABLE.into()))?;
    store.download(model).map_err(|e| Error::Engine(e.to_string()))
}

/// Where Remove with AI stands before a run.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Readiness {
    /// The model is here: run it.
    Ready,
    /// The model is downloading (this call started or resumed it when needed): run when ready, or
    /// run now with `allowDownload` and the run waits for it.
    Downloading {
        /// Bytes done.
        done: u64,
        /// Bytes in all (0: not known yet).
        total: u64,
    },
    /// It can't run here (no model service, no downloads and no model, a failed download…).
    Unavailable {
        /// Why (for people).
        reason: String,
    },
}

/// Gets Remove with AI ready: Manga LaMa's download starts (or resumes) the first time, as the
/// editor's tool does before its first stroke.
pub fn prepare_remove_ai(session: &Session) -> Readiness {
    let Some(m) = lama() else { return Readiness::Unavailable { reason: "Manga LaMa isn't in the model catalog".into() } };
    if session.inpainter().is_none() {
        return Readiness::Unavailable { reason: photocraft_engine::ai_inpaint_cmds::UNAVAILABLE.into() };
    }
    // Without a model service the inpainter finds its weights itself (a host-injected one).
    let Some(store) = session.ai_models() else { return Readiness::Ready };
    match store.status(m) {
        Status::Downloaded => Readiness::Ready,
        Status::Unavailable(reason) => Readiness::Unavailable { reason },
        Status::Downloading { done, total, .. } => Readiness::Downloading { done, total },
        Status::Paused { done, total } => match store.download(m) {
            Ok(()) => Readiness::Downloading { done, total },
            Err(e) => Readiness::Unavailable { reason: e.to_string() },
        },
        Status::Queued | Status::Checking => Readiness::Downloading { done: 0, total: m.bytes },
        Status::NotDownloaded | Status::Failed(_) | Status::UpdateAvailable => match store.download(m) {
            Ok(()) => Readiness::Downloading { done: 0, total: m.bytes },
            Err(e) => Readiness::Unavailable { reason: e.to_string() },
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Typed params

/// What a pixel command paints into (the control protocol's `target`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PaintTarget {
    /// The layer's pixels.
    #[default]
    Pixels,
    /// The layer's layer mask.
    Mask,
    /// The Quick Mask (in Quick Mask mode).
    QuickMask,
    /// Alpha channel `n` (0-based, the Channels panel's order after the colour channels).
    Channel(usize),
}

impl PaintTarget {
    fn json(self) -> Value {
        match self {
            PaintTarget::Pixels => json!("pixels"),
            PaintTarget::Mask => json!("mask"),
            PaintTarget::QuickMask => json!("quickMask"),
            PaintTarget::Channel(i) => json!({ "channel": i }),
        }
    }
}

/// A point of a stroke, in document pixels, with the pen's pressure (0..=1; 1 for a mouse).
pub type StrokePoint = [f64; 3];

/// `edit.removeAi`'s params: Remove with AI. What was painted ([`RemoveAi::strokes`]), else the
/// selection ([`RemoveAi::selection`]), is erased and filled by Manga LaMa at its best quality
/// and full resolution, on the target, as one undo step ("Remove with AI").
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoveAi {
    /// What was painted (each a stroke of points); empty: the selection.
    pub strokes: Vec<Vec<StrokePoint>>,
    /// The brush's diameter in document pixels (strokes only).
    pub size: f64,
    /// The layer (default: the active one).
    pub layer: Option<LayerId>,
    /// Read the context from every visible layer, not only the target layer.
    pub sample_all_layers: bool,
    /// Where it lands.
    pub target: PaintTarget,
    /// 0 (default): full resolution (large holes in overlapping 512 px tiles); 64..=2048,
    /// multiple of 8: larger crops downscaled to it (faster, softer).
    pub max_side: u32,
    /// Download Manga LaMa (about 100 MB) during the run when it isn't on this device.
    pub allow_download: bool,
}

impl RemoveAi {
    /// Remove what these strokes painted with a brush of `size` px.
    pub fn strokes(strokes: Vec<Vec<StrokePoint>>, size: f64) -> Self {
        Self { strokes, size, ..Self::default() }
    }

    /// Remove what the selection covers.
    pub fn selection() -> Self {
        Self::default()
    }

    /// The command's params; `Err` for a size or `maxSide` the command would refuse.
    pub fn params(&self) -> Result<Value> {
        let mut p = Map::new();
        if !self.strokes.is_empty() {
            if !self.size.is_finite() || self.size <= 0.0 || self.size > 5000.0 {
                return Err(bad(REMOVE_AI, "the brush size must be 0..5000 px"));
            }
            if self.strokes.iter().flatten().any(|q| q.iter().any(|v| !v.is_finite())) {
                return Err(bad(REMOVE_AI, "stroke points must be finite"));
            }
            p.insert("strokes".into(), json!(self.strokes));
            p.insert("size".into(), json!(self.size));
        }
        if self.max_side != 0 && (!(64..=2048).contains(&self.max_side) || self.max_side % 8 != 0) {
            return Err(bad(REMOVE_AI, "maxSide must be 0 or 64..2048, a multiple of 8"));
        }
        p.insert("maxSide".into(), json!(self.max_side));
        p.insert("sampleAllLayers".into(), json!(self.sample_all_layers));
        p.insert("target".into(), self.target.json());
        if let Some(l) = self.layer {
            p.insert("layer".into(), json!(l.0));
        }
        if self.allow_download {
            p.insert("allowDownload".into(), json!(true));
        }
        Ok(Value::Object(p))
    }
}

/// How hard [`InpaintAi`] works.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AiQuality {
    /// Manga LaMa on a capable GPU, the lite MI-GAN on the CPU, phones and weak machines.
    #[default]
    Auto,
    /// The lite MI-GAN.
    Fast,
    /// Manga LaMa.
    Best,
}

/// `edit.inpaintAi`'s params: Clean Selected Area with AI (the selection erased and filled).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InpaintAi {
    /// The layer (default: the active pixel layer).
    pub layer: Option<LayerId>,
    /// Paint the fill into the layer (`true`) instead of onto a new layer above it.
    pub into_layer: bool,
    /// Which model.
    pub quality: AiQuality,
    /// 0: full resolution in overlapping tiles; else 64..=2048, a multiple of 8 (`None`: the
    /// command's default, 512).
    pub max_side: Option<u32>,
    /// Download the model during the run when it isn't on this device.
    pub allow_download: bool,
}

impl InpaintAi {
    /// The command's params.
    pub fn params(&self) -> Result<Value> {
        let mut p = Map::new();
        p.insert("output".into(), json!(if self.into_layer { "current" } else { "new" }));
        p.insert("quality".into(), json!(match self.quality {
            AiQuality::Auto => "auto",
            AiQuality::Fast => "fast",
            AiQuality::Best => "best",
        }));
        if let Some(m) = self.max_side {
            if m != 0 && (!(64..=2048).contains(&m) || m % 8 != 0) {
                return Err(bad(INPAINT_AI, "maxSide must be 0 or 64..2048, a multiple of 8"));
            }
            p.insert("maxSide".into(), json!(m));
        }
        if let Some(l) = self.layer {
            p.insert("layer".into(), json!(l.0));
        }
        if self.allow_download {
            p.insert("allowDownload".into(), json!(true));
        }
        Ok(Value::Object(p))
    }
}

/// `edit.removeBubbleText`'s params: the lettering in the plain (solid or gradient) speech
/// bubble at a point removed, without AI. A bubble on screentone or art is refused (`notPlain`):
/// Remove with AI is the tool for those.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoveBubbleText {
    /// The click, in document pixels.
    pub x: f64,
    /// The click, in document pixels.
    pub y: f64,
    /// How far from the background (a colour difference, 1..=255) a pixel counts as text;
    /// `None`: from the bubble's noise.
    pub tolerance: Option<f64>,
    /// Model the background from every visible layer.
    pub sample_all: bool,
    /// The result on a new layer above.
    pub new_layer: bool,
}

impl RemoveBubbleText {
    /// The click at `(x, y)`.
    pub fn at(x: f64, y: f64) -> Self {
        Self { x, y, ..Self::default() }
    }

    /// The command's params.
    pub fn params(&self) -> Result<Value> {
        if !self.x.is_finite() || !self.y.is_finite() {
            return Err(bad(REMOVE_BUBBLE_TEXT, "the click must be finite document pixels"));
        }
        let mut p = json!({"x": self.x, "y": self.y, "sampleAll": self.sample_all, "newLayer": self.new_layer});
        p["tolerance"] = match self.tolerance {
            None => json!("auto"),
            Some(t) if (1.0..=255.0).contains(&t) => json!(t),
            Some(_) => return Err(bad(REMOVE_BUBBLE_TEXT, "tolerance must be 1..255")),
        };
        Ok(p)
    }
}

/// Where [`UpscaleAi`] puts the result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpscaleOutput {
    /// Replace a single-layer document, else open a new one.
    #[default]
    Auto,
    /// Replace the document.
    Replace,
    /// A new document.
    NewDocument,
}

/// `image.upscaleAi`'s params: Upscale with AI (MangaJaNai for grayscale pages, IllustrationJaNai
/// for colour, the lite Real-ESRGAN Compact when fast).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpscaleAi {
    /// 2 or 4.
    pub scale: u8,
    /// A catalog model id; `None`: automatic.
    pub model: Option<String>,
    /// Which models (as [`InpaintAi::quality`]).
    pub quality: AiQuality,
    /// Where the result goes.
    pub output: UpscaleOutput,
    /// Download the model during the run when it isn't on this device.
    pub download: bool,
}

impl Default for UpscaleAi {
    fn default() -> Self {
        Self { scale: 2, model: None, quality: AiQuality::Auto, output: UpscaleOutput::Auto, download: false }
    }
}

impl UpscaleAi {
    /// The command's params.
    pub fn params(&self) -> Result<Value> {
        if !matches!(self.scale, 2 | 4) {
            return Err(bad(UPSCALE_AI, "scale must be 2 or 4"));
        }
        if let Some(m) = &self.model
            && model(m).is_none()
        {
            return Err(bad(UPSCALE_AI, format!("unknown model `{m}`")));
        }
        Ok(json!({
            "scale": self.scale,
            "model": self.model.as_deref().unwrap_or("auto"),
            "quality": match self.quality { AiQuality::Auto => "auto", AiQuality::Fast => "fast", AiQuality::Best => "best" },
            "output": match self.output { UpscaleOutput::Auto => "auto", UpscaleOutput::Replace => "replace", UpscaleOutput::NewDocument => "newDocument" },
            "download": self.download,
        }))
    }
}

// ---------------------------------------------------------------------------------------------
// Replies

/// A device fallback during a run (#22): `class` is `ai.gpuFallback` (the GPU failed and the CPU
/// finished it), `ai.directmlFallback` (DirectML couldn't run it; wgpu did) or `ai.cpuFallback`
/// (no usable GPU).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiNotice {
    /// Which fallback.
    pub class: String,
    /// What to tell people.
    pub text: String,
    /// The underlying error.
    pub cause: String,
}

/// What an AI command's result says (fields the command didn't report are empty).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiReply {
    /// Where it ran ("Vulkan …", "DirectML …", "WebGPU", "CPU").
    pub device: String,
    /// The model's catalog id.
    pub model: String,
    /// The lite model ran.
    pub lite: bool,
    /// It ran on the CPU.
    pub cpu: bool,
    /// It left the device it was headed for ([`AiReply::notices`] say why).
    pub fell_back: bool,
    /// The run's wall time.
    pub ms: Option<u64>,
    /// The device fallbacks.
    pub notices: Vec<AiNotice>,
    /// What changed, `[x, y, width, height]` in document pixels (`damage` or `region`).
    pub rect: Option<[i64; 4]>,
    /// The layer it landed on, when the command says.
    pub layer: Option<LayerId>,
    /// The whole result.
    pub raw: Value,
}

impl AiReply {
    /// Reads a command's (or a job's) result. Never fails: missing fields stay empty.
    pub fn from_value(v: &Value) -> Self {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        let rect = ["damage", "region", "bubbleBounds"].iter().find_map(|k| rect_of(v.get(*k)?));
        let notices = v
            .get("notices")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|n| serde_json::from_value::<AiNotice>(n.clone()).ok()).collect())
            .unwrap_or_default();
        Self {
            device: s("device"),
            model: s("model"),
            lite: b("lite"),
            cpu: b("cpu"),
            fell_back: b("fellBack"),
            ms: v.get("ms").and_then(Value::as_u64),
            notices,
            rect,
            layer: v.get("layer").and_then(Value::as_u64).map(LayerId),
            raw: v.clone(),
        }
    }
}

/// `[x, y, w, h]` or `{x, y, width, height}`.
fn rect_of(v: &Value) -> Option<[i64; 4]> {
    let n = |v: Option<&Value>| v.and_then(Value::as_f64).filter(|f| f.is_finite()).map(|f| f as i64);
    match v {
        Value::Array(a) if a.len() == 4 => Some([n(a.first())?, n(a.get(1))?, n(a.get(2))?, n(a.get(3))?]),
        Value::Object(o) => Some([n(o.get("x"))?, n(o.get("y"))?, n(o.get("width").or(o.get("w")))?, n(o.get("height").or(o.get("h")))?]),
        _ => None,
    }
}

#[cfg(test)]
#[path = "ai_tests.rs"]
mod tests;
