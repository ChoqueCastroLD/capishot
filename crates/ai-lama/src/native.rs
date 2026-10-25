//! [`NativeInpainter`]: the desktop/CLI [`Inpainter`], running LaMa (or the lite MI-GAN) in the
//! calling thread (the engine's job worker) on the GPU through wgpu when there is a real one,
//! else on the CPU.
//!
//! The weights come from local files: the AI Model Manager's downloads (the app's models
//! folder, `photocraft-ai`'s cache) first, else `CAPISHOT_LAMA_WEIGHTS`; MI-GAN from `CAPISHOT_MIGAN_WEIGHTS`, else
//! [`MIGAN_FILE`] next to LaMa's. A missing file reports [`ModelState::NeedsDownload`] when
//! there is a downloads folder (the Manager fetches it), else [`ModelState::Unavailable`] with
//! the path to put it at.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use burn::tensor::Device;

use crate::service::{Done, Hooks, Inpainter, LAMA, MIGAN, ModelState, Request};
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

/// See the module docs.
pub struct NativeInpainter {
    path: PathBuf,
    migan: Option<PathBuf>,
    /// The AI Model Manager's downloads folder, searched first.
    downloads: Option<PathBuf>,
    /// The loaded model: its id, whether it is on the CPU, the generator.
    model: Mutex<Option<(&'static str, bool, Arc<Generator>)>>,
    device_name: Mutex<String>,
}

impl NativeInpainter {
    /// An inpainter loading LaMa from `path` on first use (MI-GAN: [`NativeInpainter::with_migan`]).
    pub fn new(path: PathBuf) -> Self {
        Self { path, migan: None, downloads: None, model: Mutex::new(None), device_name: Mutex::new(String::new()) }
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

    fn model(&self, id: &'static str, cpu: bool) -> Result<Arc<Generator>> {
        let mut slot = self.model.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((i, c, m)) = slot.as_ref()
            && *i == id
            && *c == cpu
        {
            return Ok(m.clone());
        }
        *slot = None;
        let path = self.path_of(id).ok_or_else(|| LamaError::Unavailable(format!("{id} isn't available here")))?;
        let path = path.as_path();
        let len = std::fs::metadata(path).map_err(|e| LamaError::Unavailable(format!("{id} weights not found at {}: {e}", path.display())))?.len();
        if len > MAX_WEIGHTS_BYTES {
            return Err(LamaError::Weights(format!("{} is {len} bytes, more than any inpainting model", path.display())));
        }
        let bytes = std::fs::read(path).map_err(|e| LamaError::Unavailable(format!("{}: {e}", path.display())))?;
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
