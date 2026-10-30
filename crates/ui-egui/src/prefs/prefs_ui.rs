//! Edit › Preferences, Keyboard Shortcuts and Menus, Color Settings and the rest of the Edit
//! menu's dialogs, plus the shell side of preferences: loading and saving them through
//! [`crate::Services`], applying the interface theme, autosave and crash recovery, and the
//! history log.
//!
//! The values live in the engine ([`photocraft_engine::prefs::Preferences`], so agents read and
//! change them with `prefs.get` / `prefs.set`); this module only edits a working copy in a dialog
//! and commits it with those commands on Apply or OK.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use egui::{Color32, RichText, Sense, vec2};
use photocraft_doc::DocId;
use photocraft_engine::jobs::{JobEvent, JobId, JobOutcome, Started};
#[cfg(test)]
use photocraft_engine::prefs::Theme;
use photocraft_engine::prefs::{self, AppearanceMode, DarkTheme, LightTheme, SECTIONS, Theme as SavedTheme};
use photocraft_engine::snap::{SnapLine, SnapOptions, SnapTargets};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::state::DialogKind;
use crate::theme::{ThemeKind, Tokens};

#[cfg(not(target_arch = "wasm32"))]
type DiscoveryResult = std::sync::Arc<wasm_sync::Mutex<Option<(Vec<crate::Recoverable>, Vec<String>)>>>;

/// Shell runtime state for preferences, autosave and snapping (not serialised).
#[derive(Default)]
pub struct Runtime {
    loaded: bool,
    /// Preferences revision last written to storage (or loaded from it).
    saved_rev: u64,
    /// This instance's preferences as of `saved_rev`: the base of the three-way merge on save.
    saved_value: Option<Value>,
    /// The text storage held after this instance's last load or write: while storage still
    /// holds it, nothing else wrote there and a save needs no merge (no parse of the stored text).
    saved_text: Option<String>,
    /// `saved_text` as a tree (what storage holds, keys this version doesn't know included): the
    /// next write merges into it in place instead of parsing the stored text again.
    saved_tree: Option<Value>,
    /// Debounce (`Services::prefs_debounce_s`): the revision last seen, when it changed, and
    /// when the oldest unsaved change was seen.
    seen_rev: u64,
    changed_at: f64,
    pending_since: Option<f64>,
    /// The TypeR workspace as last saved (`typer::persist`): an unchanged one is not serialised.
    pub(crate) typer_saved: Option<photocraft_typer::State>,
    save_retry: SaveRetry,
    next_autosave_ms: f64,
    recovery_started: bool,
    #[cfg(not(target_arch = "wasm32"))]
    discovery_pending: Option<JobId>,
    #[cfg(not(target_arch = "wasm32"))]
    discovery_result: DiscoveryResult,
    recovery_queue: VecDeque<crate::Recoverable>,
    recovery_pending: Option<(JobId, String)>,
    autosaved: HashMap<DocId, CheckpointStamp>,
    pending_autosaves: HashMap<DocId, CheckpointStamp>,
    recovery_keys: HashMap<DocId, String>,
    retiring: HashSet<DocId>,
    catch_up: HashSet<DocId>,
    retry_after: HashMap<DocId, f64>,
    recovery_error: Option<String>,
    autosave_errors: HashMap<DocId, String>,
    /// The revision each document was last written to its file at (Save, or a browser download
    /// that may not report back), for [`crate::Services::autosave_unsaved_only`].
    written_at: HashMap<DocId, u64>,
    /// When each document's last checkpoint was confirmed stored (`now_ms`), for the status
    /// bar's "Autosaved N min ago".
    autosaved_at: HashMap<DocId, f64>,
    log_len: usize,
    /// Snapping state of the drag in progress (see `snap_ui`).
    pub(crate) snap: Option<crate::snap_ui::ActiveSnap>,
    pub(crate) snap_lines: Vec<SnapLine>,
    pub(crate) guide_targets: Option<((DocId, u64, SnapOptions), SnapTargets)>,
    /// Snap targets built for drags (`snap_ui::build`), by what they were built from: a drag
    /// starting on an unchanged document reuses them instead of asking every layer its bounds.
    pub(crate) snap_cache: Vec<(u64, SnapTargets)>,
    /// Checkerboard colours the cached CPU checker texture was built with.
    pub(crate) checker_key: Option<[[u8; 3]; 2]>,
    /// Style last sent to the GPU canvas.
    pub(crate) gpu_style: Option<crate::gpu_canvas::CanvasStyle>,
    /// `unitsAndRulers.showRulers` as last applied to View › Rulers.
    show_rulers: Option<bool>,
    /// Tool options as last remembered in the preferences (see `tool_memory`).
    pub(crate) tool_options: Option<crate::state::ToolOptions>,
    pub(crate) selected_tool: Option<crate::state::Tool>,
    /// The tool owning the session brush and the other tools' brushes, as last remembered.
    pub(crate) tool_brushes: Option<crate::tool_memory::ToolBrushes>,
}

/// Logical history changes such as Purge can occur without a document revision.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct CheckpointStamp {
    revision: u64,
    undo: usize,
    redo: usize,
    label: String,
    context: Value,
    path: Option<String>,
}
impl CheckpointStamp {
    pub(super) fn of(st: &photocraft_engine::DocState, context: Value) -> Self {
        let (undo, redo, label) = st.history.checkpoint_signature();
        Self { revision: st.revision, undo, redo, label: label.to_owned(), context, path: st.path.clone() }
    }
}

/// Durable navigation context, excluding unfinished gestures and modal dialogs.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct RecoveryContext {
    active_layer: Option<photocraft_doc::LayerId>,
    selected_layers: Vec<photocraft_doc::LayerId>,
    layer_anchor: Option<photocraft_doc::LayerId>,
    view: Option<crate::state::View>,
    tool: Option<crate::state::Tool>,
    tab_order: usize,
    active: bool,
}
pub(super) fn recovery_context(app: &PhotocraftApp, index: usize, st: &photocraft_engine::DocState) -> Value {
    json!({"activeLayer": st.active_layer, "selectedLayers": st.selected_layers,
        "layerAnchor": st.layer_anchor, "view": app.ui.views.get(index), "tool": app.ui.tool,
        "tabOrder": index, "active": app.session.active_index() == Some(index)})
}

// ------------------------------------------------------------------ shortcuts

pub use crate::shortcuts::{default_shortcut, effective_shortcut};

mod appearance;
mod dialogs;
mod fields;
mod graphics;
mod save;
mod shortcuts;
mod startup;

#[cfg(test)]
use appearance::{DARK_THEME_OPTIONS, LIGHT_THEME_OPTIONS, display_scale, favorite_theme_groups, selected_theme};
pub(crate) use appearance::{appearance_context_menu, cycle_appearance, display_zoom, step_brightness, sync_appearance};
pub use appearance::{canvas_style, pasteboard_color};
pub use dialogs::{
    PREFERENCES_SIZE, SHEET_BELOW, SHEET_MARGIN, apply, confirm, invoke, is_preferences, is_sheet, open_dialog, open_mismatch, open_preferences,
    open_shortcuts, owns, preferences_changed, width,
};
#[cfg(test)]
use dialogs::{preference_values, presets_io_body, settings_height};
pub use fields::body;
#[cfg(test)]
use fields::{
    choice_label, export_field_visible, field_visible, has_visible_fields, hex_field, humanize, preference_sections, section_fields, tools_field_visible,
};
#[cfg(test)]
use graphics::{gpu_status_rows, rendering_mode_value};
use save::SaveRetry;
#[cfg(test)]
use save::{SAVE_RETRY_MAX_S, SAVE_RETRY_S, merge_changes, persist};
pub(crate) use save::{autosave_hint, document_written, recovery_pending, recovery_pending_all, retire_all, save_preferences};
pub use save::{autosave_now, flush_prefs, prefs_pending, tick};
#[cfg(test)]
pub(crate) use shortcuts::capture_key_press;
pub use shortcuts::{is_modifier_key, reserved_shortcut, shortcut_items, shortcut_text};
#[cfg(test)]
use startup::finish_recovery;
pub use startup::load;
pub(crate) use startup::{on_recovery_event, queue_recovery, remember_rulers};

// Test modules stay after the production code: the prefs_usage test reads each file up to
// its first `#[cfg(test)] mod`.
#[cfg(test)]
mod recovery_tests;

#[cfg(test)]
mod shortcut_capture_tests;
#[cfg(test)]
mod shortcut_set_tests;

#[cfg(test)]
mod tests;
