//! From the model's probabilities to regions: a port of `manga_text_mask.py`'s `fill_hollow` and
//! `find_regions`.

/// One connected piece of a word or effect: what a person keeps or drops, because the model joins
/// art to the letters near it and a whole word would take the art with it.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    /// 1.. in manga reading order.
    pub id: u32,
    /// The word or effect it belongs to (the model script's region), 1.. in reading order.
    pub group: u32,
    /// `[x0, y0, x1, y1]`, in page pixels.
    pub bbox: [u32; 4],
    pub area: u64,
    /// The mean probability of its pixels.
    pub score: f32,
}

impl Region {
    /// Whether it starts chosen. The model's doubts are mostly art: hatching and marks with the
    /// size of a letter's piece. Dots are as small as they come and stay, doubted or not.
    pub fn suggested(&self) -> bool {
        !(self.score < DOUBT_ALWAYS || (self.score < DOUBT_SIZED && (DOUBT_MIN_AREA..DOUBT_MAX_AREA).contains(&self.area)))
    }
}

/// A page's pieces.
#[derive(Clone, Debug, PartialEq)]
pub struct Detection {
    pub width: usize,
    pub height: usize,
    /// The piece of each pixel, 0 where there is none.
    pub labels: Vec<u32>,
    pub regions: Vec<Region>,
}

const FILL_MIN_AREA: f32 = 300.0;
const FILL_MAX_RATIO: u64 = 3;
const FILL_MIN_WIDTH: f32 = 4.0;
const THRESHOLD: f32 = 0.5;
/// A mark needs a pixel this sure: the file's `confident`.
const CONFIDENT: f32 = 0.99;
const MIN_AREA: u64 = 12;
const MIN_REGION_AREA: u64 = 60;
const GAP: f32 = 0.7;
const MAX_GAP: f32 = 0.04;
const DOUBT_ALWAYS: f32 = 0.66;
const DOUBT_SIZED: f32 = 0.885;
const DOUBT_MIN_AREA: u64 = 100;
const DOUBT_MAX_AREA: u64 = 2000;
/// The most a selection grows.
pub const MAX_GROW: usize = 64;
/// A hole left inside a selection, up to this part of the page (per thousand), is cleaned with it:
/// on its own it's a leftover of the letter, the worst thing to copy from.
const HOLE_PER_MILLE: u64 = 3;

/// A component's box and area.
#[derive(Clone, Copy, Debug, Default)]
struct Stat {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    area: u64,
}

/// Connected components of `set`: labels (0 outside) and each label's stat (index 0 unused),
/// numbered in the order their first pixel comes.
fn components(set: &[bool], w: usize, h: usize, eight: bool) -> (Vec<u32>, Vec<Stat>) {
    fn find(p: &mut [u32], mut i: u32) -> u32 {
        while p[i as usize] != i {
            p[i as usize] = p[p[i as usize] as usize];
            i = p[i as usize];
        }
        i
    }
    let mut labels = vec![0u32; w * h];
    let mut parent: Vec<u32> = vec![0];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !set[i] {
                continue;
            }
            let mut near = [0u32; 4];
            let mut n = 0;
            if x > 0 && labels[i - 1] != 0 {
                near[n] = labels[i - 1];
                n += 1;
            }
            if y > 0 {
                if labels[i - w] != 0 {
                    near[n] = labels[i - w];
                    n += 1;
                }
                if eight {
                    if x > 0 && labels[i - w - 1] != 0 {
                        near[n] = labels[i - w - 1];
                        n += 1;
                    }
                    if x + 1 < w && labels[i - w + 1] != 0 {
                        near[n] = labels[i - w + 1];
                        n += 1;
                    }
                }
            }
            if n == 0 {
                let l = parent.len() as u32;
                parent.push(l);
                labels[i] = l;
            } else {
                let mut root = find(&mut parent, near[0]);
                for &o in &near[1..n] {
                    let r = find(&mut parent, o);
                    if r != root {
                        let (a, b) = (root.min(r), root.max(r));
                        parent[b as usize] = a;
                        root = a;
                    }
                }
                labels[i] = root;
            }
        }
    }
    // Final numbers, in order of first appearance.
    let mut number = vec![0u32; parent.len()];
    let mut stats = vec![Stat::default()];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if labels[i] == 0 {
                continue;
            }
            let r = find(&mut parent, labels[i]) as usize;
            if number[r] == 0 {
                number[r] = stats.len() as u32;
                stats.push(Stat { x0: x as u32, y0: y as u32, x1: x as u32 + 1, y1: y as u32 + 1, area: 0 });
            }
            let l = number[r];
            labels[i] = l;
            let s = &mut stats[l as usize];
            s.x0 = s.x0.min(x as u32);
            s.x1 = s.x1.max(x as u32 + 1);
            s.y1 = s.y1.max(y as u32 + 1);
            s.area += 1;
        }
    }
    (labels, stats)
}

/// `cv2.distanceTransform(.., DIST_L2, 3)`: distance to the nearest zero, by the chamfer weights.
fn distance(set: &[bool], w: usize, h: usize) -> Vec<f32> {
    const A: f32 = 0.955;
    const B: f32 = 1.3693;
    let big = (w + h) as f32 * 2.0;
    // Past the edge is not zero in OpenCV's sense: it copies a border of "far".
    let mut d: Vec<f32> = set.iter().map(|s| if *s { big } else { 0.0 }).collect();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if d[i] == 0.0 {
                continue;
            }
            let mut v = d[i];
            if x > 0 {
                v = v.min(d[i - 1] + A);
            }
            if y > 0 {
                v = v.min(d[i - w] + A);
                if x > 0 {
                    v = v.min(d[i - w - 1] + B);
                }
                if x + 1 < w {
                    v = v.min(d[i - w + 1] + B);
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
                v = v.min(d[i + 1] + A);
            }
            if y + 1 < h {
                v = v.min(d[i + w] + A);
                if x + 1 < w {
                    v = v.min(d[i + w + 1] + B);
                }
                if x > 0 {
                    v = v.min(d[i + w - 1] + B);
                }
            }
            d[i] = v;
        }
    }
    d
}

/// `set` grown by a square of side `2 r + 1`.
fn dilate_square(set: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    let mut rows = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if set[y * w + x] {
                for v in &mut rows[y * w + x.saturating_sub(r)..y * w + (x + r + 1).min(w)] {
                    *v = true;
                }
            }
        }
    }
    let mut out = vec![false; w * h];
    for x in 0..w {
        for y in 0..h {
            if rows[y * w + x] {
                for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
                    out[yy * w + x] = true;
                }
            }
        }
    }
    out
}

fn percentile(values: &mut [f32], p: f32) -> f32 {
    values.sort_by(f32::total_cmp);
    let at = p / 100.0 * (values.len() - 1) as f32;
    let (lo, hi) = (at.floor() as usize, at.ceil() as usize);
    values[lo] + (values[hi] - values[lo]) * (at - lo as f32)
}

/// Holes of the mask that are flat bright paper and much wider than the line around them become
/// text: the white body of a large outlined letter when the model traced its outline.
pub fn fill_hollow(prob: &mut [f32], rgb: &[u8], w: usize, h: usize) {
    let set: Vec<bool> = prob.iter().map(|p| *p >= THRESHOLD).collect();
    if !set.iter().any(|s| *s) {
        return;
    }
    let gray: Vec<u8> = rgb.chunks_exact(3).map(|c| ((u32::from(c[0]) * 4899 + u32::from(c[1]) * 9617 + u32::from(c[2]) * 1868 + 8192) >> 14) as u8).collect();
    // The saturation of OpenCV's 8-bit HSV.
    let saturated = |i: usize| {
        let c = &rgb[i * 3..i * 3 + 3];
        let (max, min) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
        max != 0 && (f32::from(max - min) * 255.0 / f32::from(max)).round() > 110.0
    };
    let k = (h as f32 / 1600.0).powi(2);
    let holes: Vec<bool> = set.iter().map(|s| !*s).collect();
    let (lab, st) = components(&holes, w, h, false);
    let (mlab, mst) = components(&set, w, h, true);
    let mut border = vec![false; st.len()];
    for x in 0..w {
        border[lab[x] as usize] = true;
        border[lab[(h - 1) * w + x] as usize] = true;
    }
    for y in 0..h {
        border[lab[y * w] as usize] = true;
        border[lab[y * w + w - 1] as usize] = true;
    }
    for i in 1..st.len() {
        let s = st[i];
        if border[i] || (s.area as f32) < FILL_MIN_AREA * k {
            continue;
        }
        let (bx, by, bw, bh) = (s.x0 as usize, s.y0 as usize, (s.x1 - s.x0) as usize, (s.y1 - s.y0) as usize);
        // The mask component most of its ring belongs to.
        let mut around: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
        for y in by..by + bh {
            for x in bx..bx + bw {
                if lab[y * w + x] == i as u32 {
                    continue;
                }
                let near = (y.max(by + 1) - 1..(y + 2).min(by + bh)).any(|yy| (x.max(bx + 1) - 1..(x + 2).min(bx + bw)).any(|xx| lab[yy * w + xx] == i as u32));
                let m = mlab[y * w + x];
                if near && m > 0 {
                    *around.entry(m).or_insert(0) += 1;
                }
            }
        }
        // `np.bincount(..).argmax()`: the lowest label among the most frequent.
        let Some((&j, _)) = around.iter().max_by_key(|(l, n)| (**n, std::cmp::Reverse(**l))) else { continue };
        if s.area > FILL_MAX_RATIO * mst[j as usize].area {
            continue;
        }
        let (mut sum, mut sq, mut n) = (0f64, 0f64, 0f64);
        for y in by..by + bh {
            for x in bx..bx + bw {
                if lab[y * w + x] == i as u32 {
                    let v = f64::from(gray[y * w + x]);
                    sum += v;
                    sq += v * v;
                    n += 1.0;
                }
            }
        }
        let mean = sum / n;
        if mean < 200.0 || (sq / n - mean * mean).max(0.0).sqrt() > 30.0 {
            continue;
        }
        // An outlined letter is a thin line around a wide body. The counter of a bold letter
        // (0, 6, O) is about as wide as its stroke, and stays as it is.
        let pad = 4.max((bh.min(bw) as f32 * 0.5) as usize);
        let (x0, y0, x1, y1) = (bx.saturating_sub(pad), by.saturating_sub(pad), (bx + bw + pad).min(w), (by + bh + pad).min(h));
        let (cw, ch) = (x1 - x0, y1 - y0);
        let mut hm = vec![false; cw * ch];
        let mut ink = vec![false; cw * ch];
        for y in 0..ch {
            for x in 0..cw {
                let (px, py) = (x0 + x, y0 + y);
                let p = py * w + px;
                hm[y * cw + x] = (bx..bx + bw).contains(&px) && (by..by + bh).contains(&py) && lab[p] == i as u32;
                ink[y * cw + x] = (gray[p] < 128 || saturated(p)) && mlab[p] == j;
            }
        }
        let hole_width = 2.0 * distance(&hm, cw, ch).into_iter().fold(0.0, f32::max);
        let band = dilate_square(&hm, cw, ch, 3);
        let dist = distance(&ink, cw, ch);
        let mut stroke_values: Vec<f32> = (0..cw * ch).filter(|p| band[*p] && ink[*p]).map(|p| dist[p]).collect();
        let stroke = if stroke_values.len() > 10 { 2.0 * percentile(&mut stroke_values, 90.0) } else { 1e9 };
        if hole_width < FILL_MIN_WIDTH * stroke {
            continue;
        }
        for y in by..by + bh {
            for x in bx..bx + bw {
                if lab[y * w + x] == i as u32 {
                    let p = &mut prob[y * w + x];
                    *p = p.max(0.75);
                }
            }
        }
    }
}

/// Joins components whose distance is small relative to the smaller one, so a word or an effect
/// is one region.
fn group(st: &[Stat], keep: &[usize], max_gap: f32) -> Vec<Vec<usize>> {
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    let size: Vec<f32> = keep
        .iter()
        .map(|&i| {
            let (w, h) = ((st[i].x1 - st[i].x0) as f32, (st[i].y1 - st[i].y0) as f32);
            // Long strokes count by their thickness.
            w.max(h).min(4.0 * w.min(h) + 6.0)
        })
        .collect();
    let mut parent: Vec<usize> = (0..keep.len()).collect();
    for a in 0..keep.len() {
        for b in a + 1..keep.len() {
            let (p, q) = (st[keep[a]], st[keep[b]]);
            let dx = (p.x0.max(q.x0) as f32 - p.x1.min(q.x1) as f32).max(0.0);
            let dy = (p.y0.max(q.y0) as f32 - p.y1.min(q.y1) as f32).max(0.0);
            let limit = (GAP * size[a].min(size[b])).min(max_gap);
            if (dx * dx + dy * dy).sqrt() <= limit {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                if ra != rb {
                    parent[ra] = rb;
                }
            }
        }
    }
    let mut out: Vec<(usize, Vec<usize>)> = Vec::new();
    for a in 0..keep.len() {
        let r = find(&mut parent, a);
        match out.iter_mut().find(|(k, _)| *k == r) {
            Some((_, v)) => v.push(keep[a]),
            None => out.push((r, vec![keep[a]])),
        }
    }
    out.into_iter().map(|(_, v)| v).collect()
}

/// The regions of a probability map, in manga reading order. A component needs one confident
/// pixel, and tiny regions are dropped.
pub fn find_regions(prob: &[f32], w: usize, h: usize) -> Detection {
    let set: Vec<bool> = prob.iter().map(|p| *p >= THRESHOLD).collect();
    let (lab, st) = components(&set, w, h, true);
    let mut core = vec![false; st.len()];
    for (l, p) in lab.iter().zip(prob) {
        if *p >= CONFIDENT {
            core[*l as usize] = true;
        }
    }
    let keep: Vec<usize> = (1..st.len()).filter(|&i| st[i].area >= MIN_AREA && core[i]).collect();
    struct Found {
        bbox: [u32; 4],
        parts: Vec<usize>,
    }
    let mut found: Vec<Found> = group(&st, &keep, MAX_GAP * h as f32)
        .into_iter()
        .filter_map(|g| {
            let area: u64 = g.iter().map(|&i| st[i].area).sum();
            if area < MIN_REGION_AREA {
                return None;
            }
            let bbox = [
                g.iter().map(|&i| st[i].x0).min()?,
                g.iter().map(|&i| st[i].y0).min()?,
                g.iter().map(|&i| st[i].x1).max()?,
                g.iter().map(|&i| st[i].y1).max()?,
            ];
            Some(Found { bbox, parts: g })
        })
        .collect();
    // Top to bottom in bands, right to left inside a band.
    let band = (h / 14).max(1) as i64;
    found.sort_by_key(|r| {
        let [x0, y0, x1, y1] = r.bbox.map(i64::from);
        (((y0 + y1) / 2) / band, (-(x0 + x1)).div_euclid(2))
    });
    // Pieces in their word's order, each word's left to right, top to bottom as found.
    let mut lut = vec![0u32; st.len()];
    let mut order: Vec<(usize, u32)> = Vec::new();
    for (k, r) in found.iter().enumerate() {
        for &c in &r.parts {
            order.push((c, k as u32 + 1));
            lut[c] = order.len() as u32;
        }
    }
    let labels: Vec<u32> = lab.iter().map(|l| lut[*l as usize]).collect();
    let mut sums = vec![0f64; order.len() + 1];
    for (l, p) in labels.iter().zip(prob) {
        sums[*l as usize] += f64::from(*p);
    }
    let regions = order
        .iter()
        .enumerate()
        .map(|(k, &(c, group))| {
            let s = st[c];
            Region { id: k as u32 + 1, group, bbox: [s.x0, s.y0, s.x1, s.y1], area: s.area, score: (sums[k + 1] / s.area as f64) as f32 }
        })
        .collect();
    Detection { width: w, height: h, labels, regions }
}

/// Adds an ellipse of radius `r` around the edge pixels of `set`.
fn dilate_ellipse(set: &[bool], w: usize, h: usize, r: usize, out: &mut [bool]) {
    let r = r as i64;
    let spans: Vec<(i64, i64)> = (-r..=r)
        .map(|dy| {
            let t = 1.0 - (dy * dy) as f64 / ((r * r) as f64 + 0.0001);
            (dy, (r as f64 * t.max(0.0).sqrt()).round() as i64)
        })
        .collect();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !set[i] {
                continue;
            }
            out[i] = true;
            // The middle of a shape reaches nothing its edge doesn't.
            if x > 0 && x + 1 < w && y > 0 && y + 1 < h && set[i - 1] && set[i + 1] && set[i - w] && set[i + w] {
                continue;
            }
            for &(dy, dx) in &spans {
                let yy = y as i64 + dy;
                if !(0..h as i64).contains(&yy) {
                    continue;
                }
                let (a, b) = ((x as i64 - dx).max(0) as usize, ((x as i64 + dx).min(w as i64 - 1)) as usize);
                out[yy as usize * w + a..=yy as usize * w + b].fill(true);
            }
        }
    }
}

/// Closes the small holes of `set`: a hole doesn't touch the page's edge.
fn fill_holes(set: &mut [bool], w: usize, h: usize) {
    let outside: Vec<bool> = set.iter().map(|v| !*v).collect();
    let (lab, st) = components(&outside, w, h, false);
    let limit = (w * h) as u64 * HOLE_PER_MILLE / 1000;
    let hole: Vec<bool> = st
        .iter()
        .map(|s| s.area > 0 && s.area <= limit && s.x0 > 0 && s.y0 > 0 && (s.x1 as usize) < w && (s.y1 as usize) < h)
        .collect();
    for (v, l) in set.iter_mut().zip(&lab) {
        if hole[*l as usize] {
            *v = true;
        }
    }
}

impl Detection {
    /// Its regions as numbers, to cross to another thread or worker: eight per region.
    pub fn flat_regions(&self) -> Vec<f64> {
        self.regions
            .iter()
            .flat_map(|r| [f64::from(r.id), f64::from(r.group), f64::from(r.bbox[0]), f64::from(r.bbox[1]), f64::from(r.bbox[2]), f64::from(r.bbox[3]), r.area as f64, f64::from(r.score)])
            .collect()
    }

    /// The other side of [`Detection::flat_regions`].
    pub fn from_flat(width: usize, height: usize, labels: Vec<u32>, regions: &[f64]) -> Detection {
        let regions = regions
            .chunks_exact(8)
            .map(|v| Region { id: v[0] as u32, group: v[1] as u32, bbox: [v[2] as u32, v[3] as u32, v[4] as u32, v[5] as u32], area: v[6] as u64, score: v[7] as f32 })
            .collect();
        Detection { width, height, labels, regions }
    }

    /// Only what's inside `zone` (a flag per pixel): pieces are cut at its edge, and what little
    /// is left of one goes.
    pub fn clipped(&self, zone: impl Fn(usize, usize) -> bool) -> Detection {
        let w = self.width.max(1);
        let mut stats: Vec<Stat> = vec![Stat { x0: u32::MAX, y0: u32::MAX, ..Stat::default() }; self.regions.len() + 1];
        let mut labels = self.labels.clone();
        for (i, l) in labels.iter_mut().enumerate() {
            if *l == 0 {
                continue;
            }
            let (x, y) = (i % w, i / w);
            if !zone(x, y) {
                *l = 0;
                continue;
            }
            let s = &mut stats[*l as usize];
            let (x, y) = (x as u32, y as u32);
            (s.x0, s.y0, s.x1, s.y1, s.area) = (s.x0.min(x), s.y0.min(y), s.x1.max(x + 1), s.y1.max(y + 1), s.area + 1);
        }
        let mut lut = vec![0u32; self.regions.len() + 1];
        let mut regions = Vec::new();
        for r in &self.regions {
            let s = stats[r.id as usize];
            if s.area >= MIN_AREA {
                let id = regions.len() as u32 + 1;
                lut[r.id as usize] = id;
                regions.push(Region { id, bbox: [s.x0, s.y0, s.x1, s.y1], area: s.area, ..r.clone() });
            }
        }
        for l in &mut labels {
            *l = lut[*l as usize];
        }
        Detection { width: self.width, height: self.height, labels, regions }
    }
}

/// How far a selection reaches past the letters by default. Less leaves the letters' dark edge and
/// halo outside, and cleaning copies them back as smudges. In proportion to the page: 10 px on a
/// page 1800 px tall.
pub fn default_grow(width: usize, height: usize) -> usize {
    (width.max(height) / 180).clamp(4, MAX_GROW)
}

/// The pixels of the regions `ids`, grown outward by `grow`, with their small holes closed.
pub fn selection_mask(d: &Detection, ids: &[u32], grow: usize) -> Vec<bool> {
    let (w, h) = (d.width, d.height);
    let mut wanted = vec![false; d.regions.len() + 1];
    for id in ids {
        if let Some(v) = wanted.get_mut(*id as usize) {
            *v = true;
        }
    }
    let set: Vec<bool> = d.labels.iter().map(|l| *l != 0 && wanted[*l as usize]).collect();
    if grow == 0 {
        return set;
    }
    let mut out = vec![false; w * h];
    dilate_ellipse(&set, w, h, grow.min(MAX_GROW), &mut out);
    fill_holes(&mut out, w, h);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn blob(prob: &mut [f32], w: usize, x0: usize, y0: usize, x1: usize, y1: usize, v: f32) {
        for y in y0..y1 {
            for x in x0..x1 {
                prob[y * w + x] = v;
            }
        }
    }

    #[test]
    fn regions_are_in_reading_order_and_scored() {
        let (w, h) = (200, 140);
        let mut p = vec![0.0; w * h];
        blob(&mut p, w, 150, 5, 180, 15, 0.995); // Top right: first.
        blob(&mut p, w, 10, 5, 40, 15, 0.995); // Top left: second.
        blob(&mut p, w, 10, 100, 40, 120, 0.995); // Below: third.
        let d = find_regions(&p, w, h);
        let boxes: Vec<[u32; 4]> = d.regions.iter().map(|r| r.bbox).collect();
        assert_eq!(boxes, vec![[150, 5, 180, 15], [10, 5, 40, 15], [10, 100, 40, 120]]);
        assert!((d.regions[0].score - 0.995).abs() < 1e-3);
        assert_eq!(d.labels[10 * w + 160], 1);
    }

    #[test]
    fn a_component_with_no_confident_pixel_or_tiny_is_dropped() {
        let (w, h) = (100, 100);
        let mut p = vec![0.0; w * h];
        blob(&mut p, w, 10, 10, 40, 40, 0.6); // Never confident.
        blob(&mut p, w, 60, 60, 62, 62, 0.995); // Tiny.
        assert!(find_regions(&p, w, h).regions.is_empty());
    }

    #[test]
    fn near_components_join_into_one_region() {
        let (w, h) = (300, 200);
        let mut p = vec![0.0; w * h];
        blob(&mut p, w, 10, 10, 30, 30, 0.995);
        blob(&mut p, w, 35, 10, 55, 30, 0.995); // 5 px apart: one word.
        blob(&mut p, w, 200, 10, 220, 30, 0.995); // Far: its own.
        let d = find_regions(&p, w, h);
        assert_eq!(d.regions.len(), 3, "each piece on its own");
        let group = |x: usize| d.regions[d.labels[20 * w + x] as usize - 1].group;
        assert_eq!(group(20), group(40), "one word");
        assert_ne!(group(20), group(210), "another");
    }

    #[test]
    fn the_flat_white_body_of_an_outlined_letter_is_filled() {
        let (w, h) = (200, 200);
        let mut rgb = vec![255u8; w * h * 3];
        let mut p = vec![0.0; w * h];
        // A thin dark outline around a wide white body, with a notch: like the script, a hole only
        // sees its ring inside its own box, and a perfect square has none.
        for y in 40..160 {
            for x in 40..160 {
                if !(52..148).contains(&x) || !(52..148).contains(&y) || (x < 60 && y < 60) {
                    p[y * w + x] = 0.995;
                    rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(0);
                }
            }
        }
        fill_hollow(&mut p, &rgb, w, h);
        assert!(p[100 * w + 100] >= 0.75, "the body counts as text");
    }

    #[test]
    fn a_dark_hole_stays_a_hole() {
        let (w, h) = (200, 200);
        let rgb = vec![40u8; w * h * 3];
        let mut p = vec![0.0; w * h];
        blob(&mut p, w, 40, 40, 160, 160, 0.995);
        blob(&mut p, w, 60, 60, 140, 140, 0.0);
        fill_hollow(&mut p, &rgb, w, h);
        assert_eq!(p[100 * w + 100], 0.0);
    }

    #[test]
    fn hatching_starts_unchosen_and_letters_and_dots_start_chosen() {
        let r = |score, area| Region { id: 1, group: 1, bbox: [0; 4], area, score };
        assert!(r(0.93, 5000).suggested(), "a word");
        assert!(r(0.87, 5850).suggested(), "a big word the model doubts");
        assert!(r(0.77, 48).suggested(), "a dot");
        assert!(!r(0.87, 1073).suggested(), "hatching");
        assert!(!r(0.64, 63).suggested(), "too doubtful at any size");
    }

    #[test]
    fn a_zone_cuts_the_pieces_at_its_edge_and_numbers_what_is_left() {
        let (w, h) = (300, 100);
        let mut p = vec![0.0; w * h];
        blob(&mut p, w, 10, 10, 50, 30, 0.995);
        blob(&mut p, w, 100, 10, 140, 30, 0.995);
        blob(&mut p, w, 200, 10, 240, 30, 0.995);
        let d = find_regions(&p, w, h);
        // The middle one whole, the last one cut in half, the first one out.
        let c = d.clipped(|x, _| (90..220).contains(&x));
        assert_eq!(c.regions.len(), 2);
        assert_eq!(c.regions.iter().map(|r| r.id).collect::<Vec<_>>(), vec![1, 2]);
        let cut = c.regions.iter().find(|r| r.bbox[0] == 200).unwrap();
        assert_eq!((cut.bbox, cut.area), ([200, 10, 220, 30], 400));
        assert_eq!(c.labels[20 * w + 20], 0);
        assert_eq!(c.labels[20 * w + 230], 0);
        assert_eq!(c.labels[20 * w + 210], cut.id);
        assert!(d.clipped(|x, _| x == 100).regions.len() == 1, "a sliver of 20 px still counts");
        assert!(d.clipped(|x, y| x == 100 && y < 15).regions.is_empty(), "less than that doesn't");
    }

    #[test]
    fn a_detection_crosses_as_numbers() {
        let (w, h) = (100, 50);
        let mut p = vec![0.0; w * h];
        blob(&mut p, w, 10, 10, 40, 30, 0.995);
        let d = find_regions(&p, w, h);
        assert_eq!(Detection::from_flat(w, h, d.labels.clone(), &d.flat_regions()), d);
    }

    #[test]
    fn the_default_contour_follows_the_page() {
        assert_eq!(default_grow(1284, 1826), 10);
        assert_eq!(default_grow(400, 300), 4);
        assert_eq!(default_grow(4000, 5400), 30);
    }

    #[test]
    fn a_small_hole_closes_and_a_big_one_stays() {
        let (w, h) = (400, 400);
        let ring = |side: usize| {
            let mut p = vec![0.0; w * h];
            blob(&mut p, w, 100, 100, 130 + side, 130 + side, 0.995);
            blob(&mut p, w, 115, 115, 115 + side, 115 + side, 0.0);
            let d = find_regions(&p, w, h);
            selection_mask(&d, &[1], 1)[(115 + side / 2) * w + 115 + side / 2]
        };
        assert!(ring(20), "a leftover inside a letter is cleaned with it");
        assert!(!ring(60), "what a big shape goes around is the page's");
    }

    #[test]
    fn a_selection_is_only_the_chosen_regions_grown() {
        let (w, h) = (300, 200);
        let mut p = vec![0.0; w * h];
        blob(&mut p, w, 200, 50, 240, 90, 0.995);
        blob(&mut p, w, 20, 50, 60, 90, 0.995);
        let d = find_regions(&p, w, h);
        let id = d.labels[70 * w + 30];
        let m = selection_mask(&d, &[id], 0);
        assert!(m[70 * w + 30] && !m[70 * w + 220]);
        let g = selection_mask(&d, &[id], 3);
        assert!(g[70 * w + 62] && !g[70 * w + 65], "grown 3 px");
        assert!(selection_mask(&d, &[99], 2).iter().all(|v| !*v), "an unknown id is nothing");
    }
}
