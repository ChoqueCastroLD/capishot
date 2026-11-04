//! Paint mask, then run: the Remove Tool and Remove with AI collect strokes into a mask instead
//! of running each one when it is released. The mask shows as a red tint; the eraser (its toggle,
//! or ⌥ held while drawing, as Photoshop's Remove Tool subtracts) takes strokes back out of it.
//! Run (↩, the options bar's or the touch bar's button) fills the whole mask at once through the
//! tool's usual command (`paint.remove`, `edit.removeAi`: Manga LaMa at its fixed best, full
//! resolution) as one undo step; Clear mask (Esc) drops it.
//!
//! The strokes are not history steps, as Photoshop's waiting Remove strokes aren't: Undo while a
//! mask waits undoes the last real edit and leaves the mask alone. Each tool keeps its own mask,
//! kept while another tool is in use and shown again when the tool comes back; it belongs to a
//! document, layer and channel, is hidden while another document is active, cleared when its
//! document closes, and dropped when its layer or channel target changes (it can't be moved to
//! another one). The Remove Tool's switch is Photoshop's *Remove after each stroke* off.

use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::state::{Tool, ToolOptions};
use crate::stroke_trail::Trail;
use crate::theme::Tokens;

/// The document, layer and channel a mask was painted on. A later target must not redirect the
/// mask's removal to a different layer or colour component.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RemoveTarget {
    doc: photocraft_doc::DocId,
    layer: Option<photocraft_doc::LayerId>,
    channel: photocraft_engine::channel_cmds::ChannelTarget,
    paint: Value,
}

/// One tool's waiting mask.
#[derive(Default)]
pub(crate) struct Mask {
    /// `paint.remove` `strokes` entries: the points and the size, hardness and spacing each was
    /// drawn with (which its tint shows and its removal uses), `erase` for the eraser's.
    pub(crate) strokes: Vec<Value>,
    pub(crate) target: Option<RemoveTarget>,
    /// The tint on the canvas.
    pub(crate) trail: Option<Trail>,
}

/// The Remove Tool's and Remove with AI's masks, and the eraser toggle (`PhotocraftApp::remove_mask`).
#[derive(Default)]
pub struct RemoveMasks {
    pub(crate) remove: Mask,
    pub(crate) remove_ai: Mask,
    /// The eraser: strokes take away from the mask (⌥ held flips it for a stroke).
    pub erase: bool,
}

/// The tools that can paint a mask.
pub fn is_mask_tool(tool: Tool) -> bool {
    matches!(tool, Tool::Remove | Tool::RemoveAi)
}

/// Whether `tool` paints a mask (Paint mask, then run on) instead of running each stroke.
pub fn mask_mode(o: &ToolOptions, tool: Tool) -> bool {
    match tool {
        Tool::Remove => !o.remove_after_each_stroke,
        Tool::RemoveAi => o.remove_ai_paint_mask,
        _ => false,
    }
}

/// Turn Paint mask, then run on or off for `tool`; off drops its waiting mask.
pub fn set_mask_mode(app: &mut PhotocraftApp, tool: Tool, on: bool) {
    match tool {
        Tool::Remove => app.ui.tool_options.remove_after_each_stroke = !on,
        Tool::RemoveAi => app.ui.tool_options.remove_ai_paint_mask = on,
        _ => return,
    }
    if !on && let Some(m) = mask_mut(app, tool) {
        *m = Mask::default();
    }
}

fn mask_mut(app: &mut PhotocraftApp, tool: Tool) -> Option<&mut Mask> {
    match tool {
        Tool::Remove => Some(&mut app.remove_mask.remove),
        Tool::RemoveAi => Some(&mut app.remove_mask.remove_ai),
        _ => None,
    }
}

fn mask(app: &PhotocraftApp, tool: Tool) -> Option<&Mask> {
    match tool {
        Tool::Remove => Some(&app.remove_mask.remove),
        Tool::RemoveAi => Some(&app.remove_mask.remove_ai),
        _ => None,
    }
}

fn target(app: &PhotocraftApp) -> Option<RemoveTarget> {
    let st = app.session.active()?;
    Some(RemoveTarget { doc: st.doc.id, layer: st.active_layer, channel: st.channel_view.target, paint: crate::canvas::paint_target(app) })
}

/// Whether a stroke started with `mods` erases from the mask: the eraser toggle, flipped by ⌥.
pub fn erasing(app: &PhotocraftApp, mods: egui::Modifiers) -> bool {
    app.remove_mask.erase != (mods.alt && !mods.ctrl)
}

/// A finished stroke of a mask tool: added to (or, erasing, taken from) the mask when the tool
/// paints one. False when it doesn't (the stroke runs as before).
pub fn finish_stroke(app: &mut PhotocraftApp, tool: Tool, points: &[[f64; 3]], mods: egui::Modifiers) -> bool {
    if !mask_mode(&app.ui.tool_options, tool) {
        return false;
    }
    let erase = erasing(app, mods);
    add(app, tool, points, erase);
    true
}

/// Add a stroke to `tool`'s mask (`erase`: take it out). A mask for another target is dropped
/// first; erasing from no mask does nothing.
pub fn add(app: &mut PhotocraftApp, tool: Tool, points: &[[f64; 3]], erase: bool) {
    if points.is_empty() {
        return;
    }
    let Some(target) = target(app) else { return };
    let Some(size) = app.session.active().map(|st| [st.doc.size.width, st.doc.size.height]) else { return };
    let b = app.session.tools.brush.clone();
    let mut entry = json!({"points": points, "size": b.size, "hardness": b.hardness * 100.0, "spacing": b.spacing * 100.0});
    if erase {
        entry["erase"] = json!(true);
    }
    let brush = || photocraft_engine::retouch_cmds::stroke_brush(&b, &entry);
    let Some(m) = mask_mut(app, tool) else { return };
    if m.target.as_ref() != Some(&target) {
        *m = Mask { target: Some(target), ..Mask::default() };
    }
    if erase && m.strokes.is_empty() {
        return;
    }
    let trail = m.trail.get_or_insert_with(|| Trail::new(size));
    if erase {
        trail.erase_stroke_reach(points, brush);
    } else {
        trail.feed_stroke_reach(points, brush);
    }
    m.strokes.push(entry);
    let n = m.strokes.len();
    app.ui.status = crate::i18n::fmt(tl!("Mask: {n} strokes (Enter runs, Esc clears)"), &[("n", &n.to_string())]);
    app.ui.status_error = false;
}

/// Drop masks whose document closed, and the current tool's when its target changed. Every frame.
fn prune(app: &mut PhotocraftApp) {
    let active = target(app);
    for tool in [Tool::Remove, Tool::RemoveAi] {
        let open = |d: photocraft_doc::DocId| app.session.index_of_document(d).is_some();
        let Some(m) = mask(app, tool) else { continue };
        let Some(t) = m.target.as_ref() else { continue };
        let closed = !open(t.doc);
        let off = !mask_mode(&app.ui.tool_options, tool);
        // Same document, other layer or channel, while the tool is in use: the strokes can't go
        // there (they were painted against the other target).
        let moved = app.ui.tool == tool && active.as_ref().is_some_and(|a| a.doc == t.doc && a != t);
        if (closed || off || moved)
            && let Some(m) = mask_mut(app, tool)
        {
            *m = Mask::default();
        }
    }
}

/// Whether the current tool's mask is waiting and shown (its document active, its target the
/// current one). Checked every frame.
pub fn live(app: &mut PhotocraftApp) -> bool {
    prune(app);
    let current = target(app);
    mask(app, app.ui.tool).is_some_and(|m| !m.strokes.is_empty() && m.target.is_some() && m.target == current)
}

/// Run the current tool's mask: the whole mask at once, one job and one undo step. False when no
/// mask waits.
pub fn run(app: &mut PhotocraftApp) -> bool {
    if !live(app) {
        return false;
    }
    let tool = app.ui.tool;
    let Some(m) = mask_mut(app, tool).map(std::mem::take) else { return false };
    if m.trail.as_ref().is_none_or(Trail::is_empty) {
        app.ui.status = tl!("The mask is empty: paint over what to remove").into();
        app.ui.status_error = true;
        return true;
    }
    app.remove_mask.erase = false;
    let target = crate::canvas::paint_target(app);
    app.ui.status.clear();
    app.ui.status_error = false;
    match tool {
        Tool::RemoveAi => {
            let size = app.session.tools.brush.size;
            let p = json!({"sampleAllLayers": app.ui.tool_options.sample_all.remove_ai, "strokes": m.strokes, "size": size, "target": target});
            crate::remove_ai_ui::request(app, p);
        }
        _ => {
            let mut p = crate::retouch_ui::remove_params(&app.ui.tool_options);
            p["strokes"] = json!(m.strokes);
            p["target"] = target;
            // The AI mode runs only with its model downloaded (else the missing-model dialog).
            if let Err(e) = crate::ai_model_gate::run(app, "paint.remove", p) {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
    }
    true
}

/// Drop the current tool's mask. False when none waits.
pub fn clear(app: &mut PhotocraftApp) -> bool {
    if !live(app) {
        return false;
    }
    if let Some(m) = mask_mut(app, app.ui.tool) {
        *m = Mask::default();
    }
    app.remove_mask.erase = false;
    app.ui.status.clear();
    app.ui.status_error = false;
    true
}

/// The waiting mask's tint, drawn every frame.
pub fn draw(app: &mut PhotocraftApp, painter: &egui::Painter, xf: &ViewXform) {
    if !live(app) {
        return;
    }
    let Some(bounds) = app.session.active().map(|st| st.doc.bounds()) else { return };
    let doc_rect = xf.doc_rect(bounds);
    let col = Tokens::get(painter.ctx()).danger.gamma_multiply(0.45);
    if let Some(trail) = mask_mut(app, app.ui.tool).and_then(|m| m.trail.as_mut()) {
        trail.draw(painter, doc_rect, xf.flip, col);
    }
}

/// The tint of a drag that erases from the mask: a light veil, not the red of what is removed.
pub fn erase_tint() -> egui::Color32 {
    egui::Color32::from_white_alpha(70)
}

/// The options bar's part: the switch, then with it on the eraser, and Run and Clear mask.
pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui, tool: Tool) {
    let mut on = mask_mode(&app.ui.tool_options, tool);
    let tip = tl!("Strokes collect into a mask (tinted red) instead of running when released. Run fills the whole mask at once, as one undo step. Photoshop: Remove after each stroke, off.");
    if crate::widgets::checkbox(ui, &mut on, tl!("Paint mask, then run")).on_hover_text(tip).changed() {
        set_mask_mode(app, tool, on);
    }
    if !on {
        return;
    }
    let mut erase = app.remove_mask.erase;
    let tip = if cfg!(target_os = "macos") {
        tl!("Strokes take away from the mask (hold Option for one stroke)")
    } else {
        tl!("Strokes take away from the mask (hold Alt for one stroke)")
    };
    if crate::widgets::checkbox(ui, &mut erase, tl!("Erase from mask")).on_hover_text(tip).changed() {
        app.remove_mask.erase = erase;
    }
    let pending = live(app);
    let run_tip = crate::i18n::fmt(tl!("Fill the whole mask at once ({key})"), &[("key", &crate::shortcuts::pretty("Enter"))]);
    if ui.add_enabled_ui(pending, |ui| crate::widgets::primary_button(ui, tl!("Run"), 0.0)).inner.on_hover_text(run_tip).clicked() {
        run(app);
    }
    if ui.add_enabled_ui(pending, |ui| crate::widgets::secondary_button(ui, tl!("Clear mask"), 0.0)).inner.on_hover_text(tl!("Drop the mask (Esc)")).clicked() {
        clear(app);
    }
}

/// The control channel's view of the masks (`ui.inspect` `removeMask`, `ui.removeMask`).
pub fn inspect(app: &PhotocraftApp) -> Value {
    let one = |tool: Tool| {
        let m = mask(app, tool);
        let strokes = m.map_or(0, |m| m.strokes.len());
        let erased = m.map_or(0, |m| m.strokes.iter().filter(|e| e.get("erase").and_then(Value::as_bool) == Some(true)).count());
        json!({
            "on": mask_mode(&app.ui.tool_options, tool),
            "strokes": strokes,
            "erased": erased,
            "empty": m.and_then(|m| m.trail.as_ref()).is_none_or(Trail::is_empty),
            "document": m.and_then(|m| m.target.as_ref()).map(|t| t.doc.0),
        })
    };
    json!({"remove": one(Tool::Remove), "removeAi": one(Tool::RemoveAi), "erase": app.remove_mask.erase})
}

/// `ui.removeMask`: `{"action": "get"}` (default) reads the masks, `"strokes"` also returns the
/// current tool's `strokes` entries (what Run sends), `"clear"` drops the current tool's, `"run"`
/// runs it; `"on": bool` switches Paint mask, then run for the current tool (a Remove tool),
/// `"erase": bool` sets the eraser toggle. Checked before anything changes.
pub fn control(app: &mut PhotocraftApp, p: &Value) -> Result<Value, String> {
    let on = match p.get("on") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_bool().ok_or("`on` must be a boolean")?),
    };
    let erase = match p.get("erase") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_bool().ok_or("`erase` must be a boolean")?),
    };
    if on.is_some() && !is_mask_tool(app.ui.tool) {
        return Err("`on` needs the Remove Tool or Remove with AI selected".into());
    }
    if let Some(on) = on {
        set_mask_mode(app, app.ui.tool, on);
    }
    if let Some(e) = erase {
        app.remove_mask.erase = e;
    }
    let action = match p.get("action") {
        None | Some(Value::Null) => "get",
        Some(v) => v.as_str().ok_or("`action` must be get, strokes, clear or run")?,
    };
    let done = match action {
        "get" => None,
        "strokes" => {
            let live = live(app);
            let strokes = mask(app, app.ui.tool).filter(|_| live).map_or(json!([]), |m| json!(m.strokes));
            let mut v = inspect(app);
            v["current"] = json!(strokes);
            return Ok(v);
        }
        "clear" => Some(clear(app)),
        "run" => Some(run(app)),
        other => return Err(format!("unknown `action` {other:?}: get, strokes, clear or run")),
    };
    let mut v = inspect(app);
    if let Some(d) = done {
        v["done"] = json!(d);
    }
    Ok(v)
}

#[cfg(test)]
#[path = "remove_mask_ui/tests.rs"]
mod tests;
