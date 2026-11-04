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
//! multiple of 8; a larger crop is resized down to fit and its result resized back up, or, with
//! [`TileOptions::full_res`], split into overlapping full-resolution tiles of at most `max_side`
//! a side ([`Split`]): they run in rows, top to bottom, left to right; each sees the real pixels
//! around it and what the tiles before it filled as context, regenerates a band of that fill
//! next to its own hole and feathers into it, so no seam shows (screentone keeps its phase).

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
    /// A crop larger than `max_side` is split into overlapping full-resolution tiles (see
    /// [`Split`]) instead of resized down (which blurs screentone and fine detail).
    pub full_res: bool,
}

impl Default for TileOptions {
    fn default() -> Self {
        Self { max_side: 512, margin: 64, full_res: false }
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
    /// One of the overlapping full-resolution tiles a large crop was split into: cut out and
    /// pasted with [`Split::extract`] and [`Split::paste`] (its `core` is the whole crop's).
    pub split: bool,
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
            tiles.push(Tile { rect, core, work_w: tw, work_h: th, fit: Fit::Pad, split: false });
        } else if opts.full_res {
            split_grid(&mut tiles, core, crop, mask, w, h, opts.max_side);
        } else {
            let s = opts.max_side as f64 / long as f64;
            let side = |v: usize| round_up8(((v as f64 * s).ceil() as usize).max(MIN_WORK_SIDE)).min(opts.max_side);
            tiles.push(Tile { rect: crop, core, work_w: side(crop.w), work_h: side(crop.h), fit: Fit::Resize, split: false });
        }
    }
    Ok(tiles)
}

/// How far neighbouring tiles of a split crop overlap: a quarter of the side.
pub fn split_overlap(side: usize) -> usize {
    side / 4
}

/// How far into what earlier tiles filled a split tile regenerates and feathers: half the overlap
/// (the other half stays as context, so the new fill continues the old one's pattern).
pub fn split_band(side: usize) -> usize {
    split_overlap(side) / 2
}

/// Tile spans `(start, len)` covering `start..start + len`, at most `side` long, overlapping by at
/// least [`split_overlap`] and evenly spread.
fn spans(start: usize, len: usize, side: usize) -> Vec<(usize, usize)> {
    if len <= side {
        return vec![(start, len)];
    }
    let overlap = split_overlap(side);
    let n = (len - overlap).div_ceil(side - overlap).max(2);
    (0..n).map(|i| (start + (i * (len - side) + (n - 1) / 2) / (n - 1), side)).collect()
}

/// The overlapping full-resolution tiles of a crop larger than `side`, in rows, skipping those
/// without any of the core's hole.
fn split_grid(out: &mut Vec<Tile>, core: Rect, crop: Rect, mask: &[f32], w: usize, h: usize, side: usize) {
    for (y, th) in spans(crop.y, crop.h, side) {
        for (x, tw) in spans(crop.x, crop.w, side) {
            let r = Rect { x, y, w: tw, h: th };
            let hole = (r.y.max(core.y)..r.bottom().min(core.bottom())).any(|yy| (r.x.max(core.x)..r.right().min(core.right())).any(|xx| mask[yy * w + xx] > 0.0));
            if !hole {
                continue;
            }
            let (ww, wh) = (round_up8(tw.max(MIN_WORK_SIDE)), round_up8(th.max(MIN_WORK_SIDE)));
            let rect = grow_to(r, ww, wh, w, h);
            out.push(Tile { rect, core, work_w: ww, work_h: wh, fit: Fit::Pad, split: true });
        }
    }
}

/// What the split tiles of a run have filled so far ([`Tile::split`]): later tiles take it as
/// context, except a band of [`split_band`] pixels next to their own unfilled hole, which they
/// regenerate and blend into with a linear ramp.
#[derive(Debug, Clone)]
pub struct Split {
    w: usize,
    h: usize,
    band: usize,
    done: Vec<bool>,
}

impl Split {
    /// For a `w×h` image tiled at `max_side`.
    pub fn new(w: usize, h: usize, max_side: usize) -> Result<Self> {
        let n = w.checked_mul(h).filter(|n| *n > 0).ok_or_else(|| LamaError::Input("empty image".into()))?;
        Ok(Self { w, h, band: split_band(max_side).max(1), done: vec![false; n] })
    }

    /// Forgets what was filled (a run that starts its tiles over).
    pub fn restart(&mut self) {
        self.done.fill(false);
    }

    /// A split tile's model input and, per pixel of its `rect`, how much of its output to paste
    /// (0 = nothing). `None` when earlier tiles already filled all of its hole.
    pub fn extract(&self, rgb: &[f32], mask: &[f32], t: &Tile) -> Result<Option<(TileInput, Vec<f32>)>> {
        let (w, h) = (self.w, self.h);
        check_buffers(rgb.len(), mask, w, h)?;
        check_tile(t, w, h)?;
        if t.fit != Fit::Pad {
            return Err(LamaError::Input("a split tile runs at full resolution".into()));
        }
        let r = t.rect;
        let at = |lx: usize, ly: usize| (r.y + ly) * w + r.x + lx;
        let own = |lx: usize, ly: usize| t.core.contains(r.x + lx, r.y + ly);
        // Chessboard distance to the nearest own hole pixel no tile filled yet, capped past the band.
        let cap = u32::try_from(self.band).unwrap_or(u32::MAX).saturating_add(1);
        let mut dist = vec![cap; r.w * r.h];
        let mut fresh = false;
        for ly in 0..r.h {
            for lx in 0..r.w {
                let i = at(lx, ly);
                if mask[i] > 0.0 && !self.done[i] && own(lx, ly) {
                    dist[ly * r.w + lx] = 0;
                    fresh = true;
                }
            }
        }
        if !fresh {
            return Ok(None);
        }
        chamfer(&mut dist, r.w, r.h);
        let band = self.band as f32 + 1.0;
        let mut weights = vec![0f32; r.w * r.h];
        let mut local_mask = vec![0f32; r.w * r.h];
        for ly in 0..r.h {
            for lx in 0..r.w {
                let (i, j) = (at(lx, ly), ly * r.w + lx);
                if mask[i] <= 0.0 {
                    continue;
                }
                let wgt = match (own(lx, ly), self.done[i]) {
                    (false, _) => 0.0,
                    (true, false) => 1.0,
                    (true, true) => (1.0 - dist[j] as f32 / band).max(0.0),
                };
                weights[j] = wgt;
                if !self.done[i] || wgt > 0.0 {
                    local_mask[j] = 1.0;
                }
            }
        }
        let (tw, th) = (t.work_w, t.work_h);
        let mut o = Vec::with_capacity(tw * th * 3);
        let mut m = Vec::with_capacity(tw * th);
        for y in 0..th {
            let ly = mirror(y, r.h);
            for x in 0..tw {
                let lx = mirror(x, r.w);
                let i = at(lx, ly);
                o.extend_from_slice(&rgb[i * 3..i * 3 + 3]);
                m.push(local_mask[ly * r.w + lx]);
            }
        }
        Ok(Some((TileInput { rgb: o, mask: m, w: tw, h: th }, weights)))
    }

    /// Writes a split tile's model output into `rgb` with `weights` (from [`Split::extract`]):
    /// `cur + (out − cur)·weight`, and marks those pixels filled. The hole counts as fully
    /// covered (blend soft coverage afterwards, as [`paste`] with a binary mask). Returns the
    /// number of pixels written.
    pub fn paste(&mut self, rgb: &mut [f32], t: &Tile, out: &[f32], weights: &[f32]) -> Result<usize> {
        let (w, h) = (self.w, self.h);
        if rgb.len() != w * h * 3 {
            return Err(LamaError::Input(format!("{w}x{h} needs {} RGB samples, got {}", w * h * 3, rgb.len())));
        }
        check_tile(t, w, h)?;
        let r = t.rect;
        if t.fit != Fit::Pad || out.len() != t.work_w * t.work_h * 3 || weights.len() != r.w * r.h {
            return Err(LamaError::Input(format!("split tile output has {} samples and {} weights, expected {} and {}", out.len(), weights.len(), t.work_w * t.work_h * 3, r.w * r.h)));
        }
        let mut n = 0;
        for ly in 0..r.h {
            for lx in 0..r.w {
                let wgt = weights[ly * r.w + lx];
                if wgt <= 0.0 || !wgt.is_finite() {
                    continue;
                }
                let wgt = wgt.min(1.0);
                let i = (r.y + ly) * w + r.x + lx;
                let s = (ly * t.work_w + lx) * 3;
                for c in 0..3 {
                    let o = rgb[i * 3 + c];
                    rgb[i * 3 + c] = o + (out[s + c].clamp(0.0, 1.0) - o) * wgt;
                }
                self.done[i] = true;
                n += 1;
            }
        }
        Ok(n)
    }
}

/// Two-pass chessboard distance transform of `d` (`w×h`; seeds 0, the rest at a cap).
fn chamfer(d: &mut [u32], w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            let mut v = d[y * w + x];
            if x > 0 {
                v = v.min(d[y * w + x - 1] + 1);
            }
            if y > 0 {
                let up = (y - 1) * w;
                v = v.min(d[up + x] + 1);
                if x > 0 {
                    v = v.min(d[up + x - 1] + 1);
                }
                if x + 1 < w {
                    v = v.min(d[up + x + 1] + 1);
                }
            }
            d[y * w + x] = v;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let mut v = d[y * w + x];
            if x + 1 < w {
                v = v.min(d[y * w + x + 1] + 1);
            }
            if y + 1 < h {
                let dn = (y + 1) * w;
                v = v.min(d[dn + x] + 1);
                if x > 0 {
                    v = v.min(d[dn + x - 1] + 1);
                }
                if x + 1 < w {
                    v = v.min(d[dn + x + 1] + 1);
                }
            }
            d[y * w + x] = v;
        }
    }
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
        assert!(plan(&[1.0; 16], 4, 4, &TileOptions { max_side: 500, margin: 0, full_res: false }).is_err());
        assert!(plan(&[1.0; 16], 4, 4, &TileOptions { max_side: 8, margin: 0, full_res: true }).is_err());
        assert!(plan(&[], 0, 0, &o).is_err());
        let t = Tile { rect: Rect { x: 0, y: 0, w: 10, h: 10 }, core: Rect { x: 0, y: 0, w: 10, h: 10 }, work_w: 64, work_h: 64, fit: Fit::Pad, split: false };
        let (img, m) = (rgb(8, 8), vec![1.0; 64]);
        assert!(extract(&img, &m, 8, 8, &t).is_err(), "tile larger than the image");
        let mut img2 = rgb(16, 16);
        assert!(paste(&mut img2, &[1.0; 256], 16, 16, &t, &[0.0; 5]).is_err());
        assert!(resample(&[0.0; 5], 2, 2, 3, 4, 4).len() == 48);
    }

    #[test]
    fn big_crops_split_into_overlapping_full_res_tiles() {
        let (w, h) = (2000, 1500);
        let full = TileOptions { full_res: true, ..TileOptions::default() };
        // A small hole plans as without full_res.
        let small = mask_with(w, h, &[Rect { x: 100, y: 100, w: 120, h: 40 }]);
        assert_eq!(plan(&small, w, h, &full).unwrap(), plan(&small, w, h, &TileOptions::default()).unwrap());
        let hole = Rect { x: 100, y: 100, w: 900, h: 300 };
        let m = mask_with(w, h, &[hole]);
        let t = plan(&m, w, h, &full).unwrap();
        assert!(t.len() > 2, "{t:?}");
        for a in &t {
            assert!(a.split && a.fit == Fit::Pad && a.core == hole, "{a:?}");
            assert!(a.rect.w <= 512 && a.rect.h <= 512 && a.rect.w == a.work_w && a.rect.h == a.work_h, "{a:?}");
            assert!(a.rect.right() <= w && a.rect.bottom() <= h);
        }
        // Neighbours overlap by at least a quarter of the side, and every hole pixel is in a tile.
        for p in t.windows(2).filter(|p| p[0].rect.y == p[1].rect.y) {
            assert!(p[0].rect.right() >= p[1].rect.x + split_overlap(512), "{p:?}");
        }
        for y in hole.y..hole.bottom() {
            for x in hole.x..hole.right() {
                assert!(t.iter().any(|a| a.rect.contains(x, y)), "({x},{y}) uncovered");
            }
        }
        // Tiles with none of the hole are skipped (an L-shaped hole's empty corner).
        let l = mask_with(w, h, &[Rect { x: 100, y: 100, w: 1400, h: 30 }, Rect { x: 100, y: 100, w: 30, h: 1200 }]);
        let t = plan(&l, w, h, &full).unwrap();
        assert!(t.iter().all(|a| !a.rect.contains(1000, 1000)), "nothing far from the L");
    }

    #[test]
    fn split_extract_and_paste_reject_bad_input() {
        let (w, h) = (100, 80);
        assert!(Split::new(0, 5, 512).is_err());
        let mut sp = Split::new(w, h, 64).unwrap();
        let t = Tile { rect: Rect { x: 0, y: 0, w: 64, h: 64 }, core: Rect { x: 0, y: 0, w: 64, h: 64 }, work_w: 64, work_h: 64, fit: Fit::Pad, split: true };
        let (img, m) = (rgb(w, h), vec![1.0; w * h]);
        assert!(sp.extract(&img, &m[..10], &t).is_err());
        assert!(sp.extract(&img, &m, &Tile { fit: Fit::Resize, ..t }).is_err());
        assert!(sp.extract(&img, &m, &Tile { rect: Rect { x: 50, ..t.rect }, ..t }).is_err(), "outside the image");
        let (inp, wts) = sp.extract(&img, &m, &t).unwrap().unwrap();
        let mut img2 = img.clone();
        assert!(sp.paste(&mut img2, &t, &inp.rgb[..5], &wts).is_err());
        assert!(sp.paste(&mut img2, &t, &inp.rgb, &wts[..5]).is_err());
        assert!(sp.paste(&mut img2[..9], &t, &inp.rgb, &wts).is_err());
        assert_eq!(sp.paste(&mut img2, &t, &inp.rgb, &wts).unwrap(), 64 * 64);
        assert!(sp.extract(&img2, &m, &t).unwrap().is_none(), "all filled");
        sp.restart();
        assert!(sp.extract(&img2, &m, &t).unwrap().is_some());
    }

    /// A screentone (8 px dots) under a grid of building windows (40 px).
    fn manga(x: usize, y: usize) -> f32 {
        let (dx, dy) = ((x % 8) as f32 - 3.5, (y % 8) as f32 - 3.5);
        if x % 40 < 4 || y % 40 < 4 {
            0.05
        } else if dx * dx + dy * dy < 5.0 {
            0.2
        } else {
            0.85
        }
    }

    /// A content-aware stand-in for the model, local to its input: it finds the pattern's period
    /// along x and y from the known pixels and copies along it into the hole (an unknown period, a
    /// downscaled tile, gets a poor copy, as a real model blurs), plus an offset that differs
    /// from tile to tile (models drift: a hard seam would show it) and a little noise of its own.
    fn periodic_fake(inp: &TileInput, offset: f32) -> Vec<f32> {
        let (w, h) = (inp.w, inp.h);
        let g = |x: usize, y: usize| inp.rgb[(y * w + x) * 3];
        let known = |x: usize, y: usize| inp.mask[y * w + x] == 0.0;
        let period = |horiz: bool| {
            let mut best = (f32::MAX, 1);
            for p in 2..=64usize {
                let (mut e, mut n) = (0.0, 0u32);
                for y in (0..h).step_by(3) {
                    for x in (0..w).step_by(3) {
                        let (x2, y2) = if horiz { (x + p, y) } else { (x, y + p) };
                        if x2 < w && y2 < h && known(x, y) && known(x2, y2) {
                            e += (g(x, y) - g(x2, y2)).abs();
                            n += 1;
                        }
                    }
                }
                if n > 50 && e / (n as f32) < best.0 - 1e-4 {
                    best = (e / n as f32, p);
                }
            }
            best.1
        };
        let (px, py) = (period(true), period(false));
        let mut out = inp.rgb.clone();
        for y in 0..h {
            for x in 0..w {
                if known(x, y) {
                    continue;
                }
                // The nearest known pixel a whole number of periods to the left, else above, right,
                // below: one consistent source, as a model continues what it sees.
                let (x0, y0) = (x as isize, y as isize);
                let dirs = [(-(px as isize), 0), (0, -(py as isize)), (px as isize, 0), (0, py as isize)];
                let v = dirs
                    .iter()
                    .find_map(|(dx, dy)| {
                        (1..w.max(h) as isize).map(|k| (x0 + k * dx, y0 + k * dy)).take_while(|(cx, cy)| *cx >= 0 && *cy >= 0 && (*cx as usize) < w && (*cy as usize) < h).find(|(cx, cy)| known(*cx as usize, *cy as usize))
                    })
                    .map_or(0.5, |(cx, cy)| g(cx as usize, cy as usize));
                let noise = ((x * 7919 + y * 104_729 + (offset.to_bits() as usize % 997)) % 1000) as f32 / 1000.0 - 0.5;
                for c in 0..3 {
                    out[(y * w + x) * 3 + c] = v + offset + noise * 0.01;
                }
            }
        }
        out
    }

    /// The engine's loop: tiles in order, filled in place, the binary hole pasted. `band` overrides
    /// the split band (0: a hard switch between tiles, the control).
    fn run(img: &[f32], m: &[f32], w: usize, h: usize, opts: &TileOptions, band: Option<usize>) -> (Vec<f32>, Vec<Tile>) {
        let t = plan(m, w, h, opts).unwrap();
        let mut out = img.to_vec();
        let mut sp = Split::new(w, h, opts.max_side).unwrap();
        if let Some(b) = band {
            sp.band = b;
        }
        for (i, tile) in t.iter().enumerate() {
            let offset = if i % 2 == 0 { 0.04 } else { -0.04 };
            if tile.split {
                let Some((inp, wts)) = sp.extract(&out, m, tile).unwrap() else { continue };
                sp.paste(&mut out, tile, &periodic_fake(&inp, offset), &wts).unwrap();
            } else {
                let inp = extract(&out, m, w, h, tile).unwrap();
                paste(&mut out, m, w, h, tile, &periodic_fake(&inp, offset)).unwrap();
            }
        }
        (out, t)
    }

    /// #21: a hole far larger than the working side, over screentone and a window grid. Downscaled,
    /// the fill loses the pattern; split into full-resolution tiles it keeps it, and the seams
    /// between tiles are no more visible than the fill's own variation.
    #[test]
    fn full_res_tiles_keep_screentone_sharp_without_seams() {
        let (w, h) = (1400, 1000);
        let hole = Rect { x: 200, y: 150, w: 1000, h: 700 };
        let m = mask_with(w, h, &[hole]);
        let gt: Vec<f32> = (0..w * h).flat_map(|i| [manga(i % w, i / w); 3]).collect();
        let mut img = gt.clone();
        for (i, c) in m.iter().enumerate() {
            if *c > 0.0 {
                img[i * 3..i * 3 + 3].fill(0.5);
            }
        }
        let at = |v: &[f32], x: usize, y: usize| v[(y * w + x) * 3];
        // Detail: mean gradient magnitude in the hole; error: mean distance to the true pattern.
        let detail = |v: &[f32]| {
            let mut s = 0.0;
            for y in hole.y..hole.bottom() - 1 {
                for x in hole.x..hole.right() - 1 {
                    s += (at(v, x + 1, y) - at(v, x, y)).abs() + (at(v, x, y + 1) - at(v, x, y)).abs();
                }
            }
            s / ((hole.w - 1) * (hole.h - 1)) as f32
        };
        let error = |v: &[f32]| {
            let mut s = 0.0;
            for y in hole.y..hole.bottom() {
                for x in hole.x..hole.right() {
                    s += (at(v, x, y) - at(&gt, x, y)).abs();
                }
            }
            s / (hole.w * hole.h) as f32
        };
        // Seams: per column (and row) of the hole, the mean step of the error image (fill minus
        // truth) across it: the columns at tile edges against the rest.
        let seams = |v: &[f32], t: &[Tile]| {
            let e = |x: usize, y: usize| at(v, x, y) - at(&gt, x, y);
            let col = |x: usize| (hole.y..hole.bottom()).map(|y| (e(x + 1, y) - e(x, y)).abs()).sum::<f32>() / hole.h as f32;
            let row = |y: usize| (hole.x..hole.right()).map(|x| (e(x, y + 1) - e(x, y)).abs()).sum::<f32>() / hole.w as f32;
            let near = |p: usize, edges: &[usize]| edges.iter().any(|e| p + 3 >= *e && p <= e + 3);
            let xs: Vec<usize> = t.iter().flat_map(|a| [a.rect.x, a.rect.right()]).collect();
            let ys: Vec<usize> = t.iter().flat_map(|a| [a.rect.y, a.rect.bottom()]).collect();
            let (mut seam, mut inner) = (0f32, Vec::new());
            for x in hole.x + 4..hole.right() - 5 {
                if near(x, &xs) { seam = seam.max(col(x)) } else { inner.push(col(x)) }
            }
            for y in hole.y + 4..hole.bottom() - 5 {
                if near(y, &ys) { seam = seam.max(row(y)) } else { inner.push(row(y)) }
            }
            let mean = inner.iter().sum::<f32>() / inner.len() as f32;
            let sd = (inner.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / inner.len() as f32).sqrt();
            (seam, mean + 3.0 * sd)
        };
        let down = TileOptions::default();
        let full = TileOptions { full_res: true, ..down };
        let (a, ta) = run(&img, &m, w, h, &down, None);
        let (b, tb) = run(&img, &m, w, h, &full, None);
        let (c, tc) = run(&img, &m, w, h, &full, Some(0));
        assert!(ta.len() == 1 && ta[0].fit == Fit::Resize && tb.len() > 4 && tb.iter().all(|t| t.split), "{} {}", ta.len(), tb.len());
        let (dg, da, db) = (detail(&gt), detail(&a), detail(&b));
        let (ea, eb) = (error(&a), error(&b));
        let (sb, ib) = seams(&b, &tb);
        let (sc, ic) = seams(&c, &tc);
        eprintln!("detail: truth {dg:.4} downscaled {da:.4} full {db:.4}; error: downscaled {ea:.4} full {eb:.4}; seams: feathered {sb:.4} (interior {ib:.4}), hard {sc:.4} (interior {ic:.4})");
        assert!(db > da * 1.2 && (db - dg).abs() < dg * 0.1, "full resolution keeps the pattern's detail");
        assert!(eb < ea * 0.5, "and its phase");
        assert!(sb <= ib && sb < sc / 4.0, "no seam shows: {sb} vs {ib}");
        assert!(sc > ic, "the control: a hard switch between tiles does show ({sc} vs {ic})");
    }
}
