//! The model on ONNX Runtime: DirectML on Windows, the CPU on Windows without it and on Linux.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use ort::ep;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

use crate::regions::Detection;
use crate::{Error, ModelState, Page, TextMasker, pipeline};

/// Very large pages run as overlapping tiles to bound memory.
const TILE_SIZE: usize = 1536;
const TILE_OVERLAP: usize = 128;
const TILE_ABOVE_AREA: usize = 6_500_000;
/// On the CPU a whole page takes several GB: tiles past this area.
const CPU_TILE_ABOVE_AREA: usize = 2_400_000;

/// The file's name in the models folder.
pub const FILE: &str = "manga-text-mask.onnx";

struct Loaded {
    session: Session,
    cpu: bool,
}

/// The mask model, loaded on first use.
pub struct NativeTextMasker {
    path: PathBuf,
    loaded: Mutex<Option<Loaded>>,
    device: Mutex<String>,
}

impl NativeTextMasker {
    pub fn new(path: PathBuf) -> Self {
        NativeTextMasker { path, loaded: Mutex::new(None), device: Mutex::new(String::new()) }
    }

    fn build(path: &Path, cpu: bool) -> Result<Session, String> {
        let env = photocraft_ai_ort::environment()?;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Session, String> {
            let b = Session::builder(env).map_err(|e| e.to_string())?.with_optimization_level(GraphOptimizationLevel::All).map_err(|e| e.to_string())?;
            let mut b = if cpu {
                b.with_memory_pattern(true).map_err(|e| e.to_string())?.with_execution_providers([ep::CPU::default().build().error_on_failure()]).map_err(|e| e.to_string())?
            } else {
                // DirectML wants sequential execution and no memory pattern (its own allocator).
                b.with_memory_pattern(false)
                    .map_err(|e| e.to_string())?
                    .with_parallel_execution(false)
                    .map_err(|e| e.to_string())?
                    .with_execution_providers([ep::DirectML::default().build().error_on_failure()])
                    .map_err(|e| e.to_string())?
            };
            b.commit_from_file(path).map_err(|e| e.to_string())
        }))
        .unwrap_or_else(|_| Err("session creation failed".into()))
    }

    fn load(&self) -> Result<std::sync::MutexGuard<'_, Option<Loaded>>, Error> {
        let mut slot = self.loaded.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            let gpu = if cfg!(windows) && std::env::var("CAPISHOT_AI_DEVICE").map_or(true, |v| !v.eq_ignore_ascii_case("cpu")) {
                Self::build(&self.path, false).ok()
            } else {
                None
            };
            let (session, cpu) = match gpu {
                Some(s) => (s, false),
                None => (Self::build(&self.path, true).map_err(Error::Unavailable)?, true),
            };
            *self.device.lock().unwrap_or_else(PoisonError::into_inner) = if cpu { "CPU" } else { "DirectML" }.to_string();
            *slot = Some(Loaded { session, cpu });
        }
        Ok(slot)
    }
}

/// The logits of `rgb` (`w × h`, both multiples of 32).
fn run(session: &mut Session, rgb: &[u8], w: usize, h: usize) -> Result<Vec<f32>, Error> {
    let err = |e: String| Error::Device(e);
    let x = pipeline::planar(rgb, w, h);
    let dim = |v: usize| i64::try_from(v).map_err(|e| err(e.to_string()));
    let input = Tensor::from_array(([1, 3, dim(h)?, dim(w)?], x)).map_err(|e| err(e.to_string()))?;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Vec<f32>, Error> {
        let out = session.run(ort::inputs!["image" => input]).map_err(|e| err(e.to_string()))?;
        let (_, data) = out["logit"].try_extract_tensor::<f32>().map_err(|e| err(e.to_string()))?;
        Ok(data.to_vec())
    }))
    .unwrap_or_else(|_| Err(err("the run failed".into())))
}

/// `rgb` padded with white to multiples of 32, and its logits cut back.
fn whole(session: &mut Session, rgb: &[u8], w: usize, h: usize) -> Result<Vec<f32>, Error> {
    let (pw, ph) = (w.div_ceil(32) * 32, h.div_ceil(32) * 32);
    let mut canvas = vec![255u8; pw * ph * 3];
    for y in 0..h {
        canvas[y * pw * 3..(y * pw + w) * 3].copy_from_slice(&rgb[y * w * 3..(y * w + w) * 3]);
    }
    let out = run(session, &canvas, pw, ph)?;
    let mut logits = vec![0f32; w * h];
    for y in 0..h {
        logits[y * w..y * w + w].copy_from_slice(&out[y * pw..y * pw + w]);
    }
    Ok(logits)
}

/// Fixed-size tiles, blended with linear ramps over the overlaps.
fn tiled(session: &mut Session, rgb: &[u8], w: usize, h: usize, cancelled: &dyn Fn() -> bool) -> Result<Vec<f32>, Error> {
    let t = TILE_SIZE;
    let mut blend = pipeline::Blend::new(w, h, t, TILE_OVERLAP);
    for &y0 in &pipeline::starts(h, t, TILE_OVERLAP) {
        for &x0 in &pipeline::starts(w, t, TILE_OVERLAP) {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            let out = run(session, &blend.tile(rgb, x0, y0), t, t)?;
            blend.add(&out, x0, y0);
        }
    }
    Ok(blend.logits())
}

impl TextMasker for NativeTextMasker {
    fn model_state(&self) -> ModelState {
        if let Err(e) = photocraft_ai_ort::available() {
            return ModelState::Unavailable(e);
        }
        if self.path.is_file() { ModelState::Ready } else { ModelState::NeedsDownload }
    }

    fn device(&self) -> String {
        self.device.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn detect(&self, page: Page<'_>, cancelled: &dyn Fn() -> bool) -> Result<Detection, Error> {
        let Page { rgb, width: w, height: h } = page;
        if w == 0 || h == 0 || rgb.len() != w * h * 3 {
            return Err(Error::Input("the page's buffer doesn't match its size".into()));
        }
        if !self.path.is_file() {
            return Err(Error::Unavailable(format!("{} isn't downloaded", FILE)));
        }
        let mut slot = self.load()?;
        let Some(Loaded { session, cpu }) = slot.as_mut() else { return Err(Error::Device("no session".into())) };
        let (nw, nh) = pipeline::net_size(w, h);
        let resized = ((nw, nh) != (w, h)).then(|| crate::resize::rgb(rgb, w, h, nw, nh));
        let net = resized.as_deref().unwrap_or(rgb);
        let limit = if *cpu { CPU_TILE_ABOVE_AREA } else { TILE_ABOVE_AREA };
        let logits = if nw * nh > limit { tiled(session, net, nw, nh, cancelled)? } else { whole(session, net, nw, nh)? };
        drop(slot);
        if cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(pipeline::regions(&logits, nw, nh, rgb, w, h))
    }
}
