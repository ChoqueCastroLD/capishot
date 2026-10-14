//! Page-wide speech-bubble detection (D7) and the double-bubble neck split (D6).
//!
//! Ported from TypeR `app_src/bubbleDetection.js` (branch `feature/auto-bubble-detection`
//! f01c0d3), MIT License, Copyright (c) 2017 Swirt; see `crates/typer/LICENSE-TypeR.txt`. This is
//! TypeR's "learned" pipeline with its prior weights (the user training, `trainBubbleLearning`, is
//! not ported):
//!
//! 1. The page, downscaled to at most 1500 px (whole-pixel box average).
//! 2. White mask: every channel ≥ `250 − 5·sensitivity`.
//! 3. 4-connected white components; those touching the page border are gutters and margins.
//! 4. Candidates: area in `[max(20, 0.00012·A), 0.12·A]`, box ≥ 8 px, aspect ≤ 12, fill ≥ 0.2.
//! 5. Six features (fill, runs per line, dark outline ratio, area, short side, aspect), a logistic
//!    model with weights `[0.8, 2, 2.5, 0.2, 0.25, 0.15]` and bias −1.8, accepted when
//!    `p ≥ 0.78 − 0.055·(sensitivity − 1)`.
//! 6. Neck split: holes filled, chamfer 3-4 distance, peaks, the saddle between two peaks; a
//!    second logistic model (`[2.2, 1.2, 1, 0.6]`, −0.4, ≥ 0.62) turns one component into two.
//! 7. Reading order: rows of bubbles overlapping by more than half the smaller height, top to
//!    bottom; right to left inside a row for manga.
//!
//! Capishot counts runs per line with the component's holes filled, so text left in a bubble (a
//! raw page) doesn't read as jagged art, and adds the bubble body (tail removed by TypeR 3.0's adaptive opening) and outline rows
//! for each bubble, so a detected bubble can be pasted into and shaped like a wand click.

use serde::{Deserialize, Serialize};

use crate::bubble::{self, Mask, Rect, Row};

/// Longest side of the image the detector works on.
pub const MAX_SIDE: usize = 1500;
/// Largest page accepted (pixels before downscaling).
pub const MAX_PIXELS: usize = 1 << 28;
/// Default sensitivity (1–9).
pub const DEFAULT_SENSITIVITY: u8 = 5;

const MAX_AREA_RATIO: f64 = 0.12;
const CANDIDATE_MIN_AREA_RATIO: f64 = 0.00012;
const CANDIDATE_MIN_FILL: f64 = 0.2;
const CANDIDATE_MIN_SIZE: usize = 8;
const CANDIDATE_MAX_ASPECT: f64 = 12.0;
const OUTLINE_DARK_LUMA: f64 = 170.0;
const OUTLINE_PROBE_DEPTH: usize = 3;
/// TypeR's prior bubble model.
pub const WEIGHTS: [f64; 6] = [0.8, 2.0, 2.5, 0.2, 0.25, 0.15];
pub const BIAS: f64 = -1.8;
/// TypeR's prior neck-split model.
pub const SPLIT_WEIGHTS: [f64; 4] = [2.2, 1.2, 1.0, 0.6];
pub const SPLIT_BIAS: f64 = -0.4;
pub const SPLIT_THRESHOLD: f64 = 0.62;

/// An RGBA page (straight alpha ignored: the page is opaque).
#[derive(Clone, Copy, Debug)]
pub struct Page<'a> {
    pub px: &'a [[u8; 4]],
    pub width: usize,
    pub height: usize,
}

/// Detection settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetectOptions {
    /// 1 (strict) to 9 (accepts greyer whites and weaker bubbles).
    pub sensitivity: u8,
    /// Manga reading order: right to left inside a row.
    pub rtl: bool,
    /// Split joined double bubbles.
    pub splits: bool,
    /// Rows sampled per bubble outline (5–31).
    pub rows: usize,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self { sensitivity: DEFAULT_SENSITIVITY, rtl: true, splits: true, rows: bubble::DEFAULT_ROWS }
    }
}

impl DetectOptions {
    fn white_threshold(&self) -> u8 {
        250u8.saturating_sub(self.sensitivity.clamp(1, 9) * 5)
    }

    fn confidence_threshold(&self) -> f64 {
        0.78 - f64::from(self.sensitivity.clamp(1, 9) - 1) * 0.055
    }
}

/// A component's box in detector pixels (`right`, `bottom` exclusive).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct IBox {
    pub left: usize,
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
}

impl IBox {
    pub fn width(&self) -> usize {
        self.right.saturating_sub(self.left)
    }

    pub fn height(&self) -> usize {
        self.bottom.saturating_sub(self.top)
    }
}

/// A white component that may be a bubble.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub bounds: IBox,
    pub area: usize,
    pub fill: f64,
    pub runs_per_line: f64,
    pub outline_dark: f64,
    pub features: [f64; 6],
    pub confidence: f64,
    /// The component's pixels over `bounds` (row-major).
    pub local: Vec<bool>,
    pub split: Option<NeckSplit>,
}

/// One lobe of a split.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Lobe {
    pub min_x: usize,
    pub min_y: usize,
    pub max_x: usize,
    pub max_y: usize,
    pub area: usize,
}

/// Two lobes joined by a narrow neck (coordinates local to the component's box).
#[derive(Clone, Debug, PartialEq)]
pub struct NeckSplit {
    /// The two distance peaks (x, y, radius in px).
    pub peaks: [(usize, usize, f64); 2],
    /// A point on the split line; the line is perpendicular to the peak-to-peak direction.
    pub at: (f64, f64),
    pub dir: (f64, f64),
    pub saddle_ratio: f64,
    pub peak_ratio: f64,
    pub area_balance: f64,
    pub separation_ratio: f64,
    pub lobes: [Lobe; 2],
    pub score: f64,
}

impl NeckSplit {
    /// TypeR's split features, each clamped to ±1.
    pub fn features(&self) -> [f64; 4] {
        [
            ((0.8 - self.saddle_ratio) / 0.5).clamp(-1.0, 1.0),
            ((self.peak_ratio - 0.42) / 0.5).clamp(-1.0, 1.0),
            ((self.area_balance - 0.2) / 0.65).clamp(-1.0, 1.0),
            ((self.separation_ratio - 0.8) / 1.4).clamp(-1.0, 1.0),
        ]
    }

    /// The prior model's confidence that this is two bubbles.
    pub fn confidence(&self) -> f64 {
        let f = self.features();
        sigmoid(SPLIT_BIAS + SPLIT_WEIGHTS.iter().zip(f).map(|(w, x)| w * x).sum::<f64>())
    }

    /// Which lobe the local pixel (x, y) belongs to (0 or 1).
    pub fn side(&self, x: usize, y: usize) -> usize {
        let proj = self.at.0 * self.dir.0 + self.at.1 * self.dir.1;
        usize::from(x as f64 * self.dir.0 + y as f64 * self.dir.1 > proj)
    }
}

/// A detected bubble in page pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Detected {
    /// The white region's box.
    pub bbox: Rect,
    /// The bubble body's box (tail removed): where text is centred.
    pub opened: Rect,
    pub center: (f64, f64),
    /// Outline rows of the body, normalized to `opened`.
    pub rows: Vec<Row>,
    pub confidence: f64,
    /// Area in page pixels.
    pub area: f64,
    /// One lobe of a split double bubble.
    pub split: bool,
}

fn sigmoid(v: f64) -> f64 {
    1.0 / (1.0 + (-v.clamp(-20.0, 20.0)).exp())
}

fn luma(p: [u8; 4]) -> f64 {
    f64::from(p[0]) * 0.299 + f64::from(p[1]) * 0.587 + f64::from(p[2]) * 0.114
}

/// The page box-averaged by a whole factor so its longest side is at most [`MAX_SIDE`]; the factor
/// is returned with the image.
pub fn downscale(page: &Page) -> Option<(Vec<[u8; 4]>, usize, usize, usize)> {
    let (w, h) = (page.width, page.height);
    if w == 0 || h == 0 || w.checked_mul(h)? > MAX_PIXELS || page.px.len() < w * h {
        return None;
    }
    let f = w.max(h).div_ceil(MAX_SIDE).max(1);
    if f == 1 {
        return Some((page.px.get(..w * h)?.to_vec(), w, h, 1));
    }
    let (sw, sh) = (w.div_ceil(f), h.div_ceil(f));
    let mut acc = vec![[0u32; 4]; sw * sh];
    let mut count = vec![0u32; sw * sh];
    for y in 0..h {
        let row = (y / f) * sw;
        for x in 0..w {
            let i = row + x / f;
            let (Some(a), Some(c), Some(p)) = (acc.get_mut(i), count.get_mut(i), page.px.get(y * w + x)) else { continue };
            for k in 0..4 {
                a[k] += u32::from(p[k]);
            }
            *c += 1;
        }
    }
    let out = acc.iter().zip(&count).map(|(a, c)| if *c == 0 { [0, 0, 0, 255] } else { a.map(|v| (v / c) as u8) }).collect();
    Some((out, sw, sh, f))
}

fn feature_vector(c: &Candidate, width: usize, height: usize) -> [f64; 6] {
    let total = (width * height).max(1) as f64;
    let min_page = width.min(height).max(1) as f64;
    let area_ratio = (1.0 / total).max(c.area as f64 / total);
    let (bw, bh) = (c.bounds.width() as f64, c.bounds.height() as f64);
    let aspect = (bw / bh.max(1.0)).max(bh / bw.max(1.0));
    [
        ((c.fill - 0.45) / 0.35).clamp(-1.0, 1.0),
        ((1.8 - c.runs_per_line) / 0.8).clamp(-1.0, 1.0),
        ((c.outline_dark - 0.55) / 0.35).clamp(-1.0, 1.0),
        ((area_ratio / 0.0005).ln() / 12f64.ln()).clamp(-1.0, 1.0),
        ((bw.min(bh) / min_page - 0.012) / 0.035).clamp(-1.0, 1.0),
        ((3.0 - aspect) / 2.0).clamp(-1.0, 1.0),
    ]
}

/// The prior model's confidence for a feature vector.
pub fn confidence(features: &[f64; 6]) -> f64 {
    sigmoid(BIAS + WEIGHTS.iter().zip(features).map(|(w, x)| w * x).sum::<f64>())
}

/// White components that may be bubbles (TypeR's permissive candidate pool), with their shape
/// analysis and, when `splits`, a neck split suggestion.
pub fn candidates(img: &[[u8; 4]], width: usize, height: usize, opts: &DetectOptions) -> Vec<Candidate> {
    let total = width * height;
    if total == 0 || img.len() < total {
        return Vec::new();
    }
    let t = opts.white_threshold();
    let mask: Vec<bool> = img.iter().take(total).map(|p| p[0] >= t && p[1] >= t && p[2] >= t).collect();
    let mut visited = vec![false; total];
    let min_area = 20usize.max((total as f64 * CANDIDATE_MIN_AREA_RATIO).floor() as usize);
    let max_area = (total as f64 * MAX_AREA_RATIO).ceil() as usize;
    let mut stack: Vec<usize> = Vec::new();
    let mut out = Vec::new();
    for start in 0..total {
        if !mask[start] || visited[start] {
            continue;
        }
        let (mut area, mut min_x, mut max_x, mut min_y, mut max_y, mut border) = (0usize, width, 0usize, height, 0usize, false);
        visited[start] = true;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % width, i / width);
            area += 1;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            if x == 0 || y == 0 || x == width - 1 || y == height - 1 {
                border = true;
            }
            let mut push = |j: usize| {
                if mask[j] && !visited[j] {
                    visited[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                push(i - 1);
            }
            if x + 1 < width {
                push(i + 1);
            }
            if y > 0 {
                push(i - width);
            }
            if y + 1 < height {
                push(i + width);
            }
        }
        if border || area < min_area || area > max_area {
            continue;
        }
        let (bw, bh) = (max_x - min_x + 1, max_y - min_y + 1);
        if bw < CANDIDATE_MIN_SIZE || bh < CANDIDATE_MIN_SIZE {
            continue;
        }
        let aspect = if bw > bh { bw as f64 / bh as f64 } else { bh as f64 / bw as f64 };
        if aspect > CANDIDATE_MAX_ASPECT {
            continue;
        }
        let fill = area as f64 / (bw * bh) as f64;
        if fill < CANDIDATE_MIN_FILL {
            continue;
        }
        let bounds = IBox { left: min_x, top: min_y, right: max_x + 1, bottom: max_y + 1 };
        let mut c = analyze(&mask, img, width, height, bounds, start, area, fill, opts.splits);
        c.features = feature_vector(&c, width, height);
        c.confidence = confidence(&c.features);
        out.push(c);
    }
    out
}

/// Shape and outline analysis of one component: its own pixels (a flood from its seed inside its
/// box), runs per row/column, the dark-outline ratio and the neck split.
#[allow(clippy::too_many_arguments)]
fn analyze(mask: &[bool], img: &[[u8; 4]], width: usize, height: usize, b: IBox, seed: usize, area: usize, fill: f64, splits: bool) -> Candidate {
    let (bw, bh) = (b.width(), b.height());
    let mut local = vec![false; bw * bh];
    let li = |i: usize| (i / width - b.top) * bw + (i % width - b.left);
    let mut stack = vec![seed];
    if let Some(p) = local.get_mut(li(seed)) {
        *p = true;
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % width, i / width);
        let mut n: [Option<usize>; 4] = [None; 4];
        if x > b.left {
            n[0] = Some(i - 1);
        }
        if x + 1 < b.right {
            n[1] = Some(i + 1);
        }
        if y > b.top {
            n[2] = Some(i - width);
        }
        if y + 1 < b.bottom {
            n[3] = Some(i + width);
        }
        for j in n.into_iter().flatten() {
            if !mask.get(j).copied().unwrap_or(false) {
                continue;
            }
            if let Some(p) = local.get_mut(li(j))
                && !*p
            {
                *p = true;
                stack.push(j);
            }
        }
    }
    let runs = |len_outer: usize, len_inner: usize, at: &dyn Fn(usize, usize) -> bool| -> f64 {
        let (mut total, mut occupied) = (0usize, 0usize);
        for o in 0..len_outer {
            let (mut r, mut inrun) = (0, false);
            for i in 0..len_inner {
                let f = at(o, i);
                if f && !inrun {
                    r += 1;
                }
                inrun = f;
            }
            if r > 0 {
                total += r;
                occupied += 1;
            }
        }
        if occupied > 0 { total as f64 / occupied as f64 } else { 0.0 }
    };
    let get = |x: usize, y: usize| local.get(y * bw + x).copied().unwrap_or(false);
    // Runs are counted with holes filled (Capishot): text left inside a bubble is not jagged art.
    let sil = fill_holes(&local, bw, bh);
    let filled = |x: usize, y: usize| sil.get(y * bw + x).copied().unwrap_or(false);
    let rows = runs(bh, bw, &|y, x| filled(x, y));
    let cols = runs(bw, bh, &|x, y| filled(x, y));
    let (mut samples, mut sealed) = (0usize, 0usize);
    for y in 0..bh {
        for x in 0..bw {
            if !get(x, y) {
                continue;
            }
            let (gx, gy) = ((x + b.left) as i64, (y + b.top) as i64);
            for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
                let (nx, ny) = (gx + dx, gy + dy);
                let in_box = nx >= b.left as i64 && nx < b.right as i64 && ny >= b.top as i64 && ny < b.bottom as i64;
                if in_box && get((nx - b.left as i64) as usize, (ny - b.top as i64) as usize) {
                    continue;
                }
                samples += 1;
                for depth in 1..=OUTLINE_PROBE_DEPTH as i64 {
                    let (px, py) = (gx + dx * depth, gy + dy * depth);
                    if px < 0 || py < 0 || px >= width as i64 || py >= height as i64 {
                        break;
                    }
                    if img.get(py as usize * width + px as usize).is_some_and(|p| luma(*p) < OUTLINE_DARK_LUMA) {
                        sealed += 1;
                        break;
                    }
                }
            }
        }
    }
    let split = if splits { neck_split(&local, bw, bh) } else { None };
    Candidate {
        bounds: b,
        area,
        fill,
        runs_per_line: rows.max(cols),
        outline_dark: if samples > 0 { sealed as f64 / samples as f64 } else { 0.0 },
        features: [0.0; 6],
        confidence: 0.0,
        local,
        split,
    }
}

/// `local` with every hole (background not reachable from the box edge) filled.
pub fn fill_holes(local: &[bool], width: usize, height: usize) -> Vec<bool> {
    let n = width * height;
    let mut exterior = vec![false; n];
    let mut stack = Vec::new();
    let push = |i: usize, ext: &mut Vec<bool>, st: &mut Vec<usize>| {
        if i < n && !local.get(i).copied().unwrap_or(true) && !ext[i] {
            ext[i] = true;
            st.push(i);
        }
    };
    for x in 0..width {
        push(x, &mut exterior, &mut stack);
        push((height.saturating_sub(1)) * width + x, &mut exterior, &mut stack);
    }
    for y in 0..height {
        push(y * width, &mut exterior, &mut stack);
        push(y * width + width.saturating_sub(1), &mut exterior, &mut stack);
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % width, i / width);
        if x > 0 {
            push(i - 1, &mut exterior, &mut stack);
        }
        if x + 1 < width {
            push(i + 1, &mut exterior, &mut stack);
        }
        if y > 0 {
            push(i - width, &mut exterior, &mut stack);
        }
        if y + 1 < height {
            push(i + width, &mut exterior, &mut stack);
        }
    }
    (0..n).map(|i| local.get(i).copied().unwrap_or(false) || !exterior[i]).collect()
}

/// Chamfer 3-4 distance to the outside (units of 1/3 px; edge pixels start at 3).
fn chamfer(sil: &[bool], width: usize, height: usize) -> Vec<u16> {
    const FAR: u16 = 30000;
    let mut d: Vec<u16> = sil
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let (x, y) = (i % width, i / width);
            if !s {
                0
            } else if x == 0 || y == 0 || x + 1 == width || y + 1 == height {
                3
            } else {
                FAR
            }
        })
        .collect();
    let at = |d: &[u16], i: usize| d.get(i).copied().unwrap_or(0);
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            let mut v = at(&d, i);
            if v == 0 {
                continue;
            }
            if x > 0 {
                v = v.min(at(&d, i - 1).saturating_add(3));
            }
            if y > 0 {
                v = v.min(at(&d, i - width).saturating_add(3));
                if x > 0 {
                    v = v.min(at(&d, i - width - 1).saturating_add(4));
                }
                if x + 1 < width {
                    v = v.min(at(&d, i - width + 1).saturating_add(4));
                }
            }
            if let Some(p) = d.get_mut(i) {
                *p = v;
            }
        }
    }
    for y in (0..height).rev() {
        for x in (0..width).rev() {
            let i = y * width + x;
            let mut v = at(&d, i);
            if v == 0 {
                continue;
            }
            if x + 1 < width {
                v = v.min(at(&d, i + 1).saturating_add(3));
            }
            if y + 1 < height {
                v = v.min(at(&d, i + width).saturating_add(3));
                if x + 1 < width {
                    v = v.min(at(&d, i + width + 1).saturating_add(4));
                }
                if x > 0 {
                    v = v.min(at(&d, i + width - 1).saturating_add(4));
                }
            }
            if let Some(p) = d.get_mut(i) {
                *p = v;
            }
        }
    }
    d
}

/// Two substantial lobes separated by a narrow saddle in the component's distance map (holes
/// filled first so text can't make fake lobes). The split line can have any direction.
pub fn neck_split(local: &[bool], width: usize, height: usize) -> Option<NeckSplit> {
    if width < 18 || height < 18 || local.len() < width * height {
        return None;
    }
    let sil = fill_holes(local, width, height);
    let d = chamfer(&sil, width, height);
    let min_peak = 12f64.max(width.min(height) as f64 * 0.09 * 3.0);
    let mut cands: Vec<(usize, usize, f64)> = Vec::new();
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let v = d[y * width + x];
            if f64::from(v) < min_peak {
                continue;
            }
            let (mut is_max, mut lower) = (true, false);
            for dy in [-1i64, 0, 1] {
                for dx in [-1i64, 0, 1] {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let n = d[((y as i64 + dy) as usize) * width + (x as i64 + dx) as usize];
                    if n > v {
                        is_max = false;
                    }
                    if n < v {
                        lower = true;
                    }
                }
            }
            if is_max && lower {
                cands.push((x, y, f64::from(v) / 3.0));
            }
        }
    }
    // Stable sort by radius, descending (JS sort on the same keys).
    cands.sort_by(|a, b| b.2.total_cmp(&a.2));
    let mut peaks: Vec<(usize, usize, f64)> = Vec::new();
    for c in cands {
        if peaks.len() >= 8 {
            break;
        }
        let separated = peaks.iter().all(|p| {
            let (dx, dy) = (p.0 as f64 - c.0 as f64, p.1 as f64 - c.1 as f64);
            (dx * dx + dy * dy).sqrt() >= p.2.max(c.2) * 0.72
        });
        if separated {
            peaks.push(c);
        }
    }
    if peaks.len() < 2 {
        return None;
    }
    let mut best: Option<NeckSplit> = None;
    for (fi, first) in peaks.iter().enumerate() {
        for second in peaks.iter().skip(fi + 1) {
            let (dx, dy) = (second.0 as f64 - first.0 as f64, second.1 as f64 - first.1 as f64);
            let dist = (dx * dx + dy * dy).sqrt();
            let peak_ratio = first.2.min(second.2) / first.2.max(second.2);
            if peak_ratio < 0.42 || dist < first.2.max(second.2) * 0.9 {
                continue;
            }
            let (mut saddle, mut saddle_step) = (f64::INFINITY, 20.0);
            for step in 7..=33 {
                let s = f64::from(step);
                let x = (first.0 as f64 + dx * s / 40.0).round();
                let y = (first.1 as f64 + dy * s / 40.0).round();
                if x < 0.0 || y < 0.0 {
                    continue;
                }
                let v = f64::from(d.get(y as usize * width + x as usize).copied().unwrap_or(0)) / 3.0;
                if v < saddle {
                    saddle = v;
                    saddle_step = s;
                }
            }
            let smaller = first.2.min(second.2);
            let saddle_ratio = saddle / smaller.max(1.0);
            if saddle_ratio >= 0.86 {
                continue;
            }
            let at = (first.0 as f64 + dx * saddle_step / 40.0, first.1 as f64 + dy * saddle_step / 40.0);
            let proj = at.0 * dx + at.1 * dy;
            let mut lobes = [Lobe { min_x: width, min_y: height, ..Default::default() }, Lobe { min_x: width, min_y: height, ..Default::default() }];
            let mut total = 0usize;
            for y in 0..height {
                for x in 0..width {
                    if !sil[y * width + x] {
                        continue;
                    }
                    let l = &mut lobes[usize::from(x as f64 * dx + y as f64 * dy > proj)];
                    l.area += 1;
                    total += 1;
                    l.min_x = l.min_x.min(x);
                    l.max_x = l.max_x.max(x);
                    l.min_y = l.min_y.min(y);
                    l.max_y = l.max_y.max(y);
                }
            }
            if lobes[0].area == 0 || lobes[1].area == 0 {
                continue;
            }
            let (a0, a1) = (lobes[0].area as f64, lobes[1].area as f64);
            let balance = a0.min(a1) / a0.max(a1);
            if balance < 0.2 || a0.min(a1) < total as f64 * 0.16 {
                continue;
            }
            let separation = dist / first.2.max(second.2).max(1.0);
            let score = (1.0 - saddle_ratio) * 1.8 + peak_ratio + balance * 0.8 + separation.min(2.0) * 0.2;
            if best.as_ref().is_none_or(|b| score > b.score) {
                best = Some(NeckSplit {
                    peaks: [*first, *second],
                    at,
                    dir: (dx, dy),
                    saddle_ratio,
                    peak_ratio,
                    area_balance: balance,
                    separation_ratio: separation,
                    lobes,
                    score,
                });
            }
        }
    }
    best
}

/// Reading order: rows of bubbles that overlap by more than half the smaller height, top to
/// bottom; inside a row right to left (`rtl`) or left to right. Returns indices into `boxes`.
pub fn reading_order(boxes: &[Rect], rtl: bool) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..boxes.len()).collect();
    idx.sort_by(|a, b| boxes[*a].y.total_cmp(&boxes[*b].y));
    struct RowSpan {
        top: f64,
        bottom: f64,
        items: Vec<usize>,
    }
    let mut rows: Vec<RowSpan> = Vec::new();
    for i in idx {
        let b = &boxes[i];
        let (top, bottom) = (b.y, b.y + b.height);
        let target = rows.iter_mut().find(|r| {
            let overlap = r.bottom.min(bottom) - r.top.max(top);
            overlap > (r.bottom - r.top).min(b.height) * 0.5
        });
        match target {
            Some(r) => {
                r.top = r.top.min(top);
                r.bottom = r.bottom.max(bottom);
                r.items.push(i);
            }
            None => rows.push(RowSpan { top, bottom, items: vec![i] }),
        }
    }
    rows.sort_by(|a, b| a.top.total_cmp(&b.top));
    let mut out = Vec::with_capacity(boxes.len());
    for mut r in rows {
        let mid = |i: &usize| boxes[*i].x + boxes[*i].width / 2.0;
        r.items.sort_by(|a, b| if rtl { mid(b).total_cmp(&mid(a)) } else { mid(a).total_cmp(&mid(b)) });
        out.extend(r.items);
    }
    out
}

/// The body box, centre and rows of a lobe or component mask (detector pixels), mapped to page
/// pixels by `scale` and `origin`.
fn describe(mask: Mask, scale: f64, origin: (f64, f64), rows_wanted: usize, confidence: f64, split: bool) -> Option<Detected> {
    let (o, rows) = bubble::body_rows(&mask, rows_wanted)?;
    let map = |r: Rect| Rect::new(origin.0 + r.x * scale, origin.1 + r.y * scale, r.width * scale, r.height * scale);
    let opened = map(o.opened);
    Some(Detected { bbox: map(o.bbox), opened, center: opened.center(), rows, confidence, area: o.area as f64 * scale * scale, split })
}

/// Every speech bubble on `page`, in reading order. `origin` is the page's top-left in document
/// pixels.
pub fn detect(page: &Page, origin: (f64, f64), opts: &DetectOptions) -> Vec<Detected> {
    let Some((img, w, h, f)) = downscale(page) else { return Vec::new() };
    let scale = f as f64;
    let threshold = opts.confidence_threshold();
    let mut found: Vec<Detected> = Vec::new();
    for c in candidates(&img, w, h, opts) {
        if c.confidence < threshold {
            continue;
        }
        let (bw, bh) = (c.bounds.width(), c.bounds.height());
        let x0 = c.bounds.left as i32;
        let y0 = c.bounds.top as i32;
        if let Some(s) = c.split.as_ref().filter(|s| s.confidence() >= SPLIT_THRESHOLD) {
            let sil = fill_holes(&c.local, bw, bh);
            let mut ok = Vec::new();
            for lobe in 0..2 {
                let Some(mut m) = Mask::new(x0, y0, bw, bh) else { continue };
                for (i, p) in sil.iter().enumerate() {
                    if *p && s.side(i % bw, i / bw) == lobe {
                        m.set(x0 + (i % bw) as i32, y0 + (i / bw) as i32, true);
                    }
                }
                if let Some(d) = describe(m, scale, origin, opts.rows, c.confidence, true) {
                    ok.push(d);
                }
            }
            if ok.len() == 2 {
                found.extend(ok);
                continue;
            }
        }
        let Some(mut m) = Mask::new(x0, y0, bw, bh) else { continue };
        for (i, p) in c.local.iter().enumerate() {
            if *p {
                m.set(x0 + (i % bw) as i32, y0 + (i / bw) as i32, true);
            }
        }
        if let Some(d) = describe(m, scale, origin, opts.rows, c.confidence, false) {
            found.push(d);
        }
    }
    let boxes: Vec<Rect> = found.iter().map(|d| d.bbox).collect();
    let order = reading_order(&boxes, opts.rtl);
    order.into_iter().filter_map(|i| found.get(i).cloned()).collect()
}

#[cfg(test)]
#[path = "pagedetect_tests.rs"]
mod tests;
