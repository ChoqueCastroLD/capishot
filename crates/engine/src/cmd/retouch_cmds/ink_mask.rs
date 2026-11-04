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

    /// How far from the stroke a run of ink may reach ([`zone`]).
    pub(crate) fn zone_reach(self) -> usize {
        self.reach.max(self.dilate)
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
    let rows = local_max_rows(w, v, r);
    local_max_cols(w, h, &rows, r)
}

/// [`local_max`]'s horizontal pass.
fn local_max_rows(w: usize, v: &[f32], r: usize) -> Vec<f32> {
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
    rows
}

/// [`local_max`]'s vertical pass over the horizontal one's `rows`.
fn local_max_cols(w: usize, h: usize, rows: &[f32], r: usize) -> Vec<f32> {
    let mut c = ColsMax::new(w, h, r);
    c.advance(rows, usize::MAX);
    c.out
}

/// [`local_max_cols`] some rows at a time ([`ColsMax::advance`]).
#[derive(Debug)]
pub(crate) struct ColsMax {
    w: usize,
    h: usize,
    r: usize,
    /// The running maxima from each block's start and from its end, over the padded column.
    from_start: Vec<f32>,
    from_end: Vec<f32>,
    out: Vec<f32>,
    /// The next padded row of the current pass: the first pass goes down (`< p`), the second up,
    /// then the output rows.
    pass: u8,
    q: usize,
}

impl ColsMax {
    pub(crate) fn new(w: usize, h: usize, r: usize) -> Self {
        let p = h + 2 * r;
        Self { w, h, r, from_start: vec![0f32; p * w], from_end: vec![0f32; p * w], out: vec![0f32; w * h], pass: 0, q: 0 }
    }

    /// Does at most `budget` more rows over `rows` (the horizontal pass); true once done.
    pub(crate) fn advance(&mut self, rows: &[f32], budget: usize) -> bool {
        let (w, h, r) = (self.w, self.h, self.r);
        if rows.len() != w * h {
            self.out = rows.to_vec();
            self.pass = 3;
            return true;
        }
        let k = 2 * r + 1;
        let p = h + 2 * r;
        let none = vec![f32::NEG_INFINITY; w];
        // Row `q` of the padded column is `rows`' row `q - r`.
        let row = |q: usize| if (r..r + h).contains(&q) { &rows[(q - r) * w..(q - r + 1) * w] } else { &none[..] };
        let mut left = budget;
        while left > 0 && self.pass < 3 {
            left -= 1;
            match self.pass {
                0 => {
                    let q = self.q;
                    let (done, cur) = self.from_start.split_at_mut(q * w);
                    let cur = &mut cur[..w];
                    if q.is_multiple_of(k) {
                        cur.copy_from_slice(row(q));
                    } else {
                        for ((o, a), b) in cur.iter_mut().zip(&done[(q - 1) * w..]).zip(row(q)) {
                            *o = max(*a, *b);
                        }
                    }
                    self.q += 1;
                    if self.q == p {
                        (self.pass, self.q) = (1, p);
                    }
                }
                1 => {
                    let q = self.q - 1;
                    let (cur, done) = self.from_end.split_at_mut((q + 1) * w);
                    let cur = &mut cur[q * w..];
                    if q % k == k - 1 || q + 1 == p {
                        cur.copy_from_slice(row(q));
                    } else {
                        for ((o, a), b) in cur.iter_mut().zip(&done[..w]).zip(row(q)) {
                            *o = max(*a, *b);
                        }
                    }
                    self.q -= 1;
                    if self.q == 0 {
                        self.pass = 2;
                    }
                }
                _ => {
                    let c = self.q;
                    let (fe, fs) = (&self.from_end[c * w..], &self.from_start[(c + 2 * r) * w..]);
                    for ((o, a), b) in self.out[c * w..(c + 1) * w].iter_mut().zip(fe).zip(fs) {
                        *o = max(*a, *b);
                    }
                    self.q += 1;
                    if self.q == h {
                        self.pass = 3;
                    }
                }
            }
        }
        self.pass == 3
    }

    /// The output (once [`Self::advance`] is done).
    pub(crate) fn finish(self) -> Vec<f32> {
        self.out
    }
}

/// Which pixels are dark ink: darker than the brightest value around them by [`INK_CONTRAST`].
fn ink(w: usize, h: usize, lum: &[f32], r: usize) -> Vec<bool> {
    is_ink(lum, &local_max(w, h, lum, r))
}

/// Which of `lum` is darker than its background `bg` by [`INK_CONTRAST`].
fn is_ink(lum: &[f32], bg: &[f32]) -> Vec<bool> {
    lum.iter().zip(bg).map(|(l, b)| b - l > (INK_CONTRAST * b).max(0.06)).collect()
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
/// `lum` (0..=1): see the module docs. The same as [`ink_of`], [`zone`], [`runs`] and [`finish`]
/// one after the other (the parts Remove with AI takes a step at a time).
pub(crate) fn expand(w: usize, h: usize, lum: &[f32], painted: &[bool], g: Grow) -> Vec<bool> {
    if !applies(w, h, lum, painted) {
        return painted.to_vec();
    }
    let ink = ink_of(w, h, lum, g);
    let zone = zone(w, h, painted, g);
    let take = runs(w, h, painted, &ink, &zone);
    finish(w, h, painted, &take, g)
}

/// Whether the hole grows at all (else it is `painted`): the buffers fit and something is painted.
pub(crate) fn applies(w: usize, h: usize, lum: &[f32], painted: &[bool]) -> bool {
    let n = w.saturating_mul(h);
    lum.len() == n && painted.len() == n && painted.iter().any(|p| *p)
}

/// Where the dark ink is.
pub(crate) fn ink_of(w: usize, h: usize, lum: &[f32], g: Grow) -> Vec<bool> {
    ink(w, h, lum, (g.dilate * 3).max(8))
}

/// [`ink_of`] in two parts: the brightest value around each pixel along its row …
pub(crate) fn ink_rows(w: usize, h: usize, lum: &[f32], g: Grow) -> Vec<f32> {
    if w == 0 || lum.len() != w.saturating_mul(h) {
        return lum.to_vec();
    }
    local_max_rows(w, lum, (g.dilate * 3).max(8))
}

/// … then over the rows around it ([`ColsMax`], from [`ink_cols_start`]) …
pub(crate) fn ink_cols_start(w: usize, h: usize, g: Grow) -> ColsMax {
    ColsMax::new(w, h, (g.dilate * 3).max(8))
}

/// … and against each pixel's luminance.
pub(crate) fn ink_cols_finish(lum: &[f32], cols: ColsMax) -> Vec<bool> {
    is_ink(lum, &cols.finish())
}

/// How far from the stroke a run of ink may reach.
pub(crate) fn zone(w: usize, h: usize, painted: &[bool], g: Grow) -> Vec<bool> {
    dilate(w, h, painted, g.zone_reach())
}

/// The runs of ink outside the stroke taken whole: the stroke touches them and they stay within
/// the `zone`.
pub(crate) fn runs(w: usize, h: usize, painted: &[bool], ink: &[bool], zone: &[bool]) -> Vec<bool> {
    let mut r = Runs::new(w.saturating_mul(h));
    r.advance(w, h, painted, ink, zone, usize::MAX);
    r.take
}

/// [`runs`] a share of the window at a time ([`Runs::advance`]).
#[derive(Debug, Default)]
pub(crate) struct Runs {
    seen: Vec<bool>,
    pub take: Vec<bool>,
    /// The next pixel a run may start from.
    pub next: usize,
}

impl Runs {
    pub(crate) fn new(n: usize) -> Self {
        Self { seen: vec![false; n], take: vec![false; n], next: 0 }
    }

    /// Follows the runs starting before pixel `until` (at least those not done yet). True once
    /// every pixel was looked at.
    pub(crate) fn advance(&mut self, w: usize, h: usize, painted: &[bool], ink: &[bool], zone: &[bool], until: usize) -> bool {
        let n = w.saturating_mul(h);
        if painted.len() != n || ink.len() != n || zone.len() != n || self.seen.len() != n {
            self.next = n;
            return true;
        }
        let (mut run, mut stack) = (Vec::new(), Vec::new());
        let end = until.min(n);
        while self.next < end {
            let start = self.next;
            self.next += 1;
            if self.seen[start] || painted[start] || !ink[start] {
                continue;
            }
            run.clear();
            self.seen[start] = true;
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
                    } else if ink[j] && !self.seen[j] {
                        self.seen[j] = true;
                        stack.push(j);
                    }
                }
            }
            if touched && bounded {
                for &i in &run {
                    self.take[i] = true;
                }
            }
        }
        self.next >= n
    }
}

/// The stroke dilated, and the runs taken with their soft edge (a pixel past the threshold).
pub(crate) fn finish(w: usize, h: usize, painted: &[bool], take: &[bool], g: Grow) -> Vec<bool> {
    combine(w, h, painted, &dilate(w, h, painted, g.dilate), take)
}

/// [`finish`] with the stroke already dilated (`grown`).
pub(crate) fn combine(w: usize, h: usize, painted: &[bool], grown: &[bool], take: &[bool]) -> Vec<bool> {
    let taken = dilate(w, h, take, 1);
    painted.iter().zip(grown.iter().zip(&taken)).map(|(p, (g, t))| *p || *g || *t).collect()
}

/// Rec. 601 luminance of interleaved RGB (0..=1).
pub(crate) fn luminance(rgb: &[f32]) -> Vec<f32> {
    rgb.as_chunks::<3>().0.iter().map(|c| 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]).collect()
}

#[cfg(test)]
#[path = "ink_mask_tests.rs"]
mod tests;
