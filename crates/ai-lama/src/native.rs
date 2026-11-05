//! [`NativeInpainter`]: the desktop/CLI [`Inpainter`], running LaMa (or the lite MI-GAN) in the
//! calling thread (the engine's job worker) on the GPU when there is a real one, else on the CPU.
//! On Windows with feature `directml`, Manga LaMa runs on DirectML through ONNX Runtime when its
//! library loads ([`crate::directml`], much faster); else, and for MI-GAN, burn on wgpu.
//! CPU runs of Manga LaMa there go to ONNX Runtime's CPU provider (2–3× faster than burn's
//! CPU backend), with burn's CPU backend as the fallback.
//!
//! The weights come from local files: the AI Model Manager's downloads (the app's models
//! folder, `photocraft-ai`'s cache) first, else `CAPISHOT_LAMA_WEIGHTS`; MI-GAN from `CAPISHOT_MIGAN_WEIGHTS`, else
//! [`MIGAN_FILE`] next to LaMa's. A missing file reports [`ModelState::NeedsDownload`] when
//! there is a downloads folder (the Manager fetches it, or [`NativeInpainter::with_fetch`] for a
//! request with `allow_download`), else [`ModelState::Unavailable`] with the path to put it at.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use burn::tensor::Device;

use crate::service::{Done, Fallback, GpuBackend, Hooks, Inpainter, LAMA, MIGAN, ModelState, Request};
use crate::{Generator, LamaError, Result};

/// The weights' file name in the models folder.
pub const WEIGHTS_FILE: &str = "manga-lama-f16.safetensors";
/// MI-GAN's file name in the models folder.
pub const MIGAN_FILE: &str = "migan-512-places2-f16.safetensors";
/// Download sizes of the weight files (as `photocraft_ai::catalog`; the engine's tests check).
pub const LAMA_BYTES: u64 = 101_904_726;
pub const MIGAN_BYTES: u64 = 13_198_644;
/// Larger files are refused before reading (the f32 file is 204 MB).
const MAX_WEIGHTS_BYTES: u64 = 512 << 20;

#[cfg(all(feature = "directml", any(windows, target_os = "linux")))]
type Dml = crate::directml::DmlLama;
#[cfg(not(all(feature = "directml", any(windows, target_os = "linux"))))]
type Dml = ();

/// Failures to inject (tests): each makes that backend fail as a real one would.
#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct Faults {
    /// DirectML can't load Manga LaMa (as without ONNX Runtime's library, or an old driver).
    pub directml_load: Option<String>,
    /// DirectML loads it, but the run fails.
    pub directml_run: Option<String>,
    /// wgpu can't start (no adapter, a broken driver).
    pub wgpu: Option<String>,
}

/// The model loaded on burn.
struct Burn {
    id: &'static str,
    /// Loaded for a CPU request.
    cpu: bool,
    generator: Arc<Generator>,
    /// The device's name ([`Inpainter::device`]).
    device: String,
    /// It may use the GPU, but runs on the CPU (or a software adapter): why.
    cpu_why: Option<String>,
}

/// A fetch's progress: `0..=1` and a short status.
pub type FetchProgress<'a> = &'a (dyn Fn(f32, &str) + Sync);
/// Whether the fetch should give up.
pub type FetchCancelled<'a> = &'a (dyn Fn() -> bool + Sync);
/// Fetches a model's weights into the downloads folder: `(model id, progress, cancelled)`. The
/// shell provides it (this crate has no network): `photocraft_ai::native::model_fetcher`.
pub type Fetch = Arc<dyn Fn(&str, FetchProgress<'_>, FetchCancelled<'_>) -> std::result::Result<(), String> + Send + Sync>;

/// See the module docs.
pub struct NativeInpainter {
    path: PathBuf,
    migan: Option<PathBuf>,
    /// The AI Model Manager's downloads folder, searched first.
    downloads: Option<PathBuf>,
    /// Gets missing weights for a request that allows it (`allow_download`).
    fetch: Option<Fetch>,
    /// The loaded model.
    model: Mutex<Option<Burn>>,
    /// Manga LaMa on DirectML, once loaded.
    #[cfg_attr(not(all(feature = "directml", windows)), allow(dead_code))]
    dml: Mutex<Option<Arc<Dml>>>,
    /// Why DirectML couldn't load Manga LaMa: automatic runs use burn after that.
    #[cfg_attr(not(all(feature = "directml", windows)), allow(dead_code))]
    dml_failed: Mutex<Option<String>>,
    /// Manga LaMa on ONNX Runtime's CPU provider, once loaded (CPU runs; burn's CPU backend
    /// after it fails).
    #[cfg_attr(not(all(feature = "directml", windows)), allow(dead_code))]
    ort_cpu: Mutex<Option<(usize, Arc<Dml>)>>,
    #[cfg_attr(not(all(feature = "directml", windows)), allow(dead_code))]
    ort_cpu_failed: Mutex<bool>,
    device_name: Mutex<String>,
    faults: Faults,
}

impl NativeInpainter {
    /// An inpainter loading LaMa from `path` on first use (MI-GAN: [`NativeInpainter::with_migan`]).
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            migan: None,
            downloads: None,
            fetch: None,
            model: Mutex::new(None),
            dml: Mutex::new(None),
            dml_failed: Mutex::new(None),
            ort_cpu: Mutex::new(None),
            ort_cpu_failed: Mutex::new(false),
            device_name: Mutex::new(String::new()),
            faults: Faults::default(),
        }
    }

    /// Also offers MI-GAN, loaded from `path`.
    pub fn with_migan(mut self, path: PathBuf) -> Self {
        self.migan = Some(path);
        self
    }

    /// Makes the backends fail as `faults` says (tests).
    #[doc(hidden)]
    pub fn with_faults(mut self, faults: Faults) -> Self {
        self.faults = faults;
        self
    }

    /// Also looks in `dir` (the AI Model Manager's downloads), before the other paths.
    pub fn with_downloads(mut self, dir: PathBuf) -> Self {
        self.downloads = Some(dir);
        self
    }

    /// How a request with `allow_download` gets weights that aren't here.
    pub fn with_fetch(mut self, fetch: Fetch) -> Self {
        self.fetch = Some(fetch);
        self
    }

    /// Fetches `req`'s weights when they are missing and the request allows it.
    fn fetch_if_missing(&self, req: &Request, hooks: &Arc<dyn Hooks>) -> Result<()> {
        let Some(fetch) = self.fetch.as_ref().filter(|_| req.allow_download) else { return Ok(()) };
        if self.path_of(req.model).is_none_or(|p| p.is_file()) {
            return Ok(());
        }
        let progress = |fraction: f32, status: &str| hooks.progress(fraction, status);
        let cancelled = || hooks.cancelled();
        fetch(req.model, &progress, &cancelled).map_err(|e| if hooks.cancelled() { LamaError::Cancelled } else { LamaError::Unavailable(e) })
    }

    fn file_of(model: &str) -> Option<(&'static str, u64)> {
        match model {
            LAMA => Some((WEIGHTS_FILE, LAMA_BYTES)),
            MIGAN => Some((MIGAN_FILE, MIGAN_BYTES)),
            _ => None,
        }
    }

    fn path_of(&self, model: &str) -> Option<PathBuf> {
        let (file, _) = Self::file_of(model)?;
        if let Some(p) = self.downloads.as_ref().map(|d| d.join(file)).filter(|p| p.is_file()) {
            return Some(p);
        }
        match model {
            LAMA => Some(self.path.clone()),
            MIGAN => self.migan.clone(),
            _ => None,
        }
    }

    /// `CAPISHOT_LAMA_WEIGHTS`, else `models_dir` (the AI Model Manager's downloads) +
    /// [`WEIGHTS_FILE`]; `None` when neither is known.
    pub fn locate(models_dir: Option<&Path>) -> Option<Self> {
        let path = match std::env::var_os("CAPISHOT_LAMA_WEIGHTS") {
            Some(p) => PathBuf::from(p),
            None => models_dir?.join(WEIGHTS_FILE),
        };
        let migan = match std::env::var_os("CAPISHOT_MIGAN_WEIGHTS") {
            Some(p) => PathBuf::from(p),
            None => path.with_file_name(MIGAN_FILE),
        };
        let s = Self::new(path).with_migan(migan);
        Some(match models_dir {
            Some(m) => s.with_downloads(m.to_path_buf()),
            None => s,
        })
    }

    /// The weights file this inpainter reads.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// GPU through wgpu unless it is a software adapter (lavapipe/llvmpipe are far slower than
    /// the CPU backend) or `CAPISHOT_AI_DEVICE=cpu`; else the CPU backend. With the device's name
    /// and, when the request may use the GPU but this device is the CPU (or a software adapter),
    /// why.
    fn pick_device(cpu: bool, faults: &Faults) -> Result<(Device, String, Option<String>)> {
        let force_cpu = cpu_forced(cpu);
        let mut why = (!force_cpu).then(|| "this build has no GPU backend".to_string());
        if let Some(f) = faults.wgpu.as_ref().filter(|_| !force_cpu) {
            why = Some(format!("wgpu couldn't start: {f}"));
        } else if !force_cpu {
            #[cfg(feature = "wgpu")]
            {
                // The wgpu runtime may panic deep inside on a broken driver; keep that a fallback.
                let gpu = std::panic::catch_unwind(|| burn::tensor::Device::wgpu_options().init_with_setup());
                match gpu {
                    Ok(Ok((device, setup))) => {
                        let info = setup.adapter.get_info();
                        let name = format!("{:?}: {}", setup.backend, info.name);
                        if info.device_type != burn::tensor::wgpu::wgpu::DeviceType::Cpu {
                            return Ok((device, name, None));
                        }
                        let software = format!("{} is a software adapter (no GPU behind it)", info.name);
                        if cfg!(not(feature = "flex")) {
                            return Ok((device, name, Some(software)));
                        }
                        why = Some(software);
                    }
                    Ok(Err(e)) => why = Some(format!("wgpu couldn't start: {e}")),
                    Err(_) => why = Some("wgpu failed while starting".into()),
                }
            }
        }
        #[cfg(feature = "flex")]
        {
            Ok((Device::flex(), "CPU".into(), why))
        }
        #[cfg(not(feature = "flex"))]
        Err(LamaError::Unavailable(match why {
            None => "CAPISHOT_AI_DEVICE=cpu but this build has no CPU backend".into(),
            Some(w) => format!("no usable GPU ({w}) and this build has no CPU backend"),
        }))
    }

    /// The weight file's bytes for model `id`.
    fn weights(&self, id: &str) -> Result<Vec<u8>> {
        let path = self.path_of(id).ok_or_else(|| LamaError::Unavailable(format!("{id} isn't available here")))?;
        let path = path.as_path();
        let len = std::fs::metadata(path).map_err(|e| LamaError::Unavailable(format!("{id} weights not found at {}: {e}", path.display())))?.len();
        if len > MAX_WEIGHTS_BYTES {
            return Err(LamaError::Weights(format!("{} is {len} bytes, more than any inpainting model", path.display())));
        }
        std::fs::read(path).map_err(|e| LamaError::Unavailable(format!("{}: {e}", path.display())))
    }

    /// Whether `req` goes to DirectML: Manga LaMa on the GPU, not asked for wgpu, and (when
    /// automatic) DirectML hasn't failed to load it before.
    fn wants_directml(&self, req: &Request) -> bool {
        req.model == LAMA
            && !cpu_forced(req.cpu)
            && match req.backend {
                GpuBackend::Wgpu => false,
                GpuBackend::DirectMl => true,
                GpuBackend::Auto => self.dml_failed.lock().unwrap_or_else(PoisonError::into_inner).is_none(),
            }
    }

    /// Runs `req` on DirectML when [`Self::wants_directml`]; `None` runs it on burn instead (an
    /// automatic run where DirectML can't: no ONNX Runtime library, a failed session or run; then
    /// `left` says why).
    #[cfg(all(feature = "directml", windows))]
    fn run_directml(&self, req: &Request, left: &mut Option<String>) -> Option<Result<Vec<f32>>> {
        if let Some(r) = self.injected_directml(req, left) {
            return r;
        }
        if !self.wants_directml(req) {
            return None;
        }
        let auto = req.backend == GpuBackend::Auto;
        let mut slot = self.dml.lock().unwrap_or_else(PoisonError::into_inner);
        let dml = match slot.as_ref() {
            Some(d) => d.clone(),
            None => {
                let bytes = match self.weights(LAMA) {
                    Ok(b) => b,
                    Err(e) => return Some(Err(e)),
                };
                match crate::directml::DmlLama::from_safetensors(&bytes) {
                    Ok(d) => {
                        // The burn copy on the GPU isn't needed while DirectML runs the model.
                        let mut m = self.model.lock().unwrap_or_else(PoisonError::into_inner);
                        if m.as_ref().is_some_and(|b| b.id == LAMA && !b.cpu) {
                            *m = None;
                        }
                        let d = Arc::new(d);
                        *slot = Some(d.clone());
                        d
                    }
                    Err(e) if auto => {
                        *self.dml_failed.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.to_string());
                        *left = Some(format!("DirectML couldn't load the model: {e}"));
                        return None;
                    }
                    Err(e) => return Some(Err(e)),
                }
            }
        };
        let t = &req.tile;
        let out = dml.generate(&t.rgb, &t.mask, t.w, t.h);
        if out.is_err() || req.unload {
            // Frees its device memory (after a tile too large for it, too): the next tile builds it again.
            *slot = None;
        }
        match out {
            Ok(o) => {
                *self.device_name.lock().unwrap_or_else(PoisonError::into_inner) = "DirectML".into();
                Some(Ok(o))
            }
            Err(e) if auto => {
                *left = Some(format!("DirectML couldn't run the model: {e}"));
                None
            }
            Err(e) => Some(Err(e)),
        }
    }

    /// Runs a CPU request for Manga LaMa on ONNX Runtime's CPU provider; `None` runs it on
    /// burn's CPU backend instead (no ONNX Runtime library, a failed session or run: the CPU
    /// path never gets worse than burn's).
    #[cfg(all(feature = "directml", any(windows, target_os = "linux")))]
    fn run_ort_cpu(&self, req: &Request) -> Option<Result<Vec<f32>>> {
        if req.model != LAMA || !cpu_forced(req.cpu) || *self.ort_cpu_failed.lock().unwrap_or_else(PoisonError::into_inner) {
            return None;
        }
        let fail = || {
            *self.ort_cpu_failed.lock().unwrap_or_else(PoisonError::into_inner) = true;
            None
        };
        let mut slot = self.ort_cpu.lock().unwrap_or_else(PoisonError::into_inner);
        let ort = match slot.as_ref() {
            Some((threads, d)) if *threads == req.threads => d.clone(),
            _ => {
                *slot = None;
                let Ok(bytes) = self.weights(LAMA) else { return None };
                let Ok(d) = crate::directml::DmlLama::cpu_from_safetensors(&bytes, req.threads) else { return fail() };
                // burn's CPU copy isn't needed while ONNX Runtime runs the model.
                let mut m = self.model.lock().unwrap_or_else(PoisonError::into_inner);
                if m.as_ref().is_some_and(|b| b.id == LAMA && b.cpu) {
                    *m = None;
                }
                let d = Arc::new(d);
                *slot = Some((req.threads, d.clone()));
                d
            }
        };
        let t = &req.tile;
        let out = ort.generate(&t.rgb, &t.mask, t.w, t.h);
        if out.is_err() || req.unload {
            *slot = None;
        }
        match out {
            Ok(o) => {
                *self.device_name.lock().unwrap_or_else(PoisonError::into_inner) = "CPU".into();
                Some(Ok(o))
            }
            Err(_) => fail(),
        }
    }

    #[cfg(not(all(feature = "directml", any(windows, target_os = "linux"))))]
    fn run_ort_cpu(&self, _req: &Request) -> Option<Result<Vec<f32>>> {
        None
    }

    #[cfg(not(all(feature = "directml", windows)))]
    fn run_directml(&self, req: &Request, left: &mut Option<String>) -> Option<Result<Vec<f32>>> {
        if let Some(r) = self.injected_directml(req, left) {
            return r;
        }
        (req.backend == GpuBackend::DirectMl && self.wants_directml(req))
            .then(|| Err(LamaError::Unavailable("DirectML runs only in Windows builds with the `directml` feature".into())))
    }

    /// An injected DirectML failure ([`Faults`]), as [`Self::run_directml`] answers it; `None`
    /// when there is none for `req`.
    fn injected_directml(&self, req: &Request, left: &mut Option<String>) -> Option<Option<Result<Vec<f32>>>> {
        let f = &self.faults;
        if (f.directml_load.is_none() && f.directml_run.is_none()) || !self.wants_directml(req) {
            return None;
        }
        let auto = req.backend == GpuBackend::Auto;
        Some(match (&f.directml_load, &f.directml_run) {
            (Some(e), _) if auto => {
                *self.dml_failed.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.clone());
                *left = Some(format!("DirectML couldn't load the model: {e}"));
                None
            }
            (Some(e), _) => Some(Err(LamaError::Unavailable(e.clone()))),
            (None, Some(e)) if auto => {
                *left = Some(format!("DirectML couldn't run the model: {e}"));
                None
            }
            (None, Some(e)) => Some(Err(LamaError::Gpu(e.clone()))),
            (None, None) => return None,
        })
    }

    /// The model on burn: the generator, the device's name, and why it is on the CPU though
    /// the request may use the GPU.
    fn model(&self, id: &'static str, cpu: bool) -> Result<(Arc<Generator>, String, Option<String>)> {
        let mut slot = self.model.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(b) = slot.as_ref()
            && b.id == id
            && b.cpu == cpu
        {
            return Ok((b.generator.clone(), b.device.clone(), b.cpu_why.clone()));
        }
        *slot = None;
        let bytes = self.weights(id)?;
        let (device, name, cpu_why) = Self::pick_device(cpu, &self.faults)?;
        let generator = Arc::new(Generator::from_safetensors(&bytes, &device)?);
        *slot = Some(Burn { id, cpu, generator: generator.clone(), device: name.clone(), cpu_why: cpu_why.clone() });
        Ok((generator, name, cpu_why))
    }
}

/// The request is for the CPU, or `CAPISHOT_AI_DEVICE=cpu` says every request is.
fn cpu_forced(cpu: bool) -> bool {
    cpu || std::env::var("CAPISHOT_AI_DEVICE").is_ok_and(|v| v.eq_ignore_ascii_case("cpu"))
}

/// What an automatic request that may use the GPU left on its way to `to`: DirectML (`left`)
/// and/or the GPU (`cpu_why`, `to` is the CPU or a software adapter). `None`: nothing.
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

impl Inpainter for NativeInpainter {
    fn model_state(&self) -> ModelState {
        self.model_state_of(LAMA)
    }

    fn model_state_of(&self, model: &str) -> ModelState {
        let loaded = self.model.lock().unwrap_or_else(PoisonError::into_inner).as_ref().is_some_and(|b| b.id == model);
        match self.path_of(model) {
            _ if loaded => ModelState::Ready,
            Some(p) if p.is_file() => ModelState::Ready,
            Some(_) if self.downloads.is_some() => ModelState::NeedsDownload { bytes: Self::file_of(model).map_or(0, |(_, b)| b) },
            Some(p) => {
                let (file, var) = if model == MIGAN { (MIGAN_FILE, "CAPISHOT_MIGAN_WEIGHTS") } else { (WEIGHTS_FILE, "CAPISHOT_LAMA_WEIGHTS") };
                ModelState::Unavailable(format!("{model} weights not found: put {file} at {} (or set {var})", p.display()))
            }
            None => ModelState::Unavailable(format!("{model} isn't available here")),
        }
    }

    fn device(&self) -> String {
        self.device_name.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: Done) {
        if hooks.cancelled() {
            return done(Err(LamaError::Cancelled));
        }
        // burn panics on device failures (out of memory, a lost GPU): report it as an error and
        // drop the model so the next request loads it again.
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.fetch_if_missing(&req, &hooks)?;
            // An automatic run DirectML couldn't load the model for earlier: it leaves DirectML too.
            let mut left = (req.backend == GpuBackend::Auto && req.model == LAMA && !cpu_forced(req.cpu))
                .then(|| self.dml_failed.lock().unwrap_or_else(PoisonError::into_inner).clone())
                .flatten()
                .map(|e| format!("DirectML couldn't load the model: {e}"));
            if let Some(r) = self.run_directml(&req, &mut left).or_else(|| self.run_ort_cpu(&req)) {
                return r;
            }
            self.model(req.model, req.cpu).and_then(|(m, name, cpu_why)| {
                if let Some(f) = fallback(left, &name, cpu_why) {
                    hooks.fell_back(&f);
                }
                *self.device_name.lock().unwrap_or_else(PoisonError::into_inner) = name;
                if hooks.cancelled() {
                    return Err(LamaError::Cancelled);
                }
                let t = &req.tile;
                let out = m.generate(&t.rgb, &t.mask, t.w, t.h);
                if req.unload {
                    *self.model.lock().unwrap_or_else(PoisonError::into_inner) = None;
                }
                out
            })
        }));
        // On the GPU, a failure is the GPU's: the caller runs the request again on the CPU.
        let on_gpu = !req.cpu && self.model.lock().unwrap_or_else(PoisonError::into_inner).as_ref().is_none_or(|b| !b.cpu) && self.device() != "CPU";
        let result = match run {
            Ok(Err(LamaError::Device(e))) if on_gpu => Err(LamaError::Gpu(e)),
            Ok(r) => r,
            Err(panic) => {
                *self.model.lock().unwrap_or_else(PoisonError::into_inner) = None;
                let why = panic.downcast_ref::<String>().map(String::as_str).or_else(|| panic.downcast_ref::<&str>().copied()).unwrap_or("unknown error");
                let why = format!("the inference backend failed: {why}");
                Err(if on_gpu { LamaError::Gpu(why) } else { LamaError::Device(why) })
            }
        };
        hooks.progress(1.0, "");
        done(result);
    }
}
