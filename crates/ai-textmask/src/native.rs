//! The model on ONNX Runtime: DirectML on Windows, the CPU on Windows without it and on Linux.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use ort::ep;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::{RunOptions, Session};
use ort::value::Tensor;

use crate::regions::Detection;
use crate::{Error, ModelState, Page, TextMasker, Timing, pipeline};

/// Very large pages run as overlapping tiles to bound memory.
const TILE_SIZE: usize = 1536;
const TILE_OVERLAP: usize = 128;
const TILE_ABOVE_AREA: usize = 6_500_000;
/// On the CPU a whole page takes several GB: tiles past this area.
const CPU_TILE_ABOVE_AREA: usize = 2_400_000;
/// On DirectML the page always runs as tiles of this size, on a session whose input size is
/// written into the graph ([`fixed_tiles_on`]). A whole page in one run was one long piece of
/// work for the GPU, whose first run (shaders, allocations) on the owner's RTX 3060 Ti was
/// followed by a reset of the GPU that took the app down; the second took 2.6 s. Tiles a quarter
/// the size, with a wide overlap: against the whole page on the CPU, 2 to 7% of the mask's pixels
/// differ (as with the CPU's own tiles) and 96 to 100% of the pieces are found the same.
const DML_TILE: usize = 1024;
const DML_TILE_OVERLAP: usize = 256;
/// Giant mode's half-size pass on DirectML: padded with white to this fixed size (wide, high)
/// when it fits, which a portrait page always does (its height is at most 1300).
const DML_HALF: (usize, usize) = (1024, 1312);
/// A session's CPU allocator is an arena unless told otherwise, and the DirectML session has it
/// for what runs off the GPU. It keeps what its largest run took for as long as the model stays
/// loaded: 3.1 GB after one whole page of 4.8 megapixels (in blocks of up to 1 GB), more after a
/// page of another size, all of it counted against what the machine can commit. An allocation of
/// the app that then fails ends the process. Asked this, a run gives the arena back when it ends.
const SHRINK: (&str, &str) = ("memory.enable_memory_arena_shrinkage", "cpu:0");
/// The model's input sizes (height, width), as its file names them.
const FREE_DIMS: [&str; 2] = ["32*h", "32*w"];

/// The file's name in the models folder: Shin3.
pub const FILE: &str = "manga-text-mask-shin3.onnx";
/// The models it replaced (Shin1, Shin2): nothing reads them any more.
pub const OLD_FILES: &[&str] = &["manga-text-mask.onnx", "manga-text-mask-precise.onnx"];

/// Frees the room of [`OLD_FILES`] in the models folder `dir`.
pub fn forget_old_file(dir: &Path) {
    for f in OLD_FILES {
        let _ = std::fs::remove_file(dir.join(f));
    }
}

/// A session and whether its runs can give their memory back ([`SHRINK`]).
struct Net {
    session: Session,
    shrink: bool,
}

struct Loaded {
    /// Runs any size: on the CPU always; on DirectML built when a page needs it (a fixed session
    /// failed, or a wide page's half-size pass).
    net: Option<Net>,
    /// Fixed input sizes ([`fixed_tiles_on`]): tiles of [`DML_TILE`], and the half-size pass at
    /// [`DML_HALF`].
    tiles: Option<Net>,
    half: Option<Net>,
    cpu: bool,
}

struct Inner {
    path: PathBuf,
    /// Never the GPU.
    cpu: bool,
    /// A run on the GPU failed: the CPU from now on.
    gpu_failed: AtomicBool,
    loaded: Mutex<Option<Loaded>>,
    device: Mutex<String>,
    /// The parts of the last detection.
    timing: Mutex<Option<Timing>>,
    /// [`TextMasker::warm`] was asked.
    warming: AtomicBool,
}

/// The mask model, loaded on first use or by [`TextMasker::warm`].
pub struct NativeTextMasker {
    inner: Arc<Inner>,
}

/// Fixed input sizes: the page as tiles of [`DML_TILE`] and the half-size pass at [`DML_HALF`],
/// each on a session whose input size is written into the graph. On by default on DirectML:
/// with the size known, ONNX Runtime folds the graph's size computations away, so the upscales
/// get constant sizes, DirectML can fuse the whole graph and no page size costs a new compile.
/// `CAPISHOT_TEXTMASK_FIXED_TILES=0` turns it off there (the page in one run, as before);
/// `=1` also turns it on on the CPU, to compare.
fn fixed_tiles_on(cpu: bool) -> bool {
    match std::env::var("CAPISHOT_TEXTMASK_FIXED_TILES").as_deref() {
        Ok("0") => false,
        Ok("1") => true,
        _ => !cpu,
    }
}

/// `CAPISHOT_AI_CPU_ARENA=1`: the CPU session keeps an arena while a run lasts (see [`SHRINK`]).
/// Twice as fast, and more memory for the time of the run: off until it's measured on machines
/// with little memory.
fn cpu_arena_on() -> bool {
    std::env::var_os("CAPISHOT_AI_CPU_ARENA").is_some_and(|v| v == "1")
}

impl NativeTextMasker {
    pub fn new(path: PathBuf) -> Self {
        Self::with(path, false)
    }

    /// Only on the CPU, for a host that asked for that.
    pub fn cpu_only(self, cpu: bool) -> Self {
        Self::with(self.inner.path.clone(), cpu)
    }

    fn with(path: PathBuf, cpu: bool) -> Self {
        let inner = Inner {
            path,
            cpu,
            gpu_failed: AtomicBool::new(false),
            loaded: Mutex::new(None),
            device: Mutex::new(String::new()),
            timing: Mutex::new(None),
            warming: AtomicBool::new(false),
        };
        NativeTextMasker { inner: Arc::new(inner) }
    }
}

impl Inner {
    /// `fixed`: the input is always this size (width, height); its first run, the slow one
    /// (DirectML compiles the graph for the size), is done here, not on a page.
    fn build(path: &Path, cpu: bool, fixed: Option<(usize, usize)>) -> Result<Net, String> {
        let env = photocraft_ai_ort::environment()?;
        let session = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Session, String> {
            let b = Session::builder(env).map_err(|e| e.to_string())?.with_optimization_level(GraphOptimizationLevel::All).map_err(|e| e.to_string())?;
            // On the CPU there is no arena (`ort` turns it off for a provider that doesn't ask):
            // each run takes what it needs and gives it back, at about half the speed. With
            // `CAPISHOT_AI_CPU_ARENA=1` it has one, given back after each run as on DirectML.
            let arena = ep::CPU::default().with_arena_allocator(!cpu || cpu_arena_on()).build();
            let mut b = if cpu {
                b.with_memory_pattern(true).map_err(|e| e.to_string())?.with_execution_providers([arena.error_on_failure()]).map_err(|e| e.to_string())?
            } else {
                // DirectML wants sequential execution and no memory pattern (its own allocator).
                b.with_memory_pattern(false)
                    .map_err(|e| e.to_string())?
                    .with_parallel_execution(false)
                    .map_err(|e| e.to_string())?
                    .with_execution_providers([ep::DirectML::default().build().error_on_failure(), arena])
                    .map_err(|e| e.to_string())?
            };
            if let Some((w, h)) = fixed {
                for (name, size) in FREE_DIMS.into_iter().zip([h, w]) {
                    b = b.with_dimension_override(name, size as i64).map_err(|e| e.to_string())?;
                }
            }
            b.commit_from_file(path).map_err(|e| e.to_string())
        }))
        .unwrap_or_else(|_| Err("session creation failed".into()))?;
        let mut net = Net { session, shrink: !cpu || cpu_arena_on() };
        if let Some((w, h)) = fixed {
            run(&mut net, &vec![255u8; w * h * 3], w, h).map_err(|e| e.to_string())?;
        }
        Ok(net)
    }

    /// The model, loaded now if it wasn't (or once the load under way ends).
    fn load(&self) -> Result<std::sync::MutexGuard<'_, Option<Loaded>>, Error> {
        let mut slot = self.loaded.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            let gpu = !self.cpu
                && !self.gpu_failed.load(Ordering::Relaxed)
                && cfg!(windows)
                && std::env::var("CAPISHOT_AI_DEVICE").map_or(true, |v| !v.eq_ignore_ascii_case("cpu"));
            let loaded = if gpu { Self::load_on(&self.path, false).ok() } else { None };
            let loaded = match loaded {
                Some(l) => l,
                None => Self::load_on(&self.path, true).map_err(Error::Unavailable)?,
            };
            *self.device.lock().unwrap_or_else(PoisonError::into_inner) = if loaded.cpu { "CPU" } else { "DirectML" }.to_string();
            *slot = Some(loaded);
        }
        Ok(slot)
    }

    /// The sessions on one device: the fixed ones when they're on ([`fixed_tiles_on`]) and build,
    /// else the one of any size.
    fn load_on(path: &Path, cpu: bool) -> Result<Loaded, String> {
        let (tiles, half) = if fixed_tiles_on(cpu) {
            let tiles = Self::build(path, cpu, Some((DML_TILE, DML_TILE))).ok();
            let half = tiles.as_ref().and_then(|_| Self::build(path, cpu, Some(DML_HALF)).ok());
            (tiles, half)
        } else {
            (None, None)
        };
        let net = if tiles.is_some() && half.is_some() { None } else { Some(Self::build(path, cpu, None)?) };
        Ok(Loaded { net, tiles, half, cpu })
    }
}

impl Loaded {
    /// The session of any size, built now if it wasn't.
    fn any_size(&mut self, path: &Path) -> Result<&mut Net, Error> {
        if self.net.is_none() {
            self.net = Some(Inner::build(path, self.cpu, None).map_err(Error::Device)?);
        }
        self.net.as_mut().ok_or_else(|| Error::Device("no session".into()))
    }

    /// The logits of the page at its working size (`nw × nh`).
    fn page(&mut self, path: &Path, page: &[u8], nw: usize, nh: usize, cancelled: &dyn Fn() -> bool) -> Result<Vec<f32>, Error> {
        if let Some(t) = self.tiles.as_mut() {
            return tiled(t, page, nw, nh, DML_TILE, DML_TILE_OVERLAP, cancelled);
        }
        let limit = if self.cpu { CPU_TILE_ABOVE_AREA } else { TILE_ABOVE_AREA };
        let net = self.any_size(path)?;
        if nw * nh > limit { tiled(net, page, nw, nh, TILE_SIZE, TILE_OVERLAP, cancelled) } else { whole(net, page, nw, nh) }
    }

    /// The logits of giant mode's half-size pass (`sw × sh`).
    fn half(&mut self, path: &Path, rgb: &[u8], sw: usize, sh: usize) -> Result<Vec<f32>, Error> {
        let (fw, fh) = DML_HALF;
        if let Some(h) = self.half.as_mut().filter(|_| sw <= fw && sh <= fh) {
            return padded_to(h, rgb, sw, sh, fw, fh);
        }
        whole(self.any_size(path)?, rgb, sw, sh)
    }
}

fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The logits of `rgb` (`w × h`, both multiples of 32).
fn run(net: &mut Net, rgb: &[u8], w: usize, h: usize) -> Result<Vec<f32>, Error> {
    match run_once(net, rgb, w, h) {
        // A session that can't give its arena back refuses the run before it starts ("Did not find
        // an arena based allocator ... in the memory arena shrink list"): once more, as it is, and
        // so from now on.
        Err(Error::Device(e)) if net.shrink && (e.contains("arena") || e.contains("shrink")) => {
            net.shrink = false;
            run_once(net, rgb, w, h)
        }
        r => r,
    }
}

fn run_once(net: &mut Net, rgb: &[u8], w: usize, h: usize) -> Result<Vec<f32>, Error> {
    let err = |e: String| Error::Device(e);
    // The input as floats, and the answer.
    pipeline::room_for(w * h * 16)?;
    let x = pipeline::planar(rgb, w, h);
    let dim = |v: usize| i64::try_from(v).map_err(|e| err(e.to_string()));
    let input = Tensor::from_array(([1, 3, dim(h)?, dim(w)?], x)).map_err(|e| err(e.to_string()))?;
    let shrink = net.shrink;
    let session = &mut net.session;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Vec<f32>, Error> {
        let mut options = RunOptions::new().map_err(|e| err(e.to_string()))?;
        if shrink {
            options.set(SHRINK.0, SHRINK.1).map_err(|e| err(e.to_string()))?;
        }
        let out = session.run_with_options(ort::inputs!["image" => input], &options).map_err(|e| err(e.to_string()))?;
        let (_, data) = out["logit"].try_extract_tensor::<f32>().map_err(|e| err(e.to_string()))?;
        Ok(data.to_vec())
    }))
    .unwrap_or_else(|_| Err(err("the run failed".into())))
}

/// `rgb` padded with white to multiples of 32, and its logits cut back.
fn whole(net: &mut Net, rgb: &[u8], w: usize, h: usize) -> Result<Vec<f32>, Error> {
    padded_to(net, rgb, w, h, w.div_ceil(32) * 32, h.div_ceil(32) * 32)
}

/// `rgb` padded with white to `pw × ph`, and its logits cut back.
fn padded_to(net: &mut Net, rgb: &[u8], w: usize, h: usize, pw: usize, ph: usize) -> Result<Vec<f32>, Error> {
    pipeline::room_for(pw * ph * 8)?;
    let canvas = pipeline::padded_to(rgb, w, h, pw, ph);
    Ok(pipeline::unpadded(&run(net, &canvas, pw, ph)?, pw, w, h))
}

/// Tiles of `t` overlapping by `overlap`, blended with linear ramps over the overlaps.
fn tiled(net: &mut Net, rgb: &[u8], w: usize, h: usize, t: usize, overlap: usize, cancelled: &dyn Fn() -> bool) -> Result<Vec<f32>, Error> {
    pipeline::room_for(w * h * 12)?;
    let mut blend = pipeline::Blend::new(w, h, t, overlap);
    for &y0 in &pipeline::starts(h, t, overlap) {
        for &x0 in &pipeline::starts(w, t, overlap) {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            let out = run(net, &blend.tile(rgb, x0, y0), t, t)?;
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
        if self.inner.path.is_file() { ModelState::Ready } else { ModelState::NeedsDownload }
    }

    fn device(&self) -> String {
        self.inner.device.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn timing(&self) -> Option<Timing> {
        *self.inner.timing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn warm(&self) {
        if self.inner.warming.swap(true, Ordering::Relaxed) {
            return;
        }
        let inner = self.inner.clone();
        // A failure shows on the run that needs the model.
        let _ = std::thread::Builder::new().name("capishot-textmask-load".into()).spawn(move || {
            if inner.path.is_file() {
                drop(inner.load());
            }
        });
    }

    fn detect(&self, page: Page<'_>, cancelled: &dyn Fn() -> bool) -> Result<Detection, Error> {
        let Page { rgb, width: w, height: h } = page;
        if w == 0 || h == 0 || rgb.len() != w * h * 3 {
            return Err(Error::Input("the page's buffer doesn't match its size".into()));
        }
        if !self.inner.path.is_file() {
            return Err(Error::Unavailable(format!("{} isn't downloaded", FILE)));
        }
        let started = Instant::now();
        let mut slot = self.inner.load()?;
        // Under a millisecond when it was loaded.
        let mut load_ms = millis(started);
        let started = Instant::now();
        let (nw, nh) = pipeline::net_size(w, h);
        let resized = ((nw, nh) != (w, h)).then(|| crate::resize::rgb(rgb, w, h, nw, nh));
        let page = resized.as_deref().unwrap_or(rgb);
        let (sw, sh) = pipeline::half_size(w, h);
        let path = &self.inner.path;
        let run_all = |l: &mut Loaded| -> Result<(Vec<f32>, Option<Vec<f32>>), Error> {
            let logits = l.page(path, page, nw, nh, cancelled)?;
            // Giant mode: the page once more, in one piece at half its working height.
            let half = if pipeline::GIANT {
                if cancelled() {
                    return Err(Error::Cancelled);
                }
                Some(l.half(path, &crate::resize::rgb(rgb, w, h, sw, sh), sw, sh)?)
            } else {
                None
            };
            Ok((logits, half))
        };
        let Some(loaded) = slot.as_mut() else { return Err(Error::Device("no session".into())) };
        let mut fell_back = false;
        let (logits, half) = match run_all(loaded) {
            // The GPU failed: its sessions go, and the page runs on the CPU, now and from now on.
            Err(Error::Device(_)) if !loaded.cpu => {
                self.inner.gpu_failed.store(true, Ordering::Relaxed);
                *slot = None;
                let reload = Instant::now();
                slot = self.inner.load()?;
                load_ms += millis(reload);
                fell_back = true;
                let Some(loaded) = slot.as_mut() else { return Err(Error::Device("no session".into())) };
                run_all(loaded)?
            }
            r => r?,
        };
        drop(resized);
        let half = half.map(|l| (l, sw, sh));
        drop(slot);
        if cancelled() {
            return Err(Error::Cancelled);
        }
        let infer_ms = millis(started);
        let started = Instant::now();
        pipeline::room_for(w * h * pipeline::REGIONS_BYTES_PER_PIXEL)?;
        let found = pipeline::regions(&logits, nw, nh, half.as_ref().map(|(l, sw, sh)| (l.as_slice(), *sw, *sh)), rgb, w, h);
        *self.inner.timing.lock().unwrap_or_else(PoisonError::into_inner) = Some(Timing { load_ms, infer_ms, regions_ms: millis(started), fell_back });
        Ok(found)
    }
}
