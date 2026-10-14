//! TypeR's keyboard shortcuts in Capishot (project decision, 2026-10-12): **Photoshop's
//! shortcuts always win.** No `typer.*` command has a default key. The optional "TypeR keymap"
//! (off by default; Edit › Keyboard Shortcuts, or TypeR Settings › Shortcuts) assigns TypeR's
//! defaults where Photoshop leaves the key free, and an alternative where it doesn't:
//!
//! - TypeR's modifier-only chords (`Win+Ctrl` paste, `Win+Shift` apply, `Win+Alt` align) need
//!   global key polling; Capishot's shortcuts always end in a key, and the browser and Linux
//!   desktops take `Super`. They become `Ctrl+Alt` + a key Photoshop doesn't use.
//! - `Ctrl+Enter` (Photoshop: commit type / path to selection), `Ctrl+Tab` (next document) and
//!   `Ctrl+H` (Extras) stay Photoshop's; TypeR's next / previous line and cleaning-layers toggle
//!   get free alternatives. `Ctrl+Shift+Plus` / `Minus` would zoom (`=` matches `+`), so the size
//!   keys are `Ctrl+Alt+.` / `Ctrl+Alt+,`, next to Photoshop's own type-size keys.
//! - `Shift+X` (next page) and `Ctrl+Alt+M` (multi-bubble) are TypeR's own and free.
//!
//! The preset is a set of Edit › Keyboard Shortcuts overrides: turning it off removes the ones
//! still at the preset's keys, so the user's other changes stay.

use std::collections::BTreeMap;

/// A TypeR action: (command id, label, TypeR's default key, the preset's key).
pub type Entry = (&'static str, &'static str, &'static str, &'static str);

/// TypeR's shortcut table and the preset (Capishot notation: `Cmd` is ⌘ on macOS, Ctrl
/// elsewhere; `Ctrl` is the Control key on every platform).
pub const ENTRIES: &[Entry] = &[
    ("typer.ui.paste", "TypeR Paste", "Win+Ctrl", "Cmd+Alt+B"),
    ("typer.apply", "TypeR Apply", "Win+Shift", "Cmd+Alt+J"),
    ("typer.align", "TypeR Align", "Win+Alt", "Cmd+Alt+X"),
    ("typer.ui.insertText", "Paste Text to the Current Layer", "Win+Shift+V", "Cmd+Alt+Shift+J"),
    ("typer.line.next", "Next Line", "Ctrl+Enter", "Cmd+Alt+Enter"),
    ("typer.line.previous", "Previous Line", "Ctrl+Tab", "Cmd+Alt+Shift+Enter"),
    ("typer.page.next", "Next Page", "Shift+X", "Shift+X"),
    ("typer.ui.increaseSize", "Increase TypeR Text Size", "Ctrl+Shift+Plus", "Cmd+Alt+."),
    ("typer.ui.decreaseSize", "Decrease TypeR Text Size", "Ctrl+Shift+Minus", "Cmd+Alt+,"),
    ("typer.multiBubble.toggle", "TypeR Multi-Bubble Mode", "Ctrl+Alt+M", "Ctrl+Alt+M"),
    ("typer.toggleCleaningLayers", "TypeR Toggle Cleaning Layers", "Ctrl+H", "Cmd+Alt+Shift+H"),
];

/// TypeR commands listed in Edit › Keyboard Shortcuts (under Type › TypeR) besides the menu's:
/// the preset's plus the ones TypeR binds no key to.
pub const BINDABLE: &[(&str, &str)] = &[
    ("typer.ui.applyStyle", "Apply Style to the Current Layer"),
    ("typer.applyLines", "TypeR Apply Lines"),
    ("typer.multiBubble.removeLast", "TypeR Remove Last Bubble"),
    ("typer.multiBubble.clear", "TypeR Clear Bubbles"),
    ("typer.style.nextSizePreset", "Next Size Preset"),
];

/// The preset's key for each command, normalised like stored shortcuts.
pub fn preset() -> Vec<(&'static str, String)> {
    ENTRIES.iter().filter_map(|e| photocraft_engine::prefs::normalize_shortcut(e.3).map(|k| (e.0, k))).collect()
}

/// Is the preset on in `overrides` (every preset key assigned)?
pub fn is_on(overrides: &BTreeMap<String, String>) -> bool {
    preset().iter().all(|(id, key)| overrides.get(*id).and_then(|s| photocraft_engine::prefs::normalize_shortcut(s)).as_deref() == Some(key.as_str()))
}

/// Turn the preset on or off in `overrides`. Off removes only the overrides still at the
/// preset's keys.
pub fn set(overrides: &mut BTreeMap<String, String>, on: bool) {
    for (id, key) in preset() {
        if on {
            overrides.insert(id.to_string(), key);
        } else if overrides.get(id).and_then(|s| photocraft_engine::prefs::normalize_shortcut(s)).as_deref() == Some(key.as_str()) {
            overrides.remove(id);
        }
    }
}

/// Apply the preset to the preferences now (TypeR Settings › Shortcuts).
pub fn apply(app: &mut crate::PhotocraftApp, on: bool) -> Result<(), String> {
    let mut overrides = app.session.prefs().shortcuts.clone();
    set(&mut overrides, on);
    let reset: Vec<&str> = ENTRIES.iter().map(|e| e.0).filter(|id| !overrides.contains_key(*id)).collect();
    let set: serde_json::Map<String, serde_json::Value> =
        ENTRIES.iter().filter_map(|e| overrides.get(e.0).map(|k| (e.0.to_string(), serde_json::json!(k)))).collect();
    app.run("edit.keyboardShortcuts", serde_json::json!({"reset": reset, "set": set, "allowUnknown": true, "removeConflicts": false})).map(|_| ())
}

/// Rows for Edit › Keyboard Shortcuts: (id, label, menu path, default) for the TypeR commands
/// `listed` doesn't have yet.
pub fn shortcut_items(listed: &std::collections::HashSet<&str>) -> Vec<(String, String, Vec<String>, Option<String>)> {
    ENTRIES
        .iter()
        .map(|e| (e.0, e.1))
        .chain(BINDABLE.iter().copied())
        .filter(|(id, _)| !listed.contains(id))
        .map(|(id, label)| (id.to_string(), label.to_string(), vec!["Type".to_string(), "TypeR".to_string()], None))
        .collect()
}

/// The "TypeR keymap" checkbox of Edit › Keyboard Shortcuts: edits the dialog's overrides.
pub fn dialog_toggle(ui: &mut egui::Ui, overrides: &mut BTreeMap<String, String>, message: &mut String) {
    let mut on = is_on(overrides);
    let r = crate::widgets::checkbox(ui, &mut on, tl!("TypeR keymap")).on_hover_text(tl!(
        "TypeR's shortcuts where Photoshop leaves the key free, and Ctrl+Alt alternatives where it doesn't (Paste Ctrl+Alt+B, Apply Ctrl+Alt+J, Align Ctrl+Alt+X…)"
    ));
    if r.changed() {
        set(overrides, on);
        *message = if on { tl!("TypeR keymap on. Click OK to keep it.").into() } else { tl!("TypeR keymap off. Click OK to keep it.").into() };
    }
}
