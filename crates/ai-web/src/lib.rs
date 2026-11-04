//! The AI services in the browser: the engine's upscaler, inpainter, Generative Fill and model
//! store backed by the AI Web Worker (`apps/photocraft-ai-worker`), a second wasm module loaded
//! only when a model runs. Used by the web app ([`from_page`], `<meta>` tags) and by apps that
//! embed Capishot (`capishot-host`, [`from_config`]). See [`bridge`] for how runs and downloads go.
//!
//! Only built for `wasm32`; elsewhere the crate is empty (native shells use
//! `photocraft_ai::native`, `photocraft_ai_lama::native` and `photocraft_ai_sd::native`).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_arch = "wasm32")]
pub mod bridge;

#[cfg(target_arch = "wasm32")]
pub use bridge::{Services, WebAiConfig, WebGenFill, WebInpainter, WebModelStore, WebUpscaler, from_config, from_page};
