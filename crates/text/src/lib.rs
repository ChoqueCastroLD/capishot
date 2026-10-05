//! Photocraft's type engine.
//!
//! * [`fonts::FontDb`]: bundled fonts (Inter, JetBrains Mono; always available, including on the
//!   web), optional system fonts from a directory scan (no fontconfig), user font data, TrueType
//!   collections, PostScript-name lookup.
//! * [`layout`]: shaping and line layout with [parley] (HarfRust shaping, bidi, line breaking),
//!   point and paragraph (box) text, per-run styles, Photoshop leading/indent/spacing rules.
//! * [`render`]: anti-aliased rasterization (exact-area accumulation, f32 coverage) into a
//!   [`photocraft_raster::Surface`] at any depth and colour model.
//! * [`psd`]: the PSD `TySh` block and its `EngineData` (text, fonts, runs, paragraphs, box).
//!
//! The usual entry point is [`TextEngine::render_layer`], which refreshes a
//! [`photocraft_doc::TextLayer`]'s cache from its style model.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod cjk;
pub mod engine_data;
pub mod fonts;
pub mod glyphs;
pub mod layout;
pub mod psd;
pub mod psd_styles;
pub mod raster;
pub mod render;
pub mod spell;
pub mod warp;

use photocraft_color::PixelFormat;
use photocraft_doc::TextLayer;

pub use fonts::{FaceInfo, FontDb, ResolvedFont};
pub use layout::{ClusterInfo, LineInfo, PlacedGlyph, TextLayout};
pub use render::Rendered;

/// Font database + layout context. Create once and reuse (font loading and shaping caches).
pub struct TextEngine {
    pub fonts: FontDb,
    layouter: layout::Layouter,
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TextEngine {
    /// Bundled fonts only: deterministic output (tests, web).
    pub fn new() -> Self {
        Self { fonts: FontDb::new(), layouter: layout::Layouter::new() }
    }

    /// Bundled plus installed system fonts (native desktop).
    pub fn with_system_fonts() -> Self {
        Self { fonts: FontDb::with_system_fonts(), layouter: layout::Layouter::new() }
    }

    /// Lays out a text layer (text space: pixels, before `layer.transform`).
    pub fn layout(&mut self, layer: &TextLayer, dpi: f32) -> TextLayout {
        self.layouter.layout(&mut self.fonts, layer, dpi)
    }

    /// Lays out and rasterizes a text layer into document space.
    pub fn render(&mut self, layer: &TextLayer, dpi: f32, format: PixelFormat) -> (TextLayout, Rendered) {
        let l = self.layout(layer, dpi);
        let warp = render::layout_warp(&l, layer.warp.as_ref());
        let r = render::rasterize_warped(&l, &layer.transform, format, layer.antialias, warp.as_ref());
        (l, r)
    }

    /// Re-renders `layer.cache` from its model. Returns the document-space rectangle drawn.
    pub fn render_layer(&mut self, layer: &mut TextLayer, dpi: f32, format: PixelFormat) -> photocraft_geom::Rect {
        let (_, r) = self.render(layer, dpi, format);
        layer.cache = Some(r.surface);
        r.rect
    }
}

/// A process-wide engine for callers without their own (bundled fonts only on the web; bundled
/// plus system fonts elsewhere). Locking is cheap relative to layout.
pub fn shared() -> &'static std::sync::Mutex<TextEngine> {
    static ENGINE: std::sync::OnceLock<std::sync::Mutex<TextEngine>> = std::sync::OnceLock::new();
    ENGINE.get_or_init(|| {
        let use_system = cfg!(not(target_arch = "wasm32")) && !cfg!(test) && std::env::var_os("PHOTOCRAFT_NO_SYSTEM_FONTS").is_none();
        std::sync::Mutex::new(if use_system { TextEngine::with_system_fonts() } else { TextEngine::new() })
    })
}

#[cfg(test)]
mod tests;
