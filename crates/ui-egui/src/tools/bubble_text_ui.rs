//! The Bubble Text Remover tool (J group): a click or tap inside a speech bubble removes its text
//! without AI (`edit.removeBubbleText`), and its options bar (Sample All Layers, On a new layer,
//! the text tolerance: automatic or a colour difference).

use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::state::Tool;
use crate::theme::Tokens;

/// The tolerance a cleared Auto starts from.
const MANUAL_TOLERANCE: f32 = 48.0;

/// The click's parameters from the tool's options.
pub fn params(app: &PhotocraftApp, x: f64, y: f64) -> Value {
    let o = &app.ui.tool_options;
    let tol = if o.bubble_text_tolerance >= 1.0 { json!(o.bubble_text_tolerance.clamp(1.0, 255.0).round()) } else { json!("auto") };
    json!({"x": x.floor(), "y": y.floor(), "tolerance": tol, "sampleAll": o.sample_all.bubble_text, "newLayer": o.bubble_text_new_layer})
}

/// What a refusal tells the user (English, translated where it is shown).
pub fn refusal(e: &str) -> &'static str {
    if e.contains("notPlain") {
        photocraft_engine::bubble_text_cmds::NOT_PLAIN
    } else if e.contains("noBubble") {
        "No speech bubble here: tap inside one"
    } else if e.contains("noText") {
        "No text found in this bubble"
    } else if e.contains("locked") {
        "The layer is locked"
    } else {
        "Couldn't remove the text in this bubble"
    }
}

/// A click at document `(x, y)`. Returns false for other tools.
pub fn click(app: &mut PhotocraftApp, tool: Tool, x: f64, y: f64) -> bool {
    if tool != Tool::BubbleTextRemover {
        return false;
    }
    let p = params(app, x, y);
    match app.run(photocraft_engine::bubble_text_cmds::ID, p) {
        Err(e) => refuse(app, &e),
        Ok(_) => app.ui.notices.retain(|n| n.tag.as_deref() != Some(NOTICE_TAG)),
    }
    true
}

fn refuse(app: &mut PhotocraftApp, e: &str) {
    let why = refusal(e);
    app.ui.status = crate::i18n::t(why).to_string();
    app.ui.status_error = true;
    // A notice too: on a phone the status line is hidden.
    app.ui.notices.retain(|n| n.tag.as_deref() != Some(NOTICE_TAG));
    let id = crate::notices::post(app, why, Vec::new(), true, None);
    if let Some(n) = app.ui.notices.iter_mut().find(|n| n.id == id) {
        n.tag = Some(NOTICE_TAG.into());
    }
}

/// A click that ran as a background job (the browser threads build's worker pool) and was
/// refused: the same notice as at once. True when the event was the tool's refusal.
pub fn on_job_event(app: &mut PhotocraftApp, e: &photocraft_engine::jobs::JobEvent) -> bool {
    if e.command != photocraft_engine::bubble_text_cmds::ID {
        return false;
    }
    match &e.outcome {
        photocraft_engine::jobs::JobOutcome::Failed(err) => {
            refuse(app, err);
            true
        }
        _ => false,
    }
}

/// Tags the tool's refusal notice (one at a time).
pub const NOTICE_TAG: &str = "bubbleTextRemover";

fn opt(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    let label = ui.label(egui::RichText::new(crate::i18n::t(text)).color(t.text_dim).size(12.0));
    crate::widgets::scrub_option_label(ui, &label);
}

/// The options bar.
pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let o = &mut app.ui.tool_options;
    crate::widgets::checkbox(ui, &mut o.sample_all.bubble_text, tl!("Sample All Layers"));
    crate::widgets::checkbox(ui, &mut o.bubble_text_new_layer, tl!("On a new layer"));
    crate::widgets::vline(ui, 22.0);
    opt(ui, "Tolerance:");
    let mut auto = o.bubble_text_tolerance < 1.0;
    if crate::widgets::checkbox(ui, &mut auto, tl!("Auto"))
        .on_hover_text(tl!("How different from the bubble's background a pixel must be to count as text; Auto measures it from the bubble"))
        .clicked()
    {
        o.bubble_text_tolerance = if auto { 0.0 } else { MANUAL_TOLERANCE };
    }
    if !auto {
        crate::widgets::value_field(ui, &mut o.bubble_text_tolerance, 1.0..=255.0, "", 50.0);
    }
    crate::widgets::vline(ui, 22.0);
    opt(ui, "Click inside a speech bubble to remove its text");
}
