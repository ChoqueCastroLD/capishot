//! The preview block (TypeR `previewBlock.jsx`): Paste, Align and text size −/+ on top, then the
//! current line with its number, style and text scale, previous / next line, and the line itself
//! drawn in its style's font. Clicking the line scrolls the script to it.

use egui::{RichText, Sense, Stroke, StrokeKind, vec2};
use serde_json::json;

use super::{act, chip, font_label, icon_label_button};
use crate::PhotocraftApp;
use crate::theme::Tokens;

/// Height of the line preview.
const LINE_H: f32 = 38.0;

pub(super) fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let typer = &app.session.typer;
    let settings = typer.state.settings.clone();
    let line = typer.state.current_line().cloned();
    let style = typer.state.current_style().cloned();
    let page = typer.state.current_page();
    let stored = typer.stored_selections.len();
    // Hold Alt while pasting or applying to keep the target layer's text size (TypeR 3.0).
    let keep_size = ui.input(|i| i.modifiers.alt);
    let keep = |mut p: serde_json::Value| {
        if keep_size {
            p["keepSize"] = json!(true);
        }
        p
    };

    // Paste · Align · size −/+
    ui.horizontal(|ui| {
        let paste_on = super::is_enabled(app, "typer.ui.paste").unwrap_or(false);
        let label = if settings.multi_bubble_mode && stored > 0 { format!("{} ({stored})", tl!("Paste")) } else { tl!("Paste").to_string() };
        let mut r = icon_label_button(ui, "square-dashed", &label, true, paste_on)
            .on_hover_text(tl!("Create a text layer from the current line in the centre of the selected area"));
        if !paste_on {
            r = r.on_hover_text(crate::shortcut_dispatch::disabled_reason(app, "typer.paste"));
        }
        if r.clicked() {
            let _ = super::invoke(app, "typer.ui.paste", &keep(json!({})));
        }
        let align_on = app.session.is_enabled("typer.align");
        let mut r = icon_label_button(ui, "focus", tl!("Align"), false, align_on)
            .on_hover_text(tl!("Centre the current text layer in the selected area, or in the bubble around it"));
        if !align_on {
            r = r.on_hover_text(crate::shortcut_dispatch::disabled_reason(app, "typer.align"));
        }
        if r.clicked() {
            act(app, "typer.align", json!({}));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let size_on = app.session.is_enabled("typer.resize");
            if ui
                .add_enabled_ui(size_on, |ui| crate::icons::button(ui, "circle-plus", 22.0, false, tl!("Increase the text size of the current layer")))
                .inner
                .clicked()
            {
                act(app, "typer.resize", json!({"delta": settings.text_size_increment}));
            }
            let mut step = settings.text_size_increment as f32;
            if crate::widgets::compact_value_field(ui, &mut step, 1.0..=99.0, "px", 44.0, 22.0).changed() {
                act(app, "typer.settings.set", json!({"key": "textSizeIncrement", "value": step}));
            }
            if ui
                .add_enabled_ui(size_on, |ui| crate::icons::button(ui, "circle-minus", 22.0, false, tl!("Decrease the text size of the current layer")))
                .inner
                .clicked()
            {
                act(app, "typer.resize", json!({"delta": -settings.text_size_increment}));
            }
        });
    });

    // Badges: keep size (Alt held), multi-bubble selections.
    if keep_size || settings.multi_bubble_mode {
        ui.horizontal(|ui| {
            if keep_size {
                chip(ui, tl!("Keep size"), true, &t).on_hover_text(tl!("Alt is held: Paste and Apply keep the layer's text size"));
            }
            if settings.multi_bubble_mode {
                let n = crate::i18n::trn(crate::i18n::current(), stored as u64, "{n} selection", "{n} selections");
                chip(ui, &n, stored > 0, &t).on_hover_text(tl!("Selections captured in multi-bubble mode; Paste fills them all"));
                let r = crate::icons::button(ui, "circle-minus", 20.0, false, tl!("Remove the last selection (right-click: clear all)"));
                if r.clicked() {
                    act(app, "typer.multiBubble.removeLast", json!({}));
                }
                if r.secondary_clicked() {
                    act(app, "typer.multiBubble.clear", json!({}));
                }
            }
        });
    }

    // ↑ ↓ · line N, style X, scale [ ] % · apply
    ui.horizontal(|ui| {
        if crate::icons::button(ui, "arrow-up", 22.0, false, tl!("Previous line")).clicked() {
            act(app, "typer.line.previous", json!({}));
        }
        if crate::icons::button(ui, "arrow-down", 22.0, false, tl!("Next line")).clicked() {
            act(app, "typer.line.next", json!({}));
        }
        let number = line.as_ref().filter(|l| l.index > 0).map_or_else(|| "—".to_string(), |l| l.index.to_string());
        let mut info = format!("{} {number}", tl!("line"));
        if let Some(p) = page {
            info.push_str(&format!(" · {} {p}", tl!("page")));
        }
        ui.label(RichText::new(info).color(t.text_dim).size(11.5));
        let name = style.as_ref().map_or("—", |s| s.name.as_str()).to_string();
        ui.label(RichText::new(name).color(t.text).size(11.5).strong()).on_hover_text(tl!("Style"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let apply_on = app.session.is_enabled("typer.apply");
            let mut r = ui
                .add_enabled_ui(apply_on, |ui| {
                    crate::icons::button(ui, "circle-arrow-right", 22.0, false, tl!("Apply the text and the style to the current layer"))
                })
                .inner;
            if !apply_on {
                r = r.on_disabled_hover_text(crate::shortcut_dispatch::disabled_reason(app, "typer.apply"));
            }
            if r.clicked() {
                act(app, "typer.apply", keep(json!({})));
            }
            ui.label(RichText::new("%").color(t.text_dim).size(11.5));
            let mut scale = settings.text_scale.unwrap_or(100.0) as f32;
            if crate::widgets::compact_value_field(ui, &mut scale, 1.0..=999.0, "", 42.0, 20.0)
                .on_hover_text(tl!("Text scale: pasted text is resized without changing the styles"))
                .changed()
            {
                act(app, "typer.settings.set", json!({"key": "textScale", "value": scale}));
            }
        });
    });

    // The line in its style's font.
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), LINE_H), Sense::click());
    ui.painter().rect_filled(rect, t.radius_sm, t.field);
    ui.painter().rect_stroke(rect, t.radius_sm, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    let inner = rect.shrink2(vec2(8.0, 4.0));
    let text = line.as_ref().map(|l| if settings.interpret_markdown { photocraft_typer::markdown::parse_runs(&l.text).text } else { l.text.clone() });
    match text.filter(|s| !s.trim().is_empty()) {
        Some(text) => {
            let family = style.as_ref().and_then(|s| font_label::family_of(ui.ctx(), s));
            let drawn = family.is_some_and(|f| font_label::paint(ui, &f, &text, 20.0, inner, t.text));
            if !drawn {
                let galley = ui.painter().layout(text, egui::FontId::proportional(14.0), t.text, inner.width());
                ui.painter().with_clip_rect(inner).galley(
                    egui::pos2(inner.left(), inner.center().y - galley.size().y.min(inner.height()) / 2.0),
                    galley,
                    t.text,
                );
            }
        }
        None => {
            let hint = if app.session.typer.state.text.trim().is_empty() { tl!("Paste the translated script below") } else { tl!("No line") };
            ui.painter().text(inner.left_center(), egui::Align2::LEFT_CENTER, hint, egui::FontId::proportional(12.0), t.text_faint);
        }
    }
    let resp = resp.on_hover_text(tl!("Click to scroll the script to this line"));
    if resp.clicked()
        && let Some(l) = &line
    {
        super::script_editor::scroll_to(ui.ctx(), l.raw_index);
    }
}
