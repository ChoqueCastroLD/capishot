//! Windows: ESRGAN on the GPU through ONNX Runtime's DirectML execution provider, in fp32.
//!
//! burn's convolutions in fp32 run as direct convolutions (no tensor cores, no implicit GEMM),
//! which is what makes MangaJaNai slow on wgpu; DirectML's run several times faster on the same
//! card with the same fp32 arithmetic (the output matches the burn path to about 60 dB PSNR).
//! The network is exported to ONNX in memory from the same weights ([`crate::onnx`]).
//!
//! ONNX Runtime is not linked: `onnxruntime.dll` (a DirectML build, 1.24 or later) is loaded at
//! run time from `PHOTOCRAFT_AI_ONNXRUNTIME` (the file, or the folder holding it) or from the
//! executable's folder, with `DirectML.dll` from the same folder. Without it [`available`] says
//! why and the burn path runs instead.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use ort::environment::Environment;
use ort::ep;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

use crate::esrgan::HostEsrgan;
use crate::runtime::reflect_pad;
use crate::tile::{Blender, Plan, Tile, TileOptions, plan};
use crate::{AiError, Planar, Progress, Result};

/// Tiles no smaller than this are split in four when they fail (out of device memory).
const MIN_SPLIT: usize = 64;

/// The ONNX Runtime library to load: `PHOTOCRAFT_AI_ONNXRUNTIME` (a file or a folder), else
/// `onnxruntime.dll` next to the executable.
fn library() -> Option<PathBuf> {
    let file = |p: PathBuf| if p.is_dir() { p.join("onnxruntime.dll") } else { p };
    if let Some(p) = std::env::var_os("PHOTOCRAFT_AI_ONNXRUNTIME").filter(|p| !p.is_empty()) {
        return Some(file(PathBuf::from(p)));
    }
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join("onnxruntime.dll");
    p.is_file().then_some(p)
}

static ENV: OnceLock<std::result::Result<Environment, String>> = OnceLock::new();

fn environment() -> std::result::Result<&'static Environment, String> {
    ENV.get_or_init(|| {
        let path = library().ok_or_else(|| "onnxruntime.dll not found (next to the executable or in PHOTOCRAFT_AI_ONNXRUNTIME)".to_string())?;
        load(&path)
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn load(path: &Path) -> std::result::Result<Environment, String> {
    if !path.is_file() {
        return Err(format!("{} doesn't exist", path.display()));
    }
    // Windows ships an older DirectML.dll in System32: the one next to ONNX Runtime goes first.
    let dml = path.with_file_name("DirectML.dll");
    if dml.is_file() {
        ort::util::preload_dylib(&dml).map_err(|e| format!("{}: {e}", dml.display()))?;
    }
    // A broken or mismatched library must not take the app down.
    std::panic::catch_unwind(|| -> std::result::Result<Environment, String> {
        ort::init_from(path).map_err(|e| e.to_string())?.with_name("capishot").with_telemetry(false).build().map_err(|e| e.to_string())
    })
    .unwrap_or_else(|_| Err(format!("{} failed to initialise", path.display())))
}

/// Whether the DirectML backend can run here (the library loads), or why not.
pub fn available() -> std::result::Result<(), String> {
    environment().map(|_| ())
}

fn err(e: impl std::fmt::Display) -> AiError {
    AiError::Inference(format!("ONNX Runtime: {e}"))
}

/// Video memory the sessions kept for one model may hold together, by [`session_bytes`]'
/// estimate: the default plan of a 1000 × 1500 page fits (2x MangaJaNai at 256: four sizes,
/// 2.1 GiB measured; 4x at 128: 2.9 GiB). Sizes past it run on a session built for them and
/// dropped after (see [`DmlEsrgan::run_plan`]), so the device holds at most this plus one.
const SESSION_BUDGET: usize = 3 << 30;

/// What a session holds on the device for `w × h` input pixels at `scale` (DirectML keeps its
/// buffers for the next run): about 2.2 KB per output pixel, measured on MangaJaNai 2x and 4x.
fn session_bytes((w, h): (usize, usize), scale: usize) -> usize {
    w.saturating_mul(h).saturating_mul(scale * scale).saturating_mul(2250)
}

/// The input sizes a plan's tiles run at (padded to the unshuffle `multiple`), each once.
fn plan_sizes(p: &Plan, multiple: usize) -> Vec<(usize, usize)> {
    let m = multiple.max(1);
    let mut v: Vec<(usize, usize)> = p.tiles.iter().map(|t| padded(t, m)).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The input size tile `t` runs at: its sides rounded up to `m`.
fn padded(t: &Tile, m: usize) -> (usize, usize) {
    (t.width().div_ceil(m) * m, t.height().div_ceil(m) * m)
}

/// [`session_bytes`] summed over sessions for these sizes.
fn sessions_bytes(sizes: impl IntoIterator<Item = (usize, usize)>, scale: usize) -> usize {
    sizes.into_iter().map(|s| session_bytes(s, scale)).fold(0, usize::saturating_add)
}

/// A session and the one input size it was built for.
struct Slot {
    size: (usize, usize),
    session: Session,
}

/// ESRGAN loaded into ONNX Runtime sessions on DirectML, one per input size.
///
/// A session built for one input size (fixed dimensions) lets DirectML compile the whole graph
/// as one: 2x MangaJaNai runs a 1000 × 1500 page in 1.9 s that way, against 2.5 s with a free
/// size and 6.6 s when one session runs every size (it re-specialises on each change). A plan
/// has a few tile sizes (two or three widths by two or three heights), so each gets a session,
/// built on first use (about a second), and the tiles stay exactly the wgpu path's.
pub struct DmlEsrgan {
    env: &'static Environment,
    model: Vec<u8>,
    sessions: Mutex<Vec<Slot>>,
    /// The sizes of the last plan run.
    last: Mutex<Vec<(usize, usize)>>,
    scale: usize,
    in_channels: usize,
    out_channels: usize,
    multiple: usize,
}

impl std::fmt::Debug for DmlEsrgan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DmlEsrgan").field("scale", &self.scale).field("in_channels", &self.in_channels).finish_non_exhaustive()
    }
}

/// A DirectML session for the model bytes (graph optimisation and DirectML compilation), for
/// inputs of `size` (`w`, `h`) only, or of any size.
fn session(env: &Environment, model: &[u8], size: Option<(usize, usize)>) -> Result<Session> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> std::result::Result<Session, String> {
        let mut b = Session::builder(env).map_err(|e| e.to_string())?;
        if let Some((w, h)) = size {
            let dim = |v: usize| i64::try_from(v).map_err(|e| e.to_string());
            b = b.with_dimension_override("h", dim(h)?).map_err(|e| e.to_string())?;
            b = b.with_dimension_override("w", dim(w)?).map_err(|e| e.to_string())?;
        }
        // DirectML wants sequential execution and no memory pattern (its own allocator).
        b.with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| e.to_string())?
            .with_memory_pattern(false)
            .map_err(|e| e.to_string())?
            .with_parallel_execution(false)
            .map_err(|e| e.to_string())?
            .with_execution_providers([ep::DirectML::default().build().error_on_failure()])
            .map_err(|e| e.to_string())?
            .commit_from_memory(model)
            .map_err(|e| e.to_string())
    }))
    .unwrap_or_else(|_| Err("session creation failed".into()))
    .map_err(|e| AiError::Device(format!("DirectML: {e}")))
}

impl DmlEsrgan {
    /// Builds a session once (so a DirectML that can't take the model fails here, and the wgpu
    /// path runs instead); the sessions that run are built per size as tiles need them.
    pub(crate) fn load(h: &HostEsrgan) -> Result<Self> {
        let env = environment().map_err(AiError::Device)?;
        let model = crate::onnx::esrgan(h);
        drop(session(env, &model, None)?);
        Ok(Self {
            env,
            model,
            sessions: Mutex::new(Vec::new()),
            last: Mutex::new(Vec::new()),
            scale: h.config.scale(),
            in_channels: h.image_in,
            out_channels: h.config.out_nc,
            multiple: h.config.shuffle,
        })
    }

    pub fn scale(&self) -> usize {
        self.scale
    }
    pub fn in_channels(&self) -> usize {
        self.in_channels
    }
    pub fn out_channels(&self) -> usize {
        self.out_channels
    }

    /// One tile through the network: reflect-padded to the unshuffle multiple, the output
    /// cropped back. Runs on the kept session for its size, else on `spare` if built for it,
    /// else on a new session: kept if it fits [`SESSION_BUDGET`] beside the others, else put in
    /// `spare` (dropping the one there).
    fn run(&self, crop: &Planar, spare: &mut Option<Slot>) -> Result<Planar> {
        let m = self.multiple.max(1);
        let (pw, ph) = (crop.width.div_ceil(m) * m, crop.height.div_ceil(m) * m);
        let padded = reflect_pad(crop, pw, ph);
        let shape = [1i64, padded.channels as i64, ph as i64, pw as i64];
        let input = Tensor::from_array((shape, padded.data)).map_err(err)?;
        let size = (pw, ph);
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = match sessions.iter().position(|x| x.size == size) {
            Some(i) => sessions.get_mut(i),
            None if spare.as_ref().is_some_and(|x| x.size == size) => spare.as_mut(),
            None => {
                *spare = None;
                let new = Slot { size, session: session(self.env, &self.model, Some(size))? };
                if sessions_bytes(sessions.iter().map(|x| x.size).chain([size]), self.scale) <= SESSION_BUDGET {
                    sessions.push(new);
                    sessions.last_mut()
                } else {
                    Some(spare.insert(new))
                }
            }
        };
        let session = &mut slot.ok_or_else(|| err("no session"))?.session;
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Vec<f32>> {
            let outputs = session.run(ort::inputs![crate::onnx::INPUT => input]).map_err(err)?;
            let y = outputs.get(crate::onnx::OUTPUT).ok_or_else(|| err("no output"))?;
            let (_, data) = y.try_extract_tensor::<f32>().map_err(err)?;
            Ok(data.to_vec())
        }))
        .unwrap_or_else(|_| Err(err("the run failed")))?;
        let s = self.scale;
        let out = Planar::new(pw * s, ph * s, self.out_channels, out)?;
        Ok(if pw != crop.width || ph != crop.height { out.crop(0, 0, crop.width * s, crop.height * s) } else { out })
    }

    /// Drops the kept sessions of sizes `p` doesn't use when the ones it needs wouldn't fit
    /// [`SESSION_BUDGET`] beside them and the last plan had the same sizes: a run of pages of a
    /// new size takes the room, a single page of another size (a colour cover among grey pages)
    /// runs on spares and leaves the others' sessions.
    fn make_room(&self, p: &Plan) {
        let need = plan_sizes(p, self.multiple);
        let repeated = {
            let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
            std::mem::replace(&mut *last, need.clone()) == need
        };
        if !repeated {
            return;
        }
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let held: Vec<(usize, usize)> = sessions.iter().map(|x| x.size).collect();
        let missing = need.iter().copied().filter(|s| !held.contains(s));
        if sessions_bytes(held.iter().copied().chain(missing), self.scale) > SESSION_BUDGET {
            sessions.retain(|x| need.contains(&x.size));
        }
    }

    /// Runs the network over `input` (the network's channels, see [`Self::in_channels`]) tile by
    /// tile with blended overlaps, as [`crate::runtime::run_tiled`] does: progress per tile,
    /// cancellation between tiles, and a tile that fails (out of device memory) run again as
    /// four smaller ones.
    pub fn run_tiled(&self, input: &Planar, opts: TileOptions, progress: &dyn Progress) -> Result<Planar> {
        if input.channels != self.in_channels {
            return Err(AiError::Image(format!("{} channels for a {}-channel network", input.channels, self.in_channels)));
        }
        self.run_plan(input, opts, progress, true)
    }

    /// The wgpu path's tiles (so its output), run grouped by size: each size's tiles on one
    /// session, and a size that doesn't fit the budget on a spare session for its group only.
    /// The blend adds the tiles up, so the order doesn't matter.
    fn run_plan(&self, input: &Planar, opts: TileOptions, progress: &dyn Progress, report: bool) -> Result<Planar> {
        let p = plan(input.width, input.height, opts)?;
        self.make_room(&p);
        let m = self.multiple.max(1);
        let mut order: Vec<&Tile> = p.tiles.iter().collect();
        order.sort_by_key(|t| padded(t, m));
        let mut blend = Blender::new(input.width, input.height, self.out_channels, self.scale, &p)?;
        let total = p.tiles.len() as u64;
        if report {
            progress.report(0, total, "");
        }
        let mut spare = None;
        for (i, t) in order.into_iter().enumerate() {
            if progress.cancelled() {
                return Err(AiError::Cancelled);
            }
            let crop = input.crop(t.x0, t.y0, t.width(), t.height());
            let out = match self.run(&crop, &mut spare) {
                Ok(o) => o,
                Err(e) if crop.width.max(crop.height) > MIN_SPLIT => {
                    spare = None;
                    let half = crop.width.max(crop.height).div_ceil(2);
                    let sub = TileOptions { tile: half, overlap: opts.overlap.min(half / 2) };
                    if report {
                        progress.report(i as u64, total, &format!("a tile failed on the device ({e}): splitting it"));
                    }
                    self.run_plan(&crop, sub, progress, false)?
                }
                Err(e) => return Err(e),
            };
            blend.add(t, &out)?;
            if report {
                progress.report(i as u64 + 1, total, "");
            }
        }
        Ok(blend.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default plan of the benchmark page keeps a session per size within the budget, at
    /// both scales; a 760-wide page (six sizes) and A4 at 300 dpi run some sizes on a spare.
    #[test]
    fn default_plans_fit_the_session_budget() {
        for (scale, tile, multiple) in [(2, 256, 2), (4, 128, 1)] {
            let p = plan(1000, 1500, TileOptions { tile, overlap: 16 }).unwrap();
            let sizes = plan_sizes(&p, multiple);
            assert_eq!(sizes.len(), 4, "{sizes:?}");
            assert!(sessions_bytes(sizes.iter().copied(), scale) <= SESSION_BUDGET, "x{scale}: {sizes:?}");
            for (w, h) in [(760, 1200), (2480, 3508)] {
                let sizes = plan_sizes(&plan(w, h, TileOptions { tile, overlap: 16 }).unwrap(), multiple);
                assert!(sizes.len() <= 9, "{sizes:?}");
                // Every size alone fits: a spare never needs more than one session's room.
                assert!(sizes.iter().all(|&s| session_bytes(s, scale) <= SESSION_BUDGET / 3), "{w}x{h} x{scale}");
            }
        }
    }
}
