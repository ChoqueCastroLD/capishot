//! # photocraft-ai-lama
//!
//! LaMa inpainting ("Resolution-robust Large Mask Inpainting with Fourier Convolutions",
//! Suvorov et al., WACV 2022) for scanlation cleaning: erase text and SFX under a mask and fill
//! it with plausible manga background and screentone. The generator is the big-lama
//! architecture (Fast Fourier Convolutions, 18 FFC ResNet blocks); the intended weights are the
//! manga/anime fine-tune `lama_large_512px` (MIT), converted to safetensors with the batch norms
//! folded into the convolutions (see `README.md`).
//!
//! Two halves:
//!
//! * [`tiles`] (always compiled, no ML dependency): plans crops around each masked blob,
//!   resizes them to the model's working size, and pastes the result back so only masked
//!   pixels change. The editor's main module uses this half alone.
//! * [`Lama`] (feature `model`, pulled in by `flex` or `wgpu`): the generator on
//!   [burn](https://burn.dev), running on wgpu (WebGPU in the browser) or the pure-Rust CPU
//!   backend. The spectral transform's `rfft2`/`irfft2` are computed as dense DFT matrix
//!   products ([`dft`]), which works for every feature-map size and runs on every backend.
//!
//! Nothing here panics on bad input: weights are validated tensor by tensor before any
//! allocation on the device, and every size is checked before the model runs.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod dft;
pub mod service;
pub mod tiles;

#[cfg(feature = "model")]
mod generator;
#[cfg(feature = "model")]
mod migan;
#[cfg(feature = "model")]
mod model;
#[cfg(all(feature = "model", not(target_family = "wasm")))]
pub mod native;
#[cfg(feature = "model")]
mod weights;

#[cfg(feature = "model")]
pub use burn;
#[cfg(feature = "model")]
pub use generator::Generator;
#[cfg(feature = "model")]
pub use migan::Migan;
#[cfg(feature = "model")]
pub use model::Lama;
#[cfg(feature = "model")]
pub use weights::Arch;

/// Errors of this crate.
#[derive(Debug, thiserror::Error)]
pub enum LamaError {
    /// The weights file is not a LaMa safetensors file this crate understands.
    #[error("invalid LaMa weights: {0}")]
    Weights(String),
    /// Bad image, mask or tile dimensions.
    #[error("invalid input: {0}")]
    Input(String),
    /// The device failed while running or reading back the model.
    #[error("inference failed: {0}")]
    Device(String),
    /// The GPU couldn't run the model (a WebGPU limit, a lost device, a crash in the GPU
    /// runtime). The same request can run on the CPU; callers retry there.
    #[error("the GPU couldn't run the model: {0}")]
    Gpu(String),
    /// The model can't run here (weights missing, no device, not in this build).
    #[error("{0}")]
    Unavailable(String),
    /// The caller cancelled the work.
    #[error("cancelled")]
    Cancelled,
}

/// Result alias.
pub type Result<T> = std::result::Result<T, LamaError>;

/// The generator downsamples three times, so the working size is a multiple of this.
pub const SIZE_MULTIPLE: usize = 8;
/// Smallest working side: the reflect paddings at 1/8 scale need at least 2 pixels, and the
/// global (Fourier) branch has little to work with below this.
pub const MIN_WORK_SIDE: usize = 64;
/// Largest working side accepted by [`Lama`]: 2048 px is about 6.5 TFLOP and 3 GiB of
/// activations, well past what a browser tab can hold.
pub const MAX_WORK_SIDE: usize = 2048;
