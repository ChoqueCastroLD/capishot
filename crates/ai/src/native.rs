//! The desktop and CLI [`Upscaler`]: models run in-process on a hardware GPU (wgpu) when there
//! is one, else on the CPU; weights come from the [`DiskCache`] or are downloaded (feature
//! `download`) from a base URL the shell configures.

use std::sync::{Arc, Mutex, PoisonError};

use crate::cache::DiskCache;
use crate::catalog::ModelInfo;
use crate::runtime::{Runtime, block_on};
use crate::service::{Done, Hooks, ModelState, Request, Upscaler};
use crate::upscale::{Loaded, default_tiles, upscale};
use crate::{AiError, Progress, Result};

/// Which device to use (`PHOTOCRAFT_AI_DEVICE`: `auto`, `gpu` or `cpu`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceChoice {
    /// A discrete or integrated GPU, else the CPU (software GPU adapters are slower than the CPU
    /// path, so they are skipped).
    Auto,
    /// wgpu's default adapter, even a software one (lavapipe, SwiftShader).
    Gpu,
    Cpu,
}

impl DeviceChoice {
    pub fn from_env() -> Self {
        match std::env::var("PHOTOCRAFT_AI_DEVICE").as_deref() {
            Ok("gpu") => Self::Gpu,
            Ok("cpu") => Self::Cpu,
            _ => Self::Auto,
        }
    }
}

/// See the module docs.
pub struct NativeUpscaler {
    cache: DiskCache,
    base_url: Option<String>,
    choice: DeviceChoice,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    rt: Option<Runtime>,
    device: String,
    loaded: Option<Loaded>,
    /// `loaded` lives on the CPU runtime (a CPU request), not `rt`.
    loaded_cpu: bool,
}

impl NativeUpscaler {
    /// `base_url`: where weight files are downloaded from (`None`: only cached files run).
    pub fn new(cache: DiskCache, base_url: Option<String>, choice: DeviceChoice) -> Self {
        Self { cache, base_url, choice, inner: Mutex::new(Inner::default()) }
    }

    /// The shells' default: the cache under `config_dir` (plus `PHOTOCRAFT_AI_MODELS` folders),
    /// downloads from `PHOTOCRAFT_AI_MODELS_URL` (none by default: nothing is hosted publicly
    /// until the weights' licence is cleared), the device from `PHOTOCRAFT_AI_DEVICE`.
    pub fn from_env(config_dir: &std::path::Path) -> Self {
        let base = std::env::var("PHOTOCRAFT_AI_MODELS_URL").ok().filter(|u| !u.trim().is_empty());
        Self::new(DiskCache::in_config_dir(config_dir), base, DeviceChoice::from_env())
    }

    fn runtime(&self, inner: &mut Inner) -> Result<Runtime> {
        if let Some(rt) = &inner.rt {
            return Ok(rt.clone());
        }
        let (rt, name) = pick_runtime(self.choice)?;
        inner.device = name;
        inner.rt = Some(rt.clone());
        Ok(rt)
    }

    fn weights(&self, model: &'static ModelInfo, allow_download: bool, hooks: &dyn Hooks) -> Result<Vec<u8>> {
        if let Some(b) = self.cache.load(model) {
            return Ok(b);
        }
        if !allow_download {
            return Err(AiError::Download(format!("{} isn't downloaded yet ({} MB)", model.title, model.bytes / 1_000_000)));
        }
        let Some(base) = &self.base_url else { return Err(AiError::Download("model downloads are not configured in this build".into())) };
        let bytes = download(&model.url(base), model, hooks)?;
        self.cache.store(model, &bytes)?;
        Ok(bytes)
    }

    fn run_sync(&self, req: Request, hooks: &dyn Hooks) -> Result<crate::Planar> {
        let title = req.model.title;
        hooks.progress(0.0, &format!("Loading {title}"));
        let bytes = self.weights(req.model, req.allow_download, hooks)?;
        if hooks.cancelled() {
            return Err(AiError::Cancelled);
        }
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let cpu = req.opts.cpu || self.choice == DeviceChoice::Cpu;
        let rt = if req.opts.cpu { cpu_runtime()? } else { self.runtime(&mut inner)? };
        let device = if req.opts.cpu { "CPU".to_string() } else { inner.device.clone() };
        let reuse = inner.loaded.as_ref().is_some_and(|l| l.id == req.model.id && l.grayscale == req.grayscale) && inner.loaded_cpu == req.opts.cpu;
        if !reuse {
            inner.loaded = None;
            inner.loaded = Some(Loaded::new(&rt, req.model, &bytes, req.grayscale)?);
            inner.loaded_cpu = req.opts.cpu;
        }
        let loaded = inner.loaded.as_ref().ok_or_else(|| AiError::Inference("model not loaded".into()))?;
        let status = format!("Upscaling with {title} ({device})");
        let progress = TileHooks { hooks, lo: 0.1, status };
        let tiles = req.opts.tiles.unwrap_or_else(|| default_tiles(req.model));
        let threads = req.opts.threads;
        let run = || block_on(upscale(&rt, loaded, &req.input, tiles, &progress));
        // CPU threads: a pool of that size for this run (0: rayon's global pool, one per core).
        let out = match (cpu, threads) {
            (true, n) if n > 0 => with_threads(n, run),
            _ => run(),
        };
        if req.opts.unload {
            inner.loaded = None;
        }
        out
    }
}

#[cfg(feature = "cpu")]
fn cpu_runtime() -> Result<Runtime> {
    Ok(Runtime::cpu())
}

#[cfg(not(feature = "cpu"))]
fn cpu_runtime() -> Result<Runtime> {
    Err(AiError::Device("this build has no CPU backend".into()))
}

/// Runs `f` on a rayon pool of `n` threads (the CPU kernels use rayon's current pool).
#[cfg(feature = "cpu")]
fn with_threads<T: Send>(n: usize, f: impl FnOnce() -> T + Send) -> T {
    match rayon::ThreadPoolBuilder::new().num_threads(n).build() {
        Ok(pool) => pool.install(f),
        Err(_) => f(),
    }
}

#[cfg(not(feature = "cpu"))]
fn with_threads<T: Send>(_: usize, f: impl FnOnce() -> T + Send) -> T {
    f()
}

/// Picks the device for `choice`, with a name for the UI.
pub fn pick_runtime(choice: DeviceChoice) -> Result<(Runtime, String)> {
    #[cfg(feature = "gpu")]
    {
        use burn::tensor::DeviceKind;
        let kinds: &[DeviceKind] = match choice {
            DeviceChoice::Auto => &[DeviceKind::DiscreteGpu(0), DeviceKind::IntegratedGpu(0)],
            DeviceChoice::Gpu => &[DeviceKind::DefaultDevice],
            DeviceChoice::Cpu => &[],
        };
        for k in kinds {
            let kind = k.clone();
            // A driver that fails inside wgpu must not take the app down.
            if let Ok(Ok(rt)) = std::panic::catch_unwind(move || Runtime::gpu_kind_blocking(kind)) {
                return Ok((rt, format!("GPU, {k:?}")));
            }
        }
        if choice == DeviceChoice::Gpu {
            return Err(AiError::Device("no wgpu adapter".into()));
        }
    }
    #[cfg(feature = "cpu")]
    {
        let _ = choice;
        Ok((Runtime::cpu(), "CPU".into()))
    }
    #[cfg(not(feature = "cpu"))]
    {
        let _ = choice;
        Err(AiError::Device("no GPU, and this build has no CPU fallback".into()))
    }
}

/// Maps tile progress onto `lo..1` of the job.
struct TileHooks<'a> {
    hooks: &'a dyn Hooks,
    lo: f32,
    status: String,
}

impl Progress for TileHooks<'_> {
    fn report(&self, done: u64, total: u64, _: &str) {
        let f = if total == 0 { 0.0 } else { done as f32 / total as f32 };
        self.hooks.progress(self.lo + (1.0 - self.lo) * f, &self.status);
    }
    fn cancelled(&self) -> bool {
        self.hooks.cancelled()
    }
}

impl Upscaler for NativeUpscaler {
    fn model_state(&self, model: &ModelInfo) -> ModelState {
        if self.cache.contains(model) {
            ModelState::Ready
        } else if self.base_url.is_some() && cfg!(feature = "download") {
            ModelState::NeedsDownload
        } else {
            ModelState::Unavailable(format!("{} isn't installed and model downloads are not configured in this build", model.title))
        }
    }

    fn device(&self) -> String {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner).device.clone()
    }

    fn profile(&self) -> crate::profile::DeviceProfile {
        use crate::profile::{DeviceProfile, GpuClass};
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        // Known once the runtime came up (bringing a GPU up just to ask would cost seconds).
        let gpu = match (&inner.rt, self.choice) {
            (_, DeviceChoice::Cpu) => GpuClass::None,
            (Some(rt), _) if rt.accel() == crate::runtime::Accel::Gpu => GpuClass::Integrated,
            (Some(_), _) => GpuClass::None,
            (None, _) => GpuClass::Unknown,
        };
        DeviceProfile {
            gpu,
            gpu_name: if gpu == GpuClass::Integrated { inner.device.clone() } else { String::new() },
            memory_gb: None,
            cores: std::thread::available_parallelism().ok().map(|n| n.get()),
            mobile: false,
            cpu_threads: true,
        }
    }

    fn storage_used(&self) -> Option<u64> {
        let mut files: Vec<(&str, u64)> = crate::catalog::MODELS.iter().filter(|m| self.cache.contains(m)).map(|m| (m.file, m.bytes)).collect();
        files.sort_unstable();
        files.dedup();
        Some(files.iter().map(|(_, b)| b).sum())
    }

    fn clear_downloads(&self) -> Result<()> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner).loaded = None;
        self.cache.clear()
    }

    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: Done) {
        let cpu = req.opts.cpu || self.choice == DeviceChoice::Cpu;
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_sync(req, hooks.as_ref())))
            .unwrap_or_else(|_| Err(AiError::Inference("the AI runtime failed with an internal error".into())));
        // A failure running the model on the GPU: the caller runs it again on the CPU.
        let on_gpu = !cpu && self.inner.lock().unwrap_or_else(PoisonError::into_inner).rt.as_ref().is_some_and(|rt| rt.accel() == crate::runtime::Accel::Gpu);
        let r = match r {
            Err(AiError::Inference(e)) if on_gpu => Err(AiError::Gpu(e)),
            r => r,
        };
        if r.is_err() {
            // A failed GPU may be lost: start from a fresh model next time.
            self.inner.lock().unwrap_or_else(PoisonError::into_inner).loaded = None;
        }
        done(r);
    }
}

/// Downloads `url` (at most `model.bytes`), reporting progress on `0..0.1` and stopping when
/// cancelled. Verifies the checksum.
#[cfg(feature = "download")]
fn download(url: &str, model: &ModelInfo, hooks: &dyn Hooks) -> Result<Vec<u8>> {
    use std::io::Read;
    let err = |e: &dyn std::fmt::Display| AiError::Download(format!("{url}: {e}"));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(20)))
        .timeout_recv_body(Some(std::time::Duration::from_secs(60)))
        .user_agent("Photocraft-dev")
        .build()
        .into();
    let mut resp = agent.get(url).call().map_err(|e| err(&e))?;
    let mut reader = resp.body_mut().with_config().limit(model.bytes.saturating_add(1)).reader();
    let mut out = Vec::with_capacity(usize::try_from(model.bytes).unwrap_or(0));
    let mut buf = vec![0u8; 1 << 16];
    let status = format!("Downloading {}", model.title);
    loop {
        if hooks.cancelled() {
            return Err(AiError::Cancelled);
        }
        let n = reader.read(&mut buf).map_err(|e| err(&e))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(buf.get(..n).unwrap_or(&[]));
        if out.len() as u64 > model.bytes {
            return Err(AiError::Download(format!("{url}: larger than expected")));
        }
        hooks.progress(0.1 * out.len() as f32 / model.bytes.max(1) as f32, &status);
    }
    crate::cache::verify(model, &out)?;
    Ok(out)
}

#[cfg(not(feature = "download"))]
fn download(url: &str, _: &ModelInfo, _: &dyn Hooks) -> Result<Vec<u8>> {
    Err(AiError::Download(format!("{url}: this build can't download models")))
}
