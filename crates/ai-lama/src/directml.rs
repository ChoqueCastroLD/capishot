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

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use ort::environment::Environment;
use ort::ep;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

use crate::dft::Dft2;
use crate::model::planar_input;
use crate::onnx::{INPUT, OUTPUT, dft_inputs};
use crate::{LamaError, Result};

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
    // A broken or mismatched library must not take the app down. ONNX Runtime keeps one
    // environment per process, so this and the upscaler's share it.
    std::panic::catch_unwind(|| -> std::result::Result<Environment, String> {
        ort::init_from(path).map_err(|e| e.to_string())?.with_name("capishot").with_telemetry(false).build().map_err(|e| e.to_string())
    })
    .unwrap_or_else(|_| Err(format!("{} failed to initialise", path.display())))
}

/// Whether the DirectML backend can run here (the library loads), or why not.
pub fn available() -> std::result::Result<(), String> {
    environment().map(|_| ())
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
        let env = environment().map_err(LamaError::Unavailable)?;
        let model = crate::onnx::lama(&crate::weights::parse(bytes)?);
        let session = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> std::result::Result<Session, String> {
            // DirectML wants sequential execution and no memory pattern (its own allocator).
            Session::builder(env)
                .map_err(|e| e.to_string())?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| e.to_string())?
                .with_memory_pattern(false)
                .map_err(|e| e.to_string())?
                .with_parallel_execution(false)
                .map_err(|e| e.to_string())?
                .with_execution_providers([ep::DirectML::default().build().error_on_failure()])
                .map_err(|e| e.to_string())?
                .commit_from_memory(&model)
                .map_err(|e| e.to_string())
        }))
        .unwrap_or_else(|_| Err("session creation failed".into()))
        .map_err(|e| LamaError::Device(format!("DirectML: {e}")))?;
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
