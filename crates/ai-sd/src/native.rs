//! [`NativeGenFill`]: the desktop/CLI [`GenFill`], running in the calling thread (the engine's job
//! worker) on the GPU through wgpu when there is a real one, else on the CPU.
//!
//! Files come from the AI Model Manager's downloads (the app's models folder), else
//! `CAPISHOT_SD_DIR`. A missing file reports [`ModelState::NeedsDownload`] when there is a
//! downloads folder, else [`ModelState::Unavailable`] naming the folder.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use burn::tensor::{DType, Device};

use crate::files;
use crate::runner::{Loader, Models, generate, params, precision};
use crate::service::{Done, GenFill, Hooks, ModelState, Request, Tier};
use crate::{Result, SdError};

/// See the module docs.
pub struct NativeGenFill {
    dirs: Vec<PathBuf>,
    downloads: bool,
    loaded: Mutex<Option<(Tier, bool, Arc<Models>)>>,
    device_name: Mutex<String>,
}

impl NativeGenFill {
    /// Looks for files in `dirs` (first match wins); `downloads`: the first dir is the AI Model
    /// Manager's, so missing files can be downloaded.
    pub fn new(dirs: Vec<PathBuf>, downloads: bool) -> Self {
        Self { dirs, downloads, loaded: Mutex::new(None), device_name: Mutex::new(String::new()) }
    }

    /// `models_dir` (the AI Model Manager's downloads) and `CAPISHOT_SD_DIR`.
    pub fn locate(models_dir: Option<&Path>) -> Self {
        let mut dirs = Vec::new();
        let downloads = models_dir.is_some();
        if let Some(m) = models_dir {
            dirs.push(m.to_path_buf());
        }
        if let Some(d) = std::env::var_os("CAPISHOT_SD_DIR") {
            dirs.push(PathBuf::from(d));
        }
        Self::new(dirs, downloads)
    }

    fn find(&self, file: &str) -> Option<PathBuf> {
        self.dirs.iter().map(|d| d.join(file)).find(|p| p.is_file())
    }

    fn pick_device(cpu: bool) -> Result<(Device, String)> {
        let force_cpu = cpu || std::env::var("CAPISHOT_AI_DEVICE").is_ok_and(|v| v.eq_ignore_ascii_case("cpu"));
        #[cfg(feature = "wgpu")]
        if !force_cpu {
            let gpu = std::panic::catch_unwind(|| burn::tensor::Device::wgpu_options().init_with_setup());
            match gpu {
                Ok(Ok((device, setup))) => {
                    let info = setup.adapter.get_info();
                    let software = info.device_type == burn::tensor::wgpu::wgpu::DeviceType::Cpu;
                    if !software || cfg!(not(feature = "flex")) {
                        return Ok((device, format!("{:?}: {}", setup.backend, info.name)));
                    }
                }
                Ok(Err(burn::tensor::wgpu::WgpuInitError::Registration { .. })) => {
                    return Ok((Device::wgpu(burn::tensor::DeviceKind::DefaultDevice), "GPU".into()));
                }
                _ => {}
            }
        }
        #[cfg(feature = "flex")]
        {
            let _ = force_cpu;
            Ok((Device::flex(), "CPU".into()))
        }
        #[cfg(not(feature = "flex"))]
        Err(SdError::Unavailable("no GPU found and this build has no CPU backend".into()))
    }

    fn models(&self, tier: Tier, cpu: bool, f32: bool, hooks: &dyn Hooks) -> Result<Arc<Models>> {
        let mut slot = self.loaded.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((t, c, m)) = slot.as_ref()
            && *t == tier
            && *c == cpu
        {
            return Ok(m.clone());
        }
        *slot = None;
        let list = files::tier(tier).ok_or_else(|| SdError::Unavailable(format!("the {} tier isn't available yet", tier.id())))?;
        let (device, name) = Self::pick_device(cpu)?;
        let dtype = if name == "CPU" || f32 { DType::F32 } else { precision(&device, false) };
        let mut loader = Loader::new(&device, dtype)?;
        let n = list.files.len().max(1);
        for (i, f) in list.files.iter().enumerate() {
            if hooks.cancelled() {
                return Err(SdError::Cancelled);
            }
            hooks.progress(0.1 * i as f32 / n as f32, "Loading Generative Fill");
            let path = self.find(f.file).ok_or_else(|| SdError::Unavailable(format!("{} isn't downloaded", f.file)))?;
            let bytes = std::fs::read(&path).map_err(|e| SdError::Unavailable(format!("{}: {e}", path.display())))?;
            loader.add(&bytes)?;
        }
        let m = Arc::new(loader.finish()?);
        *self.device_name.lock().unwrap_or_else(PoisonError::into_inner) = name;
        *slot = Some((tier, cpu, m.clone()));
        Ok(m)
    }
}

impl GenFill for NativeGenFill {
    fn model_state(&self, tier: Tier) -> ModelState {
        let Some(list) = files::tier(tier) else {
            return ModelState::Unavailable(format!("the {} tier isn't available yet", tier.id()));
        };
        let missing: u64 = list.files.iter().filter(|f| self.find(f.file).is_none()).map(|f| f.bytes).sum();
        if missing == 0 {
            ModelState::Ready
        } else if self.downloads {
            ModelState::NeedsDownload { bytes: missing }
        } else {
            ModelState::Unavailable("Generative Fill's model files aren't installed (set CAPISHOT_SD_DIR)".into())
        }
    }

    fn device(&self) -> String {
        self.device_name.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: Done) {
        if hooks.cancelled() {
            return done(Err(SdError::Cancelled));
        }
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Vec<Vec<f32>>> {
            let m = self.models(req.tier, req.cpu, req.f32, hooks.as_ref())?;
            let p = params(&req, &m);
            let total = p.steps.max(1) as f32;
            let h = hooks.clone();
            let out = pollster::block_on(generate(&m, &p, &mut |d, _| {
                h.progress(0.1 + 0.85 * d as f32 / total, &format!("Generating (step {} of {})", (d + 1).min(total as usize), total as usize));
                !h.cancelled()
            }));
            if req.unload {
                *self.loaded.lock().unwrap_or_else(PoisonError::into_inner) = None;
            }
            out
        }));
        let result = match run {
            Ok(r) => r,
            Err(panic) => {
                *self.loaded.lock().unwrap_or_else(PoisonError::into_inner) = None;
                let why = panic.downcast_ref::<String>().map(String::as_str).or_else(|| panic.downcast_ref::<&str>().copied()).unwrap_or("unknown error");
                Err(SdError::Device(format!("the inference backend failed: {why}")))
            }
        };
        hooks.progress(1.0, "");
        done(result);
    }
}
