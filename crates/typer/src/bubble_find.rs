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

/// The bubble around `seed` on an 8-bit RGBA image covering `area`.
pub fn find(img: &[[u8; 4]], area: photocraft_geom::Rect, seed: (i32, i32)) -> Result<Found, Miss> {
    let (w, h) = (area.width() as usize, area.height() as usize);
    if w == 0 || h == 0 || img.len() != w * h || !area.contains(seed.0, seed.1) {
        return Err(Miss::Dark);
    }
    let lum: Vec<f32> = img.iter().map(|p| luma(*p) as f32).collect();
    let at = |x: i32, y: i32| (y - area.y0) as usize * w + (x - area.x0) as usize;
    let r = GAP_RADIUS;
    // How far from the click the paper and a passable start are looked for (a click on a letter).
    let reach = 16;
    // The paper: the lightest pixel near the click (a click may land on a letter).
    let mut paper = 0.0f32;
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            if area.contains(seed.0 + dx, seed.1 + dy) {
                paper = paper.max(lum[at(seed.0 + dx, seed.1 + dy)]);
            }
        }
    }
    if f64::from(paper) < bubble::MIN_SEED_LUMA {
        return Err(Miss::Dark);
    }
    let light_min = (f64::from(paper) - LIGHT_TOLERANCE).max(bubble::MIN_SEED_LUMA) as f32;
    let disc: Vec<(i32, i32)> = (-r..=r).flat_map(|dy| (-r..=r).map(move |dx| (dx, dy))).filter(|(dx, dy)| dx * dx + dy * dy <= r * r).collect();
    // Passable: no dark pixel within the radius (pixels off the page count as light, so a
    // bubble cut by the page edge still reaches it and is refused as touching it).
    let passable = |x: i32, y: i32| {
        disc.iter().all(|(dx, dy)| {
            let (px, py) = (x + dx, y + dy);
            !area.contains(px, py) || lum[at(px, py)] >= light_min
        })
    };
    let seed = if passable(seed.0, seed.1) {
        seed
    } else {
        // A click on a letter or right next to the outline: the nearest passable pixel.
        let mut best: Option<(i32, (i32, i32))> = None;
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                let p = (seed.0 + dx, seed.1 + dy);
                let d = dx * dx + dy * dy;
                if area.contains(p.0, p.1) && passable(p.0, p.1) && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, p));
                }
            }
        }
        best.ok_or(Miss::TooSmall)?.1
    };
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
