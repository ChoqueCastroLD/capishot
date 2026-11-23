//! The Text Selection tool (W group): the mask model finds the page's text and sound effects as
//! pieces, on the whole page or inside the selection, and a person chooses. The model also marks
//! art, so its doubtful pieces start unchosen and nothing runs by itself.
//!
//! The selection is a normal one, and what's chosen is read from it: Undo, the Lasso and
//! Select › Modify all count. Each piece chosen also has how it will be cleaned: Manga LaMa at its
//! fixed best (`edit.removeAi`, as Remove with AI), filled by content, by the Bubble Text Remover
//! (`edit.removeBubbleText`, a click per bubble, AI for a bubble it refuses) or painted flat
//! (`edit.cleanQuick`). Each piece starts with its own: in a speech bubble with a plain background
//! the Bubble Text Remover, anything else AI; pieces of one bubble are one block. Once a method is
//! picked in the bar, a click marks a block with it. A click marks a block, or takes it off; a box around
//! pieces marks them, and on nothing, or with the Rectangle, the Lasso or the Brush, a drag draws a
//! zone of its own, a piece like the others. ⌥ takes away. Clean runs each method on its pieces,
//! the quick ones first. The contour is how far a selection reaches past the letters: too close
//! and cleaning copies their dark edge back as a smudge.
//!
//! All at once or one at a time: with Clean on Click a click cleans that block, and clicks made
//! while one runs wait their turn. Either way what was cleaned leaves the pieces and the rest stay.
//!
//! Undo and Redo are the document's: every change in the tool is a history step (a selection, or
//! `select.textStep` for what the document doesn't hold: a method, a split), and a whole Clean is
//! one (its commands share a `coalesce` key). The tool's own state (the pieces, their methods and
//! blocks) is remembered by history state id ([`History::current_id`]): when Undo or Redo lands
//! on a state, the pieces come back as they were there, with no new detection. Finding the text
//! again on pixels that haven't changed keeps what a person changed of the pieces.
//!
//! [`History::current_id`]: photocraft_ops::History::current_id

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use egui::{Color32, Stroke, StrokeKind, TextureOptions, pos2};
use photocraft_ai::catalog;
use photocraft_ai::models::Status;
use photocraft_ai_textmask::regions::MAX_GROW;
use photocraft_ai_textmask::{Detection, ModelState, default_grow};
use photocraft_doc::DocId;
use photocraft_engine::ai_inpaint_cmds::quick;
use photocraft_engine::text_mask_cmds::{DETECT, PIECES, STEP, ZONE};
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
/// The strip on the canvas, before it has been measured (screen px).
const BAR_WIDTH: f32 = 520.0;
const CHIP_WIDTH: f32 = 96.0;
/// A finger lands wider than a pointer (screen px).
const TAP_SCREEN: f32 = 6.0;
const TAP_SCREEN_TOUCH: f32 = 16.0;
const REACH_SCREEN_TOUCH: f32 = 24.0;

/// A document's detection and what its selection makes of it.
#[derive(Clone)]
struct Pieces {
    detection: Arc<Detection>,
    /// The method each piece starts with (see [`auto_method`]).
    auto: Vec<CleanMethod>,
    /// Pieces taken out of their bubble's block: each a block of its own.
    alone: Vec<bool>,
    /// A method was picked in the bar since the detection: clicks mark with it.
    picked: bool,
    /// The document when the pieces were found: finding the text again on the same pixels
    /// keeps what a person changed.
    page: Arc<photocraft_doc::Document>,
    /// The contour shown, and the one the selection has.
    grow: f32,
    applied: f32,
    /// By piece, `id - 1`.
    chosen: Vec<bool>,
    /// How it's cleaned if it's chosen.
    method: Vec<CleanMethod>,
    /// Still to clean in the run under way.
    todo: Vec<bool>,
    /// Chosen again when a clean of another block ends.
    restore: Vec<bool>,
    /// Its own contour, and where its clean goes, when not the tool's.
    contour: Vec<Option<f32>>,
    output: Vec<Option<String>>,
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
    on: [Color32; 4],
    off: Color32,
}

/// A block's menu, at a screen point.
#[derive(Clone)]
struct BlockMenu {
    doc: DocId,
    ids: Vec<u32>,
    pos: egui::Pos2,
}

/// The tool's state as one history state had it.
#[derive(Clone)]
struct Snap {
    pieces: Option<Pieces>,
}

/// What the tool remembers of a document's history: the state id last seen, and the tool's state
/// at each id (at most [`MAX_SNAPS`], the oldest dropped).
#[derive(Default)]
struct Remembered {
    at: Option<u64>,
    snaps: std::collections::BTreeMap<u64, Snap>,
}

const MAX_SNAPS: usize = 300;

/// The tool's state (`PhotocraftApp::text_select`).
#[derive(Default)]
pub(crate) struct State {
    docs: HashMap<DocId, Pieces>,
    history: HashMap<DocId, Remembered>,
    /// The pieces of a document before it was found again, or closed with Done.
    memory: HashMap<DocId, Pieces>,
    /// The key the commands of a Clean share, so it's one history step.
    run_key: Option<String>,
    runs: u64,
    /// The pieces of the Bubble Text Remover's click under way, for AI if it's refused.
    bubble_running: Option<Vec<u32>>,
    /// Pieces of a run that went to AI because their bubble wasn't plain.
    to_ai: usize,
    /// The document whose detection waits for the model's download.
    waiting: Option<DocId>,
    /// The next detection keeps to the selection.
    in_selection: bool,
    /// A clean under way: the document, its revision then and what it cleans. When the revision
    /// changes that leaves the pieces.
    cleaning: Option<(DocId, u64, photocraft_raster::Surface)>,
    /// The methods left in the run under way, each with where it goes, the next one last.
    plan: Vec<(CleanMethod, Option<String>)>,
    /// A block's menu (right-click, or press and hold).
    menu: Option<BlockMenu>,
    /// The clicks waiting to clean their piece (Clean on Click): document px.
    queue: VecDeque<[f64; 2]>,
    /// What the overlay shows, and its texture.
    overlay: Option<(Look, egui::TextureHandle)>,
    /// On a touch screen, where there's no ⌥: drags take away.
    pub(crate) erase: bool,
}

/// Forgets what was found on document `id` (it went to sleep: [`crate::page_sleep`]).
pub(crate) fn forget(app: &mut PhotocraftApp, id: DocId) {
    app.text_select.docs.remove(&id);
}

const METHODS: [CleanMethod; 4] = [CleanMethod::Ai, CleanMethod::Fill, CleanMethod::Bubble, CleanMethod::Color];

/// How a piece is cleaned unless someone says otherwise (`text_mask_cmds::auto_method`).
fn auto_method(r: &photocraft_ai_textmask::Region) -> CleanMethod {
    if photocraft_engine::text_mask_cmds::auto_method(r) == "bubble" { CleanMethod::Bubble } else { CleanMethod::Ai }
}
const SHAPES: [ZoneShape; 4] = [ZoneShape::Pick, ZoneShape::Rect, ZoneShape::Lasso, ZoneShape::Brush];

fn method_label(m: CleanMethod) -> &'static str {
    match m {
        CleanMethod::Ai => tl!("AI"),
        CleanMethod::Fill => tl!("Content Fill"),
        CleanMethod::Bubble => tl!("Bubble"),
        CleanMethod::Color => tl!("Flat Color"),
    }
}

fn method_tip(m: CleanMethod) -> &'static str {
    match m {
        CleanMethod::Ai => tl!("Manga LaMa draws what was behind the letters. The best over drawings, and it takes a while"),
        CleanMethod::Fill => tl!("Copies from around each block. Quick, for tones and simple backgrounds"),
        CleanMethod::Bubble => tl!("The Bubble Text Remover: the text of a speech bubble with a plain background, without AI. Instant; a bubble it can't clean goes to AI"),
        CleanMethod::Color => tl!("Paints each block in the color around it. Instant, for letters on an even balloon"),
    }
}

fn method_tint(o: &crate::state::ToolOptions, m: CleanMethod) -> [u8; 3] {
    match m {
        CleanMethod::Ai => o.text_select_on,
        CleanMethod::Fill => o.text_select_fill_tint,
        CleanMethod::Bubble => o.text_select_bubble_tint,
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
            let bubble = app.text_select.bubble_running.take();
            if revision != before {
                finish_clean(app, doc, &zone);
            } else if let Some(group) = bubble {
                // The Bubble Text Remover refused the bubble: its pieces go to AI.
                to_ai(app, doc, &group);
                plan_next(app, doc);
            } else {
                abort_plan(app, doc);
            }
        }
    }
    if idle && app.text_select.cleaning.is_none() && app.text_select.plan.is_empty() && app.text_select.queue.is_empty() && app.text_select.run_key.is_some() {
        end_run(app);
    }
    follow_history(app, doc);
    let Some(detection) = app.session.text_detection() else {
        app.text_select.docs.remove(&doc);
        app.text_select.queue.clear();
        remember(app, doc);
        return;
    };
    if !app.text_select.docs.get(&doc).is_some_and(|p| Arc::ptr_eq(&p.detection, &detection)) {
        found(app, doc, detection);
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
    remember(app, doc);
}

/// A new detection on `doc`: each piece with its own method. On pixels that haven't changed
/// since the pieces before it, what a person changed of those (methods, blocks, contours, where
/// they go, what was chosen) is kept, piece by piece.
fn found(app: &mut PhotocraftApp, doc: DocId, detection: Arc<Detection>) {
    let Some(page) = app.session.active().map(|st| st.doc.clone()) else { return };
    let grow = (default_grow(detection.width, detection.height) as f32 + app.ui.tool_options.text_select_contour).clamp(0.0, MAX_GROW as f32);
    let n = detection.regions.len();
    let auto: Vec<CleanMethod> = detection.regions.iter().map(auto_method).collect();
    let before = app.text_select.docs.remove(&doc).or_else(|| app.text_select.memory.remove(&doc));
    let same = before.filter(|b| b.detection.width == detection.width && b.detection.height == detection.height && photocraft_engine::layer_multi_cmds::same_pixels(&b.page, &page));
    let mut p = Pieces {
        detection,
        method: auto.clone(),
        auto,
        alone: vec![false; n],
        picked: false,
        page,
        grow,
        applied: grow,
        chosen: vec![false; n],
        todo: vec![false; n],
        restore: vec![false; n],
        contour: vec![None; n],
        output: vec![None; n],
        marks: 0,
        revision: u64::MAX,
    };
    let mut choose: Option<Vec<u32>> = None;
    if let Some(old) = same {
        let from = matches(&p.detection, &old.detection);
        let old_of = |id: u32| from[id as usize].checked_sub(1).map(|o| o as usize);
        for r in &p.detection.regions {
            let (i, Some(o)) = (r.id as usize - 1, old_of(r.id)) else { continue };
            p.method[i] = old.method[o];
            p.alone[i] = old.alone[o];
            p.contour[i] = old.contour[o];
            p.output[i].clone_from(&old.output[o]);
        }
        p.picked = old.picked;
        (p.grow, p.applied) = (old.grow, old.applied);
        if old.chosen.iter().any(|c| *c) {
            choose = Some(p.detection.regions.iter().filter(|r| old_of(r.id).is_some_and(|o| old.chosen[o])).map(|r| r.id).collect());
        }
    }
    let applied = p.applied;
    app.text_select.docs.insert(doc, p);
    if app.ui.tool_options.text_select_clean_click {
        let _ = app.run("select.deselect", json!({}));
    } else if let Err(e) = app.run(PIECES, json!({"pieces": choose.map_or_else(|| json!("suggested"), |ids| json!(ids)), "grow": applied})) {
        say(app, e);
    }
}

/// For each piece of `new` (by id), the piece of `old` most of it was, 0 none.
fn matches(new: &Detection, old: &Detection) -> Vec<u32> {
    let mut count: HashMap<(u32, u32), u64> = HashMap::new();
    for (n, o) in new.labels.iter().zip(&old.labels) {
        if *n != 0 && *o != 0 {
            *count.entry((*n, *o)).or_default() += 1;
        }
    }
    let mut best = vec![(0u32, 0u64); new.regions.len() + 1];
    for ((n, o), c) in count {
        let b = &mut best[n as usize];
        if c > b.1 || (c == b.1 && o < b.0) {
            *b = (o, c);
        }
    }
    best.into_iter().map(|(o, _)| o).collect()
}

/// Undo or Redo landed on a history state the tool has known: its pieces come back as they were
/// there (not during a Clean, whose steps are one).
fn follow_history(app: &mut PhotocraftApp, doc: DocId) {
    let Some(id) = app.session.active().map(|st| st.history.current_id()) else { return };
    let mem = app.text_select.history.entry(doc).or_default();
    if mem.at == Some(id) {
        return;
    }
    let arrived = mem.at.is_some();
    mem.at = Some(id);
    if !arrived || app.text_select.run_key.is_some() {
        return;
    }
    let Some(snap) = mem.snaps.get(&id).cloned() else { return };
    app.text_select.plan.clear();
    app.text_select.queue.clear();
    match snap.pieces {
        Some(mut p) => {
            app.session.set_text_detection(p.detection.clone());
            p.revision = u64::MAX;
            p.marks += 1;
            p.todo.iter_mut().for_each(|t| *t = false);
            p.restore.iter_mut().for_each(|t| *t = false);
            app.text_select.docs.insert(doc, p);
        }
        None => {
            app.session.forget_text_detection();
            app.text_select.docs.remove(&doc);
        }
    }
}

/// What the tool has now, remembered for the current history state.
fn remember(app: &mut PhotocraftApp, doc: DocId) {
    let Some(id) = app.session.active().filter(|st| st.doc.id == doc).map(|st| st.history.current_id()) else { return };
    let pieces = app.text_select.docs.get(&doc).cloned();
    let mem = app.text_select.history.entry(doc).or_default();
    mem.at = Some(id);
    mem.snaps.insert(id, Snap { pieces });
    while mem.snaps.len() > MAX_SNAPS {
        mem.snaps.pop_first();
    }
}

/// Runs a command of the tool: during a Clean, as part of its one history step.
fn cmd(app: &mut PhotocraftApp, id: &str, mut params: Value) -> Result<Value, String> {
    if let Some(key) = &app.text_select.run_key {
        params["coalesce"] = json!(key);
    }
    app.run(id, params)
}

/// A history step for a change the document doesn't hold (a method, a block split).
fn tool_step(app: &mut PhotocraftApp) {
    let _ = cmd(app, STEP, json!({"label": tl!("Text Selection")}));
}

/// A Clean starts: its commands from here on are one history step.
fn begin_run(app: &mut PhotocraftApp) {
    if app.text_select.run_key.is_some() {
        return;
    }
    app.text_select.runs += 1;
    let key = format!("textSelect.clean.{}", app.text_select.runs);
    app.text_select.to_ai = 0;
    let _ = app.run(STEP, json!({"label": tl!("Clean Text"), "coalesce": key}));
    app.text_select.run_key = Some(key);
}

/// A Clean ended: what went to AI that was meant for the Bubble Text Remover is said.
fn end_run(app: &mut PhotocraftApp) {
    app.text_select.run_key = None;
    let n = std::mem::take(&mut app.text_select.to_ai);
    if n > 0 {
        app.ui.status = crate::i18n::fmt(tl!("{n} pieces were in bubbles without a plain background: cleaned with AI"), &[("n", &n.to_string())]);
        app.ui.status_error = false;
    }
}

/// The Bubble Text Remover refused `ids`' bubble: they're cleaned with AI, after the rest.
fn to_ai(app: &mut PhotocraftApp, doc: DocId, ids: &[u32]) {
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    let Some(first) = ids.first() else { return };
    let output = p.output[*first as usize - 1].clone();
    for id in ids {
        p.method[*id as usize - 1] = CleanMethod::Ai;
        p.todo[*id as usize - 1] = true;
    }
    p.marks += 1;
    app.text_select.to_ai += ids.len();
    if !app.text_select.plan.iter().any(|(m, o)| *m == CleanMethod::Ai && *o == output) {
        app.text_select.plan.insert(0, (CleanMethod::Ai, output));
    }
}

/// `new` takes the place of the document's pieces: what's left of each keeps its marks, and what's
/// new has the method in use (picked in the bar), else its own.
fn adopt(app: &mut PhotocraftApp, doc: DocId, new: Arc<Detection>) {
    let active = app.ui.tool_options.text_select_method;
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    let from = matches(&new, &p.detection);
    let old = |id: u32| from[id as usize].checked_sub(1).map(|o| o as usize);
    let fresh = |r: &photocraft_ai_textmask::Region| if p.picked { active } else { auto_method(r) };
    p.method = new.regions.iter().map(|r| old(r.id).map_or_else(|| fresh(r), |o| p.method[o])).collect();
    p.auto = new.regions.iter().map(|r| old(r.id).map_or_else(|| auto_method(r), |o| p.auto[o])).collect();
    p.alone = new.regions.iter().map(|r| old(r.id).is_some_and(|o| p.alone[o])).collect();
    p.todo = new.regions.iter().map(|r| old(r.id).is_some_and(|o| p.todo[o])).collect();
    p.restore = new.regions.iter().map(|r| old(r.id).is_some_and(|o| p.restore[o])).collect();
    p.contour = new.regions.iter().map(|r| old(r.id).and_then(|o| p.contour[o])).collect();
    p.output = new.regions.iter().map(|r| old(r.id).and_then(|o| p.output[o].clone())).collect();
    p.chosen = vec![false; new.regions.len()];
    p.detection = new;
    p.revision = u64::MAX;
    p.marks += 1;
}

/// A clean ended: what it cleaned leaves the pieces, and the selection with it. The run goes on.
fn finish_clean(app: &mut PhotocraftApp, doc: DocId, zone: &photocraft_raster::Surface) {
    let _ = cmd(app, "select.deselect", json!({}));
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

/// The selection takes `ids` (`mode`: "replace", "add", "subtract"), each to its contour.
fn select_pieces(app: &mut PhotocraftApp, doc: DocId, ids: &[u32], mode: &str, keep: Option<&[u32]>) {
    let Some(p) = app.text_select.docs.get(&doc) else { return };
    let mut groups: Vec<(f32, Vec<u32>)> = Vec::new();
    for id in ids {
        let reach = p.contour[*id as usize - 1].unwrap_or(p.applied);
        match groups.iter_mut().find(|(g, _)| *g == reach) {
            Some((_, g)) => g.push(*id),
            None => groups.push((reach, vec![*id])),
        }
    }
    for (n, (grow, ids)) in groups.into_iter().enumerate() {
        let mode = if n > 0 && mode == "replace" { "add" } else { mode };
        let mut params = json!({"pieces": ids, "grow": grow, "mode": mode});
        if let Some(keep) = keep {
            params["keep"] = json!(keep);
        }
        if let Err(e) = cmd(app, PIECES, params) {
            say(app, e);
            return;
        }
    }
}

/// What `flag` marks, and it's cleared.
fn take_flagged(p: &mut Pieces, flag: fn(&mut Pieces) -> &mut Vec<bool>) -> Vec<u32> {
    let flags = std::mem::take(flag(p));
    let ids = p.detection.regions.iter().filter(|r| flags[r.id as usize - 1]).map(|r| r.id).collect();
    *flag(p) = vec![false; flags.len()];
    ids
}

/// A clean changed nothing, or was cancelled: what was left of the run is chosen again.
fn abort_plan(app: &mut PhotocraftApp, doc: DocId) {
    app.text_select.plan.clear();
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    let mut left = take_flagged(p, |p| &mut p.todo);
    left.extend(take_flagged(p, |p| &mut p.restore));
    if !left.is_empty() {
        select_pieces(app, doc, &left, "add", None);
    }
}

/// A run ended: what was chosen and not in it is chosen again.
fn restore_chosen(app: &mut PhotocraftApp, doc: DocId) {
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    let left = take_flagged(p, |p| &mut p.restore);
    if !left.is_empty() {
        select_pieces(app, doc, &left, "add", None);
    }
}

/// Cleans what's selected with `m`, into `output` or where the tool's go. Not the Bubble Text
/// Remover, which goes bubble by bubble ([`plan_next`]).
fn run_method(app: &mut PhotocraftApp, m: CleanMethod, output: Option<String>) {
    let Some(st) = app.session.active() else { return };
    let (doc, revision) = (st.doc.id, st.revision);
    let Some(zone) = st.doc.selection.clone() else { return };
    app.text_select.cleaning = Some((doc, revision, zone));
    let o = &app.ui.tool_options;
    let output = output.unwrap_or_else(|| o.clean_output.clone());
    if m == CleanMethod::Ai || m == CleanMethod::Bubble {
        let all = o.sample_all.remove_ai;
        let by_blocks = o.text_select_by_blocks;
        let mut params = json!({"sampleAllLayers": all, "byBlocks": by_blocks, "output": output});
        if let Some(key) = &app.text_select.run_key {
            params["coalesce"] = json!(key);
        }
        crate::remove_ai_ui::request(app, params);
        return;
    }
    let mut params = json!({"method": if m == CleanMethod::Fill { "fill" } else { "color" }, "output": output, "layerName": tl!("AI Clean")});
    if m == CleanMethod::Color && !o.text_select_flat_auto {
        let [r, g, b] = o.text_select_flat;
        params["color"] = json!(format!("#{r:02x}{g:02x}{b:02x}"));
    }
    if let Err(e) = cmd(app, quick::ID, params) {
        app.text_select.cleaning = None;
        abort_plan(app, doc);
        say(app, e);
    }
}

/// The Bubble Text Remover's click at `seed` for the pieces `ids` (selected): if it changes
/// nothing (the bubble isn't plain, or isn't one), they go to AI.
fn run_bubble(app: &mut PhotocraftApp, doc: DocId, seed: [f64; 2], ids: Vec<u32>, output: Option<String>) {
    let Some(st) = app.session.active() else { return };
    let revision = st.revision;
    let zone = st.doc.selection.clone();
    let o = &app.ui.tool_options;
    let new_layer = output.as_deref().unwrap_or(o.clean_output.as_str()) != "current";
    let params = json!({"x": seed[0], "y": seed[1], "newLayer": new_layer, "sampleAll": o.sample_all.remove_ai});
    let Some(zone) = zone else {
        to_ai(app, doc, &ids);
        plan_next(app, doc);
        return;
    };
    app.text_select.cleaning = Some((doc, revision, zone));
    app.text_select.bubble_running = Some(ids.clone());
    if cmd(app, photocraft_engine::bubble_text_cmds::ID, params).is_err() {
        app.text_select.cleaning = None;
        app.text_select.bubble_running = None;
        to_ai(app, doc, &ids);
        plan_next(app, doc);
    }
}

/// `ids` are cleaned by the Bubble Text Remover, a bubble at a time, after what the plan has.
fn start_bubbles(app: &mut PhotocraftApp, doc: DocId, ids: &[u32], output: Option<String>) {
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    for id in ids {
        p.method[*id as usize - 1] = CleanMethod::Bubble;
        p.todo[*id as usize - 1] = true;
        p.output[*id as usize - 1].clone_from(&output);
    }
    p.marks += 1;
    if !app.text_select.plan.iter().any(|(m, o)| *m == CleanMethod::Bubble && *o == output) {
        app.text_select.plan.push((CleanMethod::Bubble, output));
    }
    plan_next(app, doc);
}

/// Cleans the pieces chosen, each with its method.
fn clean(app: &mut PhotocraftApp) {
    let Some(doc) = app.session.active().map(|st| st.doc.id) else { return };
    if !app.text_select.docs.contains_key(&doc) {
        return;
    }
    begin_run(app);
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    // Last in, first out: the quick ones first, so that the model reads a cleaner page.
    let mut plan: Vec<(CleanMethod, Option<String>)> = Vec::new();
    for m in [CleanMethod::Ai, CleanMethod::Fill, CleanMethod::Bubble, CleanMethod::Color] {
        for r in &p.detection.regions {
            let at = r.id as usize - 1;
            if p.chosen[at] && p.method[at] == m && !plan.iter().any(|(pm, po)| *pm == m && *po == p.output[at]) {
                plan.push((m, p.output[at].clone()));
            }
        }
    }
    if plan.len() <= 1 && plan.first().is_none_or(|(m, _)| *m != CleanMethod::Bubble) {
        // One way to go: what's selected, whatever else the selection has.
        let (m, output) = plan.pop().unwrap_or((CleanMethod::Ai, None));
        run_method(app, m, output);
        return;
    }
    p.todo.clone_from(&p.chosen);
    app.text_select.plan = plan;
    plan_next(app, doc);
}

/// Cleans one block now; the others chosen stay so.
fn clean_block(app: &mut PhotocraftApp, doc: DocId, ids: &[u32], m: CleanMethod) {
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    p.restore.clone_from(&p.chosen);
    for id in ids {
        p.restore[*id as usize - 1] = false;
    }
    let output = ids.first().and_then(|id| p.output[*id as usize - 1].clone());
    begin_run(app);
    if m == CleanMethod::Bubble {
        start_bubbles(app, doc, ids, output);
        return;
    }
    select_pieces(app, doc, ids, "replace", None);
    run_method(app, m, output);
}

/// The next method of a run, on its pieces.
fn plan_next(app: &mut PhotocraftApp, doc: DocId) {
    while let Some((m, output)) = app.text_select.plan.pop() {
        let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
        let ids: Vec<u32> = p.detection.regions.iter().map(|r| r.id).filter(|id| p.todo[*id as usize - 1] && p.method[*id as usize - 1] == m && p.output[*id as usize - 1] == output).collect();
        if ids.is_empty() {
            continue;
        }
        if m == CleanMethod::Bubble {
            // A click per bubble: this one now, the others next.
            let Some((seed, group)) = photocraft_engine::text_mask_cmds::bubble_clicks(&p.detection, &ids).into_iter().next() else { continue };
            for id in &group {
                p.todo[*id as usize - 1] = false;
            }
            if group.len() < ids.len() {
                app.text_select.plan.push((m, output.clone()));
            }
            select_pieces(app, doc, &group, "replace", None);
            run_bubble(app, doc, seed, group, output);
            return;
        }
        for id in &ids {
            p.todo[*id as usize - 1] = false;
        }
        select_pieces(app, doc, &ids, "replace", None);
        run_method(app, m, output);
        return;
    }
    restore_chosen(app, doc);
}

/// The next click waiting to clean its block.
fn clean_next(app: &mut PhotocraftApp, doc: DocId) {
    while let Some([x, y]) = app.text_select.queue.pop_front() {
        let Some(p) = app.text_select.docs.get(&doc) else { return };
        // A click on what an earlier one already cleaned finds nothing and is dropped.
        let Some(id) = piece_at(&p.detection, x, y, reach(app, p)) else { continue };
        let ids = block_of(p, id);
        let output = p.output[id as usize - 1].clone();
        // The method picked in the bar, else the block's own.
        let m = if p.picked { app.ui.tool_options.text_select_method } else { p.method[id as usize - 1] };
        begin_run(app);
        if m == CleanMethod::Bubble {
            start_bubbles(app, doc, &ids, output);
            return;
        }
        select_pieces(app, doc, &ids, "replace", None);
        run_method(app, m, output);
        return;
    }
}

/// The pieces of the block `id` is in: its bubble's, else its word's or effect's; a piece split
/// from its block is one alone.
fn block_of(p: &Pieces, id: u32) -> Vec<u32> {
    let Some(r) = p.detection.regions.get(id as usize - 1) else { return Vec::new() };
    if p.alone.get(id as usize - 1).copied().unwrap_or(false) {
        return vec![id];
    }
    let same = |o: &photocraft_ai_textmask::Region| if r.bubble > 0 { o.bubble == r.bubble } else { o.bubble == 0 && o.group == r.group };
    p.detection.regions.iter().filter(|o| same(o) && !p.alone[o.id as usize - 1]).map(|o| o.id).collect()
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
    let active = app.ui.tool_options.text_select_method;
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    let at = |id: u32| id as usize - 1;
    // Until a method is picked in the bar, each piece keeps its own.
    let with = p.picked.then_some(active);
    let take = how == Mark::Remove || (how == Mark::Toggle && ids.iter().all(|id| p.chosen[at(*id)] && with.is_none_or(|m| p.method[at(*id)] == m)));
    if take {
        let out: Vec<u32> = ids.iter().copied().filter(|id| p.chosen[at(*id)]).collect();
        let keep: Vec<u32> = p.detection.regions.iter().filter(|r| p.chosen[at(r.id)] && !out.contains(&r.id)).map(|r| r.id).collect();
        select_pieces(app, doc, &out, "subtract", Some(&keep));
    } else {
        if let Some(m) = with {
            for id in ids {
                p.method[at(*id)] = m;
            }
            p.marks += 1;
        }
        let new: Vec<u32> = ids.iter().copied().filter(|id| !p.chosen[at(*id)]).collect();
        if new.is_empty() {
            // Only the method changed: a step of its own all the same.
            tool_step(app);
        } else {
            select_pieces(app, doc, &new, "add", None);
        }
    }
}

/// `ids` are the pieces chosen, with the method in use.
fn mark_only(app: &mut PhotocraftApp, doc: DocId, ids: Vec<u32>) {
    let m = app.ui.tool_options.text_select_method;
    let Some(p) = app.text_select.docs.get_mut(&doc) else { return };
    // Until a method is picked in the bar, each piece keeps its own.
    if p.picked {
        for id in &ids {
            p.method[*id as usize - 1] = m;
        }
    }
    p.marks += 1;
    select_pieces(app, doc, &ids, "replace", None);
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

/// The colour of a drag: what it adds, or (⌥, or Drag Removes on a touch screen) takes away.
pub(crate) fn drag_tint(app: &PhotocraftApp, ctx: &egui::Context, mods: egui::Modifiers) -> Color32 {
    let t = Tokens::get(ctx);
    if mods.alt || app.text_select.erase { t.danger } else { t.accent }
}

/// The box or the lasso being drawn, as it's dragged: a line with a dark edge so it reads over
/// any page, in the colour of adding or of taking away (which is dashed). The brush's stroke is
/// the canvas's trail, in the same colour (`paint::draw_drag_preview`).
pub(crate) fn draw_drag(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform, points: &[[f64; 3]], mods: egui::Modifiers) {
    let (Some(a), Some(b)) = (points.first(), points.last()) else { return };
    let at = |x: f64, y: f64| xf.to_screen(x as f32, y as f32);
    let tint = drag_tint(app, painter.ctx(), mods);
    let away = mods.alt || app.text_select.erase;
    let halo = Stroke::new(3.5, Color32::from_black_alpha(140));
    let line = Stroke::new(2.0, tint);
    let outline = |pts: Vec<egui::Pos2>| {
        painter.add(egui::Shape::closed_line(pts.clone(), halo));
        if away {
            let mut ring = pts.clone();
            ring.extend(pts.first().copied());
            painter.extend(egui::Shape::dashed_line(&ring, line, 6.0, 4.0));
        } else {
            painter.add(egui::Shape::closed_line(pts, line));
        }
    };
    if app.ui.tool_options.text_select_shape == ZoneShape::Lasso {
        outline(points.iter().map(|p| at(p[0], p[1])).collect());
        return;
    }
    let corners = vec![at(a[0], a[1]), at(b[0], a[1]), at(b[0], b[1]), at(a[0], b[1])];
    painter.add(egui::Shape::convex_polygon(corners.clone(), tint.gamma_multiply(0.18), Stroke::NONE));
    outline(corners);
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
                l if look.by_suggestion && d.regions.get(l as usize - 1).is_some_and(|r| r.suggested()) => look.on[p.method[l as usize - 1] as usize],
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
    // The model loads while the person looks at the page: the first detection doesn't wait for it.
    if let Some(m) = app.session.text_masker() {
        m.warm();
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
            // From now on clicks mark with it.
            if let Some(p) = app.text_select.docs.get_mut(&doc) {
                p.picked = true;
            }
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
            if let Some(p) = app.text_select.docs.get(&doc) {
                app.text_select.memory.insert(doc, p.clone());
            }
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
    // A block with its own contour keeps it.
    let ids: Vec<u32> = p.detection.regions.iter().filter(|r| p.chosen[r.id as usize - 1] && p.contour[r.id as usize - 1].is_none()).map(|r| r.id).collect();
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
    let (count, hint, by_click) = {
        let Some(p) = app.text_select.docs.get(&doc) else { return };
        (count_line(p), hint_line(app, p), app.ui.tool_options.text_select_clean_click)
    };
    if !by_click {
        ui.label(egui::RichText::new(&count).strong());
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
        crate::widgets::vline(ui, 22.0);
    }
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
            crate::widgets::color_edit_button_srgb(ui, &mut o.text_select_bubble_tint).on_hover_text(method_label(CleanMethod::Bubble));
            crate::widgets::color_edit_button_srgb(ui, &mut o.text_select_flat_tint).on_hover_text(method_label(CleanMethod::Color));
            crate::widgets::color_edit_button_srgb(ui, &mut o.text_select_off).on_hover_text(tl!("Color of the other pieces"));
            crate::widgets::value_field_named(ui, tl!("Opacity"), &mut o.text_select_opacity, 10.0..=90.0, " %", 54.0);
        });
        if has_selection(app) && !by_click && !app.session.has_jobs() {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(tl!("What's Chosen")).strong());
            if quiet_button(ui, tl!("Content-Aware Fill…")).on_hover_text(tl!("With its options, on everything chosen")).clicked() {
                todo = Some(Act::Fill);
                egui::Popup::close_all(&ctx);
            }
            if quiet_button(ui, tl!("Leave as Selection")).clicked() {
                todo = Some(Act::Keep);
                egui::Popup::close_all(&ctx);
            }
        }
    });
    // What the pointer does, in the room that's left.
    crate::widgets::vline(ui, 22.0);
    let dim = Tokens::get(&ctx).text_dim;
    ui.add(egui::Label::new(egui::RichText::new(&hint).size(11.5).color(dim)).truncate());
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
            .min_size(egui::vec2(if main { CHIP_WIDTH + 14.0 } else { 0.0 }, 30.0)),
    )
}

/// A block's menu is open: Esc is for it, not to deselect.
pub(crate) fn menu_open(app: &PhotocraftApp) -> bool {
    app.text_select.menu.is_some()
}

/// A right-click, or a press and hold, on a block opens its menu.
pub(crate) fn open_menu(app: &mut PhotocraftApp, tool: Tool, at: [f64; 2], pos: [f32; 2]) -> bool {
    if tool != Tool::TextSelection {
        return false;
    }
    let Some(doc) = app.session.active().map(|st| st.doc.id) else { return false };
    let Some(p) = app.text_select.docs.get(&doc) else { return false };
    let Some(id) = piece_at(&p.detection, at[0], at[1], reach(app, p)) else { return false };
    let ids = block_of(p, id);
    app.text_select.menu = Some(BlockMenu { doc, ids, pos: pos2(pos[0], pos[1]) });
    true
}

/// What a block's menu asks for.
enum MenuAct {
    Toggle,
    Clean(CleanMethod),
    Method(CleanMethod),
    Output(Option<String>),
    /// Each piece of the block a block of its own.
    Split,
}

fn menu_item(ui: &mut egui::Ui, icon: &str, label: &str, enabled: bool, height: f32) -> bool {
    let t = Tokens::get(ui.ctx());
    let width = ui.available_width();
    ui.add_enabled(enabled, egui::Button::image_and_text(crate::icons::image(icon, 15.0, t.text), label).fill(Color32::TRANSPARENT).stroke(Stroke::NONE).corner_radius(6).min_size(egui::vec2(width, height))).clicked()
}

/// A block's own actions.
fn block_menu(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(menu) = app.text_select.menu.clone() else { return };
    let (doc, ids) = (menu.doc, menu.ids.clone());
    let active = app.session.active().map(|st| st.doc.id);
    let valid = app.text_select.docs.get(&doc).is_some_and(|p| ids.iter().all(|id| (*id as usize) <= p.chosen.len()));
    if app.ui.tool != Tool::TextSelection || active != Some(doc) || !valid {
        app.text_select.menu = None;
        return;
    }
    let Some(p) = app.text_select.docs.get(&doc) else { return };
    let first = ids[0] as usize - 1;
    let on = ids.iter().all(|id| p.chosen[*id as usize - 1]);
    let (method, output, grow) = (p.method[first], p.output[first].clone(), p.contour[first].unwrap_or(p.applied));
    let busy = app.session.has_jobs() || !app.text_select.plan.is_empty();
    let t = Tokens::get(ctx);
    let h = if crate::touch_ui::active(app) { 44.0 } else { 28.0 };
    let id = egui::Id::unique("text-select-menu");
    let screen = crate::safe_area::rect(ctx);
    let size = ctx.memory(|m| m.area_rect(id)).map_or(egui::vec2(270.0, 380.0), |a| a.size());
    let pos = pos2(menu.pos.x.min(screen.right() - size.x - 4.0).max(screen.left() + 4.0), menu.pos.y.min(screen.bottom() - size.y - 4.0).max(screen.top() + 4.0));
    let mut todo = None;
    let mut new_grow = grow;
    let mut grow_step = None;
    let mut grow_done = false;
    let shown = egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_width(250.0);
            ui.label(egui::RichText::new(crate::i18n::fmt(tl!("This block: {n} pieces"), &[("n", &ids.len().to_string())])).strong());
            if menu_item(ui, if on { "x" } else { "check" }, if on { tl!("Take Off the Selection") } else { tl!("Add to the Selection") }, !busy, h) {
                todo = Some(MenuAct::Toggle);
            }
            if ids.len() > 1 && menu_item(ui, "scissors", tl!("Split into Pieces"), !busy, h) {
                todo = Some(MenuAct::Split);
            }
            ui.separator();
            ui.label(egui::RichText::new(tl!("Clean Now")).color(t.text_dim));
            for (icon, label, m) in [
                ("sparkles", tl!("AI"), CleanMethod::Ai),
                ("paint-bucket", tl!("Content Fill"), CleanMethod::Fill),
                ("bubble-text-remove", tl!("Bubble"), CleanMethod::Bubble),
                ("droplet", tl!("Flat Color"), CleanMethod::Color),
            ] {
                if menu_item(ui, icon, label, !busy, h) {
                    todo = Some(MenuAct::Clean(m));
                }
            }
            ui.separator();
            ui.label(egui::RichText::new(tl!("In the Next Clean")).color(t.text_dim));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                for m in METHODS {
                    if crate::widgets::pill_tab(ui, method_label(m), m == method).on_hover_text(method_tip(m)).clicked() {
                        todo = Some(MenuAct::Method(m));
                    }
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(tl!("Contour"));
                let r = crate::widgets::value_field_named(ui, tl!("Contour"), &mut new_grow, 0.0..=MAX_GROW as f32, " px", 54.0);
                new_grow = new_grow.round();
                grow_done = !r.has_focus();
                if r.dragged() {
                    grow_step = crate::panels::layer_props_ui::drag_step(ctx);
                }
            });
            ui.label(egui::RichText::new(tl!("Result in")).color(t.text_dim));
            let mut pick = output.clone();
            ui.radio_value(&mut pick, None, tl!("As the Rest"));
            for (key, label) in crate::remove_ai_ui::output_options() {
                ui.radio_value(&mut pick, Some(key), label);
            }
            if pick != output {
                todo = Some(MenuAct::Output(pick));
            }
        });
    });
    crate::touch_ui::note_rect(ctx, "textSelectMenu", shown.response.rect);
    // A press outside closes it, and Esc.
    let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|at| !shown.response.rect.contains(at)));
    if outside || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        app.text_select.menu = None;
    }
    if new_grow != grow && grow_done {
        if let Some(p) = app.text_select.docs.get_mut(&doc) {
            for id in &ids {
                p.contour[*id as usize - 1] = Some(new_grow);
            }
        }
        if on {
            let mut params = json!({"pieces": ids, "grow": new_grow, "from": grow});
            if let Some(step) = grow_step {
                params["coalesce"] = json!(step);
            }
            if let Err(e) = app.run(PIECES, params) {
                say(app, e);
            }
        }
    }
    let Some(what) = todo else { return };
    match what {
        MenuAct::Toggle => {
            mark(app, doc, &ids, if on { Mark::Remove } else { Mark::Add });
            app.text_select.menu = None;
        }
        MenuAct::Clean(m) => {
            app.text_select.menu = None;
            clean_block(app, doc, &ids, m);
        }
        MenuAct::Method(m) => {
            if let Some(p) = app.text_select.docs.get_mut(&doc) {
                for id in &ids {
                    p.method[*id as usize - 1] = m;
                }
                p.marks += 1;
            }
            tool_step(app);
        }
        MenuAct::Split => {
            if let Some(p) = app.text_select.docs.get_mut(&doc) {
                for id in &ids {
                    p.alone[*id as usize - 1] = true;
                }
                p.marks += 1;
            }
            app.text_select.menu = None;
            tool_step(app);
        }
        MenuAct::Output(o) => {
            if let Some(p) = app.text_select.docs.get_mut(&doc) {
                for id in &ids {
                    p.output[*id as usize - 1].clone_from(&o);
                }
            }
        }
    }
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
            // Wide enough for a count: the buttons stay under the pointer.
            .min_size(egui::vec2(CHIP_WIDTH, 30.0)),
    );
    ui.painter().circle_filled(pos2(r.rect.left() + 13.0, r.rect.center().y), 5.0, color);
    r
}

/// How to clean what's chosen, on the canvas: one line, as little of the page as it can be.
pub fn bar(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.ui.tool != Tool::TextSelection || !app.ui.dialogs.is_empty() || crate::touch_ui::active(app) {
        return;
    }
    block_menu(app, ctx);
    let Some(st) = app.session.active() else { return };
    let doc = st.doc.id;
    let Some(p) = app.text_select.docs.get(&doc) else { return };
    let counts = METHODS.map(|m| p.chosen.iter().zip(&p.method).filter(|(c, h)| **c && **h == m).count());
    let chosen: usize = counts.iter().sum();
    let busy = app.session.has_jobs() || !app.text_select.plan.is_empty();
    let by_click = app.ui.tool_options.text_select_clean_click;
    let active = app.ui.tool_options.text_select_method;
    let tints = METHODS.map(|m| method_tint(&app.ui.tool_options, m));
    let t = Tokens::get(ctx);
    let screen = app.last_canvas_rect;
    let id = egui::Id::unique("text-select-bar");
    let size = ctx.memory(|m| m.area_rect(id)).map_or(egui::vec2(BAR_WIDTH, 46.0), |a| a.size());
    let pos = pos2((screen.center().x - size.x / 2.0).max(screen.left() + 8.0), screen.bottom() - size.y - 10.0);
    let mut todo = None;
    // A contextual bar: a press beside it is the tool's.
    crate::overlay_guard::non_modal(ctx, id);
    egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::new()
            .fill(t.card)
            .stroke(Stroke::new(1.0, t.card_border))
            .corner_radius(12)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .shadow(ui.style().visuals.popup_shadow)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (i, m) in METHODS.into_iter().enumerate() {
                        let label = if by_click || counts[i] == 0 { method_label(m).to_string() } else { format!("{}  {}", method_label(m), counts[i]) };
                        if method_chip(ui, &label, tints[i], m == active).on_hover_text(method_tip(m)).clicked() {
                            todo = Some(Act::Method(m));
                        }
                    }
                    if !by_click {
                        let label = crate::i18n::fmt(tl!("Clean {n}"), &[("n", &chosen.to_string())]);
                        if ui.add_enabled_ui(chosen > 0 && !busy, |ui| bar_button(ui, "sparkles", &label, true)).inner.on_hover_text("↩").clicked() {
                            todo = Some(Act::Clean);
                        }
                    }
                    if bar_button(ui, "x", tl!("Done"), false).on_hover_text("Esc").clicked() {
                        todo = Some(Act::Done);
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
    block_menu(app, ctx);
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
    let (done_w, paint_w) = (72.0, 132.0);
    let main_w = (area.width() - 16.0 - done_w - paint_w - 2.0 * gap).min(220.0);
    let row_w = done_w + gap + main_w + if by_click { 0.0 } else { paint_w + gap };
    let x0 = area.center().x - row_w / 2.0;
    let done = egui::Rect::from_min_size(pos2(x0, top), egui::vec2(done_w, h));
    let paint = egui::Rect::from_min_size(pos2(done.right() + gap, top), egui::vec2(paint_w, h));
    let main = egui::Rect::from_min_size(pos2(if by_click { done.right() + gap } else { paint.right() + gap }, top), egui::vec2(main_w, h));
    let erasing = app.text_select.erase;
    let active = app.ui.tool_options.text_select_method;
    // How to clean: above, all the same size.
    let chip_h = 40.0;
    let chip_top = top - gap - chip_h;
    let n = METHODS.len() as f32;
    let chip_w = (row_w.max(area.width() - 16.0).min(area.width() - 16.0) - (n - 1.0) * gap) / n;
    let chips_x = area.center().x - (chip_w * n + (n - 1.0) * gap) / 2.0;
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
    for (i, name) in ["textSelectMethod:ai", "textSelectMethod:fill", "textSelectMethod:bubble", "textSelectMethod:color"].into_iter().enumerate() {
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
        Look { doc, revision: 0, marks: 0, by_suggestion, active: CleanMethod::Ai, on: [Color32::BLUE, Color32::GREEN, Color32::LIGHT_BLUE, Color32::YELLOW], off: Color32::RED }
    }

    fn covered(app: &PhotocraftApp, x: i32, y: i32) -> bool {
        app.session.active().unwrap().doc.selection.as_ref().is_some_and(|s| s.sample_channel(x, y, 0) >= 0.5)
    }

    /// A method picked in the bar.
    fn pick(app: &mut PhotocraftApp, m: CleanMethod) {
        act(app, &egui::Context::default(), Act::Method(m));
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
        pick(&mut app, CleanMethod::Fill);
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
        let id = piece_at(&app.text_select.docs[&doc].detection, 50.0, 50.0, 2).unwrap() as usize - 1;
        let chosen = &app.text_select.docs[&doc].chosen;
        assert!(!chosen[id] && chosen[1 - id], "a single piece");
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
        pick(&mut app, CleanMethod::Color);
        click(&mut app, 30.0, 30.0, egui::Modifiers::NONE);
        pick(&mut app, CleanMethod::Fill);
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
    fn a_block_cleaned_on_its_own_leaves_the_others_chosen() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        assert!(open_menu(&mut app, Tool::TextSelection, [30.0, 30.0], [5.0, 5.0]));
        assert!(!open_menu(&mut app, Tool::Lasso, [30.0, 30.0], [5.0, 5.0]));
        assert!(!open_menu(&mut app, Tool::TextSelection, [150.0, 90.0], [5.0, 5.0]), "on nothing");
        let id = piece_at(&app.text_select.docs[&doc].detection, 30.0, 30.0, 4).unwrap();
        clean_block(&mut app, doc, &[id], CleanMethod::Color);
        for _ in 0..3 {
            sync(&mut app);
        }
        let px = photocraft_compose::render(&app.session.active().unwrap().doc, Rect::from_xywh(30, 30, 1, 1)).px[0][0];
        assert!(px > 0.9, "clean: {px}");
        assert_eq!(app.text_select.docs[&doc].chosen, vec![true], "the other is still chosen");
        assert!(covered(&app, 210, 130));
    }

    #[test]
    fn a_block_can_have_its_own_contour_and_its_own_layer() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        let id = piece_at(&app.text_select.docs[&doc].detection, 30.0, 30.0, 4).unwrap();
        let base = app.text_select.docs[&doc].applied;
        {
            let p = app.text_select.docs.get_mut(&doc).unwrap();
            p.contour[id as usize - 1] = Some(base + 8.0);
            p.output[id as usize - 1] = Some("new".to_string());
        }
        select_pieces(&mut app, doc, &[1, 2], "replace", None);
        assert!(covered(&app, 20 - base as i32 - 6, 30), "this one reaches further");
        assert!(!covered(&app, 200 - base as i32 - 6, 130), "the other doesn't");
        // A new contour for the rest leaves it alone.
        set_grow(&mut app, doc, base + 1.0, None);
        assert!(covered(&app, 20 - base as i32 - 6, 30));
        let layers = app.session.active().unwrap().doc.layers.len();
        pick(&mut app, CleanMethod::Color);
        act(&mut app, &egui::Context::default(), Act::All);
        {
            let p = app.text_select.docs.get_mut(&doc).unwrap();
            p.output[id as usize - 1] = Some("new".to_string());
        }
        sync(&mut app);
        act(&mut app, &egui::Context::default(), Act::Clean);
        for _ in 0..4 {
            sync(&mut app);
        }
        assert_eq!(app.session.active().unwrap().doc.layers.len(), layers + 1, "one went to its own layer");
    }

    #[test]
    fn with_one_method_the_selection_goes_as_it_is() {
        let mut app = page();
        click(&mut app, 150.0, 90.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        pick(&mut app, CleanMethod::Color);
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

    /// Finds what is pure black (the lettering; outlines and screentone are grey).
    struct Black;
    impl TextMasker for Black {
        fn model_state(&self) -> ModelState {
            ModelState::Ready
        }
        fn device(&self) -> String {
            "test".into()
        }
        fn detect(&self, page: Page<'_>, _: &dyn Fn() -> bool) -> Result<Detection, photocraft_ai_textmask::Error> {
            let prob: Vec<f32> = page.rgb.as_chunks::<3>().0.iter().map(|c| if c[0] < 10 { 0.995 } else { 0.0 }).collect();
            Ok(find_regions(&prob, page.width, page.height))
        }
    }

    /// A speech bubble with two lines of words, and an effect on screentone below: the tool on,
    /// the pieces found, AI a fake.
    fn bubble_page() -> (PhotocraftApp, DocId) {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        // A bubble is a few hundredths of a page: the page around the art is large.
        app.session.execute("file.new", json!({"width": 1200, "height": 1600, "background": "white"})).unwrap();
        app.session.set_text_masker(Some(Arc::new(Black)));
        app.session.set_inpainter(Some(Arc::new(photocraft_ai_lama::service::FakeInpainter::default())));
        app.session
            .edit("art", |doc, active| {
                let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
                let grey = |v: f32| [v, v, v, 1.0];
                for y in (450..700).step_by(6) {
                    for x in (0..600).step_by(6) {
                        surf.fill_rect(Rect::new(x, y, x + 3, y + 3), &grey(0.45));
                    }
                }
                for (r, v) in [(0, 0.2), (5, 1.0)] {
                    let (cx, cy, rx, ry) = (250, 180, 150 - r, 100 - r);
                    for y in -ry..=ry {
                        let dx = (f64::from(rx) * (1.0 - f64::from(y * y) / f64::from(ry * ry)).max(0.0).sqrt()).round() as i32;
                        surf.fill_rect(Rect::new(cx - dx, cy + y, cx + dx + 1, cy + y + 1), &grey(v));
                    }
                }
                for (x0, y0) in [(180, 150), (180, 195)] {
                    for k in 0..6 {
                        surf.fill_rect(Rect::new(x0 + k * 24, y0, x0 + k * 24 + 14, y0 + 24), &grey(0.0));
                    }
                }
                for k in 0..4 {
                    surf.fill_rect(Rect::new(120 + k * 60, 520, 160 + k * 60, 620), &grey(0.0));
                }
                Ok(())
            })
            .unwrap();
        app.ui.tool = Tool::TextSelection;
        click(&mut app, 500.0, 400.0, egui::Modifiers::NONE);
        sync(&mut app);
        let doc = app.session.active().unwrap().doc.id;
        (app, doc)
    }

    fn piece(app: &PhotocraftApp, doc: DocId, x: f64, y: f64) -> u32 {
        piece_at(&app.text_select.docs[&doc].detection, x, y, 0).unwrap_or_else(|| panic!("a piece at {x},{y}"))
    }

    fn method_of(app: &PhotocraftApp, doc: DocId, x: f64, y: f64) -> CleanMethod {
        app.text_select.docs[&doc].method[piece(app, doc, x, y) as usize - 1]
    }

    fn ink(app: &PhotocraftApp, x: i32, y: i32) -> f32 {
        photocraft_compose::render(&app.session.active().unwrap().doc, Rect::from_xywh(x, y, 1, 1)).px[0][0]
    }

    /// Each piece starts with its own method: the bubble's words the Bubble Text Remover's, the
    /// effect on screentone AI's; a bubble's pieces are one block, and all are suggested.
    #[test]
    fn pieces_in_a_plain_bubble_start_as_bubble_and_the_effect_as_ai() {
        let (app, doc) = bubble_page();
        assert_eq!(method_of(&app, doc, 185.0, 160.0), CleanMethod::Bubble);
        assert_eq!(method_of(&app, doc, 185.0, 205.0), CleanMethod::Bubble);
        assert_eq!(method_of(&app, doc, 130.0, 560.0), CleanMethod::Ai);
        let p = &app.text_select.docs[&doc];
        assert_eq!(block_of(p, piece(&app, doc, 185.0, 160.0)).len(), 12, "both lines: one bubble, one block");
        assert!(p.chosen.iter().all(|c| *c), "all suggested");
    }

    /// Clean: the bubble without AI, the effect with it, as one history step; Undo brings the
    /// pieces and their methods back as they were, Redo cleans again.
    #[test]
    fn a_clean_is_one_step_and_undo_brings_the_pieces_back() {
        let (mut app, doc) = bubble_page();
        let ctx = egui::Context::default();
        // A change of the effect's method, to see it come back.
        let fx = piece(&app, doc, 130.0, 560.0);
        let block = block_of(&app.text_select.docs[&doc], fx);
        if let Some(p) = app.text_select.docs.get_mut(&doc) {
            for id in &block {
                p.method[*id as usize - 1] = CleanMethod::Fill;
            }
        }
        tool_step(&mut app);
        sync(&mut app);
        let past = app.session.active().unwrap().history.past_len();
        let before = app.text_select.docs[&doc].clone();
        act(&mut app, &ctx, Act::Clean);
        for _ in 0..12 {
            sync(&mut app);
        }
        assert!(ink(&app, 185, 160) > 0.9 && ink(&app, 185, 205) > 0.9, "the bubble is clean");
        assert!(ink(&app, 130, 560) > 0.3, "and the effect");
        assert!(app.text_select.run_key.is_none(), "the run ended");
        assert_eq!(app.session.active().unwrap().history.past_len(), past + 1, "one step: {:?}", app.session.active().unwrap().history.entries());
        app.session.execute("edit.undo", json!({})).unwrap();
        sync(&mut app);
        assert!(ink(&app, 185, 160) < 0.1, "the letters are back");
        let p = &app.text_select.docs[&doc];
        assert!(Arc::ptr_eq(&p.detection, &before.detection), "the same pieces, no new detection");
        assert_eq!(p.method, before.method, "with their methods");
        assert_eq!(p.chosen, before.chosen);
        app.session.execute("edit.redo", json!({})).unwrap();
        sync(&mut app);
        assert!(ink(&app, 185, 160) > 0.9);
        assert!(app.session.text_detection().is_none_or(|d| d.regions.is_empty()), "nothing left after it");
    }

    /// A Clean of several pieces with AI on a new layer is one history step too: one Undo takes
    /// the clean and its layer away, one Redo brings both back.
    #[test]
    fn one_undo_takes_back_an_ai_clean_on_a_new_layer() {
        let (mut app, doc) = bubble_page();
        let ctx = egui::Context::default();
        app.ui.tool_options.clean_output = "new".into();
        if let Some(p) = app.text_select.docs.get_mut(&doc) {
            p.method.fill(CleanMethod::Ai);
        }
        tool_step(&mut app);
        sync(&mut app);
        let st = app.session.active().unwrap();
        let (layers, past) = (st.doc.walk().len(), st.history.past_len());
        assert!(app.text_select.docs[&doc].chosen.iter().filter(|c| **c).count() > 1, "several pieces");
        act(&mut app, &ctx, Act::Clean);
        for _ in 0..12 {
            sync(&mut app);
        }
        assert!(app.text_select.run_key.is_none(), "the run ended");
        assert!(ink(&app, 185, 160) > 0.9, "the bubble is clean");
        let st = app.session.active().unwrap();
        assert_eq!(st.doc.walk().len(), layers + 1, "on a layer of its own");
        assert_eq!(st.history.past_len(), past + 1, "one step: {:?}", st.history.entries());
        app.session.execute("edit.undo", json!({})).unwrap();
        sync(&mut app);
        assert_eq!(app.session.active().unwrap().doc.walk().len(), layers, "the clean's layer went with it");
        assert!(ink(&app, 185, 160) < 0.1, "the letters are back");
        app.session.execute("edit.redo", json!({})).unwrap();
        sync(&mut app);
        assert_eq!(app.session.active().unwrap().doc.walk().len(), layers + 1);
        assert!(ink(&app, 185, 160) > 0.9);
    }

    /// A bubble the Bubble Text Remover refuses goes to AI, and the status says so.
    #[test]
    fn a_refused_bubble_goes_to_ai() {
        let (mut app, doc) = bubble_page();
        let ctx = egui::Context::default();
        let fx = piece(&app, doc, 130.0, 560.0);
        let p = app.text_select.docs.get(&doc).unwrap();
        let block = block_of(p, fx);
        // Only the effect, marked for the Bubble Text Remover: no bubble there.
        select_pieces(&mut app, doc, &block, "replace", None);
        if let Some(p) = app.text_select.docs.get_mut(&doc) {
            for id in &block {
                p.method[*id as usize - 1] = CleanMethod::Bubble;
            }
        }
        sync(&mut app);
        act(&mut app, &ctx, Act::Clean);
        for _ in 0..12 {
            sync(&mut app);
        }
        assert!(ink(&app, 130, 560) > 0.3, "cleaned by AI");
        assert!(!app.ui.status_error && app.ui.status.contains(&block.len().to_string()), "said: {}", app.ui.status);
        assert!(ink(&app, 185, 160) < 0.1, "the bubble wasn't in it");
    }

    /// Every change in the tool is a step: a method, a split, a zone; Undo and Redo go through them.
    #[test]
    fn method_split_and_zone_undo_and_redo() {
        let (mut app, doc) = bubble_page();
        let ctx = egui::Context::default();
        let word = piece(&app, doc, 185.0, 160.0);
        // A method for the bubble's block, from its menu.
        assert!(open_menu(&mut app, Tool::TextSelection, [185.0, 160.0], [5.0, 5.0]));
        let ids = block_of(&app.text_select.docs[&doc], word);
        if let Some(p) = app.text_select.docs.get_mut(&doc) {
            for id in &ids {
                p.method[*id as usize - 1] = CleanMethod::Color;
            }
        }
        tool_step(&mut app);
        sync(&mut app);
        // Split it.
        if let Some(p) = app.text_select.docs.get_mut(&doc) {
            for id in &ids {
                p.alone[*id as usize - 1] = true;
            }
        }
        tool_step(&mut app);
        sync(&mut app);
        assert_eq!(block_of(&app.text_select.docs[&doc], word), vec![word]);
        // A zone.
        app.ui.tool_options.text_select_shape = ZoneShape::Rect;
        finish_stroke(&mut app, &[[400.0, 300.0, 1.0], [450.0, 340.0, 1.0]], egui::Modifiers::NONE);
        sync(&mut app);
        let n = app.text_select.docs[&doc].detection.regions.len();
        let undo = |app: &mut PhotocraftApp| {
            app.session.execute("edit.undo", json!({})).unwrap();
            sync(app);
        };
        undo(&mut app);
        assert_eq!(app.text_select.docs[&doc].detection.regions.len(), n - 1, "the zone left");
        undo(&mut app);
        assert_eq!(block_of(&app.text_select.docs[&doc], word).len(), 12, "joined again");
        assert_eq!(method_of(&app, doc, 185.0, 160.0), CleanMethod::Color);
        undo(&mut app);
        assert_eq!(method_of(&app, doc, 185.0, 160.0), CleanMethod::Bubble, "its own method again");
        for _ in 0..3 {
            app.session.execute("edit.redo", json!({})).unwrap();
            sync(&mut app);
        }
        assert_eq!(app.text_select.docs[&doc].detection.regions.len(), n, "the zone is back");
        assert_eq!(method_of(&app, doc, 185.0, 160.0), CleanMethod::Color);
        assert_eq!(block_of(&app.text_select.docs[&doc], word), vec![word]);
        let _ = ctx;
    }

    /// Finding the text again on the same pixels keeps what was changed of the pieces; on changed
    /// pixels it starts over.
    #[test]
    fn finding_again_on_the_same_pixels_keeps_the_corrections() {
        let (mut app, doc) = bubble_page();
        let ctx = egui::Context::default();
        let fx = piece(&app, doc, 130.0, 560.0);
        let block = block_of(&app.text_select.docs[&doc], fx);
        if let Some(p) = app.text_select.docs.get_mut(&doc) {
            for id in &block {
                p.method[*id as usize - 1] = CleanMethod::Fill;
            }
        }
        let word = piece(&app, doc, 185.0, 160.0);
        mark(&mut app, doc, &[word], Mark::Remove);
        sync(&mut app);
        act(&mut app, &ctx, Act::FindAgain);
        sync(&mut app);
        sync(&mut app);
        assert_eq!(method_of(&app, doc, 130.0, 560.0), CleanMethod::Fill, "the method stayed");
        let word = piece(&app, doc, 185.0, 160.0);
        assert!(!app.text_select.docs[&doc].chosen[word as usize - 1], "and what was taken off");
        // Done, then changed pixels: as found.
        act(&mut app, &ctx, Act::Done);
        sync(&mut app);
        app.session.execute("select.rect", json!({"x": 0, "y": 0, "width": 5, "height": 5})).unwrap();
        app.session.execute("edit.fill", json!({"color": "#808080"})).unwrap();
        app.session.execute("select.deselect", json!({})).unwrap();
        act(&mut app, &ctx, Act::Find);
        sync(&mut app);
        sync(&mut app);
        assert_eq!(method_of(&app, doc, 130.0, 560.0), CleanMethod::Ai);
    }
}
