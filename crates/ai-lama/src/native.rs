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
//! there is a downloads folder (the Manager fetches it), else [`ModelState::Unavailable`] with
//! the path to put it at.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use burn::tensor::Device;

use crate::service::{Done, GpuBackend, Hooks, Inpainter, LAMA, MIGAN, ModelState, Request};
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

/// See the module docs.
pub struct NativeInpainter {
    path: PathBuf,
    migan: Option<PathBuf>,
    /// The AI Model Manager's downloads folder, searched first.
    downloads: Option<PathBuf>,
    /// The loaded model: its id, whether it is on the CPU, the generator.
    model: Mutex<Option<(&'static str, bool, Arc<Generator>)>>,
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
}

impl NativeInpainter {
    /// An inpainter loading LaMa from `path` on first use (MI-GAN: [`NativeInpainter::with_migan`]).
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            migan: None,
            downloads: None,
            model: Mutex::new(None),
            dml: Mutex::new(None),
            dml_failed: Mutex::new(None),
            ort_cpu: Mutex::new(None),
            ort_cpu_failed: Mutex::new(false),
            device_name: Mutex::new(String::new()),
        }
    }

    /// Also offers MI-GAN, loaded from `path`.
    pub fn with_migan(mut self, path: PathBuf) -> Self {
        self.migan = Some(path);
        self
    }

    /// Also looks in `dir` (the AI Model Manager's downloads), before the other paths.
    pub fn with_downloads(mut self, dir: PathBuf) -> Self {
        self.downloads = Some(dir);
        self
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
    /// the CPU backend) or `CAPISHOT_AI_DEVICE=cpu`; else the CPU backend.
    fn pick_device(cpu: bool) -> Result<(Device, String)> {
        let force_cpu = cpu || std::env::var("CAPISHOT_AI_DEVICE").is_ok_and(|v| v.eq_ignore_ascii_case("cpu"));
        #[cfg(feature = "wgpu")]
        if !force_cpu {
            // The wgpu runtime may panic deep inside on a broken driver; keep that a fallback.
            let gpu = std::panic::catch_unwind(|| burn::tensor::Device::wgpu_options().init_with_setup());
            if let Ok(Ok((device, setup))) = gpu {
                let info = setup.adapter.get_info();
                let software = info.device_type == burn::tensor::wgpu::wgpu::DeviceType::Cpu;
                if !software || cfg!(not(feature = "flex")) {
                    return Ok((device, format!("{:?}: {}", setup.backend, info.name)));
                }
            }
        }
        #[cfg(feature = "flex")]
        {
            let _ = force_cpu;
            Ok((Device::flex(), "CPU".into()))
        }
        #[cfg(not(feature = "flex"))]
        Err(LamaError::Unavailable(if force_cpu {
            "CAPISHOT_AI_DEVICE=cpu but this build has no CPU backend".into()
        } else {
            "no GPU found and this build has no CPU backend".into()
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
        let cpu = req.cpu || std::env::var("CAPISHOT_AI_DEVICE").is_ok_and(|v| v.eq_ignore_ascii_case("cpu"));
        req.model == LAMA
            && !cpu
            && match req.backend {
                GpuBackend::Wgpu => false,
                GpuBackend::DirectMl => true,
                GpuBackend::Auto => self.dml_failed.lock().unwrap_or_else(PoisonError::into_inner).is_none(),
            }
    }

    /// Runs `req` on DirectML when [`Self::wants_directml`]; `None` runs it on burn instead (an
    /// automatic run where DirectML can't: no ONNX Runtime library, a failed session or run).
    #[cfg(all(feature = "directml", windows))]
    fn run_directml(&self, req: &Request) -> Option<Result<Vec<f32>>> {
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
                        if m.as_ref().is_some_and(|(id, cpu, _)| *id == LAMA && !*cpu) {
                            *m = None;
                        }
                        let d = Arc::new(d);
                        *slot = Some(d.clone());
                        d
                    }
                    Err(e) if auto => {
                        *self.dml_failed.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.to_string());
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
            Err(_) if auto => None,
            Err(e) => Some(Err(e)),
        }
    }

    /// Runs a CPU request for Manga LaMa on ONNX Runtime's CPU provider; `None` runs it on
    /// burn's CPU backend instead (no ONNX Runtime library, a failed session or run: the CPU
    /// path never gets worse than burn's).
    #[cfg(all(feature = "directml", any(windows, target_os = "linux")))]
    fn run_ort_cpu(&self, req: &Request) -> Option<Result<Vec<f32>>> {
        let cpu = req.cpu || std::env::var("CAPISHOT_AI_DEVICE").is_ok_and(|v| v.eq_ignore_ascii_case("cpu"));
        if req.model != LAMA || !cpu || *self.ort_cpu_failed.lock().unwrap_or_else(PoisonError::into_inner) {
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
                if m.as_ref().is_some_and(|(id, cpu, _)| *id == LAMA && *cpu) {
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
    fn run_directml(&self, req: &Request) -> Option<Result<Vec<f32>>> {
        (req.backend == GpuBackend::DirectMl && self.wants_directml(req))
            .then(|| Err(LamaError::Unavailable("DirectML runs only in Windows builds with the `directml` feature".into())))
    }

    fn model(&self, id: &'static str, cpu: bool) -> Result<Arc<Generator>> {
        let mut slot = self.model.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((i, c, m)) = slot.as_ref()
            && *i == id
            && *c == cpu
        {
            return Ok(m.clone());
        }
        *slot = None;
        let bytes = self.weights(id)?;
        let (device, name) = Self::pick_device(cpu)?;
        let m = Arc::new(Generator::from_safetensors(&bytes, &device)?);
        *self.device_name.lock().unwrap_or_else(PoisonError::into_inner) = name;
        *slot = Some((id, cpu, m.clone()));
        Ok(m)
    }
}

impl Inpainter for NativeInpainter {
    fn model_state(&self) -> ModelState {
        self.model_state_of(LAMA)
    }

    fn model_state_of(&self, model: &str) -> ModelState {
        let loaded = self.model.lock().unwrap_or_else(PoisonError::into_inner).as_ref().is_some_and(|(i, _, _)| *i == model);
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
            if let Some(r) = self.run_directml(&req).or_else(|| self.run_ort_cpu(&req)) {
                return r;
            }
            self.model(req.model, req.cpu).and_then(|m| {
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
        let on_gpu = !req.cpu && self.model.lock().unwrap_or_else(PoisonError::into_inner).as_ref().is_none_or(|(_, cpu, _)| !*cpu) && self.device() != "CPU";
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
