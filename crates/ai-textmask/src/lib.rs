//! Finds the text and sound effects on a manga page with the mask model
//! (`manga-text-mask.onnx`, a ResNet-18 U-Net), as regions a person chooses from: the model also
//! marks some art, so nothing here decides what gets removed.
//!
//! [`regions`] turns the model's probabilities into regions, with no ML dependency; [`native`]
//! (feature `native`) runs the model on ONNX Runtime and [`net`] (feature `model`) on burn, for
//! the browser. A port of the model's own script: the same
//! page gives the same regions (`examples/textmask.rs` compares them).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod pipeline;
pub mod regions;
pub mod resize;

#[cfg(feature = "model")]
pub mod net;

#[cfg(all(feature = "native", any(windows, target_os = "linux")))]
pub mod native;

pub use regions::{Detection, Region, default_grow, selection_mask};

/// Why a detection failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The model or its runtime can't run here.
    Unavailable(String),
    /// A run failed.
    Device(String),
    /// A run failed on the GPU: the CPU may do.
    Gpu(String),
    /// The page's buffer doesn't match its size.
    Input(String),
    Cancelled,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unavailable(s) | Error::Device(s) | Error::Gpu(s) | Error::Input(s) => f.write_str(s),
            Error::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for Error {}

/// Whether the model's file is here to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelState {
    Ready,
    NeedsDownload,
    /// Still finding out (the browser's cache answers asynchronously).
    Checking,
    Unavailable(String),
}

/// A page's RGB, interleaved, one byte a channel.
#[derive(Clone, Copy, Debug)]
pub struct Page<'a> {
    pub rgb: &'a [u8],
    pub width: usize,
    pub height: usize,
}

/// Runs the mask model. The engine holds one; a build without it has none.
pub trait TextMasker: Send + Sync {
    fn model_state(&self) -> ModelState;
    /// Where it runs, for the UI ("DirectML", "CPU"); empty until known.
    fn device(&self) -> String;
    /// The page's regions. `cancelled` is asked between tiles.
    fn detect(&self, page: Page<'_>, cancelled: &dyn Fn() -> bool) -> Result<Detection, Error>;
    /// [`TextMasker::detect`] for a job: a browser's page can't wait for its worker, so it
    /// answers later. `width` and `height` are the buffer's.
    fn detect_then(&self, rgb: Vec<u8>, width: usize, height: usize, hooks: Hooks, done: DetectDone) {
        done(self.detect(Page { rgb: &rgb, width, height }, &*hooks.cancelled));
    }
}

/// What a run tells and asks its job.
#[derive(Clone)]
pub struct Hooks {
    pub cancelled: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
    pub progress: std::sync::Arc<dyn Fn(f32) + Send + Sync>,
}

/// Where a run's answer goes.
pub type DetectDone = Box<dyn FnOnce(Result<Detection, Error>) + Send>;
