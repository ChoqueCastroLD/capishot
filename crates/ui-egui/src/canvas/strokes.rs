use super::drag::Drag;
use super::view_xform::ViewXform;
use super::stroke_job::{EngineStroke, Finished, StrokeJob, Wait};
use super::{Color32, DRect, PhotocraftApp, Pos2, Stroke, Tool, json};

pub(crate) use crate::clone_overlay::ClonePreviewCache;

/// A transient source image beneath the brush outline; never part of the document render. The
/// Clone Source panel drives the Clone Stamp and the Healing Brush alike, so both show it.
pub(super) fn draw_clone_preview(app: &mut PhotocraftApp, painter: &egui::Painter, xf: &ViewXform, at: [f64; 2], output: Option<u32>) {
    crate::clone_overlay::draw(app, painter, xf, at, output);
}

/// A stroke shown while it is drawn: the engine renders the real result onto a copy of the
/// document (off the window's thread: `StrokeJob`), and the canvas redraws only what each step
/// changed.
pub(crate) struct LiveStroke {
    /// The stroke can't be shown any further (the Mixer Brush's keeps its last preview; its
    /// release replays the input without the preview's tile budget).
    pub(super) preview_failed: bool,
    pub(super) stroke: StrokeJob,
    pub(super) doc: photocraft_doc::DocId,
    revision: u64,
    /// Preview key of the stroke; step `n` displays as `key + n`.
    pub(super) key: u64,
    /// Damage of each step (step 0 = the document before the stroke).
    pub(super) damage: Vec<DRect>,
    /// Drag points rendered so far.
    fed: usize,
}

impl LiveStroke {
    pub(super) fn display_key(&self) -> u64 {
        self.key + self.damage.len() as u64
    }

    /// What changed since the canvas showed preview `key` (0 = the document before the stroke).
    pub(super) fn since(&self, key: u64) -> Option<DRect> {
        let seen = if key == 0 { 0 } else { usize::try_from(key.checked_sub(self.key)?).ok()? };
        Some(self.damage.get(seen..)?.iter().fold(DRect::EMPTY, |a, r| a.union(r)))
    }

    /// Hand `pts` over to the stroke's job, and take what it has rendered already (all of it
    /// where strokes are rendered in the call).
    fn push(&mut self, pts: &[photocraft_engine::paint::StrokePoint]) -> Result<(), String> {
        self.stroke.push(pts);
        self.land(Wait::No)
    }

    /// The steps the stroke's job finished become steps of the stroke shown. `Err`: the stroke
    /// can't be shown any further, and why (its commit renders it).
    pub(super) fn land(&mut self, wait: Wait) -> Result<(), String> {
        self.damage.extend(self.stroke.land(wait)?);
        Ok(())
    }

    /// Is the stroke's job still rendering points of the stroke?
    pub(super) fn in_flight(&self) -> bool {
        self.stroke.in_flight()
    }

    /// Everything the stroke has drawn so far.
    fn drawn(&self) -> DRect {
        self.damage.iter().fold(DRect::EMPTY, |a, r| a.union(r))
    }
}

/// How a released stroke is committed.
enum Commit {
    /// The Brush's, Pencil's or Eraser's command with its params, taking the pixels of the job.
    Brush { cmd: &'static str, params: serde_json::Value },
    /// A retouching tool's command with its params (`retouch_ui::commit`), taking the pixels of
    /// the job.
    Retouch { tool: Tool, cmd: &'static str, params: serde_json::Value },
    /// `paint.mixerBrush`, taking the pixels of the job; `params` replay the input where the
    /// preview stopped (its tile budget).
    Mixer { params: serde_json::Value },
    /// Its command renders the stroke itself as a background job (the Healing Brush blends the
    /// whole stroke on release): the canvas shows the stroke as it was drawn until that lands.
    Job(photocraft_engine::jobs::JobId),
}

/// A stroke that was released while its job was still rendering, or whose command is a
/// background job: the canvas keeps showing it as it lands, and it is committed when it is
/// whole (`land_stroke`).
pub(crate) struct LandingStroke {
    stroke: LiveStroke,
    commit: Commit,
}

/// A live stroke dropped without committing: document `doc` at `revision` is what it was before
/// the stroke, so a cache that showed any step of it misses only where the stroke drew. Without
/// this the switch back to the document redrew all of it: on a phone, the first finger of every
/// two-finger gesture starts a stroke that the second drops, and the full redraw stalled the
/// GPU right when the view should sharpen.
pub(crate) struct DroppedStroke {
    pub(crate) doc: photocraft_doc::DocId,
    pub(crate) revision: u64,
    key: u64,
    steps: u64,
    drawn: DRect,
}

impl DroppedStroke {
    pub(crate) fn of(l: &LiveStroke) -> Self {
        Self { doc: l.doc, revision: l.revision, key: l.key, steps: l.damage.len() as u64, drawn: l.drawn() }
    }

    /// What a cache that showed raw preview `key` of the stroke misses now (`None`: it showed
    /// something else).
    pub(crate) fn since(&self, key: u64) -> Option<DRect> {
        (key >= self.key && key <= self.key + self.steps).then_some(self.drawn)
    }
}

/// A live stroke whose pixels its commit took over (`"fromLive"`): document `doc` at `revision`
/// holds exactly the live stroke's last step, so a cache that showed step `n` of it only misses
/// the steps after `n` (step 0 = the document before the stroke: everything).
pub(crate) struct CommittedStroke {
    pub(crate) doc: photocraft_doc::DocId,
    pub(crate) revision: u64,
    key: u64,
    damage: Vec<DRect>,
    /// What the commit repaints beyond the stroke whatever a cache showed: the layer's whole
    /// composite when effects laid out over its frame moved with it (a Gradient Overlay spans the
    /// grown layer, #4053; `photocraft_compose::relayout_damage`).
    pub(super) relayout: DRect,
}

impl CommittedStroke {
    #[cfg(test)]
    pub(crate) fn for_test(key: u64, damage: Vec<DRect>) -> Self {
        Self { doc: photocraft_doc::DocId(0), revision: 0, key, damage, relayout: DRect::EMPTY }
    }

    /// The raw preview key of the live stroke's last step: what a cache showed at release.
    #[cfg(test)]
    pub(crate) fn last_key(&self) -> u64 {
        self.key + self.damage.len() as u64
    }

    /// What a cache that showed raw preview `key` of this stroke at the revision before misses.
    pub(crate) fn since(&self, key: u64) -> Option<DRect> {
        let seen = if key == 0 { 0 } else { usize::try_from(key.checked_sub(self.key)?).ok()? };
        Some(self.damage.get(seen..)?.iter().fold(DRect::EMPTY, |a, r| a.union(r)))
    }
}

/// The live stroke on document `idx`, while it is current.
pub(super) fn live_stroke(app: &PhotocraftApp, idx: usize) -> Option<&LiveStroke> {
    let st = app.session.documents().get(idx)?;
    let drawn = app.live_stroke.as_ref().filter(|_| app.drag.is_some());
    drawn.or(app.landing_stroke.as_ref().map(|l| &l.stroke)).filter(|l| l.doc == st.doc.id && l.revision == st.revision)
}

/// The document the live stroke on the active document shows (tests compare it with the commit).
#[cfg(test)]
pub(crate) fn live_doc(app: &PhotocraftApp) -> Option<std::sync::Arc<photocraft_doc::Document>> {
    live_stroke(app, app.session.active_index()?).map(|l| l.stroke.doc().clone())
}

/// `paint.stroke` params for a Brush/Eraser drag (shared by the live preview and the commit). The
/// stroke smoothing is the session brush's (the options bar's Smoothing %).
pub(super) fn stroke_params(app: &PhotocraftApp, d: &Drag, points: &[Vec<f64>]) -> serde_json::Value {
    let tool = d.tool;
    let mut p = json!({ "points": points, "erase": d.erase, "zoom": app.point_zoom(), "target": paint_target(app) });
    if d.clear {
        p["mode"] = json!("clear");
    }
    if tool == Tool::Pencil {
        p["autoErase"] = json!(app.ui.tool_options.pencil_auto_erase);
    }
    // Block mode: the engine sizes the square from the stroke's zoom (16 / zoom px, #2770).
    if crate::eraser_ui::block_mode(app, tool) {
        p["block"] = json!(true);
    }
    p
}

/// Tools whose strokes the engine renders while they are drawn (`LiveStroke`).
pub(crate) fn strokes_live(tool: Tool) -> bool {
    matches!(
        tool,
        Tool::Brush
            | Tool::Pencil
            | Tool::MixerBrush
            | Tool::Eraser
            | Tool::ColorReplacement
            | Tool::Blur
            | Tool::Sharpen
            | Tool::Smudge
            | Tool::Dodge
            | Tool::Burn
            | Tool::Sponge
    ) || live_retouch_command(tool).is_some()
}

/// The Spot Healing Brush's stroke while dragging: Photoshop shows the area it will heal as a
/// semi-opaque dark overlay (an image overlay, the same in every UI theme) and heals on release.
pub(crate) const SPOT_HEALING_TRAIL: Color32 = Color32::from_black_alpha(128);

/// The retouching tools drawn live (`LiveRetouch`), by the command their live stroke previews.
/// The Healing Brush heals as it is drawn, as Photoshop CC's does. The Spot Healing Brush isn't
/// here on purpose: like Photoshop's it shows a dark footprint while drawing and heals on release
/// (its fill needs the whole stroke).
pub(super) fn live_retouch_command(tool: Tool) -> Option<&'static str> {
    match tool {
        Tool::CloneStamp => Some("paint.cloneStamp"),
        Tool::Healing => Some("paint.healingBrush"),
        Tool::HistoryBrush => Some("paint.historyBrush"),
        Tool::PatternStamp => Some("paint.patternStamp"),
        Tool::BackgroundEraser => Some("paint.backgroundEraser"),
        _ => None,
    }
}

/// The command a live-stroking tool commits: the Pencil's (and the Pencil- or Block-mode
/// Eraser's) `paint.pencil`, else `paint.stroke`.
pub(crate) fn stroke_command(app: &PhotocraftApp, tool: Tool) -> &'static str {
    if tool == Tool::Pencil || crate::eraser_ui::pencil_mode(app, tool) || crate::eraser_ui::block_mode(app, tool) { "paint.pencil" } else { "paint.stroke" }
}

pub(super) fn begin_live_stroke(app: &PhotocraftApp) -> Option<LiveStroke> {
    use photocraft_engine::retouch_cmds::{LiveDab, LiveRetouch};
    static STROKES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let st = app.session.active()?;
    let d = app.drag.as_ref()?;
    let s = &app.session;
    let dab = |(l, pts): (LiveDab, _)| (EngineStroke::Dab(Box::new(l)), pts);
    let retouch = |(l, pts): (LiveRetouch, _)| (EngineStroke::Retouch(Box::new(l)), pts);
    // The stroke and the points it starts with: none is rendered here, the press never waits
    // for a dab.
    let (stroke, first) = if d.tool == Tool::MixerBrush {
        let points = app.stylus.stroke_points(&d.points);
        let p = stroke_params(app, d, points.get(..1)?);
        let (l, pts) = photocraft_engine::brush_cmds::mixer_live::LiveMixer::start(s, &p).ok()?;
        (EngineStroke::Mixer(Box::new(l)), pts)
    } else if d.tool == Tool::ColorReplacement {
        // The params `retouch_ui::finish_stroke` commits.
        let mut p = crate::retouch_ui::color_replacement_params(app);
        p["points"] = json!(d.points);
        dab(photocraft_engine::brush_cmds::live_color_replacement(s, &p).ok()?)
    } else if let Some((cmd, mut p)) = crate::retouch_ui::dab_params(app, d.tool) {
        // The params `retouch_ui::finish_stroke` commits; Sample All Layers isn't previewed.
        p["points"] = json!(d.points);
        p["target"] = paint_target(app);
        dab(LiveDab::start(s, cmd, &p).ok()?)
    } else if d.tool == Tool::PatternStamp {
        // The params `retouch_ui::finish_stroke` commits.
        let mut p = crate::retouch_ui::pattern_stamp_params(app, &d.points);
        p["points"] = json!(d.points);
        p["target"] = paint_target(app);
        if app.ui.tool_options.pattern_stamp_impressionist {
            dab(photocraft_engine::retouch_cmds::live_pattern_impressionist(s, &p).ok()?)
        } else {
            retouch(LiveRetouch::start(s, "paint.patternStamp", &p).ok()?)
        }
    } else if d.tool == Tool::BackgroundEraser {
        // The params `eraser_ui::finish_stroke` commits (layer pixels only).
        let p = crate::eraser_ui::background_eraser_params(app, &d.points);
        dab(photocraft_engine::eraser_cmds::live_background_eraser(s, &p).ok()?)
    } else if let Some(cmd) = live_retouch_command(d.tool).or_else(|| crate::eraser_ui::erases_to_history(app, d.tool).then_some("paint.historyBrush")) {
        // The params `retouch_ui::finish_stroke` (or `eraser_ui::finish_stroke`) commits.
        let mut p = match d.tool {
            Tool::HistoryBrush => crate::retouch_ui::history_brush_params(app),
            Tool::Eraser => json!({}),
            _ => crate::retouch_ui::clone_params(app)?,
        };
        p["points"] = json!(d.points);
        p["target"] = paint_target(app);
        retouch(LiveRetouch::start(s, cmd, &p).ok()?)
    } else {
        let p = stroke_params(app, d, &app.stylus.stroke_points(&d.points));
        let (l, pts) = photocraft_engine::brush_cmds::LiveStroke::start_with(s, stroke_command(app, d.tool), &p).ok()?;
        (EngineStroke::Brush(Box::new(l)), pts)
    };
    let n = STROKES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) & 0xff_ffff;
    let mut live = LiveStroke {
        stroke: StrokeJob::new(stroke),
        doc: st.doc.id,
        revision: st.revision,
        key: (1 << 44) | (n << 20),
        damage: Vec::new(),
        fed: if d.tool == Tool::MixerBrush { 1 } else { d.points.len() },
        preview_failed: false,
    };
    // The first dab is rendered by the stroke's job, like the rest.
    if !first.is_empty() {
        live.push(&first).ok()?;
    }
    Some(live)
}

/// Cancel uncommitted mixer paint when its document, tool or input context goes away.
pub(crate) fn cancel_mixer_on_context_change(app: &mut PhotocraftApp, ctx: &egui::Context) -> bool {
    if !app.drag.as_ref().is_some_and(|drag| drag.tool == Tool::MixerBrush) {
        return false;
    }
    let changed = app.ui.tool != Tool::MixerBrush
        || app.live_stroke.as_ref().is_some_and(|live| !app.session.active().is_some_and(|st| st.doc.id == live.doc && st.revision == live.revision));
    if changed || ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        app.drag = None;
        app.live_stroke = None;
        return true;
    }
    false
}

/// The drag's points from `from` on, with the pen pressure, tilt, rotation and times the commit's
/// command gets for them (`Stylus::stroke_points`).
fn drag_points(app: &PhotocraftApp, d: &Drag, from: usize) -> Vec<photocraft_engine::paint::StrokePoint> {
    let pose = &app.stylus.stroke;
    // (Times from the drag's first point.)
    let t0 = app.stylus.point_ms(0);
    (from..d.points.len())
        .filter_map(|i| {
            let p = d.points.get(i)?;
            let t = pose.get(i).or(pose.last()).copied().unwrap_or_default();
            let mut sp = photocraft_engine::paint::StrokePoint::new(p[0], p[1], p[2] as f32);
            (sp.tilt_x, sp.tilt_y, sp.rotation) = (t[0], t[1], t[2]);
            sp.time = app.stylus.point_ms(i) - t0;
            Some(sp)
        })
        .collect()
}

/// The stroke being drawn can't be shown any further, and why. The Mixer Brush's keeps its last
/// preview and its document/revision guard: all input stays in the drag, and the release replays
/// it without the preview's tile budget. Any other stroke is shown by its commit.
pub(super) fn preview_failed(app: &mut PhotocraftApp, why: String) {
    if app.drag.as_ref().is_some_and(|d| d.tool == Tool::MixerBrush) {
        if let Some(l) = app.live_stroke.as_mut() {
            l.preview_failed = true;
        }
        app.ui.status = why;
        app.ui.status_error = true;
    } else {
        app.live_stroke = None;
    }
}

/// Hand the live stroke the drag points it hasn't seen yet.
pub(super) fn feed_live_stroke(app: &mut PhotocraftApp) {
    // A batched move replays several samples in one frame and calls this once at the end, so the
    // per-sample calls from `tool_event` are skipped (see `canvas_view`).
    if app.defer_live_stroke {
        return;
    }
    let (Some(l), Some(d)) = (app.live_stroke.as_ref(), app.drag.as_ref()) else { return };
    if l.preview_failed {
        return;
    }
    let pts = drag_points(app, d, l.fed);
    if pts.is_empty() {
        return;
    }
    let fed = d.points.len();
    let started = crate::gpu_canvas::now_ms();
    let pushed = app.live_stroke.as_mut().map(|l| {
        l.fed = fed;
        l.push(&pts)
    });
    app.perf.stage.stroke_ms += crate::gpu_canvas::now_ms() - started;
    if let Some(Err(why)) = pushed {
        preview_failed(app, why);
    }
}

/// The points the live retouching stroke has rendered.
#[cfg(test)]
pub(crate) fn live_retouch_points(app: &PhotocraftApp) -> Option<Vec<photocraft_engine::paint::StrokePoint>> {
    app.live_stroke.as_ref()?.stroke.with_stroke(|s| match s {
        EngineStroke::Retouch(l) => Some(l.points().to_vec()),
        _ => None,
    })?
}

/// A pen tap (a drag still at its one press point when released) paints its dab at the highest
/// pressure the pen reached during the contact, not just the first sample's (#1798). A mouse is
/// unchanged at full pressure.
pub(super) fn tap_at_peak_pressure(app: &mut PhotocraftApp) {
    if app.stylus.sample().is_none() {
        return;
    }
    let peak = f64::from(app.stylus.tap_pressure());
    let mut refresh_mixer = false;
    if let Some(d) = app.drag.as_mut().filter(|d| d.points.len() == 1)
        && let Some(p) = d.points.first_mut()
    {
        refresh_mixer = d.tool == Tool::MixerBrush && peak > p[2];
        p[2] = p[2].max(peak);
    }
    if refresh_mixer {
        app.live_stroke = begin_live_stroke(app);
    }
}

/// While a stroke is held still, its point repeats this often (in ms) so time-based brush
/// features (airbrush build-up, smoothing catch-up) keep going, as when the pen moves.
pub(super) const HOLD_REPEAT_MS: f64 = 16.0;

/// Is the live stroke one that changes while the pointer is held still (airbrush Build-up, or
/// smoothing catching up)? Only then does a held pointer repeat its point and keep repainting.
pub(super) fn stroke_wants_time(app: &PhotocraftApp) -> bool {
    app.live_stroke.as_ref().is_some_and(|l| l.stroke.wants_time())
}

/// Time of sample `k` of the `n` samples a frame delivered, spread evenly after `base` (the
/// previous point) up to `now` (this frame): egui's pointer events carry no timestamps.
pub(crate) fn spread_ms(base: f64, now: f64, k: usize, n: usize) -> f64 {
    // Not after `base` (or not comparable, NaN): every sample takes the frame time.
    if n == 0 || now.partial_cmp(&base) != Some(std::cmp::Ordering::Greater) {
        return now;
    }
    base + (now - base) * (k + 1) as f64 / n as f64
}

/// Tools whose gesture follows a freehand path (a polyline of the input points), so every pointer
/// sample the OS delivered improves the result. Other tools are driven by the pointer's latest
/// position (endpoints, anchors, guides), so feeding them a whole frame's moves only repeats work.
pub(crate) fn freehand_tool(tool: Tool) -> bool {
    matches!(
        tool,
        Tool::Brush
            | Tool::Pencil
            | Tool::MixerBrush
            | Tool::ColorReplacement
            | Tool::Eraser
            | Tool::BackgroundEraser
            | Tool::HistoryBrush
            | Tool::Remove
            | Tool::ArtHistoryBrush
            | Tool::RemoveAi
            | Tool::SpotHealing
            | Tool::Healing
            | Tool::CloneStamp
            | Tool::PatternStamp
            | Tool::Blur
            | Tool::Sharpen
            | Tool::Smudge
            | Tool::Dodge
            | Tool::Burn
            | Tool::Sponge
            | Tool::Lasso
            | Tool::MagneticLasso
            | Tool::Patch
            | Tool::ContentAwareMove
            | Tool::QuickSelection
            | Tool::TextSelection
            | Tool::FreeformPen
    )
}

/// The pointer moves this frame delivered while `button` was held, in order. `down_at_start` is
/// whether the button was already down when the frame began. A move before a press or after a
/// release in the same frame is dropped, so a gesture never picks up input from outside its own
/// press..release interval.
pub(super) fn pointer_moves(events: &[egui::Event], button: egui::PointerButton, down_at_start: bool) -> Vec<Pos2> {
    let mut down = down_at_start;
    let mut out = Vec::new();
    for e in events {
        match e {
            egui::Event::PointerButton { button: b, pressed, .. } if *b == button => down = *pressed,
            egui::Event::PointerMoved(p) if down => out.push(*p),
            _ => {}
        }
    }
    out
}

/// Modifiers attached to the pointer event itself. Some input sources provide these without a
/// separate `ModifiersChanged` event, so use them for click gestures before falling back to the
/// frame-wide modifier state.
pub(super) fn pointer_button_modifiers(events: &[egui::Event], button: egui::PointerButton) -> Option<egui::Modifiers> {
    events.iter().rev().find_map(|e| match e {
        egui::Event::PointerButton { button: b, modifiers, .. } if *b == button => Some(*modifiers),
        _ => None,
    })
}

/// The line from the brush to the pointer while stroke smoothing lags (Pulled String Mode and
/// Stroke Catch-up, #3479): a dark underlay and a light line stay visible on any pixels, and a dot
/// marks the brush end.
pub(super) fn draw_smoothing_string(painter: &egui::Painter, xf: &ViewXform, (brush, pointer): ([f64; 2], [f64; 2])) {
    let (a, b) = (xf.to_screen(brush[0] as f32, brush[1] as f32), xf.to_screen(pointer[0] as f32, pointer[1] as f32));
    if a.distance(b) < 2.0 {
        return;
    }
    painter.line_segment([a, b], Stroke::new(3.0, Color32::from_black_alpha(110)));
    painter.line_segment([a, b], Stroke::new(1.0, Color32::WHITE));
    painter.circle_filled(a, 2.5, Color32::WHITE);
}

/// Commit a Brush, Pencil or Eraser drag. The live stroke already rendered it: its pixels are
/// offered to the commit (`Session::offer_live_commit`), so release costs no second render, and
/// the canvas then refreshes only what it hadn't shown yet (`CommittedStroke`). A stroke whose
/// job is still rendering is committed when it has landed (`LandingStroke`): the release never
/// waits for a large tip's last dabs.
pub(super) fn commit_brush_stroke(app: &mut PhotocraftApp, d: &Drag) {
    let cmd = stroke_command(app, d.tool);
    let mut params = stroke_params(app, d, &app.stylus.stroke_points(&d.points));
    let Some(stroke) = app.live_stroke.take() else {
        let _ = app.run(cmd, params);
        return;
    };
    if let Some(seed) = stroke.stroke.seed {
        params["seed"] = json!(seed);
    }
    release(app, stroke, d, Commit::Brush { cmd, params });
}

/// Commit a Mixer Brush drag as the Brush's is (`commit_brush_stroke`). A tip above the
/// preview's size has no live stroke: its command renders the whole stroke now.
pub(super) fn commit_mixer_stroke(app: &mut PhotocraftApp, d: &Drag) {
    let mut params = stroke_params(app, d, &app.stylus.stroke_points(&d.points));
    let Some(stroke) = app.live_stroke.take() else {
        let _ = app.run("paint.mixerBrush", params);
        return;
    };
    if let Some(seed) = stroke.stroke.seed {
        params["seed"] = json!(seed);
    }
    release(app, stroke, d, Commit::Mixer { params });
}

/// Commit a Clone Stamp, History Brush, Dodge, Burn, Sponge, Blur, Sharpen or Smudge drag whose
/// live stroke draws what its command paints, as the Brush's is (`commit_brush_stroke`): the
/// command `retouch_ui::finish_stroke` would run takes the stroke's pixels instead of rendering
/// it again. `false`: not such a stroke (its command renders it).
pub(super) fn commit_retouch_stroke(app: &mut PhotocraftApp, d: &Drag) -> bool {
    let offered = crate::retouch_ui::dab_params(app, d.tool).is_some() || matches!(d.tool, Tool::CloneStamp | Tool::HistoryBrush);
    if !offered || app.live_stroke.is_none() {
        return false;
    }
    let Some(Some((cmd, params))) = crate::retouch_ui::stroke_command(app, d.tool, &d.points, d.modifiers) else { return false };
    let Some(stroke) = app.live_stroke.take() else { return false };
    release(app, stroke, d, Commit::Retouch { tool: d.tool, cmd, params });
    true
}

/// The drag ended: the stroke gets its last points (the release frame's moves) and is committed
/// once its job has rendered all of it, which this frame waits its share for.
fn release(app: &mut PhotocraftApp, mut stroke: LiveStroke, d: &Drag, commit: Commit) {
    let rest = drag_points(app, d, stroke.fed);
    stroke.fed = d.points.len();
    // (A live stroke that can't show these points: the commit renders the stroke itself.)
    let whole = !stroke.preview_failed && (rest.is_empty() || stroke.push(&rest).is_ok());
    if !whole {
        return commit_landed(app, stroke, commit, None);
    }
    stroke.stroke.finish();
    app.landing_stroke = Some(Box::new(LandingStroke { stroke, commit }));
    land_stroke(app, Wait::Frame);
}

/// A drag ended whose command renders the stroke itself (the Healing Brush's blend of the whole
/// stroke, the Pattern Stamp, Color Replacement, the Background Eraser): `stroke` is what the
/// canvas showed of it, on the document at `revision`. Run now, the command's result replaces it,
/// and the canvas repaints where either drew. Started as a background job (`job`), the canvas
/// keeps showing the stroke as it was drawn until the job lands (`on_job_event`).
pub(super) fn rendered_by_command(app: &mut PhotocraftApp, mut stroke: LiveStroke, d: &Drag, job: Option<photocraft_engine::jobs::JobId>) {
    let Some(job) = job else { return replaced(app, stroke) };
    // (The stroke goes on landing what it was handed; it shows the release frame's moves too.)
    let rest = drag_points(app, d, stroke.fed);
    stroke.fed = d.points.len();
    if !rest.is_empty() && !stroke.preview_failed {
        stroke.stroke.push(&rest);
    }
    app.landing_stroke = Some(Box::new(LandingStroke { stroke, commit: Commit::Job(job) }));
}

/// The command that commits `stroke` rendered it again and made the document's next revision:
/// a cache that showed any step of the stroke repaints where the stroke drew and where the
/// command did, not everything. (After anything else the caches find no step of theirs and
/// repaint whole.)
fn replaced(app: &mut PhotocraftApp, stroke: LiveStroke) {
    let drawn = stroke.drawn();
    let LiveStroke { doc, revision, key, damage, .. } = stroke;
    let Some(st) = app.session.documents().iter().find(|st| st.doc.id == doc) else { return };
    if st.revision != revision + 1 {
        return;
    }
    let Some(changed) = st.damage_since(revision) else { return };
    let relayout = if changed.is_empty() { drawn } else { changed.union(&drawn) };
    app.committed_stroke = Some(CommittedStroke { doc, revision: st.revision, key, damage, relayout });
}

/// The job of a command that renders a released stroke itself ended (`rendered_by_command`): what
/// it made replaces the stroke the canvas showed; a job that failed or was cancelled leaves the
/// document as it was before the stroke.
pub(crate) fn on_job_event(app: &mut PhotocraftApp, e: &photocraft_engine::jobs::JobEvent) {
    let Some(landing) = app.landing_stroke.take_if(|l| matches!(l.commit, Commit::Job(id) if id == e.id)) else { return };
    if matches!(e.outcome, photocraft_engine::jobs::JobOutcome::Done(_)) {
        replaced(app, landing.stroke);
    } else {
        app.dropped_stroke = Some(DroppedStroke::of(&landing.stroke));
    }
}

/// The released stroke's job is done (waiting for it as `wait` says): commit it. Every frame
/// looks, and so does anything about to edit the document (`Wait::All`: the stroke comes first).
fn land_stroke(app: &mut PhotocraftApp, wait: Wait) {
    let Some(landing) = app.landing_stroke.as_mut() else { return };
    // A stroke its command's job renders ends with that job (`on_job_event`), which what edits
    // the document waits for (`jobs_ui::land_first`): until then it shows what lands of it.
    if matches!(landing.commit, Commit::Job(_)) {
        if !landing.stroke.preview_failed && landing.stroke.land(Wait::No).is_err() {
            landing.stroke.preview_failed = true;
        }
        return;
    }
    let started = crate::gpu_canvas::now_ms();
    let landed = landing.stroke.land(wait);
    app.perf.stage.stroke_ms += crate::gpu_canvas::now_ms() - started;
    let finished = match landed {
        Ok(()) => match landing.stroke.stroke.take_finished() {
            Some(f) => Some(f),
            None => return,
        },
        // It can't be shown whole: the commit renders the stroke itself.
        Err(_) => None,
    };
    if let Some(landing) = app.landing_stroke.take() {
        let LandingStroke { stroke, commit } = *landing;
        commit_landed(app, stroke, commit, finished);
    }
}

/// A frame begins: commit the released stroke if its job is done, and come back for it (or for
/// the points of the stroke being drawn) if not.
pub(crate) fn land_stroke_in_frame(app: &mut PhotocraftApp, ctx: &egui::Context) {
    land_stroke(app, Wait::No);
    let landing = app.landing_stroke.as_ref().is_some_and(|l| !matches!(l.commit, Commit::Job(_)) || l.stroke.in_flight());
    if landing || app.live_stroke.as_ref().is_some_and(LiveStroke::in_flight) {
        ctx.request_repaint();
    }
}

/// Something is about to edit the document: the released stroke is committed first, in the
/// order the user did things, waiting for the rest of it.
pub(crate) fn land_stroke_before_edit(app: &mut PhotocraftApp) {
    land_stroke(app, Wait::All);
}

/// Run the command that commits `stroke`, taking the pixels of `finished` (the live stroke, all
/// of it rendered) when there are any.
fn commit_landed(app: &mut PhotocraftApp, stroke: LiveStroke, commit: Commit, finished: Option<Finished>) {
    let before = app.session.documents().iter().find(|st| st.doc.id == stroke.doc).map(|st| st.doc.clone());
    let started = crate::gpu_canvas::now_ms();
    // A refused offer (the brush changed mid-stroke, say) leaves the command to render the stroke.
    let result = match (commit, finished) {
        (Commit::Brush { cmd, params }, finished) => {
            if let Some(Finished::Brush(c)) = finished {
                app.session.offer_live_commit(*c);
            }
            app.run(cmd, params)
        }
        (Commit::Retouch { tool, cmd, params }, finished) => {
            let _ = match finished {
                Some(Finished::Retouch(l)) => app.session.offer_live_retouch(*l),
                Some(Finished::Dab(l)) => app.session.offer_live_dab(*l),
                _ => Ok(()),
            };
            crate::retouch_ui::commit(app, tool, cmd, params)
        }
        (Commit::Mixer { .. }, Some(Finished::Mixer(mixer))) => match mixer.prepare_commit(&mut app.session) {
            Ok(params) => app.run("paint.mixerBrush", params),
            Err(e) => {
                app.ui.status = e.to_string();
                app.ui.status_error = true;
                Err(e.to_string())
            }
        },
        (Commit::Mixer { params }, _) => app.run("paint.mixerBrush", params),
        // (Committed by its job: `on_job_event`.)
        (Commit::Job(_), _) => return,
    };
    app.perf.stage.stroke_ms += crate::gpu_canvas::now_ms() - started;
    app.session.clear_live_stroke();
    let Ok(r) = result else { return };
    if r.get("fromLive").and_then(serde_json::Value::as_bool) != Some(true) {
        // Rendered again from scratch.
        return replaced(app, stroke);
    }
    let LiveStroke { doc, key, damage, .. } = stroke;
    let after = app.session.documents().iter().find(|st| st.doc.id == doc).map(|st| (st.revision, st.doc.clone()));
    if let Some((revision, after)) = after {
        let relayout = before.map_or(DRect::EMPTY, |b| photocraft_compose::relayout_damage(&b, &after));
        app.committed_stroke = Some(CommittedStroke { doc, revision, key, damage, relayout });
    }
}

/// Pencil (and a Pencil-mode Eraser) dabs sit on the pixel grid. Clicks anywhere inside one
/// pixel must produce the same point, or a ⇧-click line changes with the sub-pixel position.
pub(super) fn snap_aliased_tip(app: &PhotocraftApp, tool: Tool, x: f64, y: f64) -> (f64, f64) {
    let pencil = tool == Tool::Pencil || crate::eraser_ui::pencil_mode(app, tool);
    if !pencil {
        return (x, y);
    }
    let (cx, cy) = photocraft_engine::paint::grid_center(x, y, app.session.tools.brush.size);
    (f64::from(cx), f64::from(cy))
}

/// "mask" when the Layers panel targets (or the canvas shows) the active layer's mask, or the layer
/// is an adjustment layer with one (it has no pixels: #4298), else "pixels".
pub fn paint_target(app: &PhotocraftApp) -> serde_json::Value {
    use photocraft_engine::channel_cmds::ChannelTarget;
    let Some(st) = app.session.active() else { return json!("pixels") };
    // A targeted alpha channel (Channels panel) or Quick Mask mode wins over the layer.
    match st.channel_view.target {
        ChannelTarget::Alpha(i) if i < st.doc.channels.len() => return json!({ "channel": i }),
        ChannelTarget::Composite if st.doc.quick_mask.is_some() => return json!("quickMask"),
        ChannelTarget::LayerMask => return json!("mask"),
        _ => {}
    }
    // A smart object's targeted (or shown) filter mask takes the paint (FILE-215-3).
    if app.filter_mask_targeted() {
        return json!("filterMask");
    }
    let has_mask = st.active_layer.and_then(|id| st.doc.layer(id)).is_some_and(|l| l.mask.is_some());
    // Viewing the mask (⌥-click its thumbnail, #196) paints the mask.
    let viewing = photocraft_engine::mask_view_cmds::current(st).is_some_and(|v| !v.filter);
    let mask = app.ui.mask_target || viewing || crate::adjustment_layer_active(st);
    json!(if mask && has_mask { "mask" } else { "pixels" })
}
