//! Procedural surface patterns: screentone dots or hatching behind the canvas surround and the
//! bars (`--pattern`, `--bar-pattern`). No image or url is involved: a tile's coverage is computed
//! here, uploaded once per kind and pixel size as a repeating alpha texture, and tinted when drawn,
//! so a frame only adds one textured rectangle per surface and a theme without a pattern adds nothing.
//!
//! A pattern belongs behind the interface. It is painted before the document (which covers it), and
//! never on anything that shows the page.

use egui::{Color32, Context, Id, Painter, Rect, TextureHandle, TextureId, TextureOptions};
use serde::{Deserialize, Serialize};

use super::Tokens;

/// The strongest pattern a bar takes: its labels sit on top, so a stylesheet can't drown them.
pub const MAX_BAR_OPACITY: f32 = 0.4;
/// Tiles kept alive: a surface or two per display scale.
const MAX_TILES: usize = 6;
/// The largest tile side in pixels, whatever the size and display scale.
const MAX_TILE_PX: f32 = 256.0;
/// Per axis samples of a tile's anti-aliased coverage.
const SAMPLES: usize = 4;
/// Dot radius and hatch width, as fractions of the tile.
const DOT_RADIUS: f32 = 0.2;
const LINE_WIDTH: f32 = 0.26;

/// The defaults of an unset `--pattern-size` and `--pattern-opacity`.
const DEFAULT_SIZE: f32 = 6.0;
const DEFAULT_OPACITY: f32 = 0.15;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatternKind {
    #[default]
    None,
    /// Staggered round dots, like screentone.
    Dots,
    /// 45° hatching.
    Lines,
}

impl PatternKind {
    pub const ALL: [PatternKind; 3] = [PatternKind::None, PatternKind::Dots, PatternKind::Lines];

    /// Reads a `theme.css` value.
    pub fn parse(v: &str) -> Option<Self> {
        match v.trim().to_ascii_lowercase().as_str() {
            "none" => Some(PatternKind::None),
            "dots" => Some(PatternKind::Dots),
            "lines" => Some(PatternKind::Lines),
            _ => None,
        }
    }

    /// Its `theme.css` value when it draws something.
    pub fn name(self) -> Option<&'static str> {
        match self {
            PatternKind::None => None,
            PatternKind::Dots => Some("dots"),
            PatternKind::Lines => Some("lines"),
        }
    }

    /// Its English label (the UI translates it).
    pub fn label(self) -> &'static str {
        match self {
            PatternKind::None => "None",
            PatternKind::Dots => "Dots",
            PatternKind::Lines => "Lines",
        }
    }
}

/// The surfaces a theme can pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Canvas,
    Bar,
}

/// A pattern as a theme resolves it: `None` from [`Tokens::surface_pattern`] means nothing to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfacePattern {
    pub kind: PatternKind,
    /// Tile side in points.
    pub size: f32,
    pub opacity: f32,
    pub color: Color32,
}

impl Tokens {
    /// The pattern on `surface`, if it shows one.
    pub fn surface_pattern(&self, surface: Surface) -> Option<SurfacePattern> {
        let (kind, size, opacity) = match surface {
            Surface::Canvas => (self.pattern, self.pattern_size, self.pattern_opacity),
            Surface::Bar => (self.bar_pattern, self.bar_pattern_size, self.bar_pattern_opacity),
        };
        let opacity = opacity.unwrap_or(DEFAULT_OPACITY);
        (kind != PatternKind::None && opacity > 0.0).then(|| SurfacePattern {
            kind,
            size: size.unwrap_or(DEFAULT_SIZE),
            opacity,
            color: self.pattern_color.unwrap_or(self.text),
        })
    }
}

/// How a tile is drawn: a repeating texture, its side in points and its tint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    pub texture: TextureId,
    pub cell: f32,
    pub tint: Color32,
}

/// The tiles uploaded so far (newest last).
#[derive(Clone, Default)]
struct Cache {
    tiles: Vec<(PatternKind, u16, TextureHandle)>,
}

fn cache_id() -> Id {
    Id::unique("theme-pattern-tiles")
}

/// One tile's coverage, row by row. It repeats seamlessly: the dots sit on a staggered lattice and
/// the hatching runs along `u + v`, both of which wrap at the tile's edges.
fn coverage(kind: PatternKind, side: usize) -> Vec<u8> {
    const LATTICE: [(f32, f32); 5] = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.5, 0.5)];
    let n = (side * SAMPLES).max(1) as f32;
    let mut out = Vec::with_capacity(side * side);
    for y in 0..side {
        for x in 0..side {
            let mut hits = 0u32;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let u = ((x * SAMPLES + sx) as f32 + 0.5) / n;
                    let v = ((y * SAMPLES + sy) as f32 + 0.5) / n;
                    let inside = match kind {
                        PatternKind::None => false,
                        PatternKind::Dots => LATTICE.iter().any(|(a, b)| (u - a).powi(2) + (v - b).powi(2) <= DOT_RADIUS * DOT_RADIUS),
                        PatternKind::Lines => (u + v).rem_euclid(1.0) < LINE_WIDTH,
                    };
                    hits += u32::from(inside);
                }
            }
            out.push((hits * 255 / (SAMPLES * SAMPLES) as u32) as u8);
        }
    }
    out
}

/// The tile for `pattern`, uploading it the first time. A hit takes no allocation.
pub fn tile(ctx: &Context, pattern: &SurfacePattern) -> Option<Tile> {
    let ppp = ctx.pixels_per_point();
    if pattern.kind == PatternKind::None || !pattern.size.is_finite() || !ppp.is_finite() || ppp <= 0.0 {
        return None;
    }
    // Whole pixels per tile, so a tile lands on the pixel grid and stays crisp at any scale.
    let side = (pattern.size * ppp).round().clamp(2.0, MAX_TILE_PX);
    let (key, cell) = (side as u16, side / ppp);
    let tint = pattern.color.gamma_multiply(pattern.opacity.clamp(0.0, 1.0));
    let hit =
        ctx.data_mut(|d| d.get_temp_mut_or_default::<Cache>(cache_id()).tiles.iter().find(|(k, s, _)| *k == pattern.kind && *s == key).map(|(.., h)| h.id()));
    if let Some(texture) = hit {
        return Some(Tile { texture, cell, tint });
    }
    let side_px = usize::from(key);
    let pixels: Vec<Color32> = coverage(pattern.kind, side_px).into_iter().map(Color32::from_white_alpha).collect();
    let options = TextureOptions { wrap_mode: egui::TextureWrapMode::Repeat, ..TextureOptions::LINEAR };
    let handle = ctx.load_texture("theme-pattern", egui::ColorImage::new([side_px, side_px], pixels), options);
    let texture = handle.id();
    ctx.data_mut(|d| {
        let tiles = &mut d.get_temp_mut_or_default::<Cache>(cache_id()).tiles;
        if tiles.len() >= MAX_TILES {
            drop(tiles.remove(0));
        }
        tiles.push((pattern.kind, key, handle));
    });
    Some(Tile { texture, cell, tint })
}

/// Fills `rect` with a tile. The phase comes from the window's origin, so neighbouring surfaces
/// line up and the pattern doesn't swim when a panel resizes.
pub fn paint(painter: &Painter, rect: Rect, tile: &Tile) {
    if tile.cell <= 0.0 {
        return;
    }
    let uv = Rect::from_min_max((rect.min.to_vec2() / tile.cell).to_pos2(), (rect.max.to_vec2() / tile.cell).to_pos2());
    painter.image(tile.texture, rect, uv, tile.tint);
}

/// The bar pattern behind a top or bottom panel: call first in its body. The panel's clip rectangle
/// is the whole bar, margins included.
pub fn paint_bar(ui: &egui::Ui) {
    let t = Tokens::get(ui.ctx());
    if let Some(p) = t.surface_pattern(Surface::Bar)
        && let Some(tile) = tile(ui.ctx(), &p)
    {
        paint(ui.painter(), ui.clip_rect(), &tile);
    }
}

#[cfg(test)]
#[path = "pattern_tests.rs"]
mod tests;
