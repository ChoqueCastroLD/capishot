//! Gradient Overlay's Gradient row: a swatch of the overlay's own stops whose click (or arrow)
//! opens the Gradients panel's presets (with Import Gradients…), and the Gradient Editor's stop
//! strip under it. Every edit lands in the instance's params (`stops`, `opacityStops`, and
//! `from`/`to` following the new ends), so the live preview shows it, Cancel drops it and OK
//! applies it as one `layer.layerStyle.replace` step.

use egui::{Sense, Stroke, StrokeKind, vec2};
use photocraft_doc::{Color, Effect, Fill, GradientStyle};
use photocraft_engine::gradient_fill_cmds as cmds;
use serde_json::{Map, Value, json};

use super::{defaults, entry, hex, set_param};
use crate::PhotocraftApp;
use crate::gradient_ui::StopAction;
use crate::theme::Tokens;

/// Dialog field naming the instance whose preset picker is open (automation can read it).
pub(super) const PICKER: &str = "__gradientPicker";
/// A colour-picker field of one colour stop: `stop:<index>` in location order.
const STOP_FIELD: &str = "stop:";

/// The effect `effect` as OK would apply it now: its snapshot edited by the params, or a new one.
fn resolved(f: &Map<String, Value>, effect: &str) -> Option<Effect> {
    let e = entry(f, effect)?;
    let kind = e.get("kind")?.as_str()?;
    let params = e.get("params").cloned().unwrap_or_else(|| json!({}));
    match e.get("fx").filter(|v| v.is_object()).and_then(|v| serde_json::from_value::<Effect>(v.clone()).ok()) {
        Some(mut fx) => {
            photocraft_engine::layer_style::overlay_effect(&mut fx, &params);
            Some(fx)
        }
        None => {
            let mut p = defaults(kind);
            if let (Some(o), Some(edits)) = (p.as_object_mut(), params.as_object()) {
                for (k, v) in edits {
                    o.insert(k.clone(), v.clone());
                }
            }
            photocraft_engine::layer_style::effect_from_params(kind, &p)
        }
    }
}

/// The editable colour stops (the recorded source stops when a method is set) and the opacity
/// stops of `effect`'s gradient overlay, sorted by location.
pub(super) fn stops(f: &Map<String, Value>, effect: &str) -> Option<(Vec<(f32, Color)>, Vec<(f32, f32)>)> {
    let Effect::GradientOverlay { gradient, interpolation, .. } = resolved(f, effect)? else { return None };
    let mut colors = interpolation.map_or(gradient.stops, |source| source.stops);
    colors.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut opacity = gradient.opacity_stops;
    opacity.sort_by(|a, b| a.0.total_cmp(&b.0));
    Some((colors, opacity))
}

/// Stores new colour stops (and opacity stops when given) on `effect`. `from`/`to` follow the new
/// ends, so the From/To swatches show them and their old values don't recolour the new ends.
pub(super) fn set_stops(f: &mut Map<String, Value>, effect: &str, colors: &[(f32, Color)], opacity: Option<&[(f32, f32)]>) {
    if colors.len() < 2 {
        return;
    }
    set_param(f, effect, "stops", Value::Array(colors.iter().map(|(t, c)| json!([t, c])).collect()));
    if let (Some(first), Some(last)) = (colors.first(), colors.last()) {
        set_param(f, effect, "from", json!(hex(&first.1)));
        set_param(f, effect, "to", json!(hex(&last.1)));
    }
    if let Some(opacity) = opacity {
        set_param(f, effect, "opacityStops", Value::Array(opacity.iter().map(|(t, a)| json!([t, a * 100.0])).collect()));
    }
}

/// Sets `effect`'s gradient to preset `name` of the Gradients panel (its colour stops, live
/// foreground/background resolved now; the overlay keeps its own opacity stops).
pub(super) fn pick_preset(app: &PhotocraftApp, f: &mut Map<String, Value>, effect: &str, name: &str) -> bool {
    let (fg, bg) = (app.session.tools.foreground, app.session.tools.background);
    let Some(preset) = app.session.presets.gradients.iter().flat_map(|g| g.items.iter()).find(|p| p.name == name) else { return false };
    let (colors, _) = preset.fill_stops(fg, bg);
    set_stops(f, effect, &colors, None);
    colors.len() >= 2
}

/// The colour-picker value of a `stop:<i>` field, or `None` when `field` is not one.
pub(super) fn stop_color(f: &Map<String, Value>, effect: &str, field: &str) -> Option<Result<[f32; 3], String>> {
    let index = field.strip_prefix(STOP_FIELD)?;
    Some((|| {
        let i: usize = index.parse().map_err(|_| "invalid gradient stop".to_string())?;
        let (colors, _) = stops(f, effect).ok_or("not a gradient overlay")?;
        colors.get(i).map(|s| s.1.to_rgb()).ok_or_else(|| "no such gradient stop".to_string())
    })())
}

/// Sets colour `field` of `effect` to `hex`: a plain parameter, or one gradient stop (`stop:<i>`,
/// keeping that stop's colour model and alpha).
pub(super) fn set_color_field(f: &mut Map<String, Value>, effect: &str, field: &str, hex: &str) {
    let Some(index) = field.strip_prefix(STOP_FIELD) else {
        set_param(f, effect, field, json!(hex));
        return;
    };
    let (Ok(i), Some(rgb)) = (index.parse::<usize>(), crate::color_picker_ui::parse_hex(hex)) else { return };
    let Some((mut colors, _)) = stops(f, effect) else { return };
    let Some(stop) = colors.get_mut(i) else { return };
    let mut c = Color::rgb(rgb[0], rgb[1], rgb[2]).in_mode(stop.1.mode);
    c.alpha = stop.1.alpha;
    stop.1 = c;
    set_stops(f, effect, &colors, None);
}

/// The stops as a gradient fill, the shape the Gradient Editor's strip edits.
fn fill_of(colors: &[(f32, Color)], opacity: &[(f32, f32)]) -> Fill {
    Fill::Gradient {
        stops: colors.to_vec(),
        angle: 0.0,
        scale: 1.0,
        style: GradientStyle::Linear,
        reverse: false,
        opacity_stops: opacity.to_vec(),
        midpoints: Vec::new(),
        offset: (0.0, 0.0),
        dither: false,
        align: true,
    }
}

/// RGBA stops for painting a swatch: colour and opacity stops merged.
fn paint_stops(colors: &[(f32, Color)], opacity: &[(f32, f32)]) -> Vec<(f32, [f32; 4])> {
    let rgba = colors
        .iter()
        .map(|(t, c)| {
            let [r, g, b] = c.to_rgb();
            (*t, [r, g, b, 1.0])
        })
        .collect();
    photocraft_engine::presets::gradients::apply_opacity(rgba, opacity)
}

/// The Gradient row of a Gradient Overlay page (`stacked`: the phone sheet).
pub(super) fn row(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>, effect: &str, label: &str, stacked: bool) {
    let t = Tokens::get(ui.ctx());
    let Some((colors, opacity)) = stops(f, effect) else { return };
    let was = f.get(PICKER).and_then(Value::as_str) == Some(effect);
    let mut open = was;
    let mut chosen: Option<String> = None;
    let mut import = false;
    super::field_row(ui, stacked, tl!(label), |ui| {
        let width = if stacked { ui.available_width().max(160.0) - 24.0 } else { 150.0 };
        let (r, swatch) = ui.allocate_exact_size(vec2(width, if stacked { 36.0 } else { 20.0 }), Sense::click());
        crate::preset_panels::paint_gradient(ui, r, &paint_stops(&colors, &opacity));
        let border = if swatch.hovered() { t.accent } else { t.field_border };
        ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, border), StrokeKind::Outside);
        swatch.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!("Gradient picker")));
        let (ar, arrow) = ui.allocate_exact_size(vec2(14.0, r.height()), Sense::click());
        let (cx, s) = (ar.center(), 3.2);
        let col = if arrow.hovered() { t.text } else { t.text_dim };
        ui.painter().line_segment([cx + vec2(-s, -s * 0.5), cx + vec2(0.0, s * 0.5)], Stroke::new(1.3, col));
        ui.painter().line_segment([cx + vec2(0.0, s * 0.5), cx + vec2(s, -s * 0.5)], Stroke::new(1.3, col));
        let anchor = swatch.union(arrow.clone());
        if swatch.on_hover_text(tl!("Click to pick a gradient preset")).clicked() || arrow.clicked() {
            open = !open;
        }
        let parent_layer = ui.layer_id();
        let (fg, bg) = (app.session.tools.foreground, app.session.tools.background);
        let groups = &app.session.presets.gradients;
        egui::Popup::from_response(&anchor).open_bool(&mut open).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            crate::press_menu::above_parent_ui(parent_layer, ui);
            ui.set_width(if stacked { 340.0 } else { 300.0 });
            egui::ScrollArea::vertical().max_height(280.0).id_salt(("layer-style-gradients", effect)).show(ui, |ui| {
                for group in groups {
                    ui.label(egui::RichText::new(&group.name).size(12.0).color(t.text_dim));
                    ui.add_space(3.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                        let size = if stacked { 44.0 } else { 32.0 };
                        for item in &group.items {
                            let (cell, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
                            crate::preset_panels::paint_gradient(ui, cell, &item.resolve(fg, bg));
                            let (stroke, kind) = if resp.hovered() {
                                (Stroke::new(1.5, t.accent), StrokeKind::Outside)
                            } else {
                                (Stroke::new(1.0, t.card_border), StrokeKind::Inside)
                            };
                            ui.painter().rect_stroke(cell, 2.0, stroke, kind);
                            if resp.on_hover_text(&item.name).clicked() {
                                chosen = Some(item.name.clone());
                            }
                        }
                    });
                    ui.add_space(8.0);
                }
            });
            ui.separator();
            if crate::widgets::secondary_button(ui, tl!("Import Gradients…"), 0.0).clicked() {
                import = true;
            }
        });
    });
    if let Some(name) = chosen {
        pick_preset(app, f, effect, &name);
        open = false;
    }
    if import {
        crate::preset_files_ui::import_gradients_dialog(app);
    }
    if open != was {
        if open {
            f.insert(PICKER.into(), json!(effect));
        } else {
            f.remove(PICKER);
        }
    }
    strip(app, ui, f, effect, &colors, &opacity);
    if colors.len() > 2 {
        ui.label(egui::RichText::new(tl!("From and To edit the first and last stops")).size(11.0).color(t.text_faint));
    }
}

/// The Gradient Editor's stop strip over the overlay's stops: drag to move, drag off to delete,
/// click under (or over) the bar to add a colour (or opacity) stop, double-click a colour stop for
/// the Color Picker. A drag shows in the strip and lands in the params on release.
fn strip(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>, effect: &str, colors: &[(f32, Color)], opacity: &[(f32, f32)]) {
    let fill = fill_of(colors, opacity);
    let key = egui::Id::unique(("layer-style-gradient-stops", effect));
    let pending_key = key.with("pending");
    let pending: Option<Value> = ui.data(|d| d.get_temp(pending_key));
    let (fg, bg) = (app.session.tools.foreground, app.session.tools.background);
    let width = ui.available_width().min(330.0);
    let action = ui.allocate_ui(vec2(width, 58.0), |ui| crate::gradient_ui::stop_strip(ui, key, &fill, false, pending.as_ref(), (fg, bg))).inner;
    match action {
        Some(StopAction::Preview(p)) => {
            ui.data_mut(|d| d.insert_temp(pending_key, p));
        }
        Some(StopAction::Commit(p)) => {
            ui.data_mut(|d| d.remove::<Value>(pending_key));
            apply_edit(f, effect, &fill, &p, (fg, bg));
        }
        Some(StopAction::Color(i)) => {
            f.insert(super::color_picker::REQUEST.into(), json!({"effect": effect, "field": format!("{STOP_FIELD}{i}")}));
        }
        None => {}
    }
}

/// One stop-strip edit (`gradient.fill.stop` grammar) applied to the overlay's stops.
pub(super) fn apply_edit(f: &mut Map<String, Value>, effect: &str, fill: &Fill, edit: &Value, colors: ([f32; 4], [f32; 4])) {
    let Ok(Fill::Gradient { mut stops, opacity_stops, .. }) = cmds::apply_stop(fill, edit, colors.0, colors.1) else { return };
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    let old_opacity = match fill {
        Fill::Gradient { opacity_stops, .. } => opacity_stops.as_slice(),
        _ => &[],
    };
    let opacity = (opacity_stops != old_opacity).then_some(opacity_stops.as_slice());
    set_stops(f, effect, &stops, opacity);
}

/// Test helper: the overlay's stops as a fill (what the strip edits).
#[cfg(test)]
pub(super) fn fill_for(f: &Map<String, Value>, effect: &str) -> Option<Fill> {
    stops(f, effect).map(|(c, o)| fill_of(&c, &o))
}
