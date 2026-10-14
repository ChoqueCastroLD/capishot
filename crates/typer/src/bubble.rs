// Ported from TypeR app_src/host.js (master 342be99: _createMagicWandSelection; develop 71bc061:
// _getAdaptiveSelectionOpenRadius, _getAdaptiveOpenedSelectionBounds, the selection row
// sampler), MIT, (c) 2017 Swirt. See crates/typer/LICENSE-TypeR.txt.

//! Bubble geometry: where text is centred (P7), the bubble under a point (D1/D2) and its outline
//! rows (D3).
//!
//! TypeR does all of this with Photoshop selection operations (magic wand, contract, expand,
//! bounds reads) on the live selection, then restores it. Here the same steps are pure functions
//! on a binary [`Mask`], so a scan touches neither the document selection nor history.
//!
//! - **Opening (P7, TypeR 3.0).** A bubble's bounding box includes its tail, so its midpoint is
//!   off the body. A morphological opening (erode then dilate by `r`) removes the tail; the text
//!   centres on the opened shape's box midpoint. `r = min(max(4, round(0.1·min(w, h))),
//!   floor(min(w, h)/2 − 1))`; when the eroded shape vanishes or the opened box is under
//!   [`MIN_AREA`], `r` halves and the opening is retried. Discs are exact (Euclidean distance
//!   transform), not Photoshop's approximations.
//! - **Holes** (the text inside a bubble, specks) are filled before the opening, so the result
//!   doesn't depend on whether the text was visible when the bubble was selected.
//! - **Wand (D1).** Tolerance 20 (Photoshop's 0–255 scale), contiguous, anti-aliased, on the
//!   merged page; coverage ≥ 50 % counts as selected.

use photocraft_algo::selection as sel;
use serde::{Deserialize, Serialize};

/// An axis-aligned rectangle in document pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect { x, y, width, height }
    }

    /// From `[x, y, w, h]`; `None` unless all four are finite and the size is positive.
    pub fn from_xywh(v: &[f64]) -> Option<Rect> {
        match v {
            [x, y, w, h] if [*x, *y, *w, *h].iter().all(|n| n.is_finite()) && *w > 0.0 && *h > 0.0 => Some(Rect::new(*x, *y, *w, *h)),
            _ => None,
        }
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    pub fn area(&self) -> f64 {
        self.width.max(0.0) * self.height.max(0.0)
    }

    /// TypeR's key for a stored selection (`xMid_yMid_w_h`), to skip duplicates.
    pub fn key(&self) -> String {
        let (cx, cy) = self.center();
        format!("{cx}_{cy}_{}_{}", self.width, self.height)
    }

    pub fn from_box(b: IRect) -> Rect {
        Rect::new(f64::from(b.x0), f64::from(b.y0), f64::from(b.x1 - b.x0), f64::from(b.y1 - b.y0))
    }
}

/// Integer pixel box, `x0..x1` × `y0..y1` (exclusive ends).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    pub fn width(&self) -> i32 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> i32 {
        self.y1 - self.y0
    }
    pub fn area(&self) -> f64 {
        f64::from(self.width().max(0)) * f64::from(self.height().max(0))
    }
}

/// Selections (opened) smaller than this many pixels are refused (`smallSelection`).
pub const MIN_AREA: f64 = 200.0;

/// TypeR's wand tolerance for bubble detection (Photoshop 0–255 scale).
pub const WAND_TOLERANCE: f32 = 20.0;

/// A detected region whose box is more than this many times the text's area is not a bubble
/// (an SFX on art, D2). See [`text_area`].
pub const MAX_BUBBLE_TO_TEXT: f64 = 60.0;

/// Default number of outline rows (TypeR's interactive sampler); requests are clamped to 5..=31.
pub const DEFAULT_ROWS: usize = 21;

/// Largest mask handled (pixels), so a hostile size can't allocate without bound.
pub const MAX_MASK_PIXELS: usize = 1 << 28;

/// Smallest opening radius TypeR uses.
const MIN_OPEN_RADIUS: i64 = 4;
/// Opening radius as a fraction of the shorter side.
const OPEN_RATIO: f64 = 0.1;

/// One scanline band of a bubble outline, normalized to the bubble's bounding box (0..1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub y: f64,
    pub left: f64,
    pub right: f64,
    pub width: f64,
}

/// A detected bubble (`typer.detectBubble`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BubbleHit {
    pub found: bool,
    /// Bounds of the selection (holes filled).
    pub bbox: Rect,
    /// Bounds of the opened shape (tail removed); its centre is where text is centred.
    pub opened: Rect,
    pub center: (f64, f64),
    /// Outline rows for TextShapeR.
    pub rows: Vec<Row>,
    /// Selected pixels.
    pub area: f64,
}

/// A multi-bubble stored selection (M1): bounds plus the line and style captured with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSelection {
    /// The selection's bounds (what TypeR's multi paste centres on).
    pub bounds: Rect,
    /// The opened bounds (tail removed), when the selection's pixels were known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened: Option<Rect>,
    pub line_index: Option<usize>,
    pub style_id: Option<crate::Id>,
}

/// A binary mask over a box of the document.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub x0: i32,
    pub y0: i32,
    pub width: usize,
    pub height: usize,
    /// Row-major, `width × height`.
    pub px: Vec<bool>,
}

impl Mask {
    /// An empty mask; `None` for an empty or oversized box.
    pub fn new(x0: i32, y0: i32, width: usize, height: usize) -> Option<Mask> {
        let n = width.checked_mul(height).filter(|n| *n > 0 && *n <= MAX_MASK_PIXELS)?;
        i32::try_from(width).ok()?.checked_add(x0)?;
        i32::try_from(height).ok()?.checked_add(y0)?;
        Some(Mask { x0, y0, width, height, px: vec![false; n] })
    }

    /// From 8-bit coverage over the box (≥ 128 = selected). `None` if the sizes disagree.
    pub fn from_coverage(x0: i32, y0: i32, width: usize, height: usize, coverage: &[u8]) -> Option<Mask> {
        let mut m = Mask::new(x0, y0, width, height)?;
        if coverage.len() != m.px.len() {
            return None;
        }
        for (p, c) in m.px.iter_mut().zip(coverage) {
            *p = *c >= 128;
        }
        Some(m)
    }

    /// From 0–1 coverage over the box (≥ 0.5 = selected).
    pub fn from_coverage_f32(x0: i32, y0: i32, width: usize, height: usize, coverage: &[f32]) -> Option<Mask> {
        let mut m = Mask::new(x0, y0, width, height)?;
        if coverage.len() != m.px.len() {
            return None;
        }
        for (p, c) in m.px.iter_mut().zip(coverage) {
            *p = *c >= 0.5;
        }
        Some(m)
    }

    /// Selected at document pixel `(x, y)`.
    pub fn get(&self, x: i32, y: i32) -> bool {
        let (Ok(cx), Ok(cy)) = (usize::try_from(i64::from(x) - i64::from(self.x0)), usize::try_from(i64::from(y) - i64::from(self.y0))) else {
            return false;
        };
        cx < self.width && cy < self.height && self.px.get(cy * self.width + cx).copied().unwrap_or(false)
    }

    pub fn set(&mut self, x: i32, y: i32, v: bool) {
        let (Ok(cx), Ok(cy)) = (usize::try_from(i64::from(x) - i64::from(self.x0)), usize::try_from(i64::from(y) - i64::from(self.y0))) else {
            return;
        };
        if cx < self.width
            && let Some(p) = self.px.get_mut(cy * self.width + cx)
        {
            *p = v;
        }
    }

    /// Selected pixels.
    pub fn count(&self) -> usize {
        self.px.iter().filter(|p| **p).count()
    }

    /// Tight bounds of the selected pixels, document coordinates.
    pub fn bounds(&self) -> Option<IRect> {
        let w = self.width;
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        for (y, row) in self.px.chunks(w.max(1)).enumerate() {
            let Some(first) = row.iter().position(|p| *p) else { continue };
            let last = row.iter().rposition(|p| *p).unwrap_or(first);
            (x0, x1, y0, y1) = (x0.min(first), x1.max(last + 1), y0.min(y), y1.max(y + 1));
        }
        if x0 == usize::MAX {
            return None;
        }
        // Sizes are capped by `new`, so these fit in i32.
        let c = |v: usize| i32::try_from(v).unwrap_or(i32::MAX);
        Some(IRect {
            x0: self.x0.saturating_add(c(x0)),
            y0: self.y0.saturating_add(c(y0)),
            x1: self.x0.saturating_add(c(x1)),
            y1: self.y0.saturating_add(c(y1)),
        })
    }

    /// The mask cropped to its selected pixels plus `pad` empty pixels on every side.
    pub fn cropped(&self, pad: usize) -> Option<Mask> {
        let b = self.bounds()?;
        let p = i32::try_from(pad).ok()?;
        let (w, h) = (usize::try_from(b.width()).ok()?.checked_add(2 * pad)?, usize::try_from(b.height()).ok()?.checked_add(2 * pad)?);
        let mut out = Mask::new(b.x0.checked_sub(p)?, b.y0.checked_sub(p)?, w, h)?;
        let sw = self.width;
        for y in b.y0..b.y1 {
            let sy = usize::try_from(y - self.y0).ok()?;
            let sx = usize::try_from(b.x0 - self.x0).ok()?;
            let src = self.px.get(sy * sw + sx..sy * sw + sx + usize::try_from(b.width()).ok()?)?;
            let dy = usize::try_from(y - out.y0).ok()?;
            let dst = out.px.get_mut(dy * w + pad..dy * w + pad + src.len())?;
            dst.copy_from_slice(src);
        }
        Some(out)
    }

    /// Fill holes: unselected pixels not 4-connected to the mask's border become selected.
    pub fn fill_holes(&mut self) {
        let (w, h) = (self.width, self.height);
        if w == 0 || h == 0 {
            return;
        }
        let mut outside = vec![false; self.px.len()];
        let mut stack: Vec<usize> = Vec::new();
        let seed = |i: usize, outside: &mut Vec<bool>, stack: &mut Vec<usize>| {
            if !self.px.get(i).copied().unwrap_or(true) && !outside.get(i).copied().unwrap_or(true) {
                outside[i] = true;
                stack.push(i);
            }
        };
        for x in 0..w {
            seed(x, &mut outside, &mut stack);
            seed((h - 1) * w + x, &mut outside, &mut stack);
        }
        for y in 0..h {
            seed(y * w, &mut outside, &mut stack);
            seed(y * w + w - 1, &mut outside, &mut stack);
        }
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            if x > 0 {
                seed(i - 1, &mut outside, &mut stack);
            }
            if x + 1 < w {
                seed(i + 1, &mut outside, &mut stack);
            }
            if y > 0 {
                seed(i - w, &mut outside, &mut stack);
            }
            if y + 1 < h {
                seed(i + w, &mut outside, &mut stack);
            }
        }
        for (p, o) in self.px.iter_mut().zip(outside) {
            *p = !o;
        }
    }

    /// Morphological opening with a disc of radius `r`: erode (keep pixels farther than `r` from
    /// the outside), then dilate by `r`. `None` when nothing survives the erosion.
    pub fn opened(&self, r: f32) -> Option<Mask> {
        // One empty pixel around the shape makes the outside explicit for the distance transform.
        let m = self.cropped(1)?;
        let (w, h) = (m.width, m.height);
        let outside: Vec<bool> = m.px.iter().map(|p| !p).collect();
        let to_outside = sel::edt(&outside, w, h);
        if to_outside.len() != m.px.len() {
            return None;
        }
        let eroded: Vec<bool> = m.px.iter().zip(&to_outside).map(|(p, d)| *p && *d > r).collect();
        if !eroded.iter().any(|p| *p) {
            return None;
        }
        let to_core = sel::edt(&eroded, w, h);
        if to_core.len() != m.px.len() {
            return None;
        }
        Some(Mask { px: to_core.iter().map(|d| *d <= r).collect(), ..m })
    }
}

/// TypeR 3.0's opening radius for a selection box `w × h`; 0 = too thin to open.
pub fn opening_radius(w: f64, h: f64) -> u32 {
    if !w.is_finite() || !h.is_finite() {
        return 0;
    }
    let shortest = w.min(h);
    let max_radius = (shortest / 2.0 - 1.0).floor();
    if max_radius <= 0.0 {
        return 0;
    }
    let radius = (MIN_OPEN_RADIUS as f64).max((shortest * OPEN_RATIO).round());
    radius.min(max_radius).clamp(0.0, f64::from(u32::MAX)) as u32
}

/// The selection's geometry: its box and the opened box text centres on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Opened {
    /// Bounds of the selection (holes filled).
    pub bbox: Rect,
    /// Bounds after the adaptive opening; `bbox` when no opening survives.
    pub opened: Rect,
    /// Radius that was accepted (0 = none).
    pub radius: u32,
    /// Selected pixels (holes filled).
    pub area: usize,
}

/// Holes filled, then TypeR 3.0's adaptive opening (P7). `None` for an empty mask.
pub fn opened_bounds(mask: &Mask) -> Option<Opened> {
    let mut m = mask.cropped(1)?;
    m.fill_holes();
    let b = m.bounds()?;
    let bbox = Rect::from_box(b);
    let area = m.count();
    let mut r = opening_radius(bbox.width, bbox.height);
    while r >= 1 {
        if let Some(open) = m.opened(r as f32)
            && let Some(ob) = open.bounds()
            && ob.width() > 1
            && ob.height() > 1
            && ob.area() >= MIN_AREA
        {
            return Some(Opened { bbox, opened: Rect::from_box(ob), radius: r, area });
        }
        r /= 2;
    }
    Some(Opened { bbox, opened: bbox, radius: 0, area })
}

/// Outline rows of `mask` (D3): `n` bands (clamped to 5..=31) from top to bottom of its box. Each
/// takes the leftmost and rightmost selected pixel over three scanlines (the band's middle and
/// its two edges), normalized to the box. Empty bands get `{left: 0.5, right: 0.5, width: 0}`.
pub fn rows(mask: &Mask, n: usize) -> Vec<Row> {
    let Some(b) = mask.bounds() else { return Vec::new() };
    let n = n.clamp(5, 31);
    let (min_y, height, width) = (f64::from(b.y0), f64::from(b.height()), f64::from(b.width()));
    let slice = height / n as f64;
    let span = |y: f64| -> Option<(i32, i32)> {
        let py = (y.floor() as i32).clamp(b.y0, b.y1 - 1);
        let mut left = None;
        let mut right = None;
        for x in b.x0..b.x1 {
            if mask.get(x, py) {
                left.get_or_insert(x);
                right = Some(x + 1);
            }
        }
        Some((left?, right?))
    };
    (0..n)
        .map(|r| {
            let ratio = if n <= 1 { 0.5 } else { r as f64 / (n - 1) as f64 };
            let mid = min_y + height * ratio;
            let (mut left, mut right): (Option<i32>, Option<i32>) = (None, None);
            for off in [-slice / 2.0, 0.0, slice / 2.0] {
                let mut y = mid + off;
                if y <= min_y {
                    y = min_y + height * 0.002;
                }
                if y >= min_y + height {
                    y = min_y + height - height * 0.002;
                }
                if let Some((l, rr)) = span(y) {
                    left = Some(left.map_or(l, |v| v.min(l)));
                    right = Some(right.map_or(rr, |v| v.max(rr)));
                }
            }
            match (left, right) {
                (Some(l), Some(rr)) if rr > l => {
                    let nl = (f64::from(l - b.x0) / width).clamp(0.0, 1.0);
                    let nr = (f64::from(rr - b.x0) / width).clamp(0.0, 1.0);
                    Row { y: ratio, left: nl, right: nr, width: (nr - nl).clamp(0.0, 1.0) }
                }
                _ => Row { y: ratio, left: 0.5, right: 0.5, width: 0.0 },
            }
        })
        .collect()
}

/// Magic wand on an 8-bit RGBA image covering `area` (D1): contiguous, anti-aliased, at
/// `tolerance` (0–255), seeded at `seed`. `None` when the seed is outside the image.
pub fn wand(img: &[[u8; 4]], area: photocraft_geom::Rect, seed: (i32, i32), tolerance: f32) -> Option<Mask> {
    let tol = if tolerance.is_finite() { tolerance.clamp(0.0, 255.0) } else { WAND_TOLERANCE };
    let region = sel::wand_region_sampled(img, area, seed, 1, tol, true, true)?;
    let (w, h) = (region.bbox.width() as usize, region.bbox.height() as usize);
    Mask::from_coverage(region.bbox.x0, region.bbox.y0, w, h, &region.mask)
}

/// TypeR's wand seed for a text layer whose ink box is `ink`: 5 px left of it, at its middle
/// height, clamped to the page.
pub fn seed_left_of(ink: &Rect) -> (i32, i32) {
    let x = (ink.x - 5.0).max(0.0);
    let y = (ink.y + ink.height / 2.0).max(0.0);
    (x.floor().clamp(0.0, f64::from(i32::MAX)) as i32, y.floor().clamp(0.0, f64::from(i32::MAX)) as i32)
}

/// The text area the "no bubble" rule compares against: the ink box, but at least a square of
/// four em. TypeR compares with the ink box alone, which rejects a short word ("NO!") in a large
/// bubble; four em keeps that while a region the size of the page is still refused.
pub fn text_area(ink: &Rect, size_px: f64) -> f64 {
    let em = if size_px.is_finite() { size_px.max(0.0) } else { 0.0 };
    ink.area().max((4.0 * em).powi(2))
}

/// Whether a region whose box is `region` is too large to be the bubble of text of area
/// `text_area` (D2).
pub fn too_big(region: &Rect, text_area: f64) -> bool {
    text_area > 0.0 && region.area() > MAX_BUBBLE_TO_TEXT * text_area
}

/// What a wand region is judged against to count as a speech bubble. TypeR has no such test
/// (its wand centres text in whatever region it finds; only TextShapeR applies the 60× rule).
/// Capishot refuses regions that can't be a bubble, so Align leaves SFX and narration over art
/// where they are instead of moving them into a patch of art.
#[derive(Clone, Copy, Debug, Default)]
pub struct Judge<'a> {
    /// Ink box of the text the bubble is for, and its size in pixels.
    pub text: Option<(&'a Rect, f64)>,
    /// The seed pixel (RGBA).
    pub seed: Option<[u8; 4]>,
    /// The page: a region touching two or more of its edges is background, not a bubble (a
    /// bubble cut by the page edge touches one).
    pub page: Option<Rect>,
}

/// Bubbles are filled shapes: an ellipse fills 79 % of its box, a box with a tail ~70 %.
pub const MIN_FILL: f64 = 0.35;
/// Narrowest opened bubble that can hold a line of text (px).
pub const MIN_SIDE: f64 = 16.0;
/// Share of the text's ink box that must lie inside the region's box.
pub const MIN_TEXT_INSIDE: f64 = 0.6;
/// Darkest seed (Rec. 601 luma) a bubble interior may have.
pub const MIN_SEED_LUMA: f64 = 128.0;

impl Judge<'_> {
    /// `None` if `o` can be the bubble, else why not (TypeR's error codes: `smallSelection`,
    /// `noBubble`).
    pub fn verdict(&self, o: &Opened) -> Option<&'static str> {
        if o.opened.area() < MIN_AREA {
            return Some("smallSelection");
        }
        let fill = o.area as f64 / o.bbox.area().max(1.0);
        if fill < MIN_FILL || o.opened.width.min(o.opened.height) < MIN_SIDE {
            return Some("noBubble");
        }
        if let Some(p) = self.page {
            let b = &o.bbox;
            let edges = [b.x <= p.x, b.y <= p.y, b.x + b.width >= p.x + p.width, b.y + b.height >= p.y + p.height];
            if edges.iter().filter(|e| **e).count() >= 2 {
                return Some("noBubble");
            }
        }
        if let Some([r, g, b, a]) = self.seed
            && a > 0
            && 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b) < MIN_SEED_LUMA
        {
            return Some("noBubble");
        }
        if let Some((ink, size)) = self.text {
            if too_big(&o.bbox, text_area(ink, size)) {
                return Some("noBubble");
            }
            let ix = (ink.x + ink.width).min(o.bbox.x + o.bbox.width) - ink.x.max(o.bbox.x);
            let iy = (ink.y + ink.height).min(o.bbox.y + o.bbox.height) - ink.y.max(o.bbox.y);
            let inside = ix.max(0.0) * iy.max(0.0);
            if ink.area() > 0.0 && inside < MIN_TEXT_INSIDE * ink.area() {
                return Some("noBubble");
            }
        }
        None
    }
}

/// The bubble around `mask`: holes filled, opened, rows sampled, judged by `judge` (`found` is
/// false when it isn't a bubble).
pub fn hit(mask: &Mask, judge: &Judge, rows_wanted: usize) -> (BubbleHit, Option<&'static str>) {
    let Some(o) = opened_bounds(mask) else { return (BubbleHit::default(), Some("noSelection")) };
    let mut filled = mask.cropped(1).unwrap_or_else(|| mask.clone());
    filled.fill_holes();
    let verdict = judge.verdict(&o);
    let hit = BubbleHit {
        found: verdict.is_none(),
        bbox: o.bbox,
        opened: o.opened,
        center: o.opened.center(),
        rows: rows(&filled, rows_wanted),
        area: o.area as f64,
    };
    (hit, verdict)
}

/// The seed pixel of an RGBA image covering `area`.
pub fn pixel(img: &[[u8; 4]], area: photocraft_geom::Rect, (x, y): (i32, i32)) -> Option<[u8; 4]> {
    if !area.contains(x, y) {
        return None;
    }
    img.get((y - area.y0) as usize * area.width() as usize + (x - area.x0) as usize).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A filled ellipse with an optional tail (a triangle hanging below), as a mask.
    fn bubble(w: usize, h: usize, cx: f64, cy: f64, rx: f64, ry: f64, tail: bool) -> Mask {
        let mut m = Mask::new(0, 0, w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = ((x as f64 + 0.5 - cx) / rx, (y as f64 + 0.5 - cy) / ry);
                let mut inside = dx * dx + dy * dy <= 1.0;
                if tail {
                    // Tail: from the bottom of the ellipse down 60 px, 30 px wide at its root.
                    let t = (y as f64 + 0.5 - (cy + ry - 5.0)) / 60.0;
                    if (0.0..=1.0).contains(&t) {
                        let half = 15.0 * (1.0 - t);
                        let tx = cx + 0.4 * rx + 30.0 * t;
                        inside |= (x as f64 + 0.5 - tx).abs() <= half;
                    }
                }
                m.px[y * w + x] = inside;
            }
        }
        m
    }

    #[test]
    fn radius_follows_typer_3() {
        assert_eq!(opening_radius(200.0, 100.0), 10);
        assert_eq!(opening_radius(30.0, 20.0), 4, "minimum 4");
        assert_eq!(opening_radius(8.0, 40.0), 3, "capped at floor(min/2 - 1)");
        assert_eq!(opening_radius(3.0, 40.0), 0, "too thin");
        assert_eq!(opening_radius(1000.0, 400.0), 40);
        assert_eq!(opening_radius(f64::NAN, 4.0), 0);
        assert_eq!(opening_radius(45.0, 45.0), 5, "round(4.5) = 5");
    }

    #[test]
    fn opening_removes_the_tail() {
        let m = bubble(300, 260, 150.0, 100.0, 120.0, 70.0, true);
        let b = m.bounds().unwrap();
        assert!(b.y1 > 200, "the tail extends the box: {b:?}");
        let o = opened_bounds(&m).unwrap();
        let (cx, cy) = o.opened.center();
        assert!((cx - 150.0).abs() <= 1.0 && (cy - 100.0).abs() <= 1.0, "{o:?}");
        let (bx, by) = o.bbox.center();
        assert!(by - 100.0 > 20.0 || (bx - 150.0).abs() > 5.0, "the raw box centre is off: {bx},{by}");
        assert!(o.radius >= 4);
    }

    #[test]
    fn rectangles_open_to_themselves_and_holes_fill() {
        let mut m = Mask::new(10, 20, 100, 60).unwrap();
        for y in 25..75 {
            for x in 15..95 {
                m.set(x, y, true);
            }
        }
        // A hole (text) in the middle.
        for y in 40..55 {
            for x in 30..80 {
                m.set(x, y, false);
            }
        }
        let o = opened_bounds(&m).unwrap();
        assert_eq!(o.opened, Rect::new(15.0, 25.0, 80.0, 50.0));
        assert_eq!(o.bbox, o.opened);
        assert_eq!(o.area, 80 * 50, "the hole is filled");
    }

    #[test]
    fn thin_shapes_fall_back_and_tiny_ones_are_small() {
        // 3 px tall: nothing to open, the box is kept.
        let mut m = Mask::new(0, 0, 200, 10).unwrap();
        for x in 0..200 {
            for y in 2..5 {
                m.set(x, y, true);
            }
        }
        let o = opened_bounds(&m).unwrap();
        assert_eq!(o.radius, 0);
        assert_eq!(o.opened, Rect::new(0.0, 2.0, 200.0, 3.0));
        assert!(o.opened.area() >= MIN_AREA);
        let mut tiny = Mask::new(0, 0, 20, 20).unwrap();
        for y in 0..10 {
            for x in 0..10 {
                tiny.set(x, y, true);
            }
        }
        let o = opened_bounds(&tiny).unwrap();
        assert!(o.opened.area() < MIN_AREA);
        assert!(opened_bounds(&Mask::new(0, 0, 5, 5).unwrap()).is_none(), "empty mask");
    }

    #[test]
    fn rows_sample_the_outline() {
        let m = bubble(200, 120, 100.0, 60.0, 90.0, 50.0, false);
        let r = rows(&m, 21);
        assert_eq!(r.len(), 21);
        assert_eq!(r[0].y, 0.0);
        assert_eq!(r[20].y, 1.0);
        let mid = r[10];
        assert!(mid.width > 0.97 && (mid.left + mid.right - 1.0).abs() < 0.02, "{mid:?}");
        assert!(r[1].width < r[5].width && r[5].width < mid.width, "an ellipse narrows towards its ends");
        assert!(r[0].width > 0.0, "the edge bands probe inside the box");
        assert_eq!(rows(&m, 1).len(), 5, "clamped to 5..=31");
        assert_eq!(rows(&m, 1000).len(), 31);
        assert!(rows(&Mask::new(0, 0, 4, 4).unwrap(), 21).is_empty());
    }

    #[test]
    fn wand_finds_a_white_bubble_on_grey() {
        let (w, h) = (120usize, 100usize);
        let mut img = vec![[90u8, 90, 90, 255]; w * h];
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = ((x as f64 + 0.5 - 60.0) / 40.0, (y as f64 + 0.5 - 50.0) / 30.0);
                let d = dx * dx + dy * dy;
                if d <= 1.0 {
                    img[y * w + x] = [250, 250, 250, 255];
                } else if d <= 1.15 {
                    img[y * w + x] = [0, 0, 0, 255];
                }
            }
        }
        let area = photocraft_geom::Rect::from_xywh(0, 0, w as u32, h as u32);
        let m = wand(&img, area, (60, 50), WAND_TOLERANCE).unwrap();
        let ink = Rect::new(50.0, 45.0, 20.0, 10.0);
        let page = Rect::new(0.0, 0.0, w as f64, h as f64);
        let judge = Judge { text: Some((&ink, 5.0)), seed: pixel(&img, area, (60, 50)), page: Some(page) };
        let (hit, why) = hit(&m, &judge, 21);
        assert!(hit.found, "{why:?}");
        assert!((hit.center.0 - 60.0).abs() <= 1.0 && (hit.center.1 - 50.0).abs() <= 1.0, "{hit:?}");
        // Seeding on the grey page selects the page: far bigger than the text → no bubble.
        let page = wand(&img, area, (2, 2), WAND_TOLERANCE).unwrap();
        assert!(!hit_found(&page, &Rect::new(2.0, 2.0, 4.0, 4.0)));
        // A dark region is no bubble interior.
        let dark = Judge { seed: Some([20, 20, 20, 255]), ..Judge::default() };
        assert_eq!(super::hit(&m, &dark, 5).1, Some("noBubble"));
        // Text outside the region: not its bubble.
        let away = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(super::hit(&m, &Judge { text: Some((&away, 5.0)), ..Judge::default() }, 5).1, Some("noBubble"));
        // The page background touches the page edges.
        assert_eq!(super::hit(&page, &Judge { page: Some(Rect::new(0.0, 0.0, w as f64, h as f64)), ..Judge::default() }, 5).1, Some("noBubble"));
        assert!(wand(&img, area, (500, 2), WAND_TOLERANCE).is_none(), "seed outside the page");
        assert_eq!(seed_left_of(&Rect::new(3.0, 10.0, 20.0, 10.0)), (0, 15));
    }

    fn hit_found(m: &Mask, text: &Rect) -> bool {
        hit(m, &Judge { text: Some((text, 2.0)), ..Judge::default() }, 5).0.found
    }

    #[test]
    fn short_words_count_as_four_em() {
        assert_eq!(text_area(&Rect::new(0.0, 0.0, 40.0, 10.0), 20.0), 6400.0);
        assert_eq!(text_area(&Rect::new(0.0, 0.0, 400.0, 100.0), 20.0), 40_000.0);
        assert_eq!(text_area(&Rect::new(0.0, 0.0, 40.0, 10.0), f64::NAN), 400.0);
        assert!(too_big(&Rect::new(0.0, 0.0, 1000.0, 1000.0), 6400.0));
        assert!(!too_big(&Rect::new(0.0, 0.0, 300.0, 200.0), 6400.0));
        assert!(!too_big(&Rect::new(0.0, 0.0, 300.0, 200.0), 0.0));
    }

    #[test]
    fn hostile_sizes_are_refused() {
        assert!(Mask::new(0, 0, usize::MAX, 2).is_none());
        assert!(Mask::new(i32::MAX - 1, 0, 10, 2).is_none());
        assert!(Mask::from_coverage(0, 0, 2, 2, &[255; 3]).is_none());
        assert!(Rect::from_xywh(&[0.0, 0.0, f64::INFINITY, 1.0]).is_none());
        assert!(Rect::from_xywh(&[0.0, 0.0, -1.0, 1.0]).is_none());
        assert!(Rect::from_xywh(&[1.0, 2.0]).is_none());
        let m = Mask::new(-5, -5, 3, 3).unwrap();
        assert!(!m.get(i32::MIN, i32::MAX));
    }
}
