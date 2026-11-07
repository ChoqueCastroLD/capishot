//! The Text Selection tool (W group): the mask model finds the page's text and sound effects as
//! pieces, on the whole page or inside the selection, and a person chooses. The model also marks
//! art, so its doubtful pieces start unchosen and nothing runs by itself.
//!
//! The selection is a normal one, and what's chosen is read from it: Undo, the Lasso and
//! Select › Modify all count. Each piece chosen also has how it will be cleaned: Manga LaMa at its
//! fixed best (`edit.removeAi`, as Remove with AI), filled by content or painted flat
//! (`edit.cleanQuick`). A click marks a block with the method in use, or takes it off; a box around
//! pieces marks them, and on nothing, or with the Rectangle, the Lasso or the Brush, a drag draws a
//! zone of its own, a piece like the others. ⌥ takes away. Clean runs each method on its pieces,
//! the quick ones first. The contour is how far a selection reaches past the letters: too close
//! and cleaning copies their dark edge back as a smudge.
//!
//! All at once or one at a time: with Clean on Click a click cleans that block, and clicks made
//! while one runs wait their turn. Either way what was cleaned leaves the pieces and the rest stay.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use egui::{Color32, Stroke, StrokeKind, TextureOptions, pos2};
use photocraft_ai::catalog;
use photocraft_ai::models::Status;
use photocraft_ai_textmask::regions::MAX_GROW;
use photocraft_ai_textmask::{Detection, ModelState, default_grow};
use photocraft_doc::DocId;
use photocraft_engine::ai_inpaint_cmds::quick;
use photocraft_engine::text_mask_cmds::{DETECT, PIECES, ZONE};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::state::{CleanMethod, Tool, ZoneShape};
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
    /// How it's cleaned if it's chosen.
    method: Vec<CleanMethod>,
    /// Still to clean in the run under way.
    todo: Vec<bool>,
    /// Changes when `method` does.
    marks: u64,
    /// The document revision `chosen` was read at.
    revision: u64,
}

/// What an overlay was made from.
#[derive(Clone, Copy, PartialEq)]
struct Look {
    doc: DocId,
    revision: u64,
    marks: u64,
    by_suggestion: bool,
    active: CleanMethod,
    on: [Color32; 3],
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
    /// The methods left in the run under way, the next one last.
    plan: Vec<CleanMethod>,
    /// The clicks waiting to clean their piece (Clean on Click): document px.
    queue: VecDeque<[f64; 2]>,
    /// What the overlay shows, and its texture.
    overlay: Option<(Look, egui::TextureHandle)>,
    /// On a touch screen, where there's no ⌥: drags take away.
    pub(crate) erase: bool,
}

const METHODS: [CleanMethod; 3] = [CleanMethod::Ai, CleanMethod::Fill, CleanMethod::Color];
const SHAPES: [ZoneShape; 4] = [ZoneShape::Pick, ZoneShape::Rect, ZoneShape::Lasso, ZoneShape::Brush];

fn method_label(m: CleanMethod) -> &'static str {
    match m {
        CleanMethod::Ai => tl!("AI"),
        CleanMethod::Fill => tl!("Content Fill"),
        CleanMethod::Color => tl!("Flat Color"),
    }
}

fn method_tip(m: CleanMethod) -> &'static str {
    match m {
        CleanMethod::Ai => tl!("Manga LaMa draws what was behind the letters. The best over drawings, and it takes a while"),
        CleanMethod::Fill => tl!("Copies from around each block. Quick, for tones and simple backgrounds"),
        CleanMethod::Color => tl!("Paints each block in the color around it. Instant, for letters on an even balloon"),
    }
}

fn method_tint(o: &crate::state::ToolOptions, m: CleanMethod) -> [u8; 3] {
    match m {
        CleanMethod::Ai => o.text_select_on,
        CleanMethod::Fill => o.text_select_fill_tint,
        CleanMethod::Color => o.text_select_flat_tint,
    }
}

fn shape_label(s: ZoneShape) -> &'static str {
    match s {
        ZoneShape::Pick => tl!("Pick"),
        ZoneShape::Rect => tl!("Rectangle"),
        ZoneShape::Lasso => tl!("Lasso"),
        ZoneShape::Brush => tl!("Brush"),
    }
}

fn shape_tip(s: ZoneShape) -> &'static str {
    match s {
        ZoneShape::Pick => tl!("Click a block, or drag a box around several"),
        ZoneShape::Rect => tl!("Drag a rectangle: a zone of its own"),
        ZoneShape::Lasso => tl!("Draw around a zone of its own"),
        ZoneShape::Brush => tl!("Paint a zone of its own"),
    }
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
        if let Some((_, before, zone)) = app.text_select.cleaning.take() {
            if revision != before {
                finish_clean(app, doc, &zone);
            } else {
                abort_plan(app, doc);
            }
        }
    }
    let Some(detection) = app.session.text_detection() else {
        app.text_select.docs.remove(&doc);
        app.text_select.queue.clear();
        return;
    };
    if !app.text_select.docs.get(&doc).is_some_and(|p| Arc::ptr_eq(&p.detection, &detection)) {
        let grow = (default_grow(detection.width, detection.height) as f32 + app.ui.tool_options.text_select_contour).clamp(0.0, MAX_GROW as f32);
        let n = detection.regions.len();
        let method = vec![app.ui.tool_options.text_select_method; n];
        app.text_select.docs.insert(doc, Pieces { detection, grow, applied: grow, chosen: vec![false; n], method, todo: vec![false; n], marks: 0, revision: u64::MAX });
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

/// `new` takes the place of the document's pieces: what's left of each keeps its marks, and what's
/// new has the method in use.
fn adopt(app: &mut PhotocraftApp, doc: DocId, new: Arc<Detection>) {
    let active = app.ui.tool_options.text_select_method;
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    let mut from = vec![0u32; new.regions.len() + 1];
    for (n, o) in new.labels.iter().zip(&p.detection.labels) {
        if *n != 0 && *o != 0 {
            from[*n as usize] = *o;
        }
    }
    let old = |id: u32| from[id as usize].checked_sub(1).map(|o| o as usize);
    p.method = new.regions.iter().map(|r| old(r.id).map_or(active, |o| p.method[o])).collect();
    p.todo = new.regions.iter().map(|r| old(r.id).is_some_and(|o| p.todo[o])).collect();
    p.chosen = vec![false; new.regions.len()];
    p.detection = new;
    p.revision = u64::MAX;
    p.marks += 1;
}

/// A clean ended: what it cleaned leaves the pieces, and the selection with it. The run goes on.
fn finish_clean(app: &mut PhotocraftApp, doc: DocId, zone: &photocraft_raster::Surface) {
    let _ = app.run("select.deselect", json!({}));
    let Some(d) = app.session.text_detection() else { return };
    let left = Arc::new(d.clipped(|x, y| zone.sample_channel(x as i32, y as i32, 0) < 0.5));
    if left.regions.is_empty() {
        app.session.forget_text_detection();
        app.text_select.plan.clear();
        return;
    }
    app.session.set_text_detection(left.clone());
    adopt(app, doc, left);
    plan_next(app, doc);
}

/// A clean changed nothing, or was cancelled: what was left of the run is chosen again.
fn abort_plan(app: &mut PhotocraftApp, doc: DocId) {
    app.text_select.plan.clear();
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    let left: Vec<u32> = p.detection.regions.iter().filter(|r| p.todo[r.id as usize - 1]).map(|r| r.id).collect();
    p.todo.fill(false);
    let grow = p.applied;
    if !left.is_empty() && let Err(e) = app.run(PIECES, json!({"pieces": left, "grow": grow, "mode": "add"})) {
        say(app, e);
    }
}

/// Cleans what's selected with `m`.
fn run_method(app: &mut PhotocraftApp, m: CleanMethod) {
    let Some(st) = app.session.active() else { return };
    let (doc, revision) = (st.doc.id, st.revision);
    let Some(zone) = st.doc.selection.clone() else { return };
    app.text_select.cleaning = Some((doc, revision, zone));
    let o = &app.ui.tool_options;
    if m == CleanMethod::Ai {
        let all = o.sample_all.remove_ai;
        let by_blocks = o.text_select_by_blocks;
        crate::remove_ai_ui::request(app, json!({"sampleAllLayers": all, "byBlocks": by_blocks}));
        return;
    }
    let mut params = json!({"method": if m == CleanMethod::Fill { "fill" } else { "color" }, "output": o.clean_output, "layerName": tl!("AI Clean")});
    if m == CleanMethod::Color && !o.text_select_flat_auto {
        let [r, g, b] = o.text_select_flat;
        params["color"] = json!(format!("#{r:02x}{g:02x}{b:02x}"));
    }
    if let Err(e) = app.run(quick::ID, params) {
        app.text_select.cleaning = None;
        abort_plan(app, doc);
        say(app, e);
    }
}

/// Cleans the pieces chosen, each with its method.
fn clean(app: &mut PhotocraftApp) {
    let Some(doc) = app.session.active().map(|st| st.doc.id) else { return };
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    let used = |m: CleanMethod| p.chosen.iter().zip(&p.method).any(|(c, h)| *c && *h == m);
    // The quick ones first, so that the model reads a cleaner page. Last in, first out.
    let plan: Vec<CleanMethod> = [CleanMethod::Ai, CleanMethod::Fill, CleanMethod::Color].into_iter().filter(|m| used(*m)).collect();
    match plan.as_slice() {
        // One method: what's selected, whatever else the selection has.
        [] | [_] => {
            let m = plan.first().copied().unwrap_or(app.ui.tool_options.text_select_method);
            run_method(app, m);
        }
        _ => {
            p.todo.clone_from(&p.chosen);
            app.text_select.plan = plan;
            plan_next(app, doc);
        }
    }
}

/// The next method of a run, on its pieces.
fn plan_next(app: &mut PhotocraftApp, doc: DocId) {
    while let Some(m) = app.text_select.plan.pop() {
        let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
        let grow = p.applied;
        let ids: Vec<u32> = p.detection.regions.iter().filter(|r| p.todo[r.id as usize - 1] && p.method[r.id as usize - 1] == m).map(|r| r.id).collect();
        if ids.is_empty() {
            continue;
        }
        for id in &ids {
            p.todo[*id as usize - 1] = false;
        }
        if let Err(e) = app.run(PIECES, json!({"pieces": ids, "grow": grow})) {
            abort_plan(app, doc);
            say(app, e);
            return;
        }
        run_method(app, m);
        return;
    }
}

/// The next click waiting to clean its block.
fn clean_next(app: &mut PhotocraftApp, doc: DocId) {
    while let Some([x, y]) = app.text_select.queue.pop_front() {
        let Some(p) = app.text_select.docs.get(&doc) else { return };
        let grow = p.applied;
        // A click on what an earlier one already cleaned finds nothing and is dropped.
        let Some(id) = piece_at(&p.detection, x, y, reach(app, p)) else { continue };
        let ids = block_of(p, id);
        if let Err(e) = app.run(PIECES, json!({"pieces": ids, "grow": grow})) {
            say(app, e);
            return;
        }
        let m = app.ui.tool_options.text_select_method;
        run_method(app, m);
        return;
    }
}

/// The pieces of the block `id` is in.
fn block_of(p: &Pieces, id: u32) -> Vec<u32> {
    let group = p.detection.regions.get(id as usize - 1).map(|r| r.group);
    p.detection.regions.iter().filter(|r| Some(r.group) == group).map(|r| r.id).collect()
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

#[derive(Clone, Copy, PartialEq)]
enum Mark {
    Toggle,
    Add,
    Remove,
}

/// `ids` get the method in use, or (a [`Mark::Toggle`] of ones that have it) leave the selection.
fn mark(app: &mut PhotocraftApp, doc: DocId, ids: &[u32], how: Mark) {
    let m = app.ui.tool_options.text_select_method;
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    let at = |id: u32| id as usize - 1;
    let take = how == Mark::Remove || (how == Mark::Toggle && ids.iter().all(|id| p.chosen[at(*id)] && p.method[at(*id)] == m));
    let params = if take {
        let out: Vec<u32> = ids.iter().copied().filter(|id| p.chosen[at(*id)]).collect();
        if out.is_empty() {
            return;
        }
        let keep: Vec<u32> = p.detection.regions.iter().filter(|r| p.chosen[at(r.id)] && !out.contains(&r.id)).map(|r| r.id).collect();
        json!({"pieces": out, "grow": p.applied, "mode": "subtract", "keep": keep})
    } else {
        for id in ids {
            p.method[at(*id)] = m;
        }
        p.marks += 1;
        let new: Vec<u32> = ids.iter().copied().filter(|id| !p.chosen[at(*id)]).collect();
        if new.is_empty() {
            return;
        }
        json!({"pieces": new, "grow": p.applied, "mode": "add"})
    };
    if let Err(e) = app.run(PIECES, params) {
        say(app, e);
    }
}

/// `ids` are the pieces chosen, with the method in use.
fn mark_only(app: &mut PhotocraftApp, doc: DocId, ids: Vec<u32>) {
    let m = app.ui.tool_options.text_select_method;
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    for id in &ids {
        p.method[*id as usize - 1] = m;
    }
    p.marks += 1;
    let grow = p.applied;
    if let Err(e) = app.run(PIECES, json!({"pieces": ids, "grow": grow})) {
        say(app, e);
    }
}

/// A zone drawn on the page joins the pieces, or leaves them.
fn draw_zone(app: &mut PhotocraftApp, doc: DocId, mut params: Value, remove: bool) {
    params["mode"] = json!(if remove { "subtract" } else { "add" });
    if let Err(e) = app.run(ZONE, params) {
        say(app, e);
        return;
    }
    if let Some(new) = app.session.text_detection() {
        adopt(app, doc, new);
    }
}

/// A finished drag or click of the tool (document px).
pub fn finish_stroke(app: &mut PhotocraftApp, points: &[[f64; 3]], mods: egui::Modifiers) {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in points {
        (x0, y0, x1, y1) = (x0.min(p[0]), y0.min(p[1]), x1.max(p[0]), y1.max(p[1]));
    }
    let Some(doc) = app.session.active().map(|st| st.doc.id) else { return };
    sync(app);
    let slop = if crate::touch_ui::active(app) { TAP_SCREEN_TOUCH } else { TAP_SCREEN };
    let dragged = (x1 - x0).max(y1 - y0) > TAP_DOC.max(screen_px(app) * f64::from(slop));
    let remove = mods.alt || app.text_select.erase;
    let shape = app.ui.tool_options.text_select_shape;
    let Some(p) = app.text_select.docs.get(&doc) else {
        if detecting(app, doc) || app.text_select.waiting.is_some() {
            return;
        }
        // A drag with nothing found yet: look there, and only there.
        if dragged && let Err(e) = app.run("select.rect", json!({"x": x0.floor(), "y": y0.floor(), "width": (x1 - x0).ceil(), "height": (y1 - y0).ceil()})) {
            say(app, e);
        }
        detect_here(app);
        return;
    };
    if dragged {
        let (first, last) = (points.first().copied().unwrap_or_default(), points.last().copied().unwrap_or_default());
        let rect = json!({"rect": [first[0].min(last[0]), first[1].min(last[1]), first[0].max(last[0]), first[1].max(last[1])]});
        let xy: Vec<[f64; 2]> = points.iter().map(|q| [q[0], q[1]]).collect();
        match shape {
            ZoneShape::Pick => {
                let (bx0, by0, bx1, by1) = (first[0].min(last[0]), first[1].min(last[1]), first[0].max(last[0]), first[1].max(last[1]));
                // What the box holds at least half of, by its bounds.
                let ids: Vec<u32> = p
                    .detection
                    .regions
                    .iter()
                    .filter(|r| {
                        let [a, b, c, d] = r.bbox.map(f64::from);
                        let (w, h) = ((c.min(bx1) - a.max(bx0)).max(0.0), (d.min(by1) - b.max(by0)).max(0.0));
                        w * h * 2.0 >= (c - a) * (d - b)
                    })
                    .map(|r| r.id)
                    .collect();
                if ids.is_empty() {
                    if !remove {
                        draw_zone(app, doc, rect, false);
                    }
                } else {
                    mark(app, doc, &ids, if remove { Mark::Remove } else { Mark::Add });
                }
            }
            ZoneShape::Rect => draw_zone(app, doc, rect, remove),
            ZoneShape::Lasso => draw_zone(app, doc, json!({"points": xy}), remove),
            ZoneShape::Brush => draw_zone(app, doc, json!({"points": xy, "size": app.session.tools.brush.size}), remove),
        }
        return;
    }
    let Some(&[x, y, _]) = points.first() else { return };
    if app.ui.tool_options.text_select_clean_click {
        app.text_select.queue.push_back([x, y]);
        sync(app);
        return;
    }
    let Some(id) = piece_at(&p.detection, x, y, reach(app, p)) else { return };
    // ⌘ or Ctrl: only that piece, not its block.
    let ids = if mods.command || mods.ctrl { vec![id] } else { block_of(p, id) };
    mark(app, doc, &ids, if remove { Mark::Remove } else { Mark::Toggle });
}

/// The box or the lasso being drawn.
pub(crate) fn draw_drag(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform, points: &[[f64; 3]], mods: egui::Modifiers) {
    let (Some(a), Some(b)) = (points.first(), points.last()) else { return };
    let at = |x: f64, y: f64| xf.to_screen(x as f32, y as f32);
    let t = Tokens::get(painter.ctx());
    let tint = if mods.alt || app.text_select.erase { t.danger } else { t.accent };
    if app.ui.tool_options.text_select_shape == ZoneShape::Lasso {
        let line: Vec<egui::Pos2> = points.iter().map(|p| at(p[0], p[1])).collect();
        painter.add(egui::Shape::closed_line(line, Stroke::new(1.5, tint)));
        return;
    }
    let corners = vec![at(a[0], a[1]), at(b[0], a[1]), at(b[0], b[1]), at(a[0], b[1])];
    painter.add(egui::Shape::convex_polygon(corners, tint.gamma_multiply(0.15), Stroke::new(1.5, tint)));
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
                l if p.chosen[l as usize - 1] || p.todo[l as usize - 1] => look.on[p.method[l as usize - 1] as usize],
                // Nothing is chosen when a click cleans: the model's doubts still show.
                l if look.by_suggestion && d.regions.get(l as usize - 1).is_some_and(|r| r.suggested()) => look.on[look.active as usize],
                _ => look.off,
            });
        }
    }
    egui::ColorImage::new([tw, th], px)
}

fn look(app: &PhotocraftApp, doc: DocId, revision: u64, marks: u64) -> Look {
    let o = &app.ui.tool_options;
    let a = (o.text_select_opacity / 100.0).clamp(0.0, 1.0);
    let tint = |c: [u8; 3], a: f32| Color32::from_rgba_unmultiplied(c[0], c[1], c[2], (a.min(1.0) * 255.0) as u8);
    Look { doc, revision, marks, by_suggestion: o.text_select_clean_click, active: o.text_select_method, on: METHODS.map(|m| tint(method_tint(o, m), a)), off: tint(o.text_select_off, a * OTHERS_STRONGER) }
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
    let Some((revision, marks)) = app.text_select.docs.get(&doc).map(|p| (p.revision, p.marks)) else { return };
    let wanted = look(app, doc, revision, marks);
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
    if let Some(id) = under {
        // A click takes the block, and that's what shows.
        let block = block_of(p, id);
        let boxes = || block.iter().filter_map(|id| p.detection.regions.get(*id as usize - 1)).map(|r| r.bbox.map(|v| v as i32));
        let (x0, y0) = (boxes().map(|b| b[0]).min().unwrap_or(0), boxes().map(|b| b[1]).min().unwrap_or(0));
        let (x1, y1) = (boxes().map(|b| b[2]).max().unwrap_or(0), boxes().map(|b| b[3]).max().unwrap_or(0));
        let edge = Tokens::get(&ctx).accent;
        painter.rect_stroke(xf.doc_rect(Rect::new(x0, y0, x1, y1)), 2.0, Stroke::new(1.5, edge), StrokeKind::Outside);
    }
}

/// What a button of the tool's bars does. The bars of a pointer and of a finger share these.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Act {
    Find,
    FindAgain,
    Method(CleanMethod),
    Shape(ZoneShape),
    NextShape,
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
        Act::Method(m) => {
            app.ui.tool_options.text_select_method = m;
            return;
        }
        Act::Shape(shape) => {
            app.ui.tool_options.text_select_shape = shape;
            return;
        }
        Act::NextShape => {
            let at = SHAPES.iter().position(|s| *s == app.ui.tool_options.text_select_shape).unwrap_or(0);
            app.ui.tool_options.text_select_shape = SHAPES[(at + 1) % SHAPES.len()];
            return;
        }
        Act::All => {
            let ids = app.text_select.docs.get(&doc).map_or_else(Vec::new, |p| p.detection.regions.iter().map(|r| r.id).collect());
            mark_only(app, doc, ids);
            return;
        }
        Act::Suggested => {
            let ids = app.text_select.docs.get(&doc).map_or_else(Vec::new, |p| p.detection.regions.iter().filter(|r| r.suggested()).map(|r| r.id).collect());
            mark_only(app, doc, ids);
            return;
        }
        Act::Invert => {
            let ids = piece_ids(app, false);
            mark_only(app, doc, ids);
            return;
        }
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
            app.text_select.plan.clear();
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

/// How many pieces are chosen, of how many.
fn count_line(p: &Pieces) -> String {
    let on = p.chosen.iter().filter(|c| **c).count();
    crate::i18n::fmt(tl!("{on} of {total} pieces"), &[("on", &on.to_string()), ("total", &p.detection.regions.len().to_string())])
}

/// What the pointer does now.
fn hint_line(app: &PhotocraftApp, p: &Pieces) -> String {
    let touch = crate::touch_ui::active(app);
    if app.ui.tool_options.text_select_clean_click {
        let hint = if touch { tl!("Tap a block to clean it") } else { tl!("Click a block to clean it") };
        let waiting = app.text_select.queue.len();
        let pieces = crate::i18n::fmt(tl!("{total} pieces"), &[("total", &p.detection.regions.len().to_string())]);
        return if waiting > 0 { format!("{pieces}  ·  {}  ·  {hint}", crate::i18n::fmt(tl!("{n} waiting"), &[("n", &waiting.to_string())])) } else { format!("{pieces}  ·  {hint}") };
    }
    match (app.ui.tool_options.text_select_shape, touch) {
        (ZoneShape::Pick, false) => tl!("Click a block or drag a box around several. Ctrl: a single piece. Alt: take away").to_string(),
        (ZoneShape::Pick, true) => tl!("Tap a block or drag a box around several").to_string(),
        (ZoneShape::Rect, false) => tl!("Drag a rectangle to add a zone. Alt cuts it out").to_string(),
        (ZoneShape::Rect, true) => tl!("Drag a rectangle to add a zone").to_string(),
        (ZoneShape::Lasso, false) => tl!("Draw around a zone to add it. Alt cuts it out").to_string(),
        (ZoneShape::Lasso, true) => tl!("Draw around a zone to add it").to_string(),
        (ZoneShape::Brush, false) => tl!("Paint a zone to add it. Alt cuts it out").to_string(),
        (ZoneShape::Brush, true) => tl!("Paint a zone to add it").to_string(),
    }
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
        ui.label(tl!("Finds the text and sound effects on the page, or drag a box to look only there. You choose what to clean"));
        return;
    };
    let mut todo = None;
    if crate::widgets::secondary_button(ui, tl!("Find Again"), 0.0).on_hover_text(tl!("Looks on the whole page again")).clicked() {
        todo = Some(Act::FindAgain);
    }
    crate::widgets::vline(ui, 22.0);
    for shape in SHAPES {
        let on = app.ui.tool_options.text_select_shape == shape;
        if crate::widgets::pill_tab(ui, shape_label(shape), on).on_hover_text(shape_tip(shape)).clicked() {
            todo = Some(Act::Shape(shape));
        }
    }
    crate::widgets::vline(ui, 22.0);
    let more = crate::widgets::secondary_button(ui, tl!("Options"), 0.0);
    egui::Popup::menu(&more).close_behavior(egui::PopupCloseBehavior::IgnoreClicks).show(|ui| {
        ui.set_min_width(280.0);
        ui.label(egui::RichText::new(tl!("Cleaning")).strong());
        ui.horizontal(|ui| {
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
        });
        let mut by_click_box = app.ui.tool_options.text_select_clean_click;
        if crate::widgets::checkbox(ui, &mut by_click_box, tl!("Clean on Click")).on_hover_text(tl!("Each click cleans that block right away, one at a time")).clicked() {
            todo = Some(Act::ToggleClick);
        }
        let o = &mut app.ui.tool_options;
        crate::widgets::checkbox(ui, &mut o.text_select_by_blocks, tl!("Block by Block")).on_hover_text(tl!("Cleans each block of text on its own, one after the other: slower and a little cleaner"));
        ui.horizontal(|ui| {
            crate::widgets::checkbox(ui, &mut o.text_select_flat_auto, tl!("Flat Color from Around Each Block"));
            if !o.text_select_flat_auto {
                crate::widgets::color_edit_button_srgb(ui, &mut o.text_select_flat).on_hover_text(tl!("The color to paint with"));
            }
        });
        ui.add_space(4.0);
        ui.label(tl!("Result in"));
        for (key, label) in crate::remove_ai_ui::output_options() {
            ui.radio_value(&mut o.clean_output, key, label);
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tl!("On the Page")).strong());
        crate::widgets::checkbox(ui, &mut o.text_select_show, tl!("Show Pieces"));
        ui.horizontal(|ui| {
            crate::widgets::color_edit_button_srgb(ui, &mut o.text_select_on).on_hover_text(method_label(CleanMethod::Ai));
            crate::widgets::color_edit_button_srgb(ui, &mut o.text_select_fill_tint).on_hover_text(method_label(CleanMethod::Fill));
            crate::widgets::color_edit_button_srgb(ui, &mut o.text_select_flat_tint).on_hover_text(method_label(CleanMethod::Color));
            crate::widgets::color_edit_button_srgb(ui, &mut o.text_select_off).on_hover_text(tl!("Color of the other pieces"));
            crate::widgets::value_field_named(ui, tl!("Opacity"), &mut o.text_select_opacity, 10.0..=90.0, " %", 54.0);
        });
    });
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

/// A button that's a word.
fn quiet_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    ui.add(egui::Button::new(egui::RichText::new(label).size(12.0).color(t.text)).fill(Color32::TRANSPARENT).stroke(Stroke::NONE).corner_radius(6).min_size(egui::vec2(0.0, 24.0)))
}

/// A method to clean with: its colour on the page, then its name.
fn method_chip(ui: &mut egui::Ui, label: &str, tint: [u8; 3], on: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let color = Color32::from_rgb(tint[0], tint[1], tint[2]);
    let r = ui.add(
        egui::Button::new(egui::RichText::new(format!("     {label}")).color(if on { t.text } else { t.text_dim }))
            .fill(if on { color.gamma_multiply(0.25) } else { Color32::TRANSPARENT })
            .stroke(if on { Stroke::new(1.5, color) } else { Stroke::new(1.0, t.card_border) })
            .corner_radius(8)
            .min_size(egui::vec2(0.0, 30.0)),
    );
    ui.painter().circle_filled(pos2(r.rect.left() + 13.0, r.rect.center().y), 5.0, color);
    r
}

/// What to do on the canvas: which pieces, how to clean them, and what the pointer does.
pub fn bar(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.ui.tool != Tool::TextSelection || !app.ui.dialogs.is_empty() || crate::touch_ui::active(app) {
        return;
    }
    let Some(st) = app.session.active() else { return };
    let doc = st.doc.id;
    let Some(p) = app.text_select.docs.get(&doc) else { return };
    let (count, hint) = (count_line(p), hint_line(app, p));
    let counts = METHODS.map(|m| p.chosen.iter().zip(&p.method).filter(|(c, h)| **c && **h == m).count());
    let chosen: usize = counts.iter().sum();
    let busy = app.session.has_jobs() || !app.text_select.plan.is_empty();
    let by_click = app.ui.tool_options.text_select_clean_click;
    let active = app.ui.tool_options.text_select_method;
    let tints = METHODS.map(|m| method_tint(&app.ui.tool_options, m));
    let t = Tokens::get(ctx);
    let screen = app.last_canvas_rect;
    let id = egui::Id::unique("text-select-bar");
    let size = ctx.memory(|m| m.area_rect(id)).map_or(egui::vec2(560.0, 110.0), |a| a.size());
    let pos = pos2((screen.center().x - size.x / 2.0).max(screen.left() + 8.0), screen.bottom() - size.y - 14.0);
    let mut todo = None;
    egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::new()
            .fill(t.card)
            .stroke(Stroke::new(1.0, t.card_border))
            .corner_radius(12)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .shadow(ui.style().visuals.popup_shadow)
            .show(ui, |ui| {
                ui.set_max_width((screen.width() - 40.0).max(240.0));
                // Which pieces.
                ui.horizontal(|ui| {
                    if !by_click {
                        ui.label(egui::RichText::new(&count).strong());
                        ui.add_space(6.0);
                        for (label, what, tip) in [
                            (tl!("All"), Act::All, tl!("Marks every piece with the method in use")),
                            (tl!("Suggested"), Act::Suggested, tl!("Leaves out what the model doubts")),
                            (tl!("Invert"), Act::Invert, tl!("Marks the others instead")),
                            (tl!("None"), Act::None, tl!("Takes every piece off")),
                        ] {
                            if quiet_button(ui, label).on_hover_text(tip).clicked() {
                                todo = Some(what);
                            }
                        }
                    }
                    ui.add_space(6.0);
                    if bar_button(ui, "x", tl!("Done"), false).on_hover_text("Esc").clicked() {
                        todo = Some(Act::Done);
                    }
                });
                ui.add_space(4.0);
                // How to clean them.
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tl!("Clean with")).color(t.text_dim));
                    for (i, m) in METHODS.into_iter().enumerate() {
                        let label = if by_click || counts[i] == 0 { method_label(m).to_string() } else { format!("{}  {}", method_label(m), counts[i]) };
                        if method_chip(ui, &label, tints[i], m == active).on_hover_text(method_tip(m)).clicked() {
                            todo = Some(Act::Method(m));
                        }
                    }
                    if !by_click {
                        ui.add_space(8.0);
                        let label = crate::i18n::fmt(tl!("Clean {n}"), &[("n", &chosen.to_string())]);
                        if ui.add_enabled_ui(chosen > 0 && !busy, |ui| bar_button(ui, "sparkles", &label, true)).inner.on_hover_text("↩").clicked() {
                            todo = Some(Act::Clean);
                        }
                    }
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&hint).size(11.5).color(t.text_dim)).wrap());
                    if !by_click && chosen > 0 && !busy {
                        ui.add_space(8.0);
                        if quiet_button(ui, tl!("Content-Aware Fill…")).on_hover_text(tl!("With its options, on everything chosen")).clicked() {
                            todo = Some(Act::Fill);
                        }
                        if quiet_button(ui, tl!("Leave as Selection")).clicked() {
                            todo = Some(Act::Keep);
                        }
                    }
                });
            });
    });
    // ↩ and Esc, unless a field is taking the keys.
    if !ctx.egui_wants_keyboard_input() {
        if chosen > 0 && !by_click && !busy && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
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
    let shape = o.text_select_shape;
    let contour = crate::i18n::fmt(tl!("Contour {px}"), &[("px", &format!("{:.0}", p.applied))]);
    let brush = crate::i18n::fmt(tl!("Brush {px}"), &[("px", &format!("{:.0}", app.session.tools.brush.size))]);
    let (count, hint) = (count_line(p), hint_line(app, p));
    let line = if by_click { hint } else { format!("{count}  ·  {hint}") };
    let counts = METHODS.map(|m| p.chosen.iter().zip(&p.method).filter(|(c, h)| **c && **h == m).count());
    let chosen: usize = counts.iter().sum();
    let output = crate::remove_ai_ui::output_options().into_iter().find(|(k, _)| *k == o.clean_output).map_or(tl!("Current Layer"), |(_, l)| l);
    let drawing = crate::i18n::fmt(tl!("Drag: {shape}"), &[("shape", shape_label(shape))]);
    let mut items: Vec<(&str, &str, &str)> = Vec::new();
    if !by_click {
        items.extend([
            ("all", tl!("All"), "check"),
            ("suggested", tl!("Suggested"), "text-select"),
            ("invert", tl!("Invert"), "contrast"),
            ("none", tl!("None"), "x"),
            ("shape", drawing.as_str(), "square-dashed"),
        ]);
        if shape == ZoneShape::Brush {
            items.extend([
                ("smaller", tl!("Smaller Brush"), "minus"),
                ("brush", brush.as_str(), "brush"),
                ("bigger", tl!("Bigger Brush"), "plus"),
            ]);
        }
    }
    items.push(("click", if by_click { tl!("Clean on Tap: On") } else { tl!("Clean on Tap: Off") }, "sparkles"));
    items.extend([
        ("less", tl!("Less Contour"), "minus"),
        ("contour", contour.as_str(), "circle-dashed"),
        ("more", tl!("More Contour"), "plus"),
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
            "shape" => Act::NextShape,
            _ => return None,
        })
    });
    let can_clean = chosen > 0 && !by_click && !app.session.has_jobs() && app.text_select.plan.is_empty();
    let (done_w, paint_w) = (84.0, 96.0);
    let main_w = (area.width() - 16.0 - done_w - paint_w - 2.0 * gap).min(220.0);
    let row_w = done_w + gap + main_w + if by_click { 0.0 } else { paint_w + gap };
    let x0 = area.center().x - row_w / 2.0;
    let done = egui::Rect::from_min_size(pos2(x0, top), egui::vec2(done_w, h));
    let paint = egui::Rect::from_min_size(pos2(done.right() + gap, top), egui::vec2(paint_w, h));
    let main = egui::Rect::from_min_size(pos2(if by_click { done.right() + gap } else { paint.right() + gap }, top), egui::vec2(main_w, h));
    let erasing = app.text_select.erase;
    let active = app.ui.tool_options.text_select_method;
    // How to clean: above, three the same size.
    let chip_h = 44.0;
    let chip_top = top - gap - chip_h;
    let chip_w = (row_w.max(area.width() - 16.0).min(area.width() - 16.0) - 2.0 * gap) / 3.0;
    let chips_x = area.center().x - (chip_w * 3.0 + 2.0 * gap) / 2.0;
    let chip = |i: usize| egui::Rect::from_min_size(pos2(chips_x + (chip_w + gap) * i as f32, chip_top), egui::vec2(chip_w, chip_h));
    let t = Tokens::get(ctx);
    let g = ctx.fonts_mut(|f| f.layout(line.clone(), crate::theme::medium(11.0), t.text, area.width() - 32.0));
    let back = egui::Rect::from_center_size(pos2(area.center().x, chip_top - 8.0 - (g.size().y + 8.0) / 2.0), g.size() + egui::vec2(16.0, 8.0));
    let clean_label = crate::i18n::fmt(tl!("Clean {n}"), &[("n", &chosen.to_string())]);
    let tints = METHODS.map(|m| method_tint(&app.ui.tool_options, m));
    // The area starts here, not at the canvas's top: it would lie over the bar up there and take
    // its touches.
    egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(pos2(area.left(), back.top())).show(ctx, |ui| {
        ui.set_min_size(egui::Vec2::ZERO);
        ui.painter().rect_filled(back, 8.0, t.card);
        ui.painter().galley(back.min + egui::vec2(8.0, 4.0), g, t.text);
        for (i, m) in METHODS.into_iter().enumerate() {
            let label = if by_click || counts[i] == 0 { method_label(m).to_string() } else { format!("{}  {}", method_label(m), counts[i]) };
            let r = chip(i);
            if crate::touch_ui::round_button(ui, r, None, &label, m == active).clicked() {
                todo = Some(Act::Method(m));
            }
            ui.painter().circle_filled(pos2(r.left() + 12.0, r.center().y), 5.0, Color32::from_rgb(tints[i][0], tints[i][1], tints[i][2]));
        }
        if crate::touch_ui::round_button(ui, done, None, tl!("Done"), false).clicked() {
            todo = Some(Act::Done);
        }
        // What a drag does: the big switch to take zones away, as ⌥ does with a keyboard.
        if !by_click && crate::touch_ui::round_button(ui, paint, None, if erasing { tl!("Drag Removes") } else { tl!("Drag Adds") }, erasing).clicked() {
            todo = Some(Act::ToggleErase);
        }
        if can_clean && crate::touch_ui::round_button(ui, main, None, &clean_label, true).clicked() {
            todo = Some(Act::Clean);
        }
    });
    for (i, name) in ["textSelectMethod:ai", "textSelectMethod:fill", "textSelectMethod:color"].into_iter().enumerate() {
        crate::touch_ui::note_rect(ctx, name, chip(i));
    }
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
            let prob: Vec<f32> = page.rgb.chunks_exact(3).map(|c| if c[0] < 128 { 0.995 } else { 0.0 }).collect();
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
        Look { doc, revision: 0, marks: 0, by_suggestion, active: CleanMethod::Ai, on: [Color32::BLUE, Color32::GREEN, Color32::YELLOW], off: Color32::RED }
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
        app.ui.tool_options.text_select_shape = ZoneShape::Rect;
        finish_stroke(&mut app, &[[100.0, 20.0, 1.0], [150.0, 60.0, 1.0]], egui::Modifiers::NONE);
        assert!(covered(&app, 125, 40));
        app.text_select.erase = true;
        finish_stroke(&mut app, &[[100.0, 20.0, 1.0], [150.0, 60.0, 1.0]], egui::Modifiers::NONE);
        assert!(!covered(&app, 125, 40));
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
    fn a_box_marks_the_pieces_in_it_and_on_nothing_it_is_a_zone() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        let drag = |app: &mut PhotocraftApp, a: [f64; 2], b: [f64; 2], m| finish_stroke(app, &[[a[0], a[1], 1.0], [b[0], b[1], 1.0]], m);
        // A box around one takes it off with ⌥ and marks it with the method in use without.
        drag(&mut app, [10.0, 10.0], [80.0, 70.0], egui::Modifiers::ALT);
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].chosen, vec![false, true]);
        app.ui.tool_options.text_select_method = CleanMethod::Fill;
        drag(&mut app, [10.0, 10.0], [80.0, 70.0], egui::Modifiers::NONE);
        sync(&mut app);
        let p = &app.text_select.docs[&doc];
        assert_eq!(p.chosen, vec![true, true]);
        assert_eq!(p.method, vec![CleanMethod::Fill, CleanMethod::Ai]);
        // On nothing: a zone, a piece of its own, with the method in use. No new detection.
        drag(&mut app, [100.0, 20.0], [150.0, 60.0], egui::Modifiers::NONE);
        sync(&mut app);
        let p = &app.text_select.docs[&doc];
        assert_eq!(p.detection.regions.len(), 3);
        assert_eq!(p.chosen, vec![true, true, true]);
        assert_eq!(p.method, vec![CleanMethod::Fill, CleanMethod::Ai, CleanMethod::Fill]);
        assert!(covered(&app, 125, 40));
    }

    #[test]
    fn a_lasso_and_a_brush_draw_zones_and_alt_cuts_them() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        app.ui.tool_options.text_select_shape = ZoneShape::Lasso;
        let shape = [[100.0, 100.0, 1.0], [150.0, 100.0, 1.0], [150.0, 150.0, 1.0], [100.0, 150.0, 1.0]];
        finish_stroke(&mut app, &shape, egui::Modifiers::NONE);
        assert!(covered(&app, 125, 125));
        app.ui.tool_options.text_select_shape = ZoneShape::Brush;
        app.session.tools.brush.size = 10.0;
        finish_stroke(&mut app, &[[100.0, 125.0, 1.0], [150.0, 125.0, 1.0]], egui::Modifiers::ALT);
        assert!(!covered(&app, 125, 125) && covered(&app, 125, 110), "a cut through it");
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].detection.regions.len(), 3);
    }

    #[test]
    fn a_click_takes_the_block_and_with_ctrl_a_single_piece() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.session.execute("file.new", json!({"width": 300, "height": 200, "background": "white"})).unwrap();
        app.session.set_text_masker(Some(Arc::new(Dark)));
        // Two letters of a word, close together.
        for x in [40, 65] {
            app.session.execute("select.rect", json!({"x": x, "y": 40, "width": 20, "height": 30})).unwrap();
            app.session.execute("edit.fill", json!({"color": "#000000"})).unwrap();
        }
        app.session.execute("select.deselect", json!({})).unwrap();
        app.ui.tool = Tool::TextSelection;
        click(&mut app, 150.0, 150.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        assert_eq!(app.text_select.docs[&doc].chosen, vec![true, true]);
        click(&mut app, 50.0, 50.0, egui::Modifiers::CTRL);
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].chosen, vec![true, false], "a single piece: the first in manga order is the right one");
        click(&mut app, 75.0, 50.0, egui::Modifiers::NONE);
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].chosen, vec![true, true], "the block: not all had it");
        click(&mut app, 75.0, 50.0, egui::Modifiers::NONE);
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].chosen, vec![false, false], "the block leaves");
    }

    #[test]
    fn each_piece_is_cleaned_with_its_own_method() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        // One flat, one by content.
        app.ui.tool_options.text_select_method = CleanMethod::Color;
        click(&mut app, 30.0, 30.0, egui::Modifiers::NONE);
        app.ui.tool_options.text_select_method = CleanMethod::Fill;
        click(&mut app, 210.0, 130.0, egui::Modifiers::NONE);
        sync(&mut app);
        let p = &app.text_select.docs[&doc];
        assert_eq!((p.chosen.clone(), p.method.clone()), (vec![true, true], vec![CleanMethod::Color, CleanMethod::Fill]));
        let img = overlay_image(p, &test_look(doc, false));
        assert_ne!(img.pixels[30 * img.size[0] + 30], img.pixels[130 * img.size[0] + 210], "a colour each");
        let ctx = egui::Context::default();
        act(&mut app, &ctx, Act::Clean);
        for _ in 0..4 {
            sync(&mut app);
        }
        let px = |app: &PhotocraftApp, x, y| photocraft_compose::render(&app.session.active().unwrap().doc, Rect::from_xywh(x, y, 1, 1)).px[0][0];
        assert!(px(&app, 30, 30) > 0.9, "the flat one is clean");
        assert!(px(&app, 210, 130) > 0.9, "and the other");
        assert!(app.session.text_detection().is_none(), "nothing left");
        assert!(app.text_select.plan.is_empty());
    }

    #[test]
    fn with_one_method_the_selection_goes_as_it_is() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        app.ui.tool_options.text_select_method = CleanMethod::Color;
        act(&mut app, &egui::Context::default(), Act::All);
        // What another tool added stays in the clean.
        app.session.execute("select.rect", json!({"x": 100, "y": 150, "width": 20, "height": 20, "mode": "add"})).unwrap();
        app.session.execute("edit.fill", json!({"color": "#000000"})).unwrap();
        sync(&mut app);
        assert_eq!(app.text_select.docs[&doc].method, vec![CleanMethod::Color, CleanMethod::Color]);
        act(&mut app, &egui::Context::default(), Act::Clean);
        sync(&mut app);
        let px = photocraft_compose::render(&app.session.active().unwrap().doc, Rect::from_xywh(110, 160, 1, 1)).px[0][0];
        assert!(px > 0.9, "{px}");
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
