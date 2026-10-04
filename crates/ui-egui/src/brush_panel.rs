//! Window › Brush Settings (F5): Photoshop's floating brush panel. Section list with enable boxes
//! on the left, the selected section's controls on the right, and a live stroke preview below.
//! A second tab lists the brush presets. Edits go straight to the session brush (tool state, like
//! the options bar); presets are applied through `tools.setBrush`.

use std::sync::Arc;

use egui::{Color32, CornerRadius, RichText, Sense, Stroke, vec2};
use photocraft_engine::BrushSettings;
use photocraft_engine::paint::{self, Control, Dynamic, MaskMode, Pattern, PatternStyle, StrokePoint};
use serde_json::json;

use crate::theme::{self, Tokens};
use crate::{PhotocraftApp, icons, widgets};

/// Sections in Photoshop's order. The bool says whether the section has an enable box.
pub const SECTIONS: [(&str, bool); 13] = [
    ("Brush Tip Shape", false),
    ("Shape Dynamics", true),
    ("Scattering", true),
    ("Texture", true),
    ("Dual Brush", true),
    ("Color Dynamics", true),
    ("Transfer", true),
    ("Brush Pose", true),
    ("Noise", true),
    ("Wet Edges", true),
    ("Build-up", true),
    ("Smoothing", false),
    ("Protect Texture", true),
];

/// The enable flag behind section `i` (None for Brush Tip Shape).
pub fn section_flag(b: &mut BrushSettings, i: usize) -> Option<&mut bool> {
    Some(match i {
        1 => &mut b.shape_dynamics.enabled,
        2 => &mut b.scattering.enabled,
        3 => &mut b.texture.enabled,
        4 => &mut b.dual_brush.enabled,
        5 => &mut b.color_dynamics.enabled,
        6 => &mut b.transfer.enabled,
        7 => &mut b.pose.enabled,
        8 => &mut b.noise,
        9 => &mut b.wet_edges,
        10 => &mut b.build_up,
        11 => return None,
        12 => &mut b.protect_texture,
        _ => return None,
    })
}

const CONTROLS: [(Control, &str); 8] = [
    (Control::Off, "Off"),
    (Control::Fade, "Fade"),
    (Control::PenPressure, "Pen Pressure"),
    (Control::PenTilt, "Pen Tilt"),
    (Control::StylusWheel, "Stylus Wheel"),
    (Control::Rotation, "Rotation"),
    (Control::InitialDirection, "Initial Direction"),
    (Control::Direction, "Direction"),
];

const MASK_MODES: [(MaskMode, &str); 10] = [
    (MaskMode::Multiply, "Multiply"),
    (MaskMode::Subtract, "Subtract"),
    (MaskMode::Darken, "Darken"),
    (MaskMode::Overlay, "Overlay"),
    (MaskMode::ColorDodge, "Color Dodge"),
    (MaskMode::ColorBurn, "Color Burn"),
    (MaskMode::LinearBurn, "Linear Burn"),
    (MaskMode::HardMix, "Hard Mix"),
    (MaskMode::LinearHeight, "Linear Height"),
    (MaskMode::Height, "Height"),
];

/// Percent slider over a 0..`max` fraction.
fn pct(ui: &mut egui::Ui, label: &str, v: &mut f32, max: f32) -> bool {
    let mut p = *v * 100.0;
    let changed = widgets::slider_row(ui, label, &mut p, 0.0..=max * 100.0, "%", None).changed();
    if changed {
        *v = (p / 100.0).clamp(0.0, max);
    }
    changed
}

fn num(ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str) -> bool {
    widgets::slider_row(ui, label, v, range, suffix, None).changed()
}

/// A jitter slider plus its Control dropdown (and the Fade step count when fading).
fn dynamic(ui: &mut egui::Ui, id: &str, label: &str, d: &mut Dynamic, max: f32, minimum: Option<&str>) {
    let t = Tokens::get(ui.ctx());
    pct(ui, label, &mut d.jitter, max);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Control:").color(t.text_dim));
        widgets::dropdown(ui, id, &mut d.control, &CONTROLS, 130.0);
        if d.control == Control::Fade {
            let mut steps = d.fade_steps as f32;
            if widgets::value_field(ui, &mut steps, 1.0..=9999.0, "", 52.0).changed() {
                d.fade_steps = steps.round().clamp(1.0, 9999.0) as u32;
            }
        }
    });
    if let Some(m) = minimum {
        pct(ui, m, &mut d.minimum, 1.0);
    }
    ui.add_space(4.0);
}

/// Size: a value field over a logarithmic slider (small sizes get most of the travel).
fn size_row(ui: &mut egui::Ui, label: &str, size: &mut f32, max: f32) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            widgets::value_field(ui, size, 1.0..=max, "px", 74.0);
        });
    });
    let mut lv = size.max(1.0).ln();
    if widgets::slider(ui, &mut lv, 0.0..=max.ln(), None).changed() {
        *size = lv.exp().round().clamp(1.0, max);
    }
    ui.add_space(4.0);
}

fn tip_shape(ui: &mut egui::Ui, b: &mut BrushSettings) {
    size_row(ui, "Size", &mut b.size, 5000.0);
    ui.horizontal(|ui| {
        widgets::checkbox(ui, &mut b.flip_x, "Flip X");
        widgets::checkbox(ui, &mut b.flip_y, "Flip Y");
    });
    num(ui, "Angle", &mut b.angle, -180.0..=180.0, "°");
    pct(ui, "Roundness", &mut b.roundness, 1.0);
    if matches!(b.tip, paint::TipShape::Round) {
        pct(ui, "Hardness", &mut b.hardness, 1.0);
    }
    let mut spacing_on = b.spacing > 0.0;
    widgets::checkbox(ui, &mut spacing_on, "Spacing");
    if spacing_on {
        pct(ui, "Spacing", &mut b.spacing, 10.0);
        b.spacing = b.spacing.max(0.01);
    }
}

fn texture(ui: &mut egui::Ui, b: &mut BrushSettings) {
    let tx = &mut b.texture;
    let t = Tokens::get(ui.ctx());
    if let Pattern::Procedural { style, size, .. } = &mut tx.pattern {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Pattern").color(t.text_dim));
            let opts = [(PatternStyle::Paper, "Paper"), (PatternStyle::Canvas, "Canvas"), (PatternStyle::Noise, "Noise"), (PatternStyle::Dots, "Dots")];
            widgets::dropdown(ui, "brush-pattern", style, &opts, 110.0);
        });
        let mut s = *size as f32;
        if num(ui, "Pattern Size", &mut s, 16.0..=1024.0, "px") {
            *size = s.round() as u32;
        }
    } else {
        ui.label(RichText::new("Custom pattern").color(t.text_faint));
    }
    widgets::checkbox(ui, &mut tx.invert, "Invert");
    pct(ui, "Scale", &mut tx.scale, 10.0);
    let mut br = tx.brightness * 150.0;
    if num(ui, "Brightness", &mut br, -150.0..=150.0, "") {
        tx.brightness = br / 150.0;
    }
    let mut ct = tx.contrast * 50.0;
    if num(ui, "Contrast", &mut ct, -50.0..=100.0, "") {
        tx.contrast = ct / 50.0;
    }
    widgets::checkbox(ui, &mut tx.each_tip, "Texture Each Tip");
    ui.horizontal(|ui| {
        ui.label(RichText::new("Mode").color(t.text_dim));
        widgets::dropdown(ui, "brush-texture-mode", &mut tx.mode, &MASK_MODES, 130.0);
    });
    pct(ui, "Depth", &mut tx.depth, 1.0);
    if tx.each_tip {
        dynamic(ui, "brush-depth-ctl", "Depth Jitter", &mut tx.depth_jitter, 1.0, Some("Minimum Depth"));
    }
}

fn dual_brush(ui: &mut egui::Ui, b: &mut BrushSettings) {
    let t = Tokens::get(ui.ctx());
    let d = &mut b.dual_brush;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Mode").color(t.text_dim));
        widgets::dropdown(ui, "brush-dual-mode", &mut d.mode, &MASK_MODES, 130.0);
        widgets::checkbox(ui, &mut d.flip, "Flip");
    });
    size_row(ui, "Size", &mut d.size, 2500.0);
    pct(ui, "Hardness", &mut d.hardness, 1.0);
    pct(ui, "Spacing", &mut d.spacing, 10.0);
    pct(ui, "Scatter", &mut d.scatter, 10.0);
    widgets::checkbox(ui, &mut d.both_axes, "Both Axes");
    let mut c = d.count as f32;
    if num(ui, "Count", &mut c, 1.0..=16.0, "") {
        d.count = c.round() as u32;
    }
}

fn color_dynamics(ui: &mut egui::Ui, b: &mut BrushSettings) {
    let c = &mut b.color_dynamics;
    widgets::checkbox(ui, &mut c.per_tip, "Apply Per Tip");
    dynamic(ui, "brush-fgbg-ctl", "Foreground/Background Jitter", &mut c.fg_bg, 1.0, None);
    pct(ui, "Hue Jitter", &mut c.hue_jitter, 1.0);
    pct(ui, "Saturation Jitter", &mut c.saturation_jitter, 1.0);
    pct(ui, "Brightness Jitter", &mut c.brightness_jitter, 1.0);
    let mut p = c.purity * 100.0;
    if num(ui, "Purity", &mut p, -100.0..=100.0, "%") {
        c.purity = p / 100.0;
    }
}

fn pose(ui: &mut egui::Ui, b: &mut BrushSettings) {
    let p = &mut b.pose;
    let mut tx = p.tilt_x * 100.0;
    if num(ui, "Tilt X", &mut tx, -100.0..=100.0, "%") {
        p.tilt_x = tx / 100.0;
    }
    let mut ty = p.tilt_y * 100.0;
    if num(ui, "Tilt Y", &mut ty, -100.0..=100.0, "%") {
        p.tilt_y = ty / 100.0;
    }
    widgets::checkbox(ui, &mut p.override_tilt, "Override Tilt");
    num(ui, "Rotation", &mut p.rotation, 0.0..=360.0, "°");
    widgets::checkbox(ui, &mut p.override_rotation, "Override Rotation");
    pct(ui, "Pressure", &mut p.pressure, 1.0);
    widgets::checkbox(ui, &mut p.override_pressure, "Override Pressure");
}

fn smoothing(ui: &mut egui::Ui, b: &mut BrushSettings) {
    let s = &mut b.smoothing;
    pct(ui, "Smoothing", &mut s.amount, 1.0);
    widgets::checkbox(ui, &mut s.pulled_string, "Pulled String Mode");
    widgets::checkbox(ui, &mut s.catch_up, "Stroke Catch-up");
    widgets::checkbox(ui, &mut s.catch_up_on_end, "Catch-up on Stroke End");
    widgets::checkbox(ui, &mut s.adjust_for_zoom, "Adjust for Zoom");
}

/// Controls for section `i`.
pub fn section_body(ui: &mut egui::Ui, b: &mut BrushSettings, i: usize) {
    let t = Tokens::get(ui.ctx());
    let note = |ui: &mut egui::Ui, s: &str| {
        ui.label(RichText::new(s).color(t.text_faint));
    };
    match i {
        0 => tip_shape(ui, b),
        1 => {
            let sd = &mut b.shape_dynamics;
            dynamic(ui, "brush-size-ctl", "Size Jitter", &mut sd.size, 1.0, Some("Minimum Diameter"));
            dynamic(ui, "brush-angle-ctl", "Angle Jitter", &mut sd.angle, 1.0, None);
            dynamic(ui, "brush-round-ctl", "Roundness Jitter", &mut sd.roundness, 1.0, Some("Minimum Roundness"));
            widgets::checkbox(ui, &mut sd.flip_x_jitter, "Flip X Jitter");
            widgets::checkbox(ui, &mut sd.flip_y_jitter, "Flip Y Jitter");
        }
        2 => {
            let sc = &mut b.scattering;
            widgets::checkbox(ui, &mut sc.both_axes, "Both Axes");
            dynamic(ui, "brush-scatter-ctl", "Scatter", &mut sc.scatter, 10.0, None);
            let mut c = sc.count as f32;
            if num(ui, "Count", &mut c, 1.0..=16.0, "") {
                sc.count = c.round() as u32;
            }
            dynamic(ui, "brush-count-ctl", "Count Jitter", &mut sc.count_jitter, 1.0, None);
        }
        3 => texture(ui, b),
        4 => dual_brush(ui, b),
        5 => color_dynamics(ui, b),
        6 => {
            let tr = &mut b.transfer;
            dynamic(ui, "brush-opacity-ctl", "Opacity Jitter", &mut tr.opacity, 1.0, Some("Minimum"));
            dynamic(ui, "brush-flow-ctl", "Flow Jitter", &mut tr.flow, 1.0, Some("Minimum"));
        }
        7 => pose(ui, b),
        8 => note(ui, "Adds grain to the soft edges of the brush tip."),
        9 => note(ui, "Watercolour look: lighter interior, darker rims."),
        10 => {
            note(ui, "Airbrush: keeps painting while the pointer rests.");
            num(ui, "Rate", &mut b.build_up_rate, 1.0..=200.0, "/s");
        }
        11 => smoothing(ui, b),
        _ => note(ui, "Keeps the current texture when switching presets."),
    }
}

/// A stroke rendered with the brush (scaled down to fit), as RGBA8, for previews and preset rows.
pub fn preview_pixels(b: &BrushSettings, w: u32, h: u32, color: [f32; 4]) -> Vec<u8> {
    use photocraft_color::{ColorMode, PixelFormat, SampleType};
    let mut brush = b.clone();
    let k = ((h as f32 * 0.55) / brush.size.max(1.0)).min(1.0);
    brush.size = (brush.size * k).max(1.0);
    brush.dual_brush.size = (brush.dual_brush.size * k).max(1.0);
    brush.color = color;
    brush.erase = false;
    brush.seed = 7;
    // Photoshop's preview: an S-curve with pressure tapering in and out.
    let n = 64;
    let points: Vec<StrokePoint> = (0..=n)
        .map(|i| {
            let s = i as f64 / n as f64;
            let x = 10.0 + s * (w as f64 - 20.0);
            let y = h as f64 / 2.0 - (s * std::f64::consts::TAU).sin() * h as f64 * 0.22;
            StrokePoint::new(x, y, (s * std::f64::consts::PI).sin().max(0.05) as f32)
        })
        .collect();
    let fmt = PixelFormat::new(ColorMode::Rgb, SampleType::U8, true);
    let mut s = photocraft_raster::Surface::new(fmt);
    paint::apply_stroke(&mut s, &paint::Stroke { brush, points }, None, false);
    let r = photocraft_geom::Rect::new(0, 0, w as i32, h as i32);
    let mut px = vec![[0.0f32; 4]; (w * h) as usize];
    s.read_rgba_into(r, &mut px);
    px.iter().flat_map(|p| p.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)).collect()
}

/// Cheap signature of everything that changes a brush's preview. Sampled tips and pattern tiles
/// contribute their size and a strided sample instead of every pixel, so imported presets with
/// big tips don't serialise megabytes per frame.
pub fn preview_sig(b: &BrushSettings) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    fn js<T: serde::Serialize>(h: &mut DefaultHasher, v: &T) {
        serde_json::to_vec(v).unwrap_or_default().hash(h);
    }
    fn tile(h: &mut DefaultHasher, t: &paint::GrayTile) {
        (t.width, t.height).hash(h);
        let step = (t.data.len() / 4096).max(1);
        t.data.iter().step_by(step).for_each(|v| v.hash(h));
    }
    fn tip(h: &mut DefaultHasher, t: &paint::TipShape) {
        match t {
            paint::TipShape::Round => 0u8.hash(h),
            paint::TipShape::Sampled(g) => tile(h, g),
        }
    }
    let mut h = DefaultHasher::new();
    js(&mut h, &(b.size, b.hardness, b.spacing, b.opacity, b.flow, b.pressure_size, b.pressure_opacity, b.erase, b.mode, b.angle, b.roundness));
    js(&mut h, &(b.flip_x, b.flip_y, b.aliased, b.noise, b.wet_edges, b.build_up, b.build_up_rate, b.protect_texture, b.seed));
    js(&mut h, &(&b.shape_dynamics, &b.scattering, &b.color_dynamics, &b.transfer, &b.pose, &b.smoothing));
    tip(&mut h, &b.tip);
    let d = &b.dual_brush;
    js(&mut h, &(d.enabled, d.mode, d.size, d.hardness, d.roundness, d.angle, d.spacing, d.scatter, d.both_axes, d.count, d.flip));
    tip(&mut h, &d.tip);
    let t = &b.texture;
    js(&mut h, &(t.enabled, t.invert, t.scale, t.brightness, t.contrast, t.each_tip, t.mode, t.depth, t.depth_jitter));
    match &t.pattern {
        Pattern::Tile(g) => tile(&mut h, g),
        p => js(&mut h, p),
    }
    h.finish()
}

/// Texture for a brush preview, cached on [`preview_sig`].
fn preview_texture(ui: &egui::Ui, b: &BrushSettings, w: u32, h: u32, color: Color32) -> Arc<egui::TextureHandle> {
    let id = egui::Id::new(("brush-preview", preview_sig(b), w, h, color));
    if let Some(tex) = ui.data(|d| d.get_temp::<Arc<egui::TextureHandle>>(id)) {
        return tex;
    }
    let c = color.to_normalized_gamma_f32();
    let px = preview_pixels(b, w, h, [c[0], c[1], c[2], 1.0]);
    let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &px);
    let tex = Arc::new(ui.ctx().load_texture("brush-preview", img, egui::TextureOptions::LINEAR));
    ui.data_mut(|d| d.insert_temp(id, tex.clone()));
    tex
}

/// Group label for presets saved without a group.
pub const UNGROUPED: &str = "My Brushes";

/// Preset indices in panel order: groups in order of first appearance.
pub fn grouped_presets(presets: &[paint::BrushPreset]) -> Vec<(String, Vec<usize>)> {
    let mut out: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, p) in presets.iter().enumerate() {
        let g = if p.group.is_empty() { UNGROUPED } else { p.group.as_str() };
        match out.iter_mut().find(|(n, _)| n == g) {
            Some((_, v)) => v.push(i),
            None => out.push((g.to_string(), vec![i])),
        }
    }
    out
}

fn presets_tab(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{} presets", app.session.tools.presets.len())).color(t.text_faint));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Import Brushes…").on_hover_text("Load Photoshop brushes (.abr)").clicked() {
                app.open_dialog_file();
            }
        });
    });
    ui.add_space(4.0);
    let groups = grouped_presets(&app.session.tools.presets);
    // The current brush matches a preset when everything but its colour and size agrees.
    // Cheap fields first: the full comparison clones the preset (and its tip).
    let same = |a: &BrushSettings, b: &BrushSettings| {
        a.hardness == b.hardness
            && a.spacing == b.spacing
            && a.tip == b.tip
            && BrushSettings { color: b.color, size: b.size, background: b.background, ..a.clone() } == *b
    };
    let brush = &app.session.tools.brush;
    let mut clicked = None;
    egui::ScrollArea::vertical().id_salt("brush-presets").max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
        for (group, items) in groups {
            ui.add_space(4.0);
            ui.label(RichText::new(&group).font(theme::semibold(12.0)).color(t.text));
            ui.add_space(2.0);
            for i in items {
                let Some(p) = app.session.tools.presets.get(i) else { continue };
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                if !ui.is_rect_visible(r) {
                    continue;
                }
                if same(&p.brush, brush) {
                    ui.painter().rect_filled(r, t.radius_sm, t.accent_soft);
                } else if resp.hovered() {
                    ui.painter().rect_filled(r, t.radius_sm, t.hover);
                }
                let tex = preview_texture(ui, &p.brush, 180, 36, t.text);
                let ir = egui::Rect::from_min_size(r.left_top() + vec2(4.0, 2.0), vec2(180.0, 36.0));
                ui.painter().image(tex.id(), ir, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
                ui.painter().text(
                    egui::pos2(ir.right() + 10.0, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    &p.name,
                    egui::FontId::proportional(12.0),
                    t.text_dim,
                );
                if resp.clicked() {
                    clicked = Some(p.name.clone());
                }
            }
        }
    });
    if let Some(name) = clicked
        && let Err(e) = app.session.execute("tools.setBrush", json!({"preset": name}))
    {
        app.ui.status = e.to_string();
    }
}

pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ui.panels.brush_settings {
        return;
    }
    let t = Tokens::get(ctx);
    let frame = egui::Frame::NONE
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.card_border))
        .corner_radius(CornerRadius::same(t.radius_lg as u8))
        .shadow(egui::Shadow { offset: [0, 10], blur: 30, spread: 0, color: t.shadow })
        .inner_margin(egui::Margin::same(10));
    let canvas = app.last_canvas_rect;
    let mut open = true;
    egui::Window::new("Brush Settings")
        .id(egui::Id::new("brush-settings"))
        .title_bar(false)
        .resizable(false)
        .frame(frame)
        .default_pos(egui::pos2((canvas.right() - 500.0).max(canvas.left() + 8.0), canvas.top() + 40.0))
        .show(ctx, |ui| {
            ui.set_width(470.0);
            ui.horizontal(|ui| {
                let mut tab = app.ui.brush_tab;
                for (i, name) in ["Brush Settings", "Brushes"].iter().enumerate() {
                    if widgets::pill_tab(ui, name, tab == i).clicked() {
                        tab = i;
                    }
                }
                app.ui.brush_tab = tab;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, "x", 20.0, false, "Close").clicked() {
                        open = false;
                    }
                });
            });
            ui.add_space(6.0);
            widgets::hairline(ui);
            ui.add_space(6.0);
            if app.ui.brush_tab == 1 {
                presets_tab(app, ui);
                return;
            }
            let mut b = app.session.tools.brush.clone();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(150.0);
                    ui.spacing_mut().item_spacing.y = 1.0;
                    for (i, (name, has_box)) in SECTIONS.iter().enumerate() {
                        let sel = app.ui.brush_section == i;
                        let (r, resp) = ui.allocate_exact_size(vec2(150.0, 21.0), Sense::click());
                        if sel {
                            ui.painter().rect_filled(r, t.radius_sm, t.accent_soft);
                        } else if resp.hovered() {
                            ui.painter().rect_filled(r, t.radius_sm, t.hover);
                        }
                        let mut x = r.left() + 6.0;
                        if *has_box && let Some(flag) = section_flag(&mut b, i) {
                            let br = egui::Rect::from_center_size(egui::pos2(x + 6.0, r.center().y), vec2(12.0, 12.0));
                            let box_resp = ui.interact(br, ui.id().with(("brush-sec-box", i)), Sense::click());
                            if *flag {
                                ui.painter().rect_filled(br, 2.0, t.accent);
                                ui.painter().text(br.center(), egui::Align2::CENTER_CENTER, "✓", egui::FontId::proportional(10.0), Color32::WHITE);
                            } else {
                                ui.painter().rect_stroke(br, 2.0, Stroke::new(1.2, t.text_faint), egui::StrokeKind::Inside);
                            }
                            if box_resp.clicked() {
                                *flag = !*flag;
                            }
                            x += 18.0;
                        } else if i > 0 {
                            x += 18.0;
                        }
                        let color = if sel { t.text } else { t.text_dim };
                        ui.painter().text(egui::pos2(x, r.center().y), egui::Align2::LEFT_CENTER, *name, theme::medium(12.0), color);
                        if resp.clicked() {
                            app.ui.brush_section = i;
                        }
                    }
                });
                widgets::vline(ui, 350.0);
                ui.vertical(|ui| {
                    ui.set_width(290.0);
                    ui.label(RichText::new(SECTIONS[app.ui.brush_section.min(SECTIONS.len() - 1)].0).font(theme::semibold(12.5)).color(t.text));
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical().id_salt("brush-section").max_height(330.0).auto_shrink([false, true]).show(ui, |ui| {
                        section_body(ui, &mut b, app.ui.brush_section);
                    });
                });
            });
            ui.add_space(6.0);
            widgets::hairline(ui);
            ui.add_space(6.0);
            let tex = preview_texture(ui, &b, 450, 72, t.text);
            let (r, _) = ui.allocate_exact_size(vec2(450.0, 72.0), Sense::hover());
            ui.painter().rect_filled(r, t.radius_sm, t.field);
            ui.painter().image(tex.id(), r, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
            if b != app.session.tools.brush {
                app.session.tools.brush = b;
            }
        });
    if !open {
        app.ui.panels.brush_settings = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_with_boxes_have_flags() {
        let mut b = BrushSettings::default();
        for (i, (_, has_box)) in SECTIONS.iter().enumerate() {
            assert_eq!(section_flag(&mut b, i).is_some(), *has_box, "{i}");
        }
        *section_flag(&mut b, 3).unwrap() = true;
        assert!(b.texture.enabled);
    }

    #[test]
    fn preview_draws_a_stroke_that_fits() {
        let mut b = BrushSettings { size: 400.0, ..Default::default() };
        let px = preview_pixels(&b, 120, 40, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(px.len(), 120 * 40 * 4);
        let covered = px.as_chunks::<4>().0.iter().filter(|p| p[3] > 128).count();
        assert!(covered > 200 && covered < 120 * 40 / 2, "{covered}");
        // Dynamics change the preview.
        b.scattering.enabled = true;
        b.scattering.scatter.jitter = 3.0;
        assert_ne!(preview_pixels(&b, 120, 40, [1.0, 0.0, 0.0, 1.0]), px);
    }
}
