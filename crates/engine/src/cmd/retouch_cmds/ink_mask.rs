//! Remove with AI's hole: the painted strokes, grown so nothing of what they touched is left for
//! the fill to rebuild from.
//!
//! A rough stroke over lettering leaves the glyphs' anti-aliased edges and the parts of letters
//! it only half covered, and an inpainting model reconstructs from those leftovers (grey smudges
//! in the bubble). So, before the fill:
//!
//! 1. the stroke dilates by a few pixels (scaled with the page's resolution);
//! 2. it grows over every connected run of dark ink the stroke touches, when the whole run fits
//!    within a bounded reach of the stroke: a letter is taken whole, while a bubble outline, a
//!    panel border or a field of screentone (runs that leave the reach) is not, and the dilation
//!    keeps off them and off nearby ink the stroke never touched;
//! 3. the paste-back feathers over a thin ring around that ([`Hole::weight`]), so the fill
//!    blends into the page instead of meeting it at a hard edge.
//!
//! Dark is relative to the local background (the brightest value around), so ink on screentone
//! or on a grey sky counts as it does on a white bubble.
//!
//! Manga LaMa also continues the text it sees around a hole: a word removed from a bubble comes
//! back as letter-like scribbles. So the letters around the hole (runs of ink of a letter's size
//! on clear paper, not screentone or hatching) are hidden from the model too ([`Hole::hide`]);
//! only the hole is pasted back, so they stay on the page. And its fill of a bubble comes out a
//! shade off the paper: where the hole meets flat paper, the fill that is near the paper's tone
//! takes it ([`settle_paper`]).

use std::collections::{HashMap, VecDeque};

use photocraft_algo::remove::dilate;

/// How far the hole grows, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Grow {
    /// The plain dilation of the stroke.
    pub dilate: usize,
    /// How far from the stroke a run of ink may reach and still be taken whole.
    pub reach: usize,
    /// The soft ring at the paste-back.
    pub feather: usize,
}

impl Grow {
    /// For a page whose shorter side is `short` pixels and a brush `brush` pixels wide: about
    /// 5 px on a 1600 px page, at least 4 and at most 10; the reach is the brush's width, at
    /// least four dilations (a letter the stroke only grazes) and at most 128.
    pub(crate) fn for_page(short: u32, brush: f32) -> Self {
        let dilate = ((f64::from(short) / 300.0).round() as usize).clamp(4, 10);
        let brush = if brush.is_finite() { brush.max(0.0).round() as usize } else { 0 };
        Grow { dilate, reach: brush.max(dilate * 4).min(128), feather: 2 }
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

/// The hole and its paste-back weights.
pub(crate) struct Hole {
    pub mask: Vec<bool>,
    /// 1 over the hole's core, then falling over the soft ring around it (`k/(f+1)` at `f+1−k`
    /// pixels out), 0 outside: the fill blends into what is around it instead of meeting it at a
    /// hard edge, and never lets what the stroke covered show through.
    pub weight: Vec<f32>,
    /// Letters around the hole the model shouldn't see (masked in its input, not changed).
    pub hide: Vec<bool>,
}

/// The share of a run's surroundings (3 px around it, outside the hole) that may be other ink for
/// the run to count as a letter on clear paper (screentone and hatching have more).
const CLEAR_PAPER: f32 = 0.2;

/// Whether `run` (pixel indices, outside the hole) is a letter on clear paper: between `min` and
/// `max` pixels a side, off the window's edge, and mostly paper around it.
fn letter_on_paper(w: usize, h: usize, run: &[usize], ink: &[bool], hole: &[bool], min: usize, max: usize) -> bool {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for &i in run {
        (x0, y0, x1, y1) = (x0.min(i % w), y0.min(i / w), x1.max(i % w), y1.max(i / w));
    }
    let side = (x1 - x0 + 1).max(y1 - y0 + 1);
    if side < min || side > max || x0 == 0 || y0 == 0 || x1 + 1 == w || y1 + 1 == h {
        return false;
    }
    const R: usize = 3;
    let (bx0, by0) = (x0.saturating_sub(R), y0.saturating_sub(R));
    let (bw, bh) = ((x1 + R + 1).min(w) - bx0, (y1 + R + 1).min(h) - by0);
    let mut own = vec![false; bw * bh];
    for &i in run {
        own[(i / w - by0) * bw + (i % w - bx0)] = true;
    }
    let near = dilate(bw, bh, &own, R);
    let (mut around, mut inked) = (0usize, 0usize);
    for (k, n) in near.iter().enumerate() {
        let i = (by0 + k / bw) * w + bx0 + k % bw;
        if *n && !own[k] && !hole[i] {
            around += 1;
            inked += usize::from(ink[i]);
        }
    }
    around > 0 && (inked as f32) <= CLEAR_PAPER * around as f32
}

/// The hole for `painted` (the strokes' footprint over a `w×h` window) on an image of luminance
/// `lum` (0..=1): see the module docs.
pub(crate) fn expand(w: usize, h: usize, lum: &[f32], painted: &[bool], g: Grow) -> Hole {
    let n = w.saturating_mul(h);
    if lum.len() != n || painted.len() != n || !painted.iter().any(|p| *p) {
        let weight = painted.iter().map(|m| if *m { 1.0 } else { 0.0 }).collect();
        return Hole { mask: painted.to_vec(), weight, hide: vec![false; painted.len()] };
    }
    let ink = ink(w, h, lum, (g.dilate * 3).max(8));
    let zone = dilate(w, h, painted, g.reach.max(g.dilate));
    // Runs of ink outside the stroke: taken whole (`take`) when the stroke touches them and they
    // stay within the reach; else kept out of the hole (`keep`).
    let mut seen = vec![false; n];
    let mut take = vec![false; n];
    let mut keep = vec![false; n];
    let mut run = Vec::new();
    let mut stack = Vec::new();
    let mut kept: Vec<Vec<usize>> = Vec::new();
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
        let out = if touched && bounded { &mut take } else { &mut keep };
        for &i in &run {
            out[i] = true;
        }
        if !(touched && bounded) {
            kept.push(run.clone());
        }
    }
    // The stroke dilated, and the runs taken with their soft edge (a pixel past the threshold).
    let grown = dilate(w, h, painted, g.dilate);
    let taken = dilate(w, h, &take, 1);
    let mut mask: Vec<bool> = (0..n).map(|i| painted[i] || ((grown[i] || taken[i]) && !keep[i])).collect();
    let mut weight: Vec<f32> = mask.iter().map(|m| if *m { 1.0 } else { 0.0 }).collect();
    let step = 1.0 / (g.feather as f32 + 1.0);
    for k in 0..g.feather {
        let ring = dilate(w, h, &mask, 1);
        for i in 0..n {
            if ring[i] && !mask[i] && !keep[i] {
                mask[i] = true;
                weight[i] = 1.0 - step * (k as f32 + 1.0);
            }
        }
    }
    // The letters around: from a dilation's size (smaller is screentone dots and specks) to twice
    // the reach (bigger is art), with their soft edge.
    let mut letters = vec![false; n];
    let (min, max) = (g.dilate.max(5), (g.reach * 2).max(96));
    for run in &kept {
        if letter_on_paper(w, h, run, &ink, &mask, min, max) {
            for &i in run {
                letters[i] = true;
            }
        }
    }
    let hide = dilate(w, h, &letters, 2).iter().zip(&mask).map(|(l, m)| *l && !m).collect();
    Hole { mask, weight, hide }
}

/// Flat paper: its neighbourhood (2 px) varies by at most this, and it is at least this light
/// (a fine, light screentone is flat at 2 px too, but darker).
const FLAT: f32 = 0.06;
const PAPER: f32 = 0.9;
/// How far the fill may be from the paper's tone and still be paper.
const NEAR_PAPER: f32 = 0.12;

/// Where `hole` meets flat paper on the original (`orig` interleaved RGB 0..=1), the fill
/// (`generated`, the same layout) that stays near the paper's tone, reached from that paper
/// without crossing anything darker, takes the nearest paper's tone: a bubble's fill
/// comes out exactly the bubble's white, while screentone, art and lines keep the model's fill.
pub(crate) fn settle_paper(w: usize, h: usize, orig: &[f32], hole: &[bool], generated: &mut [f32]) {
    let n = w.saturating_mul(h);
    if orig.len() != n * 3 || generated.len() != n * 3 || hole.len() != n {
        return;
    }
    let lum = luminance(orig);
    // The pixels within 2 of `i` (clipped to the grid) outside the hole: what the flatness and the
    // paper's tone look at. Only read at the hole's edge, so taken there rather than everywhere.
    let around = |i: usize, f: &mut dyn FnMut(usize)| {
        let (x, y) = (i % w, i / w);
        for yy in y.saturating_sub(2)..(y + 3).min(h) {
            let row = yy * w;
            for (j, _) in hole.iter().enumerate().take(row + (x + 3).min(w)).skip(row + x.saturating_sub(2)).filter(|(_, h)| !**h) {
                f(j);
            }
        }
    };
    // From each flat paper pixel at the hole's edge, outward into the hole (breadth first, so each
    // pixel takes its nearest paper's colour).
    let mut from = vec![usize::MAX; n];
    let mut queue = VecDeque::new();
    for i in 0..n {
        if hole[i] || lum[i] < PAPER || !neighbours(i, w, h).any(|j| hole[j]) {
            continue;
        }
        // The brightest and the darkest around, without the hole (it counts as black for the
        // brightest and as white for the darkest; it is always within reach of the edge).
        let (mut hi, mut lo) = (0.0f32, 0.0f32);
        around(i, &mut |j| {
            hi = max(hi, lum[j]);
            lo = max(lo, 1.0 - lum[j]);
        });
        if hi - (1.0 - lo) <= FLAT {
            from[i] = i;
            queue.push_back(i);
        }
    }
    let glum = |j: usize| generated.get(j * 3..j * 3 + 3).map_or(f32::NAN, |c| 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]);
    while let Some(i) = queue.pop_front() {
        let src = from[i];
        for j in neighbours(i, w, h) {
            if hole[j] && from[j] == usize::MAX && (glum(j) - lum[src]).abs() <= NEAR_PAPER {
                from[j] = src;
                queue.push_back(j);
            }
        }
    }
    // The paper's tone at each source: the brightest around it (ink and compression ringing only
    // darken paper), so one noisy pixel doesn't spread.
    let mut tone: HashMap<usize, [f32; 3]> = HashMap::new();
    for i in 0..n {
        let src = from[i];
        if !hole[i] || src == usize::MAX {
            continue;
        }
        let t = *tone.entry(src).or_insert_with(|| {
            let mut t = [0.0f32; 3];
            around(src, &mut |j| {
                for (c, t) in t.iter_mut().enumerate() {
                    *t = max(*t, orig[j * 3 + c]);
                }
            });
            t
        });
        if let Some(g) = generated.get_mut(i * 3..i * 3 + 3) {
            g.copy_from_slice(&t);
        }
    }
}

/// Rec. 601 luminance of interleaved RGB (0..=1).
pub(crate) fn luminance(rgb: &[f32]) -> Vec<f32> {
    rgb.as_chunks::<3>().0.iter().map(|c| 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]).collect()
}

#[cfg(test)]
#[path = "ink_mask_tests.rs"]
mod tests;
