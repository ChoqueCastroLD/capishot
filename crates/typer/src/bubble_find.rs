//! The speech bubble under a click (the Bubble Type tool, `type.createInBubble`): the light
//! interior around the point, found on the page without its type layers.
//!
//! A balloon's outline is rarely closed: a thin stroke breaks, the tail meets the art, a
//! lettering pass leaves a gap. So the flood doesn't walk the light pixels themselves but those
//! with no dark pixel within [`GAP_RADIUS`] (a morphological close of the outline, computed only
//! where the flood goes): a gap narrower than twice the radius stays shut. The flooded core is
//! then grown back by the same radius, inside the light pixels, to reach the outline again.
//!
//! What isn't a bubble: a dark seed, a region touching the page edge (the page background, a
//! panel gutter), one larger than [`MAX_PAGE_SHARE`] of the page, one smaller than a line of text
//! fits ([`crate::bubble::MIN_SIDE`], [`crate::bubble::MIN_AREA`]), one whose edge mostly meets no
//! dark outline ([`crate::bubble::MIN_OUTLINE_DARK`]: a light patch of art), or one that fills
//! too little of its box. The body the text goes in drops the tail with TypeR's adaptive opening
//! ([`crate::bubble::opening_radius`]).

use crate::bubble::{self, Mask};

/// Outline gaps up to about twice this wide (px) don't let the flood out.
pub const GAP_RADIUS: i32 = 2;
/// A light pixel is at most this much darker (luma, 0–255) than the clicked one.
pub const LIGHT_TOLERANCE: f64 = 60.0;
/// How far from the click (px) a light pixel to start from is looked for.
pub const REACH: i32 = 24;
/// The darkest paper (luma) a bubble may have: a light grey tone still counts.
pub const MIN_PAPER: f64 = 96.0;
/// Starts tried at most (each on another side of a letter or the outline)…
const MAX_STARTS: usize = 6;
/// …at least this far (px) from each other.
const SPREAD: i32 = 6;
/// The largest share of the page a bubble may cover.
pub const MAX_PAGE_SHARE: f64 = 0.4;
/// The flood stops after this many pixels (a bubble this large is the page, not a balloon).
pub const MAX_PIXELS: usize = 6_000_000;

/// Why there is no bubble at a point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Miss {
    /// The point is outside the page or in a dark area.
    Dark,
    /// The light region reaches the page edge.
    Edge,
    /// Larger than a bubble can be.
    TooBig,
    /// Too small for a line of text.
    TooSmall,
    /// Light, closed and sized like a bubble, but without a bubble's outline or shape.
    NotBubble,
}

impl Miss {
    pub fn code(self) -> &'static str {
        match self {
            Miss::Dark => "dark",
            Miss::Edge => "edge",
            Miss::TooBig => "tooBig",
            Miss::TooSmall => "tooSmall",
            Miss::NotBubble => "notBubble",
        }
    }
}

/// A bubble found at a point.
#[derive(Clone, Debug)]
pub struct Found {
    /// The whole interior, tail included, holes (lettering) filled.
    pub interior: Mask,
    /// The body the text goes in: the interior without its tail.
    pub body: Mask,
}

fn luma(p: [u8; 4]) -> f64 {
    if p[3] < 128 { 0.0 } else { 0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]) }
}

/// The bubble around `seed` on an 8-bit RGBA image covering `area`. A click on lettering or on
/// the outline still finds it: the flood starts from the nearest light pixels around the click
/// (up to [`REACH`] px away), trying the next one when a start leads nowhere (the other side of
/// the outline). What counts as light is relative to the start, so a bubble lighter than a grey
/// or toned page is found as well as a white one.
pub fn find(img: &[[u8; 4]], area: photocraft_geom::Rect, seed: (i32, i32)) -> Result<Found, Miss> {
    let (w, h) = (area.width() as usize, area.height() as usize);
    if w == 0 || h == 0 || img.len() != w * h || !area.contains(seed.0, seed.1) {
        return Err(Miss::Dark);
    }
    let lum: Vec<f32> = img.iter().map(|p| luma(*p) as f32).collect();
    let at = |x: i32, y: i32| (y - area.y0) as usize * w + (x - area.x0) as usize;
    // Light starts near the click, nearest first.
    let mut starts: Vec<(i32, (i32, i32))> = Vec::new();
    for dy in -REACH..=REACH {
        for dx in -REACH..=REACH {
            let p = (seed.0 + dx, seed.1 + dy);
            if area.contains(p.0, p.1) && f64::from(lum[at(p.0, p.1)]) >= MIN_PAPER && dx * dx + dy * dy <= REACH * REACH {
                starts.push((dx * dx + dy * dy, p));
            }
        }
    }
    starts.sort_by_key(|(d, _)| *d);
    let mut tried: Vec<(i32, i32)> = Vec::new();
    let mut first_miss = None;
    for (_, start) in starts {
        if tried.len() >= MAX_STARTS {
            break;
        }
        if tried.iter().any(|t| (t.0 - start.0).pow(2) + (t.1 - start.1).pow(2) < SPREAD * SPREAD) {
            continue;
        }
        // The paper here: the lightest pixel right around the start.
        let mut paper = 0.0f32;
        for (dx, dy) in [(0, 0), (-1, 0), (1, 0), (0, -1), (0, 1), (-2, 0), (2, 0), (0, -2), (0, 2)] {
            if area.contains(start.0 + dx, start.1 + dy) {
                paper = paper.max(lum[at(start.0 + dx, start.1 + dy)]);
            }
        }
        let light_min = (f64::from(paper) - LIGHT_TOLERANCE.min(0.4 * f64::from(paper))) as f32;
        match flood_from(img, area, &lum, start, light_min) {
            Ok(found) => return Ok(found),
            // Too dark to start here (a letter's edge): not a try.
            Err(Miss::Dark) => continue,
            Err(m) => {
                tried.push(start);
                first_miss.get_or_insert(m);
            }
        }
    }
    Err(first_miss.unwrap_or(Miss::Dark))
}

/// The flood from `start` over pixels with no pixel darker than `light_min` within the radius.
fn flood_from(img: &[[u8; 4]], area: photocraft_geom::Rect, lum: &[f32], start: (i32, i32), light_min: f32) -> Result<Found, Miss> {
    let (w, h) = (area.width() as usize, area.height() as usize);
    let at = |x: i32, y: i32| (y - area.y0) as usize * w + (x - area.x0) as usize;
    let r = GAP_RADIUS;
    let disc: Vec<(i32, i32)> = (-r..=r).flat_map(|dy| (-r..=r).map(move |dx| (dx, dy))).filter(|(dx, dy)| dx * dx + dy * dy <= r * r).collect();
    // Passable: no dark pixel within the radius (pixels off the page count as light, so a
    // bubble cut by the page edge still reaches it and is refused as touching it).
    let passable = |x: i32, y: i32| {
        disc.iter().all(|(dx, dy)| {
            let (px, py) = (x + dx, y + dy);
            !area.contains(px, py) || lum[at(px, py)] >= light_min
        })
    };
    if !passable(start.0, start.1) {
        return Err(Miss::Dark);
    }
    let seed = start;
    // The flood over passable pixels; the core stays `r` inside the page, so one that gets
    // there touches the edge.
    let cap = MAX_PIXELS.min(((w * h) as f64 * MAX_PAGE_SHARE) as usize).max(1);
    let mut seen = vec![false; w * h];
    let mut core: Vec<(i32, i32)> = vec![seed];
    seen[at(seed.0, seed.1)] = true;
    let mut i = 0;
    let (mut x0, mut y0, mut x1, mut y1) = (seed.0, seed.1, seed.0, seed.1);
    while let Some(&(x, y)) = core.get(i) {
        i += 1;
        if x - r <= area.x0 || y - r <= area.y0 || x + r >= area.x1 - 1 || y + r >= area.y1 - 1 {
            return Err(Miss::Edge);
        }
        if core.len() > cap {
            return Err(Miss::TooBig);
        }
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        for n in [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)] {
            let j = at(n.0, n.1);
            if !seen[j] && passable(n.0, n.1) {
                seen[j] = true;
                core.push(n);
            }
        }
    }
    // Grow the core back by the radius, over light pixels, to the outline.
    let pad = r + 1;
    let (mx, my) = (x0 - pad, y0 - pad);
    let (mw, mh) = ((x1 - x0 + 1 + 2 * pad) as usize, (y1 - y0 + 1 + 2 * pad) as usize);
    let mut interior = Mask::new(mx, my, mw, mh).ok_or(Miss::TooBig)?;
    for &(x, y) in &core {
        interior.set(x, y, true);
    }
    let to_core = photocraft_algo::selection::edt(&interior.px, mw, mh);
    for (k, d) in to_core.iter().enumerate() {
        let (x, y) = (mx + (k % mw) as i32, my + (k / mw) as i32);
        if *d <= r as f32 + 0.5 && area.contains(x, y) && lum[at(x, y)] >= light_min {
            interior.px[k] = true;
        }
    }
    interior.fill_holes();
    judge(img, area, interior)
}

fn judge(img: &[[u8; 4]], area: photocraft_geom::Rect, interior: Mask) -> Result<Found, Miss> {
    let b = interior.bounds().ok_or(Miss::TooSmall)?;
    let count = interior.count();
    if (count as f64) < bubble::MIN_AREA || f64::from(b.width().min(b.height())) < bubble::MIN_SIDE {
        return Err(Miss::TooSmall);
    }
    if (count as f64) > MAX_PAGE_SHARE * f64::from(area.width()) * f64::from(area.height()) {
        return Err(Miss::TooBig);
    }
    let fill = count as f64 / (f64::from(b.width()) * f64::from(b.height())).max(1.0);
    if fill < bubble::MIN_FILL || bubble::outline_dark(&interior, img, area) < bubble::MIN_OUTLINE_DARK {
        return Err(Miss::NotBubble);
    }
    let radius = bubble::opening_radius(f64::from(b.width()), f64::from(b.height()));
    let mut body = if radius > 0 { interior.opened(radius as f32).unwrap_or_else(|| interior.clone()) } else { interior.clone() };
    // The opening can split a body pinched in the middle: keep the larger piece.
    body = largest_piece(&body);
    let bb = body.bounds().ok_or(Miss::TooSmall)?;
    if f64::from(bb.width().min(bb.height())) < bubble::MIN_SIDE {
        return Err(Miss::TooSmall);
    }
    Ok(Found { interior, body })
}

/// A region at least this rectangular (its area over its minimum-area bounding rectangle's) is a
/// box, a narration caption: an ellipse fills 79 % of its rectangle, a rounded rectangle with
/// corners a tenth of its side 98 %.
pub const BOX_RECTANGULARITY: f64 = 0.92;

/// How rectangular `m` is: its pixel count over the area of its minimum-area bounding rectangle
/// (any angle; rotating calipers over the convex hull of the pixels' corners), and that
/// rectangle's corners in document pixels, clockwise from the first. `None` for an empty mask.
pub fn rectangularity(m: &Mask) -> Option<(f64, [(f64, f64); 4])> {
    // The hull of each row's outer pixel corners holds every pixel's.
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for (y, row) in m.px.chunks(m.width.max(1)).enumerate() {
        let Some(first) = row.iter().position(|p| *p) else { continue };
        let last = row.iter().rposition(|p| *p).unwrap_or(first);
        let (x0, x1, y0) = (f64::from(m.x0) + first as f64, f64::from(m.x0) + last as f64 + 1.0, f64::from(m.y0) + y as f64);
        pts.extend([(x0, y0), (x0, y0 + 1.0), (x1, y0), (x1, y0 + 1.0)]);
    }
    let hull = convex_hull(pts);
    if hull.len() < 3 {
        return None;
    }
    let n = hull.len();
    let mut best: Option<(f64, [(f64, f64); 4])> = None;
    for i in 0..n {
        let (a, b) = (hull[i], hull[(i + 1) % n]);
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        if len < 1e-9 {
            continue;
        }
        let (ux, uy) = ((b.0 - a.0) / len, (b.1 - a.1) / len);
        let (mut s0, mut s1, mut t0, mut t1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for p in &hull {
            let (s, t) = (p.0 * ux + p.1 * uy, -p.0 * uy + p.1 * ux);
            (s0, s1, t0, t1) = (s0.min(s), s1.max(s), t0.min(t), t1.max(t));
        }
        let area = (s1 - s0) * (t1 - t0);
        if best.as_ref().is_none_or(|(a, _)| area < *a) {
            let pt = |s: f64, t: f64| (s * ux - t * uy, s * uy + t * ux);
            best = Some((area, [pt(s0, t0), pt(s1, t0), pt(s1, t1), pt(s0, t1)]));
        }
    }
    let (area, corners) = best?;
    Some(((m.count() as f64 / area.max(1.0)).min(1.0), corners))
}

/// Andrew's monotone chain: the convex hull, counter-clockwise, without collinear points.
fn convex_hull(mut pts: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    pts.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: (f64, f64), a: (f64, f64), b: (f64, f64)| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
    let mut hull: Vec<(f64, f64)> = Vec::with_capacity(pts.len() * 2);
    for pass in 0..2 {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &(f64, f64)>> = if pass == 0 { Box::new(pts.iter()) } else { Box::new(pts.iter().rev()) };
        for &p in iter {
            while hull.len() >= start + 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    hull
}

/// The largest 4-connected piece of `m`.
fn largest_piece(m: &Mask) -> Mask {
    let (w, h) = (m.width, m.height);
    let mut label = vec![0u32; w * h];
    let (mut best, mut best_n, mut next) = (0u32, 0usize, 0u32);
    for start in 0..w * h {
        if !m.px[start] || label[start] != 0 {
            continue;
        }
        next += 1;
        let mut n = 0usize;
        let mut stack = vec![start];
        label[start] = next;
        while let Some(i) = stack.pop() {
            n += 1;
            let (x, y) = (i % w, i / w);
            let mut push = |j: usize| {
                if m.px[j] && label[j] == 0 {
                    label[j] = next;
                    stack.push(j);
                }
            };
            if x > 0 {
                push(i - 1);
            }
            if x + 1 < w {
                push(i + 1);
            }
            if y > 0 {
                push(i - w);
            }
            if y + 1 < h {
                push(i + w);
            }
        }
        if n > best_n {
            (best, best_n) = (next, n);
        }
    }
    if next <= 1 {
        return m.clone();
    }
    Mask { px: label.iter().map(|l| *l == best).collect(), ..m.clone() }
}

#[cfg(test)]
#[path = "bubble_find_tests.rs"]
mod tests;
