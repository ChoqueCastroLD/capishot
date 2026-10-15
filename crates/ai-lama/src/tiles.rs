//! Crop-around-mask tiling for LaMa: plan one working tile per group of masked blobs (with
//! context margin), cut it out at the model's working size, and paste the model's output back so
//! that only masked pixels change.
//!
//! Buffers are interleaved RGB `f32` in 0..=1 (`w·h·3`) and mask coverage `f32` in 0..=1 (`w·h`),
//! whatever the document's bit depth; the caller converts. Nothing here depends on burn, so the
//! editor's main module can use it while the model runs elsewhere (a worker module or a thread).
//!
//! Like lama-cleaner's "crop" strategy, each blob's bounding box grows by a margin; boxes that then
//! overlap merge as long as the merged crop still fits `max_side` (neighbouring tiles may share
//! context; each masked pixel is written by exactly one tile, its blob's). A tile whose crop fits within `max_side` runs at full resolution, padded on the
//! right and bottom by mirroring (`numpy.pad(mode="symmetric")`, as the reference does) up to a
//! multiple of 8; a larger crop is resized down to fit and its result resized back up.

use crate::{LamaError, MAX_WORK_SIDE, MIN_WORK_SIDE, Result, SIZE_MULTIPLE};

/// A pixel rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left.
    pub x: usize,
    /// Top.
    pub y: usize,
    /// Width.
    pub w: usize,
    /// Height.
    pub h: usize,
}

impl Rect {
    fn right(&self) -> usize {
        self.x + self.w
    }
    fn bottom(&self) -> usize {
        self.y + self.h
    }
    fn overlaps(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    fn union(&self, o: &Rect) -> Rect {
        let (x, y) = (self.x.min(o.x), self.y.min(o.y));
        Rect { x, y, w: self.right().max(o.right()) - x, h: self.bottom().max(o.bottom()) - y }
    }
    fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

/// How a tile maps its crop to the working size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Full resolution; the right and bottom are mirror-padded up to the working size.
    Pad,
    /// Resized to the working size and back.
    Resize,
}

/// Tiling options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileOptions {
    /// Longest working side; larger crops are downscaled to it (multiple of 8, 64..=2048).
    pub max_side: usize,
    /// Minimum context margin around each blob, in pixels.
    pub margin: usize,
}

impl Default for TileOptions {
    fn default() -> Self {
        Self { max_side: 512, margin: 64 }
    }
}

/// One unit of model work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    /// The crop in image coordinates (inside the image).
    pub rect: Rect,
    /// The masked blobs this tile is responsible for (inside `rect`): only masked pixels in
    /// here are written back, so tiles never fight over a pixel.
    pub core: Rect,
    /// Working width (multiple of 8).
    pub work_w: usize,
    /// Working height (multiple of 8).
    pub work_h: usize,
    /// Crop-to-working-size mapping.
    pub fit: Fit,
}

/// The model's input for one tile.
#[derive(Debug, Clone, PartialEq)]
pub struct TileInput {
    /// Interleaved RGB, `work_w·work_h·3`.
    pub rgb: Vec<f32>,
    /// Binary mask (0 or 1), `work_w·work_h`.
    pub mask: Vec<f32>,
    /// Working width.
    pub w: usize,
    /// Working height.
    pub h: usize,
}

fn check_buffers(rgb_len: usize, mask: &[f32], w: usize, h: usize) -> Result<()> {
    let n = w.checked_mul(h).ok_or_else(|| LamaError::Input("image size overflows".into()))?;
    if w == 0 || h == 0 {
        return Err(LamaError::Input("empty image".into()));
    }
    if mask.len() != n || rgb_len != n.saturating_mul(3) {
        return Err(LamaError::Input(format!("{w}x{h} needs {} RGB and {n} mask samples, got {rgb_len} and {}", n.saturating_mul(3), mask.len())));
    }
    Ok(())
}

fn round_up8(v: usize) -> usize {
    v.div_ceil(SIZE_MULTIPLE) * SIZE_MULTIPLE
}

/// Bounding boxes of the 8-connected masked blobs (coverage > 0).
fn blobs(mask: &[f32], w: usize, h: usize) -> Vec<Rect> {
    let mut seen = vec![false; mask.len()];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..mask.len() {
        if seen[start] || mask[start] <= 0.0 {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            for dy in [-1isize, 0, 1] {
                for dx in [-1isize, 0, 1] {
                    let (nx, ny) = (x as isize + dx, y as isize + dy);
                    if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if !seen[j] && mask[j] > 0.0 {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        out.push(Rect { x: x0, y: y0, w: x1 - x0 + 1, h: y1 - y0 + 1 });
    }
    out
}

fn expand(r: &Rect, m: usize, w: usize, h: usize) -> Rect {
    let x = r.x.saturating_sub(m);
    let y = r.y.saturating_sub(m);
    Rect { x, y, w: (r.right() + m).min(w) - x, h: (r.bottom() + m).min(h) - y }
}

/// Grows `r` (inside a `w×h` image) towards `tw×th`, keeping it inside the image.
fn grow_to(r: Rect, tw: usize, th: usize, w: usize, h: usize) -> Rect {
    let fit = |pos: usize, len: usize, target: usize, max: usize| -> (usize, usize) {
        let len2 = target.min(max).max(len);
        let extra = len2 - len;
        let start = pos.saturating_sub(extra / 2).min(max - len2);
        (start, len2)
    };
    let (x, nw) = fit(r.x, r.w, tw, w);
    let (y, nh) = fit(r.y, r.h, th, h);
    Rect { x, y, w: nw, h: nh }
}

/// Plans the tiles for a `w×h` mask. An empty mask gives no tiles.
pub fn plan(mask: &[f32], w: usize, h: usize, opts: &TileOptions) -> Result<Vec<Tile>> {
    check_buffers(w.saturating_mul(h).saturating_mul(3), mask, w, h)?;
    if !opts.max_side.is_multiple_of(SIZE_MULTIPLE) || !(MIN_WORK_SIDE..=MAX_WORK_SIDE).contains(&opts.max_side) {
        return Err(LamaError::Input(format!("max_side {} must be a multiple of {SIZE_MULTIPLE} in {MIN_WORK_SIDE}..={MAX_WORK_SIDE}", opts.max_side)));
    }
    // Each group: (core = union of blob boxes, crop = union of expanded boxes).
    let mut groups: Vec<(Rect, Rect)> = blobs(mask, w, h)
        .into_iter()
        .map(|b| {
            let m = opts.margin.max(b.w.max(b.h) / 4);
            (b, expand(&b, m, w, h))
        })
        .collect();
    // Merge overlapping crops while the union still runs at full resolution (a downscaled tile
    // blurs screentone), and always when the cores overlap (each pixel has one owner).
    let mut merged = true;
    while merged {
        merged = false;
        let mut i = 0;
        while i < groups.len() {
            let mut j = i + 1;
            while j < groups.len() {
                let (a, b) = (&groups[i], &groups[j]);
                let u = a.1.union(&b.1);
                if a.0.overlaps(&b.0) || (a.1.overlaps(&b.1) && u.w.max(u.h) <= opts.max_side) {
                    let (core, crop) = groups.swap_remove(j);
                    groups[i] = (groups[i].0.union(&core), groups[i].1.union(&crop));
                    merged = true;
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
    }
    groups.sort_by_key(|(core, _)| (core.y, core.x));
    let mut tiles = Vec::with_capacity(groups.len());
    for (core, crop) in groups {
        let long = crop.w.max(crop.h);
        if long <= opts.max_side {
            // Full resolution: use real pixels for the padding where the image has them.
            let (tw, th) = (round_up8(crop.w.max(MIN_WORK_SIDE)), round_up8(crop.h.max(MIN_WORK_SIDE)));
            let rect = grow_to(crop, tw, th, w, h);
            tiles.push(Tile { rect, core, work_w: tw, work_h: th, fit: Fit::Pad });
        } else {
            let s = opts.max_side as f64 / long as f64;
            let side = |v: usize| round_up8(((v as f64 * s).ceil() as usize).max(MIN_WORK_SIDE)).min(opts.max_side);
            tiles.push(Tile { rect: crop, core, work_w: side(crop.w), work_h: side(crop.h), fit: Fit::Resize });
        }
    }
    Ok(tiles)
}

/// Mirror index for `numpy.pad(mode="symmetric")`: …2 1 0 | 0 1 2 … n−1 | n−1 n−2 …
fn mirror(i: usize, n: usize) -> usize {
    let j = i % (2 * n);
    if j < n { j } else { 2 * n - 1 - j }
}

fn check_tile(t: &Tile, w: usize, h: usize) -> Result<()> {
    let ok = t.rect.w > 0
        && t.rect.h > 0
        && t.rect.right() <= w
        && t.rect.bottom() <= h
        && t.work_w.is_multiple_of(SIZE_MULTIPLE)
        && t.work_h.is_multiple_of(SIZE_MULTIPLE)
        && (MIN_WORK_SIDE..=MAX_WORK_SIDE).contains(&t.work_w)
        && (MIN_WORK_SIDE..=MAX_WORK_SIDE).contains(&t.work_h)
        && (t.fit == Fit::Resize || (t.rect.w <= t.work_w && t.rect.h <= t.work_h));
    if ok { Ok(()) } else { Err(LamaError::Input(format!("tile {t:?} does not fit a {w}x{h} image"))) }
}

fn crop_buf(src: &[f32], w: usize, ch: usize, r: &Rect) -> Vec<f32> {
    let mut out = Vec::with_capacity(r.w * r.h * ch);
    for y in r.y..r.bottom() {
        out.extend_from_slice(&src[(y * w + r.x) * ch..(y * w + r.right()) * ch]);
    }
    out
}

/// Cuts a tile's model input out of the image.
pub fn extract(rgb: &[f32], mask: &[f32], w: usize, h: usize, t: &Tile) -> Result<TileInput> {
    check_buffers(rgb.len(), mask, w, h)?;
    check_tile(t, w, h)?;
    let (tw, th) = (t.work_w, t.work_h);
    let r = t.rect;
    let bin = |m: f32| if m > 0.0 { 1.0 } else { 0.0 };
    let (out_rgb, out_mask) = match t.fit {
        Fit::Pad => {
            let mut o = Vec::with_capacity(tw * th * 3);
            let mut m = Vec::with_capacity(tw * th);
            for y in 0..th {
                let sy = r.y + mirror(y, r.h);
                for x in 0..tw {
                    let i = sy * w + r.x + mirror(x, r.w);
                    o.extend_from_slice(&rgb[i * 3..i * 3 + 3]);
                    m.push(bin(mask[i]));
                }
            }
            (o, m)
        }
        Fit::Resize => {
            let c = crop_buf(rgb, w, 3, &r);
            let cm = crop_buf(mask, w, 1, &r);
            let o = resample(&c, r.w, r.h, 3, tw, th);
            let m = resample(&cm, r.w, r.h, 1, tw, th).into_iter().map(|v| if v > 1e-4 { 1.0 } else { 0.0 }).collect();
            (o, m)
        }
    };
    Ok(TileInput { rgb: out_rgb, mask: out_mask, w: tw, h: th })
}

/// Writes a tile's model output (`work_w·work_h·3`) back into `rgb`: each masked pixel of the
/// tile's core becomes `orig + (out − orig)·coverage`; nothing else changes. Returns the number
/// of pixels written.
pub fn paste(rgb: &mut [f32], mask: &[f32], w: usize, h: usize, t: &Tile, out: &[f32]) -> Result<usize> {
    check_buffers(rgb.len(), mask, w, h)?;
    check_tile(t, w, h)?;
    if out.len() != t.work_w * t.work_h * 3 {
        return Err(LamaError::Input(format!("tile output has {} samples, expected {}", out.len(), t.work_w * t.work_h * 3)));
    }
    let r = t.rect;
    let full;
    let (src, sw) = match t.fit {
        Fit::Pad => (out, t.work_w),
        Fit::Resize => {
            full = resample(out, t.work_w, t.work_h, 3, r.w, r.h);
            (&full[..], r.w)
        }
    };
    let mut n = 0;
    for y in r.y..r.bottom() {
        for x in r.x..r.right() {
            let i = y * w + x;
            let a = mask[i];
            if a <= 0.0 || !t.core.contains(x, y) {
                continue;
            }
            let a = a.min(1.0);
            let s = ((y - r.y) * sw + (x - r.x)) * 3;
            for c in 0..3 {
                let o = rgb[i * 3 + c];
                rgb[i * 3 + c] = o + (src[s + c].clamp(0.0, 1.0) - o) * a;
            }
            n += 1;
        }
    }
    Ok(n)
}

/// Separable resampling with a triangle (bilinear) kernel, widened when downscaling so it
/// averages instead of aliasing (like Pillow's `BILINEAR` with its antialiasing support).
pub fn resample(src: &[f32], sw: usize, sh: usize, ch: usize, dw: usize, dh: usize) -> Vec<f32> {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 || ch == 0 || src.len() != sw * sh * ch {
        return vec![0.0; dw * dh * ch];
    }
    let horiz = weights(sw, dw);
    let mut tmp = vec![0f32; dw * sh * ch];
    for y in 0..sh {
        for (x, (start, ws)) in horiz.iter().enumerate() {
            for (k, wgt) in ws.iter().enumerate() {
                let s = (y * sw + start + k) * ch;
                let d = (y * dw + x) * ch;
                for c in 0..ch {
                    tmp[d + c] += src[s + c] * wgt;
                }
            }
        }
    }
    let vert = weights(sh, dh);
    let mut out = vec![0f32; dw * dh * ch];
    for (y, (start, ws)) in vert.iter().enumerate() {
        for (k, wgt) in ws.iter().enumerate() {
            let s = (start + k) * dw * ch;
            let d = y * dw * ch;
            for i in 0..dw * ch {
                out[d + i] += tmp[s + i] * wgt;
            }
        }
    }
    out
}

/// Per output index: first source index and normalized weights.
fn weights(src: usize, dst: usize) -> Vec<(usize, Vec<f32>)> {
    let scale = src as f64 / dst as f64;
    let support = scale.max(1.0);
    (0..dst)
        .map(|i| {
            let center = (i as f64 + 0.5) * scale;
            let lo = ((center - support).floor().max(0.0)) as usize;
            let hi = ((center + support).ceil() as usize).min(src);
            let mut ws: Vec<f64> = (lo..hi).map(|j| (1.0 - ((j as f64 + 0.5 - center) / support).abs()).max(0.0)).collect();
            let sum: f64 = ws.iter().sum();
            if sum > 0.0 {
                ws.iter_mut().for_each(|v| *v /= sum);
            } else {
                // Unreachable for valid sizes; fall back to the nearest sample.
                let near = (center.floor() as usize).min(src - 1);
                return (near, vec![1.0]);
            }
            (lo, ws.into_iter().map(|v| v as f32).collect())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask_with(w: usize, h: usize, rects: &[Rect]) -> Vec<f32> {
        let mut m = vec![0f32; w * h];
        for r in rects {
            for y in r.y..r.bottom() {
                for x in r.x..r.right() {
                    m[y * w + x] = 1.0;
                }
            }
        }
        m
    }

    fn rgb(w: usize, h: usize) -> Vec<f32> {
        (0..w * h * 3).map(|i| (i % 251) as f32 / 250.0).collect()
    }

    #[test]
    fn empty_mask_has_no_tiles() {
        assert!(plan(&[0.0; 100 * 80], 100, 80, &TileOptions::default()).unwrap().is_empty());
    }

    #[test]
    fn far_apart_blobs_get_separate_full_res_tiles() {
        let (w, h) = (1000, 800);
        let a = Rect { x: 10, y: 10, w: 30, h: 20 };
        let b = Rect { x: 800, y: 600, w: 50, h: 40 };
        let tiles = plan(&mask_with(w, h, &[a, b]), w, h, &TileOptions::default()).unwrap();
        assert_eq!(tiles.len(), 2);
        for t in &tiles {
            assert_eq!(t.fit, Fit::Pad);
            assert_eq!(t.work_w % 8, 0);
            assert!(t.rect.w == t.work_w && t.rect.h == t.work_h, "grown to the working size: {t:?}");
            assert!(t.rect.right() <= w && t.rect.bottom() <= h);
        }
        assert_eq!(tiles[0].core, a);
        assert_eq!(tiles[1].core, b);
    }

    #[test]
    fn near_blobs_merge_while_they_fit_and_big_crops_resize() {
        let (w, h) = (2000, 1500);
        let o = TileOptions::default();
        // Two words 40 px apart: one full-resolution tile.
        let a = Rect { x: 100, y: 100, w: 120, h: 40 };
        let b = Rect { x: 260, y: 100, w: 100, h: 40 };
        let tiles = plan(&mask_with(w, h, &[a, b]), w, h, &o).unwrap();
        assert_eq!(tiles.len(), 1);
        assert_eq!((tiles[0].fit, tiles[0].core), (Fit::Pad, a.union(&b)));
        // Two big blocks whose union would need downscaling stay separate tiles.
        let a = Rect { x: 100, y: 100, w: 300, h: 200 };
        let b = Rect { x: 420, y: 120, w: 300, h: 200 };
        let tiles = plan(&mask_with(w, h, &[a, b]), w, h, &o).unwrap();
        assert_eq!(tiles.len(), 2);
        assert!(tiles.iter().all(|t| t.fit == Fit::Pad));
        // One blob larger than max_side is resized.
        let big = Rect { x: 100, y: 100, w: 900, h: 300 };
        let t = plan(&mask_with(w, h, &[big]), w, h, &o).unwrap()[0];
        assert_eq!(t.fit, Fit::Resize);
        assert!(t.work_w <= 512 && t.work_h <= 512 && t.work_w.is_multiple_of(8) && t.work_h.is_multiple_of(8));
        // A dot inside an L-shaped blob's bounding box: the cores overlap, so they merge even
        // though the union needs downscaling.
        let l1 = Rect { x: 100, y: 100, w: 600, h: 20 };
        let l2 = Rect { x: 100, y: 100, w: 20, h: 600 };
        let dot = Rect { x: 400, y: 400, w: 10, h: 10 };
        let tiles = plan(&mask_with(w, h, &[l1, l2, dot]), w, h, &o).unwrap();
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].core, l1.union(&l2));
    }

    #[test]
    fn small_image_pads_by_mirroring() {
        let (w, h) = (20, 12);
        let m = mask_with(w, h, &[Rect { x: 5, y: 5, w: 3, h: 3 }]);
        let img = rgb(w, h);
        let tiles = plan(&m, w, h, &TileOptions::default()).unwrap();
        assert_eq!(tiles.len(), 1);
        let t = tiles[0];
        assert_eq!((t.work_w, t.work_h, t.fit), (64, 64, Fit::Pad));
        assert_eq!(t.rect, Rect { x: 0, y: 0, w, h });
        let inp = extract(&img, &m, w, h, &t).unwrap();
        // Column 20 mirrors column 19, column 21 mirrors 18.
        assert_eq!(inp.rgb[20 * 3..21 * 3], img[19 * 3..20 * 3]);
        assert_eq!(inp.rgb[21 * 3..22 * 3], img[18 * 3..19 * 3]);
        assert_eq!(inp.mask.iter().filter(|v| **v > 0.0).count() % 9, 0);
    }

    #[test]
    fn paste_touches_only_masked_core_pixels() {
        let (w, h) = (300, 200);
        let hole = Rect { x: 100, y: 80, w: 40, h: 10 };
        let mut m = mask_with(w, h, &[hole]);
        m[85 * w + 120] = 0.5; // soft coverage blends halfway
        let orig = rgb(w, h);
        let tiles = plan(&m, w, h, &TileOptions::default()).unwrap();
        let t = tiles[0];
        let mut img = orig.clone();
        let out = vec![0.25f32; t.work_w * t.work_h * 3];
        let n = paste(&mut img, &m, w, h, &t, &out).unwrap();
        assert_eq!(n, 400);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                let a = m[y * w + x];
                for c in 0..3 {
                    let want = orig[i + c] + (0.25 - orig[i + c]) * a;
                    assert!((img[i + c] - want).abs() < 1e-6, "({x},{y})");
                }
            }
        }
    }

    #[test]
    fn resize_round_trip_of_flat_color_is_exact() {
        let src = vec![0.5f32; 700 * 300 * 3];
        let small = resample(&src, 700, 300, 3, 512, 224);
        assert!(small.iter().all(|v| (v - 0.5).abs() < 1e-5));
        let back = resample(&small, 512, 224, 3, 700, 300);
        assert!(back.iter().all(|v| (v - 0.5).abs() < 1e-5));
    }

    #[test]
    fn resized_tile_round_trips_through_extract_and_paste() {
        let (w, h) = (1200, 900);
        let m = mask_with(w, h, &[Rect { x: 200, y: 200, w: 700, h: 400 }]);
        let img = vec![0.75f32; w * h * 3];
        let t = plan(&m, w, h, &TileOptions::default()).unwrap()[0];
        assert_eq!(t.fit, Fit::Resize);
        let inp = extract(&img, &m, w, h, &t).unwrap();
        assert_eq!(inp.rgb.len(), t.work_w * t.work_h * 3);
        let mut out = img.clone();
        let gen_out = vec![0.1f32; t.work_w * t.work_h * 3];
        paste(&mut out, &m, w, h, &t, &gen_out).unwrap();
        assert!((out[(400 * w + 500) * 3] - 0.1).abs() < 1e-5);
        assert_eq!(out[(100 * w + 100) * 3], 0.75);
    }

    #[test]
    fn bad_inputs_are_errors() {
        let o = TileOptions::default();
        assert!(plan(&[1.0; 10], 4, 4, &o).is_err());
        assert!(plan(&[1.0; 16], 4, 4, &TileOptions { max_side: 500, margin: 0 }).is_err());
        assert!(plan(&[1.0; 16], 4, 4, &TileOptions { max_side: 8, margin: 0 }).is_err());
        assert!(plan(&[], 0, 0, &o).is_err());
        let t = Tile { rect: Rect { x: 0, y: 0, w: 10, h: 10 }, core: Rect { x: 0, y: 0, w: 10, h: 10 }, work_w: 64, work_h: 64, fit: Fit::Pad };
        let (img, m) = (rgb(8, 8), vec![1.0; 64]);
        assert!(extract(&img, &m, 8, 8, &t).is_err(), "tile larger than the image");
        let mut img2 = rgb(16, 16);
        assert!(paste(&mut img2, &[1.0; 256], 16, 16, &t, &[0.0; 5]).is_err());
        assert!(resample(&[0.0; 5], 2, 2, 3, 4, 4).len() == 48);
    }
}
