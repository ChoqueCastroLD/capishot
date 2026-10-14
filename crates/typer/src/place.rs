// Ported from TypeR app_src/host.js (master 342be99: _calculateSelectionDimensions,
// _resizeTextBoxToContent, _positionLayerWithinSelection; develop 71bc061: the measure-box fit
// of _setActiveLayerText), MIT, (c) 2017 Swirt. See crates/typer/LICENSE-TypeR.txt.

//! Placement arithmetic (P1, P2, P5, P6): text box size from a bubble, box refit to the text,
//! and the move that centres the text. Pure numbers; the engine measures the text and applies
//! the results.

use crate::bubble::Rect;

/// The box takes this share of the bubble's width.
pub const SELECTION_SCALE: f64 = 0.9;
/// Narrowest box when padding applies (px).
pub const MIN_TEXTBOX_WIDTH: f64 = 10.0;
/// Size used when neither the style nor a layer gives one (px, TypeR's fallback).
pub const FALLBACK_SIZE_PX: f64 = 20.0;

/// Text box for a bubble `sel` (P1): `0.9 × width − 2 × padding` wide (at least 10 when padding
/// applies) and as tall as the bubble.
pub fn box_size(sel: &Rect, padding: f64) -> (f64, f64) {
    let mut width = sel.width * SELECTION_SCALE;
    if padding.is_finite() && padding > 0.0 {
        width = (width - padding * 2.0).max(MIN_TEXTBOX_WIDTH);
    }
    (width.max(1.0), sel.height.max(1.0))
}

/// Box height that holds text whose ink is `extent_h` tall at `size_px`: `extent + size + 2`.
pub fn fit_height(extent_h: f64, size_px: f64) -> f64 {
    (extent_h.max(0.0) + size_px.max(0.0) + 2.0).max(1.0)
}

/// Box width that holds lines whose widest ink is `extent_w` (TypeR 3.0's shrink after a
/// measure): `extent + max(2, 0.4 × size)`.
pub fn fit_width(extent_w: f64, size_px: f64) -> f64 {
    (extent_w.max(0.0) + (0.4 * size_px.max(0.0)).max(2.0)).max(1.0)
}

/// Whole-pixel move that puts the centre of `ink` on `target` (a layer moves by whole pixels so
/// its glyphs keep their rasterization).
pub fn center_offset(target: (f64, f64), ink: &Rect) -> (f64, f64) {
    let (cx, cy) = ink.center();
    let r = |v: f64| if v.is_finite() { v.round() } else { 0.0 };
    (r(target.0 - cx), r(target.1 - cy))
}

/// Pixels → points at `dpi` (TypeR's `_convertPixelToPoint`, without its integer truncation).
pub fn px_to_pt(px: f64, dpi: f64) -> f64 {
    crate::psjson::to_pt(px, crate::psjson::Unit::Pixels, dpi)
}

/// Points → pixels at `dpi`.
pub fn pt_to_px(pt: f64, dpi: f64) -> f64 {
    crate::psjson::from_pt(pt, crate::psjson::Unit::Pixels, dpi)
}

/// Box scale for a size change `old → new` (P6 fallback): `new / old`, 1 when undefined.
pub fn size_ratio(old: f64, new: f64) -> f64 {
    let r = new / old;
    if r.is_finite() && r > 0.0 { r } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_follows_the_bubble() {
        let sel = Rect::new(100.0, 50.0, 200.0, 120.0);
        assert_eq!(box_size(&sel, 10.0), (160.0, 120.0), "0.9 × 200 − 2 × 10");
        assert_eq!(box_size(&sel, 0.0), (180.0, 120.0), "no padding: 0.9 × width");
        assert_eq!(box_size(&Rect::new(0.0, 0.0, 20.0, 30.0), 10.0), (10.0, 30.0), "min 10 with padding");
        assert_eq!(box_size(&Rect::new(0.0, 0.0, 20.0, 30.0), f64::NAN), (18.0, 30.0));
    }

    #[test]
    fn refit_rules() {
        assert_eq!(fit_height(40.0, 20.0), 62.0, "extent + size + 2");
        assert_eq!(fit_width(100.0, 20.0), 108.0, "extent + 0.4 × size");
        assert_eq!(fit_width(100.0, 3.0), 102.0, "at least 2");
        assert_eq!(size_ratio(10.0, 12.0), 1.2);
        assert_eq!(size_ratio(0.0, 12.0), 1.0);
    }

    #[test]
    fn centring_moves_by_whole_pixels() {
        let ink = Rect::new(10.0, 10.0, 21.0, 10.0);
        assert_eq!(center_offset((100.0, 50.0), &ink), (80.0, 35.0), "79.5 rounds to 80");
        assert_eq!(center_offset((f64::NAN, 50.0), &ink).0, 0.0);
    }

    #[test]
    fn pixels_and_points() {
        assert_eq!(px_to_pt(20.0, 72.0), 20.0);
        assert!((px_to_pt(300.0, 300.0) - 72.0).abs() < 1e-9);
        assert!((pt_to_px(px_to_pt(33.0, 350.0), 350.0) - 33.0).abs() < 1e-9);
        assert_eq!(px_to_pt(10.0, 0.0), 10.0, "a bad dpi counts as 72");
    }
}
