//! The Text Selection tool (W group): the mask model finds the page's text and sound effects as
//! pieces, on the whole page or inside the selection, and a person chooses. The model also marks
//! art, so its doubtful pieces start unchosen and nothing runs by itself.
//!
//! The selection is a normal one, and what's chosen is read from it: Undo, the Lasso and
//! Select › Modify all count. A click on a piece adds or takes it; a drag paints the selection
//! (⌥ takes away), with the brush's size. The contour is how far a selection reaches past the
//! letters: too close and cleaning copies their dark edge back as a smudge. The bar on the canvas
//! cleans what's selected with Manga LaMa at its fixed best (`edit.removeAi`, as Remove with AI),
//! fills it by content, or leaves it as a selection.
//!
//! All at once or one at a time: with Clean on Click a click cleans that piece, and clicks made
//! while one runs wait their turn. Either way what was cleaned leaves the pieces and the rest stay.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use egui::{Color32, Stroke, StrokeKind, TextureOptions, pos2};
use photocraft_ai::catalog;
use photocraft_ai::models::Status;
use photocraft_ai_textmask::regions::MAX_GROW;
use photocraft_ai_textmask::{Detection, ModelState, default_grow};
use photocraft_doc::DocId;
use photocraft_engine::text_mask_cmds::{DETECT, PIECES, STROKE};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::state::Tool;
use crate::theme::Tokens;

/// Longest side of the overlay's texture (a guide, not the selection).
const TEX_MAX: usize = 2048;
/// A stroke no wider than this is a click on a piece (document px, at the least).
const TAP_DOC: f64 = 2.0;
/// How much further the pieces not chosen show: they're the ones to look at.
const OTHERS_STRONGER: f32 = 1.4;
/// A finger lands wider than a pointer (screen px).
const TAP_SCREEN: f32 = 6.0;
const TAP_SCREEN_TOUCH: f32 = 16.0;
const REACH_SCREEN_TOUCH: f32 = 24.0;

/// A document's detection and what its selection makes of it.
struct Pieces {
    detection: Arc<Detection>,
    /// The contour shown, and the one the selection has.
    grow: f32,
    applied: f32,
    /// By piece, `id - 1`.
    chosen: Vec<bool>,
    /// The document revision `chosen` was read at.
    revision: u64,
}

/// What an overlay was made from.
#[derive(Clone, Copy, PartialEq)]
struct Look {
    doc: DocId,
    revision: u64,
    by_suggestion: bool,
    on: Color32,
    off: Color32,
}

/// The tool's state (`PhotocraftApp::text_select`).
#[derive(Default)]
pub(crate) struct State {
    docs: HashMap<DocId, Pieces>,
    /// The document whose detection waits for the model's download.
    waiting: Option<DocId>,
    /// The next detection keeps to the selection.
    in_selection: bool,
    /// A clean under way: the document, its revision then and what it cleans. When the revision
    /// changes that leaves the pieces.
    cleaning: Option<(DocId, u64, photocraft_raster::Surface)>,
    /// The clicks waiting to clean their piece (Clean on Click): document px.
    queue: VecDeque<[f64; 2]>,
    /// What the overlay shows, and its texture.
    overlay: Option<(Look, egui::TextureHandle)>,
    /// On a touch screen, where there's no ⌥: drags take away.
    pub(crate) erase: bool,
}

fn model() -> Option<&'static catalog::ModelInfo> {
    catalog::model(catalog::TEXTMASK_ID)
}

fn say(app: &mut PhotocraftApp, why: String) {
    app.ui.status = why;
    app.ui.status_error = true;
}

fn has_selection(app: &PhotocraftApp) -> bool {
    app.session.active().is_some_and(|st| st.doc.selection.as_ref().is_some_and(|s| !s.content_bounds().is_empty()))
}

/// Looks for the text inside the selection, if there's one, else on the whole page.
fn detect_here(app: &mut PhotocraftApp) {
    app.text_select.in_selection = has_selection(app);
    detect(app);
}

/// Runs the model on the active document, or, the first time, downloads it first.
pub(crate) fn detect(app: &mut PhotocraftApp) {
    let Some(doc) = app.session.active().map(|st| st.doc.id) else { return };
    let Some(state) = app.session.text_masker().map(|m| m.model_state()) else {
        say(app, tl!("Text Selection isn't available in this build").to_string());
        return;
    };
    match state {
        ModelState::Ready => {
            app.ui.status.clear();
            let in_selection = app.text_select.in_selection;
            if let Err(e) = app.run(DETECT, json!({"inSelection": in_selection})) {
                say(app, e);
            }
        }
        ModelState::Unavailable(why) => say(app, why),
        // The browser's cache hasn't answered: asked again in a moment.
        ModelState::Checking => app.text_select.waiting = Some(doc),
        ModelState::NeedsDownload => {
            let started = match (app.session.ai_models(), model()) {
                (Some(store), Some(m)) => store.download(m).map_err(|e| e.to_string()),
                _ => Err(tl!("Text Selection isn't available in this build").to_string()),
            };
            match started {
                Ok(()) => app.text_select.waiting = Some(doc),
                Err(e) => say(app, e),
            }
        }
    }
}

fn detecting(app: &PhotocraftApp, doc: DocId) -> bool {
    app.session.jobs().iter().any(|j| j.command == DETECT && j.document == Some(doc))
}

/// Which pieces the selection holds: more than half of their pixels.
fn chosen_in(d: &Detection, selection: Option<&photocraft_raster::Surface>) -> Vec<bool> {
    let mut hits = vec![0u64; d.regions.len() + 1];
    if let Some(sel) = selection {
        for (i, l) in d.labels.iter().enumerate() {
            if *l != 0 && sel.sample_channel((i % d.width) as i32, (i / d.width) as i32, 0) >= 0.5 {
                hits[*l as usize] += 1;
            }
        }
    }
    d.regions.iter().map(|r| hits[r.id as usize] * 2 > r.area).collect()
}

/// Follows what happened since the last frame: a download or a clean that ended, a new detection
/// (it selects what it suggests), a selection that changed.
fn sync(app: &mut PhotocraftApp) {
    let Some(st) = app.session.active() else { return };
    let (doc, revision) = (st.doc.id, st.revision);
    if app.text_select.waiting == Some(doc) {
        match app.session.text_masker().map(|m| m.model_state()) {
            Some(ModelState::Checking) => {}
            Some(ModelState::NeedsDownload) => {
                if let Some(Status::Failed(e)) = app.session.ai_models().zip(model()).map(|(s, m)| s.status(m)) {
                    app.text_select.waiting = None;
                    say(app, e);
                }
            }
            _ => {
                app.text_select.waiting = None;
                detect(app);
            }
        }
    }
    let idle = !app.session.has_jobs() && app.ui.dialogs.is_empty() && !crate::remove_ai_ui::is_open(app);
    if idle && app.text_select.cleaning.as_ref().is_some_and(|(d, _, _)| *d == doc) {
        if let Some((_, before, zone)) = app.text_select.cleaning.take()
            && revision != before
        {
            finish_clean(app, doc, &zone);
        }
    }
    let Some(detection) = app.session.text_detection() else {
        app.text_select.docs.remove(&doc);
        app.text_select.queue.clear();
        return;
    };
    if !app.text_select.docs.get(&doc).is_some_and(|p| Arc::ptr_eq(&p.detection, &detection)) {
        let grow = (default_grow(detection.width, detection.height) as f32 + app.ui.tool_options.text_select_contour).clamp(0.0, MAX_GROW as f32);
        let chosen = vec![false; detection.regions.len()];
        app.text_select.docs.insert(doc, Pieces { detection, grow, applied: grow, chosen, revision: u64::MAX });
        if !app.ui.tool_options.text_select_clean_click {
            if let Err(e) = app.run(PIECES, json!({"pieces": "suggested", "grow": grow})) {
                say(app, e);
            }
        } else {
            let _ = app.run("select.deselect", json!({}));
        }
    }
    if idle && app.text_select.cleaning.is_none() {
        clean_next(app, doc);
    }
    let Some(st) = app.session.active() else { return };
    let (selection, revision) = (st.doc.selection.as_ref(), st.revision);
    if let Some(p) = app.text_select.docs.get(&doc)
        && p.revision != revision
    {
        let chosen = chosen_in(&p.detection, selection);
        if let Some(p) = app.text_select.docs.get_mut(&doc) {
            p.chosen = chosen;
            p.revision = revision;
        }
    }
}

/// A clean ended: what it cleaned leaves the pieces, and the selection with it.
fn finish_clean(app: &mut PhotocraftApp, doc: DocId, zone: &photocraft_raster::Surface) {
    let _ = app.run("select.deselect", json!({}));
    let Some(d) = app.session.text_detection() else { return };
    let left = Arc::new(d.clipped(|x, y| zone.sample_channel(x as i32, y as i32, 0) < 0.5));
    if left.regions.is_empty() {
        app.session.forget_text_detection();
        return;
    }
    app.session.set_text_detection(left.clone());
    if let Some(p) = app.text_select.docs.get_mut(&doc) {
        p.chosen = vec![false; left.regions.len()];
        p.detection = left;
        p.revision = u64::MAX;
    }
}

/// Cleans what's selected (`edit.removeAi`).
fn clean(app: &mut PhotocraftApp) {
    let Some(st) = app.session.active() else { return };
    let (doc, revision) = (st.doc.id, st.revision);
    let Some(zone) = st.doc.selection.clone() else { return };
    app.text_select.cleaning = Some((doc, revision, zone));
    let all = app.ui.tool_options.sample_all.remove_ai;
    let by_blocks = app.ui.tool_options.text_select_by_blocks;
    crate::remove_ai_ui::request(app, json!({"sampleAllLayers": all, "byBlocks": by_blocks}));
}

/// The next click waiting to clean its piece.
fn clean_next(app: &mut PhotocraftApp, doc: DocId) {
    while let Some([x, y]) = app.text_select.queue.pop_front() {
        let Some(p) = app.text_select.docs.get(&doc) else { return };
        let grow = p.applied;
        // A click on what an earlier one already cleaned finds nothing and is dropped.
        let Some(id) = piece_at(&p.detection, x, y, reach(app, p)) else { continue };
        if let Err(e) = app.run(PIECES, json!({"pieces": [id], "grow": grow})) {
            say(app, e);
            return;
        }
        clean(app);
        return;
    }
}

/// The piece under a pixel, else the nearest within `reach`: its contour counts as the piece.
fn piece_at(d: &Detection, x: f64, y: f64, reach: i64) -> Option<u32> {
    let (x, y) = (x.floor() as i64, y.floor() as i64);
    let (w, h) = (d.width as i64, d.height as i64);
    let mut best: Option<(i64, u32)> = None;
    for yy in (y - reach).max(0)..(y + reach + 1).min(h) {
        for xx in (x - reach).max(0)..(x + reach + 1).min(w) {
            let l = d.labels[(yy * w + xx) as usize];
            let far = (xx - x).pow(2) + (yy - y).pow(2);
            if l != 0 && far <= reach * reach && best.is_none_or(|(b, _)| far < b) {
                best = Some((far, l));
            }
        }
    }
    best.map(|(_, l)| l)
}

/// A finished stroke of the tool (document px): a click takes or adds a piece, a drag paints.
pub fn finish_stroke(app: &mut PhotocraftApp, points: &[[f64; 3]], mods: egui::Modifiers) {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in points {
        (x0, y0, x1, y1) = (x0.min(p[0]), y0.min(p[1]), x1.max(p[0]), y1.max(p[1]));
    }
    let Some(doc) = app.session.active().map(|st| st.doc.id) else { return };
    sync(app);
    let slop = if crate::touch_ui::active(app) { TAP_SCREEN_TOUCH } else { TAP_SCREEN };
    if (x1 - x0).max(y1 - y0) > TAP_DOC.max(screen_px(app) * f64::from(slop)) {
        let xy: Vec<[f64; 2]> = points.iter().map(|q| [q[0], q[1]]).collect();
        let mode = if mods.alt || app.text_select.erase { "subtract" } else { "add" };
        if let Err(e) = app.run(STROKE, json!({"points": xy, "size": app.session.tools.brush.size, "mode": mode})) {
            say(app, e);
        }
        return;
    }
    let Some(p) = app.text_select.docs.get(&doc) else {
        if !detecting(app, doc) && app.text_select.waiting.is_none() {
            detect_here(app);
        }
        return;
    };
    let Some(&[x, y, _]) = points.first() else { return };
    if app.ui.tool_options.text_select_clean_click {
        app.text_select.queue.push_back([x, y]);
        sync(app);
        return;
    }
    let Some(id) = piece_at(&p.detection, x, y, reach(app, p)) else { return };
    let params = toggle_params(p, id);
    if let Err(e) = app.run(PIECES, params) {
        say(app, e);
    }
}

fn toggle_params(p: &Pieces, id: u32) -> Value {
    if p.chosen.get(id as usize - 1).copied().unwrap_or(false) {
        let keep: Vec<u32> = p.detection.regions.iter().filter(|r| r.id != id && p.chosen[r.id as usize - 1]).map(|r| r.id).collect();
        json!({"pieces": [id], "grow": p.applied, "mode": "subtract", "keep": keep})
    } else {
        json!({"pieces": [id], "grow": p.applied, "mode": "add"})
    }
}

/// The overlay: the pieces chosen in a colour, the others in another.
fn overlay_image(p: &Pieces, look: &Look) -> egui::ColorImage {
    let d = &p.detection;
    let step = d.width.max(d.height).div_ceil(TEX_MAX).max(1);
    let (tw, th) = (d.width.div_ceil(step), d.height.div_ceil(step));
    let mut px = Vec::with_capacity(tw * th);
    for y in 0..th {
        for x in 0..tw {
            let l = d.labels[y * step * d.width + x * step];
            px.push(match l {
                0 => Color32::TRANSPARENT,
                l if p.chosen.get(l as usize - 1).copied().unwrap_or(false) => look.on,
                // Nothing is chosen when a click cleans: the model's doubts still show.
                l if look.by_suggestion && d.regions.get(l as usize - 1).is_some_and(|r| r.suggested()) => look.on,
                _ => look.off,
            });
        }
    }
    egui::ColorImage::new([tw, th], px)
}

fn look(app: &PhotocraftApp, doc: DocId, revision: u64) -> Look {
    let o = &app.ui.tool_options;
    let a = (o.text_select_opacity / 100.0).clamp(0.0, 1.0);
    let tint = |c: [u8; 3], a: f32| Color32::from_rgba_unmultiplied(c[0], c[1], c[2], (a.min(1.0) * 255.0) as u8);
    Look { doc, revision, by_suggestion: o.text_select_clean_click, on: tint(o.text_select_on, a), off: tint(o.text_select_off, a * OTHERS_STRONGER) }
}

/// One screen pixel, in document pixels.
fn screen_px(app: &PhotocraftApp) -> f64 {
    let Some(xf) = ViewXform::active(app) else { return 1.0 };
    let (a, b) = (xf.to_doc(pos2(0.0, 0.0)), xf.to_doc(pos2(1.0, 0.0)));
    (b[0] - a[0]).hypot(b[1] - a[1])
}

/// How far from a piece a click still means it: its contour, or a fingertip.
fn reach(app: &PhotocraftApp, p: &Pieces) -> i64 {
    let finger = if crate::touch_ui::active(app) { screen_px(app) * f64::from(REACH_SCREEN_TOUCH) } else { 0.0 };
    (f64::from(p.applied).max(finger) as i64).min(MAX_GROW as i64 * 2)
}

/// Draws the pieces on the active document's canvas, and the one under the pointer.
pub(crate) fn draw(app: &mut PhotocraftApp, painter: &egui::Painter, xf: &ViewXform, idx: usize, hover: Option<egui::Pos2>) {
    if app.ui.tool != Tool::TextSelection || app.session.active_index() != Some(idx) {
        return;
    }
    sync(app);
    let ctx = painter.ctx().clone();
    if app.text_select.waiting.is_some() || !app.text_select.queue.is_empty() || app.text_select.cleaning.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
    if !app.ui.tool_options.text_select_show {
        return;
    }
    let Some(doc) = app.session.active().map(|st| st.doc.id) else { return };
    let Some(revision) = app.text_select.docs.get(&doc).map(|p| p.revision) else { return };
    let wanted = look(app, doc, revision);
    if !app.text_select.overlay.as_ref().is_some_and(|(l, _)| *l == wanted) {
        let Some(p) = app.text_select.docs.get(&doc) else { return };
        let tex = ctx.load_texture("text-select", overlay_image(p, &wanted), TextureOptions::LINEAR);
        app.text_select.overlay = Some((wanted, tex));
    }
    let Some(p) = app.text_select.docs.get(&doc) else { return };
    let Some((_, tex)) = &app.text_select.overlay else { return };
    let (w, h) = (p.detection.width as f32, p.detection.height as f32);
    let mut mesh = egui::Mesh::with_texture(tex.id());
    for (x, y, u, v) in [(0.0, 0.0, 0.0, 0.0), (w, 0.0, 1.0, 0.0), (w, h, 1.0, 1.0), (0.0, h, 0.0, 1.0)] {
        mesh.vertices.push(egui::epaint::Vertex { pos: xf.to_screen(x, y), uv: pos2(u, v), color: Color32::WHITE });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
    let under = hover.map(|h| xf.to_doc(h)).and_then(|at| piece_at(&p.detection, at[0], at[1], p.applied as i64));
    if let Some(r) = under.and_then(|id| p.detection.regions.get(id as usize - 1)) {
        let [x0, y0, x1, y1] = r.bbox.map(|v| v as i32);
        let edge = Tokens::get(&ctx).accent;
        painter.rect_stroke(xf.doc_rect(Rect::new(x0, y0, x1, y1)), 2.0, Stroke::new(1.5, edge), StrokeKind::Outside);
    }
}

/// What a button of the tool's bars does. The bars of a pointer and of a finger share these.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Act {
    Find,
    FindAgain,
    All,
    Suggested,
    Invert,
    None,
    Grow(f32),
    ToggleClick,
    ToggleShow,
    ToggleErase,
    ToggleBlocks,
    Brush(f32),
    Clean,
    Fill,
    Keep,
    Done,
    NextOutput,
}

fn act(app: &mut PhotocraftApp, ctx: &egui::Context, what: Act) {
    let Some(st) = app.session.active() else { return };
    let (doc, revision) = (st.doc.id, st.revision);
    let piece_ids = |app: &PhotocraftApp, on: bool| -> Vec<u32> {
        app.text_select.docs.get(&doc).map_or_else(Vec::new, |p| p.detection.regions.iter().filter(|r| p.chosen[r.id as usize - 1] == on).map(|r| r.id).collect())
    };
    let applied = app.text_select.docs.get(&doc).map_or(0.0, |p| p.applied);
    let run = match what {
        Act::Find => {
            detect_here(app);
            return;
        }
        Act::FindAgain => {
            // The selection here is the pieces themselves: again means the whole page.
            app.text_select.in_selection = false;
            detect(app);
            return;
        }
        Act::All => Some((PIECES, json!({"pieces": "all", "grow": applied}))),
        Act::Suggested => Some((PIECES, json!({"pieces": "suggested", "grow": applied}))),
        Act::Invert => Some((PIECES, json!({"pieces": piece_ids(app, false), "grow": applied}))),
        Act::None => Some(("select.deselect", json!({}))),
        Act::Grow(by) => {
            set_grow(app, doc, applied + by, None);
            return;
        }
        Act::ToggleClick => {
            let on = !app.ui.tool_options.text_select_clean_click;
            app.ui.tool_options.text_select_clean_click = on;
            app.text_select.queue.clear();
            Some(if on { ("select.deselect", json!({})) } else { (PIECES, json!({"pieces": "suggested", "grow": applied})) })
        }
        Act::ToggleShow => {
            app.ui.tool_options.text_select_show = !app.ui.tool_options.text_select_show;
            return;
        }
        Act::ToggleErase => {
            app.text_select.erase = !app.text_select.erase;
            return;
        }
        Act::ToggleBlocks => {
            app.ui.tool_options.text_select_by_blocks = !app.ui.tool_options.text_select_by_blocks;
            return;
        }
        Act::Brush(by) => {
            let size = (app.session.tools.brush.size * by).clamp(4.0, 500.0);
            app.session.tools.brush.size = size;
            return;
        }
        Act::Clean => {
            clean(app);
            return;
        }
        Act::Fill => {
            if let Some(zone) = app.session.active().and_then(|st| st.doc.selection.clone()) {
                app.text_select.cleaning = Some((doc, revision, zone));
            }
            if let Err(e) = crate::menus::invoke(app, ctx, "edit.contentAwareFill", json!({}))
                && e != crate::file_dialog::CANCELLED
            {
                say(app, e);
            }
            return;
        }
        Act::Keep => {
            app.ui.tool = Tool::RectMarquee;
            return;
        }
        Act::NextOutput => {
            let options = crate::remove_ai_ui::output_options();
            let at = options.iter().position(|(k, _)| *k == app.ui.tool_options.clean_output).unwrap_or(0);
            app.ui.tool_options.clean_output = options[(at + 1) % options.len()].0.clone();
            return;
        }
        Act::Done => {
            app.session.forget_text_detection();
            app.text_select.queue.clear();
            return;
        }
    };
    if let Some((id, params)) = run
        && let Err(e) = app.run(id, params)
    {
        say(app, e);
    }
}

/// A new contour: for the pieces chosen, and remembered for the next page. `step` joins a drag's
/// changes in one history step.
fn set_grow(app: &mut PhotocraftApp, doc: DocId, grow: f32, step: Option<String>) {
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    let grow = grow.clamp(0.0, MAX_GROW as f32).round();
    let applied = p.applied;
    p.grow = grow;
    p.applied = grow;
    if grow == applied {
        return;
    }
    let base = default_grow(p.detection.width, p.detection.height) as f32;
    let ids: Vec<u32> = p.detection.regions.iter().filter(|r| p.chosen[r.id as usize - 1]).map(|r| r.id).collect();
    app.ui.tool_options.text_select_contour = grow - base;
    if ids.is_empty() {
        return;
    }
    let mut params = json!({"pieces": ids, "grow": grow, "from": applied});
    if let Some(step) = step {
        params["coalesce"] = json!(step);
    }
    if let Err(e) = app.run(PIECES, params) {
        say(app, e);
    }
}

/// The hint on the page, and how many pieces there are.
fn status_line(app: &PhotocraftApp, p: &Pieces) -> String {
    let total = p.detection.regions.len();
    let touch = crate::touch_ui::active(app);
    if app.ui.tool_options.text_select_clean_click {
        let hint = if touch { tl!("Tap a piece to clean it") } else { tl!("Click a piece to clean it") };
        let pieces = crate::i18n::fmt(tl!("{total} pieces"), &[("total", &total.to_string())]);
        let waiting = app.text_select.queue.len();
        return if waiting > 0 { format!("{pieces}  ·  {}  ·  {hint}", crate::i18n::fmt(tl!("{n} waiting"), &[("n", &waiting.to_string())])) } else { format!("{pieces}  ·  {hint}") };
    }
    let on = p.chosen.iter().filter(|c| **c).count();
    let hint = if touch { tl!("Tap a piece to add or take it; paint to add") } else { tl!("Click a piece to add or take it; paint to add, Alt to take away") };
    format!("{}  ·  {hint}", crate::i18n::fmt(tl!("{on} of {total} pieces"), &[("on", &on.to_string()), ("total", &total.to_string())]))
}

/// Whether a detection can't start now, and what to say instead.
fn working_label(app: &PhotocraftApp, doc: DocId) -> Option<String> {
    if let Some(Status::Downloading { done, total, .. }) = app.text_select.waiting.and(app.session.ai_models().zip(model()).map(|(s, m)| s.status(m))) {
        let share = done as f32 / total.max(1) as f32 * 100.0;
        return Some(crate::i18n::fmt(tl!("Downloading the text model… {percent} %"), &[("percent", &format!("{share:.0}"))]));
    }
    (app.text_select.waiting.is_some() || detecting(app, doc)).then(|| tl!("Finding the text…").to_string())
}

fn find_label(app: &PhotocraftApp) -> &'static str {
    if has_selection(app) { tl!("Find Text in Selection") } else { tl!("Find Text") }
}

/// The tool's options: detect, then which pieces and how far past them, then how they show.
pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    sync(app);
    let ctx = ui.ctx().clone();
    let Some(doc) = app.session.active().map(|st| st.doc.id) else {
        ui.label(tl!("Open a page to find its text"));
        return;
    };
    if let Some(text) = working_label(app, doc) {
        ui.label(text);
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
        return;
    }
    let Some((shown, applied)) = app.text_select.docs.get(&doc).map(|p| (p.grow, p.applied)) else {
        if crate::widgets::primary_button(ui, find_label(app), 0.0).clicked() {
            act(app, &ctx, Act::Find);
        }
        ui.label(tl!("Finds the text and sound effects on the page; you choose what to select"));
        return;
    };
    let mut todo = None;
    let by_click = app.ui.tool_options.text_select_clean_click;
    if crate::widgets::secondary_button(ui, tl!("Find Again"), 0.0).on_hover_text(tl!("Looks on the whole page again")).clicked() {
        todo = Some(Act::FindAgain);
    }
    crate::widgets::vline(ui, 22.0);
    if !by_click {
        for (label, what) in [(tl!("All"), Act::All), (tl!("Suggested"), Act::Suggested), (tl!("Invert"), Act::Invert), (tl!("None"), Act::None)] {
            if crate::widgets::secondary_button(ui, label, 0.0).clicked() {
                todo = Some(what);
            }
        }
        crate::widgets::vline(ui, 22.0);
    }
    ui.label(tl!("Contour"));
    let mut grow = shown;
    let r = crate::widgets::value_field_named(ui, tl!("Contour"), &mut grow, 0.0..=MAX_GROW as f32, " px", 54.0);
    grow = grow.round();
    if let Some(p) = app.text_select.docs.get_mut(&doc) {
        p.grow = grow;
    }
    // Live while dragging, as one history step; a typed number when it's done.
    if grow != applied && !r.has_focus() {
        let step = r.dragged().then(|| crate::panels::layer_props_ui::drag_step(&ctx)).flatten();
        set_grow(app, doc, grow, step);
    }
    r.on_hover_text(tl!("How far the selection reaches past the letters. Too little leaves smudges when cleaning; too much takes the lines around them"));
    crate::widgets::vline(ui, 22.0);
    let mut by_click_box = by_click;
    if crate::widgets::checkbox(ui, &mut by_click_box, tl!("Clean on Click")).on_hover_text(tl!("Each click cleans that piece right away, one at a time")).clicked() {
        todo = Some(Act::ToggleClick);
    }
    crate::widgets::checkbox(ui, &mut app.ui.tool_options.text_select_by_blocks, tl!("Block by Block")).on_hover_text(tl!("Cleans each block of text on its own, one after another, not those close together at once"));
    crate::widgets::vline(ui, 22.0);
    crate::remove_ai_ui::output_dropdown(app, ui);
    crate::widgets::vline(ui, 22.0);
    let o = &mut app.ui.tool_options;
    crate::widgets::checkbox(ui, &mut o.text_select_show, tl!("Show Pieces"));
    crate::widgets::color_edit_button_srgb(ui, &mut o.text_select_on).on_hover_text(tl!("Color of the chosen pieces"));
    crate::widgets::color_edit_button_srgb(ui, &mut o.text_select_off).on_hover_text(tl!("Color of the other pieces"));
    crate::widgets::value_field_named(ui, tl!("Opacity"), &mut o.text_select_opacity, 10.0..=90.0, " %", 54.0);
    if let Some(what) = todo {
        act(app, &ctx, what);
    }
}

/// A button of the bar on the canvas: with its icon, quiet unless it's the main one.
fn bar_button(ui: &mut egui::Ui, icon: &str, label: &str, main: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let ink = if main { t.primary_text } else { t.text };
    ui.add(
        egui::Button::image_and_text(crate::icons::image(icon, 15.0, ink), egui::RichText::new(label).color(ink))
            .fill(if main { t.primary_bg } else { Color32::TRANSPARENT })
            .stroke(Stroke::NONE)
            .corner_radius(8)
            .min_size(egui::vec2(0.0, 30.0)),
    )
}

/// What to do with the selection, on the canvas: how many pieces, then what next.
pub fn bar(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.ui.tool != Tool::TextSelection || !app.ui.dialogs.is_empty() || crate::touch_ui::active(app) {
        return;
    }
    let Some(st) = app.session.active() else { return };
    let doc = st.doc.id;
    let Some(p) = app.text_select.docs.get(&doc) else { return };
    let line = status_line(app, p);
    let busy = app.session.has_jobs();
    let actions = has_selection(app) && !app.ui.tool_options.text_select_clean_click && !busy;
    let t = Tokens::get(ctx);
    let screen = app.last_canvas_rect;
    let id = egui::Id::unique("text-select-bar");
    let size = ctx.memory(|m| m.area_rect(id)).map_or(egui::vec2(520.0, 70.0), |a| a.size());
    let pos = pos2((screen.center().x - size.x / 2.0).max(screen.left() + 8.0), screen.bottom() - size.y - 14.0);
    let mut todo = None;
    crate::overlay_guard::non_modal(ctx, id);
    egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::NONE
            .fill(t.card)
            .stroke(Stroke::new(1.0, t.card_border))
            .corner_radius(12)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .shadow(ui.style().visuals.popup_shadow)
            .show(ui, |ui| {
                ui.set_max_width((screen.width() - 40.0).max(240.0));
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&line).size(12.0).color(t.text_dim)).wrap());
                    if bar_button(ui, "x", tl!("Done"), false).on_hover_text("Esc").clicked() {
                        todo = Some(Act::Done);
                    }
                });
                if actions {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if bar_button(ui, "sparkles", tl!("Remove with AI"), true).on_hover_text("↩").clicked() {
                            todo = Some(Act::Clean);
                        }
                        if bar_button(ui, "paint-bucket", tl!("Content-Aware Fill…"), false).clicked() {
                            todo = Some(Act::Fill);
                        }
                        if bar_button(ui, "square-dashed", tl!("Leave as Selection"), false).clicked() {
                            todo = Some(Act::Keep);
                        }
                    });
                }
            });
    });
    // ↩ and Esc, unless a field is taking the keys.
    if !ctx.egui_wants_keyboard_input() {
        if actions && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            todo = Some(Act::Clean);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            todo = Some(Act::Done);
        }
    }
    if let Some(what) = todo {
        act(app, ctx, what);
    }
}

/// The tool on a touch screen: what to do along the top, big buttons at the bottom, and no keys.
pub fn touch_bar(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(doc) = app.session.active().map(|st| st.doc.id) else { return };
    let area = crate::touch_ui::canvas_area(app, ctx);
    let (h, gap) = (48.0, 8.0);
    let top = area.bottom() - 8.0 - h;
    let mut todo = None;
    let id = egui::Id::unique("text-select-touch");
    crate::overlay_guard::non_modal(ctx, id);
    let working = working_label(app, doc);
    let Some(p) = app.text_select.docs.get(&doc).filter(|_| working.is_none()) else {
        // Nothing found yet: one big button.
        let w = (area.width() - 32.0).min(280.0);
        let r = egui::Rect::from_center_size(pos2(area.center().x, top + h / 2.0), egui::vec2(w, h));
        let label = working.unwrap_or_else(|| find_label(app).to_string());
        let idle = !app.session.has_jobs() && app.text_select.waiting.is_none();
        egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(r.min).show(ctx, |ui| {
            ui.set_min_size(egui::Vec2::ZERO);
            if crate::touch_ui::round_button(ui, r, None, &label, idle).clicked() && idle {
                todo = Some(Act::Find);
            }
        });
        crate::touch_ui::note_rect(ctx, "textSelectFind", r);
        if let Some(what) = todo {
            act(app, ctx, what);
        }
        return;
    };
    let o = &app.ui.tool_options;
    let by_click = o.text_select_clean_click;
    let contour = crate::i18n::fmt(tl!("Contour {px}"), &[("px", &format!("{:.0}", p.applied))]);
    let brush = crate::i18n::fmt(tl!("Brush {px}"), &[("px", &format!("{:.0}", app.session.tools.brush.size))]);
    let line = status_line(app, p);
    let output = crate::remove_ai_ui::output_options().into_iter().find(|(k, _)| *k == o.clean_output).map_or(tl!("Current Layer"), |(_, l)| l);
    let mut items: Vec<(&str, &str, &str)> = vec![("click", if by_click { tl!("Clean on Tap: On") } else { tl!("Clean on Tap: Off") }, "sparkles")];
    if !by_click {
        items.extend([
            ("all", tl!("All"), "check"),
            ("suggested", tl!("Suggested"), "text-select"),
            ("invert", tl!("Invert"), "contrast"),
            ("none", tl!("None"), "x"),
        ]);
    }
    items.extend([
        ("less", tl!("Less Contour"), "minus"),
        ("contour", contour.as_str(), "circle-dashed"),
        ("more", tl!("More Contour"), "plus"),
    ]);
    if !by_click {
        items.extend([
            ("smaller", tl!("Smaller Brush"), "minus"),
            ("brush", brush.as_str(), "brush"),
            ("bigger", tl!("Bigger Brush"), "plus"),
        ]);
    }
    items.extend([
        ("blocks", if o.text_select_by_blocks { tl!("Block by Block: On") } else { tl!("Block by Block: Off") }, "layers"),
        ("output", output, "layers"),
        ("show", if o.text_select_show { tl!("Hide Pieces") } else { tl!("Show Pieces") }, if o.text_select_show { "eye-off" } else { "eye" }),
        ("again", tl!("Find Again"), "search"),
    ]);
    if !by_click {
        items.extend([("fill", tl!("Content-Aware Fill"), "paint-bucket"), ("keep", tl!("Leave as Selection"), "square-dashed")]);
    }
    let picked = crate::touch_ui::action_bar(ctx, area, "text-select-actions", "textSelect", &items);
    todo = picked.and_then(|cmd| {
        Some(match cmd {
            "all" => Act::All,
            "suggested" => Act::Suggested,
            "invert" => Act::Invert,
            "none" => Act::None,
            "less" => Act::Grow(-2.0),
            "more" => Act::Grow(2.0),
            "smaller" => Act::Brush(0.75),
            "bigger" => Act::Brush(1.34),
            "click" => Act::ToggleClick,
            "blocks" => Act::ToggleBlocks,
            "show" => Act::ToggleShow,
            "again" => Act::FindAgain,
            "fill" => Act::Fill,
            "keep" => Act::Keep,
            "output" => Act::NextOutput,
            _ => return None,
        })
    });
    let can_clean = has_selection(app) && !by_click && !app.session.has_jobs();
    let (done_w, paint_w) = (84.0, 96.0);
    let main_w = (area.width() - 16.0 - done_w - paint_w - 2.0 * gap).min(220.0);
    let row_w = done_w + gap + main_w + if by_click { 0.0 } else { paint_w + gap };
    let x0 = area.center().x - row_w / 2.0;
    let done = egui::Rect::from_min_size(pos2(x0, top), egui::vec2(done_w, h));
    let paint = egui::Rect::from_min_size(pos2(done.right() + gap, top), egui::vec2(paint_w, h));
    let main = egui::Rect::from_min_size(pos2(if by_click { done.right() + gap } else { paint.right() + gap }, top), egui::vec2(main_w, h));
    let erasing = app.text_select.erase;
    let t = Tokens::get(ctx);
    let g = ctx.fonts_mut(|f| f.layout(line.clone(), crate::theme::medium(11.0), t.text, area.width() - 32.0));
    let back = egui::Rect::from_center_size(pos2(area.center().x, top - 8.0 - (g.size().y + 8.0) / 2.0), g.size() + egui::vec2(16.0, 8.0));
    // The area starts here, not at the canvas's top: it would lie over the bar up there and take
    // its touches.
    egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(pos2(area.left(), back.top())).show(ctx, |ui| {
        ui.set_min_size(egui::Vec2::ZERO);
        ui.painter().rect_filled(back, 8.0, t.card);
        ui.painter().galley(back.min + egui::vec2(8.0, 4.0), g, t.text);
        if crate::touch_ui::round_button(ui, done, None, tl!("Done"), false).clicked() {
            todo = Some(Act::Done);
        }
        // What a drag does: the big switch to take zones away, as ⌥ does with a keyboard.
        if !by_click && crate::touch_ui::round_button(ui, paint, None, if erasing { tl!("Painting Removes") } else { tl!("Painting Adds") }, erasing).clicked() {
            todo = Some(Act::ToggleErase);
        }
        if can_clean && crate::touch_ui::round_button(ui, main, None, tl!("Remove with AI"), true).clicked() {
            todo = Some(Act::Clean);
        }
    });
    crate::touch_ui::note_rect(ctx, "textSelectDone", done);
    crate::touch_ui::note_rect(ctx, "textSelectPaint", paint);
    crate::touch_ui::note_rect(ctx, "textSelectClean", main);
    if let Some(what) = todo {
        act(app, ctx, what);
    }
}

#[cfg(test)]
mod tests {
    use photocraft_ai_textmask::regions::find_regions;
    use photocraft_ai_textmask::{Page, TextMasker};

    use super::*;
    use crate::canvas::{ToolEvent, tool_event};

    /// Finds whatever is dark.
    struct Dark;
    impl TextMasker for Dark {
        fn model_state(&self) -> ModelState {
            ModelState::Ready
        }
        fn device(&self) -> String {
            "test".into()
        }
        fn detect(&self, page: Page<'_>, _: &dyn Fn() -> bool) -> Result<Detection, photocraft_ai_textmask::Error> {
            let prob: Vec<f32> = page.rgb.chunks_exact(3).map(|c| if c[0] < 128 { 0.95 } else { 0.0 }).collect();
            Ok(find_regions(&prob, page.width, page.height))
        }
    }

    /// A page with two dark blocks and the tool on.
    fn page() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.session.execute("file.new", json!({"width": 300, "height": 200, "background": "white"})).unwrap();
        app.session.set_text_masker(Some(Arc::new(Dark)));
        for (x, y) in [(20, 20), (200, 120)] {
            app.session.execute("select.rect", json!({"x": x, "y": y, "width": 40, "height": 30})).unwrap();
            app.session.execute("edit.fill", json!({"color": "#000000"})).unwrap();
        }
        app.session.execute("select.deselect", json!({})).unwrap();
        app.ui.tool = Tool::TextSelection;
        app
    }

    fn test_look(doc: DocId, by_suggestion: bool) -> Look {
        Look { doc, revision: 0, by_suggestion, on: Color32::BLUE, off: Color32::RED }
    }

    fn covered(app: &PhotocraftApp, x: i32, y: i32) -> bool {
        app.session.active().unwrap().doc.selection.as_ref().is_some_and(|s| s.sample_channel(x, y, 0) >= 0.5)
    }

    fn click(app: &mut PhotocraftApp, x: f64, y: f64, m: egui::Modifiers) {
        tool_event(app, ToolEvent::Down { x, y, pressure: 1.0 }, m);
        tool_event(app, ToolEvent::Up { x, y }, m);
    }

    #[test]
    fn a_detection_selects_what_it_suggests_and_clicks_take_and_add_pieces() {
        let mut app = page();
        assert!(!covered(&app, 30, 30));
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        assert!(covered(&app, 30, 30) && covered(&app, 210, 130), "both pieces");
        let doc = app.session.active().unwrap().doc.id;
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].chosen, vec![true, true]);
        click(&mut app, 30.0, 30.0, egui::Modifiers::NONE);
        assert!(!covered(&app, 30, 30) && covered(&app, 210, 130), "one out");
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].chosen, vec![false, true]);
        click(&mut app, 30.0, 30.0, egui::Modifiers::NONE);
        assert!(covered(&app, 30, 30), "and back");
    }

    #[test]
    fn with_a_selection_it_looks_only_there() {
        let mut app = page();
        app.session.execute("select.rect", json!({"x": 0, "y": 0, "width": 100, "height": 100})).unwrap();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        assert_eq!(app.text_select.docs[&doc].chosen.len(), 1, "only the piece in the zone");
        assert!(covered(&app, 30, 30) && !covered(&app, 210, 130));
    }

    #[test]
    fn what_a_clean_took_leaves_the_pieces_and_the_rest_stay() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        click(&mut app, 210.0, 130.0, egui::Modifiers::NONE);
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].chosen, vec![true, false]);
        // A clean that changed the page, as `edit.removeAi` does.
        let zone = app.session.active().unwrap().doc.selection.clone().unwrap();
        let before = app.session.active().unwrap().revision;
        app.text_select.cleaning = Some((doc, before, zone));
        app.session.execute("edit.fill", json!({"color": "#ffffff"})).unwrap();
        sync(&mut app);
        assert!(app.text_select.cleaning.is_none());
        let p = &app.text_select.docs[&doc];
        assert_eq!(p.detection.regions.len(), 1, "the other one stays");
        assert_eq!(p.detection.regions[0].bbox[0], 200);
        assert!(app.session.active().unwrap().doc.selection.is_none());
    }

    #[test]
    fn a_clean_that_changed_nothing_keeps_the_pieces() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        let zone = app.session.active().unwrap().doc.selection.clone().unwrap();
        let before = app.session.active().unwrap().revision;
        app.text_select.cleaning = Some((doc, before, zone));
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].detection.regions.len(), 2);
        assert!(covered(&app, 30, 30), "the selection stays too");
    }

    #[test]
    fn with_clean_on_click_a_detection_selects_nothing_and_a_click_goes_for_its_piece() {
        let mut app = page();
        app.ui.tool_options.text_select_clean_click = true;
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        assert!(app.session.active().unwrap().doc.selection.is_none(), "nothing runs by itself");
        let doc = app.session.active().unwrap().doc.id;
        let img = overlay_image(&app.text_select.docs[&doc], &test_look(doc, true));
        assert_eq!(img.pixels[30 * img.size[0] + 30], img.pixels[130 * img.size[0] + 210], "both read as suggested");
        // No Manga LaMa here: the click still selects its piece and starts on it.
        click(&mut app, 30.0, 30.0, egui::Modifiers::NONE);
        assert!(covered(&app, 30, 30) && !covered(&app, 210, 130), "only that piece");
        assert!(app.text_select.queue.is_empty());
    }

    #[test]
    fn invert_all_and_none_change_the_pieces_at_once() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        let ctx = egui::Context::default();
        click(&mut app, 30.0, 30.0, egui::Modifiers::NONE);
        sync(&mut app);
        act(&mut app, &ctx, Act::Invert);
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].chosen, vec![true, false]);
        act(&mut app, &ctx, Act::All);
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].chosen, vec![true, true]);
        act(&mut app, &ctx, Act::None);
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].chosen, vec![false, false]);
        act(&mut app, &ctx, Act::Done);
        sync(&mut app);
        assert!(!app.text_select.docs.contains_key(&doc), "done with the pieces");
    }

    #[test]
    fn the_contour_changes_what_is_chosen_and_stays_for_the_next_page() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        let base = app.text_select.docs[&doc].applied;
        assert!(!covered(&app, 20 - base as i32 - 6, 30));
        set_grow(&mut app, doc, base + 6.0, None);
        assert!(covered(&app, 20 - base as i32 - 5, 30), "the selection reached further");
        assert_eq!(app.ui.tool_options.text_select_contour, 6.0);
        // The next page starts with that extra.
        let ctx = egui::Context::default();
        act(&mut app, &ctx, Act::Done);
        sync(&mut app);
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].applied, base + 6.0);
    }

    #[test]
    fn on_a_touch_screen_the_erase_toggle_stands_for_alt() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        app.session.tools.brush.size = 10.0;
        finish_stroke(&mut app, &[[100.0, 20.0, 1.0], [150.0, 20.0, 1.0]], egui::Modifiers::NONE);
        assert!(covered(&app, 125, 20));
        app.text_select.erase = true;
        finish_stroke(&mut app, &[[100.0, 20.0, 1.0], [150.0, 20.0, 1.0]], egui::Modifiers::NONE);
        assert!(!covered(&app, 125, 20));
    }

    #[test]
    fn undo_is_read_back_from_the_selection() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        click(&mut app, 30.0, 30.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        assert_eq!(app.text_select.docs[&doc].chosen, vec![false, true]);
        app.session.execute("edit.undo", json!({})).unwrap();
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].chosen, vec![true, true]);
    }

    #[test]
    fn a_drag_paints_the_selection_and_alt_takes_away() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        app.session.tools.brush.size = 10.0;
        let stroke = |app: &mut PhotocraftApp, m| finish_stroke(app, &[[100.0, 20.0, 1.0], [150.0, 20.0, 1.0]], m);
        stroke(&mut app, egui::Modifiers::NONE);
        assert!(covered(&app, 125, 20), "added");
        stroke(&mut app, egui::Modifiers::ALT);
        assert!(!covered(&app, 125, 20), "taken");
        assert!(covered(&app, 30, 30), "the rest stays");
    }

    #[test]
    fn a_click_on_the_contour_counts_as_the_piece_and_on_nothing_does_nothing() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        let d = app.text_select.docs[&doc].detection.clone();
        assert_eq!(piece_at(&d, 30.0, 30.0, 4), Some(1));
        assert_eq!(piece_at(&d, 17.0, 30.0, 4), Some(1), "in the contour");
        assert_eq!(piece_at(&d, 10.0, 30.0, 4), None);
        assert_eq!(piece_at(&d, -50.0, 900.0, 4), None, "off the page");
    }

    #[test]
    fn the_overlay_tells_chosen_from_not() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        click(&mut app, 30.0, 30.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        let img = overlay_image(&app.text_select.docs[&doc], &test_look(doc, false));
        let at = |x: usize, y: usize| img.pixels[y * img.size[0] + x];
        assert_ne!(at(30, 30), at(210, 130));
        assert_eq!(at(150, 90), Color32::TRANSPARENT);
    }
}
