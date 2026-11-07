//! The letters of a zone: its ink, found against the ground, level, cut into components at one
//! letter height.

use crate::glyph::trim;
use crate::{Bitmap, CAP, Zone};

/// A zone's letters.
#[derive(Clone, Debug)]
pub struct Letters {
    /// Each at the height of [`CAP`].
    pub letters: Vec<Bitmap>,
    /// The letters' height in the zone, in pixels.
    pub cap: f32,
    /// The ink, in the zone's pixels, before levelling.
    pub ink: Bitmap,
    /// Degrees the lettering is turned, positive counter-clockwise.
    pub tilt: f32,
    /// The letter bodies were hollow: this ring around them, in pixels, is an outline.
    pub ring: Option<f32>,
    /// The zone held lettering of other sizes too; these are the largest.
    pub mixed: bool,
}

/// At most this many letters are compared.
const MAX_LETTERS: usize = 14;
/// Shorter letters than this don't say enough.
const MIN_CAP: f32 = 7.0;

pub(crate) fn gray(z: &Zone<'_>) -> Vec<u8> {
    z.rgb.chunks_exact(3).map(|c| ((u32::from(c[0]) * 77 + u32::from(c[1]) * 150 + u32::from(c[2]) * 29) >> 8) as u8).collect()
}

fn otsu(g: &[u8]) -> u8 {
    let mut hist = [0u64; 256];
    for v in g {
        hist[*v as usize] += 1;
    }
    let total: u64 = hist.iter().sum();
    let sum: u64 = hist.iter().enumerate().map(|(i, n)| i as u64 * n).sum();
    let (mut w0, mut sum0, mut best, mut at) = (0u64, 0u64, 0f64, 127u8);
    for (i, n) in hist.iter().enumerate() {
        w0 += n;
        if w0 == 0 || w0 == total {
            continue;
        }
        sum0 += i as u64 * n;
        let (m0, m1) = (sum0 as f64 / w0 as f64, (sum - sum0) as f64 / (total - w0) as f64);
        let v = w0 as f64 * (total - w0) as f64 * (m0 - m1) * (m0 - m1);
        if v > best {
            best = v;
            at = i as u8;
        }
    }
    at
}

/// How many pixels are inside a square of side `r` entirely set (a fast erosion, by summed areas).
fn eroded(b: &Bitmap, r: usize) -> Bitmap {
    let (w, h) = (b.width, b.height);
    let mut sat = vec![0u32; (w + 1) * (h + 1)];
    for y in 0..h {
        for x in 0..w {
            sat[(y + 1) * (w + 1) + x + 1] = u32::from(b.get(x, y)) + sat[y * (w + 1) + x + 1] + sat[(y + 1) * (w + 1) + x] - sat[y * (w + 1) + x];
        }
    }
    let mut out = Bitmap::new(w, h);
    if w < r || h < r {
        return out;
    }
    let full = (r * r) as u32;
    for y in 0..=h - r {
        for x in 0..=w - r {
            let s = sat[(y + r) * (w + 1) + x + r] + sat[y * (w + 1) + x] - sat[y * (w + 1) + x + r] - sat[(y + r) * (w + 1) + x];
            if s == full {
                out.data[(y + r / 2) * w + x + r / 2] = true;
            }
        }
    }
    out
}

/// Removes what a 2 × 2 square doesn't fit in: screentone's dots.
fn opened(b: &Bitmap) -> Bitmap {
    let (w, h) = (b.width, b.height);
    let mut out = Bitmap::new(w, h);
    for y in 0..h.saturating_sub(1) {
        for x in 0..w.saturating_sub(1) {
            if b.get(x, y) && b.get(x + 1, y) && b.get(x, y + 1) && b.get(x + 1, y + 1) {
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    out.data[(y + dy) * w + x + dx] = true;
                }
            }
        }
    }
    out
}

/// A component's box and area.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Comp {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
    pub area: usize,
}

/// Connected components: a label per pixel (0: none) and each one's box, by flood fill.
pub(crate) fn components(b: &Bitmap, eight: bool) -> (Vec<u32>, Vec<Comp>) {
    let (w, h) = (b.width, b.height);
    let mut labels = vec![0u32; w * h];
    let mut comps = Vec::new();
    let mut stack = Vec::new();
    for start in 0..w * h {
        if !b.data[start] || labels[start] != 0 {
            continue;
        }
        let id = comps.len() as u32 + 1;
        labels[start] = id;
        stack.push(start);
        let mut c = Comp { x0: w, y0: h, x1: 0, y1: 0, area: 0 };
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            (c.x0, c.y0, c.x1, c.y1) = (c.x0.min(x), c.y0.min(y), c.x1.max(x + 1), c.y1.max(y + 1));
            c.area += 1;
            let mut go = |nx: usize, ny: usize| {
                let j = ny * w + nx;
                if b.data[j] && labels[j] == 0 {
                    labels[j] = id;
                    stack.push(j);
                }
            };
            if x > 0 {
                go(x - 1, y);
            }
            if x + 1 < w {
                go(x + 1, y);
            }
            if y > 0 {
                go(x, y - 1);
            }
            if y + 1 < h {
                go(x, y + 1);
            }
            if eight {
                if x > 0 && y > 0 {
                    go(x - 1, y - 1);
                }
                if x + 1 < w && y > 0 {
                    go(x + 1, y - 1);
                }
                if x > 0 && y + 1 < h {
                    go(x - 1, y + 1);
                }
                if x + 1 < w && y + 1 < h {
                    go(x + 1, y + 1);
                }
            }
        }
        comps.push(c);
    }
    (labels, comps)
}

/// How well a threshold separates `g`: the between-class variance at it.
fn contrast(g: &[u8], t: u8) -> f64 {
    let (mut n0, mut s0, mut n1, mut s1) = (0f64, 0f64, 0f64, 0f64);
    for v in g {
        if *v > t {
            n1 += 1.0;
            s1 += f64::from(*v);
        } else {
            n0 += 1.0;
            s0 += f64::from(*v);
        }
    }
    if n0 == 0.0 || n1 == 0.0 {
        return 0.0;
    }
    let d = s0 / n0 - s1 / n1;
    n0 * n1 / ((n0 + n1) * (n0 + n1)) * d * d
}

/// The zone's ink, on whichever of gray and the three channels tells it from the ground best: dark
/// red on a pale yellow hardly differs in gray. Of the two sides of the threshold, the ground is the
/// one that survives an erosion a tenth of the zone wide. A letter's stroke doesn't.
fn ink_of(z: &Zone<'_>) -> Bitmap {
    let mut planes = vec![gray(z)];
    for k in 0..3 {
        planes.push(z.rgb.iter().skip(k).step_by(3).copied().collect());
    }
    let mut best = (0.0, 0usize, 127u8);
    for (i, g) in planes.iter().enumerate() {
        let t = otsu(g);
        // Gray unless a channel is clearly better: it's the steadiest on black and white.
        let c = contrast(g, t) * if i == 0 { 1.25 } else { 1.0 };
        if c > best.0 {
            best = (c, i, t);
        }
    }
    let (w, h) = (z.width, z.height);
    let light = Bitmap { width: w, height: h, data: planes[best.1].iter().map(|v| *v > best.2).collect() };
    let dark = Bitmap { width: w, height: h, data: light.data.iter().map(|v| !*v).collect() };
    let r = (w.min(h) / 10).max(3) | 1;
    if eroded(&light, r).area() >= eroded(&dark, r).area() { dark } else { light }
}

/// The widest a stroke of `ink` goes: 90th percentile of twice the distance to its edge.
fn stroke_width(ink: &Bitmap) -> f32 {
    let d = distance(ink);
    let mut v: Vec<f32> = d.iter().copied().filter(|x| *x > 0.0).collect();
    if v.is_empty() {
        return 1.0;
    }
    v.sort_by(f32::total_cmp);
    2.0 * v[v.len() * 9 / 10]
}

/// Distance to the nearest unset pixel, by a 3-4 chamfer, in pixels.
pub(crate) fn distance(b: &Bitmap) -> Vec<f32> {
    let (w, h) = (b.width, b.height);
    let big = (w + h) as f32 * 3.0;
    let mut d: Vec<f32> = b.data.iter().map(|s| if *s { big } else { 0.0 }).collect();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if d[i] == 0.0 {
                continue;
            }
            let mut v = d[i];
            if x > 0 {
                v = v.min(d[i - 1] + 3.0);
            }
            if y > 0 {
                v = v.min(d[i - w] + 3.0);
                if x > 0 {
                    v = v.min(d[i - w - 1] + 4.0);
                }
                if x + 1 < w {
                    v = v.min(d[i - w + 1] + 4.0);
                }
            }
            d[i] = v;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            if d[i] == 0.0 {
                continue;
            }
            let mut v = d[i];
            if x + 1 < w {
                v = v.min(d[i + 1] + 3.0);
            }
            if y + 1 < h {
                v = v.min(d[i + w] + 3.0);
                if x + 1 < w {
                    v = v.min(d[i + w + 1] + 4.0);
                }
                if x > 0 {
                    v = v.min(d[i + w - 1] + 4.0);
                }
            }
            d[i] = v;
        }
    }
    for v in &mut d {
        *v /= 3.0;
    }
    d
}

/// Outlined letters: the ink is a ring around bodies of the ground's colour. Those bodies, and how
/// wide the ring is.
fn hollow(ink: &Bitmap) -> Option<(Bitmap, f32)> {
    let (w, h) = (ink.width, ink.height);
    let ground = Bitmap { width: w, height: h, data: ink.data.iter().map(|v| !*v).collect() };
    let (labels, comps) = components(&ground, false);
    let mut inner: Vec<bool> = comps.iter().map(|c| c.x0 > 0 && c.y0 > 0 && c.x1 < w && c.y1 < h && c.area > 30).collect();
    // What sits inside another's box is a counter, the O's middle: the ground again, not a body.
    let bodies = inner.clone();
    for (i, c) in comps.iter().enumerate() {
        let nested = comps.iter().zip(&bodies).enumerate().any(|(j, (o, is))| *is && j != i && o.x0 < c.x0 && o.y0 < c.y0 && o.x1 > c.x1 && o.y1 > c.y1);
        if nested {
            inner[i] = false;
        }
    }
    // Every shape of ink goes around something: plain lettering only does at A, B, D, O…
    let (_, shapes) = components(ink, true);
    let mut sizes: Vec<usize> = shapes.iter().map(|c| c.area).collect();
    sizes.sort_unstable();
    let big = sizes.get(sizes.len() * 9 / 10).copied().unwrap_or(0);
    // The ring around an O's counter is part of that O.
    let outer: Vec<&Comp> = shapes.iter().filter(|c| !shapes.iter().any(|o| o.x0 < c.x0 && o.y0 < c.y0 && o.x1 > c.x1 && o.y1 > c.y1)).collect();
    let shapes: Vec<&Comp> = outer.into_iter().filter(|c| c.area * 10 >= big).collect();
    let around = shapes.iter().filter(|o| comps.iter().zip(&inner).any(|(c, is)| *is && o.x0 < c.x0 && o.y0 < c.y0 && o.x1 > c.x1 && o.y1 > c.y1)).count();
    if shapes.is_empty() || around * 5 < shapes.len() * 4 {
        return None;
    }
    let mut body = Bitmap::new(w, h);
    for (i, l) in labels.iter().enumerate() {
        if *l != 0 && inner[*l as usize - 1] {
            body.data[i] = true;
        }
    }
    Some((body, stroke_width(ink)))
}

/// Takes out specks and whatever touches the zone's edge: a balloon's line, the drawing.
fn cleaned(ink: &Bitmap) -> Bitmap {
    let (labels, comps) = components(ink, true);
    let mut areas: Vec<usize> = comps.iter().map(|c| c.area).collect();
    areas.sort_unstable();
    let big = areas.get(areas.len() * 9 / 10).copied().unwrap_or(0);
    let min = (big / 100).max(6);
    let keep: Vec<bool> = comps.iter().map(|c| c.area >= min && c.x0 > 0 && c.y0 > 0 && c.x1 < ink.width && c.y1 < ink.height).collect();
    let mut out = Bitmap::new(ink.width, ink.height);
    for (i, l) in labels.iter().enumerate() {
        if *l != 0 && keep[*l as usize - 1] {
            out.data[i] = true;
        }
    }
    out
}

/// Sizes this far apart are different lettering.
const OTHER_SIZE: f32 = 1.5;
/// So many shapes make a size lettering, and not one mark or a piece of the drawing.
const SHAPES_OF_A_SIZE: usize = 2;

/// Only the largest lettering in `ink`, and whether there was other: someone who drags over a big
/// title and a small line means the title. Dots and accents are too small to count as other.
fn largest_lettering(ink: Bitmap) -> (Bitmap, bool) {
    let (labels, comps) = components(&ink, true);
    let mut order: Vec<usize> = (0..comps.len()).collect();
    order.sort_by_key(|i| comps[*i].y1 - comps[*i].y0);
    // Runs of similar heights, smallest first.
    let mut runs: Vec<Vec<usize>> = Vec::new();
    let mut last = 0usize;
    for i in order {
        let h = comps[i].y1 - comps[i].y0;
        match runs.last_mut() {
            Some(run) if (h as f32) < last as f32 * OTHER_SIZE => run.push(i),
            _ => runs.push(vec![i]),
        }
        last = h;
    }
    let Some(top) = runs.iter().rposition(|r| r.len() >= SHAPES_OF_A_SIZE) else { return (ink, false) };
    let cap = runs[top].iter().map(|i| comps[*i].y1 - comps[*i].y0).max().unwrap_or(0);
    // Below it, a run of several shapes tall enough to be letters is other lettering.
    let letter_like = |i: &usize| {
        let (w, h) = (comps[*i].x1 - comps[*i].x0, comps[*i].y1 - comps[*i].y0);
        // Tall enough, and not the round marks of dots and ellipses.
        h * 5 >= cap && w.abs_diff(h) * 4 > h
    };
    let other = runs[..top].iter().any(|r| r.len() >= 3 && r.iter().any(letter_like));
    if !other {
        return (ink, false);
    }
    let mut keep = vec![false; comps.len()];
    for i in &runs[top] {
        keep[*i] = true;
    }
    let mut out = Bitmap::new(ink.width, ink.height);
    for (i, l) in labels.iter().enumerate() {
        if *l != 0 && keep[*l as usize - 1] {
            out.data[i] = true;
        }
    }
    (out, true)
}

/// Degrees the lines of `m` are turned: where its rows of ink are sharpest. A line through the
/// ink's middle would read lines of different lengths as a tilt.
fn tilt_of(m: &Bitmap) -> f32 {
    let mut ink: Vec<(f32, f32)> = Vec::new();
    for y in 0..m.height {
        for x in 0..m.width {
            if m.get(x, y) {
                ink.push((x as f32, y as f32));
            }
        }
    }
    if ink.len() < 40 {
        return 0.0;
    }
    let side = (m.width + m.height) * 2;
    let sharpness = |deg: f32| -> f64 {
        let (s, c) = deg.to_radians().sin_cos();
        let mut rows = vec![0u32; side];
        for (x, y) in &ink {
            let at = (y * c - x * s + m.width as f32) as usize;
            if let Some(r) = rows.get_mut(at) {
                *r += 1;
            }
        }
        rows.iter().map(|r| f64::from(*r) * f64::from(*r)).sum()
    };
    let flat = sharpness(0.0);
    let (mut best, mut at) = (flat, 0.0f32);
    for k in -25..=25 {
        let deg = k as f32;
        let v = sharpness(deg);
        if v > best {
            best = v;
            at = deg;
        }
    }
    // Clearly better than level, or level.
    if at.abs() < 2.0 || best < flat * 1.15 { 0.0 } else { at }
}

/// `m` turned by `deg`, nearest neighbour, same size.
fn rotated(m: &Bitmap, deg: f32) -> Bitmap {
    let (s, c) = deg.to_radians().sin_cos();
    let (cx, cy) = (m.width as f32 / 2.0, m.height as f32 / 2.0);
    let mut out = Bitmap::new(m.width, m.height);
    for y in 0..m.height {
        for x in 0..m.width {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let (sx, sy) = (cx + c * dx - s * dy, cy + s * dx + c * dy);
            if sx >= 0.0 && sy >= 0.0 && (sx as usize) < m.width && (sy as usize) < m.height && m.get(sx as usize, sy as usize) {
                out.data[y * m.width + x] = true;
            }
        }
    }
    out
}

/// `b` at `k` times its size, each new pixel set when most of what it covers is.
pub(crate) fn scaled(b: &Bitmap, k: f32) -> Bitmap {
    let (w, h) = (((b.width as f32 * k).round() as usize).max(1), ((b.height as f32 * k).round() as usize).max(1));
    let mut out = Bitmap::new(w, h);
    let (sx, sy) = (b.width as f32 / w as f32, b.height as f32 / h as f32);
    for y in 0..h {
        let (y0, y1) = ((y as f32 * sy) as usize, (((y + 1) as f32 * sy).ceil() as usize).min(b.height));
        for x in 0..w {
            let (x0, x1) = ((x as f32 * sx) as usize, (((x + 1) as f32 * sx).ceil() as usize).min(b.width));
            let (mut on, mut n) = (0, 0);
            for yy in y0..y1.max(y0 + 1).min(b.height) {
                for xx in x0..x1.max(x0 + 1).min(b.width) {
                    on += usize::from(b.get(xx, yy));
                    n += 1;
                }
            }
            out.data[y * w + x] = on * 2 > n;
        }
    }
    out
}

/// Below this the letters found are too few to trust.
const FEW_LETTERS: usize = 2;
/// Letters this tall or less: strokes of a pixel or two, which screentone's opening would take.
const SMALL_CAP: f32 = 16.0;

/// The zone's letters. `None`: nothing that reads as lettering.
pub fn letters(z: &Zone<'_>) -> Option<Letters> {
    if z.width < 8 || z.height < 8 || z.rgb.len() != z.width * z.height * 3 {
        return None;
    }
    let raw = ink_of(z);
    let opened = letters_of(opened(&raw));
    // Small letters on a clean ground lose their strokes to the opening.
    match opened {
        Some(l) if l.letters.len() >= FEW_LETTERS && l.cap > SMALL_CAP => Some(l),
        other => match (letters_of(raw), other) {
            (Some(a), Some(b)) => Some(if a.letters.len() > b.letters.len() { a } else { b }),
            (a, b) => a.or(b),
        },
    }
}

fn letters_of(raw: Bitmap) -> Option<Letters> {
    let (ink, ring) = match hollow(&raw) {
        Some((body, ring)) => (body, Some(ring)),
        None => (raw, None),
    };
    let (ink, mixed) = largest_lettering(cleaned(&ink));
    let tilt = tilt_of(&ink);
    let level = if tilt == 0.0 { ink.clone() } else { rotated(&ink, tilt) };
    let (labels, comps) = components(&level, true);
    let mut heights: Vec<usize> = comps.iter().map(|c| c.y1 - c.y0).collect();
    heights.sort_unstable();
    if heights.is_empty() {
        return None;
    }
    // The taller half's median: dots and accents don't count.
    let cap = heights[heights.len() * 3 / 4] as f32;
    if cap < MIN_CAP {
        return None;
    }
    let k = CAP as f32 / cap;
    let mut out = Vec::new();
    for (i, c) in comps.iter().enumerate() {
        let (cw, ch) = ((c.x1 - c.x0) as f32, (c.y1 - c.y0) as f32);
        // Too small is a mark; too wide is letters stuck together.
        if ch < cap * 0.55 || ch > cap * 1.6 || cw > cap * 1.8 {
            continue;
        }
        let mut b = Bitmap::new(c.x1 - c.x0, c.y1 - c.y0);
        for y in c.y0..c.y1 {
            for x in c.x0..c.x1 {
                if labels[y * level.width + x] == i as u32 + 1 {
                    b.data[(y - c.y0) * b.width + x - c.x0] = true;
                }
            }
        }
        out.push(trim(&scaled(&b, k)));
        if out.len() == MAX_LETTERS {
            break;
        }
    }
    (!out.is_empty()).then_some(Letters { letters: out, cap, ink, tilt, ring, mixed })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A black ring of `t` around a white body, with a hole in it that's ringed too.
    fn outlined_o() -> Bitmap {
        let (w, h) = (120, 120);
        let mut ink = Bitmap::new(w, h);
        let ring = |x: usize, y: usize, a: usize, b: usize, t: usize| {
            (a..b).contains(&x) && (a..b).contains(&y) && !((a + t..b - t).contains(&x) && (a + t..b - t).contains(&y))
        };
        for y in 0..h {
            for x in 0..w {
                ink.data[y * w + x] = ring(x, y, 10, 110, 6) || ring(x, y, 40, 80, 6);
            }
        }
        ink
    }

    #[test]
    fn an_outlined_letter_keeps_its_body_and_not_its_counter() {
        let (body, ring) = hollow(&outlined_o()).unwrap();
        assert!(body.get(25, 60), "the body");
        assert!(!body.get(60, 60), "the middle of the O");
        assert!(!body.get(12, 12), "the ring");
        assert!(ring >= 4.0, "{ring}");
    }

    #[test]
    fn dark_red_on_pale_yellow_reads_by_the_channel_that_tells_them_apart() {
        let (w, h) = (100, 60);
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let ink = (20..80).contains(&x) && (20..40).contains(&y) && x % 12 < 6;
                // Close in gray; far in blue.
                let c: [u8; 3] = if ink { [190, 150, 30] } else { [190, 170, 200] };
                rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
            }
        }
        let found = letters(&Zone { rgb: &rgb, width: w, height: h }).unwrap();
        assert_eq!(found.letters.len(), 5, "each bar a letter");
    }

    #[test]
    fn thin_small_letters_survive_the_clean_up_of_screentone() {
        let (w, h) = (80, 30);
        let mut rgb = vec![255u8; w * h * 3];
        for x in (10..70).step_by(6) {
            for y in 8..20 {
                rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(0);
            }
        }
        let found = letters(&Zone { rgb: &rgb, width: w, height: h }).unwrap();
        assert!(found.letters.len() >= 5, "{}", found.letters.len());
    }

    #[test]
    fn with_a_big_title_and_a_small_line_it_takes_the_title_and_says_so() {
        let (w, h) = (300, 140);
        let mut rgb = vec![255u8; w * h * 3];
        let mut bar = |x0: usize, y0: usize, bw: usize, bh: usize| {
            for y in y0..y0 + bh {
                for x in x0..x0 + bw {
                    rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(0);
                }
            }
        };
        for k in 0..12 {
            bar(20 + k * 14, 10, 4, 14); // A small line, most of the shapes.
        }
        for k in 0..4 {
            bar(20 + k * 60, 50, 30, 70); // The title.
        }
        let found = letters(&Zone { rgb: &rgb, width: w, height: h }).unwrap();
        assert!(found.mixed);
        assert_eq!(found.letters.len(), 4, "the title's");
        assert!((found.cap - 70.0).abs() < 4.0, "{}", found.cap);
    }

    #[test]
    fn dots_and_lower_case_are_one_lettering() {
        let (w, h) = (300, 80);
        let mut rgb = vec![255u8; w * h * 3];
        let mut bar = |x0: usize, y0: usize, bh: usize| {
            for y in y0..y0 + bh {
                for x in x0..x0 + 6 {
                    rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(0);
                }
            }
        };
        for k in 0..8 {
            bar(20 + k * 20, if k % 2 == 0 { 20 } else { 28 }, if k % 2 == 0 { 30 } else { 22 });
        }
        for k in 0..3 {
            bar(200 + k * 14, 44, 6); // Three dots.
        }
        assert!(!letters(&Zone { rgb: &rgb, width: w, height: h }).unwrap().mixed);
    }

    #[test]
    fn lines_of_different_lengths_are_not_a_tilt() {
        let (w, h) = (300, 100);
        let mut m = Bitmap::new(w, h);
        for (x1, y0) in [(280, 20), (120, 60)] {
            for y in y0..y0 + 20 {
                for x in 20..x1 {
                    m.data[y * w + x] = (x / 12) % 2 == 0;
                }
            }
        }
        assert_eq!(tilt_of(&m), 0.0);
    }

    #[test]
    fn a_turned_line_reads_as_its_angle() {
        let (w, h) = (300, 200);
        let mut m = Bitmap::new(w, h);
        let (s, c) = 10f32.to_radians().sin_cos();
        for k in 0..240 {
            for t in 0..14 {
                let (px, py) = (k as f32, t as f32);
                let (x, y) = (30.0 + px * c - py * s, 60.0 + px * s + py * c);
                m.data[y as usize * w + x as usize] = true;
            }
        }
        assert!((tilt_of(&m).abs() - 10.0).abs() <= 1.0, "{}", tilt_of(&m));
    }
}
