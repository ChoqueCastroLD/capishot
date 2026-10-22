//! Window › AI Models…: the AI Model Manager.
//!
//! Every model of the platform catalog (`photocraft_ai::catalog`), one row per weight file,
//! grouped Cleaning / Upscaling: its name and size, the tools that use it, where it runs well,
//! its licence and author (a link to the original release), its status (Not downloaded, Queued,
//! Downloading 42 % with speed and time left, Downloaded, Failed and why, Update available) and
//! Download / Cancel / Delete. The header shows the storage the models take and the browser's
//! quota, and offers Download recommended for this device (full models with a usable GPU, lite
//! ones otherwise), Download all and Delete all.
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

use photocraft_ai::catalog::ModelInfo;
use photocraft_ai::models::{self, Group, Runs, Status};
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

fn runs_text(model: &ModelInfo) -> &'static str {
    match models::runs(model) {
        Runs::BestOnGpu => tl!("GPU or CPU (best on a GPU)"),
        Runs::Both => tl!("GPU or CPU (lite: fine on either)"),
    }
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
    Cancel(Option<&'static str>),
    Delete(&'static str),
    Recommended,
    All,
    DeleteAll,
    /// Every missing file of a group (Generative Fill's parts).
    Group(Vec<&'static str>),
}

/// Draws the window when it is open.
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
    let recommended = ai_models_cmds::recommended(&app.session);
    let lite = recommended.first().is_some_and(|m| m.lite);
    let missing = |list: &[&'static ModelInfo]| -> u64 {
        list.iter().filter(|m| rows.iter().any(|(r, s)| r.file == m.file && !s.is_downloaded() && !s.is_pending())).map(|m| m.bytes).sum()
    };
    // "Download all" means the tools' models; Generative Fill (2 GB) has its own button.
    let tool_files: Vec<&'static ModelInfo> = models::files().into_iter().filter(|m| models::group(m) != Group::Generative).collect();
    let gen_files: Vec<&'static ModelInfo> = models::files().into_iter().filter(|m| models::group(m) == Group::Generative).collect();
    let (rec_missing, all_missing, gen_missing) = (missing(&recommended), missing(&tool_files), missing(&gen_files));
    let any_downloaded = rows.iter().any(|(_, s)| matches!(s, Status::Downloaded | Status::UpdateAvailable));
    let busy = rows.iter().any(|(_, s)| s.is_pending());
    let highlight = app.ui.ai_models.highlight.clone();
    let mut scrolled = app.ui.ai_models.scrolled;
    let mut confirm_all = app.ui.ai_models.confirm_delete_all;
    let mut open = true;
    let mut action: Option<Action> = None;
    let canvas = app.last_canvas_rect;
    // Its own window, or docked (`crate::floaters`).
    let host = crate::floaters::Host::of(app, ctx, crate::dock::Group::AiModels);
    let window = egui::Window::new(tl!("AI Models"))
        .id(egui::Id::new("ai-models-window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(600.0)
        .default_height(560.0)
        .default_pos(egui::pos2((canvas.center().x - 300.0).max(canvas.left() + 8.0), canvas.top() + 40.0));
    host.show(ctx, egui::Id::new("ai-models-window"), window, |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| host.header_button(ui));
        if let Err(why) = &available {
            ui.colored_label(t.warning, why.as_str());
            ui.add_space(4.0);
        }
        // Storage.
        let mut line = crate::i18n::fmt(tl!("Models on this device: {size}"), &[("size", &size(storage.models.unwrap_or(0)))]);
        if let (Some(used), Some(quota)) = (storage.used, storage.quota) {
            line.push_str(" · ");
            line.push_str(&crate::i18n::fmt(tl!("site storage {used} of {quota}"), &[("used", &size(used)), ("quota", &size(quota))]));
        }
        ui.label(egui::RichText::new(line).color(t.text_dim));
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let ok = available.is_ok();
            let rec_label = crate::i18n::fmt(tl!("Download recommended for this device ({size})"), &[("size", &size(rec_missing))]);
            let hint = if lite {
                tl!("No usable GPU here: the lite models (MI-GAN, Real-ESRGAN Compact).")
            } else {
                tl!("A GPU is available: the full models (Manga LaMa, MangaJaNai for your open pages, IllustrationJaNai).")
            };
            if ui.add_enabled_ui(ok && rec_missing > 0, |ui| crate::widgets::primary_button(ui, &rec_label, 0.0)).inner.on_hover_text(hint).clicked() {
                action = Some(Action::Recommended);
            }
            let all_label = crate::i18n::fmt(tl!("Download all ({size})"), &[("size", &size(all_missing))]);
            if ui.add_enabled_ui(ok && all_missing > 0, |ui| crate::widgets::secondary_button(ui, &all_label, 0.0)).inner.clicked() {
                action = Some(Action::All);
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
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(queue_text(&rows)).color(t.text_dim));
                if crate::widgets::secondary_button(ui, tl!("Cancel all"), 0.0).clicked() {
                    action = Some(Action::Cancel(None));
                }
            });
        }
        ui.add_space(6.0);
        crate::widgets::hairline(ui);
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            for (group, title) in [(Group::Cleaning, tl!("Cleaning")), (Group::Upscaling, tl!("Upscaling")), (Group::Generative, tl!("Generative Fill"))] {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(title).font(crate::theme::semibold(13.0)));
                    if group == Group::Generative && gen_missing > 0 && available.is_ok() {
                        let label = format!("{} ({})", tl!("Download"), size(gen_missing));
                        if crate::widgets::secondary_button(ui, &label, 0.0).clicked() {
                            let ids = gen_files
                                .iter()
                                .filter(|m| rows.iter().any(|(r, s)| r.file == m.file && !s.is_downloaded() && !s.is_pending()))
                                .map(|m| m.id)
                                .collect();
                            action = Some(Action::Group(ids));
                        }
                    }
                });
                ui.add_space(2.0);
                for (m, status) in rows.iter().filter(|(m, _)| models::group(m) == group) {
                    let lit = highlight.as_deref() == Some(m.file);
                    let frame = egui::Frame::NONE.inner_margin(egui::Margin::symmetric(8, 6)).corner_radius(t.radius_sm).fill(if lit {
                        t.accent.gamma_multiply(0.18)
                    } else {
                        egui::Color32::TRANSPARENT
                    });
                    let r = frame.show(ui, |ui| row(ui, &t, m, status, &mut action)).response;
                    if lit && !scrolled {
                        r.scroll_to_me(Some(egui::Align::Center));
                        scrolled = true;
                    }
                }
            }
            ui.add_space(6.0);
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
    match action {
        Some(Action::Download(id)) => act(app, ai_models_cmds::DOWNLOAD, json!({"id": id})),
        Some(Action::Cancel(id)) => act(app, ai_models_cmds::CANCEL, json!({"id": id})),
        Some(Action::Delete(id)) => act(app, ai_models_cmds::DELETE, json!({"id": id})),
        Some(Action::Recommended) => act(app, ai_models_cmds::DOWNLOAD, json!({"recommended": true})),
        Some(Action::All) => {
            let ids: Vec<&str> = models::files().into_iter().filter(|m| models::group(m) != Group::Generative).map(|m| m.id).collect();
            act(app, ai_models_cmds::DOWNLOAD, json!({"ids": ids}))
        }
        Some(Action::Group(ids)) => act(app, ai_models_cmds::DOWNLOAD, json!({"ids": ids})),
        Some(Action::DeleteAll) => act(app, ai_models_cmds::DELETE, json!({"all": true})),
        None => {}
    }
    // Progress and the browser's answers arrive in the background.
    ctx.request_repaint_after(std::time::Duration::from_millis(if busy { 150 } else { 1000 }));
}

/// "Downloading Manga LaMa 42% · 2 more queued".
fn queue_text(rows: &[(&'static ModelInfo, Status)]) -> String {
    let queued = rows.iter().filter(|(_, s)| *s == Status::Queued).count();
    let mut s = match rows.iter().find(|(_, s)| matches!(s, Status::Downloading { .. })) {
        Some((m, Status::Downloading { done, total, .. })) => {
            let pct = format!("{:.0}", *done as f64 * 100.0 / (*total).max(1) as f64);
            crate::i18n::fmt(tl!("Downloading {model} {pct}%"), &[("model", &models::file_title(m)), ("pct", &pct)])
        }
        _ => tl!("Starting the next download…").to_string(),
    };
    if queued > 0 {
        s.push_str(" · ");
        s.push_str(&crate::i18n::trn(crate::i18n::current(), queued as u64, "{n} more queued", "{n} more queued"));
    }
    s
}

/// One model's row.
fn row(ui: &mut egui::Ui, t: &Tokens, m: &'static ModelInfo, status: &Status, action: &mut Option<Action>) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(models::file_title(m)).strong());
        ui.label(egui::RichText::new(size(m.bytes)).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match status {
            Status::NotDownloaded | Status::Failed(_) => {
                let label = if matches!(status, Status::Failed(_)) { tl!("Retry") } else { tl!("Download") };
                if crate::widgets::secondary_button(ui, label, 84.0).clicked() {
                    *action = Some(Action::Download(m.id));
                }
            }
            Status::Queued | Status::Downloading { .. } => {
                if crate::widgets::secondary_button(ui, tl!("Cancel"), 84.0).clicked() {
                    *action = Some(Action::Cancel(Some(m.id)));
                }
            }
            Status::Downloaded => {
                if crate::widgets::secondary_button(ui, tl!("Delete"), 84.0).clicked() {
                    *action = Some(Action::Delete(m.id));
                }
            }
            Status::UpdateAvailable => {
                if crate::widgets::secondary_button(ui, tl!("Delete"), 70.0).clicked() {
                    *action = Some(Action::Delete(m.id));
                }
                if crate::widgets::secondary_button(ui, tl!("Update"), 70.0).clicked() {
                    *action = Some(Action::Download(m.id));
                }
            }
            Status::Checking | Status::Unavailable(_) => {}
        });
    });
    let dim = |s: String| egui::RichText::new(s).color(t.text_dim).size(12.0);
    ui.label(dim(crate::i18n::fmt(tl!("Used by: {tools}"), &[("tools", &tools_text(m))])));
    ui.label(dim(crate::i18n::fmt(tl!("Runs on: {where}"), &[("where", runs_text(m))])));
    ui.horizontal(|ui| {
        ui.label(dim(tl!("Licence:").to_string()));
        ui.hyperlink_to(egui::RichText::new(format!("{} · {}", m.license, m.author)).size(12.0), m.source);
    });
    let color = match status {
        Status::Failed(_) | Status::Unavailable(_) => t.warning,
        Status::Downloaded => t.text,
        _ => t.text_dim,
    };
    if let Status::Downloading { done, total, .. } = status {
        let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().min(360.0), 6.0), egui::Sense::hover());
        crate::jobs_ui::bar(ui, r, Some(*done as f32 / (*total).max(1) as f32), t);
    }
    ui.label(egui::RichText::new(status_text(status)).color(color).size(12.0));
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
        Some((m, Status::Downloading { done, total, .. })) => format!("↓ {} {:.0}%", models::file_title(m), *done as f64 * 100.0 / (*total).max(1) as f64),
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
    json!({"open": app.ui.ai_models.open, "highlight": app.ui.ai_models.highlight})
}

#[cfg(test)]
#[path = "ai_models_ui_tests.rs"]
mod tests;
