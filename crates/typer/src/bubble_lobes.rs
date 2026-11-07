//! Two balloons drawn into one, each with its own text: the flood of [`crate::bubble_find`] finds
//! them as one region. They overlap, so there may be no narrow neck between them; what they do have
//! is a notch on each side where their outlines cross. Here the region is cut along the line
//! between its two notches, by its shape alone, so it works on a clean page too.
//!
//! A notch is a dent in the outline, measured from the region's convex hull. A tail makes two, with
//! little behind them; a dent is too shallow; a cloud or a burst has too many to be two balloons.
//! Lettering across the line also says one balloon.

use photocraft_algo::selection::edt;
use photocraft_geom::Rect;

use crate::bubble::Mask;
use crate::bubble_find::{Found, body_of, convex_hull};

/// A notch is at least this deep against the square root of the region's area.
const MIN_DEPTH: f64 = 0.07;
/// More notches than this is a cloud or a burst.
const MAX_NOTCHES: usize = 6;
/// The shallower notch of a pair, at least this deep against the line between them.
const MIN_DEPTH_OVER_CUT: f64 = 0.08;
/// Each side holds at least this share of the region.
const MIN_AREA: f64 = 0.15;
/// This much of the line stays inside the region.
const MIN_INSIDE: f64 = 0.9;
/// Lettering on this share of the line means one text across it.
const MAX_INK_ON_CUT: f64 = 0.25;
/// Darker than this (luma, 0–255) is lettering.
const INK: f64 = 128.0;
/// Letters that touch the outline open channels in the region, as wide as a line of them: closed
/// before reading its outline, by this against their height, or with no letters to measure by
/// [`CLEAN_CLOSE`] px. At most [`MAX_CLOSE`] against the square root of the region's area.
const CLOSE: f64 = 0.75;
const CLEAN_CLOSE: f64 = 3.0;
const MAX_CLOSE: f64 = 0.08;
/// Balloons joined in a row, at most.
const MAX_CUTS: usize = 3;

fn luma(p: [u8; 4]) -> f64 {
    0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2])
}

struct Grid {
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    on: Vec<bool>,
}

impl Grid {
    /// The mask's box with a margin outside it, so the box's sides count as outline.
    fn of(m: &Mask) -> Option<Grid> {
        Grid::with_margin(m, 1)
    }

    fn with_margin(m: &Mask, margin: i32) -> Option<Grid> {
        let b = m.bounds()?;
        let (x0, y0) = (b.x0 - margin, b.y0 - margin);
        let (w, h) = (b.width() as usize + 2 * margin as usize, b.height() as usize + 2 * margin as usize);
        let mut on = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                on[y * w + x] = m.get(x0 + x as i32, y0 + y as i32);
            }
        }
        Some(Grid { x0, y0, w, h, on })
    }

    /// Dents narrower than twice `r` filled.
    fn closed(&mut self, r: f32) {
        let grown: Vec<bool> = edt(&self.on, self.w, self.h).iter().map(|d| *d <= r).collect();
        let out: Vec<bool> = grown.iter().map(|v| !v).collect();
        self.on = edt(&out, self.w, self.h).iter().map(|d| *d > r).collect();
    }

    fn around(&self, i: usize) -> impl Iterator<Item = usize> + use<> {
        let (w, h) = (self.w, self.h);
        let (x, y) = (i % w, i / w);
        (y.saturating_sub(1)..(y + 2).min(h)).flat_map(move |ny| (x.saturating_sub(1)..(x + 2).min(w)).map(move |nx| ny * w + nx)).filter(move |n| *n != i)
    }
}

/// The region's outer outline, in order (Moore neighbour tracing).
fn outline(g: &Grid) -> Vec<usize> {
    const WALK: [(i32, i32); 8] = [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)];
    let Some(start) = g.on.iter().position(|v| *v) else { return Vec::new() };
    let mut out = vec![start];
    let (mut at, mut from) = (start, 4usize);
    for _ in 0..g.on.len() * 2 {
        let (x, y) = ((at % g.w) as i32, (at / g.w) as i32);
        let mut next = None;
        for k in 0..8 {
            let dir = (from + 1 + k) % 8;
            let (nx, ny) = (x + WALK[dir].0, y + WALK[dir].1);
            if nx >= 0 && ny >= 0 && (nx as usize) < g.w && (ny as usize) < g.h && g.on[ny as usize * g.w + nx as usize] {
                next = Some((ny as usize * g.w + nx as usize, (dir + 4) % 8));
                break;
            }
        }
        let Some((n, back)) = next else { break };
        if n == start && out.len() > 2 {
            break;
        }
        out.push(n);
        at = n;
        from = back;
    }
    out
}

/// A dent in the outline: where it is along it, and how deep.
#[derive(Clone, Copy)]
struct Notch {
    at: usize,
    depth: f64,
}

fn notches(line: &[usize], g: &Grid, min_depth: f64) -> Vec<Notch> {
    let xy = |i: usize| ((i % g.w) as f64, (i / g.w) as f64);
    let hull: std::collections::HashSet<(i64, i64)> = convex_hull(line.iter().map(|i| xy(*i)).collect()).iter().map(|p| (p.0 as i64, p.1 as i64)).collect();
    let corners: Vec<usize> = (0..line.len()).filter(|k| { let p = xy(line[*k]); hull.contains(&(p.0 as i64, p.1 as i64)) }).collect();
    let mut out = Vec::new();
    for (n, &a) in corners.iter().enumerate() {
        let b = corners[(n + 1) % corners.len()];
        let (pa, pb) = (xy(line[a]), xy(line[b]));
        let (dx, dy) = (pb.0 - pa.0, pb.1 - pa.1);
        let len = dx.hypot(dy).max(1.0);
        let span = (b + line.len() - a) % line.len();
        let mut best = Notch { at: a, depth: 0.0 };
        for k in 1..span {
            let at = (a + k) % line.len();
            let p = xy(line[at]);
            let depth = ((p.0 - pa.0) * dy - (p.1 - pa.1) * dx).abs() / len;
            if depth > best.depth {
                best = Notch { at, depth };
            }
        }
        if best.depth >= min_depth {
            out.push(best);
        }
    }
    out
}

/// The area the outline from `a` to `b` closes with the line back.
fn area_between(line: &[usize], g: &Grid, a: usize, b: usize) -> f64 {
    let xy = |i: usize| ((i % g.w) as f64, (i / g.w) as f64);
    let span = (b + line.len() - a) % line.len();
    let mut sum = 0.0;
    let mut prev = xy(line[a]);
    for k in 1..=span {
        let p = xy(line[(a + k) % line.len()]);
        sum += prev.0 * p.1 - p.0 * prev.1;
        prev = p;
    }
    let first = xy(line[a]);
    sum += prev.0 * first.1 - first.0 * prev.1;
    (sum / 2.0).abs()
}

/// The pixels of the line between two grid points.
fn along(g: &Grid, a: usize, b: usize) -> Vec<usize> {
    let (ax, ay, bx, by) = ((a % g.w) as f64, (a / g.w) as f64, (b % g.w) as f64, (b / g.w) as f64);
    let n = (bx - ax).abs().max((by - ay).abs()).ceil().max(1.0) as usize;
    (0..=n)
        .map(|k| {
            let t = k as f64 / n as f64;
            (ay + (by - ay) * t).round() as usize * g.w + (ax + (bx - ax) * t).round() as usize
        })
        .collect()
}

/// The pixels of the lobe at `seed`, or `None` when the mask is one balloon.
fn cut(m: &Mask, img: &[[u8; 4]], area: Rect, seed: (i32, i32)) -> Option<Mask> {
    let close = lettering_height(m, img, area).map_or(CLEAN_CLOSE, |h| (h * CLOSE).min(MAX_CLOSE * (m.count() as f64).sqrt()));
    let mut g = Grid::with_margin(m, close.ceil() as i32 + 2)?;
    g.closed(close as f32);
    let g = g;
    let total = g.on.iter().filter(|v| **v).count() as f64;
    let line = outline(&g);
    if line.len() < 16 {
        return None;
    }
    let found = notches(&line, &g, MIN_DEPTH * total.sqrt());
    if found.len() < 2 || found.len() > MAX_NOTCHES {
        return None;
    }
    let (iw, ih) = (area.width() as usize, area.height() as usize);
    let dark = |i: usize| {
        let (x, y) = (g.x0 + (i % g.w) as i32 - area.x0, g.y0 + (i / g.w) as i32 - area.y0);
        x >= 0 && y >= 0 && (x as usize) < iw && (y as usize) < ih && img.get(y as usize * iw + x as usize).is_some_and(|p| luma(*p) < INK)
    };
    // The pair most like two balloons: deep against the line between them.
    let mut best: Option<(f64, usize, usize)> = None;
    for (n, p) in found.iter().enumerate() {
        for q in &found[n + 1..] {
            let (a, b) = (line[p.at], line[q.at]);
            let length = ((a % g.w) as f64 - (b % g.w) as f64).hypot((a / g.w) as f64 - (b / g.w) as f64).max(1.0);
            let score = p.depth.min(q.depth) / length;
            let side = area_between(&line, &g, p.at, q.at);
            let smaller = side.min(total - side) / total;
            if score < MIN_DEPTH_OVER_CUT || smaller < MIN_AREA {
                continue;
            }
            let pixels = along(&g, a, b);
            let count = pixels.len() as f64;
            if pixels.iter().filter(|i| g.on[**i]).count() as f64 / count < MIN_INSIDE || pixels.iter().filter(|i| dark(**i)).count() as f64 / count > MAX_INK_ON_CUT {
                continue;
            }
            let score = score * smaller;
            if best.is_none_or(|(s, _, _)| score > s) {
                best = Some((score, a, b));
            }
        }
    }
    let (_, a, b) = best?;
    // Each side of the line; the line goes with the side of the click.
    let side_of = |x: f64, y: f64| {
        let (ax, ay, bx, by) = ((a % g.w) as f64, (a / g.w) as f64, (b % g.w) as f64, (b / g.w) as f64);
        (bx - ax) * (y - ay) - (by - ay) * (x - ax) >= 0.0
    };
    let wanted = side_of(f64::from(seed.0 - g.x0), f64::from(seed.1 - g.y0));
    let mut barrier = g.on.clone();
    for i in along(&g, a, b) {
        barrier[i] = false;
    }
    let mut seen = vec![false; g.on.len()];
    let pad = close.ceil() as usize + 2;
    let mut lobe = Mask::new(m.x0 - pad as i32, m.y0 - pad as i32, m.width + 2 * pad, m.height + 2 * pad)?;
    let mut stack = Vec::new();
    let mut found_lobe = false;
    for start in 0..barrier.len() {
        if !barrier[start] || seen[start] || side_of((start % g.w) as f64, (start / g.w) as f64) != wanted {
            continue;
        }
        // The flood from each piece on the click's side: a piece past the far end of the line
        // belongs to the other balloon.
        seen[start] = true;
        stack.push(start);
        let mut piece = Vec::new();
        while let Some(i) = stack.pop() {
            piece.push(i);
            // Straight neighbours: a diagonal step would cross the line.
            for n in [i.wrapping_sub(1), i + 1, i.wrapping_sub(g.w), i + g.w] {
                if barrier.get(n).copied().unwrap_or(false) && !seen[n] {
                    seen[n] = true;
                    stack.push(n);
                }
            }
        }
        if piece.len() as f64 >= MIN_AREA * total {
            found_lobe = true;
            for i in piece {
                lobe.set(g.x0 + (i % g.w) as i32, g.y0 + (i / g.w) as i32, true);
            }
        }
    }
    if !found_lobe {
        return None;
    }
    for i in along(&g, a, b) {
        lobe.set(g.x0 + (i % g.w) as i32, g.y0 + (i / g.w) as i32, true);
    }
    Some(lobe)
}

/// `found` as the single balloon at `seed`, when it's several joined.
pub fn lobe_at(found: Found, img: &[[u8; 4]], area: Rect, seed: (i32, i32)) -> Found {
    let mut interior = found.interior.clone();
    let mut changed = false;
    for _ in 0..MAX_CUTS {
        match cut(&interior, img, area, seed) {
            Some(lobe) => {
                interior = lobe;
                changed = true;
            }
            None => break,
        }
    }
    if !changed {
        return found;
    }
    match body_of(&interior) {
        Some(body) => Found { interior, body },
        None => found,
    }
}

/// How tall the lettering in `m` is (px), from what's dark in it, if there's lettering to measure.
pub fn lettering_height(m: &Mask, img: &[[u8; 4]], area: Rect) -> Option<f64> {
    let g = Grid::of(m)?;
    let (iw, ih) = (area.width() as usize, area.height() as usize);
    let tall = (m.bounds()?.height() / 2).max(8) as usize;
    let mut ink = vec![false; g.on.len()];
    for (i, v) in ink.iter_mut().enumerate() {
        let (x, y) = (g.x0 + (i % g.w) as i32 - area.x0, g.y0 + (i / g.w) as i32 - area.y0);
        *v = g.on[i] && x >= 0 && y >= 0 && (x as usize) < iw && (y as usize) < ih && img.get(y as usize * iw + x as usize).is_some_and(|p| luma(*p) < INK);
    }
    let mut heights = Vec::new();
    let mut stack = Vec::new();
    for start in 0..ink.len() {
        if !ink[start] {
            continue;
        }
        ink[start] = false;
        stack.push(start);
        let (mut top, mut bottom, mut count) = (usize::MAX, 0, 0u32);
        while let Some(i) = stack.pop() {
            top = top.min(i / g.w);
            bottom = bottom.max(i / g.w);
            count += 1;
            for n in g.around(i) {
                if ink[n] {
                    ink[n] = false;
                    stack.push(n);
                }
            }
        }
        let h = bottom + 1 - top;
        // Specks and the marks over a letter are lower; whatever spans the balloon isn't lettering.
        if count >= 8 && (6..tall).contains(&h) {
            heights.push(h);
        }
    }
    if heights.len() < 3 {
        return None;
    }
    heights.sort_unstable();
    // Capitals rather than the small marks among them.
    Some(heights[heights.len() * 3 / 4] as f64)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const W: usize = 400;
    const H: usize = 300;

    fn mask_of(shape: impl Fn(f64, f64) -> bool) -> Mask {
        let mut m = Mask::new(0, 0, W, H).unwrap();
        for y in 0..H {
            for x in 0..W {
                if shape(x as f64, y as f64) {
                    m.set(x as i32, y as i32, true);
                }
            }
        }
        m
    }

    fn blank() -> Vec<[u8; 4]> {
        vec![[255, 255, 255, 255]; W * H]
    }

    fn area() -> Rect {
        Rect::new(0, 0, W as i32, H as i32)
    }

    fn circle(x: f64, y: f64, cx: f64, cy: f64, r: f64) -> bool {
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    }

    fn ellipse(x: f64, y: f64, cx: f64, cy: f64, a: f64, b: f64) -> bool {
        ((x - cx) / a).powi(2) + ((y - cy) / b).powi(2) <= 1.0
    }

    #[test]
    fn two_joined_balloons_are_cut_between_their_notches() {
        let m = mask_of(|x, y| circle(x, y, 130.0, 150.0, 90.0) || circle(x, y, 270.0, 150.0, 90.0));
        let left = cut(&m, &blank(), area(), (100, 150)).expect("two");
        assert!(left.get(100, 150) && !left.get(300, 150));
        let b = left.bounds().unwrap();
        assert!((195..=206).contains(&b.x1), "at the notches: {}", b.x1);
        let right = cut(&m, &blank(), area(), (300, 150)).expect("two");
        assert!(right.get(300, 150) && !right.get(100, 150));
    }

    #[test]
    fn balloons_overlapping_by_a_lot_are_two_too() {
        // Tall ellipses side by side: the region's widest depth is where they overlap, with no neck.
        let m = mask_of(|x, y| ellipse(x, y, 160.0, 150.0, 70.0, 135.0) || ellipse(x, y, 250.0, 150.0, 70.0, 135.0));
        let left = cut(&m, &blank(), area(), (130, 150)).expect("two");
        assert!(left.get(130, 150) && !left.get(280, 150));
        // Of different sizes and offset.
        let m = mask_of(|x, y| ellipse(x, y, 120.0, 180.0, 60.0, 100.0) || ellipse(x, y, 225.0, 120.0, 80.0, 110.0));
        let small = cut(&m, &blank(), area(), (110, 220)).expect("two");
        assert!(small.get(110, 220) && !small.get(250, 120));
    }

    #[test]
    fn one_balloon_stays_one() {
        let bumps = |x: f64, y: f64| {
            let (dx, dy) = (x - 200.0, y - 150.0);
            dx.hypot(dy) <= 100.0 + 22.0 * (dy.atan2(dx) * 9.0).cos()
        };
        for (name, shape) in [
            ("ellipse", mask_of(|x, y| ellipse(x, y, 200.0, 150.0, 170.0, 80.0))),
            ("a tail", mask_of(|x, y| ellipse(x, y, 200.0, 120.0, 150.0, 90.0) || ((190.0..210.0).contains(&x) && (120.0..280.0).contains(&y)))),
            ("a slight dent", mask_of(|x, y| circle(x, y, 170.0, 150.0, 110.0) || circle(x, y, 230.0, 150.0, 110.0))),
            ("a box", mask_of(|x, y| (40.0..360.0).contains(&x) && (60.0..240.0).contains(&y))),
            ("a cloud", mask_of(bumps)),
        ] {
            assert!(cut(&shape, &blank(), area(), (200, 150)).is_none(), "{name}");
        }
    }

    #[test]
    fn lettering_across_the_line_is_one_text() {
        let m = mask_of(|x, y| circle(x, y, 130.0, 150.0, 90.0) || circle(x, y, 270.0, 150.0, 90.0));
        let mut img = blank();
        for y in 100..200 {
            for x in 190..210 {
                img[y * W + x] = [0, 0, 0, 255];
            }
        }
        assert!(cut(&m, &img, area(), (100, 150)).is_none());
    }

    #[test]
    fn the_lettering_is_measured_and_a_clean_balloon_has_none() {
        let m = mask_of(|x, y| ellipse(x, y, 200.0, 150.0, 170.0, 120.0));
        assert_eq!(lettering_height(&m, &blank(), area()), None);
        let mut img = blank();
        for k in 0..8 {
            for y in 140..160 {
                for x in 100 + k * 25..112 + k * 25 {
                    img[y * W + x] = [0, 0, 0, 255];
                }
            }
        }
        assert_eq!(lettering_height(&m, &img, area()), Some(20.0));
    }
}
