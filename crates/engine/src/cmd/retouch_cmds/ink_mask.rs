//! Remove with AI's hole: the painted strokes, grown so nothing of what they touched is left for
//! the fill to rebuild from.
//!
//! A rough stroke over lettering leaves the glyphs' anti-aliased edges, the parts of letters it
//! only half covered and the white outline around an SFX, and an inpainting model reconstructs
//! from those leftovers (grey smudges in a bubble, a white smear over dark art). So, before the
//! fill:
//!
//! 1. the stroke dilates by a few pixels (scaled with the page's resolution);
//! 2. it grows over every connected run of dark ink the stroke touches, when the whole run fits
//!    within a bounded reach of the stroke: a letter is taken whole, while a bubble outline, a
//!    panel border or a field of screentone (runs that leave the reach) is not.
//!
//! Nothing else: the hole is filled exactly as Clean Selected Area with AI fills a selection of its
//! shape (the model sees the page around it, the fill is pasted back whole). Growing it further
//! over the light around it (an SFX's white outline) took hair highlights and art with it, and the
//! model smeared grey over what it had to rebuild.
//!
//! Dark is relative to the local background (the brightest value around), so ink on screentone
//! or on a grey sky counts as it does on a white bubble.

use photocraft_algo::remove::dilate;

/// How far the hole grows, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Grow {
    /// The plain dilation of the stroke.
    pub dilate: usize,
    /// How far from the stroke a run of ink may reach and still be taken whole.
    pub reach: usize,
}

impl Grow {
    /// For a page whose shorter side is `short` pixels and a brush `brush` pixels wide: about
    /// 5 px on a 1600 px page, at least 4 and at most 10; the reach is the brush's width, at
    /// least four dilations (a letter the stroke only grazes) and at most 128.
    pub(crate) fn for_page(short: u32, brush: f32) -> Self {
        let dilate = ((f64::from(short) / 300.0).round() as usize).clamp(4, 10);
        let brush = if brush.is_finite() { brush.max(0.0).round() as usize } else { 0 };
        Grow { dilate, reach: brush.max(dilate * 4).min(128) }
    }
}

/// Ink is darker than its local background by at least this share of it (and 0.06).
const INK_CONTRAST: f32 = 0.2;

fn max(a: f32, b: f32) -> f32 {
    if b > a { b } else { a }
}

/// The brightest value within `r` of each pixel (a square window): separable running maxima
/// (van Herk/Gil-Werman: per block of `2r+1`, the maxima from its start and from its end, so each
/// output is the larger of two), the vertical pass over whole rows.
fn local_max(w: usize, h: usize, v: &[f32], r: usize) -> Vec<f32> {
    if w == 0 || v.len() != w.saturating_mul(h) {
        return v.to_vec();
    }
    let k = 2 * r + 1;
    // Rows, each padded by `r` on both sides with -inf.
    let p = w + 2 * r;
    let (mut padded, mut from_start, mut from_end) = (vec![f32::NEG_INFINITY; p], vec![0f32; p], vec![0f32; p]);
    let mut rows = vec![0f32; v.len()];
    for (src, out) in v.chunks_exact(w).zip(rows.chunks_exact_mut(w)) {
        padded[r..r + w].copy_from_slice(src);
        for ((s, e), x) in from_start.chunks_mut(k).zip(from_end.chunks_mut(k)).zip(padded.chunks(k)) {
            let mut m = f32::NEG_INFINITY;
            for (o, x) in s.iter_mut().zip(x) {
                m = max(m, *x);
                *o = m;
            }
            let mut m = f32::NEG_INFINITY;
            for (o, x) in e.iter_mut().zip(x).rev() {
                m = max(m, *x);
                *o = m;
            }
        }
        for ((o, a), b) in out.iter_mut().zip(&from_end).zip(&from_start[2 * r..]) {
            *o = max(*a, *b);
        }
    }
    // Columns: the same over rows (row `q` of the padded column is `rows`' row `q - r`).
    let p = h + 2 * r;
    let none = vec![f32::NEG_INFINITY; w];
    let row = |q: usize| if (r..r + h).contains(&q) { &rows[(q - r) * w..(q - r + 1) * w] } else { &none[..] };
    let (mut from_start, mut from_end) = (vec![0f32; p * w], vec![0f32; p * w]);
    for q in 0..p {
        let (done, cur) = from_start.split_at_mut(q * w);
        let cur = &mut cur[..w];
        if q % k == 0 {
            cur.copy_from_slice(row(q));
        } else {
            for ((o, a), b) in cur.iter_mut().zip(&done[(q - 1) * w..]).zip(row(q)) {
                *o = max(*a, *b);
            }
        }
    }
    for q in (0..p).rev() {
        let (cur, done) = from_end.split_at_mut((q + 1) * w);
        let cur = &mut cur[q * w..];
        if q % k == k - 1 || q + 1 == p {
            cur.copy_from_slice(row(q));
        } else {
            for ((o, a), b) in cur.iter_mut().zip(&done[..w]).zip(row(q)) {
                *o = max(*a, *b);
            }
        }
    }
    let mut out = vec![0f32; v.len()];
    for (c, o) in out.chunks_exact_mut(w).enumerate() {
        for ((o, a), b) in o.iter_mut().zip(&from_end[c * w..]).zip(&from_start[(c + 2 * r) * w..]) {
            *o = max(*a, *b);
        }
    }
    out
}

/// Which pixels are dark ink: darker than the brightest value around them by [`INK_CONTRAST`].
fn ink(w: usize, h: usize, lum: &[f32], r: usize) -> Vec<bool> {
    let bg = local_max(w, h, lum, r);
    lum.iter().zip(&bg).map(|(l, b)| b - l > (INK_CONTRAST * b).max(0.06)).collect()
}

/// The 8 neighbours of `i` on a `w×h` grid: the row above, the two beside, the row below, each
/// left to right.
fn neighbours(i: usize, w: usize, h: usize) -> impl Iterator<Item = usize> {
    let (x, y) = (i % w, i / w);
    let (left, right) = (x > 0, x + 1 < w);
    let mut n = [0usize; 8];
    let mut len = 0;
    let mut row = |c: usize, mid: bool| {
        for (ok, j) in [(left, c.wrapping_sub(1)), (mid, c), (right, c + 1)] {
            if ok {
                n[len] = j;
                len += 1;
            }
        }
    };
    if y > 0 {
        row(i - w, true);
    }
    row(i, false);
    if y + 1 < h {
        row(i + w, true);
    }
    n.into_iter().take(len)
}

/// The hole for `painted` (the strokes' footprint over a `w×h` window) on an image of luminance
/// `lum` (0..=1): see the module docs.
pub(crate) fn expand(w: usize, h: usize, lum: &[f32], painted: &[bool], g: Grow) -> Vec<bool> {
    let n = w.saturating_mul(h);
    if lum.len() != n || painted.len() != n || !painted.iter().any(|p| *p) {
        return painted.to_vec();
    }
    let ink = ink(w, h, lum, (g.dilate * 3).max(8));
    let zone = dilate(w, h, painted, g.reach.max(g.dilate));
    // Runs of ink outside the stroke, taken whole when the stroke touches them and they stay
    // within the reach.
    let mut seen = vec![false; n];
    let mut take = vec![false; n];
    let mut run = Vec::new();
    let mut stack = Vec::new();
    for start in 0..n {
        if seen[start] || painted[start] || !ink[start] {
            continue;
        }
        run.clear();
        seen[start] = true;
        stack.push(start);
        let (mut touched, mut bounded) = (false, true);
        while let Some(i) = stack.pop() {
            run.push(i);
            let (x, y) = (i % w, i / w);
            if !zone[i] || x == 0 || y == 0 || x + 1 == w || y + 1 == h {
                bounded = false;
            }
            for j in neighbours(i, w, h) {
                if painted[j] {
                    touched = true;
                } else if ink[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        if touched && bounded {
            for &i in &run {
                take[i] = true;
            }
        }
    }
    // The stroke dilated, and the runs taken with their soft edge (a pixel past the threshold).
    let grown = dilate(w, h, painted, g.dilate);
    let taken = dilate(w, h, &take, 1);
    (0..n).map(|i| painted[i] || grown[i] || taken[i]).collect()
}

/// Rec. 601 luminance of interleaved RGB (0..=1).
pub(crate) fn luminance(rgb: &[f32]) -> Vec<f32> {
    rgb.as_chunks::<3>().0.iter().map(|c| 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]).collect()
}

#[cfg(test)]
#[path = "ink_mask_tests.rs"]
mod tests;
