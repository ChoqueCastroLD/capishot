//! Contorno: the type stroke control of the type bars (a Stroke layer effect).

use super::defaults::target;
use super::options_bar::styles_at;
use super::{Color32, LayerId, PhotocraftApp, Pos2, Stroke, hex, json};

// ------------------------------------------------------------------ Contorno: the type stroke

/// The type target's Stroke layer effect: the layer, and the effect (its index in the layer's
/// effects, the effect, and whether it shows) when it has one.
/// A layer's Stroke effect: its index in the effects, the effect, and whether it shows.
type LayerStroke = (usize, photocraft_doc::effects::StrokeFx, bool);

pub(crate) fn target_stroke(app: &PhotocraftApp) -> Option<(u64, Option<LayerStroke>)> {
    let (layer, _) = target(app)?;
    let st = app.session.active()?;
    let l = st.doc.layer(LayerId(layer))?;
    let fx = l.effects.items.iter().enumerate().find_map(|(i, e)| match e {
        photocraft_doc::Effect::Stroke(s) => Some((i, s.clone(), s.common.enabled && l.effects.enabled)),
        _ => None,
    });
    Some((layer, fx))
}

/// What the Contorno control edits: on, width (px), position and colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TypeStroke {
    pub on: bool,
    pub size: f32,
    pub position: photocraft_doc::effects::StrokePosition,
    pub color: [f32; 3],
}

/// The stroke shown for the target: its own, else (none yet) a 3 px outside stroke in the text
/// colour's opposite, off.
pub(crate) fn shown_stroke(app: &PhotocraftApp) -> Option<(u64, TypeStroke)> {
    let (layer, fx) = target_stroke(app)?;
    Some((
        layer,
        match fx {
            Some((_, s, visible)) => TypeStroke {
                on: visible,
                size: s.size,
                position: s.position,
                color: match &s.paint {
                    photocraft_doc::effects::FxPaint::Color(c) => c.to_rgb(),
                    _ => [0.0, 0.0, 0.0],
                },
            },
            None => TypeStroke { on: false, size: 3.0, position: photocraft_doc::effects::StrokePosition::Outside, color: text_inverse(app) },
        },
    ))
}

/// The opposite of the target's text colour (white for black text): a quick stroke colour.
fn text_inverse(app: &PhotocraftApp) -> [f32; 3] {
    let c = styles_at(app).map_or_else(
        || {
            let f = app.session.tools.foreground;
            [f[0], f[1], f[2]]
        },
        |(c, _)| c.color.to_rgb(),
    );
    [1.0 - c[0], 1.0 - c[1], 1.0 - c[2]]
}

fn position_name(p: photocraft_doc::effects::StrokePosition) -> &'static str {
    match p {
        photocraft_doc::effects::StrokePosition::Outside => "outside",
        photocraft_doc::effects::StrokePosition::Inside => "inside",
        photocraft_doc::effects::StrokePosition::Center => "center",
    }
}

/// The `layer.layerStyle.stroke` params for `s` on `layer`, keeping what the control doesn't show
/// of an existing stroke (opacity, blend mode), in history step `key`.
fn stroke_params(app: &PhotocraftApp, layer: u64, s: &TypeStroke, key: &str) -> serde_json::Value {
    let mut p = json!({"layer": layer, "size": s.size.clamp(0.0, 250.0), "position": position_name(s.position), "color": hex([s.color[0], s.color[1], s.color[2], 1.0]), "coalesce": key});
    if let Some((_, Some((_, fx, _)))) = target_stroke(app) {
        p["opacity"] = json!((fx.common.opacity * 100.0).round());
        p["blend"] = json!(fx.common.blend.label());
    }
    p
}

/// Applies `s` to the target layer's Stroke effect (made when missing, edited when there, hidden
/// when off): one undo step per `key` (a popover session).
pub(crate) fn set_type_stroke(app: &mut PhotocraftApp, s: TypeStroke, key: &str) -> Result<(), String> {
    let Some((layer, fx)) = target_stroke(app) else { return Ok(()) };
    if !s.on {
        if let Some((index, _, true)) = fx {
            app.run("layer.setEffectsVisible", json!({"layer": layer, "index": index, "visible": false, "coalesce": key}))?;
        }
        return Ok(());
    }
    let p = stroke_params(app, layer, &s, key);
    app.run("layer.layerStyle.stroke", p)?;
    // The effects list's own switch (the Layers panel's "Effects" eye) shows it too.
    if app.session.active().and_then(|st| st.doc.layer(LayerId(layer))).is_some_and(|l| !l.effects.enabled) {
        app.run("layer.setEffectsVisible", json!({"layer": layer, "visible": true, "coalesce": key}))?;
    }
    Ok(())
}

/// The quick colours: white, black, red, yellow, blue, then the text colour's opposite.
fn stroke_chips(app: &PhotocraftApp) -> [[f32; 3]; 6] {
    [[1.0, 1.0, 1.0], [0.0, 0.0, 0.0], [0.9, 0.16, 0.16], [1.0, 0.85, 0.1], [0.12, 0.45, 0.9], text_inverse(app)]
}

/// A ringed swatch of the stroke colour: a ring in it, slashed when there's no stroke.
fn stroke_swatch(ui: &mut egui::Ui, side: f32, s: Option<&TypeStroke>) -> egui::Response {
    let t = crate::theme::Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let c = center_radius(rect);
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, t.hover);
    }
    match s.filter(|s| s.on) {
        Some(s) => {
            let col = Color32::from_rgb(q(s.color[0]), q(s.color[1]), q(s.color[2]));
            ui.painter().circle_stroke(c.0, c.1, Stroke::new((c.1 * 0.4).max(3.0), col));
            ui.painter().circle_stroke(c.0, c.1 + c.1 * 0.2, Stroke::new(1.0, t.card_border));
        }
        None => {
            ui.painter().circle_stroke(c.0, c.1, Stroke::new(1.5, t.text_faint));
            ui.painter().line_segment([c.0 + egui::vec2(-c.1, c.1), c.0 + egui::vec2(c.1, -c.1)], Stroke::new(1.5, t.text_faint));
        }
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!("Outline")));
    resp.on_hover_text(tl!("Outline"))
}

fn center_radius(rect: egui::Rect) -> (Pos2, f32) {
    (rect.center(), rect.width().min(rect.height()) * 0.3)
}

/// The Contorno popover: on/off, the colour (the picker and quick chips), the width (0-20 px
/// slider and ±) and the position (outside, centred, inside), applied live to the edited layer
/// as one undo step per opening. `phone`: finger-sized, above the button.
fn stroke_popover(app: &mut PhotocraftApp, ui: &mut egui::Ui, button: &egui::Response, phone: bool) {
    let open_id = egui::Id::unique("type-stroke-open");
    let key_id = egui::Id::unique("type-stroke-key");
    let open = ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false));
    if button.clicked() {
        if !open {
            // A new undo step for this opening.
            let n = ui.data_mut(|d| {
                let n = d.get_temp::<u64>(egui::Id::unique("type-stroke-n")).unwrap_or(0) + 1;
                d.insert_temp(egui::Id::unique("type-stroke-n"), n);
                n
            });
            ui.data_mut(|d| d.insert_temp(key_id, format!("type-stroke-{n}")));
        }
        ui.data_mut(|d| d.insert_temp(open_id, !open));
    }
    if !ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false)) {
        return;
    }
    let Some((layer, mut s)) = shown_stroke(app) else {
        ui.data_mut(|d| d.insert_temp(open_id, false));
        return;
    };
    let key: String = ui.data(|d| d.get_temp(key_id)).unwrap_or_else(|| "type-stroke".into());
    let before = s;
    let t = crate::theme::Tokens::get(ui.ctx());
    let screen = crate::safe_area::rect(ui.ctx());
    let w = if phone { 300.0 } else { 260.0 };
    let x = button.rect.center().x.clamp(screen.left() + w / 2.0 + 8.0, (screen.right() - w / 2.0 - 8.0).max(screen.left() + w / 2.0 + 8.0));
    let (pivot, y) = if phone { (egui::Align2::CENTER_BOTTOM, button.rect.top() - 8.0) } else { (egui::Align2::CENTER_TOP, button.rect.bottom() + 6.0) };
    let h = if phone { 40.0 } else { 26.0 };
    let mut pick = false;
    let area = egui::Area::new(egui::Id::unique("type-stroke")).order(egui::Order::Foreground).pivot(pivot).fixed_pos(egui::pos2(x, y)).show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).fill(t.chrome).show(ui, |ui| {
            ui.set_width(w);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!("Outline")).font(crate::theme::semibold(14.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.add_sized([64.0, h], egui::Button::new(if s.on { tl!("On") } else { tl!("Off") }).selected(s.on));
                    crate::touch_ui::note_rect(ui.ctx(), "typeStrokeOn", r.rect);
                    if r.clicked() {
                        s.on = !s.on;
                    }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let r = stroke_swatch(ui, h, Some(&TypeStroke { on: true, ..s }));
                crate::touch_ui::note_rect(ui.ctx(), "typeStrokeColor", r.rect);
                if r.clicked() {
                    pick = true;
                }
                for (i, c) in stroke_chips(app).into_iter().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(h * 0.8, h * 0.8), egui::Sense::click());
                    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                    let on = s.on && (0..3).all(|k| (s.color[k] - c[k]).abs() < 0.01);
                    ui.painter().circle(
                        rect.center(),
                        rect.width() * 0.42,
                        Color32::from_rgb(q(c[0]), q(c[1]), q(c[2])),
                        Stroke::new(if on { 2.5 } else { 1.0 }, if on { t.accent } else { t.card_border }),
                    );
                    crate::touch_ui::note_rect(ui.ctx(), &format!("typeStrokeChip{i}"), rect);
                    if resp.clicked() {
                        s.color = c;
                        s.on = true;
                    }
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.spacing_mut().slider_width = w - 3.0 * 40.0 - 40.0;
                let r = ui.add_sized([36.0, h], egui::Button::new("−"));
                crate::touch_ui::note_rect(ui.ctx(), "typeStrokeMinus", r.rect);
                if r.clicked() {
                    s.size = (s.size - 1.0).max(0.0).round();
                    s.on = true;
                }
                let mut v = s.size.min(20.0);
                let r = ui.add(egui::Slider::new(&mut v, 0.0..=20.0).step_by(0.5).suffix(" px")).on_hover_text(tl!("Outline width"));
                crate::touch_ui::note_rect(ui.ctx(), "typeStrokeWidth", r.rect);
                if r.changed() {
                    s.size = v;
                    s.on = true;
                }
                let r = ui.add_sized([36.0, h], egui::Button::new("+"));
                crate::touch_ui::note_rect(ui.ctx(), "typeStrokePlus", r.rect);
                if r.clicked() {
                    s.size = (s.size + 1.0).min(250.0).round();
                    s.on = true;
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                use photocraft_doc::effects::StrokePosition as P;
                for (p, label, key) in [
                    (P::Outside, tl!("Outer"), "typeStrokeOutside"),
                    (P::Center, tl!("Centered"), "typeStrokeCenter"),
                    (P::Inside, tl!("Inner"), "typeStrokeInside"),
                ] {
                    let r = ui.add_sized([(w - 12.0) / 3.0, h], egui::Button::new(label).selected(s.position == p));
                    crate::touch_ui::note_rect(ui.ctx(), key, r.rect);
                    if r.clicked() {
                        s.position = p;
                        s.on = true;
                    }
                }
            });
        });
    });
    if s != before
        && let Err(e) = set_type_stroke(app, s, &key)
    {
        app.ui.status = e;
        app.ui.status_error = true;
    }
    if pick {
        let mut p = stroke_params(app, layer, &TypeStroke { on: true, ..s }, &key);
        if let Some(o) = p.as_object_mut() {
            o.remove("color");
        }
        crate::color_picker_ui::open_for_command(app, "Color Picker (Outline)", s.color, "layer.layerStyle.stroke", p);
        ui.data_mut(|d| d.insert_temp(open_id, false));
        return;
    }
    let outside = ui.input(|i| i.pointer.any_pressed())
        && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| !area.response.rect.contains(p) && !button.rect.contains(p));
    if outside || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.data_mut(|d| d.insert_temp(open_id, false));
    }
}

/// The type bars' Contorno button (phone: 44 px, next to the text colour) and its popover.
pub(crate) fn stroke_control(app: &mut PhotocraftApp, ui: &mut egui::Ui, phone: bool) {
    let s = shown_stroke(app).map(|(_, s)| s);
    let open = ui.data(|d| d.get_temp::<bool>(egui::Id::unique("type-stroke-open")).unwrap_or(false));
    let side = if phone { 44.0 } else { 24.0 };
    let button = stroke_swatch(ui, side, s.as_ref());
    if open {
        ui.painter().rect_stroke(button.rect, 6.0, Stroke::new(1.5, crate::theme::Tokens::get(ui.ctx()).accent), egui::StrokeKind::Inside);
    }
    crate::touch_ui::note_rect(ui.ctx(), "typeStroke", button.rect);
    if s.is_none() {
        // No type layer to edit yet.
        return;
    }
    stroke_popover(app, ui, &button, phone);
}
