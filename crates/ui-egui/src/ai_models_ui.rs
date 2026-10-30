//! Window › AI Models…: the AI Model Manager.
//!
//! One card per model or Generative Fill tier (`photocraft_ai::cards`), in sections by what they
//! do (Clean, Upscale, Generative Fill): a small before/after picture, the size, quality and speed
//! (1–5), whether it needs a GPU, "Recommended for this device" (the same routing as Auto), one
//! sentence, a coloured state (Not downloaded, Downloading with MB, speed and time left, Paused,
//! Ready, Failed with Retry) and one button for all its files (Download, Pause / Resume, Cancel,
//! Delete). Details: licence, precision, memory, the files with their sha256. Shared files (TAESD)
//! download once and stay while another card uses them. The header shows the storage and the
//! browser's quota, Download recommended and Delete all. A full-screen sheet on phones; docks
//! like the other panels.
//!
//! The window is thin: every action is an `ai.models.*` engine command (so agents and automation
//! manage models the same way), and the rows are what the injected
//! [`photocraft_ai::models::ModelStore`] reports. Downloads run in the background, one at a
//! time, and go on while the window is closed and the user works; a small indicator in the status
//! bar shows the queue and opens the window. The queue lives for the session: a reload or restart
//! starts with an empty queue (what finished downloading stays).
//!
//! Using a tool never downloads (`ai_model_gate`): a tool whose model is missing opens this
//! window on that model's row.

use photocraft_ai::cards::{self, Card, CardState, Section};
use photocraft_ai::catalog::ModelInfo;
use photocraft_ai::models::{self, Status};
use photocraft_ai_sd::service::Tier;
use photocraft_engine::ai_models_cmds;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;

/// The menu item (Window › AI Models…).
pub const COMMAND: &str = "window.aiModels";

/// The window's view state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelsUi {
    pub open: bool,
    /// The weight file to point at (the tool that opened the window needs it).
    pub highlight: Option<String>,
    /// Delete all asked to confirm.
    pub confirm_delete_all: bool,
    /// The highlighted row was scrolled into view.
    #[serde(skip)]
    pub scrolled: bool,
    /// The cards' buttons last frame (card, action, screen rect), for `ui.inspect` and tests.
    #[serde(skip)]
    pub buttons: Vec<(String, String, [f32; 4])>,
}

/// Opens the window, pointing at `model`'s row when given.
pub fn open(app: &mut PhotocraftApp, model: Option<&ModelInfo>) {
    let m = &mut app.ui.ai_models;
    m.open = true;
    m.highlight = model.map(|m| m.file.to_string());
    m.scrolled = false;
    m.confirm_delete_all = false;
}

/// Window › AI Models… (`{}` opens it; `{"open": false}` closes it).
pub fn invoke(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id != COMMAND {
        return None;
    }
    if params.get("open").and_then(Value::as_bool) == Some(false) {
        app.ui.ai_models.open = false;
    } else {
        let model = params.get("model").and_then(Value::as_str).and_then(models::by_id_or_file);
        open(app, model);
    }
    Some(Ok(json!({"open": app.ui.ai_models.open})))
}

/// Megabytes for the UI: one decimal under 10 MB.
pub fn mb(bytes: u64) -> String {
    let v = bytes as f64 / 1e6;
    if v < 10.0 { format!("{v:.1}") } else { format!("{v:.0}") }
}

/// "12 MB", "1.4 GB".
fn size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 { format!("{:.1} GB", bytes as f64 / 1e9) } else { format!("{} MB", mb(bytes)) }
}

/// "12 s", "3 min".
fn duration(secs: f64) -> String {
    if secs < 90.0 { format!("{:.0} s", secs.max(1.0)) } else { format!("{:.0} min", (secs / 60.0).ceil()) }
}

/// The tools that use `model`, in the UI language.
pub fn tools_text(model: &ModelInfo) -> String {
    models::tools(model).iter().map(|id| crate::ai_model_gate::tool_name(id)).collect::<Vec<_>>().join(", ")
}

/// The status, as the row shows it.
pub fn status_text(status: &Status) -> String {
    match status {
        Status::NotDownloaded => tl!("Not downloaded").to_string(),
        Status::Queued => tl!("Queued").to_string(),
        Status::Downloading { done, total, bytes_per_sec } => {
            let pct = format!("{:.0}", *done as f64 * 100.0 / (*total).max(1) as f64);
            let mut s = crate::i18n::fmt(tl!("Downloading {pct}%"), &[("pct", &pct)]);
            if *bytes_per_sec > 1.0 {
                s.push_str(&format!(" · {}/s", size(*bytes_per_sec as u64)));
            }
            if let Some(eta) = status.eta_secs() {
                s.push_str(" · ");
                s.push_str(&crate::i18n::fmt(tl!("{time} left"), &[("time", &duration(eta))]));
            }
            s
        }
        Status::Paused { done, total } => format!("{} · {}", tl!("Paused"), progress_text(*done, *total, 0.0)),
        Status::Downloaded => tl!("Downloaded").to_string(),
        Status::UpdateAvailable => tl!("Update available").to_string(),
        Status::Failed(why) => crate::i18n::fmt(tl!("Failed: {reason}"), &[("reason", why)]),
        Status::Checking => tl!("Checking…").to_string(),
        Status::Unavailable(why) => why.clone(),
    }
}

/// Runs an `ai.models.*` command; errors go to the status bar.
fn act(app: &mut PhotocraftApp, cmd: &str, params: Value) {
    if let Err(e) = app.run(cmd, params) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// What a click asked for (applied after drawing).
enum Action {
    Download(&'static str),
    Pause(&'static str),
    Cancel(&'static str),
    Delete(&'static str),
    PauseAll,
    CancelAll,
    Recommended,
    DeleteAll,
}

/// A Generative Fill tier's name in the UI language.
pub fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Ultra => tl!("Ultra-light"),
        Tier::Fast => tl!("Fast"),
        Tier::Standard => tl!("Standard"),
        Tier::High => tl!("High"),
    }
}

/// A card's name as shown.
fn card_title(c: &Card) -> &'static str {
    match c.id {
        cards::GENFILL_ULTRA_CARD => tier_name(Tier::Ultra),
        cards::GENFILL_FAST_CARD => tier_name(Tier::Fast),
        cards::GENFILL_STANDARD_CARD => tier_name(Tier::Standard),
        cards::GENFILL_HIGH_CARD => tier_name(Tier::High),
        _ => c.title,
    }
}

/// A card's one sentence, in the UI language.
fn card_text(c: &Card) -> &'static str {
    match c.id {
        "manga-lama" => tl!("Erases text and SFX and redraws screentone and lines."),
        "migan" => tl!("A small cleaner for phones and computers without a GPU."),
        "mangajanai" => tl!("Sharp 2× and 4× upscaling of black-and-white manga pages."),
        "illustrationjanai" => tl!("2× and 4× upscaling of colour pages and covers."),
        "illustrationjanai-v3-detail" => tl!("2× and 4× upscaling of colour pages that keeps linework and screen texture."),
        "realesr-compact" => tl!("Fast upscaling that runs well on any device."),
        cards::GENFILL_ULTRA_CARD => tl!("Rough fills on any device, even without a GPU."),
        cards::GENFILL_FAST_CARD => tl!("Good fills with a smaller download."),
        cards::GENFILL_STANDARD_CARD => tl!("Detailed fills from a prompt, three variants in seconds on a GPU."),
        cards::GENFILL_HIGH_CARD => tl!("The most detail: many steps with guidance, slow, for a strong GPU."),
        _ => c.description,
    }
}

fn section_text(s: Section) -> (&'static str, &'static str) {
    match s {
        Section::Clean => (tl!("Clean"), tl!("Erase text and objects.")),
        Section::Upscale => (tl!("Upscale"), tl!("Enlarge pages without blur.")),
        Section::Generative => (tl!("Generative Fill"), tl!("Draw new content from a prompt.")),
    }
}

const GREEN: egui::Color32 = egui::Color32::from_rgb(46, 160, 90);

/// A section's icon, drawn (no font glyphs: the web build has no emoji font).
fn section_icon(ui: &mut egui::Ui, s: Section, color: egui::Color32) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
    let p = ui.painter();
    let c = r.center();
    let st = egui::Stroke::new(1.6, color);
    match s {
        // An eraser.
        Section::Clean => {
            let pts = [c + egui::vec2(-7.0, 2.0), c + egui::vec2(-1.0, -6.0), c + egui::vec2(7.0, 0.0), c + egui::vec2(1.0, 7.0)];
            p.add(egui::Shape::closed_line(pts.to_vec(), st));
            p.line_segment([c + egui::vec2(-4.0, -2.0), c + egui::vec2(4.0, 4.0)], st);
        }
        // Arrows out of a corner.
        Section::Upscale => {
            p.rect_stroke(egui::Rect::from_center_size(c + egui::vec2(-3.0, 3.0), egui::vec2(7.0, 7.0)), 1.0, st, egui::StrokeKind::Middle);
            p.line_segment([c + egui::vec2(0.0, 0.0), c + egui::vec2(7.0, -7.0)], st);
            p.line_segment([c + egui::vec2(2.0, -7.0), c + egui::vec2(7.0, -7.0)], st);
            p.line_segment([c + egui::vec2(7.0, -2.0), c + egui::vec2(7.0, -7.0)], st);
        }
        // A four-point sparkle.
        Section::Generative => star(p, c, 8.0, color),
    }
}

fn star(p: &egui::Painter, c: egui::Pos2, r: f32, color: egui::Color32) {
    let k = r * 0.28;
    let pts = vec![
        c + egui::vec2(0.0, -r),
        c + egui::vec2(k, -k),
        c + egui::vec2(r, 0.0),
        c + egui::vec2(k, k),
        c + egui::vec2(0.0, r),
        c + egui::vec2(-k, k),
        c + egui::vec2(-r, 0.0),
        c + egui::vec2(-k, -k),
    ];
    p.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
}

/// A tiny before | after picture of what a section does, drawn procedurally.
fn thumbnail(ui: &mut egui::Ui, s: Section, t: &Tokens) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(76.0, 46.0), egui::Sense::hover());
    let p = ui.painter_at(r);
    p.rect_filled(r, 4.0, egui::Color32::from_gray(238));
    let (l, rt) = r.split_left_right_at_fraction(0.5);
    let ink = egui::Color32::from_gray(40);
    match s {
        Section::Clean => {
            for half in [l, rt] {
                for i in 0..6 {
                    for j in 0..4 {
                        p.circle_filled(half.min + egui::vec2(4.0 + i as f32 * 6.0, 6.0 + j as f32 * 10.0), 1.4, egui::Color32::from_gray(150));
                    }
                }
            }
            // "Text" on the left only.
            for y in [16.0, 24.0] {
                p.rect_filled(egui::Rect::from_min_size(l.min + egui::vec2(8.0, y), egui::vec2(22.0, 4.0)), 1.0, ink);
            }
        }
        Section::Upscale => {
            for i in 0..5 {
                for j in 0..5 {
                    let g = if (i + j) % 2 == 0 { 70 } else { 200 };
                    p.rect_filled(
                        egui::Rect::from_min_size(l.min + egui::vec2(4.0 + i as f32 * 6.5, 6.0 + j as f32 * 6.5), egui::vec2(6.5, 6.5)),
                        0.0,
                        egui::Color32::from_gray(g),
                    );
                }
            }
            p.circle_filled(rt.center(), 14.0, egui::Color32::from_gray(70));
            p.circle_filled(rt.center() + egui::vec2(-4.0, -4.0), 6.0, egui::Color32::from_gray(200));
        }
        Section::Generative => {
            for half in [l, rt] {
                p.rect_filled(half.shrink(1.0), 3.0, egui::Color32::from_rgb(170, 200, 240));
            }
            let sel = egui::Rect::from_center_size(l.center(), egui::vec2(18.0, 18.0));
            p.rect_stroke(sel, 0.0, egui::Stroke::new(1.0, ink), egui::StrokeKind::Middle);
            star(&p, rt.center(), 10.0, egui::Color32::from_rgb(240, 190, 40));
        }
    }
    p.line_segment([egui::pos2(r.center().x, r.top()), egui::pos2(r.center().x, r.bottom())], egui::Stroke::new(1.0, t.card_border));
}

/// Five small bars, `n` of them lit.
fn meter(ui: &mut egui::Ui, label: &str, n: u8, t: &Tokens) {
    // One unit, so a wrapping row never splits the label from its bars.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(egui::RichText::new(label).color(t.text_dim).size(11.0));
        let (r, _) = ui.allocate_exact_size(egui::vec2(34.0, 10.0), egui::Sense::hover());
        for i in 0..5u8 {
            let b = egui::Rect::from_min_size(r.min + egui::vec2(f32::from(i) * 7.0, 2.0), egui::vec2(5.0, 7.0));
            ui.painter().rect_filled(b, 1.0, if i < n { t.accent } else { t.field_border });
        }
    });
}

/// A small rounded label.
fn chip(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(6, 1)).corner_radius(8.0).stroke(egui::Stroke::new(1.0, color)).show(ui, |ui| {
        ui.label(egui::RichText::new(text).color(color).size(11.0));
    });
}

/// "120 MB of 277 MB · 3.1 MB/s · 40 s left".
fn progress_text(done: u64, total: u64, speed: f64) -> String {
    let mut s = format!("{} / {}", size(done), size(total));
    if speed > 1.0 {
        s.push_str(&format!(" · {}/s", size(speed as u64)));
        let left = total.saturating_sub(done) as f64 / speed;
        s.push_str(" · ");
        s.push_str(&crate::i18n::fmt(tl!("{time} left"), &[("time", &duration(left))]));
    }
    s
}

/// One card.
#[allow(clippy::too_many_arguments)]
fn card_ui(
    ui: &mut egui::Ui,
    t: &Tokens,
    c: &'static Card,
    rows: &[(&'static ModelInfo, Status)],
    recommended: bool,
    lit: bool,
    action: &mut Option<Action>,
    buttons: &mut Vec<(String, String, [f32; 4])>,
) -> egui::Response {
    // Only buttons a click can reach (inside the scroll area's visible part).
    let mut rec = |name: &str, r: &egui::Response| {
        let v = r.rect.intersect(r.interact_rect);
        if r.interact_rect.is_positive() && v.is_positive() {
            buttons.push((c.id.to_string(), name.to_string(), [v.min.x, v.min.y, v.max.x, v.max.y]));
        }
    };
    let files = c.models();
    let status_of = |m: &ModelInfo| rows.iter().find(|(r, _)| r.file == m.file).map(|(_, s)| s.clone()).unwrap_or(Status::NotDownloaded);
    let st: Vec<(&'static ModelInfo, Status)> = files.iter().map(|m| (*m, status_of(m))).collect();
    // A shared file (TAESD) downloading or paused for another card doesn't make this one
    // "downloading" or "paused": only its own files do.
    let own: Vec<&str> =
        files.iter().filter(|m| !cards::CARDS.iter().any(|o| o.id != c.id && o.models().iter().any(|x| x.file == m.file))).map(|m| m.file).collect();
    let active = |s: &Status| s.is_pending() || matches!(s, Status::Paused { .. });
    let started = own.is_empty() || st.iter().any(|(m, s)| own.contains(&m.file) && active(s));
    let state = CardState::of(&st.iter().map(|(m, s)| (m.bytes, if !started && active(s) { Status::NotDownloaded } else { s.clone() })).collect::<Vec<_>>());
    let (label, color) = match &state {
        CardState::NotDownloaded => (tl!("Not downloaded").to_string(), t.text_dim),
        CardState::Downloading { .. } => (tl!("Downloading").to_string(), t.accent),
        CardState::Paused { .. } => (tl!("Paused").to_string(), t.warning),
        CardState::Ready => (tl!("Ready").to_string(), GREEN),
        CardState::Error(e) => (crate::i18n::fmt(tl!("Failed: {reason}"), &[("reason", e)]), t.danger),
        CardState::Checking => (tl!("Checking…").to_string(), t.text_dim),
        CardState::Unavailable(why) => (why.clone(), t.warning),
    };
    let border = if lit { t.accent } else { t.card_border };
    egui::Frame::NONE
        .fill(t.card)
        .stroke(egui::Stroke::new(if lit { 2.0 } else { 1.0 }, border))
        .corner_radius(t.radius)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            // The frame's margins and border sit outside this width.
            let w = (ui.max_rect().width() - 22.0).max(180.0);
            ui.set_width(w);
            ui.set_max_width(w);
            ui.horizontal(|ui| {
                thumbnail(ui, c.section, t);
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(card_title(c)).strong());
                        ui.label(egui::RichText::new(size(c.bytes())).color(t.text_dim));
                    });
                    ui.horizontal_wrapped(|ui| {
                        meter(ui, tl!("Quality"), c.quality, t);
                        meter(ui, tl!("Speed"), c.speed, t);
                        if c.gpu { chip(ui, tl!("Needs a GPU"), t.text_dim) } else { chip(ui, tl!("No GPU needed"), GREEN) }
                    });
                    if recommended {
                        chip(ui, tl!("Recommended for this device"), t.accent);
                    }
                });
            });
            ui.label(egui::RichText::new(card_text(c)).size(12.5));
            ui.add_space(2.0);
            match &state {
                CardState::Downloading { done, total, .. } | CardState::Paused { done, total } if *total > 0 => {
                    let speed = if let CardState::Downloading { bytes_per_sec, .. } = state { bytes_per_sec } else { 0.0 };
                    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::hover());
                    crate::jobs_ui::bar(ui, r, Some(*done as f32 / (*total).max(1) as f32), t);
                    ui.label(egui::RichText::new(progress_text(*done, *total, speed)).color(t.text_dim).size(11.5));
                }
                _ => {}
            }
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&label).color(color).size(12.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match &state {
                    CardState::NotDownloaded => {
                        if {
                            let r = crate::widgets::primary_button(ui, tl!("Download"), 84.0);
                            rec("download", &r);
                            r
                        }
                        .clicked()
                        {
                            *action = Some(Action::Download(c.id));
                        }
                    }
                    CardState::Error(_) => {
                        if {
                            let r = crate::widgets::primary_button(ui, tl!("Retry"), 84.0);
                            rec("retry", &r);
                            r
                        }
                        .clicked()
                        {
                            *action = Some(Action::Download(c.id));
                        }
                    }
                    CardState::Downloading { .. } => {
                        if {
                            let r = crate::widgets::secondary_button(ui, tl!("Cancel"), 70.0);
                            rec("cancel", &r);
                            r
                        }
                        .clicked()
                        {
                            *action = Some(Action::Cancel(c.id));
                        }
                        if {
                            let r = crate::widgets::secondary_button(ui, tl!("Pause"), 70.0);
                            rec("pause", &r);
                            r
                        }
                        .clicked()
                        {
                            *action = Some(Action::Pause(c.id));
                        }
                    }
                    CardState::Paused { .. } => {
                        if {
                            let r = crate::widgets::secondary_button(ui, tl!("Cancel"), 70.0);
                            rec("cancel", &r);
                            r
                        }
                        .clicked()
                        {
                            *action = Some(Action::Cancel(c.id));
                        }
                        if {
                            let r = crate::widgets::primary_button(ui, tl!("Resume"), 70.0);
                            rec("resume", &r);
                            r
                        }
                        .clicked()
                        {
                            *action = Some(Action::Download(c.id));
                        }
                    }
                    CardState::Ready => {
                        if {
                            let r = crate::widgets::secondary_button(ui, tl!("Delete"), 70.0);
                            rec("delete", &r);
                            r
                        }
                        .clicked()
                        {
                            *action = Some(Action::Delete(c.id));
                        }
                    }
                    CardState::Checking | CardState::Unavailable(_) => {}
                });
            });
            egui::CollapsingHeader::new(egui::RichText::new(tl!("Details")).size(12.0)).id_salt(("ai-card-details", c.id)).default_open(false).show(ui, |ui| {
                let dim = |s: String| egui::RichText::new(s).color(t.text_dim).size(11.5);
                if let Some(m) = files.first() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(dim(tl!("Licence:").to_string()));
                        ui.hyperlink_to(egui::RichText::new(format!("{} · {}", m.license, m.author)).size(11.5), m.source);
                    });
                }
                let mem = if c.gpu { tl!("GPU memory") } else { tl!("Memory") };
                ui.label(dim(format!("{}: {} · {mem}: {}", tl!("Precision"), c.precision, size(u64::from(c.memory_mb) * 1_000_000))));
                ui.label(dim(crate::i18n::fmt(tl!("Used by: {tools}"), &[("tools", &files.first().map(|m| tools_text(m)).unwrap_or_default())])));
                for (m, s) in &st {
                    ui.label(dim(format!("{} · {} · {} · sha256 {}…", m.file, size(m.bytes), status_text(s), m.sha256.get(..12).unwrap_or(""))));
                }
            });
        })
        .response
}

/// Draws the window (or the docked panel) when it is open.
pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ui.ai_models.open {
        return;
    }
    let t = Tokens::get(ctx);
    let store = app.session.ai_models();
    let rows: Vec<(&'static ModelInfo, Status)> = match &store {
        Some(s) => models::files().into_iter().map(|m| (m, s.status(m))).collect(),
        None => Vec::new(),
    };
    let storage = store.as_ref().map(|s| s.storage()).unwrap_or_default();
    let available = store.as_ref().map_or_else(|| Err(ai_models_cmds::UNAVAILABLE.to_string()), |s| s.available());
    let rec_cards = ai_models_cmds::recommended_cards(&app.session);
    let rec_missing: u64 = rec_cards
        .iter()
        .filter_map(|id| cards::card(id))
        .flat_map(|c| c.models())
        .filter(|m| rows.iter().any(|(r, s)| r.file == m.file && !s.is_downloaded() && !s.is_pending()))
        .map(|m| m.bytes)
        .sum();
    let any_downloaded = rows.iter().any(|(_, s)| matches!(s, Status::Downloaded | Status::UpdateAvailable));
    let busy = rows.iter().any(|(_, s)| s.is_pending());
    let checking = rows.iter().any(|(_, s)| matches!(s, Status::Checking));
    let highlight = app.ui.ai_models.highlight.clone();
    let mut scrolled = app.ui.ai_models.scrolled;
    let mut confirm_all = app.ui.ai_models.confirm_delete_all;
    let mut open = true;
    let mut action: Option<Action> = None;
    let mut buttons: Vec<(String, String, [f32; 4])> = Vec::new();
    let canvas = app.last_canvas_rect;
    let screen = ctx.content_rect();
    // Phones: a full-screen sheet.
    let phone = screen.width() < 600.0;
    let host = crate::floaters::Host::of(app, ctx, crate::dock::Group::AiModels);
    let mut window = egui::Window::new(tl!("AI Models")).id(egui::Id::unique("ai-models-window")).open(&mut open).collapsible(false);
    window = if phone {
        window.fixed_rect(screen).resizable(false).title_bar(true)
    } else {
        // Big by default: three cards a row on a laptop.
        let w = (screen.width() * 0.8).clamp(560.0, 1120.0);
        let h = (screen.height() * 0.84).clamp(420.0, 900.0);
        window
            .resizable(true)
            .default_width(w)
            .default_height(h)
            .default_pos(egui::pos2((canvas.center().x - w / 2.0).max(screen.left() + 8.0), (screen.top() + 40.0).max(canvas.top() - 30.0)))
    };
    host.show(ctx, egui::Id::unique("ai-models-window"), window, |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| host.header_button(ui));
        if let Err(why) = &available {
            ui.colored_label(t.warning, why.as_str());
            ui.add_space(4.0);
        }
        // Header: storage, Download recommended, Delete all.
        let mut line = crate::i18n::fmt(tl!("Models on this device: {size}"), &[("size", &size(storage.models.unwrap_or(0)))]);
        if let (Some(used), Some(quota)) = (storage.used, storage.quota) {
            line.push_str(" · ");
            line.push_str(&crate::i18n::fmt(tl!("site storage {used} of {quota}"), &[("used", &size(used)), ("quota", &size(quota))]));
        }
        ui.label(egui::RichText::new(line).color(t.text_dim));
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let ok = available.is_ok();
            let rec_label = crate::i18n::fmt(tl!("Download recommended for this device ({size})"), &[("size", &size(rec_missing))]);
            if ui.add_enabled_ui(ok && rec_missing > 0, |ui| crate::widgets::primary_button(ui, &rec_label, 0.0)).inner.clicked() {
                action = Some(Action::Recommended);
            }
            if phone {
                ui.end_row();
            }
            if confirm_all {
                ui.label(tl!("Delete every downloaded model?"));
                if crate::widgets::secondary_button(ui, tl!("Delete all"), 0.0).clicked() {
                    action = Some(Action::DeleteAll);
                    confirm_all = false;
                }
                if crate::widgets::secondary_button(ui, tl!("Keep"), 0.0).clicked() {
                    confirm_all = false;
                }
            } else if ui.add_enabled_ui(any_downloaded || busy, |ui| crate::widgets::secondary_button(ui, tl!("Delete all"), 0.0)).inner.clicked() {
                confirm_all = true;
            }
        });
        if busy {
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(queue_text(&rows)).color(t.text_dim));
                if crate::widgets::secondary_button(ui, tl!("Pause all"), 0.0).clicked() {
                    action = Some(Action::PauseAll);
                }
                if crate::widgets::secondary_button(ui, tl!("Cancel all"), 0.0).clicked() {
                    action = Some(Action::CancelAll);
                }
            });
        }
        ui.add_space(6.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let width = ui.available_width().min(ui.clip_rect().width());
            // Cards side by side when there is room.
            let cols = if width >= 760.0 {
                3
            } else if width >= 500.0 {
                2
            } else {
                1
            };
            let card_w = (width - 10.0 * (cols as f32 - 1.0)) / cols as f32;
            for section in [Section::Clean, Section::Upscale, Section::Generative] {
                crate::widgets::hairline(ui);
                ui.add_space(6.0);
                let (title, purpose) = section_text(section);
                ui.horizontal(|ui| {
                    section_icon(ui, section, t.accent);
                    ui.label(egui::RichText::new(title).font(crate::theme::semibold(14.0)));
                    ui.label(egui::RichText::new(purpose).color(t.text_dim).size(12.0));
                });
                ui.add_space(4.0);
                let list: Vec<&'static Card> = cards::CARDS.iter().filter(|c| c.section == section).collect();
                for chunk in list.chunks(cols) {
                    ui.horizontal_top(|ui| {
                        for c in chunk {
                            let lit = highlight.as_deref().is_some_and(|h| c.models().iter().any(|m| m.file == h));
                            let r = ui
                                .allocate_ui_with_layout(egui::vec2(card_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                                    ui.set_max_width(card_w);
                                    card_ui(ui, &t, c, &rows, rec_cards.contains(&c.id), lit, &mut action, &mut buttons)
                                })
                                .inner;
                            if lit && !scrolled {
                                r.scroll_to_me(Some(egui::Align::Center));
                                scrolled = true;
                            }
                        }
                    });
                    ui.add_space(8.0);
                }
            }
            let note = if cfg!(target_arch = "wasm32") {
                tl!("Models are kept in this browser and run on this device: your images are not uploaded.")
            } else {
                tl!("Models are kept on this computer and run on this device: your images are not uploaded.")
            };
            ui.label(egui::RichText::new(note).color(t.text_faint).size(11.5));
        });
    });
    let m = &mut app.ui.ai_models;
    m.open = open;
    m.scrolled = scrolled;
    m.confirm_delete_all = confirm_all;
    m.buttons = buttons;
    match action {
        Some(Action::Download(id)) => act(app, ai_models_cmds::DOWNLOAD, json!({"card": id})),
        Some(Action::Pause(id)) => act(app, ai_models_cmds::PAUSE, json!({"card": id})),
        Some(Action::Cancel(id)) => act(app, ai_models_cmds::CANCEL, json!({"card": id})),
        Some(Action::Delete(id)) => act(app, ai_models_cmds::DELETE, json!({"card": id})),
        Some(Action::PauseAll) => act(app, ai_models_cmds::PAUSE, json!({})),
        Some(Action::CancelAll) => act(app, ai_models_cmds::CANCEL, json!({})),
        Some(Action::Recommended) => {
            for id in rec_cards {
                act(app, ai_models_cmds::DOWNLOAD, json!({"card": id}));
            }
        }
        Some(Action::DeleteAll) => act(app, ai_models_cmds::DELETE, json!({"all": true})),
        None => {}
    }
    // Progress arrives in the background: ~10 Hz while downloading, nothing when idle (the
    // browser's cache answers once at start).
    if busy {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    } else if checking {
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

/// The name of the card a file belongs to (the one that owns it alone, else the first), for
/// progress lines: "Ultra-light", not "text encoder, 4-bit".
fn file_card_title(m: &ModelInfo) -> String {
    let with: Vec<&Card> = cards::CARDS.iter().filter(|c| c.models().iter().any(|x| x.file == m.file)).collect();
    match with.first() {
        Some(c) if c.section == Section::Generative => crate::i18n::fmt(tl!("Generative Fill - {tier}"), &[("tier", card_title(c))]),
        Some(c) if with.len() == 1 => card_title(c).to_string(),
        _ => models::file_title(m),
    }
}

/// "Downloading Manga LaMa 42% · 2 more queued".
fn queue_text(rows: &[(&'static ModelInfo, Status)]) -> String {
    let queued = rows.iter().filter(|(_, s)| *s == Status::Queued).count();
    let mut s = match rows.iter().find(|(_, s)| matches!(s, Status::Downloading { .. })) {
        Some((m, Status::Downloading { done, total, .. })) => {
            let pct = format!("{:.0}", *done as f64 * 100.0 / (*total).max(1) as f64);
            crate::i18n::fmt(tl!("Downloading {model} {pct}%"), &[("model", &file_card_title(m)), ("pct", &pct)])
        }
        _ => tl!("Starting the next download…").to_string(),
    };
    if queued > 0 {
        s.push_str(" · ");
        s.push_str(&crate::i18n::trn(crate::i18n::current(), queued as u64, "{n} more queued", "{n} more queued"));
    }
    s
}

/// The status bar's download indicator ("↓ Manga LaMa 42% (+2)"); a click opens the window.
/// Draws nothing while no download is queued.
pub fn status_indicator(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some(store) = app.session.ai_models() else { return };
    let rows: Vec<(&'static ModelInfo, Status)> = models::files().into_iter().map(|m| (m, store.status(m))).filter(|(_, s)| s.is_pending()).collect();
    if rows.is_empty() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let more = rows.iter().filter(|(_, s)| *s == Status::Queued).count();
    let text = match rows.iter().find(|(_, s)| matches!(s, Status::Downloading { .. })) {
        Some((m, Status::Downloading { done, total, .. })) => format!("↓ {} {:.0}%", file_card_title(m), *done as f64 * 100.0 / (*total).max(1) as f64),
        _ => "↓ …".to_string(),
    };
    let text = if more > 0 { format!("{text} (+{more})") } else { text };
    ui.add_space(6.0);
    let r = ui
        .add(egui::Label::new(egui::RichText::new(text).color(t.text_dim).size(12.0)).sense(egui::Sense::click()))
        .on_hover_text(tl!("AI model downloads: click to open the AI Model Manager"));
    if r.clicked() {
        open(app, None);
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
}

/// What `ui.inspect` reports about the window.
pub fn inspect(app: &PhotocraftApp) -> Value {
    let buttons: Vec<Value> = app.ui.ai_models.buttons.iter().map(|(c, a, r)| json!({"card": c, "action": a, "rect": r})).collect();
    json!({"open": app.ui.ai_models.open, "highlight": app.ui.ai_models.highlight, "buttons": buttons})
}

#[cfg(test)]
#[path = "ai_models_ui_tests.rs"]
mod tests;
