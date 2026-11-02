//! Windows: the upscalers (ESRGAN, FDAT and the lite Compact) on the GPU through ONNX Runtime's
//! DirectML execution provider, in fp32, and the same graphs on its CPU provider for CPU runs.
//!
//! burn's convolutions in fp32 run as direct convolutions (no tensor cores, no implicit GEMM),
//! which is what makes MangaJaNai slow on wgpu; DirectML's run several times faster on the same
//! card with the same fp32 arithmetic (the output matches the burn path to about 60 dB PSNR).
//! FDAT (IllustrationJaNai V3) gains about as much: a colour page at ×4 in about 6.5 s against
//! 27 s on burn's wgpu path. The network is exported to ONNX in memory from the same weights
//! ([`crate::onnx`]). Compact runs a grey 1000 × 1500 page at ×2 in about 0.55 s against 5.5 s.
//!
//! On the CPU ([`DmlUpscaler::cpu_threads`]), ONNX Runtime's convolutions are 2–6× faster than
//! burn's CPU backend on the same cores (Compact ×2 on that page: about 22 s against 54 s), with
//! the same tiles as burn's CPU path.
//!
//! ONNX Runtime is not linked: `onnxruntime.dll` (a DirectML build, 1.24 or later) is loaded at
//! run time from `PHOTOCRAFT_AI_ONNXRUNTIME` (the file, or the folder holding it) or from the
//! executable's folder, with `DirectML.dll` from the same folder. Without it [`available`] says
//! why and the burn path runs instead.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use ort::environment::Environment;
use ort::ep;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

use crate::compact::HostCompact;
use crate::esrgan::HostEsrgan;
use crate::fdat::Fdat;
use crate::runtime::reflect_pad;
use crate::tile::{Blender, Plan, Tile, TileOptions, plan, uniform};
use crate::weights::Weights;
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
/// 2.1 GiB measured; 4x at 128: 2.9 GiB; FDAT: one size, 1.1 GiB at ×4). Sizes past it run on
/// a session built for them and dropped after (see [`DmlUpscaler::run_plan`]), so the device
/// holds at most this plus one.
const SESSION_BUDGET: usize = 3 << 30;

/// What an ESRGAN session holds on the device per output pixel (DirectML keeps its buffers for
/// the next run): about 2.2 KB, measured on MangaJaNai 2x and 4x.
const ESRGAN_BYTES_PER_PIXEL: usize = 2250;
/// The same for FDAT: 1.1 GiB for a 222 × 204 tile at ×4 (1.6 KB per output pixel), 0.28 GiB
/// at 128 × 128, measured on IllustrationJaNai V3 detail.
const FDAT_BYTES_PER_PIXEL: usize = 1700;
/// The same for Compact (64 channels at the input resolution): an estimate well above its
/// activations (about 0.2 GiB at the peak for a page at ×2).
const COMPACT_BYTES_PER_PIXEL: usize = 300;

/// What a session holds on the device for `w × h` input pixels at `scale`, at `per_pixel` bytes
/// per output pixel.
fn session_bytes((w, h): (usize, usize), scale: usize, per_pixel: usize) -> usize {
    w.saturating_mul(h).saturating_mul(scale * scale).saturating_mul(per_pixel)
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
fn sessions_bytes(sizes: impl IntoIterator<Item = (usize, usize)>, scale: usize, per_pixel: usize) -> usize {
    sizes.into_iter().map(|s| session_bytes(s, scale, per_pixel)).fold(0, usize::saturating_add)
}

/// The [`Slot`] size of a session with free dimensions (on the CPU, one runs every size).
const ANY_SIZE: (usize, usize) = (0, 0);

/// A session and the one input size it was built for.
struct Slot {
    size: (usize, usize),
    session: Session,
}

/// What a session is built from.
enum Graph {
    /// ESRGAN's or Compact's model bytes, with free input dimensions (`h`, `w`): fixed per
    /// session on DirectML, left free on the CPU (one session runs every size).
    Free(Vec<u8>),
    /// FDAT's weights: its graph is written per input size ([`crate::onnx::fdat`]), every shape
    /// a constant (window padding, reshapes).
    Fdat(Box<Weights>),
}

/// An upscaler loaded into ONNX Runtime sessions on DirectML, one per input size.
///
/// A session built for one input size (fixed dimensions) lets DirectML compile the whole graph
/// as one: 2x MangaJaNai runs a 1000 × 1500 page in 1.9 s that way, against 2.5 s with a free
/// size and 6.6 s when one session runs every size (it re-specialises on each change). An
/// ESRGAN plan has a few tile sizes (two or three widths by two or three heights), so each gets
/// a session, built on first use (about a second), and the tiles stay exactly the burn path's.
/// FDAT's tiles are all grown to one size ([`uniform`], as burn's native GPU path runs them), so
/// a page needs one session (about 2.4 s to build at ×4 for 192-pixel tiles).
pub struct DmlUpscaler {
    env: &'static Environment,
    graph: Graph,
    /// On ONNX Runtime's CPU provider with this many threads (0: one per core), else DirectML.
    cpu: Option<usize>,
    /// Video memory a session holds per output pixel (see [`session_bytes`]).
    per_pixel: usize,
    /// Tiles all run at one size ([`uniform`]).
    uniform: bool,
    /// A session has been built (DirectML takes this graph): one that fails to build before that
    /// is a device that can't run the model, not a tile too large for it.
    built: AtomicBool,
    sessions: Mutex<Vec<Slot>>,
    /// The sizes of the last plan run.
    last: Mutex<Vec<(usize, usize)>>,
    scale: usize,
    in_channels: usize,
    out_channels: usize,
    multiple: usize,
}

impl std::fmt::Debug for DmlUpscaler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DmlUpscaler").field("scale", &self.scale).field("in_channels", &self.in_channels).finish_non_exhaustive()
    }
}

/// A DirectML session for the model bytes (graph optimisation and DirectML compilation), for
/// inputs of `size` (`w`, `h`) only, or of any size; with `cpu`, a session on the CPU provider
/// with that many threads (0: one per core).
fn session(env: &Environment, model: &[u8], size: Option<(usize, usize)>, cpu: Option<usize>) -> Result<Session> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> std::result::Result<Session, String> {
        let mut b = Session::builder(env).map_err(|e| e.to_string())?;
        if let Some((w, h)) = size {
            let dim = |v: usize| i64::try_from(v).map_err(|e| e.to_string());
            b = b.with_dimension_override("h", dim(h)?).map_err(|e| e.to_string())?;
            b = b.with_dimension_override("w", dim(w)?).map_err(|e| e.to_string())?;
        }
        b = b.with_optimization_level(GraphOptimizationLevel::Level3).map_err(|e| e.to_string())?;
        b = match cpu {
            Some(threads) => {
                if threads > 0 {
                    b = b.with_intra_threads(threads).map_err(|e| e.to_string())?;
                }
                b.with_execution_providers([ep::CPU::default().build().error_on_failure()]).map_err(|e| e.to_string())?
            }
            // DirectML wants sequential execution and no memory pattern (its own allocator).
            None => b
                .with_memory_pattern(false)
                .map_err(|e| e.to_string())?
                .with_parallel_execution(false)
                .map_err(|e| e.to_string())?
                .with_execution_providers([ep::DirectML::default().build().error_on_failure()])
                .map_err(|e| e.to_string())?,
        };
        b.commit_from_memory(model).map_err(|e| e.to_string())
    }))
    .unwrap_or_else(|_| Err("session creation failed".into()))
    .map_err(|e| AiError::Device(format!("{}: {e}", if cpu.is_some() { "ONNX Runtime CPU" } else { "DirectML" })))
}

impl DmlUpscaler {
    /// ESRGAN. Builds a session once (so a DirectML that can't take the model fails here, and
    /// the wgpu path runs instead); the sessions that run are built per size as tiles need them.
    /// `cpu`: on ONNX Runtime's CPU provider with that many threads, one session for every size.
    pub(crate) fn esrgan(h: &HostEsrgan, cpu: Option<usize>) -> Result<Self> {
        let c = h.config;
        Self::free(crate::onnx::esrgan(h), cpu, ESRGAN_BYTES_PER_PIXEL, (c.scale(), h.image_in, c.out_nc, c.shuffle))
    }

    /// Real-ESRGAN Compact, as [`Self::esrgan`].
    pub(crate) fn compact(h: &HostCompact, cpu: Option<usize>) -> Result<Self> {
        Self::free(crate::onnx::compact(h), cpu, COMPACT_BYTES_PER_PIXEL, (h.scale, h.channels, h.channels, 1))
    }

    /// A graph with free input dimensions: `(scale, in, out, multiple)`.
    fn free(model: Vec<u8>, cpu: Option<usize>, per_pixel: usize, (scale, in_channels, out_channels, multiple): (usize, usize, usize, usize)) -> Result<Self> {
        let env = environment().map_err(AiError::Device)?;
        let first = session(env, &model, None, cpu)?;
        // On the CPU that session runs every size; DirectML's are built per size.
        let sessions = if cpu.is_some() { vec![Slot { size: ANY_SIZE, session: first }] } else { Vec::new() };
        Ok(Self {
            env,
            graph: Graph::Free(model),
            cpu,
            per_pixel,
            uniform: false,
            built: AtomicBool::new(true),
            sessions: Mutex::new(sessions),
            last: Mutex::new(Vec::new()),
            scale,
            in_channels,
            out_channels,
            multiple,
        })
    }

    /// FDAT (RGB in and out). Checks the weights by writing a small graph; the sessions are
    /// built per tile size as tiles need them (a session for a sample size would cost a second
    /// or two here), and when the first one fails to build (DirectML can't take the model) the
    /// run fails with [`AiError::Device`], so the wgpu path runs it instead.
    pub(crate) fn fdat(w: Weights, cpu: Option<usize>) -> Result<Self> {
        let env = environment().map_err(AiError::Device)?;
        let c = Fdat::config(&w)?;
        crate::onnx::fdat(&w, (8 * c.shuffle, 8 * c.shuffle))?;
        let out_channels = w.shape("upsampler.3.weight").and_then(|s| s.first().copied()).unwrap_or(3);
        Ok(Self {
            env,
            graph: Graph::Fdat(Box::new(w)),
            cpu,
            per_pixel: FDAT_BYTES_PER_PIXEL,
            uniform: true,
            built: AtomicBool::new(false),
            sessions: Mutex::new(Vec::new()),
            last: Mutex::new(Vec::new()),
            scale: c.scale(),
            in_channels: 3,
            out_channels,
            multiple: c.shuffle,
        })
    }

    /// A session for inputs of `size` (`w`, `h`; [`ANY_SIZE`]: free).
    fn build(&self, size: (usize, usize)) -> Result<Session> {
        let fixed = (size != ANY_SIZE).then_some(size);
        let s = match &self.graph {
            Graph::Free(model) => session(self.env, model, fixed, self.cpu)?,
            Graph::Fdat(w) => session(self.env, &crate::onnx::fdat(w, size)?, None, self.cpu)?,
        };
        self.built.store(true, Ordering::Relaxed);
        Ok(s)
    }

    /// The session an input of `size` runs on: its own, or on the CPU the one for every size
    /// (a free-dimension graph).
    fn key(&self, size: (usize, usize)) -> (usize, usize) {
        if self.cpu.is_some() && matches!(self.graph, Graph::Free(_)) { ANY_SIZE } else { size }
    }

    /// On ONNX Runtime's CPU provider: the thread count it was built with (0: one per core).
    pub fn cpu_threads(&self) -> Option<usize> {
        self.cpu
    }

    /// The tiling of a `w × h` input.
    fn plan(&self, w: usize, h: usize, opts: TileOptions) -> Result<Plan> {
        let p = plan(w, h, opts)?;
        Ok(if self.uniform { uniform(&p, w, h) } else { p })
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
        let size = self.key((pw, ph));
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = match sessions.iter().position(|x| x.size == size) {
            Some(i) => sessions.get_mut(i),
            None if spare.as_ref().is_some_and(|x| x.size == size) => spare.as_mut(),
            None => {
                *spare = None;
                let new = Slot { size, session: self.build(size)? };
                if sessions_bytes(sessions.iter().map(|x| x.size).chain([size]), self.scale, self.per_pixel) <= SESSION_BUDGET {
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
        let mut need: Vec<(usize, usize)> = plan_sizes(p, self.multiple).into_iter().map(|s| self.key(s)).collect();
        need.dedup();
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
        if sessions_bytes(held.iter().copied().chain(missing), self.scale, self.per_pixel) > SESSION_BUDGET {
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
        let p = self.plan(input.width, input.height, opts)?;
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
                // No session ever built: DirectML can't take the model (no use splitting).
                Err(e @ AiError::Device(_)) if !self.built.load(Ordering::Relaxed) => return Err(e),
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
pub(crate) mod tests {
    use super::*;

    /// The default plan of the benchmark page keeps a session per size within the budget, at
    /// both scales; a 760-wide page (six sizes) and A4 at 300 dpi run some sizes on a spare.
    #[test]
    fn default_plans_fit_the_session_budget() {
        let per = ESRGAN_BYTES_PER_PIXEL;
        for (scale, tile, multiple) in [(2, 256, 2), (4, 128, 1)] {
            let p = plan(1000, 1500, TileOptions { tile, overlap: 16 }).unwrap();
            let sizes = plan_sizes(&p, multiple);
            assert_eq!(sizes.len(), 4, "{sizes:?}");
            assert!(sessions_bytes(sizes.iter().copied(), scale, per) <= SESSION_BUDGET, "x{scale}: {sizes:?}");
            for (w, h) in [(760, 1200), (2480, 3508)] {
                let sizes = plan_sizes(&plan(w, h, TileOptions { tile, overlap: 16 }).unwrap(), multiple);
                assert!(sizes.len() <= 9, "{sizes:?}");
                // Every size alone fits: a spare never needs more than one session's room.
                assert!(sizes.iter().all(|&s| session_bytes(s, scale, per) <= SESSION_BUDGET / 3), "{w}x{h} x{scale}");
            }
        }
    }

    /// FDAT's tiling (192 at ×4, 256 at ×2, every tile grown to one size): one session per
    /// page, and two pages' sizes fit the budget together.
    #[test]
    fn fdat_plans_take_one_session() {
        let per = FDAT_BYTES_PER_PIXEL;
        for (scale, tile, multiple) in [(4, 192, 1), (2, 256, 2)] {
            for (w, h) in [(760, 1200), (1000, 1500), (2480, 3508)] {
                let p = plan(w, h, TileOptions { tile, overlap: 16 }).unwrap();
                let sizes = plan_sizes(&uniform(&p, w, h), multiple);
                assert_eq!(sizes.len(), 1, "{w}x{h} x{scale}: {sizes:?}");
                assert!(session_bytes(sizes[0], scale, per) <= SESSION_BUDGET / 2, "{w}x{h} x{scale}: {sizes:?}");
            }
        }
    }

    /// Tiny random FDAT weights (dim 8, one group of a spatial and a channel block), ×4 or the
    /// ×2 unshuffle release's layout. The spatial block gets a qkv bias, so the zero-padded
    /// window tokens aren't zero after it (the case the graph must reproduce unmasked).
    pub(crate) fn tiny_fdat(shuffle: usize) -> Weights {
        let (d, hidden, red, n) = (8usize, 16usize, 2usize, 64usize);
        let mut seed = 7u32;
        let mut rnd = |len: usize, amp: f32| -> Vec<f32> {
            (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 2.0 * amp
                })
                .collect()
        };
        let mut t: Vec<(String, Vec<usize>, Vec<f32>)> = Vec::new();
        let mut add = |name: String, shape: Vec<usize>, amp: f32| {
            let len = shape.iter().product();
            t.push((name, shape, rnd(len, amp)));
        };
        let first = if shuffle > 1 { "conv_first.1" } else { "conv_first" };
        add(format!("{first}.weight"), vec![d, 3 * shuffle * shuffle, 3, 3], 0.3);
        add(format!("{first}.bias"), vec![d], 0.1);
        for b in 0..2 {
            let p = format!("groups.0.blocks.{b}");
            for nm in ["n1", "n2"] {
                add(format!("{p}.{nm}.weight"), vec![d], 1.0);
                add(format!("{p}.{nm}.bias"), vec![d], 0.2);
            }
            add(format!("{p}.attn.qkv.weight"), vec![3 * d, d], 0.5);
            add(format!("{p}.attn.proj.weight"), vec![d, d], 0.4);
            add(format!("{p}.attn.proj.bias"), vec![d], 0.1);
            if b == 0 {
                add(format!("{p}.attn.qkv.bias"), vec![3 * d], 0.3);
                add(format!("{p}.attn.bias"), vec![4, n, n], 0.5);
            } else {
                add(format!("{p}.attn.temp"), vec![4, 1, 1], 2.0);
            }
            add(format!("{p}.conv.0.weight"), vec![d, 1, 3, 3], 0.4);
            add(format!("{p}.inter.sg.0.weight"), vec![1, d, 1, 1], 0.5);
            add(format!("{p}.inter.cg.1.weight"), vec![red, d, 1, 1], 0.5);
            add(format!("{p}.inter.cg.3.weight"), vec![d, red, 1, 1], 0.5);
            add(format!("{p}.ffn.fc1.weight"), vec![hidden, d], 0.4);
            add(format!("{p}.ffn.fc2.weight"), vec![d, hidden], 0.3);
            add(format!("{p}.ffn.smix.weight"), vec![hidden, 1, 3, 3], 0.4);
        }
        add("groups.0.conv.weight".into(), vec![d, d, 3, 3], 0.1);
        add("conv_after.weight".into(), vec![d, d, 3, 3], 0.1);
        add("upsampler.0.weight".into(), vec![d, d, 4, 4], 0.2);
        add("upsampler.0.bias".into(), vec![d], 0.1);
        add("upsampler.2.weight".into(), vec![d, 3, 4, 4], 0.2);
        add("upsampler.2.bias".into(), vec![3], 0.1);
        add("upsampler.3.weight".into(), vec![3, 3, 3, 3], 0.3);
        add("upsampler.3.bias".into(), vec![3], 0.1);
        let refs: Vec<(&str, &[usize], &[f32])> = t.iter().map(|(n, s, v)| (n.as_str(), s.as_slice(), v.as_slice())).collect();
        Weights::from_safetensors(&crate::weights::to_safetensors(&refs, false, &[])).unwrap()
    }

    /// burn's FDAT on the CPU for `img`.
    fn burn_fdat(w: &Weights, img: &Planar) -> Planar {
        use crate::runtime::{Network, Runtime, block_on};
        let rt = Runtime::cpu();
        let net = Fdat::load(&rt, w).unwrap();
        block_on(rt.read(net.forward(rt.image(img).unwrap()))).unwrap()
    }

    fn pattern(w: usize, h: usize) -> Planar {
        Planar::new(w, h, 3, (0..w * h * 3).map(|i| (i * 37 % 101) as f32 / 101.0).collect()).unwrap()
    }

    /// The FDAT graph on DirectML against burn on the CPU, at ×4 and ×2 (unshuffle), on sizes
    /// that aren't multiples of the window (padded windows, cropped back), with a second size in
    /// the same model (a second session) and tiled. Skipped without ONNX Runtime.
    #[test]
    fn tiny_fdat_matches_burn() {
        if let Err(e) = available() {
            return eprintln!("ONNX Runtime with DirectML not available ({e}): skipped");
        }
        for (shuffle, cpu) in [(1, None), (2, None), (1, Some(0)), (2, Some(2))] {
            let w = tiny_fdat(shuffle);
            let dml = DmlUpscaler::fdat(w.clone(), cpu).unwrap();
            assert_eq!((dml.scale(), dml.in_channels(), dml.out_channels()), (4 / shuffle, 3, 3));
            for (iw, ih) in [(22, 14), (16, 30)] {
                let img = pattern(iw, ih);
                let want = burn_fdat(&w, &img);
                let mut spare = None;
                let got = dml.run(&img, &mut spare).unwrap();
                assert_eq!((got.width, got.height, got.channels), (want.width, want.height, want.channels));
                let max = got.data.iter().zip(&want.data).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                assert!(max < 1e-4, "x{} {iw}x{ih}: off by {max}", 4 / shuffle);
            }
            // Tiled: every tile grown to one size, blended as burn's tiles would be.
            let img = pattern(42, 36);
            let opts = TileOptions { tile: 20, overlap: 4 };
            let p = uniform(&plan(img.width, img.height, opts).unwrap(), img.width, img.height);
            assert_eq!(plan_sizes(&p, shuffle).len(), 1);
            let mut blend = Blender::new(img.width, img.height, 3, 4 / shuffle, &p).unwrap();
            for t in &p.tiles {
                blend.add(t, &burn_fdat(&w, &img.crop(t.x0, t.y0, t.width(), t.height()))).unwrap();
            }
            let want = blend.finish();
            let got = dml.run_tiled(&img, opts, &crate::NoProgress).unwrap();
            assert_eq!(got.data.len(), want.data.len());
            let max = got.data.iter().zip(&want.data).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(max < 1e-4, "x{} tiled: off by {max}", 4 / shuffle);
        }
    }

    /// ESRGAN (grey with unshuffle, RGB) and Compact (×4, ×2) on DirectML and on ONNX Runtime's
    /// CPU provider against burn's tiled CPU run with the same tiles. Skipped without ONNX
    /// Runtime.
    #[test]
    fn tiny_esrgan_and_compact_match_burn() {
        use crate::compact::Compact;
        use crate::esrgan::Esrgan;
        use crate::runtime::{Network, Runtime, block_on, run_tiled};
        if let Err(e) = available() {
            return eprintln!("ONNX Runtime with DirectML not available ({e}): skipped");
        }
        let rt = Runtime::cpu();
        let gray = |w: usize, h: usize| Planar::new(w, h, 1, (0..w * h).map(|i| (i * 53 % 97) as f32 / 97.0).collect()).unwrap();
        let esrgan = crate::esrgan::tests::tiny(12, "2");
        let rgb = crate::esrgan::tests::tiny(3, "1");
        let compact = crate::compact::tests::tiny();
        let opts = TileOptions { tile: 24, overlap: 4 };
        for cpu in [None, Some(0), Some(3)] {
            let nets: Vec<(&str, Box<dyn Network>, DmlUpscaler, Planar)> = vec![
                (
                    "esrgan gray",
                    Box::new(Esrgan::load(&rt, &esrgan, true).unwrap()),
                    DmlUpscaler::esrgan(&Esrgan::host(&esrgan, true).unwrap(), cpu).unwrap(),
                    gray(46, 30),
                ),
                (
                    "esrgan rgb",
                    Box::new(Esrgan::load(&rt, &rgb, false).unwrap()),
                    DmlUpscaler::esrgan(&Esrgan::host(&rgb, false).unwrap(), cpu).unwrap(),
                    pattern(41, 29),
                ),
                (
                    "compact x4",
                    Box::new(Compact::load(&rt, &compact, 4, false).unwrap()),
                    DmlUpscaler::compact(&Compact::host(&compact, 4, false).unwrap(), cpu).unwrap(),
                    pattern(37, 26),
                ),
                (
                    "compact x2 gray",
                    Box::new(Compact::load(&rt, &compact, 2, true).unwrap()),
                    DmlUpscaler::compact(&Compact::host(&compact, 2, true).unwrap(), cpu).unwrap(),
                    gray(50, 33),
                ),
            ];
            for (name, burn, ort, img) in nets {
                assert_eq!((ort.scale(), ort.in_channels(), ort.out_channels()), (burn.scale(), burn.in_channels(), burn.out_channels()), "{name}");
                assert_eq!(ort.cpu_threads(), cpu);
                let want = block_on(run_tiled(&rt, burn.as_ref(), &img, opts, &crate::NoProgress)).unwrap();
                // Twice: the second page reuses the sessions (one for every size on the CPU).
                for _ in 0..2 {
                    let got = ort.run_tiled(&img, opts, &crate::NoProgress).unwrap();
                    assert_eq!((got.width, got.height, got.channels), (want.width, want.height, want.channels), "{name}");
                    let max = got.data.iter().zip(&want.data).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                    assert!(max < 1e-4, "{name} {cpu:?}: off by {max}");
                }
                if cpu.is_some() {
                    assert_eq!(ort.sessions.lock().unwrap().len(), 1, "{name}: one CPU session for every size");
                }
            }
        }
    }

    /// The real IllustrationJaNai V3 detail weights on a crop against burn's fp32 CPU output,
    /// with the warm DirectML time per tile: `CAPISHOT_FDAT_DML=<model.safetensors>[:<crop.png>]`
    /// (a 192 × 192 pattern without a crop). Ignored: run with `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_fdat_matches_burn() {
        let Ok(spec) = std::env::var("CAPISHOT_FDAT_DML") else { return eprintln!("CAPISHOT_FDAT_DML unset: skipped") };
        available().unwrap();
        let (model, crop) = match spec.rsplit_once(".safetensors:") {
            Some((m, c)) => (format!("{m}.safetensors"), Some(c.to_string())),
            None => (spec.clone(), None),
        };
        let w = Weights::from_safetensors(&std::fs::read(&model).unwrap()).unwrap();
        let img = match crop {
            Some(p) => {
                let im = ::image::open(p).unwrap().to_rgb8();
                Planar::from_interleaved_u8(im.width() as usize, im.height() as usize, 3, im.as_raw()).unwrap()
            }
            None => pattern(192, 192),
        };
        let dml = DmlUpscaler::fdat(w.clone(), None).unwrap();
        let mut spare = None;
        let t = std::time::Instant::now();
        let got = dml.run(&img, &mut spare).unwrap();
        let first = t.elapsed();
        let mut warm = std::time::Duration::MAX;
        for _ in 0..4 {
            let t = std::time::Instant::now();
            dml.run(&img, &mut spare).unwrap();
            warm = warm.min(t.elapsed());
        }
        let t = std::time::Instant::now();
        let want = burn_fdat(&w, &img);
        let burn = t.elapsed();
        let clamp = |v: f32| v.clamp(0.0, 1.0);
        let max = got.data.iter().zip(&want.data).map(|(a, b)| (clamp(*a) - clamp(*b)).abs()).fold(0.0f32, f32::max);
        let mse = got.data.iter().zip(&want.data).map(|(a, b)| f64::from((clamp(*a) - clamp(*b)) * 255.0).powi(2)).sum::<f64>() / got.data.len() as f64;
        let level = |v: f32| (clamp(v) * 255.0).round();
        let levels = got.data.iter().zip(&want.data).map(|(a, b)| (level(*a) - level(*b)).abs()).fold(0.0f32, f32::max);
        let psnr = 10.0 * (255.0f64 * 255.0 / mse.max(1e-30)).log10();
        eprintln!(
            "{}x{}: max diff {max:.2e} ({levels} levels), PSNR {psnr:.1} dB; DirectML first run {first:.2?} (session included), warm {warm:.2?}; burn CPU {burn:.2?}",
            img.width, img.height
        );
        assert!(psnr >= 40.0, "PSNR {psnr:.1} dB");
    }
}
