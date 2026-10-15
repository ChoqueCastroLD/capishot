//! Quality, Device and the Advanced settings of the local AI commands: Image › Upscale with AI
//! (in its dialog) and Clean with AI / the Remove tool's AI mode (in the AI Options… dialog).
//!
//! The simple part is two choices, **Quality** (Auto, Fast (lite), Best (full)) and **Device**
//! (Auto, GPU, CPU), plus one line saying what that resolves to on this machine and why, with the
//! model's download size ([`photocraft_ai::profile::choose`] on the shell's device profile):
//! quality is never lowered without saying so. **Advanced…** reveals the real settings: the exact
//! model, tile size and overlap (upscale) or the largest working side (inpaint), precision, CPU
//! threads, keeping the model in memory, and the downloaded models' storage with a button that
//! clears it.
//!
//! The choices are remembered in `prefs.dialogs["ai.upscale"]` / `["ai.inpaint"]` (only the
//! values that differ from the defaults) and every UI run of the commands gets them
//! ([`with_saved`], called by the download-consent gate all UI entry points go through), so the
//! menu item, the palette, the Remove tool's brush and its Clean Selection button agree.

use photocraft_ai::catalog::{Arch, Content, MODELS, ModelInfo};
use photocraft_ai::profile::Reason;
use photocraft_engine::ai_cmds::{self, RunChoice};
use photocraft_engine::ai_inpaint_cmds;
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::state::DialogKind;

/// Which command family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Upscale,
    Inpaint,
}

/// Marks the AI Options… dialog (Clean with AI and the Remove tool's AI mode).
const MARK: &str = "__aiInpaintOptions";
/// The dialog field that keeps Advanced open.
pub const ADVANCED: &str = "__aiAdvanced";
/// The `prefs.dialogs` key prefix of download consents (cleared with the downloads).
const CONSENT_PREFIX: &str = "aiModelDownload.";

impl Kind {
    /// The command family of a UI run, if it is an AI one.
    pub fn of(id: &str, params: &Value) -> Option<Self> {
        match id {
            ai_cmds::COMMAND => Some(Self::Upscale),
            ai_inpaint_cmds::ID => Some(Self::Inpaint),
            "paint.remove" if params.get("ai").and_then(Value::as_bool) == Some(true) => Some(Self::Inpaint),
            _ => None,
        }
    }

    fn pref(self) -> &'static str {
        match self {
            Self::Upscale => "ai.upscale",
            Self::Inpaint => "ai.inpaint",
        }
    }

    /// The params these settings own, with their defaults (a default is not stored or sent).
    fn defaults(self) -> &'static [(&'static str, Dflt)] {
        match self {
            Self::Upscale => &[
                ("quality", Dflt::Text("auto")),
                ("device", Dflt::Text("auto")),
                ("model", Dflt::Text("auto")),
                ("tile", Dflt::Num(0)),
                ("overlap", Dflt::Num(16)),
                ("threads", Dflt::Num(0)),
                ("keepLoaded", Dflt::Bool(true)),
            ],
            Self::Inpaint => &[
                ("quality", Dflt::Text("auto")),
                ("device", Dflt::Text("auto")),
                ("model", Dflt::Text("auto")),
                ("maxSide", Dflt::Num(0)),
                ("threads", Dflt::Num(0)),
                ("keepLoaded", Dflt::Bool(true)),
            ],
        }
    }
}

/// A setting's default value.
#[derive(Clone, Copy, Debug)]
enum Dflt {
    Text(&'static str),
    Num(u64),
    Bool(bool),
}

impl Dflt {
    fn json(self) -> Value {
        match self {
            Self::Text(s) => json!(s),
            Self::Num(n) => json!(n),
            Self::Bool(b) => json!(b),
        }
    }
    /// A value of the right type for this setting (anything else is dropped).
    fn accepts(self, v: &Value) -> bool {
        match self {
            Self::Text(_) => v.is_string(),
            Self::Num(_) => v.as_u64().is_some(),
            Self::Bool(_) => v.is_boolean(),
        }
    }
}

/// The saved settings (only non-default values).
pub fn saved(app: &PhotocraftApp, kind: Kind) -> Map<String, Value> {
    let stored = app.session.prefs().dialogs.get(kind.pref()).and_then(Value::as_object).cloned().unwrap_or_default();
    kind.defaults()
        .iter()
        .filter_map(|(k, d)| stored.get(*k).filter(|v| d.accepts(v) && **v != d.json()).map(|v| ((*k).to_string(), v.clone())))
        .collect()
}

/// The settings in dialog fields `f`, without the defaults.
fn settings_of(kind: Kind, f: &Map<String, Value>) -> Map<String, Value> {
    kind.defaults()
        .iter()
        .filter_map(|(k, d)| f.get(*k).filter(|v| d.accepts(v) && **v != d.json()).map(|v| ((*k).to_string(), v.clone())))
        .collect()
}

/// Remembers the settings in dialog fields `f` (and saves the preferences when they changed).
pub fn save(app: &mut PhotocraftApp, kind: Kind, f: &Map<String, Value>) {
    let new = Value::Object(settings_of(kind, f));
    if app.session.prefs().dialogs.get(kind.pref()) == Some(&new) {
        return;
    }
    let key = kind.pref();
    app.session.edit_prefs(|p| {
        p.dialogs.insert(key.into(), new);
    });
    if let Err(e) = crate::prefs_ui::save_preferences(app) {
        log::warn!("couldn't save the AI settings: {e}");
    }
}

/// `params` with the saved settings filled in where the run doesn't set them.
pub fn with_saved(app: &PhotocraftApp, id: &str, params: &Value) -> Value {
    let Some(kind) = Kind::of(id, params) else { return params.clone() };
    let mut p = if params.is_object() { params.clone() } else { json!({}) };
    if let Some(o) = p.as_object_mut() {
        for (k, v) in saved(app, kind) {
            o.entry(k).or_insert(v);
        }
    }
    p
}

/// Command params of the settings in dialog fields `f` (defaults omitted).
pub fn params_of(kind: Kind, f: &Map<String, Value>) -> Map<String, Value> {
    settings_of(kind, f)
}

/// Fills dialog fields `f` with the saved settings (defaults where none).
pub fn fill_fields(app: &PhotocraftApp, kind: Kind, f: &mut Map<String, Value>) {
    let saved = saved(app, kind);
    for (k, d) in kind.defaults() {
        f.insert((*k).into(), saved.get(*k).cloned().unwrap_or_else(|| d.json()));
    }
}

fn mb(m: &ModelInfo) -> String {
    let v = m.bytes as f64 / 1e6;
    if v < 10.0 { format!("{v:.1}") } else { format!("{v:.0}") }
}

/// The one line under Quality / Device: what runs, where, and why.
pub fn explanation(model: &ModelInfo, run: &RunChoice) -> String {
    let text = match run.choice.reason {
        Reason::Gpu => tl!("Your GPU runs the full model: {model} ({mb} MB)."),
        Reason::GpuUnknown => tl!("Full model: {model} ({mb} MB), on your GPU when there is one, else on the CPU."),
        Reason::NoGpu => tl!("No GPU available: the lite model {model} ({mb} MB) on the CPU. Choose Best for full quality (slower)."),
        Reason::SoftwareGpu => tl!("Only a software GPU: the lite model {model} ({mb} MB) on the CPU. Choose Best for full quality (slower)."),
        Reason::Mobile => tl!("Phone or tablet: the lite model {model} ({mb} MB), faster and lighter. Choose Best for full quality."),
        Reason::Weak => tl!("Little memory or few cores: the lite model {model} ({mb} MB). Choose Best for full quality."),
        Reason::UserFast => tl!("Fast: the lite model {model} ({mb} MB); lower quality than the full model."),
        Reason::UserBest if run.choice.cpu => tl!("Best on the CPU: the full model {model} ({mb} MB); this can take minutes per page."),
        Reason::UserBest => tl!("Best: the full model {model} ({mb} MB)."),
        Reason::UserCpu => tl!("CPU: the lite model {model} ({mb} MB). Choose Best for full quality (slower)."),
    };
    // A model picked by hand (Advanced) against the automatic tier is named as is.
    let text = if model.lite != run.choice.lite { tl!("Model chosen in Advanced: {model} ({mb} MB).") } else { text };
    crate::i18n::fmt(text, &[("model", model.title), ("mb", &mb(model))])
}

fn text_field(f: &Map<String, Value>, k: &str) -> String {
    f.get(k).and_then(Value::as_str).unwrap_or("auto").to_string()
}

fn num_field(f: &Map<String, Value>, k: &str, d: u64) -> f32 {
    f.get(k).and_then(Value::as_u64).unwrap_or(d) as f32
}

/// Quality and Device rows, for a two-column grid.
pub fn simple_rows(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let mut quality = text_field(f, "quality");
    let mut device = text_field(f, "device");
    ui.label(tl!("Quality:"));
    let q = [("auto".to_string(), tl!("Auto")), ("fast".to_string(), tl!("Fast (lite)")), ("best".to_string(), tl!("Best (full)"))];
    crate::widgets::dropdown(ui, "ai-opts-quality", &mut quality, &q, 200.0);
    ui.end_row();
    ui.label(tl!("Device:"));
    let d = [("auto".to_string(), tl!("Auto")), ("gpu".to_string(), tl!("GPU")), ("cpu".to_string(), tl!("CPU"))];
    crate::widgets::dropdown(ui, "ai-opts-device", &mut device, &d, 200.0);
    ui.end_row();
    f.insert("quality".into(), json!(quality));
    f.insert("device".into(), json!(device));
}

/// The Advanced… toggle and, when open, the Advanced settings.
pub fn advanced(app: &mut PhotocraftApp, ui: &mut egui::Ui, kind: Kind, f: &mut Map<String, Value>) {
    let open = f.get(ADVANCED).and_then(Value::as_bool).unwrap_or(false);
    ui.add_space(6.0);
    let label = if open { tl!("Hide Advanced") } else { tl!("Advanced…") };
    if crate::widgets::secondary_button(ui, label, 0.0).clicked() {
        f.insert(ADVANCED.into(), json!(!open));
    }
    if !open {
        return;
    }
    ui.add_space(6.0);
    let tokens = crate::theme::Tokens::for_kind(app.ui.theme);
    let mut model = text_field(f, "model");
    let mut threads = num_field(f, "threads", 0);
    let mut keep = f.get("keepLoaded").and_then(Value::as_bool).unwrap_or(true);
    egui::Grid::new(("ai-opts-advanced", kind as u8)).num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label(tl!("Model:"));
        // Automatic: Quality and Device decide (MangaJaNai's page-height rules for the full model).
        let auto = tl!("Automatic");
        let wanted = |m: &&ModelInfo| match kind {
            Kind::Upscale => matches!(m.arch, Arch::Esrgan | Arch::Compact),
            Kind::Inpaint => m.content == Content::Inpaint,
        };
        let models: Vec<(String, String)> = std::iter::once(("auto".to_string(), auto.to_string()))
            .chain(MODELS.iter().filter(wanted).map(|m| (m.id.to_string(), format!("{} · {} MB", m.title, mb(m)))))
            .collect();
        let opts: Vec<(String, &str)> = models.iter().map(|(a, b)| (a.clone(), b.as_str())).collect();
        crate::widgets::dropdown(ui, "ai-opts-model", &mut model, &opts, 280.0);
        ui.end_row();
        match kind {
            Kind::Upscale => {
                let mut tile = num_field(f, "tile", 0);
                let mut overlap = num_field(f, "overlap", 16);
                ui.label(tl!("Tile size:"));
                crate::widgets::value_field(ui, &mut tile, 0.0..=1024.0, "px", 70.0).on_hover_text(tl!("Input pixels per tile; 0 = automatic. Larger tiles are faster but need more memory."));
                ui.end_row();
                ui.label(tl!("Overlap:"));
                crate::widgets::value_field(ui, &mut overlap, 0.0..=128.0, "px", 70.0);
                ui.end_row();
                // 1..31 is not a valid tile: snap to automatic or the minimum.
                let tile = if tile < 1.0 { 0 } else { (tile as u64).max(32) };
                f.insert("tile".into(), json!(tile));
                f.insert("overlap".into(), json!(overlap as u64));
            }
            Kind::Inpaint => {
                let mut side = num_field(f, "maxSide", 0);
                ui.label(tl!("Max working side:"));
                crate::widgets::value_field(ui, &mut side, 0.0..=2048.0, "px", 70.0)
                    .on_hover_text(tl!("Larger areas are scaled down to this size for the model; 0 = automatic (512, at most 512 for MI-GAN)."));
                ui.end_row();
                let side = if side < 1.0 { 0 } else { ((side as u64).clamp(64, 2048) / 8) * 8 };
                f.insert("maxSide".into(), json!(side));
            }
        }
        ui.label(tl!("Precision:"));
        ui.label(egui::RichText::new(tl!("f32 (weights downloaded as f16; int8 isn't available)")).color(tokens.text_dim));
        ui.end_row();
        ui.label(tl!("CPU threads:"));
        crate::widgets::value_field(ui, &mut threads, 0.0..=64.0, "", 70.0).on_hover_text(tl!("0 = one per core."));
        ui.end_row();
        ui.label("");
        crate::widgets::checkbox(ui, &mut keep, tl!("Keep the model in memory between runs"));
        ui.end_row();
    });
    f.insert("model".into(), json!(model));
    f.insert("threads".into(), json!(threads as u64));
    f.insert("keepLoaded".into(), json!(keep));
    storage_row(app, ui);
}

/// Downloaded models' size and the button that clears them.
fn storage_row(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some(up) = app.session.ai_upscaler() else { return };
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let used = up.storage_used();
        let text = match used {
            Some(b) => crate::i18n::fmt(tl!("Downloaded models: {mb} MB"), &[("mb", &format!("{:.0}", b as f64 / 1e6))]),
            None => tl!("Downloaded models: unknown").to_string(),
        };
        ui.label(text);
        let any = used != Some(0);
        if ui.add_enabled_ui(any, |ui| crate::widgets::secondary_button(ui, tl!("Clear Downloaded Models"), 0.0)).inner.clicked() {
            clear_downloads(app);
        }
    });
    // The browser answers the size asynchronously.
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
}

/// Deletes the downloaded models and forgets the download consents (the next use asks again).
pub fn clear_downloads(app: &mut PhotocraftApp) {
    let Some(up) = app.session.ai_upscaler() else { return };
    match up.clear_downloads() {
        Ok(()) => {
            app.session.edit_prefs(|p| p.dialogs.retain(|k, _| !k.starts_with(CONSENT_PREFIX)));
            if let Err(e) = crate::prefs_ui::save_preferences(app) {
                log::warn!("couldn't save the preferences: {e}");
            }
            app.ui.status = tl!("Downloaded models cleared; they download again (after asking) when next used.").to_string();
            app.ui.status_error = false;
        }
        Err(e) => {
            app.ui.status = e.to_string();
            app.ui.status_error = true;
        }
    }
}

// ------------------------------------------------------------------ AI Options… (inpaint)

pub fn owns(fields: &Map<String, Value>) -> bool {
    fields.contains_key(MARK)
}

/// Opens AI Options… for Clean with AI and the Remove tool's AI mode.
pub fn open_inpaint_options(app: &mut PhotocraftApp) -> u64 {
    let mut f = Map::new();
    f.insert(MARK.into(), json!(true));
    f.insert("__label".into(), json!(tl!("AI Cleaning Options")));
    fill_fields(app, Kind::Inpaint, &mut f);
    app.ui.open_dialog(DialogKind::Command, f)
}

pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let tokens = crate::theme::Tokens::for_kind(app.ui.theme);
    ui.label(egui::RichText::new(tl!("Quality and device for Clean with AI and the Remove tool's AI mode.")).color(tokens.text_dim));
    ui.add_space(8.0);
    egui::Grid::new("ai-inpaint-opts").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| simple_rows(ui, f));
    ui.add_space(6.0);
    let params = Value::Object(params_of(Kind::Inpaint, f));
    match ai_inpaint_cmds::model_for(&app.session, &params, ai_inpaint_cmds::ID) {
        Ok((model, run)) => {
            ui.label(explanation(model, &run));
        }
        Err(e) => {
            ui.colored_label(tokens.warning, e.to_string());
        }
    }
    advanced(app, ui, Kind::Inpaint, f);
}

pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    save(app, Kind::Inpaint, f);
    Ok(Value::Object(params_of(Kind::Inpaint, f)))
}

#[cfg(test)]
#[path = "ai_options_ui_tests.rs"]
mod tests;
