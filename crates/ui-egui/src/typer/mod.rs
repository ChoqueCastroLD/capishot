//! The TypeR panel (Window › TypeR): TypeR's three blocks for manga typesetting, natively.
//!
//! - **Preview** ([`preview`]): Paste / Align, text size −/+, the current line in its style's font,
//!   previous / next line, text scale, apply to the active layer, the keep-size and multi-bubble
//!   badges.
//! - **Script** ([`script_editor`]): the translated chapter with line numbers, page markers, tag
//!   chips and the current line highlighted; a click in the gutter makes a line current; ⌘B / ⌘I
//!   and the toolbar toggle Markdown bold / italic; a `.txt` imports.
//! - **Styles** ([`styles`]): the style library as a folder tree, each style's name in its font,
//!   tag and colour chips, size presets and a quick −/+; a right-click copies, pastes,
//!   duplicates, edits or deletes. The style editor ([`style_editor`]), folder editor
//!   ([`folder_editor`]) and settings ([`settings`]) are dialogs.
//!
//! The panel is thin (AGENTS.md rule 5): it draws the engine's TypeR session
//! (`Session::typer`, read-only) and changes it only through `typer.*` commands. Its own view
//! state is [`TyperUi`] in the serialisable UI state. The TypeR workspace (script, cursor,
//! library, settings) persists through [`crate::Services::load_typer`] / `save_typer`
//! ([`persist`]). Keyboard shortcuts and the optional TypeR keymap are in [`keymap`].
//!
//! Layout and behaviour follow TypeR (MIT, (c) 2017 Swirt; `app_src/components`), redrawn with
//! Capishot's widgets, theme tokens and Lucide icons; no TypeR code, styles or icons are used.

pub(crate) mod folder_editor;
mod font_label;
pub mod keymap;
pub mod pages;
mod preview;
pub mod script_editor;
mod settings;
pub(crate) mod style_editor;
mod styles;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use egui::{Color32, RichText, Sense, Stroke, StrokeKind, vec2};
use photocraft_engine::typer_state::TyperSession;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;

/// The TypeR panel's view state (dialogs, collapsed folders, splitter). The TypeR data itself is
/// engine state (`Session::typer`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TyperUi {
    /// Collapsed style folders by id (`""` = Unsorted).
    pub collapsed: Vec<String>,
    /// The script's share of the room the script and the style list split (0.15–0.85; drag the
    /// Styles bar between them). A share, so both grow with the dock group.
    pub script_share: f32,
    /// Blocks folded to their bar: `"preview"` (the line drawn in its font), `"script"`,
    /// `"styles"`. The open ones take the room.
    pub folded: Vec<String>,
    /// The style editor, when open.
    pub style_editor: Option<style_editor::StyleDraft>,
    /// The folder editor, when open.
    pub folder_editor: Option<folder_editor::FolderDraft>,
    /// The settings dialog, when open.
    pub settings: Option<settings::SettingsDraft>,
    /// A yes/no question waiting for an answer.
    pub confirm: Option<Confirm>,
    /// A style copied from the style menu (TypeR style JSON), for Paste.
    pub clipboard: Option<Value>,
    /// The panel's last error, shown under the preview until the next successful action.
    pub message: String,
    /// Page files picked in the browser, in memory (see [`pages`]); never saved.
    #[serde(skip)]
    pub page_files: pages::PageFiles,
}

impl Default for TyperUi {
    fn default() -> Self {
        Self {
            collapsed: Vec::new(),
            script_share: 0.5,
            folded: Vec::new(),
            style_editor: None,
            folder_editor: None,
            settings: None,
            confirm: None,
            clipboard: None,
            message: String::new(),
            page_files: pages::PageFiles::default(),
        }
    }
}

/// A question the panel asks before acting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Confirm {
    /// Delete a style.
    DeleteStyle { id: Value, name: String },
    /// Delete a folder; `permanent` also deletes its styles (TypeR's Shift + Delete).
    DeleteFolder { id: Value, name: String, permanent: bool },
    /// A full TypeR import: keep the local styles missing from it or edited later?
    ImportKeepLocal { json: String },
}

/// Panel commands that wrap engine commands with the panel's routing and settings (bindable in
/// Edit › Keyboard Shortcuts; see [`keymap`]).
pub const UI_COMMANDS: [&str; 5] = ["typer.ui.paste", "typer.ui.insertText", "typer.ui.applyStyle", "typer.ui.increaseSize", "typer.ui.decreaseSize"];

/// Ids handled by [`invoke`].
pub fn handles(id: &str) -> bool {
    UI_COMMANDS.contains(&id)
}

/// The engine command a panel command runs now, with its parameters.
fn target(app: &PhotocraftApp, id: &str) -> Option<(&'static str, Value)> {
    let step = app.session.typer.state.settings.text_size_increment;
    Some(match id {
        // TypeR's "add": the engine's Paste fills the stored bubbles in multi-bubble mode, or
        // gives one line per selected text layer with several selected and no selection.
        "typer.ui.paste" => ("typer.paste", json!({})),
        "typer.ui.insertText" => ("typer.apply", json!({"mode": "textOnly"})),
        "typer.ui.applyStyle" => ("typer.applyStyle", json!({})),
        "typer.ui.increaseSize" => ("typer.resize", json!({"delta": step})),
        "typer.ui.decreaseSize" => ("typer.resize", json!({"delta": -step})),
        _ => return None,
    })
}

/// Run a panel command (menus, shortcuts, automation). `None` when `id` isn't one.
pub fn invoke(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let (cmd, mut p) = target(app, id)?;
    // Explicit parameters win over the panel's (e.g. `{"keepSize": true}`).
    if let (Some(p), Some(extra)) = (p.as_object_mut(), params.as_object()) {
        p.extend(extra.clone());
    }
    Some(run(app, cmd, p))
}

/// Can a panel command run now? `None` when `id` isn't one.
pub fn is_enabled(app: &PhotocraftApp, id: &str) -> Option<bool> {
    let (cmd, _) = target(app, id)?;
    Some(app.session.is_enabled(cmd))
}

/// Run a `typer.*` command for the panel: the panel's message shows an error, a success clears it.
pub(crate) fn run(app: &mut PhotocraftApp, id: &str, params: Value) -> Result<Value, String> {
    let r = app.run(id, params);
    match &r {
        Ok(_) => app.ui.typer.message.clear(),
        Err(e) => app.ui.typer.message = e.clone(),
    }
    r
}

/// [`run`] for buttons, whose result is only the message.
pub(crate) fn act(app: &mut PhotocraftApp, id: &str, params: Value) {
    let _ = run(app, id, params);
}

/// `params` plus `"now"`: the shell's wall clock in ms, for the `edited` stamps the engine can't
/// take on the web (no clock in wasm). Without a clock service the engine uses its own.
pub(crate) fn with_now(app: &PhotocraftApp, mut params: Value) -> Value {
    if let (Some(clock), Some(p)) = (app.services.clock, params.as_object_mut()) {
        let now = clock();
        if now.is_finite() && now > 0.0 {
            p.insert("now".into(), json!(now.floor()));
        }
    }
    params
}

/// The dock body: preview, script and styles, top to bottom, filling the group.
pub fn panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
    let full = ui.available_rect_before_wrap();
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        preview::show(app, ui);
        if !app.ui.typer.message.is_empty() {
            let msg = app.ui.typer.message.clone();
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(msg).color(t.warning).size(11.0)).wrap());
                if crate::icons::button(ui, "x", 16.0, false, tl!("Close")).clicked() {
                    app.ui.typer.message.clear();
                }
            });
        }
        crate::widgets::hairline(ui);
        // What the script and the styles list share once the script's toolbar, the Styles bar
        // and the footer (about 90 points with spacing) have theirs, so the footer stays in a
        // short group. A folded block gives its share to the other.
        let lists = (full.bottom() - ui.cursor().top() - 90.0).max(60.0);
        let (script_open, styles_open) = (!is_folded(app, "script"), !is_folded(app, "styles"));
        let script_h = match (script_open, styles_open) {
            (true, true) => (lists * app.ui.typer.script_share.clamp(0.15, 0.85)).clamp(36.0, (lists - 30.0).max(36.0)),
            (true, false) => lists,
            _ => 0.0,
        };
        script_editor::show(app, ui, script_h);
        styles_bar(app, ui, lists, script_open && styles_open);
        if styles_open {
            let styles_h = if script_open { (lists - script_h).max(30.0) } else { lists };
            styles::show(app, ui, styles_h);
        }
        footer_row(app, ui);
    });
}

/// Is the panel block `block` (`"preview"`, `"script"`, `"styles"`) folded?
pub(crate) fn is_folded(app: &PhotocraftApp, block: &str) -> bool {
    app.ui.typer.folded.iter().any(|b| b == block)
}

/// Fold or unfold a panel block.
pub(crate) fn toggle_fold(app: &mut PhotocraftApp, block: &str) {
    if is_folded(app, block) {
        app.ui.typer.folded.retain(|b| b != block);
    } else {
        app.ui.typer.folded.push(block.to_string());
    }
}

/// The chevron that folds a block to its bar (and unfolds it).
pub(crate) fn fold_button(app: &mut PhotocraftApp, ui: &mut egui::Ui, block: &str) {
    let folded = is_folded(app, block);
    let (icon, tip) = if folded { ("chevron-right", tl!("Expand")) } else { ("chevron-down", tl!("Collapse")) };
    if crate::icons::button_with_icon_size(ui, icon, 18.0, 13.0, false, tip).clicked() {
        toggle_fold(app, block);
    }
}

/// The Styles bar between the script and the styles: its chevron folds the style list, its
/// label counts the styles, and dragging it (with both blocks open) moves the split.
fn styles_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui, lists: f32, draggable: bool) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        fold_button(app, ui, "styles");
        let n = app.session.typer.state.library.styles.len();
        ui.label(RichText::new(format!("{} ({n})", tl!("Styles"))).color(t.text_dim).size(11.5));
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), if draggable { Sense::drag() } else { Sense::hover() });
        if !draggable {
            return;
        }
        let hot = resp.hovered() || resp.dragged();
        if hot {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        let c = rect.center();
        ui.painter().line_segment([egui::pos2(c.x - 14.0, c.y), egui::pos2(c.x + 14.0, c.y)], Stroke::new(2.0, if hot { t.accent } else { t.separator }));
        if resp.dragged() && lists > 1.0 {
            let share = app.ui.typer.script_share.clamp(0.15, 0.85) + resp.drag_delta().y / lists;
            if share.is_finite() {
                app.ui.typer.script_share = share.clamp(0.15, 0.85);
            }
        }
    });
}

/// Add folder / style, import, multi-bubble, settings.
fn footer_row(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    crate::widgets::hairline(ui);
    ui.horizontal(|ui| {
        if crate::icons::button_with_icon_size(ui, "folder-plus", 24.0, 15.0, false, tl!("Add folder")).clicked() {
            folder_editor::open_new(app, None);
        }
        if crate::icons::button_with_icon_size(ui, "plus", 24.0, 15.0, false, tl!("Add style")).clicked() {
            style_editor::open_new(app);
        }
        if crate::icons::button_with_icon_size(ui, "upload", 24.0, 15.0, false, tl!("Import styles or settings…")).clicked() {
            settings::import(app);
        }
        pages::footer(app, ui);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::icons::button_with_icon_size(ui, "settings", 24.0, 15.0, app.ui.typer.settings.is_some(), tl!("TypeR Settings")).clicked() {
                settings::open(app);
            }
            // Multi-bubble mode (TypeR's footer "MB ON/OFF").
            let on = app.session.typer.state.settings.multi_bubble_mode;
            let label = if on { tl!("MB ON") } else { tl!("MB OFF") };
            let resp = chip(ui, label, on, &t).on_hover_text(tl!("Multi-bubble mode: capture several bubbles, then paste them all at once"));
            if resp.clicked() {
                // The toggle also drops the stored bubbles; until placement lands, the setting.
                if app.session.is_enabled("typer.multiBubble.toggle") {
                    act(app, "typer.multiBubble.toggle", json!({}));
                } else {
                    act(app, "typer.settings.set", json!({"key": "multiBubbleMode", "value": !on}));
                }
            }
        });
    });
}

/// A small rounded toggle chip.
pub(crate) fn chip(ui: &mut egui::Ui, label: &str, on: bool, t: &Tokens) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(label.to_owned(), egui::FontId::proportional(11.0), if on { t.accent_text } else { t.text_dim });
    let size = vec2(galley.size().x + 12.0, 18.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if on {
        t.accent_soft
    } else if resp.hovered() {
        t.hover
    } else {
        t.field
    };
    ui.painter().rect_filled(rect, 9.0, fill);
    ui.painter().rect_stroke(rect, 9.0, Stroke::new(1.0, if on { t.accent_border } else { t.field_border }), StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, Color32::PLACEHOLDER);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label));
    resp
}

/// An icon button with a text label, for the preview's Paste and Align.
pub(crate) fn icon_label_button(ui: &mut egui::Ui, icon: &str, label: &str, primary: bool, enabled: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let fg = if !enabled {
        t.text_faint
    } else if primary {
        t.primary_text
    } else {
        t.text
    };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), crate::theme::medium(12.5), fg);
    let size = vec2(galley.size().x + 34.0, 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let fill = match (primary && enabled, resp.hovered() && enabled) {
        (true, true) => t.primary_bg.gamma_multiply(0.9),
        (true, false) => t.primary_bg,
        (false, true) => t.hover,
        (false, false) => t.field,
    };
    ui.painter().rect_filled(rect, t.radius_sm, fill);
    if !(primary && enabled) {
        ui.painter().rect_stroke(rect, t.radius_sm, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    }
    let icon_rect = egui::Rect::from_center_size(egui::pos2(rect.left() + 15.0, rect.center().y), vec2(16.0, 16.0));
    crate::icons::paint(ui, icon_rect, icon, 16.0, fg);
    ui.painter().galley(egui::pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0), galley, fg);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    resp
}

/// A TypeR colour (`#RRGGBB`, the tag colours) as a colour; `None` when malformed.
pub(crate) fn hex_color(s: &str) -> Option<Color32> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let c = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
    Some(Color32::from_rgb(c(0)?, c(2)?, c(4)?))
}

/// A TypeR `{red, green, blue}` (text colour) or `{r, g, b}` (stroke colour) object as a colour.
pub(crate) fn rgb_color(v: Option<&Value>) -> Option<Color32> {
    let o = v?.as_object()?;
    let ch = |a: &str, b: &str| o.get(a).or_else(|| o.get(b)).and_then(Value::as_f64).map(|x| x.clamp(0.0, 255.0).round() as u8);
    Some(Color32::from_rgb(ch("red", "r")?, ch("green", "g")?, ch("blue", "b")?))
}

/// Draws the TypeR dialogs (style, folder, settings, confirmations).
pub fn windows(app: &mut PhotocraftApp, ctx: &egui::Context) {
    style_editor::window(app, ctx);
    folder_editor::window(app, ctx);
    settings::window(app, ctx);
    confirm_window(app, ctx);
}

/// The frame of the TypeR dialogs.
pub(crate) fn dialog_frame(t: &Tokens) -> egui::Frame {
    egui::Frame::NONE
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.card_border))
        .corner_radius(egui::CornerRadius::same(t.radius_lg as u8))
        .shadow(egui::Shadow { offset: [0, 10], blur: 30, spread: 0, color: t.shadow })
        .inner_margin(egui::Margin::same(14))
}

fn confirm_window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(c) = app.ui.typer.confirm.clone() else { return };
    let t = Tokens::get(ctx);
    let (title, text, yes, no) = match &c {
        Confirm::DeleteStyle { name, .. } => {
            (tl!("Delete style"), format!("{} \u{201c}{name}\u{201d}", tl!("Are you sure that you want to delete the style?")), tl!("Delete"), None)
        }
        Confirm::DeleteFolder { name, permanent, .. } => (
            tl!("Delete folder"),
            format!(
                "\u{201c}{name}\u{201d}: {}",
                if *permanent {
                    tl!("Delete the folder, its subfolders and all their styles?")
                } else {
                    tl!("Delete the folder and its subfolders? Their styles move to Unsorted.")
                }
            ),
            tl!("Delete"),
            None,
        ),
        Confirm::ImportKeepLocal { .. } => {
            (tl!("Import styles"), tl!("Keep the local styles that are not in the import or were edited later?").to_string(), tl!("Keep"), Some(tl!("Replace")))
        }
    };
    let mut answer: Option<crate::widgets::ButtonRole> = None;
    egui::Modal::new(egui::Id::new("typer-confirm")).frame(dialog_frame(&t)).show(ctx, |ui| {
        ui.set_width(340.0);
        ui.label(RichText::new(title).color(t.text).size(13.0).strong());
        ui.add_space(6.0);
        ui.add(egui::Label::new(RichText::new(text).color(t.text_dim)).wrap());
        ui.add_space(10.0);
        use crate::widgets::{ButtonRole, DialogButton};
        let mut buttons = vec![DialogButton::new(ButtonRole::Default, yes, 80.0)];
        if let Some(no) = no {
            buttons.push(DialogButton::new(ButtonRole::Alternate, no, 80.0));
        }
        buttons.push(DialogButton::new(ButtonRole::Cancel, tl!("Cancel"), 80.0));
        answer = crate::widgets::dialog_buttons(ui, &buttons);
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            answer = Some(ButtonRole::Cancel);
        }
    });
    let Some(answer) = answer else { return };
    app.ui.typer.confirm = None;
    use crate::widgets::ButtonRole;
    if answer == ButtonRole::Cancel || answer == ButtonRole::Apply {
        return;
    }
    let yes = answer == ButtonRole::Default;
    match c {
        Confirm::DeleteStyle { id, .. } => {
            if run(app, "typer.style.delete", json!({"id": id})).is_ok() {
                app.ui.typer.style_editor = None;
            }
        }
        Confirm::DeleteFolder { id, permanent, .. } => {
            if run(app, "typer.folder.delete", json!({"id": id, "permanent": permanent})).is_ok() {
                app.ui.typer.folder_editor = None;
            }
        }
        Confirm::ImportKeepLocal { json } => {
            let p = with_now(app, json!({"json": json, "kind": "full", "keepLocal": yes}));
            act(app, "typer.library.import", p);
        }
    }
}

/// Seconds between checks for TypeR workspace changes to save.
const SAVE_EVERY: f64 = 1.0;

/// What [`persist`] remembers between frames.
#[derive(Clone, Copy, Default)]
struct PersistState {
    restored: bool,
    last_check: f64,
    saved: u64,
}

fn text_hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// The TypeR workspace as saved (library, settings, script, cursor, page files).
pub(crate) fn workspace_text(typer: &TyperSession) -> String {
    serde_json::to_string(&typer.to_json()).unwrap_or_default()
}

/// Restore the TypeR workspace once (the first frame), then save it about once a second while it
/// changes and no button is held. Only the workspace's hash is kept between checks.
pub fn persist(app: &mut PhotocraftApp, ctx: &egui::Context) {
    capture_bubbles(app, ctx);
    pages::sync(app, ctx);
    let id = egui::Id::new("typer-persist");
    let mut st = ctx.data(|d| d.get_temp::<PersistState>(id)).unwrap_or_default();
    let now = ctx.input(|i| i.time);
    if !st.restored {
        st.restored = true;
        st.last_check = now;
        restore(app);
        st.saved = if app.services.save_typer.is_some() { text_hash(&workspace_text(&app.session.typer)) } else { 0 };
    } else if app.services.save_typer.is_some() && now - st.last_check >= SAVE_EVERY && !ctx.input(|i| i.pointer.any_down()) {
        st.last_check = now;
        let text = workspace_text(&app.session.typer);
        let hash = text_hash(&text);
        if hash != st.saved {
            // Remembered even on failure, so a full storage reports once rather than every second.
            st.saved = hash;
            if let Some(save) = app.services.save_typer.as_mut()
                && let Err(e) = save(&text)
            {
                app.ui.status = format!("{}: {e}", tl!("Couldn't save the TypeR workspace"));
                app.ui.status_error = true;
            }
        }
    }
    ctx.data_mut(|d| d.insert_temp(id, st));
}

/// Load the saved TypeR workspace into a session that hasn't used TypeR yet.
fn restore(app: &mut PhotocraftApp) {
    let Some(text) = app.services.load_typer.as_mut().and_then(|load| load()) else { return };
    if app.session.typer != TyperSession::default() {
        return;
    }
    match serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()).and_then(|v| TyperSession::from_json(&v)) {
        Ok(s) => app.session.typer = s,
        Err(e) => {
            log::warn!("TypeR workspace not restored: {e}");
            app.ui.status = format!("{}: {e}", tl!("Couldn't restore the TypeR workspace"));
            app.ui.status_error = true;
        }
    }
}

/// What [`capture_bubbles`] saw last: (document, revision) and the selection's bounds.
#[derive(Clone, Copy, Default, PartialEq)]
struct SeenSelection {
    doc: Option<(photocraft_doc::DocId, u64)>,
    bounds: Option<[i32; 4]>,
}

/// Multi-bubble mode (TypeR M1): each new selection is stored as a bubble with the current line
/// and style (`typer.multiBubble.capture`), once the gesture that made it is over. Only a change of
/// the selection's bounds counts, so a paste or another edit doesn't store the same bubble again;
/// with ⇧ held (adding to a selection) nothing is stored, as in TypeR. The bounds are read only
/// when the document's revision moves.
fn capture_bubbles(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.session.typer.state.settings.multi_bubble_mode {
        return;
    }
    let id = egui::Id::new("typer-seen-selection");
    let mut seen = ctx.data(|d| d.get_temp::<SeenSelection>(id)).unwrap_or_default();
    let Some(st) = app.session.active() else { return };
    let key = Some((st.doc.id, st.revision));
    if seen.doc == key || ctx.input(|i| i.pointer.any_down()) {
        return;
    }
    let bounds =
        st.doc.selection.as_ref().map(photocraft_compose::bounds::content_bounds).filter(|r| r.x1 > r.x0 && r.y1 > r.y0).map(|r| [r.x0, r.y0, r.x1, r.y1]);
    let first = seen.doc.is_none_or(|(doc, _)| doc != st.doc.id);
    let changed = bounds.is_some() && bounds != seen.bounds && !first;
    seen = SeenSelection { doc: key, bounds };
    ctx.data_mut(|d| d.insert_temp(id, seen));
    if !changed {
        return;
    }
    if ctx.input(|i| i.modifiers.shift) {
        app.ui.typer.message = tl!("Multi-bubble mode stores one selection at a time: release Shift and select the bubbles one by one.").into();
        return;
    }
    act(app, "typer.multiBubble.capture", json!({}));
}
