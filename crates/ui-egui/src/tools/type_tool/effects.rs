//! Efectos: quick layer effects for type, next to Contorno in the type bars (the phone's and the
//! Type and Bubble Type options bars). Three effects, each made or edited on the type target
//! (never a second copy), shown live, and one undo step per opening of the popover:
//!
//! - Degradado (Gradient Overlay): two colours, the angle, linear or radial, six presets;
//! - Sombra (Drop Shadow): colour, distance, size, angle, opacity;
//! - Resplandor (Outer Glow): colour, size, opacity.
//!
//! "More Options…" opens the Layer Style dialog at the effect.

use serde_json::Value;

use super::defaults::target;
use super::{Color32, LayerId, PhotocraftApp, Stroke, json};

/// The effects, as their `layer.layerStyle.<kind>` kinds, and their tab labels.
pub(crate) const KINDS: [(&str, &str); 3] = [("gradientOverlay", "Gradient"), ("dropShadow", "Shadow"), ("outerGlow", "Glow")];

/// The gradient presets: name and the two colours.
pub(crate) const PRESETS: [(&str, &str, &str); 6] = [
    ("Fire", "#ff3b1f", "#ffd23f"),
    ("Ocean", "#0a3d91", "#36d1dc"),
    ("Gold", "#8a5a00", "#ffe08a"),
    ("Purple", "#4b1d8f", "#d36bff"),
    ("Dark Red", "#ff2a2a", "#3a0000"),
    ("Black to Gray", "#000000", "#9a9a9a"),
];

/// An effect of a layer's style: its index in the style, its params, and whether it shows.
type Effect = (usize, Value, bool);

/// The target's effect of `kind`: the layer, and the effect when it has one.
fn current(app: &PhotocraftApp, kind: &str) -> Option<(u64, Option<Effect>)> {
    let (layer, _) = target(app)?;
    let st = app.session.active()?;
    let l = st.doc.layer(LayerId(layer))?;
    let light = st.doc.global_light.angle;
    let fx = l.effects.items.iter().enumerate().find_map(|(i, e)| {
        (photocraft_engine::layer_style::kind_of(e) == kind).then(|| (i, crate::layer_style::values_of(e, light), e.enabled() && l.effects.enabled))
    });
    Some((layer, fx))
}

/// What the popover shows for `kind`: the target's own effect (and whether it shows), else the
/// popover's starting values, off.
pub(crate) fn shown(app: &PhotocraftApp, kind: &str) -> Option<(u64, Value, bool)> {
    let (layer, fx) = current(app, kind)?;
    Some(match fx {
        Some((_, p, on)) => (layer, p, on),
        None => {
            let mut p = photocraft_engine::layer_style::effect_defaults(kind);
            match kind {
                "gradientOverlay" => {
                    p["from"] = json!(PRESETS[0].1);
                    p["to"] = json!(PRESETS[0].2);
                    p["method"] = json!("classic");
                }
                "dropShadow" => {
                    p["useGlobalLight"] = json!(false);
                    p["angle"] = json!(135);
                    p["distance"] = json!(4);
                    p["size"] = json!(4);
                }
                _ => {
                    p["size"] = json!(8);
                    p["color"] = json!("#ffffff");
                }
            }
            (layer, p, false)
        }
    })
}

/// Applies `params` as the target's effect of `kind` (made when missing, replaced when there,
/// hidden when `on` is false), in history step `key`.
pub(crate) fn set_effect(app: &mut PhotocraftApp, kind: &str, params: &Value, on: bool, key: &str) -> Result<(), String> {
    let Some((layer, fx)) = current(app, kind) else { return Ok(()) };
    if !on {
        if let Some((index, _, true)) = fx {
            app.run("layer.setEffectsVisible", json!({"layer": layer, "index": index, "visible": false, "coalesce": key}))?;
        }
        return Ok(());
    }
    let mut p = params.clone();
    p["layer"] = json!(layer);
    p["coalesce"] = json!(key);
    p["enabled"] = json!(true);
    if kind == "dropShadow" {
        // The popover's angle is the shadow's own, not the document's light.
        p["useGlobalLight"] = json!(false);
    }
    app.run(&format!("layer.layerStyle.{kind}"), p)?;
    if app.session.active().and_then(|st| st.doc.layer(LayerId(layer))).is_some_and(|l| !l.effects.enabled) {
        app.run("layer.setEffectsVisible", json!({"layer": layer, "visible": true, "coalesce": key}))?;
    }
    Ok(())
}

fn rgb(hex: &str) -> [f32; 3] {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    [((v >> 16) & 255) as f32 / 255.0, ((v >> 8) & 255) as f32 / 255.0, (v & 255) as f32 / 255.0]
}

fn color32(hex: &str) -> Color32 {
    let c = rgb(hex);
    let q = |v: f32| (v * 255.0).round() as u8;
    Color32::from_rgb(q(c[0]), q(c[1]), q(c[2]))
}

/// A swatch of `hex`, `side` points.
fn swatch(ui: &mut egui::Ui, hex: &str, side: f32, label: &str) -> egui::Response {
    let t = crate::theme::Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(side * 1.4, side), egui::Sense::click());
    ui.painter().rect_filled(rect, 6.0, color32(hex));
    ui.painter().rect_stroke(rect, 6.0, Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ColorWell, true, label));
    resp.on_hover_text(label)
}

/// A round chip painted from `from` to `to`.
fn gradient_chip(ui: &mut egui::Ui, from: &str, to: &str, side: f32, selected: bool, label: &str) -> egui::Response {
    let t = crate::theme::Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
    let (a, b) = (color32(from), color32(to));
    let n = 12;
    for i in 0..n {
        let f0 = i as f32 / n as f32;
        let f1 = (i + 1) as f32 / n as f32;
        let r = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + rect.height() * f0), egui::pos2(rect.right(), rect.top() + rect.height() * f1));
        let c = a.lerp_to_gamma(b, (f0 + f1) / 2.0);
        ui.painter().rect_filled(r.shrink2(egui::vec2(rect.width() * 0.08, 0.0)), 0.0, c);
    }
    ui.painter().rect_stroke(rect.shrink(1.0), side * 0.25, Stroke::new(if selected { 2.5 } else { 1.0 }, if selected { t.accent } else { t.card_border }), egui::StrokeKind::Inside);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    resp.on_hover_text(label)
}

/// The popover's state between frames: open, the tab, the undo step key.
fn ids() -> (egui::Id, egui::Id, egui::Id) {
    (egui::Id::unique("type-fx-open"), egui::Id::unique("type-fx-tab"), egui::Id::unique("type-fx-key"))
}

/// The type bars' Efectos button (phone: 44 pt) and its popover.
pub(crate) fn effects_control(app: &mut PhotocraftApp, ui: &mut egui::Ui, phone: bool) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let (open_id, _, key_id) = ids();
    let open = ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false));
    let side = if phone { 44.0 } else { 24.0 };
    let any_on = KINDS.iter().any(|(k, _)| shown(app, k).is_some_and(|s| s.2));
    let (rect, button) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
    if button.hovered() || open {
        ui.painter().rect_filled(rect, 6.0, t.hover);
    }
    if open {
        ui.painter().rect_stroke(rect, 6.0, Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
    }
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "fx", egui::FontId::proportional(side * 0.48), if any_on { t.accent } else { t.text });
    let button = button.on_hover_text(tl!("Effects"));
    button.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!("Effects")));
    crate::touch_ui::note_rect(ui.ctx(), "typeEffects", rect);
    if target(app).is_none() {
        return;
    }
    if button.clicked() {
        if !open {
            let n = ui.data_mut(|d| {
                let n = d.get_temp::<u64>(egui::Id::unique("type-fx-n")).unwrap_or(0) + 1;
                d.insert_temp(egui::Id::unique("type-fx-n"), n);
                n
            });
            ui.data_mut(|d| d.insert_temp(key_id, format!("type-fx-{n}")));
        }
        ui.data_mut(|d| d.insert_temp(open_id, !open));
    }
    if ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false)) {
        popover(app, ui, &button, phone);
    }
}

/// What a click in the popover asks for beyond a live edit.
enum Ask {
    Pick { kind: &'static str, key: &'static str, title: &'static str, color: String },
    More(&'static str),
}

fn popover(app: &mut PhotocraftApp, ui: &mut egui::Ui, button: &egui::Response, phone: bool) {
    let (open_id, tab_id, key_id) = ids();
    let key: String = ui.data(|d| d.get_temp(key_id)).unwrap_or_else(|| "type-fx".into());
    let mut tab: usize = ui.data(|d| d.get_temp(tab_id)).unwrap_or(0).min(KINDS.len() - 1);
    let kind = KINDS[tab].0;
    let Some((_, before, was_on)) = shown(app, kind) else {
        ui.data_mut(|d| d.insert_temp(open_id, false));
        return;
    };
    let mut p = before.clone();
    let mut on = was_on;
    let mut ask: Option<Ask> = None;
    let t = crate::theme::Tokens::get(ui.ctx());
    let screen = crate::safe_area::rect(ui.ctx());
    let w = if phone { (screen.width() - 24.0).min(320.0) } else { 280.0 };
    let x = button.rect.center().x.clamp(screen.left() + w / 2.0 + 8.0, (screen.right() - w / 2.0 - 8.0).max(screen.left() + w / 2.0 + 8.0));
    let (pivot, y) = if phone { (egui::Align2::CENTER_BOTTOM, button.rect.top() - 8.0) } else { (egui::Align2::CENTER_TOP, button.rect.bottom() + 6.0) };
    let h = if phone { 40.0 } else { 26.0 };
    let area = egui::Area::new(egui::Id::unique("type-fx")).order(egui::Order::Foreground).pivot(pivot).fixed_pos(egui::pos2(x, y)).show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).fill(t.chrome).show(ui, |ui| {
            ui.set_width(w);
            // Tabs: the three effects, a dot on the ones that show.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (i, (k, label)) in KINDS.iter().enumerate() {
                    let lit = shown(app, k).is_some_and(|s| s.2);
                    let text = if lit { format!("● {}", tl!(label)) } else { tl!(label).to_string() };
                    let r = ui.add_sized([(w - 8.0) / 3.0, h], egui::Button::new(text).selected(tab == i));
                    crate::touch_ui::note_rect(ui.ctx(), &format!("typeFxTab{i}"), r.rect);
                    if r.clicked() {
                        tab = i;
                    }
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!(KINDS[tab].1)).font(crate::theme::semibold(14.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.add_sized([64.0, h], egui::Button::new(if on { tl!("On") } else { tl!("Off") }).selected(on));
                    crate::touch_ui::note_rect(ui.ctx(), "typeFxOn", r.rect);
                    if r.clicked() {
                        on = !on;
                    }
                });
            });
            ui.add_space(4.0);
            ui.spacing_mut().slider_width = w - 90.0;
            let num = |p: &Value, k: &str, d: f64| p.get(k).and_then(Value::as_f64).unwrap_or(d) as f32;
            let slider = |ui: &mut egui::Ui, p: &mut Value, on: &mut bool, k: &str, label: &str, range: std::ops::RangeInclusive<f32>, suffix: &str, note: &str| {
                let mut v = num(p, k, *range.start() as f64);
                ui.label(egui::RichText::new(tl!(label)).color(t.text_dim));
                let r = ui.add(egui::Slider::new(&mut v, range).suffix(suffix));
                crate::touch_ui::note_rect(ui.ctx(), note, r.rect);
                if r.changed() {
                    p[k] = json!(v.round());
                    *on = true;
                }
            };
            match kind {
                "gradientOverlay" => {
                    ui.horizontal(|ui| {
                        let from = p["from"].as_str().unwrap_or("#000000").to_string();
                        let to = p["to"].as_str().unwrap_or("#ffffff").to_string();
                        let r = swatch(ui, &from, h, tl!("Gradient start color"));
                        crate::touch_ui::note_rect(ui.ctx(), "typeFxFrom", r.rect);
                        if r.clicked() {
                            ask = Some(Ask::Pick { kind: "gradientOverlay", key: "from", title: "Color Picker (Gradient)", color: from });
                        }
                        let r = swatch(ui, &to, h, tl!("Gradient end color"));
                        crate::touch_ui::note_rect(ui.ctx(), "typeFxTo", r.rect);
                        if r.clicked() {
                            ask = Some(Ask::Pick { kind: "gradientOverlay", key: "to", title: "Color Picker (Gradient)", color: to });
                        }
                        ui.add_space(8.0);
                        for (style, label, note) in [("linear", tl!("Linear"), "typeFxLinear"), ("radial", tl!("Radial"), "typeFxRadial")] {
                            let r = ui.add_sized([58.0, h], egui::Button::new(label).selected(p["style"].as_str() == Some(style)));
                            crate::touch_ui::note_rect(ui.ctx(), note, r.rect);
                            if r.clicked() {
                                p["style"] = json!(style);
                                on = true;
                            }
                        }
                    });
                    ui.add_space(4.0);
                    let mut a = num(&p, "angle", 90.0);
                    if crate::widgets::angle_dial_row(ui, "Angle", &mut a, -180.0..=180.0).changed() {
                        p["angle"] = json!(a.round());
                        on = true;
                    }
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        for (i, (name, from, to)) in PRESETS.iter().enumerate() {
                            let selected = p["from"].as_str() == Some(*from) && p["to"].as_str() == Some(*to);
                            let r = gradient_chip(ui, from, to, h, selected, tl!(name));
                            crate::touch_ui::note_rect(ui.ctx(), &format!("typeFxPreset{i}"), r.rect);
                            if r.clicked() {
                                p["from"] = json!(from);
                                p["to"] = json!(to);
                                on = true;
                            }
                        }
                    });
                }
                "dropShadow" => {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(tl!("Color")).color(t.text_dim));
                        let c = p["color"].as_str().unwrap_or("#000000").to_string();
                        let r = swatch(ui, &c, h, tl!("Shadow color"));
                        crate::touch_ui::note_rect(ui.ctx(), "typeFxColor", r.rect);
                        if r.clicked() {
                            ask = Some(Ask::Pick { kind: "dropShadow", key: "color", title: "Color Picker (Drop Shadow)", color: c });
                        }
                    });
                    slider(ui, &mut p, &mut on, "distance", "Distance", 0.0..=50.0, " px", "typeFxDistance");
                    slider(ui, &mut p, &mut on, "size", "Size", 0.0..=50.0, " px", "typeFxSize");
                    let mut a = num(&p, "angle", 135.0);
                    if crate::widgets::angle_dial_row(ui, "Angle", &mut a, -180.0..=180.0).changed() {
                        p["angle"] = json!(a.round());
                        p["useGlobalLight"] = json!(false);
                        on = true;
                    }
                    slider(ui, &mut p, &mut on, "opacity", "Opacity", 0.0..=100.0, " %", "typeFxOpacity");
                }
                _ => {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(tl!("Color")).color(t.text_dim));
                        let c = p["color"].as_str().unwrap_or("#ffffff").to_string();
                        let r = swatch(ui, &c, h, tl!("Glow color"));
                        crate::touch_ui::note_rect(ui.ctx(), "typeFxColor", r.rect);
                        if r.clicked() {
                            ask = Some(Ask::Pick { kind: "outerGlow", key: "color", title: "Color Picker (Outer Glow)", color: c });
                        }
                    });
                    slider(ui, &mut p, &mut on, "size", "Size", 0.0..=50.0, " px", "typeFxSize");
                    slider(ui, &mut p, &mut on, "opacity", "Opacity", 0.0..=100.0, " %", "typeFxOpacity");
                }
            }
            ui.add_space(6.0);
            let r = ui.add_sized([w, h], egui::Button::new(tl!("More Options…")));
            crate::touch_ui::note_rect(ui.ctx(), "typeFxMore", r.rect);
            if r.clicked() {
                ask = Some(Ask::More(KINDS[tab].0));
            }
        });
    });
    ui.data_mut(|d| d.insert_temp(tab_id, tab));
    // A tab switch shows the other effect: its values were read before the click.
    if KINDS[tab].0 == kind
        && (p != before || on != was_on)
        && let Err(e) = set_effect(app, kind, &p, on, &key)
    {
        app.ui.status = e;
        app.ui.status_error = true;
    }
    match ask {
        Some(Ask::Pick { kind, key: field, title, color }) => {
            // The picker's OK makes (or edits) the effect with the chosen colour.
            if let Some((layer, mut q, _)) = shown(app, kind) {
                if let Some(o) = q.as_object_mut() {
                    o.remove(field);
                }
                q["layer"] = json!(layer);
                q["enabled"] = json!(true);
                if kind == "dropShadow" {
                    q["useGlobalLight"] = json!(false);
                }
                crate::color_picker_ui::open_for_command_key(app, title, rgb(&color), &format!("layer.layerStyle.{kind}"), q, field);
            }
            ui.data_mut(|d| d.insert_temp(open_id, false));
            return;
        }
        Some(Ask::More(kind)) => {
            ui.data_mut(|d| d.insert_temp(open_id, false));
            if let Some((layer, _)) = target(app) {
                crate::layer_style::open_for_layer(app, LayerId(layer), Some(kind));
            }
            return;
        }
        None => {}
    }
    let outside = ui.input(|i| i.pointer.any_pressed())
        && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| !area.response.rect.contains(p) && !button.rect.contains(p));
    if outside || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.data_mut(|d| d.insert_temp(open_id, false));
    }
}

#[cfg(test)]
#[path = "effects_tests.rs"]
mod tests;
