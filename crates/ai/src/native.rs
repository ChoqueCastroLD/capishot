//! The desktop and CLI [`Upscaler`]: models run in-process on a hardware GPU (wgpu) when there
//! is one, else on the CPU; weights come from the [`DiskCache`] or are downloaded (feature
//! `download`) from a base URL the shell configures.

use std::sync::{Arc, Mutex, PoisonError};

use crate::cache::DiskCache;
use crate::catalog::{Arch, ModelInfo};
use crate::runtime::{Runtime, block_on};
use crate::service::{Done, GpuBackend, Hooks, ModelState, Request, Upscaler};
use crate::tile::TileOptions;
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
    /// Why DirectML couldn't load a model (then `Auto` requests stay on wgpu).
    #[cfg_attr(not(all(feature = "directml", windows)), allow(dead_code))]
    directml_failed: Option<String>,
}

impl NativeUpscaler {
    /// `base_url`: where weight files are downloaded from (`None`: only cached files run).
    pub fn new(cache: DiskCache, base_url: Option<String>, choice: DeviceChoice) -> Self {
        Self { cache, base_url, choice, inner: Mutex::new(Inner::default()) }
    }

    /// The shells' default: the cache in `models_dir` ([`crate::cache::models_dir`], plus
    /// `PHOTOCRAFT_AI_MODELS` folders), downloads from [`crate::cache::models_url_from_env`], the
    /// device from `PHOTOCRAFT_AI_DEVICE`.
    pub fn from_env(models_dir: &std::path::Path) -> Self {
        Self::new(DiskCache::in_dir(models_dir), Some(crate::cache::models_url_from_env()), DeviceChoice::from_env())
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
        if !cpu && backend::directml_wanted(&req, &inner)? {
            match self.run_directml(&req, &bytes, &mut inner, hooks) {
                // Auto: DirectML couldn't load the model (an old driver, a broken install); the
                // wgpu path runs it, now and from now on.
                Err(AiError::Device(e)) if req.opts.backend == GpuBackend::Auto => inner.directml_failed = Some(e),
                r => return r,
            }
        }
        let rt = if req.opts.cpu { cpu_runtime()? } else { self.runtime(&mut inner)? };
        let device = if req.opts.cpu { "CPU".to_string() } else { inner.device.clone() };
        let reuse =
            inner.loaded.as_ref().is_some_and(|l| l.id == req.model.id && l.grayscale == req.grayscale && !l.is_directml()) && inner.loaded_cpu == req.opts.cpu;
        if !reuse {
            inner.loaded = None;
            inner.loaded = Some(Loaded::new(&rt, req.model, &bytes, req.grayscale)?);
            inner.loaded_cpu = req.opts.cpu;
        }
        let loaded = inner.loaded.as_ref().ok_or_else(|| AiError::Inference("model not loaded".into()))?;
        let status = format!("Upscaling with {title} ({device})");
        let progress = TileHooks { hooks, lo: 0.1, status };
        let tiles = req.opts.tiles.unwrap_or_else(|| if req.opts.cpu { default_tiles(req.model) } else { gpu_tiles(req.model) });
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

    /// The request on ONNX Runtime + DirectML. A failure while running is a GPU failure (the
    /// caller retries on the CPU); one while loading is [`AiError::Device`].
    #[cfg(all(feature = "directml", windows))]
    fn run_directml(&self, req: &Request, bytes: &[u8], inner: &mut Inner, hooks: &dyn Hooks) -> Result<crate::Planar> {
        let reuse = inner.loaded.as_ref().is_some_and(|l| l.id == req.model.id && l.grayscale == req.grayscale && l.is_directml());
        if !reuse {
            inner.loaded = None;
            inner.loaded = Some(Loaded::new_directml(req.model, bytes, req.grayscale)?);
            inner.loaded_cpu = false;
        }
        let loaded = inner.loaded.as_ref().ok_or_else(|| AiError::Inference("model not loaded".into()))?;
        let progress = TileHooks { hooks, lo: 0.1, status: format!("Upscaling with {} (GPU, DirectML)", req.model.title) };
        let tiles = req.opts.tiles.unwrap_or_else(|| default_tiles(req.model));
        let out = crate::upscale::upscale_directml(loaded, &req.input, tiles, &progress);
        if req.opts.unload {
            inner.loaded = None;
        }
        match out {
            Err(AiError::Inference(e)) => Err(AiError::Gpu(e)),
            r => r,
        }
    }

    #[cfg(not(all(feature = "directml", windows)))]
    fn run_directml(&self, _: &Request, _: &[u8], _: &mut Inner, _: &dyn Hooks) -> Result<crate::Planar> {
        Err(AiError::Device("DirectML is not in this build".into()))
    }
}

/// Whether a GPU request runs on DirectML.
mod backend {
    use super::{AiError, GpuBackend, Inner, Request, Result};

    #[cfg(all(feature = "directml", windows))]
    pub(super) fn directml_wanted(req: &Request, inner: &Inner) -> Result<bool> {
        let supported = super::Loaded::directml_supports(req.model);
        match req.opts.backend {
            GpuBackend::Wgpu => Ok(false),
            GpuBackend::Auto => Ok(supported && inner.directml_failed.is_none() && crate::directml::available().is_ok()),
            GpuBackend::DirectMl if !supported => Err(AiError::Device(format!("{} doesn't run on DirectML (only the ESRGAN models do)", req.model.title))),
            GpuBackend::DirectMl => crate::directml::available().map(|()| true).map_err(AiError::Device),
        }
    }

    #[cfg(not(all(feature = "directml", windows)))]
    pub(super) fn directml_wanted(req: &Request, _: &Inner) -> Result<bool> {
        match req.opts.backend {
            GpuBackend::DirectMl => Err(AiError::Device("DirectML is only in Windows builds (feature `directml`)".into())),
            _ => Ok(false),
        }
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

/// The tiling on a native GPU (wgpu): the model's default, but 192 input pixels for 4x FDAT
/// (IllustrationJaNai V3 detail) instead of 128: about 10 % faster on an RTX 3060 Ti (fewer,
/// fuller dispatches: a 760 × 1200 page in 27–29 s instead of 28–32; 0.65 GiB of video memory at
/// the peak), the output 41 dB PSNR from the 128 tiling's.
fn gpu_tiles(model: &ModelInfo) -> TileOptions {
    let d = default_tiles(model);
    if model.arch == Arch::Fdat && model.scale == 4 { TileOptions { tile: 192, ..d } } else { d }
}

/// Maps tile progress onto `lo..1` of the job.
struct TileHooks<'a> {
    hooks: &'a dyn Hooks,
    lo: f32,
    status: String,
}

impl Progress for TileHooks<'_> {
    /// "Upscaling with 2x Manga (GPU, DirectML) (tile 3 of 24)": tiles done of the total (the
    /// UI's step convention), with the runner's note (a tile being split) when there is one.
    fn report(&self, done: u64, total: u64, note: &str) {
        let f = if total == 0 { 0.0 } else { done as f32 / total as f32 };
        let status =
            if note.is_empty() { format!("{} (tile {done} of {total})", self.status) } else { format!("{}, {note} (tile {done} of {total})", self.status) };
        self.hooks.progress(self.lo + (1.0 - self.lo) * f, &status);
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
    // "Downloading Manga LaMa (100 MB): 45% at 12.3 MB/s", refreshed twice a second.
    let mb = model.bytes as f64 / 1e6;
    let t0 = std::time::Instant::now();
    let mut said: Option<std::time::Instant> = None;
    let mut status = format!("Downloading {} ({mb:.0} MB)", model.title);
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
        if said.is_none_or(|t| t.elapsed().as_millis() >= 500) {
            said = Some(std::time::Instant::now());
            let speed = out.len() as f64 / t0.elapsed().as_secs_f64().max(0.001) / 1e6;
            status =
                format!("Downloading {} ({mb:.0} MB): {:.0}% at {speed:.1} MB/s", model.title, (100.0 * out.len() as f64 / model.bytes.max(1) as f64).floor());
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
