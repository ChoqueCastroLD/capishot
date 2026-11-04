//! # capishot-host
//!
//! Capishot (PhotoCraft's engine) as a library inside a host app's own process, on native and
//! on `wasm32` with the same API: open documents from bytes, run any command of the control
//! protocol by id, read and write pixels, group undo steps, run long commands as jobs, get
//! events and keep a document in step between collaborators. Nothing of the app shell (menus,
//! file dialogs, windows) is here; the egui widget (#31) builds on [`Host`].
//!
//! ```
//! use capishot_host::{Host, OpenOptions};
//! use serde_json::json;
//!
//! let mut host = Host::new();
//! host.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
//! let doc = host.active_document().unwrap();
//! host.execute("layer.new.layer", json!({"name": "Ink"})).unwrap();
//! let _baseline = host.poll();
//! host.undo(doc).unwrap();
//! for event in host.poll() {
//!     println!("{}", event.to_json());
//! }
//! let psd = host.save_psd(doc).unwrap();
//! let (copy, _warnings) = host.open_psd(psd, OpenOptions { name: Some("copy.psd".into()), hidden: true }).unwrap();
//! assert_ne!(copy, doc);
//! ```
//!
//! ## Stability
//!
//! Stable (changes only with a `CHANGELOG.md` entry under "Breaking"): [`Host`]'s methods not
//! marked unstable, [`Event`], [`Error`], the id types and the option/result structs. Command
//! ids and params are the control protocol's (`docs/control-protocol.md`), which only grows.
//!
//! Experimental: [`live`] (the [`live::Op`] wire form may change), [`Host::session`] /
//! [`Host::session_mut`] and the [`unstable`] re-exports.
//!
//! ## Threads and the browser
//!
//! A [`Host`] lives on one thread (the one that draws). Nothing blocks except
//! [`Host::wait`], which exists natively only. Long commands go through [`Host::start`]: on a
//! worker thread natively and in the browser threads build, inline in the single-threaded
//! browser build. Call [`Host::poll`] once per frame: it applies finished jobs and reports the
//! events.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![warn(missing_docs)]

mod error;
mod events;
mod host;
mod ids;
pub mod live;

pub use error::{Error, Result};
pub use events::{Event, JobEnd, Origin};
pub use host::{
    BatchSteps, DocumentInfo, ExecOptions, GetPixels, History, Host, InspectOptions, JobStatus, OpenOptions, Pixels, PutPixels, SaveStarted, Started,
    Subscription, ViewHooks,
};
pub use ids::{DocumentId, JobHandle, LayerId, LayerUid};
pub use serde_json::{self, Value};

/// This crate's API version: bumped on every breaking change (see `CHANGELOG.md`).
pub const API_VERSION: u32 = 1;

/// **Unstable** escape hatches: the engine crates this one wraps. Their APIs change without
/// notice; use them only for what [`Host`] doesn't offer yet (and tell us what that is).
pub mod unstable {
    pub use photocraft_doc as doc;
    pub use photocraft_engine as engine;
    pub use photocraft_io as io;
}
