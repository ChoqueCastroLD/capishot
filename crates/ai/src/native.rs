//! The desktop and CLI [`Upscaler`]: models run in-process on a hardware GPU (wgpu) when there
//! is one, else on the CPU; weights come from the [`DiskCache`] or are downloaded (feature
//! `download`) from a base URL the shell configures.
//! On Windows with feature `directml`, GPU runs go to DirectML and CPU runs to ONNX Runtime's
//! CPU provider when its library loads ([`crate::directml`]); burn runs them otherwise.

use std::sync::{Arc, Mutex, PoisonError};

use crate::cache::DiskCache;
use crate::catalog::{Arch, ModelInfo};
use crate::runtime::{Runtime, block_on};
use crate::service::{Done, Fallback, GpuBackend, Hooks, ModelState, Request, Upscaler};
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

/// Failures to inject (tests): each makes that backend fail as a real one would.
#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct Faults {
    /// DirectML can't load the model (as without ONNX Runtime's library, or an old driver).
    pub directml_load: Option<String>,
    /// DirectML loads it, but the run fails (a GPU failure: the caller retries on the CPU).
    pub directml_run: Option<String>,
    /// wgpu can't start (no adapter, a broken driver).
    pub wgpu: Option<String>,
}

/// See the module docs.
pub struct NativeUpscaler {
    cache: DiskCache,
    base_url: Option<String>,
    choice: DeviceChoice,
    inner: Mutex<Inner>,
    faults: Faults,
}

#[derive(Default)]
struct Inner {
    rt: Option<Runtime>,
    device: String,
    /// `rt` is the CPU (or a software adapter) though GPU runs were asked for: why.
    rt_cpu_why: Option<String>,
    /// Where the last request ran ([`Upscaler::device`]).
    last_device: String,
    loaded: Option<Loaded>,
    /// `loaded` lives on the CPU runtime (a CPU request), not `rt`.
    loaded_cpu: bool,
    /// Why DirectML couldn't load a model (then `Auto` requests stay on wgpu).
    #[cfg_attr(not(all(feature = "directml", windows)), allow(dead_code))]
    directml_failed: Option<String>,
    /// Why ONNX Runtime's CPU provider couldn't run a model (then CPU requests stay on burn).
    #[cfg_attr(not(all(feature = "directml", any(windows, target_os = "linux"))), allow(dead_code))]
    onnx_cpu_failed: Option<String>,
}

impl NativeUpscaler {
    /// `base_url`: where weight files are downloaded from (`None`: only cached files run).
    pub fn new(cache: DiskCache, base_url: Option<String>, choice: DeviceChoice) -> Self {
        Self { cache, base_url, choice, inner: Mutex::new(Inner::default()), faults: Faults::default() }
    }

    /// Makes the backends fail as `faults` says (tests).
    #[doc(hidden)]
    pub fn with_faults(mut self, faults: Faults) -> Self {
        self.faults = faults;
        self
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
        let (rt, name, why) = pick_runtime_why(self.choice, &self.faults)?;
        inner.device = name;
        inner.rt_cpu_why = why;
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
        let auto = req.opts.backend == GpuBackend::Auto && !cpu && Loaded::directml_supports(req.model);
        // DirectML couldn't load a model before: this automatic run leaves it too.
        let mut left = inner.directml_failed.clone().filter(|_| auto).map(|e| format!("DirectML couldn't load the model: {e}"));
        let injected = !cpu && req.opts.backend != GpuBackend::Wgpu && Loaded::directml_supports(req.model) && inner.directml_failed.is_none();
        if injected && let Some(e) = &self.faults.directml_load {
            if req.opts.backend == GpuBackend::DirectMl {
                return Err(AiError::Device(e.clone()));
            }
            inner.directml_failed = Some(e.clone());
            left = Some(format!("DirectML couldn't load the model: {e}"));
        } else if injected && let Some(e) = &self.faults.directml_run {
            return Err(AiError::Gpu(e.clone()));
        } else if !cpu && backend::directml_wanted(&req, &inner)? {
            match self.run_directml(&req, &bytes, &mut inner, hooks) {
                // Auto: DirectML couldn't load the model (an old driver, a broken install); the
                // wgpu path runs it, now and from now on.
                Err(AiError::Device(e)) if req.opts.backend == GpuBackend::Auto => {
                    left = Some(format!("DirectML couldn't load the model: {e}"));
                    inner.directml_failed = Some(e);
                }
                r => {
                    inner.last_device = "DirectML".into();
                    return r;
                }
            }
        }
        if cpu && let Some(r) = self.run_onnx_cpu(&req, &bytes, &mut inner, hooks) {
            inner.last_device = "CPU".into();
            return r;
        }
        let rt = if req.opts.cpu { cpu_runtime()? } else { self.runtime(&mut inner)? };
        let device = if req.opts.cpu { "CPU".to_string() } else { inner.device.clone() };
        inner.last_device.clone_from(&device);
        let cpu_why = inner.rt_cpu_why.clone().filter(|_| !req.opts.cpu);
        if let Some(f) = fallback(left, &device, cpu_why) {
            hooks.fell_back(&f);
        }
        let gray = Loaded::runs_gray(req.model, req.grayscale);
        let reuse = inner.loaded.as_ref().is_some_and(|l| l.id == req.model.id && l.grayscale == gray && l.is_burn()) && inner.loaded_cpu == req.opts.cpu;
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
        let gray = Loaded::runs_gray(req.model, req.grayscale);
        let reuse = inner.loaded.as_ref().is_some_and(|l| l.id == req.model.id && l.grayscale == gray && l.is_directml());
        if !reuse {
            inner.loaded = None;
            inner.loaded = Some(Loaded::new_directml(req.model, bytes, req.grayscale)?);
            inner.loaded_cpu = false;
        }
        let loaded = inner.loaded.as_ref().ok_or_else(|| AiError::Inference("model not loaded".into()))?;
        let progress = TileHooks { hooks, lo: 0.1, status: format!("Upscaling with {} (GPU, DirectML)", req.model.title) };
        let tiles = req.opts.tiles.unwrap_or_else(|| gpu_tiles(req.model));
        let out = crate::upscale::upscale_directml(loaded, &req.input, tiles, &progress);
        if req.opts.unload {
            inner.loaded = None;
        }
        match out {
            Err(AiError::Inference(e)) => Err(AiError::Gpu(e)),
            r => r,
        }
    }

    /// A CPU request on ONNX Runtime's CPU provider (2–6× faster than burn's CPU backend), with
    /// the CPU tiling; `None` runs it on burn instead (a model it doesn't take, no ONNX Runtime
    /// library, or a failure loading or running there, remembered: the CPU path never gets
    /// worse than burn's).
    #[cfg(all(feature = "directml", any(windows, target_os = "linux")))]
    fn run_onnx_cpu(&self, req: &Request, bytes: &[u8], inner: &mut Inner, hooks: &dyn Hooks) -> Option<Result<crate::Planar>> {
        if !Loaded::directml_supports(req.model) || inner.onnx_cpu_failed.is_some() || crate::directml::available().is_err() {
            return None;
        }
        let threads = req.opts.threads;
        let gray = Loaded::runs_gray(req.model, req.grayscale);
        let reuse = inner.loaded.as_ref().is_some_and(|l| l.id == req.model.id && l.grayscale == gray && l.onnx_cpu_threads() == Some(threads));
        if !reuse {
            inner.loaded = None;
            match Loaded::new_onnx_cpu(req.model, bytes, req.grayscale, threads) {
                Ok(l) => inner.loaded = Some(l),
                Err(e) => {
                    inner.onnx_cpu_failed = Some(e.to_string());
                    return None;
                }
            }
            inner.loaded_cpu = true;
        }
        let loaded = inner.loaded.as_ref()?;
        let progress = TileHooks { hooks, lo: 0.1, status: format!("Upscaling with {} (CPU)", req.model.title) };
        let tiles = req.opts.tiles.unwrap_or_else(|| default_tiles(req.model));
        let out = crate::upscale::upscale_directml(loaded, &req.input, tiles, &progress);
        if req.opts.unload || out.is_err() {
            inner.loaded = None;
        }
        match out {
            Err(AiError::Cancelled) => Some(Err(AiError::Cancelled)),
            Err(e) => {
                inner.onnx_cpu_failed = Some(e.to_string());
                None
            }
            r => Some(r),
        }
    }

    #[cfg(not(all(feature = "directml", any(windows, target_os = "linux"))))]
    fn run_onnx_cpu(&self, _: &Request, _: &[u8], _: &mut Inner, _: &dyn Hooks) -> Option<Result<crate::Planar>> {
        None
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
            // No usable ONNX Runtime fails the run with a device error: the caller leaves DirectML and says so.
            GpuBackend::Auto => Ok(supported && inner.directml_failed.is_none()),
            GpuBackend::DirectMl if !supported => {
                Err(AiError::Device(format!("{} doesn't run on DirectML (only the ESRGAN, FDAT and Compact models do)", req.model.title)))
            }
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
    pick_runtime_why(choice, &Faults::default()).map(|(rt, name, _)| (rt, name))
}

/// [`pick_runtime`], with why the runtime is the CPU (or a software adapter) when `choice`
/// allows the GPU.
fn pick_runtime_why(choice: DeviceChoice, faults: &Faults) -> Result<(Runtime, String, Option<String>)> {
    let mut why = (choice != DeviceChoice::Cpu).then(|| "this build has no GPU backend".to_string());
    if let Some(f) = faults.wgpu.as_ref().filter(|_| choice != DeviceChoice::Cpu) {
        if choice == DeviceChoice::Gpu {
            return Err(AiError::Device(f.clone()));
        }
        why = Some(format!("wgpu couldn't start: {f}"));
    } else {
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
                match std::panic::catch_unwind(move || Runtime::gpu_kind_blocking(kind)) {
                    Ok(Ok(rt)) => {
                        let software = rt.info().filter(|i| i.software).map(|i| format!("{} is a software adapter (no GPU behind it)", i.name));
                        return Ok((rt, format!("GPU, {k:?}"), software));
                    }
                    Ok(Err(e)) => why = Some(format!("no {k:?} adapter: {e}")),
                    Err(_) => why = Some("wgpu failed while starting".into()),
                }
            }
            if choice == DeviceChoice::Gpu {
                return Err(AiError::Device("no wgpu adapter".into()));
            }
        }
    }
    #[cfg(feature = "cpu")]
    {
        Ok((Runtime::cpu(), "CPU".into(), why))
    }
    #[cfg(not(feature = "cpu"))]
    {
        let _ = why;
        Err(AiError::Device("no GPU, and this build has no CPU fallback".into()))
    }
}

/// What an automatic request that may use the GPU left on its way to `to`: DirectML (`left`)
/// and/or the GPU (`cpu_why`). `None`: nothing.
fn fallback(left: Option<String>, to: &str, cpu_why: Option<String>) -> Option<Fallback> {
    let cpu = cpu_why.is_some();
    let from = match (&left, cpu) {
        (Some(_), _) => "DirectML",
        (None, true) => "GPU",
        (None, false) => return None,
    };
    let why = [left, cpu_why].into_iter().flatten().collect::<Vec<_>>().join("; ");
    Some(Fallback { from: from.into(), to: to.into(), cpu, why })
}

/// The tiling on a native GPU (wgpu): the model's default, but 192 input pixels for 4x FDAT
/// (IllustrationJaNai V3 detail) instead of 128: about 10 % faster on an RTX 3060 Ti (fewer,
/// fuller dispatches: a 760 × 1200 page in 27–29 s instead of 28–32; 0.65 GiB of video memory at
/// the peak), the output 41 dB PSNR from the 128 tiling's. DirectML runs the same tiles (FDAT grown
/// to one size there too, so its output is the wgpu path's).
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
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if inner.last_device.is_empty() { inner.device.clone() } else { inner.last_device.clone() }
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

#[cfg(all(test, feature = "cpu"))]
mod tests {
    use super::*;

    /// #22: wgpu can't start on an automatic run: the CPU runtime, and why; asked for the GPU:
    /// an error; asked for the CPU: nothing to say.
    #[test]
    fn a_runtime_fallback_says_why() {
        let faults = Faults { wgpu: Some("no adapter (injected)".into()), ..Default::default() };
        let (rt, name, why) = pick_runtime_why(DeviceChoice::Auto, &faults).unwrap();
        assert_eq!((rt.accel(), name.as_str()), (crate::runtime::Accel::Cpu, "CPU"));
        assert!(why.as_deref().is_some_and(|w| w.contains("injected")), "{why:?}");
        assert!(pick_runtime_why(DeviceChoice::Gpu, &faults).is_err());
        assert_eq!(pick_runtime_why(DeviceChoice::Cpu, &faults).unwrap().2, None);
        let up = NativeUpscaler::new(DiskCache::in_dir(&std::env::temp_dir()), None, DeviceChoice::Auto).with_faults(faults);
        let mut inner = Inner::default();
        up.runtime(&mut inner).unwrap();
        assert!(inner.rt_cpu_why.is_some());
    }

    #[test]
    fn fallback_names_what_was_left() {
        assert_eq!(fallback(None, "GPU, DiscreteGpu(0)", None), None);
        let f = fallback(Some("DirectML couldn't load the model: x".into()), "GPU, DiscreteGpu(0)", None).unwrap();
        assert_eq!((f.from.as_str(), f.cpu), ("DirectML", false));
        let f = fallback(Some("a".into()), "CPU", Some("b".into())).unwrap();
        assert_eq!((f.from.as_str(), f.to.as_str(), f.cpu, f.why.as_str()), ("DirectML", "CPU", true, "a; b"));
        let f = fallback(None, "CPU", Some("b".into())).unwrap();
        assert_eq!((f.from.as_str(), f.cpu), ("GPU", true));
    }
}
