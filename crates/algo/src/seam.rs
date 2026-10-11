//! Content-aware scaling by seam carving (Edit › Content-Aware Scale).
//!
//! S. Avidan, A. Shamir, *Seam Carving for Content-Aware Image Resizing*, SIGGRAPH 2007:
//! a seam is an 8-connected path of one pixel per row (vertical seam) found by dynamic
//! programming over a gradient-magnitude energy map. Shrinking removes the lowest-energy seams one
//! at a time; enlarging finds the `k` lowest seams on a shrinking copy and duplicates them
//! (averaging each with its right neighbour) so the same seam is not stretched repeatedly.
//! Heights are handled by transposing.
//!
//! A protect map (`0..=1`, e.g. an alpha channel or detected skin tones) adds a large energy so
//! seams avoid protected pixels. Buffers are interleaved `w × h × ch` normalised floats of any
//! colour model and depth (alpha is carried like any other channel). Deterministic.

use photocraft_raster::{Cancelled, Interrupt};

/// Energy added per unit of protection: far above any gradient magnitude.
const PROTECT_ENERGY: f32 = 1.0e4;

/// Keep full horizontal detail while reducing the height used to find vertical seams. At a
/// 24 MP working size this cuts the dynamic-programming work by about four times without
/// quantising the output pixels or changing the requested output dimensions.
const SEAM_SEARCH_MAX_HEIGHT: usize = 512;
const PROXY_SHRINK_MIN_HEIGHT: usize = 2_048;
const PROXY_SHRINK_MIN_SEAMS: usize = 64;

/// Gradient-magnitude energy (the paper's e1, `|∂x| + |∂y|` summed over channels). Forward and
/// backward differences are both counted so one-pixel lines (whose central difference is zero)
/// carry energy too.
pub fn energy(w: usize, h: usize, ch: usize, img: &[f32]) -> Vec<f32> {
    let mut e = vec![0.0f32; w * h];
    if w == 0 || h == 0 {
        return e;
    }
    let row = |(y, out): (usize, &mut [f32])| {
        for (x, v) in out.iter_mut().enumerate() {
            *v = energy_at(img, w, w, h, ch, x, y);
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        e.par_chunks_mut(w).enumerate().for_each(row);
    }
    #[cfg(target_arch = "wasm32")]
    e.chunks_mut(w).enumerate().for_each(row);
    e
}

/// [`energy`] of one pixel of a `cw`-wide image whose rows are `stride` pixels apart. The
/// carver's per-seam updates use the same arithmetic, so both give bit-identical values.
#[inline]
fn energy_at(img: &[f32], stride: usize, cw: usize, h: usize, ch: usize, x: usize, y: usize) -> f32 {
    let (xl, xr) = (x.saturating_sub(1), (x + 1).min(cw - 1));
    let (yu, yd) = (y.saturating_sub(1), (y + 1).min(h - 1));
    let at = |xx: usize, yy: usize, c: usize| img[(yy * stride + xx) * ch + c];
    let mut s = 0.0;
    for c in 0..ch {
        let v = at(x, y, c);
        s += (at(xr, y, c) - v).abs() + (v - at(xl, y, c)).abs();
        s += (at(x, yd, c) - v).abs() + (v - at(x, yu, c)).abs();
    }
    s * 0.5
}

/// The lowest-energy vertical seam: one x per row.
pub fn find_seam(w: usize, h: usize, energy: &[f32]) -> Vec<usize> {
    if w == 0 || h == 0 || energy.len() < w * h {
        return vec![0; h];
    }
    let (mut cost, mut from) = (vec![0.0f32; w * h], vec![0u8; w * h]);
    full_dp(w, h, &|y| y * w, energy, &mut cost, &mut from);
    backtrack(w, h, &|y| y * w, &cost, &from)
}

/// One row of the cumulative-cost DP: `out[x - lo]` and `dir[x - lo]` for `x` in `lo..hi` of a
/// `cw`-wide row, from the previous row's costs (`prev[x - plo]`, covering `lo - 1..hi + 1`
/// within the row) and this row's energy (`e[x - elo]`, covering `lo..hi`). `dir` is where the
/// cheapest path came from: 0 = up-left, 1 = up, 2 = up-right; ties go to the pixel straight
/// up, then up-left, then up-right.
#[inline]
#[allow(clippy::too_many_arguments)]
fn dp_row(prev: &[f32], plo: usize, e: &[f32], elo: usize, cw: usize, lo: usize, hi: usize, out: &mut [f32], dir: &mut [u8]) {
    let (p, ev) = (|x: usize| prev[x - plo], |x: usize| e[x - elo]);
    if cw == 1 {
        (out[0], dir[0]) = (p(0) + ev(0), 1);
        return;
    }
    if lo == 0 {
        let (b, d) = if p(1) < p(0) { (p(1), 2) } else { (p(0), 1) };
        (out[0], dir[0]) = (b + ev(0), d);
    }
    // The interior has all three parents: no edge tests in the hot loop.
    let (ilo, ihi) = (lo.max(1), hi.min(cw - 1));
    if ilo < ihi {
        let cells = out[ilo - lo..ihi - lo]
            .iter_mut()
            .zip(&mut dir[ilo - lo..ihi - lo])
            .zip(&e[ilo - elo..ihi - elo])
            .zip(prev[ilo - 1 - plo..ihi + 1 - plo].windows(3));
        for (((n, f), &ev), c) in cells {
            let (b, d) = if c[0] < c[1] { (c[0], 0) } else { (c[1], 1) };
            let (b, d) = if c[2] < b { (c[2], 2) } else { (b, d) };
            (*n, *f) = (b + ev, d);
        }
    }
    if hi == cw {
        let (b, d) = if p(cw - 2) < p(cw - 1) { (p(cw - 2), 0) } else { (p(cw - 1), 1) };
        (out[cw - 1 - lo], dir[cw - 1 - lo]) = (b + ev(cw - 1), d);
    }
}

/// The whole DP of a `cw`-wide, `h`-tall energy map whose row `y` starts at `at(y)` in `energy`,
/// `cost` and `from` alike.
fn full_dp(cw: usize, h: usize, at: &dyn Fn(usize) -> usize, energy: &[f32], cost: &mut [f32], from: &mut [u8]) {
    cost[at(0)..at(0) + cw].copy_from_slice(&energy[at(0)..at(0) + cw]);
    for y in 1..h {
        let (p, r) = (at(y - 1), at(y));
        let (before, row) = cost.split_at_mut(r);
        dp_row(&before[p..p + cw], 0, &energy[r..r + cw], 0, cw, 0, cw, &mut row[..cw], &mut from[r..r + cw]);
    }
}

/// The seam ending at the cheapest bottom pixel (the leftmost of equals), traced up `from`.
fn backtrack(cw: usize, h: usize, at: &dyn Fn(usize) -> usize, cost: &[f32], from: &[u8]) -> Vec<usize> {
    let last = &cost[at(h - 1)..at(h - 1) + cw];
    let mut x = (0..cw).min_by(|a, b| last[*a].total_cmp(&last[*b])).unwrap_or(0);
    let mut seam = vec![0usize; h];
    for y in (0..h).rev() {
        seam[y] = x;
        if y > 0 {
            x = match from[at(y) + x] {
                0 => x - 1,
                2 => x + 1,
                _ => x,
            };
        }
    }
    seam
}

/// Cells per chunk of a carver row.
const CHUNK: usize = 256;

#[cfg(test)]
thread_local! {
    /// Chunk size for tests, so small images get many chunks.
    static TEST_CHUNK: std::cell::Cell<usize> = const { std::cell::Cell::new(CHUNK) };
}

/// Where each row's remaining cells are: a row's `stride` slots are cut into chunks of
/// [`CHUNK`] slots, each holding its first `len` cells, so removing a cell shifts only the rest
/// of its chunk instead of the rest of the row.
struct Layout {
    stride: usize,
    /// Slots per chunk ([`CHUNK`]).
    chunk: usize,
    /// Chunks per row.
    nck: usize,
    /// Cells in each chunk, `nck` per row.
    lens: Vec<u16>,
}

impl Layout {
    fn new(w: usize, h: usize) -> Self {
        #[cfg(test)]
        let chunk = TEST_CHUNK.with(std::cell::Cell::get).clamp(1, CHUNK);
        #[cfg(not(test))]
        let chunk = CHUNK;
        let nck = w.div_ceil(chunk).max(1);
        let row: Vec<u16> = (0..nck).map(|k| chunk.min(w.saturating_sub(k * chunk)) as u16).collect();
        Layout { stride: w, chunk, nck, lens: row.repeat(h) }
    }

    /// The chunk holding cell `x` of a row with chunk lengths `lens`, and its index there.
    fn locate(lens: &[u16], mut x: usize) -> (usize, usize) {
        for (k, &l) in lens.iter().enumerate() {
            if x < l as usize {
                return (k, x);
            }
            x -= l as usize;
        }
        (lens.len().saturating_sub(1), 0)
    }

    /// The slot of cell `x` of row `y`.
    fn slot(&self, y: usize, x: usize) -> usize {
        let (k, i) = Self::locate(&self.lens[y * self.nck..(y + 1) * self.nck], x);
        y * self.stride + k * self.chunk + i
    }

    /// Calls `f(first slot, count)` for the runs of slots holding cells `lo..hi` of row `y`, in
    /// order.
    fn runs(&self, y: usize, lo: usize, hi: usize, mut f: impl FnMut(usize, usize)) {
        let mut at = 0;
        for (k, &l) in self.lens[y * self.nck..(y + 1) * self.nck].iter().enumerate() {
            let (a, b) = (lo.max(at), hi.min(at + l as usize));
            if a < b {
                f(y * self.stride + k * self.chunk + a - at, b - a);
            }
            at += l as usize;
            if at >= hi {
                break;
            }
        }
    }

    /// Cells `lo..hi` of row `y` of `arr`, in order, into `out`.
    fn gather<T: Copy>(&self, arr: &[T], y: usize, lo: usize, hi: usize, out: &mut Vec<T>) {
        out.clear();
        self.runs(y, lo, hi, |s, n| out.extend_from_slice(&arr[s..s + n]));
    }

    /// `src` into cells `lo..` of row `y` of `arr`.
    fn scatter<T: Copy>(&self, arr: &mut [T], y: usize, lo: usize, src: &[T]) {
        let mut i = 0;
        self.runs(y, lo, lo + src.len(), |s, n| {
            arr[s..s + n].copy_from_slice(&src[i..i + n]);
            i += n;
        });
    }
}

/// One row of each per-cell map of a [`Carver`] (energy, columns, costs, directions), its chunk
/// lengths and the column to delete.
type RowMaps<'a> = ((((&'a mut [f32], &'a mut [u32]), &'a mut [f32]), &'a mut [u8]), (&'a mut [u16], &'a usize));

/// Shrinks an image one seam at a time, exactly as recomputing everything per seam would.
///
/// - The pixels never move: each row keeps a map from its remaining columns to the original ones.
/// - Removing a seam deletes one cell from each row of the maps it keeps (energy, cumulative cost,
///   directions, columns), shifting only the rest of that cell's chunk ([`Layout`]).
/// - Only energies next to the seam change (a gradient changes only when a neighbour does), and
///   only the cumulative costs those reach: per row, the cells near the seam (new energy or new
///   neighbours) and below the cells that changed in the row above. Everything else keeps its
///   value bit for bit, so the DP is redone only there, typically a few percent of the map.
struct Carver {
    cw: usize,
    h: usize,
    ch: usize,
    /// The original pixels, `stride × h × ch`, never moved.
    img: Vec<f32>,
    prot: Option<Vec<f32>>,
    lay: Layout,
    /// Energy plus protection, as the seam search sees it.
    e: Vec<f32>,
    /// Cumulative cost of the cheapest path from the top to each cell, and where it came from.
    cost: Vec<f32>,
    from: Vec<u8>,
    /// Original column of each remaining cell.
    col: Vec<u32>,
    /// Per row, the cells whose energy the last seam changed.
    changed: Vec<(usize, usize)>,
    scratch: Scratch,
}

#[derive(Default)]
struct Scratch {
    prev: Vec<f32>,
    e: Vec<f32>,
    old: Vec<f32>,
    out: Vec<f32>,
    dir: Vec<u8>,
}

impl Carver {
    fn new(w: usize, h: usize, ch: usize, img: Vec<f32>, prot: Option<Vec<f32>>) -> Self {
        let mut e = energy(w, h, ch, &img);
        if let Some(p) = &prot {
            for (e, p) in e.iter_mut().zip(p) {
                *e += p.clamp(0.0, 1.0) * PROTECT_ENERGY;
            }
        }
        // Every chunk starts full, so cell x of row y is slot y * w + x.
        let (mut cost, mut from) = (vec![0.0f32; w * h], vec![0u8; w * h]);
        full_dp(w, h, &|y| y * w, &e, &mut cost, &mut from);
        let col = (0..h).flat_map(|_| 0..w as u32).collect();
        Carver { cw: w, h, ch, img, prot, lay: Layout::new(w, h), e, cost, from, col, changed: vec![(0, 0); h], scratch: Scratch::default() }
    }

    /// Remove the lowest-energy seam. Returns, per row, the removed pixel's original column.
    fn remove_seam(&mut self) -> Vec<usize> {
        let (cw, h) = (self.cw, self.h);
        // The cheapest bottom cell (the leftmost of equals), traced up the directions.
        let last = &mut self.scratch.prev;
        self.lay.gather(&self.cost, h - 1, 0, cw, last);
        let mut x = (0..cw).min_by(|a, b| last[*a].total_cmp(&last[*b])).unwrap_or(0);
        let mut seam = vec![0usize; h];
        for y in (0..h).rev() {
            seam[y] = x;
            if y > 0 {
                x = match self.from[self.lay.slot(y, x)] {
                    0 => x - 1,
                    2 => x + 1,
                    _ => x,
                };
            }
        }
        let removed = seam.iter().enumerate().map(|(y, &x)| self.col[self.lay.slot(y, x)] as usize).collect();
        self.delete(&seam);
        self.cw -= 1;
        if self.cw > 0 {
            self.update_energy(&seam);
            self.update_cost(&seam);
        }
        removed
    }

    /// Delete cell `seam[y]` of every row of the per-cell maps.
    fn delete(&mut self, seam: &[usize]) {
        let (stride, nck, chunk) = (self.lay.stride, self.lay.nck, self.lay.chunk);
        let row = |((((e, c), m), f), (lens, &x)): RowMaps| {
            let (k, i) = Layout::locate(lens, x);
            let (base, len) = (k * chunk, lens[k] as usize);
            if i < len {
                e.copy_within(base + i + 1..base + len, base + i);
                c.copy_within(base + i + 1..base + len, base + i);
                m.copy_within(base + i + 1..base + len, base + i);
                f.copy_within(base + i + 1..base + len, base + i);
                lens[k] -= 1;
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            use rayon::prelude::*;
            let maps =
                self.e.par_chunks_mut(stride).zip(self.col.par_chunks_mut(stride)).zip(self.cost.par_chunks_mut(stride)).zip(self.from.par_chunks_mut(stride));
            maps.zip(self.lay.lens.par_chunks_mut(nck).zip(seam.par_iter())).for_each(row);
        }
        #[cfg(target_arch = "wasm32")]
        {
            let maps = self.e.chunks_mut(stride).zip(self.col.chunks_mut(stride)).zip(self.cost.chunks_mut(stride)).zip(self.from.chunks_mut(stride));
            maps.zip(self.lay.lens.chunks_mut(nck).zip(seam)).for_each(row);
        }
    }

    /// Recompute the energy of the pixels whose neighbours changed when `seam` was removed: in
    /// row y, from one left of the leftmost of the seam's x in rows y-1..=y+1 to the rightmost.
    fn update_energy(&mut self, seam: &[usize]) {
        let (cw, h, ch) = (self.cw, self.h, self.ch);
        let (img, prot, col, lay) = (&self.img, self.prot.as_deref(), &self.col, &self.lay);
        let stride = lay.stride;
        let row = |((y, e), span): ((usize, &mut [f32]), &mut (usize, usize))| {
            let near = &seam[y.saturating_sub(1)..(y + 2).min(h)];
            let lo = near.iter().min().map_or(0, |&s| s.saturating_sub(1));
            let hi = near.iter().max().map_or(0, |&s| s.min(cw - 1));
            // Original columns of this row and the rows around it, cells lo - 1..=hi + 1.
            let (glo, ghi) = (lo.saturating_sub(1), (hi + 2).min(cw));
            let mut cols = [Vec::new(), Vec::new(), Vec::new()];
            for (c, yy) in cols.iter_mut().zip([y.saturating_sub(1), y, (y + 1).min(h - 1)]) {
                lay.gather(col, yy, glo, ghi, c);
            }
            let mut vals = Vec::with_capacity(hi + 1 - lo);
            for x in lo..=hi {
                let (xl, xr) = (x.saturating_sub(1), (x + 1).min(cw - 1));
                let at = |r: usize, xx: usize, c: usize| {
                    let yy = [y.saturating_sub(1), y, (y + 1).min(h - 1)][r];
                    img[(yy * stride + cols[r][xx - glo] as usize) * ch + c]
                };
                let mut v = 0.0;
                for c in 0..ch {
                    let p = at(1, x, c);
                    v += (at(1, xr, c) - p).abs() + (p - at(1, xl, c)).abs();
                    v += (at(2, x, c) - p).abs() + (p - at(0, x, c)).abs();
                }
                v *= 0.5;
                if let Some(pr) = prot {
                    v += pr[y * stride + cols[1][x - glo] as usize].clamp(0.0, 1.0) * PROTECT_ENERGY;
                }
                vals.push(v);
            }
            // `e` is this row's slots only.
            let mut i = 0;
            lay.runs(y, lo, hi + 1, |s, n| {
                let s = s - y * stride;
                e[s..s + n].copy_from_slice(&vals[i..i + n]);
                i += n;
            });
            *span = (lo, hi + 1);
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            use rayon::prelude::*;
            self.e.par_chunks_mut(stride).enumerate().zip(self.changed.par_iter_mut()).for_each(row);
        }
        #[cfg(target_arch = "wasm32")]
        self.e.chunks_mut(stride).enumerate().zip(self.changed.iter_mut()).for_each(row);
    }

    /// Redo the DP where it can differ after removing `seam` (pre-removal columns): per row, the
    /// cells with new energy, the cells whose three parents are no longer the same cells (between
    /// the seam's x in this row and the row above), and the cells below any cost that changed in
    /// the row above. A cell outside these has the same parents with the same costs and the same
    /// energy, so it keeps its bits.
    fn update_cost(&mut self, seam: &[usize]) {
        let (cw, h) = (self.cw, self.h);
        let Scratch { prev, e, old, out, dir } = &mut self.scratch;
        // The cells of the row above whose cost changed, as a half-open range (empty: lo >= hi).
        let mut dirty = (0usize, 0usize);
        for y in 0..h {
            let (mut lo, mut hi) = self.changed[y];
            if y > 0 {
                // New parents: already inside the energy span, kept so correctness doesn't hang
                // on how wide that span is.
                let (a, b) = (seam[y - 1].min(seam[y]), seam[y - 1].max(seam[y]));
                (lo, hi) = (lo.min(a.saturating_sub(2)), hi.max(b + 3));
                if dirty.0 < dirty.1 {
                    (lo, hi) = (lo.min(dirty.0.saturating_sub(1)), hi.max(dirty.1 + 1));
                }
            }
            let (lo, hi) = (lo.min(cw), hi.min(cw));
            dirty = (usize::MAX, 0);
            if lo >= hi {
                continue;
            }
            self.lay.gather(&self.e, y, lo, hi, e);
            out.resize(hi - lo, 0.0);
            dir.resize(hi - lo, 0);
            if y == 0 {
                out.copy_from_slice(e);
                dir.fill(0);
            } else {
                let plo = lo.saturating_sub(1);
                self.lay.gather(&self.cost, y - 1, plo, (hi + 1).min(cw), prev);
                dp_row(prev, plo, e, lo, cw, lo, hi, out, dir);
            }
            self.lay.gather(&self.cost, y, lo, hi, old);
            let same = |(a, b): (&f32, &f32)| a.to_bits() == b.to_bits();
            if let Some(first) = old.iter().zip(out.iter()).position(|p| !same(p)) {
                let last = old.iter().zip(out.iter()).rposition(|p| !same(p)).unwrap_or(first);
                dirty = (lo + first, lo + last + 1);
                self.lay.scatter(&mut self.cost, y, lo, out);
            }
            self.lay.scatter(&mut self.from, y, lo, dir);
        }
    }

    /// The carved pixels, `cw × h × ch`, rows packed.
    fn into_pixels(self) -> Vec<f32> {
        let (stride, cw, ch) = (self.lay.stride, self.cw, self.ch);
        if cw == stride {
            return self.img;
        }
        let mut out = vec![0.0f32; cw * self.h * ch];
        let (img, col, lay) = (&self.img, &self.col, &self.lay);
        let row = |(y, o): (usize, &mut [f32])| {
            let mut px = o.chunks_exact_mut(ch);
            lay.runs(y, 0, cw, |s, n| {
                for (&c, p) in col[s..s + n].iter().zip(px.by_ref()) {
                    let at = (y * stride + c as usize) * ch;
                    p.copy_from_slice(&img[at..at + ch]);
                }
            });
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            use rayon::prelude::*;
            out.par_chunks_mut((cw * ch).max(1)).enumerate().for_each(row);
        }
        #[cfg(target_arch = "wasm32")]
        out.chunks_mut((cw * ch).max(1)).enumerate().for_each(row);
        out
    }
}

/// `img` (`w × h`) turned so its columns become rows, in 32-pixel tiles so both sides stay in
/// cache, rows of the result in parallel.
fn transpose(w: usize, h: usize, ch: usize, img: &[f32]) -> Vec<f32> {
    const T: usize = 32;
    let mut out = vec![0.0f32; img.len()];
    if w == 0 || h == 0 || ch == 0 {
        return out;
    }
    // A band of T result rows is T source columns.
    let band = |(b, o): (usize, &mut [f32])| {
        let x0 = b * T;
        let x1 = (x0 + T).min(w);
        for y0 in (0..h).step_by(T) {
            for x in x0..x1 {
                for y in y0..(y0 + T).min(h) {
                    let (s, d) = ((y * w + x) * ch, ((x - x0) * h + y) * ch);
                    o[d..d + ch].copy_from_slice(&img[s..s + ch]);
                }
            }
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        out.par_chunks_mut(T * h * ch).enumerate().for_each(band);
    }
    #[cfg(target_arch = "wasm32")]
    out.chunks_mut(T * h * ch).enumerate().for_each(band);
    out
}

/// Change the width of `img` to `new_w` by removing or duplicating vertical seams.
pub fn carve_width(w: usize, h: usize, ch: usize, img: &[f32], protect: Option<&[f32]>, new_w: usize) -> Vec<f32> {
    // Never cancelled; the fallback is unreachable.
    carve_width_with(w, h, ch, img, protect, new_w, &Interrupt::NONE).unwrap_or_else(|_| img.to_vec())
}

/// [`carve_width`] that checks `ctl` before every seam and reports progress per seam.
pub fn carve_width_with(w: usize, h: usize, ch: usize, img: &[f32], protect: Option<&[f32]>, new_w: usize, ctl: &Interrupt) -> Result<Vec<f32>, Cancelled> {
    let new_w = new_w.max(1);
    if new_w == w || w == 0 || h == 0 {
        return Ok(img.to_vec());
    }
    let seams = new_w.abs_diff(w).max(1) as f32;
    if new_w < w {
        if h >= PROXY_SHRINK_MIN_HEIGHT && w - new_w >= PROXY_SHRINK_MIN_SEAMS {
            return carve_width_proxy_height(w, h, ch, img, protect, new_w, ctl);
        }
        let mut carver = Carver::new(w, h, ch, img.to_vec(), protect.map(<[f32]>::to_vec));
        while carver.cw > new_w {
            ctl.check()?;
            ctl.progress((w - carver.cw) as f32 / seams);
            carver.remove_seam();
        }
        return Ok(carver.into_pixels());
    }
    // Enlarge in rounds of at most half the current width, each duplicating the k lowest seams.
    let (mut cur, mut cw) = (img.to_vec(), w);
    let mut prot = protect.map(<[f32]>::to_vec);
    while cw < new_w {
        let k = (new_w - cw).min((cw / 2).max(1));
        // Find k seams on a shrinking copy, mapping each back to original columns.
        let mut carver = Carver::new(cw, h, ch, cur.clone(), prot.clone());
        let mut dup: Vec<Vec<usize>> = vec![Vec::with_capacity(k); h];
        for _ in 0..k {
            // A 1 px wide image's only seam is its one column: take it, so it is duplicated.
            if carver.cw == 0 {
                break;
            }
            ctl.check()?;
            ctl.progress((cw + (cw - carver.cw)).saturating_sub(w) as f32 / seams);
            for (d, x) in dup.iter_mut().zip(carver.remove_seam()) {
                d.push(x);
            }
        }
        let added = dup[0].len();
        if added == 0 {
            break;
        }
        let nw = cw + added;
        let mut out = Vec::with_capacity(nw * h * ch);
        let mut nprot = prot.as_ref().map(|_| Vec::with_capacity(nw * h));
        for (y, d) in dup.iter_mut().enumerate() {
            d.sort_unstable();
            let row = &cur[y * cw * ch..(y + 1) * cw * ch];
            let mut di = 0;
            for x in 0..cw {
                out.extend_from_slice(&row[x * ch..(x + 1) * ch]);
                if let (Some(np), Some(p)) = (nprot.as_mut(), prot.as_ref()) {
                    np.push(p[y * cw + x]);
                }
                while di < d.len() && d[di] == x {
                    let xr = (x + 1).min(cw - 1);
                    for c in 0..ch {
                        out.push((row[x * ch + c] + row[xr * ch + c]) * 0.5);
                    }
                    if let (Some(np), Some(p)) = (nprot.as_mut(), prot.as_ref()) {
                        np.push(p[y * cw + x]);
                    }
                    di += 1;
                }
            }
        }
        cur = out;
        prot = nprot;
        cw = nw;
    }
    Ok(cur)
}

/// Find large-image seams on vertically averaged rows, then apply those paths to the original
/// pixels. The proxy has the same width, so each seam still removes exactly one source pixel per
/// row. Keeping output pixels at full resolution avoids the quality loss of resizing the result;
/// only the seam decisions use the coarser vertical signal. Small operations retain the exact
/// full-resolution algorithm above.
fn carve_width_proxy_height(w: usize, h: usize, ch: usize, img: &[f32], protect: Option<&[f32]>, new_w: usize, ctl: &Interrupt) -> Result<Vec<f32>, Cancelled> {
    let step = h.div_ceil(SEAM_SEARCH_MAX_HEIGHT);
    let proxy_h = h.div_ceil(step);
    let row_len = w * ch;
    let proxy_row_len = proxy_h * row_len;
    let mut proxy = vec![0.0; proxy_row_len];
    for (py, out_row) in proxy.chunks_exact_mut(row_len).enumerate() {
        let y0 = py * step;
        let y1 = (y0 + step).min(h);
        let divisor = (y1 - y0) as f32;
        for y in y0..y1 {
            let in_row = &img[y * row_len..(y + 1) * row_len];
            for (out, &sample) in out_row.iter_mut().zip(in_row) {
                *out += sample / divisor;
            }
        }
    }
    let proxy_protect = protect.map(|mask| {
        let mut reduced = vec![0.0; proxy_h * w];
        for (py, out_row) in reduced.chunks_exact_mut(w).enumerate() {
            let y0 = py * step;
            let y1 = (y0 + step).min(h);
            let divisor = (y1 - y0) as f32;
            for y in y0..y1 {
                let in_row = &mask[y * w..(y + 1) * w];
                for (out, &sample) in out_row.iter_mut().zip(in_row) {
                    *out += sample / divisor;
                }
            }
        }
        reduced
    });

    let seam_count = w - new_w;
    let mut carver = Carver::new(w, proxy_h, ch, proxy, proxy_protect);
    let mut removed = Vec::with_capacity(seam_count);
    while carver.cw > new_w {
        ctl.check()?;
        removed.push(carver.remove_seam());
        ctl.progress(removed.len() as f32 / seam_count as f32);
    }

    let mut removed_by_row = Vec::with_capacity(proxy_h);
    for proxy_y in 0..proxy_h {
        let mut columns: Vec<usize> = removed.iter().map(|path| path[proxy_y]).collect();
        columns.sort_unstable();
        removed_by_row.push(columns);
    }
    let mut out = Vec::with_capacity(new_w * h * ch);
    let mut columns = vec![0; seam_count];
    for y in 0..h {
        ctl.check()?;
        let proxy_y = (y / step).min(proxy_h - 1);
        let next_proxy_y = (proxy_y + 1).min(proxy_h - 1);
        let blend = (y % step) as f32 / step as f32;
        let lower = &removed_by_row[proxy_y];
        let upper = &removed_by_row[next_proxy_y];
        let mut min_x = 0;
        for (i, x) in columns.iter_mut().enumerate() {
            let interpolated = lower[i] as f32 + (upper[i] as f32 - lower[i] as f32) * blend;
            let max_x = w - (seam_count - i);
            *x = (interpolated.round() as usize).clamp(min_x, max_x);
            min_x = x.saturating_add(1);
        }
        let mut next = 0;
        let row = &img[y * row_len..(y + 1) * row_len];
        for (x, pixel) in row.chunks_exact(ch).enumerate() {
            if columns.get(next) == Some(&x) {
                next += 1;
            } else {
                out.extend_from_slice(pixel);
            }
        }
    }
    ctl.progress(1.0);
    Ok(out)
}

/// Content-aware resize of `img` to `new_w × new_h` (width first, then height by transposing).
pub fn carve(w: usize, h: usize, ch: usize, img: &[f32], protect: Option<&[f32]>, new_w: usize, new_h: usize) -> Vec<f32> {
    // Never cancelled; the fallback is unreachable.
    carve_with(w, h, ch, img, protect, new_w, new_h, &Interrupt::NONE).unwrap_or_else(|_| img.to_vec())
}

/// [`carve`] that checks `ctl` before every seam (a cancel takes effect within one seam) and
/// reports progress over both passes.
#[allow(clippy::too_many_arguments)]
pub fn carve_with(
    w: usize,
    h: usize,
    ch: usize,
    img: &[f32],
    protect: Option<&[f32]>,
    new_w: usize,
    new_h: usize,
    ctl: &Interrupt,
) -> Result<Vec<f32>, Cancelled> {
    let (new_w, new_h) = (new_w.max(1), new_h.max(1));
    // Split progress between the passes by their seam counts.
    let (sw, sh) = (new_w.abs_diff(w) as f32, new_h.abs_diff(h) as f32);
    let split = if sw + sh > 0.0 { sw / (sw + sh) } else { 1.0 };
    let cancel = || ctl.cancelled();
    let first = |f: f32| ctl.progress(f * split);
    let wide = carve_width_with(w, h, ch, img, protect, new_w, &Interrupt::new(&cancel, &first))?;
    if new_h == h {
        return Ok(wide);
    }
    // Protection follows the width change by plain resampling (it only steers seams).
    let prot_t = protect.map(|p| transpose(new_w, h, 1, &resize_bilinear(w, h, 1, p, new_w, h)));
    let t = transpose(new_w, h, ch, &wide);
    let second = |f: f32| ctl.progress(split + f * (1.0 - split));
    let tall = carve_width_with(h, new_w, ch, &t, prot_t.as_deref(), new_h, &Interrupt::new(&cancel, &second))?;
    Ok(transpose(new_h, new_w, ch, &tall))
}

/// Plain bilinear resize (the non-content-aware part of the Amount blend).
pub fn resize_bilinear(w: usize, h: usize, ch: usize, img: &[f32], nw: usize, nh: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; nw * nh * ch];
    if w == 0 || h == 0 || nw == 0 || ch == 0 {
        return out;
    }
    let row = |(y, o): (usize, &mut [f32])| {
        let fy = ((y as f32 + 0.5) * h as f32 / nh as f32 - 0.5).clamp(0.0, (h - 1) as f32);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        let y1 = (y0 + 1).min(h - 1);
        for x in 0..nw {
            let fx = ((x as f32 + 0.5) * w as f32 / nw as f32 - 0.5).clamp(0.0, (w - 1) as f32);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let x1 = (x0 + 1).min(w - 1);
            for c in 0..ch {
                let p = |xx: usize, yy: usize| img[(yy * w + xx) * ch + c];
                let top = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * tx;
                let bot = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * tx;
                o[x * ch + c] = top + (bot - top) * ty;
            }
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        out.par_chunks_mut(nw * ch).enumerate().for_each(row);
    }
    #[cfg(target_arch = "wasm32")]
    out.chunks_mut(nw * ch).enumerate().for_each(row);
    out
}

/// Skin-tone protection map (1 = skin) for RGB(A) buffers: the explicit RGB rule of
/// J. Kovač, P. Peer, F. Solina, *Human Skin Colour Clustering for Face Detection*, EUROCON 2003.
pub fn skin_mask(w: usize, h: usize, ch: usize, img: &[f32]) -> Vec<f32> {
    let mut m = vec![0.0f32; w * h];
    if ch < 3 {
        return m;
    }
    for (i, v) in m.iter_mut().enumerate() {
        let p = &img[i * ch..i * ch + 3];
        let (r, g, b) = (p[0] * 255.0, p[1] * 255.0, p[2] * 255.0);
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        if r > 95.0 && g > 40.0 && b > 20.0 && mx - mn > 15.0 && (r - g).abs() > 15.0 && r > g && r > b {
            *v = 1.0;
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The straightforward implementation the per-seam carver replaced (full energy map, fresh
    /// seam search and a new buffer per seam), kept as the oracle for `carver_matches_reference`.
    mod reference {
        use super::super::{PROTECT_ENERGY, resize_bilinear, transpose};

        pub(super) fn energy(w: usize, h: usize, ch: usize, img: &[f32]) -> Vec<f32> {
            let mut e = vec![0.0f32; w * h];
            for y in 0..h {
                for x in 0..w {
                    let (xl, xr) = (x.saturating_sub(1), (x + 1).min(w - 1));
                    let (yu, yd) = (y.saturating_sub(1), (y + 1).min(h - 1));
                    let at = |xx: usize, yy: usize, c: usize| img[(yy * w + xx) * ch + c];
                    let mut s = 0.0;
                    for c in 0..ch {
                        let v = at(x, y, c);
                        s += (at(xr, y, c) - v).abs() + (v - at(xl, y, c)).abs();
                        s += (at(x, yd, c) - v).abs() + (v - at(x, yu, c)).abs();
                    }
                    e[y * w + x] = s * 0.5;
                }
            }
            e
        }
        pub(super) fn find_seam(w: usize, h: usize, energy: &[f32]) -> Vec<usize> {
            let mut cost = energy[..w].to_vec();
            let mut from = vec![0u8; w * h]; // 0 = up-left, 1 = up, 2 = up-right
            let mut next = vec![0.0f32; w];
            for y in 1..h {
                for x in 0..w {
                    let mut best = (cost[x], 1u8);
                    if x > 0 && cost[x - 1] < best.0 {
                        best = (cost[x - 1], 0);
                    }
                    if x + 1 < w && cost[x + 1] < best.0 {
                        best = (cost[x + 1], 2);
                    }
                    next[x] = best.0 + energy[y * w + x];
                    from[y * w + x] = best.1;
                }
                std::mem::swap(&mut cost, &mut next);
            }
            let mut x = (0..w).min_by(|a, b| cost[*a].total_cmp(&cost[*b])).unwrap_or(0);
            let mut seam = vec![0usize; h];
            for y in (0..h).rev() {
                seam[y] = x;
                if y > 0 {
                    x = match from[y * w + x] {
                        0 => x - 1,
                        2 => x + 1,
                        _ => x,
                    };
                }
            }
            seam
        }
        fn remove_seam(w: usize, h: usize, ch: usize, img: &[f32], seam: &[usize]) -> Vec<f32> {
            let mut out = Vec::with_capacity((w - 1) * h * ch);
            for (y, &sx) in seam.iter().enumerate().take(h) {
                let row = &img[y * w * ch..(y + 1) * w * ch];
                out.extend_from_slice(&row[..sx * ch]);
                out.extend_from_slice(&row[(sx + 1) * ch..]);
            }
            out
        }
        pub(super) fn carve_width(w: usize, h: usize, ch: usize, img: &[f32], protect: Option<&[f32]>, new_w: usize) -> Vec<f32> {
            let new_w = new_w.max(1);
            if new_w == w || w == 0 || h == 0 {
                return img.to_vec();
            }
            let protect_e = |e: &mut [f32], p: Option<&Vec<f32>>| {
                if let Some(p) = p {
                    for (e, p) in e.iter_mut().zip(p) {
                        *e += p.clamp(0.0, 1.0) * PROTECT_ENERGY;
                    }
                }
            };
            if new_w < w {
                let (mut cur, mut cw) = (img.to_vec(), w);
                let mut prot = protect.map(<[f32]>::to_vec);
                while cw > new_w {
                    let mut e = energy(cw, h, ch, &cur);
                    protect_e(&mut e, prot.as_ref());
                    let seam = find_seam(cw, h, &e);
                    cur = remove_seam(cw, h, ch, &cur, &seam);
                    prot = prot.map(|p| remove_seam(cw, h, 1, &p, &seam));
                    cw -= 1;
                }
                return cur;
            }
            // Enlarge in rounds of at most half the current width, each duplicating the k lowest seams.
            let (mut cur, mut cw) = (img.to_vec(), w);
            let mut prot = protect.map(<[f32]>::to_vec);
            while cw < new_w {
                let k = (new_w - cw).min((cw / 2).max(1));
                // Find k seams on a shrinking copy, mapping each back to original columns.
                let mut idx: Vec<Vec<usize>> = (0..h).map(|_| (0..cw).collect()).collect();
                let (mut tmp, mut tw) = (cur.clone(), cw);
                let mut tprot = prot.clone();
                let mut dup: Vec<Vec<usize>> = vec![Vec::with_capacity(k); h];
                for _ in 0..k {
                    // A 1 px wide image's only seam is its one column: take it, so it is duplicated.
                    if tw == 0 {
                        break;
                    }
                    let mut e = energy(tw, h, ch, &tmp);
                    protect_e(&mut e, tprot.as_ref());
                    let seam = find_seam(tw, h, &e);
                    for (y, &sx) in seam.iter().enumerate() {
                        dup[y].push(idx[y].remove(sx));
                    }
                    tmp = remove_seam(tw, h, ch, &tmp, &seam);
                    tprot = tprot.map(|p| remove_seam(tw, h, 1, &p, &seam));
                    tw -= 1;
                }
                let added = dup[0].len();
                if added == 0 {
                    break;
                }
                let nw = cw + added;
                let mut out = Vec::with_capacity(nw * h * ch);
                let mut nprot = prot.as_ref().map(|_| Vec::with_capacity(nw * h));
                for (y, d) in dup.iter_mut().enumerate() {
                    d.sort_unstable();
                    let row = &cur[y * cw * ch..(y + 1) * cw * ch];
                    let mut di = 0;
                    for x in 0..cw {
                        out.extend_from_slice(&row[x * ch..(x + 1) * ch]);
                        if let (Some(np), Some(p)) = (nprot.as_mut(), prot.as_ref()) {
                            np.push(p[y * cw + x]);
                        }
                        while di < d.len() && d[di] == x {
                            let xr = (x + 1).min(cw - 1);
                            for c in 0..ch {
                                out.push((row[x * ch + c] + row[xr * ch + c]) * 0.5);
                            }
                            if let (Some(np), Some(p)) = (nprot.as_mut(), prot.as_ref()) {
                                np.push(p[y * cw + x]);
                            }
                            di += 1;
                        }
                    }
                }
                cur = out;
                prot = nprot;
                cw = nw;
            }
            cur
        }

        pub(super) fn carve(w: usize, h: usize, ch: usize, img: &[f32], protect: Option<&[f32]>, new_w: usize, new_h: usize) -> Vec<f32> {
            let (new_w, new_h) = (new_w.max(1), new_h.max(1));
            let wide = carve_width(w, h, ch, img, protect, new_w);
            if new_h == h {
                return wide;
            }
            let prot_t = protect.map(|p| transpose(new_w, h, 1, &resize_bilinear(w, h, 1, p, new_w, h)));
            let t = transpose(new_w, h, ch, &wide);
            let tall = carve_width(h, new_w, ch, &t, prot_t.as_deref(), new_h);
            transpose(new_h, new_w, ch, &tall)
        }
    }

    /// The carver gives bit-identical output to the reference implementation it replaced, over
    /// random shapes (including 1 px wide and tall), channel counts, tie-heavy quantised noise,
    /// protection, shrinking, enlarging past half the width and both axes at once.
    #[test]
    fn carver_matches_reference() {
        for chunk in [CHUNK, 1, 2, 3, 7] {
            TEST_CHUNK.with(|c| c.set(chunk));
            matches_reference(chunk);
        }
        TEST_CHUNK.with(|c| c.set(CHUNK));
    }

    fn matches_reference(chunk: usize) {
        let mut seed = 1u32;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize % n
        };
        for case in 0..400u32 {
            let (w, h, ch) = (1 + next(24), 1 + next(18), [1, 3, 4][next(3)]);
            let levels = [2, 5, 255, 65_536][next(4)];
            let img = noise(w * h * ch, levels, 11 + case);
            let protect = (next(3) == 0).then(|| noise(w * h, 3, 101 + case));
            let (nw, nh) = (1 + next(2 * w + 2), 1 + next(2 * h + 2));
            let got = carve(w, h, ch, &img, protect.as_deref(), nw, nh);
            let want = reference::carve(w, h, ch, &img, protect.as_deref(), nw, nh);
            let bits = |v: &[f32]| v.iter().map(|f| f.to_bits()).collect::<Vec<_>>();
            assert_eq!(bits(&got), bits(&want), "chunk {chunk}, case {case}: {w}x{h}x{ch} -> {nw}x{nh}, levels {levels}, protect {}", protect.is_some());
        }
        // The seam search alone, on raw energy maps full of ties.
        for case in 0..200u32 {
            let (w, h) = (1 + next(40), 1 + next(30));
            let e = noise(w * h, [2, 3, 1000][next(3)], 7 + case);
            assert_eq!(find_seam(w, h, &e), reference::find_seam(w, h, &e), "seam case {case}: {w}x{h}");
            let img = noise(w * h * 3, 7, 3 + case);
            let bits = |v: Vec<f32>| v.into_iter().map(f32::to_bits).collect::<Vec<_>>();
            assert_eq!(bits(energy(w, h, 3, &img)), bits(reference::energy(w, h, 3, &img)), "energy case {case}");
        }
    }

    /// A wide image (parallel row passes, long seams, many rows shifted from either side) carves
    /// exactly like the reference.
    #[test]
    fn wide_carve_matches_reference() {
        let (w, h, ch) = (1100, 420, 3);
        let img = noise(w * h * ch, 9, 77);
        let got = carve(w, h, ch, &img, None, w - 6, h);
        let want = reference::carve(w, h, ch, &img, None, w - 6, h);
        assert!(got.iter().map(|f| f.to_bits()).eq(want.iter().map(|f| f.to_bits())));
    }

    /// The tiled transpose moves every pixel where the plain double loop does, for sizes around
    /// and across its tile.
    #[test]
    fn transpose_matches_naive() {
        for (w, h, ch) in [(1, 1, 1), (31, 33, 3), (64, 32, 4), (70, 5, 1), (5, 97, 2)] {
            let img: Vec<f32> = (0..w * h * ch).map(|i| i as f32).collect();
            let mut want = vec![0.0f32; img.len()];
            for y in 0..h {
                for x in 0..w {
                    want[(x * h + y) * ch..(x * h + y + 1) * ch].copy_from_slice(&img[(y * w + x) * ch..(y * w + x + 1) * ch]);
                }
            }
            assert_eq!(transpose(w, h, ch, &img), want, "{w}x{h}x{ch}");
        }
    }

    #[test]
    fn cancelled_carve_stops() {
        let (w, h) = (40, 20);
        let img = vec![0.5f32; w * h * 3];
        let yes = || true;
        assert_eq!(carve_with(w, h, 3, &img, None, 30, 15, &Interrupt::cancel_only(&yes)), Err(Cancelled));
        assert_eq!(carve_with(w, h, 3, &img, None, 30, 15, &Interrupt::NONE).unwrap(), carve(w, h, 3, &img, None, 30, 15));
    }

    /// A flat grey image with a salient vertical bar at x = 10..14 (red/blue columns).
    fn bar(w: usize, h: usize) -> Vec<f32> {
        let mut v = vec![0.5f32; w * h * 3];
        for y in 0..h {
            for x in 10..14 {
                let c = if x % 2 == 0 { [1.0, 0.0, 0.0] } else { [0.0, 0.0, 1.0] };
                v[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
            }
        }
        v
    }

    /// Columns that are pure red or pure blue top to bottom.
    fn red_columns(w: usize, h: usize, img: &[f32]) -> usize {
        let pure = |p: &[f32]| p[1] < 0.05 && ((p[0] > 0.95 && p[2] < 0.05) || (p[2] > 0.95 && p[0] < 0.05));
        (0..w).filter(|x| (0..h).all(|y| pure(&img[(y * w + x) * 3..(y * w + x) * 3 + 3]))).count()
    }

    #[test]
    fn seam_follows_low_energy() {
        let (w, h) = (5, 4);
        let mut e = vec![9.0f32; w * h];
        for y in 0..h {
            e[y * w + 2] = 0.0;
        }
        assert_eq!(find_seam(w, h, &e), vec![2; 4]);
    }

    #[test]
    fn shrinking_keeps_the_salient_bar() {
        let (w, h) = (40, 12);
        let img = bar(w, h);
        let out = carve(w, h, 3, &img, None, 24, h);
        assert_eq!(out.len(), 24 * h * 3);
        assert_eq!(red_columns(24, h, &out), 4, "the bar survives intact");
        // Plain resampling squashes it instead.
        let plain = resize_bilinear(w, h, 3, &img, 24, h);
        assert!(red_columns(24, h, &plain) < 4);
    }

    #[test]
    fn tall_proxy_shrink_preserves_full_resolution_salient_and_protected_pixels() {
        let (w, h, ch, new_w) = (512, PROXY_SHRINK_MIN_HEIGHT, 3, 384);
        let mut img = bar(w, h);
        let mut protect = vec![0.0; w * h];
        for y in 0..h {
            let i = (y * w + 256) * ch;
            img[i..i + ch].copy_from_slice(&[0.03, 0.87, 0.42]);
            protect[y * w + 256] = 1.0;
        }

        let out = carve_width_with(w, h, ch, &img, Some(&protect), new_w, &Interrupt::NONE).unwrap();

        assert_eq!(out.len(), new_w * h * ch);
        assert_eq!(red_columns(new_w, h, &out), 4, "the full-resolution bar survives");
        for y in 0..h {
            let row = &out[y * new_w * ch..(y + 1) * new_w * ch];
            assert!(row.chunks_exact(ch).any(|px| px == [0.03, 0.87, 0.42]), "protected mark survives row {y}");
        }
    }

    #[test]
    fn tall_proxy_shrink_observes_cancellation_before_modifying_source() {
        let (w, h, ch) = (128, PROXY_SHRINK_MIN_HEIGHT, 3);
        let img = vec![0.5; w * h * ch];
        let cancelled = || true;
        assert_eq!(carve_width_with(w, h, ch, &img, None, 64, &Interrupt::cancel_only(&cancelled)), Err(Cancelled));
    }

    #[test]
    fn enlarging_duplicates_flat_areas() {
        let (w, h) = (30, 8);
        let img = bar(w, h);
        let out = carve(w, h, 3, &img, None, 50, h);
        assert_eq!(out.len(), 50 * h * 3);
        assert_eq!(red_columns(50, h, &out), 4);
    }

    #[test]
    fn height_and_protection() {
        let (w, h) = (20, 30);
        // Horizontal bar: carve height.
        let mut img = vec![0.2f32; w * h];
        for x in 0..w {
            img[12 * w + x] = 1.0;
        }
        let out = carve(w, h, 1, &img, None, w, 18);
        assert_eq!(out.len(), w * 18);
        assert_eq!((0..18).filter(|y| (0..w).all(|x| out[y * w + x] > 0.9)).count(), 1);
        // Protecting a flat region keeps it: the seams go elsewhere.
        let flat = vec![0.5f32; 16 * 4];
        let mut protect = vec![0.0f32; 16 * 4];
        for y in 0..4 {
            protect[y * 16 + 3] = 1.0;
        }
        let mut marked = flat.clone();
        for y in 0..4 {
            marked[y * 16 + 3] = 0.51;
        }
        let out = carve(16, 4, 1, &marked, Some(&protect), 8, 4);
        assert!((0..4).all(|y| out[y * 8..y * 8 + 8].iter().any(|v| (*v - 0.51).abs() < 1e-6)));
    }

    /// Noise quantised to `levels` values, so the energy map is full of exact ties.
    fn noise(len: usize, levels: u32, seed: u32) -> Vec<f32> {
        let mut s = seed | 1;
        (0..len)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s % levels) as f32 / (levels - 1) as f32
            })
            .collect()
    }

    fn fnv(v: &[f32]) -> u64 {
        v.iter().flat_map(|f| f.to_bits().to_le_bytes()).fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
    }

    /// The exact output bits of a spread of carves (shrink, enlarge past half the width, both
    /// axes, protection, 1/3/4 channels, tie-heavy and smooth images), pinned so that speed work
    /// can't change which seams are chosen or how pixels are blended.
    #[test]
    fn output_bits_are_pinned() {
        let cases: [(usize, usize, usize, u32, bool, usize, usize); 8] = [
            (61, 47, 3, 2, false, 40, 47),
            (61, 47, 3, 255, false, 61, 30),
            (61, 47, 4, 4, true, 35, 29),
            (33, 20, 1, 3, false, 80, 20),
            (33, 20, 3, 1000, true, 50, 45),
            (48, 36, 4, 7, false, 70, 18),
            (5, 3, 1, 2, false, 1, 1),
            (40, 40, 3, 2, true, 39, 41),
        ];
        let got: Vec<u64> = cases
            .iter()
            .enumerate()
            .map(|(i, &(w, h, ch, levels, prot, nw, nh))| {
                let img = noise(w * h * ch, levels, 7 + i as u32);
                let protect = prot.then(|| noise(w * h, 3, 99 + i as u32));
                let out = carve(w, h, ch, &img, protect.as_deref(), nw, nh);
                assert_eq!(out.len(), nw * nh * ch);
                fnv(&out)
            })
            .collect();
        assert_eq!(got, PINNED);
    }

    const PINNED: [u64; 8] = [
        0x24ea_9353_02c2_7c58,
        0x5a6e_8554_a0cb_ebb1,
        0x8b18_d63f_396e_21c9,
        0x1db2_23ad_3e6a_a7c5,
        0x721d_6cc5_34e0_3fdf,
        0x961a_ea6d_e512_ec7d,
        0x4d25_767f_9dce_13f5,
        0x2157_9f86_27d6_b835,
    ];

    #[test]
    fn one_pixel_lines_enlarge_by_repeating() {
        // Width 1 has no seam to spare: its one column is repeated. Height 1 likewise (through
        // the transposed pass), alone or with the other axis.
        let col = [0.1f32, 0.2, 0.3, 0.4, 0.5];
        assert_eq!(carve(1, 5, 1, &col, None, 4, 5), col.iter().flat_map(|v| [*v; 4]).collect::<Vec<_>>());
        assert_eq!(carve(5, 1, 1, &col, None, 5, 3), col.repeat(3));
        assert_eq!(carve(1, 1, 3, &[0.7, 0.8, 0.9], None, 3, 2), [0.7, 0.8, 0.9].repeat(6));
        for (w, h, nw, nh) in [(1, 5, 6, 10), (1, 17, 6, 34), (16, 1, 21, 2), (7, 1, 3, 8), (2, 1, 1, 8)] {
            let img = vec![0.5f32; w * h * 4];
            assert_eq!(carve(w, h, 4, &img, Some(&vec![1.0; w * h]), nw, nh).len(), nw * nh * 4, "{w}x{h} -> {nw}x{nh}");
        }
    }

    #[test]
    fn skin_rule() {
        let px = [0.86f32, 0.62, 0.5, 0.2, 0.4, 0.8];
        assert_eq!(skin_mask(2, 1, 3, &px), vec![1.0, 0.0]);
    }
}
