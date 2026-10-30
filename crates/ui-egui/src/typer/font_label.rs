//! Text drawn in a TypeR style's font (style names, the preview line), rendered by the same
//! text engine as document type. A family the font database doesn't have falls back to the UI
//! font, as TypeR's CSS preview does.

use std::collections::VecDeque;
use std::sync::PoisonError;

use egui::{ColorImage, Rect, TextureHandle, TextureOptions, pos2, vec2};
use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::TextLayer;
use photocraft_geom::Affine;

const CACHE_ID: &str = "typer-font-labels";
const CACHE_LIMIT: usize = 96;
/// Rendered labels are capped (pixels) so a pasted chapter in the preview stays cheap.
const MAX_W: usize = 2400;
const MAX_H: usize = 240;
/// Characters rendered at most (the preview shows one line).
const MAX_CHARS: usize = 160;

/// Family, text, point size, display scale and the font database generations (so a font that
/// arrives later replaces the fallback).
type Key = (String, String, u32, u32, (u64, u64));

/// A rendered label and its size in points.
type Label = (TextureHandle, [f32; 2]);

#[derive(Clone, Default)]
struct Cache(VecDeque<(Key, Option<Label>)>);

/// The family the font database knows for a TypeR style (its family name, else what its
/// PostScript name resolves to), or `None`.
/// Answers are cached per (family name, PostScript name) and font database generation.
pub(crate) fn family_of(ctx: &egui::Context, style: &photocraft_typer::Style) -> Option<String> {
    let ts = style.text_style()?;
    let name = ts.get("fontName").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let ps = ts.get("fontPostScriptName").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let key = (name, ps, (photocraft_text::fonts::generation(), photocraft_text::served::generation()));
    let id = egui::Id::unique("typer-font-families");
    type Families = std::collections::HashMap<(String, String, (u64, u64)), Option<String>>;
    if let Some(hit) = ctx.data_mut(|d| d.get_temp_mut_or_default::<Families>(id).get(&key).cloned()) {
        return hit;
    }
    let found = {
        let mut engine = photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner);
        if !key.0.is_empty() && engine.fonts.has_family(&key.0) {
            Some(key.0.clone())
        } else if key.1.is_empty() {
            None
        } else {
            let family = engine.fonts.resolve_postscript(&key.1).family;
            engine.fonts.has_family(&family).then_some(family)
        }
    };
    ctx.data_mut(|d| {
        let map = d.get_temp_mut_or_default::<Families>(id);
        if map.len() > 512 {
            map.clear();
        }
        map.insert(key, found.clone());
    });
    found
}

fn render(family: &str, text: &str, size_pt: f32, scale: u32) -> Option<(ColorImage, [f32; 2])> {
    let mut engine = photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner);
    if !engine.fonts.has_family(family) {
        return None;
    }
    let layer = TextLayer { text: text.into(), font_family: family.into(), size_pt: size_pt * scale as f32, ..Default::default() };
    let layout = engine.layout(&layer, 72.0);
    let b = layout.bounds()?;
    let (w, h) = ((b[2] - b[0]).ceil() + 2.0, (b[3] - b[1]).ceil() + 2.0);
    if !(w.is_finite() && h.is_finite()) || w < 1.0 || h < 1.0 {
        return None;
    }
    let (width, height) = ((w as usize).min(MAX_W), (h as usize).min(MAX_H));
    let transform = Affine::translate(f64::from(1.0 - b[0]), f64::from(1.0 - b[1]));
    let ink = photocraft_text::render::ink_rect(&layout, &transform);
    if ink.is_empty() || ink.width() > width as u32 * 4 || ink.height() > height as u32 * 4 {
        return None;
    }
    let rendered = photocraft_text::render::rasterize(&layout, &transform, PixelFormat::new(ColorMode::Rgb, SampleType::U8, true), layer.antialias);
    let mut rgba = vec![[0u8; 4]; width * height];
    rendered.surface.read_rgba8_into(photocraft_geom::Rect::new(0, 0, width as i32, height as i32), &mut rgba);
    let image = ColorImage::from_rgba_unmultiplied([width, height], &rgba.into_iter().flat_map(|p| [255, 255, 255, p[3]]).collect::<Vec<_>>());
    Some((image, [width as f32 / scale as f32, height as f32 / scale as f32]))
}

/// The texture of `text` in `family` at `size_pt` and its size in points, rendered once and
/// cached; `None` when the family isn't available.
fn texture(ctx: &egui::Context, family: &str, text: &str, size_pt: f32) -> Option<(TextureHandle, [f32; 2])> {
    let text: String = text.chars().take(MAX_CHARS).collect();
    if text.trim().is_empty() || !size_pt.is_finite() {
        return None;
    }
    let scale = ctx.pixels_per_point().ceil().clamp(1.0, 4.0) as u32;
    let key: Key =
        (family.to_owned(), text.clone(), size_pt.round() as u32, scale, (photocraft_text::fonts::generation(), photocraft_text::served::generation()));
    let id = egui::Id::unique(CACHE_ID);
    let cached = ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<Cache>(id);
        cache.0.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone())
    });
    if let Some(v) = cached {
        return v;
    }
    let made = render(family, &text, size_pt, scale).map(|(img, size)| (ctx.load_texture(format!("typer-font-{family}"), img, TextureOptions::LINEAR), size));
    ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<Cache>(id);
        if cache.0.len() >= CACHE_LIMIT {
            cache.0.pop_front();
        }
        cache.0.push_back((key, made.clone()));
    });
    made
}

/// Paint `text` in `family` inside `rect` (left aligned, vertically centred, scaled down to fit),
/// tinted `color`. Returns false when the family isn't available (draw a plain label instead).
pub(crate) fn paint(ui: &egui::Ui, family: &str, text: &str, size_pt: f32, rect: Rect, color: egui::Color32) -> bool {
    let Some((tex, [w, h])) = texture(ui.ctx(), family, text, size_pt) else { return false };
    let fit = (rect.width() / w).min(rect.height() / h).min(1.0);
    let size = vec2(w * fit, h * fit);
    let r = Rect::from_min_size(pos2(rect.left(), rect.center().y - size.y / 2.0), size);
    ui.painter().with_clip_rect(rect).image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), color);
    true
}
