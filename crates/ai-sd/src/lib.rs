//! # photocraft-ai-sd
//!
//! Generative Fill for Capishot: Stable Diffusion 1.5 inpainting (9-channel UNet) with a fused
//! few-step LoRA (LCM / TCD), ported to [burn](https://burn.dev) like the LaMa port: WebGPU in
//! the browser, Vulkan/Metal/DX12 natively, or the pure-Rust CPU backend. No Python, no ONNX.
//!
//! Always compiled (no ML dependency): [`tokenizer`] (CLIP BPE), [`scheduler`] (LCM/TCD
//! coefficients), [`rng`] (deterministic noise). The networks are behind the `model` feature.
//!
//! Nothing here panics on bad input: weights are validated tensor by tensor before any device
//! allocation, and every size is checked before a network runs.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod crop;
pub mod files;
pub mod prompt;
pub mod rng;
pub mod scheduler;
pub mod service;
pub mod stream;
pub mod tokenizer;

#[cfg(feature = "model")]
pub mod clip;
#[cfg(feature = "model")]
pub mod lora;
#[cfg(all(feature = "native", not(target_family = "wasm")))]
pub mod native;
#[cfg(feature = "model")]
pub mod nn;
#[cfg(feature = "model")]
pub mod pipeline;
#[cfg(feature = "model")]
pub mod runner;
#[cfg(feature = "model")]
pub mod taesd;
#[cfg(feature = "model")]
pub mod unet;
#[cfg(feature = "model")]
pub mod vae;

#[cfg(feature = "model")]
pub use burn;

/// Errors of this crate.
#[derive(Debug, thiserror::Error)]
pub enum SdError {
    /// The weights file is not one this crate understands.
    #[error("invalid Stable Diffusion weights: {0}")]
    Weights(String),
    /// Bad prompt, image, mask or parameter.
    #[error("invalid input: {0}")]
    Input(String),
    /// The device failed while running or reading back a network.
    #[error("inference failed: {0}")]
    Device(String),
    /// The model can't run here (weights missing, no device, not in this build).
    #[error("{0}")]
    Unavailable(String),
    /// The caller cancelled the work.
    #[error("cancelled")]
    Cancelled,
}

/// Result alias.
pub type Result<T> = std::result::Result<T, SdError>;

/// The VAE downsamples 8×: pixel sizes are multiples of this.
pub const LATENT_FACTOR: usize = 8;
/// Latent channels of the SD 1.x VAE.
pub const LATENT_CHANNELS: usize = 4;
/// SD 1.x latent scaling factor.
pub const VAE_SCALE: f32 = 0.18215;
