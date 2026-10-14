// Ported from TypeR app_src/tabStorage.js and the tab cases of app_src/context.jsx (develop
// 71bc061), MIT, (c) 2017 Swirt. See crates/typer/LICENSE-TypeR.txt.

//! Work tabs (TypeR 3.0, N6): several chapters or series open side by side, each with its own
//! script, page files, cursor and used line styles. The library and the settings are shared.
//!
//! In [`crate::State`] the active tab's data is the state's own fields (`text`, `images`, …);
//! its entry in `tabs` keeps only its id and name, so nothing is stored twice. Switching moves
//! the data between the two.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::id::Id;
use crate::script::UsedLineStyle;

/// The fields each tab holds, by their names in the workspace JSON (TypeR's `TAB_FIELDS`;
/// TypeR calls the fourth `lastOpenedImagePath`).
pub const TAB_FIELDS: [&str; 5] = ["text", "images", "currentLineIndex", "lastOpenedImage", "usedLineStyles"];

/// TypeR's spelling of a tab field, accepted on import.
const TYPER_LAST_OPENED: &str = "lastOpenedImagePath";

/// Longest tab name (characters).
pub const MAX_TAB_NAME: usize = 80;

/// Most tabs in a workspace.
pub const MAX_TABS: usize = 64;

/// One work tab.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Tab {
    pub id: String,
    pub name: String,
    pub text: String,
    pub images: Vec<String>,
    pub current_line_index: usize,
    #[serde(alias = "lastOpenedImagePath")]
    pub last_opened_image: Option<String>,
    pub used_line_styles: BTreeMap<usize, UsedLineStyle>,
}

impl Tab {
    /// A tab named `name` with the tab fields found in `data` (TypeR's `createTab`: anything of
    /// the wrong type is left empty). Page files may be names or TypeR's `{path}` objects.
    pub fn from_value(id: String, name: String, data: &Value) -> Tab {
        let images = data
            .get("images")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter().filter_map(|v| v.as_str().or_else(|| v.get("path").and_then(Value::as_str))).filter(|s| !s.is_empty()).map(str::to_string).collect()
            })
            .unwrap_or_default();
        let used_line_styles = data
            .get("usedLineStyles")
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| {
                        let i = k.parse::<usize>().ok()?;
                        let raw_text = v.get("rawText")?.as_str()?.to_string();
                        let style_id = Id::from_value(v.get("styleId")?)?;
                        Some((i, UsedLineStyle { raw_text, style_id }))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let last = data.get("lastOpenedImage").or_else(|| data.get(TYPER_LAST_OPENED));
        Tab {
            id,
            name,
            text: data.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
            images,
            current_line_index: data.get("currentLineIndex").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).unwrap_or(0),
            last_opened_image: last.and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
            used_line_styles,
        }
    }

    /// Has the tab a script or page files (closing it asks first)?
    pub fn has_content(&self) -> bool {
        !self.text.is_empty() || !self.images.is_empty()
    }
}

/// The outcome of [`migrate`].
#[derive(Clone, Debug, PartialEq)]
pub struct Migration {
    pub tabs: Vec<Value>,
    pub current_tab_id: String,
    /// The tabs changed and should be saved at once.
    pub migrated: bool,
}

/// TypeR's `migrateTabStorage` on the stored JSON: storage without tabs becomes one tab named
/// `default_name` holding the top-level tab fields; an unknown `currentTabId` falls back to the
/// first tab; and top-level tab fields that differ from the active tab's win (an older version
/// rewrote them after a downgrade), including an intentionally empty text. Other tabs are left
/// as they are; `data` is not changed.
pub fn migrate(data: &Value, default_name: &str, mut create_id: impl FnMut() -> String) -> Migration {
    let empty = Map::new();
    let data = data.as_object().unwrap_or(&empty);
    let tabs: Vec<Value> = data.get("tabs").and_then(Value::as_array).cloned().unwrap_or_default();
    let field = |k: &str| -> Option<&Value> {
        match k {
            "lastOpenedImage" => data.get(k).or_else(|| data.get(TYPER_LAST_OPENED)),
            _ => data.get(k),
        }
    };
    if tabs.is_empty() {
        let id = create_id();
        let mut tab = Map::new();
        tab.insert("id".into(), Value::String(id.clone()));
        tab.insert("name".into(), Value::String(default_name.to_string()));
        let t = Tab::from_value(id.clone(), default_name.to_string(), &Value::Object(data.clone()));
        if let Value::Object(o) = serde_json::to_value(&t).unwrap_or(Value::Null) {
            tab.extend(o);
        }
        // TypeR keeps the legacy values as they were (its images are `{path}` objects).
        for k in TAB_FIELDS {
            if let Some(v) = field(k) {
                tab.insert(k.to_string(), v.clone());
            }
        }
        return Migration { tabs: vec![Value::Object(tab)], current_tab_id: id, migrated: true };
    }
    let mut migrated = false;
    let wanted = data.get("currentTabId").and_then(Value::as_str);
    let active = match tabs.iter().position(|t| t.get("id").and_then(Value::as_str).is_some_and(|id| Some(id) == wanted)) {
        Some(i) => i,
        None => {
            migrated = true;
            0
        }
    };
    let mut tabs = tabs;
    let current_tab_id = tabs.get(active).and_then(|t| t.get("id")).and_then(Value::as_str).unwrap_or_default().to_string();
    if let Some(Value::Object(tab)) = tabs.get_mut(active) {
        let stale = TAB_FIELDS.iter().any(|k| field(k).is_some_and(|v| tab.get(*k) != Some(v)));
        if stale {
            for k in TAB_FIELDS {
                if let Some(v) = field(k) {
                    tab.insert(k.to_string(), v.clone());
                }
            }
            migrated = true;
        }
    }
    Migration { tabs, current_tab_id, migrated }
}

/// A tab id not used by `tabs`: `tab-1`, `tab-2`, …
pub fn next_id(tabs: &[Tab]) -> String {
    (1..).map(|n| format!("tab-{n}")).find(|id| !tabs.iter().any(|t| &t.id == id)).unwrap_or_default()
}

/// TypeR's default tab name: `Tab N` with N one more than the tab count.
pub fn default_name(count: usize) -> String {
    format!("Tab {}", count.saturating_add(1))
}

/// A tab name as typed: trimmed, at most [`MAX_TAB_NAME`] characters; `None` when empty.
pub fn clean_name(name: &str) -> Option<String> {
    let n = crate::pages::js_trim(name);
    (!n.is_empty()).then(|| n.chars().take(MAX_TAB_NAME).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // scripts/testTabStorage.js
    #[test]
    fn pre_tab_storage_becomes_the_first_tab() {
        let legacy = json!({
            "text": "Script kept from master",
            "images": [{"path": "page-001.psd"}],
            "currentLineIndex": 12,
            "lastOpenedImagePath": "page-001.psd",
            "usedLineStyles": {"12": {"rawText": "Line 12", "styleId": "style-1"}},
        });
        let m = migrate(&legacy, "Tab 1", || "tab-migrated".into());
        assert!(m.migrated, "pre-tab storage must be marked for immediate persistence");
        assert_eq!(m.current_tab_id, "tab-migrated");
        assert_eq!(m.tabs.len(), 1);
        let t = &m.tabs[0];
        assert_eq!((t["id"].as_str(), t["name"].as_str()), (Some("tab-migrated"), Some("Tab 1")));
        assert_eq!(t["text"], legacy["text"]);
        assert_eq!(t["images"], legacy["images"]);
        assert_eq!(t["currentLineIndex"], 12);
        assert_eq!(t["lastOpenedImage"], "page-001.psd");
        assert_eq!(t["usedLineStyles"], legacy["usedLineStyles"]);
        // Read as a tab: TypeR's `{path}` page files become names.
        let tab = Tab::from_value("x".into(), "Tab 1".into(), t);
        assert_eq!(tab.images, ["page-001.psd"]);
        assert_eq!(tab.used_line_styles.get(&12).map(|u| u.raw_text.as_str()), Some("Line 12"));
        assert_eq!(tab.last_opened_image.as_deref(), Some("page-001.psd"));
    }

    #[test]
    fn current_tab_storage_is_not_rewritten() {
        let current = json!({"tabs": [{"id": "tab-1", "name": "Tab 1", "text": "Current tab text"}], "currentTabId": "tab-1"});
        let m = migrate(&current, "Tab 1", || "unused".into());
        assert!(!m.migrated, "current tab storage must not be rewritten on startup");
        assert_eq!(Value::Array(m.tabs), current["tabs"], "unchanged tabs stay as they are");
    }

    #[test]
    fn legacy_fields_rewritten_after_a_downgrade_win() {
        let storage = json!({
            "tabs": [
                {"id": "tab-1", "name": "Tab 1", "text": "Stale tab text", "currentLineIndex": 2},
                {"id": "tab-2", "name": "Tab 2", "text": "Other tab text"},
            ],
            "currentTabId": "tab-1",
            "text": "Text rewritten by master",
            "currentLineIndex": 8,
        });
        let before = storage.clone();
        let m = migrate(&storage, "Tab 1", String::new);
        assert!(m.migrated, "legacy fields that differ from tabs must be migrated");
        assert_eq!(m.tabs[0]["text"], "Text rewritten by master");
        assert_eq!(m.tabs[0]["currentLineIndex"], 8);
        assert_eq!(m.tabs[1], storage["tabs"][1], "unrelated tabs stay untouched");
        assert_eq!(storage, before, "migration doesn't change its input");

        let cleared = json!({"tabs": [{"id": "tab-1", "name": "Tab 1", "text": "Stale text"}], "currentTabId": "tab-1", "text": ""});
        assert_eq!(migrate(&cleared, "Tab 1", String::new).tabs[0]["text"], "", "an intentionally cleared legacy text beats a stale tab");
    }

    #[test]
    fn unknown_current_tab_falls_back_to_the_first() {
        let s = json!({"tabs": [{"id": "a", "name": "A"}, {"id": "b", "name": "B"}], "currentTabId": "zz"});
        let m = migrate(&s, "Tab 1", String::new);
        assert!(m.migrated);
        assert_eq!(m.current_tab_id, "a");
        let m = migrate(&json!("garbage"), "Tab 1", || "t".into());
        assert_eq!((m.tabs.len(), m.current_tab_id.as_str()), (1, "t"));
        assert_eq!(m.tabs[0]["text"], "");
    }

    #[test]
    fn ids_and_names() {
        let tabs = vec![Tab { id: "tab-1".into(), ..Tab::default() }, Tab { id: "tab-3".into(), ..Tab::default() }];
        assert_eq!(next_id(&tabs), "tab-2");
        assert_eq!(default_name(2), "Tab 3");
        assert_eq!(clean_name("  Ch. 12 "), Some("Ch. 12".into()));
        assert_eq!(clean_name("   "), None);
        assert_eq!(clean_name(&"x".repeat(200)).map(|n| n.chars().count()), Some(MAX_TAB_NAME));
        let weird = Tab::from_value(
            "i".into(),
            "n".into(),
            &json!({"text": 3, "images": "x", "currentLineIndex": -1, "usedLineStyles": {"a": 1, "2": {"rawText": "r"}}}),
        );
        assert_eq!(weird, Tab { id: "i".into(), name: "n".into(), ..Tab::default() });
    }
}
