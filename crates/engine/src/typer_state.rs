//! TypeR state held by the [`crate::Session`] (see `docs/typer.md`).
//!
//! The script, cursor, page files, style library and settings live engine-side so the UI, the
//! CLI, actions and MCP agents all drive the same loop. This is session state, not document data:
//! it is not saved in `.pcraft` and not part of undo. Undo reverts only the layer changes a
//! placement made; the cursor keeps its place, as in TypeR.

use photocraft_typer::{State, StoredSelection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TyperSession {
    /// Script, cursor, page files, library and settings.
    pub state: State,
    /// Multi-bubble selections captured on the current page (M1, WP1). Bound to the open page,
    /// so never persisted.
    #[serde(skip)]
    pub stored_selections: Vec<StoredSelection>,
    /// Placement caches and per-document state (WP1): the page image bubbles are found on, the
    /// cleaning layers hidden per document. Never persisted.
    #[serde(skip)]
    pub(crate) place: crate::typer_place_cmds::PlaceState,
}

impl TyperSession {
    /// The persistent part as JSON (library, settings, script, cursor, page files), for the
    /// workspace file WP2 writes.
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// Restore from [`TyperSession::to_json`]. Errors on malformed data; nothing is applied then.
    /// Workspaces from before tabs (and TypeR storage) get TypeR's tab migration first.
    pub fn from_json(v: &Value) -> Result<TyperSession, String> {
        let mut v = v.clone();
        if let Some(st) = v.get_mut("state").and_then(Value::as_object_mut) {
            photocraft_typer::state::migrate_tabs(st);
        }
        let s: TyperSession = serde_json::from_value(v).map_err(|e| format!("TypeR workspace: {e}"))?;
        Ok(TyperSession { state: s.state.restored(), ..TyperSession::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn workspace_json_round_trips_and_rejects_garbage() {
        let mut t = TyperSession::default();
        t.state.set_text("Page 1\nhello\nworld").unwrap();
        t.state.next_line(false);
        t.state.ensure_tabs();
        let back = TyperSession::from_json(&t.to_json()).unwrap();
        assert_eq!(back, t);
        assert_eq!(back.state.current_line().map(|l| l.text.as_str()), Some("world"));
        assert!(TyperSession::from_json(&json!({"state": {"settings": {"internalPadding": "wide"}}})).is_err());
        assert!(TyperSession::from_json(&json!({})).is_ok());
    }
}
