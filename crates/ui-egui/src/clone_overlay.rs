//! The Clone Source overlay (Window › Clone Source › Show Overlay): the source image drawn under
//! the brush of the Clone Stamp and the Healing Brush, so the cleaner sees what a click will
//! paint before painting it. Photoshop's options, observed:
//!
//! - **Opacity** of the overlay, and its **mode** against the canvas: Normal, Darken, Lighten or
//!   Difference (Difference shows black where source and destination already line up).
//! - **Clipped**: only inside the brush tip (its real shape for sampled tips); off, a wide area
//!   around the pointer.
//! - **Auto Hide**: hidden while a stroke is painted.
//! - **Invert**: the source's colours inverted.
//!
//! It is transient (never part of the document render) and per pointer position: the source
//! window and, for the blend modes, the canvas under it are rendered for the brush's rectangle
//! only (bounded to 1024 × 1024 document pixels), never the whole page.

use egui::{Color32, Pos2, pos2};
use photocraft_engine::presets::clone_source::{CloneOverlay, Mapping, transform_matrix};
use photocraft_geom::Rect as DRect;

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::state::Tool;

/// Longest side of the unclipped overlay (document pixels).
const UNCLIPPED_SIDE: f64 = 1024.0;

pub(crate) struct ClonePreviewCache {
    pub(crate) key: String,
    texture: egui::TextureHandle,
}

/// The tools that show the overlay: the clone-source tools.
pub(crate) fn shows_overlay(tool: Tool) -> bool {
    matches!(tool, Tool::CloneStamp | Tool::Healing)
}

/// The overlay's colour for source `s` over canvas `u` (straight RGBA, document space) in the
/// panel's mode; the overlay's alpha stays the source's.
pub(crate) fn blend_pixel(mode: &str, u: [f32; 4], s: [f32; 4]) -> [f32; 4] {
    let f = |g: fn(f32, f32) -> f32| [g(u[0], s[0]), g(u[1], s[1]), g(u[2], s[2]), s[3]];
    match mode {
        "darken" => f(f32::min),
        "lighten" => f(f32::max),
        "difference" => f(|a, b| (a - b).abs()),
        _ => s,
    }
}

/// Alpha of the brush tip at each pixel of `rect` for a clipped overlay of a non-round tip
/// (sampled tips, an angle with roundness): coverage pushed to an outline so the overlay is
/// clipped by the tip's shape, not faded by its softness. `None` for round tips (a disc clip).
fn tip_mask(brush: &photocraft_engine::BrushSettings, at: [f64; 2]) -> Option<(DRect, Vec<f32>)> {
    use photocraft_engine::paint::{BrushContext, Dab, TipShape};
    use photocraft_geom::Point;
    if matches!(brush.tip, TipShape::Round) && brush.roundness >= 0.999 {
        return None;
    }
    let mut d = Dab::round(Point::new(at[0], at[1]), brush.size / 2.0, 1.0);
    (d.angle, d.roundness, d.flip_x, d.flip_y) = (brush.angle.to_radians(), brush.roundness, brush.flip_x, brush.flip_y);
    let ctx = BrushContext::new(brush);
    let r = ctx.dab_rect(&d, false);
    if r.is_empty() || r.width() > 1024 || r.height() > 1024 {
        return None;
    }
    let fp = ctx.footprint(&d, 0);
    Some((fp.rect, fp.cov.iter().map(|c| (c * 8.0).clamp(0.0, 1.0)).collect()))
}

/// Draw the overlay at document point `at` (the pointer).
pub(crate) fn draw(app: &mut PhotocraftApp, painter: &egui::Painter, xf: &ViewXform, at: [f64; 2], output: Option<u32>) {
    let overlay: CloneOverlay = app.session.presets.clone.overlay.clone();
    if !shows_overlay(app.ui.tool) || !overlay.show || (overlay.auto_hide && app.drag.is_some()) {
        return;
    }
    let slot = app.session.presets.clone.active().clone();
    let Some(source) = slot.source else { return };
    let radius = f64::from(app.session.tools.brush.size) / 2.0;
    if !radius.is_finite() || radius <= 0.0 || radius > 500.0 || !at.iter().all(|v| v.is_finite() && v.abs() < 1_000_000.0) {
        return;
    }
    let stroke_start = app.drag.as_ref().map(|d| d.start);
    let anchor = if app.ui.tool_options.clone_aligned { slot.anchor.or(stroke_start) } else { stroke_start }.unwrap_or(at);
    let map =
        Mapping { source: (source[0], source[1]), anchor: (anchor[0], anchor[1]), m: transform_matrix(slot.scale, slot.rotation, slot.flip_h, slot.flip_v) };
    let tip = if overlay.clipped { tip_mask(&app.session.tools.brush, at) } else { None };
    let rect = match (&tip, overlay.clipped) {
        (Some((r, _)), _) => *r,
        (None, true) => {
            DRect::new((at[0] - radius).floor() as i32, (at[1] - radius).floor() as i32, (at[0] + radius).ceil() as i32, (at[1] + radius).ceil() as i32)
        }
        // Unclipped: what of the view lies around the pointer, up to the overlay budget.
        (None, false) => {
            let h = UNCLIPPED_SIDE / 2.0;
            let around = DRect::new((at[0] - h).floor() as i32, (at[1] - h).floor() as i32, (at[0] + h).ceil() as i32, (at[1] + h).ceil() as i32);
            let [x0, y0] = xf.to_doc(xf.rect.min);
            let [x1, y1] = xf.to_doc(xf.rect.max);
            let view = DRect::new(x0.min(x1).floor() as i32, y0.min(y1).floor() as i32, x0.max(x1).ceil() as i32, y0.max(y1).ceil() as i32);
            let r = around.intersect(&view);
            if r.is_empty() { around } else { r }
        }
    };
    let Some(st) = app.session.active() else { return };
    let (display, display_key) = crate::canvas::canvas_display(app, &st.doc, output);
    let key = format!(
        "{:?}:{}:{:?}:{rect:?}:{map:?}:{}:{}:{}:{}:{}:{}",
        st.doc.id,
        st.revision,
        st.active_layer,
        app.ui.tool_options.clone_sample,
        display_key,
        overlay.invert,
        overlay.blend,
        tip.is_some(),
        app.session.tools.brush.angle
    );
    if app.clone_preview.as_ref().is_none_or(|c| c.key != key) {
        let Ok(mut buf) = photocraft_engine::retouch_cmds::clone_preview(&app.session, rect, &map, &app.ui.tool_options.clone_sample) else { return };
        if overlay.invert {
            for px in &mut buf.px {
                for c in px.iter_mut().take(3) {
                    *c = 1.0 - *c;
                }
            }
        }
        if overlay.blend != "normal" {
            let under = photocraft_compose::render(&st.doc, rect);
            for (s, u) in buf.px.iter_mut().zip(&under.px) {
                *s = blend_pixel(&overlay.blend, *u, *s);
            }
        }
        if let Some((_, mask)) = &tip {
            for (px, m) in buf.px.iter_mut().zip(mask) {
                px[3] *= m;
            }
        }
        let image = crate::canvas::display_image(display.as_deref(), &buf);
        match app.clone_preview.as_mut() {
            Some(cache) => {
                cache.texture.set(image, crate::canvas::canvas_texture_options(1));
                cache.key = key;
            }
            None => {
                app.clone_preview =
                    Some(ClonePreviewCache { key, texture: painter.ctx().load_texture("clone-preview", image, crate::canvas::canvas_texture_options(1)) })
            }
        }
    }
    let Some(cache) = &app.clone_preview else { return };
    let tint = Color32::from_white_alpha((overlay.opacity.clamp(0.0, 100.0) * 2.55).round() as u8);
    let mut mesh = egui::Mesh::with_texture(cache.texture.id());
    let vertex = |x: f64, y: f64| egui::epaint::Vertex {
        pos: xf.to_screen(x as f32, y as f32),
        uv: pos2((x - f64::from(rect.x0)) as f32 / rect.width() as f32, (y - f64::from(rect.y0)) as f32 / rect.height() as f32),
        color: tint,
    };
    if !overlay.clipped || tip.is_some() {
        // A quad: the whole area, or the tip's shape carried by the texture's alpha.
        for (x, y) in [(rect.x0, rect.y0), (rect.x1, rect.y0), (rect.x1, rect.y1), (rect.x0, rect.y1)] {
            mesh.vertices.push(vertex(f64::from(x), f64::from(y)));
        }
        mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
        painter.add(egui::Shape::mesh(mesh));
        return;
    }
    // A textured triangle fan provides a circular clip, including mirrored canvas views.
    mesh.vertices.push(vertex(at[0], at[1]));
    for i in 0..=64 {
        let angle = f64::from(i) * std::f64::consts::TAU / 64.0;
        mesh.vertices.push(vertex(at[0] + radius * angle.cos(), at[1] + radius * angle.sin()));
    }
    for i in 1..=64 {
        mesh.indices.extend_from_slice(&[0, i, i + 1]);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Photoshop's clone-source cursor while ⌥ is held with the Clone Stamp or Healing Brush (before
/// the ⌥-click that sets the source): a target, a small circle with a crosshair through it, in
/// place of the brush outline.
pub(crate) fn wants_target_cursor(tool: Tool, alt: bool, dragging: bool) -> bool {
    shows_overlay(tool) && alt && !dragging
}

/// Draw the target cursor at `p` (black under white so it reads on any page).
pub(crate) fn draw_target(painter: &egui::Painter, p: Pos2) -> egui::CursorIcon {
    let (r, arm) = (6.0, 11.0);
    for (w, c) in [(3.0, Color32::from_black_alpha(170)), (1.0, Color32::WHITE)] {
        let s = egui::Stroke::new(w, c);
        painter.circle_stroke(p, r, s);
        painter.line_segment([p - egui::vec2(arm, 0.0), p + egui::vec2(arm, 0.0)], s);
        painter.line_segment([p - egui::vec2(0.0, arm), p + egui::vec2(0.0, arm)], s);
    }
    egui::CursorIcon::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn overlay_modes_blend_against_the_canvas() {
        let (u, s) = ([0.2, 0.8, 0.5, 1.0], [0.6, 0.4, 0.5, 0.7]);
        assert_eq!(blend_pixel("normal", u, s), s);
        assert_eq!(blend_pixel("darken", u, s), [0.2, 0.4, 0.5, 0.7]);
        assert_eq!(blend_pixel("lighten", u, s), [0.6, 0.8, 0.5, 0.7]);
        let d = blend_pixel("difference", u, s);
        assert!((d[0] - 0.4).abs() < 1e-6 && (d[1] - 0.4).abs() < 1e-6 && d[2] == 0.0 && d[3] == 0.7);
    }

    #[test]
    fn the_target_cursor_shows_with_alt_before_a_stroke() {
        assert!(wants_target_cursor(Tool::CloneStamp, true, false));
        assert!(wants_target_cursor(Tool::Healing, true, false));
        assert!(!wants_target_cursor(Tool::CloneStamp, false, false));
        assert!(!wants_target_cursor(Tool::CloneStamp, true, true), "a stroke keeps its brush outline");
        assert!(!wants_target_cursor(Tool::Brush, true, false), "the Brush's ⌥ is the eyedropper");
    }

    fn overlay_mesh(app: &mut PhotocraftApp, ctx: &egui::Context, xf: &ViewXform, at: [f64; 2]) -> Option<egui::Mesh> {
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("clone-test")));
            draw(app, &painter, xf, at, None);
        });
        output.textures_delta.clear();
        output.shapes.into_iter().find_map(|s| match s.shape {
            egui::Shape::Mesh(m) => Some(std::sync::Arc::unwrap_or_clone(m)),
            _ => None,
        })
    }

    fn overlay_app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 256, "height": 256})).unwrap();
        app.run("cloneSource.set", json!({"source": [40, 40]})).unwrap();
        app.session.tools.brush.size = 20.0;
        app
    }

    #[test]
    fn the_overlay_follows_the_panel_options_for_both_clone_tools() {
        let ctx = egui::Context::default();
        let xf = ViewXform {
            rect: egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 200.0)),
            zoom: 1.0,
            center: [128.0, 128.0],
            flip: false,
            rotation: 0.0,
            aspect: 1.0,
        };
        for tool in [Tool::CloneStamp, Tool::Healing] {
            let mut app = overlay_app();
            app.ui.tool = tool;
            // Clipped (default): a disc of the brush's radius at the given opacity.
            app.run("cloneSource.overlay", json!({"opacity": 40})).unwrap();
            let m = overlay_mesh(&mut app, &ctx, &xf, [128.0, 128.0]).unwrap_or_else(|| panic!("{tool:?}: overlay drawn"));
            assert_eq!(m.indices.len(), 64 * 3, "{tool:?}");
            assert!(m.vertices.iter().all(|v| v.color.a() == 102), "{tool:?}: 40 % opacity");
            // Unclipped: a wide quad, larger than the brush, inside the view.
            app.run("cloneSource.overlay", json!({"clipped": false, "blend": "difference"})).unwrap();
            let m = overlay_mesh(&mut app, &ctx, &xf, [128.0, 128.0]).unwrap();
            assert_eq!(m.indices.len(), 6);
            let w = m.vertices.iter().map(|v| v.pos.x).fold(f32::MIN, f32::max) - m.vertices.iter().map(|v| v.pos.x).fold(f32::MAX, f32::min);
            assert!(w > 100.0 && w <= 201.0, "{tool:?}: unclipped spans the view ({w})");
            // Auto Hide: nothing while a stroke is painted.
            app.drag = Some(crate::canvas::Drag::new(tool, [128.0, 128.0], vec![[128.0, 128.0, 1.0]], egui::Modifiers::NONE, false));
            assert!(overlay_mesh(&mut app, &ctx, &xf, [128.0, 128.0]).is_none(), "{tool:?}: auto-hidden while painting");
            app.run("cloneSource.overlay", json!({"autoHide": false})).unwrap();
            assert!(overlay_mesh(&mut app, &ctx, &xf, [128.0, 128.0]).is_some(), "{tool:?}: shown while painting without Auto Hide");
        }
        // Other tools never show it.
        let mut app = overlay_app();
        app.ui.tool = Tool::Brush;
        assert!(overlay_mesh(&mut app, &ctx, &xf, [128.0, 128.0]).is_none());
    }

    #[test]
    fn a_clipped_overlay_takes_a_non_round_tip_shape() {
        let ctx = egui::Context::default();
        let xf = ViewXform {
            rect: egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 200.0)),
            zoom: 1.0,
            center: [128.0, 128.0],
            flip: false,
            rotation: 0.0,
            aspect: 1.0,
        };
        let mut app = overlay_app();
        app.ui.tool = Tool::CloneStamp;
        app.session.tools.brush.size = 30.0;
        app.session.tools.brush.roundness = 0.3;
        let (r, mask) = tip_mask(&app.session.tools.brush, [128.0, 128.0]).expect("an elliptical tip clips by its shape");
        let at = |x: i32, y: i32| mask[(y - r.y0) as usize * r.width() as usize + (x - r.x0) as usize];
        assert!(at(128, 128) > 0.99 && at(140, 128) > 0.99, "inside the ellipse");
        assert_eq!(at(128, 140), 0.0, "outside its narrow axis");
        let m = overlay_mesh(&mut app, &ctx, &xf, [128.0, 128.0]).unwrap();
        assert_eq!(m.indices.len(), 6, "a quad whose texture carries the tip");
        app.session.tools.brush.roundness = 1.0;
        assert!(tip_mask(&app.session.tools.brush, [128.0, 128.0]).is_none());
    }
}
