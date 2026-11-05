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

use std::sync::{Mutex, PoisonError};

use ort::environment::Environment;
use ort::ep;
use ort::session::Session;
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

/// Manga LaMa in one ONNX Runtime session on DirectML.
pub struct DmlLama {
    session: Mutex<Session>,
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
    pub fn from_safetensors(bytes: &[u8]) -> Result<Self> {
        Self::build(bytes, None)
    }

    /// As [`Self::from_safetensors`] on ONNX Runtime's CPU execution provider (same graph, same
    /// fp32 arithmetic): its convolutions are 2–3× faster than burn's CPU backend on the
    /// same cores (a 2-crop page in about 11 s instead of 29 s). `threads`: 0, one per core.
    pub fn cpu_from_safetensors(bytes: &[u8], threads: usize) -> Result<Self> {
        Self::build(bytes, Some(threads))
    }

    /// `cpu`: on the CPU provider with that many threads (0: one per core), else on DirectML.
    fn build(bytes: &[u8], cpu: Option<usize>) -> Result<Self> {
        let env = environment().map_err(LamaError::Unavailable)?;
        let model = crate::onnx::lama(&crate::weights::parse(bytes)?);
        let what = if cpu.is_some() { "ONNX Runtime CPU" } else { "DirectML" };
        let session = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> std::result::Result<Session, String> {
            let b = Session::builder(env).map_err(|e| e.to_string())?.with_optimization_level(GraphOptimizationLevel::Level3).map_err(|e| e.to_string())?;
            let mut b = if let Some(threads) = cpu {
                // The CPU provider plans its buffers per shape (memory pattern) and uses one
                // thread per physical core by default.
                let b = if threads > 0 { b.with_intra_threads(threads).map_err(|e| e.to_string())? } else { b };
                b.with_memory_pattern(true)
                    .map_err(|e| e.to_string())?
                    .with_execution_providers([ep::CPU::default().build().error_on_failure()])
                    .map_err(|e| e.to_string())?
            } else {
                // DirectML wants sequential execution and no memory pattern (its own allocator).
                b.with_memory_pattern(false)
                    .map_err(|e| e.to_string())?
                    .with_parallel_execution(false)
                    .map_err(|e| e.to_string())?
                    .with_execution_providers([ep::DirectML::default().build().error_on_failure()])
                    .map_err(|e| e.to_string())?
            };
            b.commit_from_memory(&model).map_err(|e| e.to_string())
        }))
        .unwrap_or_else(|_| Err("session creation failed".into()))
        .map_err(|e| LamaError::Device(format!("{what}: {e}")))?;
        Ok(Self { session: Mutex::new(session) })
    }

    /// [`crate::Lama::generate`] on DirectML: the raw generator output (interleaved RGB,
    /// `w·h·3`, 0..=1) for every pixel of a working tile.
    pub fn generate(&self, rgb: &[f32], mask: &[f32], w: usize, h: usize) -> Result<Vec<f32>> {
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
            let outputs = session.run(inputs).map_err(err)?;
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
