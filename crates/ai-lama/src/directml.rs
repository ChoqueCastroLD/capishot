//! Windows: Manga LaMa on the GPU through ONNX Runtime's DirectML execution provider, in fp32.
//!
//! burn's fp32 convolutions on wgpu run as direct convolutions (no implicit GEMM, no tensor
//! cores); DirectML's are 13–19× faster on the same card with the same fp32 arithmetic and the
//! same weights (a 512² crop in 0.09 s instead of 1.7 s, 1000² in 0.5 s instead of 6.8 s; the
//! output matches burn's to about 1/255 at most). The generator is written to ONNX in memory ([`crate::onnx`]) with the DFT matrices
//! as inputs, so one session takes every tile size. A size the session hasn't run yet costs
//! DirectML a re-specialisation (about 0.3–0.5 s), still far below the burn run.
//!
//! ONNX Runtime is not linked: `onnxruntime.dll` (a DirectML build, 1.24 or later) is loaded at
//! run time from `PHOTOCRAFT_AI_ONNXRUNTIME` (the file, or the folder holding it) or from the
//! executable's folder, with `DirectML.dll` from the same folder (the same library and lookup as
//! `photocraft_ai`'s upscaler, which shares the process's ONNX Runtime). Without it
//! [`available`] says why and the burn path runs instead.
//!
//! Linux: only the CPU provider, from the official CPU build's `libonnxruntime.so` (1.24 or
//! later) found the same way; the GPU stays on burn's wgpu (Vulkan) path.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use ort::ep;
use ort::session::{RunOptions, Session};
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;
use photocraft_ai_ort::environment;

use crate::dft::Dft2;
use crate::model::planar_input;
use crate::onnx::{INPUT, OUTPUT, dft_inputs};
use crate::{LamaError, Result};

/// Whether the DirectML backend can run here (the library loads), or why not.
pub fn available() -> std::result::Result<(), String> {
    photocraft_ai_ort::available()
}

fn err(e: impl std::fmt::Display) -> LamaError {
    LamaError::Device(format!("ONNX Runtime: {e}"))
}

/// The DirectML session's CPU allocator is an arena that keeps what its largest run took for as
/// long as the model stays loaded, in blocks that double. Asked this, a run gives it back when it
/// ends. The CPU session has no arena (`ort` turns it off for a provider that doesn't ask), unless
/// `CAPISHOT_AI_CPU_ARENA=1` gives it one.
const SHRINK: (&str, &str) = ("memory.enable_memory_arena_shrinkage", "cpu:0");

fn cpu_arena_on() -> bool {
    std::env::var_os("CAPISHOT_AI_CPU_ARENA").is_some_and(|v| v == "1")
}

/// Manga LaMa in one ONNX Runtime session on DirectML.
pub struct DmlLama {
    session: Mutex<Session>,
    /// Runs ask for [`SHRINK`] (the session has the arena).
    shrink: AtomicBool,
}

impl std::fmt::Debug for DmlLama {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DmlLama").finish_non_exhaustive()
    }
}

impl DmlLama {
    /// Parses the weights (a `lama-ffc` safetensors file), writes the ONNX graph and builds the
    /// session (about a second). Fails (and the burn path runs) when ONNX Runtime or DirectML
    /// can't take it.
    pub fn from_safetensors(bytes: Vec<u8>) -> Result<Self> {
        Self::build(bytes, None)
    }

    /// As [`Self::from_safetensors`] on ONNX Runtime's CPU execution provider (same graph, same
    /// fp32 arithmetic): its convolutions are 2–3× faster than burn's CPU backend on the
    /// same cores (a 2-crop page in about 11 s instead of 29 s). `threads`: 0, one per core.
    pub fn cpu_from_safetensors(bytes: Vec<u8>, threads: usize) -> Result<Self> {
        Self::build(bytes, Some(threads))
    }

    /// `cpu`: on the CPU provider with that many threads (0: one per core), else on DirectML.
    /// The file's bytes are let go once read, and the weights once written: each is 100 to 200 MB,
    /// and the session then makes its own copy.
    fn build(bytes: Vec<u8>, cpu: Option<usize>) -> Result<Self> {
        let env = environment().map_err(LamaError::Unavailable)?;
        let weights = crate::weights::parse(&bytes)?;
        drop(bytes);
        let model = crate::onnx::lama(&weights)?;
        drop(weights);
        let what = if cpu.is_some() { "ONNX Runtime CPU" } else { "DirectML" };
        let session = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> std::result::Result<Session, String> {
            let b = Session::builder(env).map_err(|e| e.to_string())?.with_optimization_level(GraphOptimizationLevel::Level3).map_err(|e| e.to_string())?;
            let mut b = if let Some(threads) = cpu {
                // The CPU provider plans its buffers per shape (memory pattern) and uses one
                // thread per physical core by default.
                let b = if threads > 0 { b.with_intra_threads(threads).map_err(|e| e.to_string())? } else { b };
                b.with_memory_pattern(true)
                    .map_err(|e| e.to_string())?
                    .with_execution_providers([ep::CPU::default().with_arena_allocator(cpu_arena_on()).build().error_on_failure()])
                    .map_err(|e| e.to_string())?
            } else {
                // DirectML wants sequential execution and no memory pattern (its own allocator).
                b.with_memory_pattern(false)
                    .map_err(|e| e.to_string())?
                    .with_parallel_execution(false)
                    .map_err(|e| e.to_string())?
                    .with_execution_providers([ep::DirectML::default().build().error_on_failure(), ep::CPU::default().with_arena_allocator(true).build()])
                    .map_err(|e| e.to_string())?
            };
            b.commit_from_memory(&model).map_err(|e| e.to_string())
        }))
        .unwrap_or_else(|_| Err("session creation failed".into()))
        .map_err(|e| LamaError::Device(format!("{what}: {e}")))?;
        Ok(Self { session: Mutex::new(session), shrink: AtomicBool::new(cpu.is_none() || cpu_arena_on()) })
    }

    /// [`crate::Lama::generate`] on DirectML: the raw generator output (interleaved RGB,
    /// `w·h·3`, 0..=1) for every pixel of a working tile.
    pub fn generate(&self, rgb: &[f32], mask: &[f32], w: usize, h: usize) -> Result<Vec<f32>> {
        match self.generate_once(rgb, mask, w, h) {
            // A session that can't give its arena back refuses the run before it starts ("Did not
            // find an arena based allocator ... in the memory arena shrink list"): once more, as it
            // is, and so from now on.
            Err(LamaError::Device(e)) if (e.contains("arena") || e.contains("shrink")) && self.shrink.swap(false, Ordering::Relaxed) => self.generate_once(rgb, mask, w, h),
            r => r,
        }
    }

    fn generate_once(&self, rgb: &[f32], mask: &[f32], w: usize, h: usize) -> Result<Vec<f32>> {
        let planar = planar_input(rgb, mask, w, h)?;
        let d = Dft2::new(h / 8, w / 8).ok_or_else(|| LamaError::Input(format!("bottleneck {}x{} out of range", w / 8, h / 8)))?;
        let dim = |v: usize| i64::try_from(v).map_err(err);
        let x = Tensor::from_array(([1, 4, dim(h)?, dim(w)?], planar)).map_err(err)?;
        let mut inputs: Vec<(&str, ort::session::SessionInputValue<'_>)> = vec![(INPUT, x.into())];
        for (name, [r, c], v) in dft_inputs(&d) {
            inputs.push((name, Tensor::from_array(([dim(r)?, dim(c)?], v)).map_err(err)?.into()));
        }
        let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        let planar = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Vec<f32>> {
            let mut options = RunOptions::new().map_err(err)?;
            if self.shrink.load(Ordering::Relaxed) {
                options.set(SHRINK.0, SHRINK.1).map_err(err)?;
            }
            let outputs = session.run_with_options(inputs, &options).map_err(err)?;
            let y = outputs.get(OUTPUT).ok_or_else(|| err("no output"))?;
            let (_, data) = y.try_extract_tensor::<f32>().map_err(err)?;
            Ok(data.to_vec())
        }))
        .unwrap_or_else(|_| Err(err("the run failed")))?;
        let n = w * h;
        if planar.len() != n * 3 {
            return Err(err(format!("{} output values, expected {}", planar.len(), n * 3)));
        }
        let mut out = vec![0f32; n * 3];
        for (c, plane) in planar.chunks_exact(n).enumerate() {
            for (i, v) in plane.iter().enumerate() {
                if let Some(o) = out.get_mut(i * 3 + c) {
                    *o = *v;
                }
            }
        }
        Ok(out)
    }
}
