//! The AI progress card: while an AI tool runs (Remove with AI, Clean with AI, Upscale with AI,
//! Generative Fill, the Remove tool's AI mode) or its model downloads, a card floats near the top
//! (under the simple interface's bars; bottom right on the desktop) with the tool, the stage in
//! plain words ("Downloading model 45% · 12 MB/s", "Preparing AI…", "Processing 3/8",
//! "Applying…"), a bar (sliding while the share isn't known), the time so far and Cancel. The
//! area being worked on pulses on the canvas and the Capi button pulses. A finished run says
//! "Done" for a moment (and taps the phone's vibration); a failed one says why, with Retry.
//!
//! The stages are read from the job's progress messages (the engine's and the AI worker's,
//! English and language-neutral, see [`stage`]).

use egui::{Align2, Color32, CornerRadius, Id, Rect, RichText, Sense, Stroke, pos2, vec2};
use photocraft_doc::DocId;
use photocraft_engine::jobs::{JobEvent, JobId, JobOutcome};
use serde_json::Value;

use crate::PhotocraftApp;
use crate::theme::Tokens;

/// The commands the card follows.
pub const AI_COMMANDS: [&str; 5] = [
    photocraft_engine::remove_ai_cmds::ID,
    photocraft_engine::ai_inpaint_cmds::ID,
    photocraft_engine::ai_cmds::COMMAND,
    photocraft_engine::genfill_cmds::ID,
    "edit.generativeFillVariant",
];

/// How long "Done" stays (seconds).
const DONE_FOR: f64 = 2.2;

/// Whether running `id` with `params` is an AI run (the Remove tool's AI mode included).
pub fn is_ai(id: &str, params: &Value) -> bool {
    AI_COMMANDS.contains(&id) || (id == "paint.remove" && params.get("ai").and_then(Value::as_bool) == Some(true))
}

/// The card's state (on [`PhotocraftApp`]).
#[derive(Default)]
pub struct State {
    /// The AI run last started, for Retry.
    last: Option<(String, Value)>,
    /// The run being followed.
    tracked: Option<Tracked>,
    /// How the last run ended, while it is shown.
    outcome: Option<Outcome>,
}

struct Tracked {
    job: JobId,
    /// The localized tool name.
    title: String,
    /// When the card first saw it (egui time, seconds).
    started: Option<f64>,
    /// The GPU couldn't run it: the CPU does now.
    cpu: bool,
    /// What it works on, in document pixels, and the document.
    region: Option<(DocId, photocraft_geom::Rect)>,
}

enum Outcome {
    Done { title: String, at: f64 },
    Failed { title: String, error: String, retry: Option<(String, Value)> },
}

/// What the run is doing, read from its progress message.
#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    /// The model downloads: the share and the speed (bytes a second), when known.
    Download { fraction: Option<f32>, speed: Option<f64> },
    /// Loading the model, starting the GPU.
    Preparing,
    /// The GPU's first run compiles its shaders.
    Compiling,
    /// Working: step `i` of `n`, when there are several.
    Processing { step: Option<(usize, usize)> },
    /// The result is landing.
    Applying,
}

/// The stage `message` (the job's, English) names at `fraction` done.
pub fn stage(message: &str, fraction: f32) -> Stage {
    let m = message.trim();
    if let Some(rest) = m.strip_prefix("Downloading") {
        let fraction = percent_in(rest).map(|p| p / 100.0);
        let speed = rest.split(" at ").nth(1).and_then(|s| s.trim().strip_suffix("MB/s")).and_then(|s| s.trim().parse::<f64>().ok()).map(|mb| mb * 1e6);
        return Stage::Download { fraction, speed };
    }
    if m.contains("Compiling") {
        return Stage::Compiling;
    }
    if m.starts_with("Applying") {
        return Stage::Applying;
    }
    if ["Loading", "Preparing", "Starting", "Switching"].iter().any(|p| m.starts_with(p)) {
        return Stage::Preparing;
    }
    if let Some(step) = step_in(m) {
        return Stage::Processing { step: Some(step) };
    }
    if fraction >= 0.995 {
        return Stage::Applying;
    }
    if fraction <= 0.0 {
        return Stage::Preparing;
    }
    Stage::Processing { step: None }
}

/// "45" from "…: 45% at…".
fn percent_in(s: &str) -> Option<f32> {
    let end = s.find('%')?;
    let start = s[..end].rfind(|c: char| !(c.is_ascii_digit() || c == '.')).map_or(0, |i| i + 1);
    s[start..end].parse().ok().filter(|p: &f32| p.is_finite())
}

/// `(3, 8)` from "… (3 of 8)" or "… (step 3 of 8)".
fn step_in(s: &str) -> Option<(usize, usize)> {
    let open = s.rfind('(')?;
    let inner = s[open + 1..].strip_suffix(')')?.trim_start_matches("step ").trim();
    let (a, b) = inner.split_once(" of ")?;
    let (i, n) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    (n > 1 && i >= 1 && i <= n).then_some((i, n))
}

/// The stage in the UI language.
pub fn stage_text(s: &Stage) -> String {
    match s {
        Stage::Download { fraction, speed } => {
            let mut t = match fraction {
                Some(f) => crate::i18n::fmt(tl!("Downloading model {pct}%"), &[("pct", &format!("{:.0}", (f * 100.0).floor()))]),
                None => tl!("Downloading model…").to_string(),
            };
            if let Some(b) = speed.filter(|b| *b > 1.0) {
                t.push_str(&format!(" · {:.1} MB/s", b / 1e6));
            }
            t
        }
        Stage::Preparing => tl!("Preparing AI…").to_string(),
        Stage::Compiling => tl!("Preparing AI for this GPU (first run only)…").to_string(),
        Stage::Processing { step: Some((i, n)) } => crate::i18n::fmt(tl!("Processing {i}/{n}"), &[("i", &i.to_string()), ("n", &n.to_string())]),
        Stage::Processing { step: None } => tl!("Processing…").to_string(),
        Stage::Applying => tl!("Applying…").to_string(),
    }
}

/// A run of `id` is starting: remember it (Retry) and what it works on (the canvas pulse).
pub fn note_start(app: &mut PhotocraftApp, id: &str, params: &Value) {
    app.ai_progress.last = Some((id.to_string(), params.clone()));
    app.ai_progress.outcome = None;
}

/// `id` started as job `job` (after [`note_start`]).
pub fn started(app: &mut PhotocraftApp, id: &str, params: &Value, job: JobId) {
    let region = region_of(app, params);
    let title = title_of(id);
    app.ai_progress.tracked = Some(Tracked { job, title, started: None, cpu: false, region });
}

fn title_of(id: &str) -> String {
    match id {
        photocraft_engine::remove_ai_cmds::ID | "paint.remove" => tl!("Remove with AI").to_string(),
        photocraft_engine::ai_inpaint_cmds::ID => tl!("Clean with AI").to_string(),
        photocraft_engine::ai_cmds::COMMAND => tl!("Upscale with AI…").trim_end_matches('…').to_string(),
        _ => tl!("Generative Fill").to_string(),
    }
}

/// What a run with `params` works on: its strokes' bounds (with the brush), else the selection,
/// else the whole canvas.
fn region_of(app: &PhotocraftApp, params: &Value) -> Option<(DocId, photocraft_geom::Rect)> {
    let st = app.session.active()?;
    let canvas = st.doc.bounds();
    let pad = params.get("size").and_then(Value::as_f64).unwrap_or(0.0) / 2.0 + 2.0;
    let mut b: Option<(f64, f64, f64, f64)> = None;
    for stroke in params.get("strokes").and_then(Value::as_array).into_iter().flatten().chain(params.get("points")) {
        for p in stroke.as_array().into_iter().flatten() {
            let (Some(x), Some(y)) = (p.get(0).and_then(Value::as_f64), p.get(1).and_then(Value::as_f64)) else { continue };
            b = Some(b.map_or((x, y, x, y), |(a, c, d, e)| (a.min(x), c.min(y), d.max(x), e.max(y))));
        }
    }
    let r = match b {
        Some((x0, y0, x1, y1)) => {
            photocraft_geom::Rect::new((x0 - pad).floor() as i32, (y0 - pad).floor() as i32, (x1 + pad).ceil() as i32, (y1 + pad).ceil() as i32)
        }
        None => st.doc.selection.as_ref().map(|s| s.content_bounds()).filter(|r| !r.is_empty()).unwrap_or(canvas),
    };
    let r = r.intersect(&canvas);
    (!r.is_empty()).then_some((st.doc.id, r))
}

/// A job ended: an AI one says "Done" (and taps the vibration) or keeps its error for the card.
/// Returns whether the card shows the failure (the shell then doesn't post it again).
pub fn on_event(app: &mut PhotocraftApp, ctx: &egui::Context, e: &JobEvent) -> bool {
    let Some(t) = app.ai_progress.tracked.as_ref().filter(|t| t.job == e.id) else { return false };
    let title = t.title.clone();
    app.ai_progress.tracked = None;
    let now = ctx.input(|i| i.time);
    match &e.outcome {
        JobOutcome::Done(_) => {
            app.ai_progress.outcome = Some(Outcome::Done { title, at: now });
            if let Some(vibrate) = app.services.vibrate {
                vibrate(40);
            }
            false
        }
        JobOutcome::Failed(err) => {
            app.ai_progress.outcome = Some(Outcome::Failed { title, error: err.clone(), retry: app.ai_progress.last.clone() });
            true
        }
        JobOutcome::Cancelled => false,
    }
}

/// The card follows job `id` (the modal progress dialog stays away).
pub fn follows(app: &PhotocraftApp, id: JobId) -> bool {
    app.ai_progress.tracked.as_ref().is_some_and(|t| t.job == id)
}

/// Nothing to show about the last run (its failure is handled elsewhere).
pub fn dismiss(app: &mut PhotocraftApp) {
    app.ai_progress.outcome = None;
}

/// An AI run (or its model's download) is in progress: the Capi button pulses.
pub fn busy(app: &PhotocraftApp) -> bool {
    app.ai_progress.tracked.as_ref().is_some_and(|t| app.session.job(t.job).is_some()) || crate::remove_ai_ui::is_open(app) || downloading(app).is_some()
}

/// A model download the card shows (none while the AI Model Manager, which shows its own, is open).
fn downloading(app: &PhotocraftApp) -> Option<(&'static photocraft_ai::catalog::ModelInfo, photocraft_ai::models::Status)> {
    if app.ui.ai_models.open {
        return None;
    }
    let store = app.session.ai_models()?;
    photocraft_ai::models::files().into_iter().find_map(|m| match store.status(m) {
        s @ photocraft_ai::models::Status::Downloading { .. } => Some((m, s)),
        _ => None,
    })
}

/// What the card shows this frame.
struct Card {
    title: String,
    stage: Stage,
    /// The bar's share (`None`: sliding).
    fraction: Option<f32>,
    /// A smaller line under the stage.
    note: Option<String>,
    elapsed: Option<f64>,
    cancel: CancelWith,
    /// The download failed: Retry starts it again.
    retry: bool,
}

enum CancelWith {
    Job(JobId),
    RemoveAiDownload,
    Download(&'static photocraft_ai::catalog::ModelInfo),
}

fn card(app: &mut PhotocraftApp, now: f64) -> Option<Card> {
    if let Some(t) = app.ai_progress.tracked.as_mut()
        && let Some(j) = app.session.job(t.job)
    {
        let started = *t.started.get_or_insert(now);
        let stage = stage(&j.message, j.progress);
        if j.message.contains("the CPU") {
            t.cpu = true;
        }
        let fraction = match stage {
            Stage::Download { fraction: Some(f), .. } => Some(f),
            _ => (j.progress > 0.0).then_some(j.progress),
        };
        let note = t.cpu.then(|| tl!("Using the CPU, it may take longer").to_string());
        return Some(Card { title: t.title.clone(), stage, fraction, note, elapsed: Some(now - started), cancel: CancelWith::Job(t.job), retry: false });
    }
    if crate::remove_ai_ui::is_open(app) {
        let (fraction, speed, failed) = crate::remove_ai_ui::download_progress(app);
        let bytes = photocraft_ai::catalog::model(photocraft_ai::catalog::LAMA_ID).map_or(0, |m| m.bytes);
        let note = match &failed {
            Some(why) => crate::i18n::fmt(tl!("The download failed: {reason}"), &[("reason", why)]),
            None => crate::i18n::fmt(
                tl!("Remove with AI needs to download the Manga LaMa model ({size}). Only once."),
                &[("size", &format!("{} MB", (bytes as f64 / 1e6).round()))],
            ),
        };
        return Some(Card {
            title: tl!("Remove with AI").to_string(),
            stage: Stage::Download { fraction, speed },
            fraction: if failed.is_some() { Some(0.0) } else { fraction },
            note: Some(note),
            elapsed: None,
            cancel: CancelWith::RemoveAiDownload,
            retry: failed.is_some(),
        });
    }
    if let Some((m, photocraft_ai::models::Status::Downloading { done, total, bytes_per_sec })) = downloading(app) {
        let fraction = (total > 0).then(|| done as f32 / total as f32);
        let speed = (bytes_per_sec > 1.0).then_some(bytes_per_sec);
        return Some(Card {
            title: m.title.to_string(),
            stage: Stage::Download { fraction, speed },
            fraction,
            note: None,
            elapsed: None,
            cancel: CancelWith::Download(m),
            retry: false,
        });
    }
    None
}

/// Where the card goes: under the simple interface's bars, else bottom right.
fn anchor(app: &PhotocraftApp, ctx: &egui::Context, width: f32) -> (egui::Pos2, Align2) {
    let screen = crate::safe_area::rect(ctx);
    if app.simple {
        let options = app.ui.panels.options_bar && app.session.active().is_some();
        let top = screen.top() + crate::simple_ui::TOP_BAR * if options { 2.0 } else { 1.0 } + 8.0;
        (pos2(screen.center().x, top), Align2::CENTER_TOP)
    } else {
        let bottom = screen.bottom() - if app.ui.panels.status_bar { 34.0 } else { 12.0 };
        (pos2(screen.right() - 12.0 - width / 2.0, bottom), Align2::CENTER_BOTTOM)
    }
}

fn inspect_id() -> Id {
    Id::new("ai-progress-inspect")
}

/// `ui.inspect`'s `aiProgress`: what the card shows this frame (`kind` "progress", "done" or
/// "error", the title, the stage, the bar's share, the buttons' rects), or null.
pub fn inspect(ctx: &egui::Context) -> Value {
    ctx.data(|d| d.get_temp::<Value>(inspect_id())).unwrap_or(Value::Null)
}

fn rect_json(r: Rect) -> Value {
    serde_json::json!([r.min.x, r.min.y, r.max.x, r.max.y])
}

/// Every frame: the card, the pulse over the canvas, "Done" and the error card.
pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<Value>(inspect_id()));
    let now = ctx.input(|i| i.time);
    // A job that ended without an event the card saw (it was replaced) stops being followed.
    if app.ai_progress.tracked.as_ref().is_some_and(|t| t.started.is_some() && app.session.job(t.job).is_none()) {
        app.ai_progress.tracked = None;
    }
    let screen = crate::safe_area::rect(ctx);
    let width = (screen.width() - 24.0).min(380.0);
    let (pos, align) = anchor(app, ctx, width);
    let t = Tokens::get(ctx);
    if let Some(c) = card(app, now) {
        pulse_region(app, ctx, now);
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
        let (mut cancel, mut retry) = (false, false);
        let (mut cancel_rect, mut retry_rect) = (Rect::NOTHING, Rect::NOTHING);
        area(ctx, "ai-progress", pos, align, width, &t, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                let k = (((now * 3.0).sin() * 0.5 + 0.5) as f32).mul_add(0.5, 0.5);
                crate::icons::paint(ui, r, "sparkles", 16.0, t.accent.gamma_multiply(k));
                ui.label(RichText::new(&c.title).font(crate::theme::semibold(14.0)).color(t.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = crate::widgets::secondary_button(ui, tl!("Cancel"), 84.0);
                    cancel_rect = r.rect;
                    cancel = r.clicked();
                    if let Some(s) = c.elapsed {
                        ui.label(RichText::new(elapsed(s)).color(t.text_faint).size(11.5).monospace());
                    }
                });
            });
            ui.add_space(2.0);
            ui.label(RichText::new(stage_text(&c.stage)).color(t.text_dim).size(12.5));
            ui.add_space(4.0);
            let (br, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
            crate::jobs_ui::bar(ui, br, c.fraction, &t);
            if let Some(n) = &c.note {
                ui.add_space(4.0);
                ui.label(RichText::new(n).color(if c.retry { t.danger } else { t.text_faint }).size(11.5));
            }
            if c.retry {
                ui.add_space(6.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = crate::widgets::primary_button(ui, tl!("Retry"), 84.0);
                    retry_rect = r.rect;
                    retry = r.clicked();
                });
            }
        });
        ctx.data_mut(|d| {
            d.insert_temp(
                inspect_id(),
                serde_json::json!({"kind": "progress", "title": c.title, "stage": stage_text(&c.stage), "fraction": c.fraction, "note": c.note,
                    "cancel": rect_json(cancel_rect), "retry": (retry_rect != Rect::NOTHING).then(|| rect_json(retry_rect))}),
            )
        });
        if retry {
            crate::remove_ai_ui::retry_download(app);
        }
        if cancel {
            match c.cancel {
                CancelWith::Job(j) => {
                    crate::jobs_ui::cancel(app, j);
                }
                CancelWith::RemoveAiDownload => crate::remove_ai_ui::cancel(app),
                CancelWith::Download(m) => {
                    if let Some(store) = app.session.ai_models() {
                        store.pause(Some(m));
                    }
                }
            }
        }
        return;
    }
    match app.ai_progress.outcome.as_ref() {
        Some(Outcome::Done { title, at }) => {
            if now - at > DONE_FOR {
                app.ai_progress.outcome = None;
                return;
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            let text = crate::i18n::fmt(tl!("{tool}: done"), &[("tool", title)]);
            ctx.data_mut(|d| d.insert_temp(inspect_id(), serde_json::json!({"kind": "done", "title": title, "text": text})));
            egui::Area::new(Id::new("ai-progress-done")).order(egui::Order::Foreground).pivot(align).fixed_pos(pos).interactable(false).show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(t.card)
                    .stroke(Stroke::new(1.0, t.card_border))
                    .corner_radius(CornerRadius::same(18))
                    .shadow(egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: t.shadow })
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                            crate::icons::paint(ui, r, "check", 15.0, t.accent);
                            ui.label(RichText::new(text).font(crate::theme::medium(13.0)).color(t.text));
                        });
                    });
            });
        }
        Some(Outcome::Failed { title, error, retry }) => {
            let (title, error, has_retry) = (title.clone(), error.clone(), retry.is_some());
            let (mut close, mut again) = (false, false);
            ctx.data_mut(|d| d.insert_temp(inspect_id(), serde_json::json!({"kind": "error", "title": title, "error": error})));
            area(ctx, "ai-progress-error", pos, align, width, &t, |ui| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                    crate::icons::paint(ui, r, "triangle-alert", 16.0, t.danger);
                    ui.label(RichText::new(&title).font(crate::theme::semibold(14.0)).color(t.text));
                });
                ui.add_space(2.0);
                ui.label(RichText::new(&error).color(t.danger).size(12.5));
                ui.add_space(6.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = crate::widgets::secondary_button(ui, tl!("Close"), 84.0).clicked();
                    if has_retry {
                        again = crate::widgets::primary_button(ui, tl!("Retry"), 84.0).clicked();
                    }
                });
            });
            if close {
                app.ai_progress.outcome = None;
            } else if again
                && let Some(Outcome::Failed { retry: Some((id, params)), .. }) = app.ai_progress.outcome.take()
                && let Err(e) = app.run(&id, params)
            {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
        None => {}
    }
}

/// A card at `pos`.
fn area(ctx: &egui::Context, id: &str, pos: egui::Pos2, align: Align2, width: f32, t: &Tokens, add: impl FnOnce(&mut egui::Ui)) {
    egui::Area::new(Id::new(id)).order(egui::Order::Foreground).pivot(align).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::NONE
            .fill(t.card)
            .stroke(Stroke::new(1.0, t.card_border))
            .corner_radius(CornerRadius::same(14))
            .shadow(egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: t.shadow })
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(width - 24.0);
                add(ui);
            });
    });
}

/// The followed run's area on the canvas: an outline that breathes and a light band sweeping
/// across it.
fn pulse_region(app: &PhotocraftApp, ctx: &egui::Context, now: f64) {
    let Some((doc, r)) = app.ai_progress.tracked.as_ref().and_then(|t| t.region) else { return };
    if app.session.active().map(|d| d.doc.id) != Some(doc) {
        return;
    }
    let Some(xf) = crate::canvas::ViewXform::active(app) else { return };
    let rect = xf.doc_rect(r);
    let clip = app.last_canvas_rect;
    if !rect.intersects(clip) {
        return;
    }
    let t = Tokens::get(ctx);
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, Id::new("ai-progress-region"))).with_clip_rect(clip);
    let k = ((now * 2.4).sin() * 0.5 + 0.5) as f32;
    p.rect_filled(rect, 2.0, t.accent.gamma_multiply(0.06 + 0.08 * k));
    p.rect_stroke(rect, 2.0, Stroke::new(1.5 + k, t.accent.gamma_multiply(0.55 + 0.4 * k)), egui::StrokeKind::Outside);
    // The shimmer: a soft diagonal band crossing the area every 1.6 s.
    let band = rect.width().max(rect.height()) * 0.35;
    let x = rect.left() - band + ((now / 1.6).fract() as f32) * (rect.width() + 2.0 * band);
    let shimmer = Rect::from_min_max(pos2(x.max(rect.left()), rect.top()), pos2((x + band).min(rect.right()), rect.bottom()));
    if shimmer.width() > 0.0 {
        p.with_clip_rect(rect.intersect(clip)).rect_filled(shimmer, 0.0, Color32::from_white_alpha(28));
    }
}

/// The Capi button's ring while [`busy`]: drawn by `simple_ui`.
pub fn capi_ring(ui: &egui::Ui, center: egui::Pos2, radius: f32) {
    let t = Tokens::get(ui.ctx());
    let now = ui.input(|i| i.time);
    let k = ((now * 3.0).sin() * 0.5 + 0.5) as f32;
    ui.painter().circle_stroke(center, radius + 2.0 + 4.0 * k, Stroke::new(3.0, t.accent.gamma_multiply(0.35 + 0.5 * (1.0 - k))));
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
}

fn elapsed(s: f64) -> String {
    let s = s.max(0.0);
    if s < 60.0 { format!("{s:.0} s") } else { format!("{}:{:02}", (s / 60.0).floor() as u64, (s % 60.0).floor() as u64) }
}

#[cfg(test)]
#[path = "ai_progress_tests.rs"]
mod tests;
