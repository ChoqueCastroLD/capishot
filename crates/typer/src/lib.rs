//! # photocraft-typer
//!
//! The TypeR typesetting workflow for manga and comics, as pure logic: a translated script split
//! into lines with page markers, tags and Markdown; the line/page cursor; a library of text styles
//! in folders; TypeR's JSON style-library format; and the mapping of TypeR styles onto Capishot's
//! type model. No UI and no I/O; the engine's `typer.*` commands drive it.
//!
//! Ported from TypeR (<https://github.com/ScanR/TypeR>), MIT License, Copyright (c) 2017 Swirt;
//! see `LICENSE-TypeR.txt` next to this crate's manifest. Architecture and the work packages
//! that build on this crate: `docs/typer.md`.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod bubble;
pub mod exchange;
pub mod id;
pub mod library;
pub mod markdown;
pub mod pagedetect;
pub mod pages;
pub mod payload;
pub mod place;
pub mod presets;
pub mod psjson;
pub mod script;
pub mod settings;
pub mod shaper;
pub mod state;
#[doc(hidden)]
pub mod synth;

pub use bubble::{BubbleHit, Rect, Row, StoredSelection};
pub use id::Id;
pub use library::{Folder, Library, Style, TextType};
pub use script::{Line, UsedLineStyle};
pub use settings::{Direction, Settings};
pub use state::{Moved, State};

/// Settings by their plan name (`TyperSettings`).
pub type TyperSettings = Settings;
