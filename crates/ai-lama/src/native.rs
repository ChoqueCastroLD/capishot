//! [`NativeInpainter`]: the desktop/CLI [`Inpainter`], running LaMa in the calling thread (the
//! engine's job worker) on the GPU through wgpu when there is a real one, else on the CPU.
//!
//! The weights come from a local file: `CAPISHOT_LAMA_WEIGHTS`, else
//! `<config dir>/models/manga-lama-f16.safetensors`. Downloading them is the shared AI runtime's
//! job (`photocraft-ai`'s cache); until that is wired, a missing file reports
//! [`ModelState::Unavailable`] with the path to put it at.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use burn::tensor::Device;

use crate::service::{Done, Hooks, Inpainter, ModelState, Request};
use crate::{Lama, LamaError, Result};

/// The weights' file name in the models folder.
pub const WEIGHTS_FILE: &str = "manga-lama-f16.safetensors";
/// Larger files are refused before reading (the f32 file is 204 MB).
const MAX_WEIGHTS_BYTES: u64 = 512 << 20;

/// See the module docs.
pub struct NativeInpainter {
    path: PathBuf,
    model: Mutex<Option<Arc<Lama>>>,
    device_name: Mutex<String>,
}

impl NativeInpainter {
    /// An inpainter loading `path` on first use.
    pub fn new(path: PathBuf) -> Self {
        Self { path, model: Mutex::new(None), device_name: Mutex::new(String::new()) }
    }

    /// `CAPISHOT_LAMA_WEIGHTS`, else `<config_dir>/models/` + [`WEIGHTS_FILE`]; `None` when
    /// neither is known.
    pub fn locate(config_dir: Option<&Path>) -> Option<Self> {
        let path = match std::env::var_os("CAPISHOT_LAMA_WEIGHTS") {
            Some(p) => PathBuf::from(p),
            None => config_dir?.join("models").join(WEIGHTS_FILE),
        };
        Some(Self::new(path))
    }

    /// The weights file this inpainter reads.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// GPU through wgpu unless it is a software adapter (lavapipe/llvmpipe are far slower than
    /// the CPU backend) or `CAPISHOT_AI_DEVICE=cpu`; else the CPU backend.
    fn pick_device() -> Result<(Device, String)> {
        let force_cpu = std::env::var("CAPISHOT_AI_DEVICE").is_ok_and(|v| v.eq_ignore_ascii_case("cpu"));
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

    fn model(&self) -> Result<Arc<Lama>> {
        let mut slot = self.model.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(m) = slot.as_ref() {
            return Ok(m.clone());
        }
        let len =
            std::fs::metadata(&self.path).map_err(|e| LamaError::Unavailable(format!("Manga LaMa weights not found at {}: {e}", self.path.display())))?.len();
        if len > MAX_WEIGHTS_BYTES {
            return Err(LamaError::Weights(format!("{} is {len} bytes, more than any LaMa", self.path.display())));
        }
        let bytes = std::fs::read(&self.path).map_err(|e| LamaError::Unavailable(format!("{}: {e}", self.path.display())))?;
        let (device, name) = Self::pick_device()?;
        let m = Arc::new(Lama::from_safetensors(&bytes, &device)?);
        *self.device_name.lock().unwrap_or_else(PoisonError::into_inner) = name;
        *slot = Some(m.clone());
        Ok(m)
    }
}

impl Inpainter for NativeInpainter {
    fn model_state(&self) -> ModelState {
        if self.model.lock().unwrap_or_else(PoisonError::into_inner).is_some() || self.path.is_file() {
            ModelState::Ready
        } else {
            ModelState::Unavailable(format!("Manga LaMa weights not found: put {WEIGHTS_FILE} at {} (or set CAPISHOT_LAMA_WEIGHTS)", self.path.display()))
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
            self.model().and_then(|m| {
                if hooks.cancelled() {
                    return Err(LamaError::Cancelled);
                }
                let t = &req.tile;
                m.generate(&t.rgb, &t.mask, t.w, t.h)
            })
        }));
        let result = match run {
            Ok(r) => r,
            Err(panic) => {
                *self.model.lock().unwrap_or_else(PoisonError::into_inner) = None;
                let why = panic.downcast_ref::<String>().map(String::as_str).or_else(|| panic.downcast_ref::<&str>().copied()).unwrap_or("unknown error");
                Err(LamaError::Device(format!("the inference backend failed: {why}")))
            }
        };
        hooks.progress(1.0, "");
        done(result);
    }
}
