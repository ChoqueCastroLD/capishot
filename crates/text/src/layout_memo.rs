//! The layouts a [`crate::TextEngine`] made last, by what they were made from. One keystroke
//! asks for the same layer's layout several times: the edit renders it, `type.info` reports its
//! lines, the type tool draws the caret and selection over it. The first one shapes; the others
//! get a copy, as long as nothing a layout depends on has changed since: the layer's model, the
//! resolution, and the fonts (any registered or removed font moves [`crate::fonts::generation`];
//! Missing Glyph Protection and the system font scan are read here).

use std::collections::VecDeque;

use photocraft_doc::TextLayer;

use crate::fonts::FontDb;
use crate::layout::TextLayout;

/// Layers whose last layout is kept: the one being edited and a few others the shell lays out
/// around it (hit tests, the overlay of another selected layer).
const KEPT: usize = 8;

/// What a layout was made from.
#[derive(PartialEq)]
struct Key {
    /// The layer's model without its rendered pixels and stored `TySh` (outputs, not inputs).
    layer: TextLayer,
    dpi: u32,
    fonts: u64,
    protection: bool,
    system: bool,
}

impl Key {
    fn of(fonts: &FontDb, t: &TextLayer, dpi: f32) -> Self {
        // Every field by name: a field added to the layer is either an input here or explicitly
        // left out, never forgotten.
        let TextLayer { text, font_family, size_pt, color, transform, cache: _, psd_raw: _, runs, paragraphs, shape, orientation, antialias, warp, perspective } = t;
        let layer = TextLayer {
            text: text.clone(),
            font_family: font_family.clone(),
            size_pt: *size_pt,
            color: *color,
            transform: *transform,
            cache: None,
            psd_raw: None,
            runs: runs.clone(),
            paragraphs: paragraphs.clone(),
            shape: shape.clone(),
            orientation: *orientation,
            antialias: *antialias,
            warp: warp.clone(),
            perspective: *perspective,
        };
        Self { layer, dpi: dpi.to_bits(), fonts: crate::fonts::generation(), protection: fonts.missing_glyph_protection(), system: fonts.system_loaded() }
    }
}

#[derive(Default)]
pub(crate) struct Memo {
    kept: VecDeque<(Key, TextLayout)>,
}

impl Memo {
    /// The layout of `t` at `dpi`: a copy of the one kept for the same inputs, else `make`'s,
    /// kept for next time.
    pub(crate) fn get(&mut self, fonts: &mut FontDb, t: &TextLayer, dpi: f32, make: impl FnOnce(&mut FontDb) -> TextLayout) -> TextLayout {
        let key = Key::of(fonts, t, dpi);
        if let Some(at) = self.kept.iter().position(|(k, _)| *k == key) {
            // Most recently used last, so the layer being edited is never the one to go.
            if let Some(e) = self.kept.remove(at) {
                let layout = e.1.clone();
                self.kept.push_back(e);
                return layout;
            }
        }
        let layout = make(fonts);
        // Fonts registered meanwhile (by another thread, or this layout's own first use of a
        // font): which ones it saw isn't known, so it isn't kept.
        if crate::fonts::generation() != key.fonts || fonts.system_loaded() != key.system {
            return layout;
        }
        if self.kept.len() == KEPT {
            self.kept.pop_front();
        }
        self.kept.push_back((key, layout.clone()));
        layout
    }
}

#[cfg(test)]
mod tests {
    use photocraft_doc::TextLayer;

    use super::Memo;
    use crate::fonts::FontDb;
    use crate::layout::Layouter;

    #[test]
    fn a_layer_is_shaped_again_only_when_what_its_layout_depends_on_changes() {
        let (mut fonts, mut layouter, mut memo) = (FontDb::new(), Layouter::new(), Memo::default());
        let mut made = 0;
        let mut get = |memo: &mut Memo, fonts: &mut FontDb, t: &TextLayer, dpi: f32| {
            memo.get(fonts, t, dpi, |f| {
                made += 1;
                layouter.layout(f, t, dpi)
            })
        };
        let t = TextLayer { text: "Hola, ¿qué tal?".into(), font_family: "Inter".into(), size_pt: 24.0, ..Default::default() };
        let first = get(&mut memo, &mut fonts, &t, 72.0);
        let again = get(&mut memo, &mut fonts, &t, 72.0);
        assert_eq!(format!("{first:?}"), format!("{again:?}"), "the copy is the layout");
        // Its rendered pixels are an output: the layout stays.
        let rendered = TextLayer { cache: Some(photocraft_raster::Surface::new(photocraft_color::PixelFormat::RGBA8)), ..t.clone() };
        get(&mut memo, &mut fonts, &rendered, 72.0);
        drop(get);
        assert_eq!(made, 1);
        let mut get = |memo: &mut Memo, fonts: &mut FontDb, t: &TextLayer, dpi: f32| {
            memo.get(fonts, t, dpi, |f| {
                made += 1;
                layouter.layout(f, t, dpi)
            })
        };
        let typed = TextLayer { text: "Hola, ¿qué tal?!".into(), ..t.clone() };
        let longer = get(&mut memo, &mut fonts, &typed, 72.0);
        assert_ne!(format!("{longer:?}"), format!("{first:?}"));
        get(&mut memo, &mut fonts, &typed, 72.0);
        get(&mut memo, &mut fonts, &t, 72.0);
        get(&mut memo, &mut fonts, &t, 144.0);
        let protection = fonts.missing_glyph_protection();
        fonts.set_missing_glyph_protection(!protection);
        get(&mut memo, &mut fonts, &t, 144.0);
        crate::fonts::bump_generation();
        get(&mut memo, &mut fonts, &t, 144.0);
        drop(get);
        // The edited text, the resolution, Missing Glyph Protection and a font registered: each
        // shaped once; the first layer again came from the memo.
        assert_eq!(made, 5);
    }
}
