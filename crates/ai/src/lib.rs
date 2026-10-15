//! # photocraft-ai
//!
//! Capishot's local AI runtime: models run on the user's own GPU (WebGPU in the browser, Vulkan,
//! Metal or DX12 natively, through wgpu) or on the CPU, with no server and no Python.
//!
//! The crate has two halves:
//!
//! * **Always built** (small, no inference): the [`catalog`] of known models and the
//!   MangaJaNai selection rules ([`mangajanai`]), the planar f32 [`Planar`] image, [`tile`]
//!   planning with blended overlaps, [`weights`] parsing from safetensors bytes, and the
//!   [`cache`] (verified downloads; the native disk cache). The engine and the main web bundle
//!   use only this half.
//! * **`runtime` feature** (`cpu` and/or `gpu`): burn inference. [`runtime::Runtime`] owns the
//!   device; a model is anything implementing [`runtime::Network`] (a burn forward pass);
//!   [`runtime::run_tiled`] runs a network over an image of any size, tile by tile, with progress
//!   and cancellation through [`Progress`]. [`esrgan::Esrgan`] is the ESRGAN (RRDBNet) network
//!   MangaJaNai uses; [`upscale::upscale`] is the whole MangaJaNai pipeline.
//!
//! In the browser the runtime half lives in a separately loaded Web Worker module
//! (`apps/photocraft-ai-worker`), so the main wasm doesn't grow.
//!
//! ## Adding a model (for example an inpainting network)
//!
//! 1. Convert its weights to safetensors outside the repository (never commit weights).
//! 2. Implement [`runtime::Network`] with burn tensor ops, loading parameters from
//!    [`weights::Weights`] with [`runtime::Runtime::tensor`].
//! 3. Add a [`catalog::ModelInfo`] entry (id, file name, size, sha256, licence).
//! 4. Run it with [`runtime::run_tiled`] (or call `forward` directly for whole-image models).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod cache;
pub mod catalog;
pub mod image;
pub mod mangajanai;
pub mod profile;
pub mod service;
pub mod tile;
pub mod weights;

#[cfg(feature = "runtime")]
pub mod compact;
#[cfg(feature = "runtime")]
pub mod esrgan;
#[cfg(all(feature = "runtime", not(target_family = "wasm")))]
pub mod native;
#[cfg(feature = "runtime")]
pub mod runtime;
#[cfg(feature = "runtime")]
pub mod upscale;

pub use image::Planar;

/// Everything that can go wrong in this crate. Nothing here panics on bad input.
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("invalid model weights: {0}")]
    Weights(String),
    #[error("unknown model `{0}`")]
    UnknownModel(String),
    #[error("invalid image: {0}")]
    Image(String),
    #[error("the model file doesn't match its checksum (expected {expected}, got {actual})")]
    Checksum { expected: String, actual: String },
    #[error("download failed: {0}")]
    Download(String),
    #[error("model cache: {0}")]
    Cache(String),
    #[error("no compute device: {0}")]
    Device(String),
    #[error("inference failed: {0}")]
    Inference(String),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, AiError>;

/// Progress and cancellation for long work (downloads, tiled inference). Implementations must be
/// cheap: they are called once per tile or chunk.
pub trait Progress {
    /// `done` of `total` units finished, with a short status.
    fn report(&self, done: u64, total: u64, status: &str);
    /// Stop as soon as possible; the work then returns [`AiError::Cancelled`].
    fn cancelled(&self) -> bool {
        false
    }
}

/// A [`Progress`] that ignores reports and never cancels.
pub struct NoProgress;

impl Progress for NoProgress {
    fn report(&self, _done: u64, _total: u64, _status: &str) {}
}
