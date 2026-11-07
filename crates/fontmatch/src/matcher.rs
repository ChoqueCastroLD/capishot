//! Which faces draw these letters: each letter against every glyph on a coarse grid, then the
//! closest faces again by their edges. Same zone, same list, same order.

use std::collections::HashSet;

use crate::index::{GRID, Library, coarse};
use crate::look::{Look, measure};
use crate::segment::{distance, letters};
use crate::{Bitmap, CAP, Zone};

/// How many faces reach the edge comparison.
const SHORTLIST: usize = 60;
/// Its share of letters that count: a stuck pair or a stray mark shouldn't decide.
const BEST_SHARE: f32 = 0.8;
/// A glyph's size may differ this much from the letter's and still be compared.
const SIZE_SLACK: f32 = 0.3;
const EDGE_CAP: f32 = 12.0;

/// A face that may be the one, with what the lettering looks like.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub family: String,
    pub style: String,
    /// Lower is closer: the mean distance between the edges, at [`CAP`] pixels a letter.
    pub distance: f32,
    pub look: Look,
}

fn edges(b: &Bitmap, pad: usize) -> (Vec<(i32, i32)>, Vec<f32>, usize, usize) {
    let (w, h) = (b.width + 2 * pad, b.height + 2 * pad);
    let mut p = Bitmap::new(w, h);
    for y in 0..b.height {
        for x in 0..b.width {
            p.data[(y + pad) * w + x + pad] = b.get(x, y);
        }
    }
    let mut e = Vec::new();
    let mut not_edge = Bitmap { width: w, height: h, data: vec![true; w * h] };
    for y in 0..h {
        for x in 0..w {
            if p.get(x, y) && (x == 0 || y == 0 || x + 1 == w || y + 1 == h || !p.get(x - 1, y) || !p.get(x + 1, y) || !p.get(x, y - 1) || !p.get(x, y + 1)) {
                e.push((x as i32, y as i32));
                not_edge.data[y * w + x] = false;
            }
        }
    }
    (e, distance(&not_edge), w, h)
}

type Edged = (Vec<(i32, i32)>, Vec<f32>, usize, usize);

fn one_way(a: &[(i32, i32)], ox: i32, oy: i32, d: &[f32], w: usize, h: usize) -> f32 {
    let mut sum = 0.0;
    for (x, y) in a {
        let (xx, yy) = (x + ox, y + oy);
        sum += if xx >= 0 && yy >= 0 && (xx as usize) < w && (yy as usize) < h { d[yy as usize * w + xx as usize].min(EDGE_CAP) } else { EDGE_CAP };
    }
    sum / a.len().max(1) as f32
}

/// Both ways, centred on each other.
fn chamfer(a: &Edged, b: &Edged) -> f32 {
    let (ox, oy) = ((b.2 as i32 - a.2 as i32) / 2, (b.3 as i32 - a.3 as i32) / 2);
    (one_way(&a.0, ox, oy, &b.1, b.2, b.3) + one_way(&b.0, -ox, -oy, &a.1, a.2, a.3)) / 2.0
}

/// Which faces a search looks at.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    /// Only these families; empty: all of them.
    pub only: HashSet<String>,
    /// These families count as a little closer: the work's fonts first.
    pub prefer: HashSet<String>,
}

/// How much closer a preferred family counts.
const PREFER: f32 = 0.8;

/// The `k` faces closest to the zone's lettering, one per design, closest first. Empty when the zone
/// holds nothing that reads as letters.
pub fn find(lib: &Library, zone: &Zone<'_>, k: usize) -> Vec<Candidate> {
    find_in(lib, zone, k, &Scope::default())
}

/// [`find`] among the faces `scope` says.
pub fn find_in(lib: &Library, zone: &Zone<'_>, k: usize, scope: &Scope) -> Vec<Candidate> {
    if lib.is_empty() {
        return Vec::new();
    }
    let Some(found) = letters(zone) else { return Vec::new() };
    let look = measure(zone, &found);
    let n = lib.owner.len();
    let cell = GRID * GRID;
    let mut totals = vec![0f32; lib.faces.len()];
    let mut best = vec![f32::MAX; lib.faces.len()];
    let mut q = Vec::with_capacity(cell);
    for l in &found.letters {
        q.clear();
        coarse(l, &mut q);
        let (lw, lh) = (l.width as f32 / CAP as f32, l.height as f32 / CAP as f32);
        best.fill(f32::MAX);
        for g in 0..n {
            let c = &lib.coarse[g * cell..(g + 1) * cell];
            let shape: u32 = c.iter().zip(&q).map(|(a, b)| u32::from(a.abs_diff(*b))).sum();
            let [gw, gh] = lib.sizes[g];
            let d = shape as f32 / (cell as f32 * 255.0) + 0.5 * (gw - lw).abs() + 0.5 * (gh - lh).abs();
            let f = lib.owner[g] as usize;
            if d < best[f] {
                best[f] = d;
            }
        }
        for (t, b) in totals.iter_mut().zip(&best) {
            *t += *b;
        }
    }
    let allowed = |f: usize| scope.only.is_empty() || scope.only.contains(&lib.faces[f].family);
    let mut order: Vec<usize> = (0..lib.faces.len()).filter(|f| allowed(*f)).collect();
    order.sort_by(|a, b| totals[*a].total_cmp(&totals[*b]).then(a.cmp(b)));
    order.truncate(SHORTLIST);
    let queries: Vec<Edged> = found.letters.iter().map(|l| edges(l, 6)).collect();
    let mut scored: Vec<(f32, usize)> = order
        .into_iter()
        .map(|f| {
            let face = &lib.faces[f];
            let glyphs: Vec<Edged> = face.glyphs.iter().map(|g| edges(&g.bitmap, 6)).collect();
            let mut per: Vec<f32> = found
                .letters
                .iter()
                .zip(&queries)
                .map(|(l, q)| {
                    let mut best = EDGE_CAP;
                    for (g, e) in face.glyphs.iter().zip(&glyphs) {
                        let (dw, dh) = (g.bitmap.width.abs_diff(l.width) as f32, g.bitmap.height.abs_diff(l.height) as f32);
                        if dh <= l.height as f32 * SIZE_SLACK && dw <= (l.width as f32 * SIZE_SLACK).max(6.0) {
                            best = best.min(chamfer(q, e));
                        }
                    }
                    best
                })
                .collect();
            per.sort_by(f32::total_cmp);
            let keep = ((per.len() as f32 * BEST_SHARE) as usize).max(1);
            let d = per[..keep].iter().sum::<f32>() / keep as f32;
            (if scope.prefer.contains(&face.family) { d * PREFER } else { d }, f)
        })
        .collect();
    scored.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    // Five that differ: not five weights of one family, nor copies of one design.
    let mut names = HashSet::new();
    let mut taken: Vec<usize> = Vec::new();
    let mut out = Vec::new();
    for (d, f) in scored {
        let face = &lib.faces[f];
        if !names.insert(face.base_name()) || taken.iter().any(|t| lib.faces[*t].near_copy(face)) {
            continue;
        }
        taken.push(f);
        out.push(Candidate { family: face.family.clone(), style: face.style.clone(), distance: d, look: look.for_face(face.size_for_cap) });
        if out.len() == k {
            break;
        }
    }
    out
}
