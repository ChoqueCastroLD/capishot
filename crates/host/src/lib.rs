//! # capishot-host
//!
//! Capishot (PhotoCraft's engine) as a library inside a host app's own process, on native and
//! on `wasm32` with the same API. This branch carries the files, fonts and rendering part (#33):
//!
//! - [`files`]: open documents from bytes, save them to bytes (PSD/PSB incremental, `.pcraft`,
//!   flat formats), with a snapshot/encode/finish split for saving off the editor's thread.
//! - [`fonts`]: the host's fonts, registered at run time; per-document fonts-ready; the
//!   `type.layoutDigest` determinism switch.
//! - [`render`]: a region, or the whole document at a scale, to RGBA (thumbnails, compare).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![warn(missing_docs)]

mod error;
mod ids;

pub mod files;
pub mod fonts;
pub mod render;

pub use error::{Error, Result};
pub use ids::{DocumentId, JobHandle, LayerId, LayerUid};
