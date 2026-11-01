//! `edit.removeBubbleText`: the Bubble Text Remover tool's click. The text in the speech bubble
//! under a point is removed, without AI: the bubble's interior is found as the Bubble Type tool
//! finds it (`photocraft_typer::bubble_find`: outline gaps closed, a click on a letter still finds
//! it), its background modelled, and only the pixels that differ from it (the lettering, with its
//! anti-aliased edge) are painted over with the model.
//!
//! - **Background.** A solid colour (the median of a ring just inside the outline), or a smooth
//!   gradient: a plane (a linear gradient) or a quadratic surface (near enough a radial one),
//!   fitted by least squares over the interior with the lettering rejected as outliers. The
//!   simplest that fits about as well as the best is used.
//! - **Refusal.** A background these don't describe (screentone, a drawing, a texture) leaves the
//!   ring's residual large: the command refuses with an error starting `notPlain` and changes
//!   nothing; Remove with AI is the tool for those.
//! - **Text.** Pixels deviating from the model beyond an adaptive threshold (from the ring's noise;
//!   or `tolerance`), grown through their weaker anti-aliased neighbours and dilated a pixel. A
//!   band along the outline ([`MARGIN`]) is never touched.
//! - **Fill.** The model plus noise matched to the ring's, pasted with a 1 px feather, in one undo
//!   step on the layer (or a new layer above it).

use photocraft_color::PixelFormat;
use photocraft_doc::{Layer, LayerContent};
use photocraft_geom::Rect;
use photocraft_raster::{from_rgba_into, to_rgba};
use photocraft_typer::bubble::Mask;
use photocraft_typer::bubble_find;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

/// The command id.
pub const ID: &str = "edit.removeBubbleText";
/// The undo step.
pub const LABEL: &str = "Remove Bubble Text";
/// The refusal for a background that isn't plain (the UI shows it translated).
pub const NOT_PLAIN: &str = "This bubble has no plain background; use Remove with AI";

/// The band (px) along the interior's edge that is never painted: the outline stays as it was.
pub const MARGIN: f32 = 2.0;
/// The ring (px from the edge, past [`MARGIN`]) whose pixels give the solid colour and the noise.
const RING: f32 = 8.0;
/// At most this many interior pixels feed the fit (a grid over larger bubbles).
const MAX_SAMPLES: usize = 24_000;
/// A ring residual (robust sigma, 0..1) above this is a textured background: refused.
const MAX_NOISE: f32 = 0.03;
/// At least this share of the ring must fit the model.
const MIN_RING_FIT: f32 = 0.6;
/// The ring is judged in this many angular sectors…
const SECTORS: usize = 16;
/// …and is a texture when this many of them have pixels off the model.
const TEXTURED_SECTORS: usize = 11;
/// Lettering covering more of the interior than this isn't lettering on a plain bubble.
const MAX_TEXT_SHARE: f32 = 0.7;

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: ID.into(), msg: msg.into() }
}

/// The background model: per channel (premultiplied RGBA) over the basis `1, u, v, u², v², uv`
/// (bubble-centred coordinates), `terms` of them used.
#[derive(Clone, Copy, Debug)]
struct Model {
    coef: [[f32; 6]; 4],
    terms: usize,
    cx: f32,
    cy: f32,
    scale: f32,
}

impl Model {
    fn basis(&self, x: f32, y: f32) -> [f32; 6] {
        let (u, v) = ((x - self.cx) / self.scale, (y - self.cy) / self.scale);
        [1.0, u, v, u * u, v * v, u * v]
    }

    fn at(&self, x: f32, y: f32) -> [f32; 4] {
        let b = self.basis(x, y);
        let mut out = [0.0; 4];
        for (c, o) in out.iter_mut().enumerate() {
            *o = (0..self.terms).map(|k| self.coef[c][k] * b[k]).sum();
        }
        out
    }
}

/// One fit sample: its position, premultiplied RGBA and whether it lies in the ring.
#[derive(Clone, Copy)]
struct Sample {
    x: f32,
    y: f32,
    px: [f32; 4],
    ring: bool,
}

fn dev(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    (0..4).map(|c| (a[c] - b[c]).abs()).fold(0.0, f32::max)
}

fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let k = v.len() / 2;
    *v.select_nth_unstable_by(k, f32::total_cmp).1
}

/// A robust sigma (1.4826 × the median absolute value) of `res`.
fn robust_sigma(res: &[f32]) -> f32 {
    let mut r = res.to_vec();
    1.4826 * median(&mut r)
}

#[allow(clippy::needless_range_loop)] // Gaussian elimination reads two rows of `m` at once.
/// Least squares of `terms` basis functions over the `use_` samples, by normal equations (in f64).
fn solve(model: &Model, samples: &[Sample], use_: &[bool], terms: usize) -> Option<[[f32; 6]; 4]> {
    let mut a = [[0f64; 6]; 6];
    let mut rhs = [[0f64; 6]; 4];
    let mut n = 0usize;
    for (s, _) in samples.iter().zip(use_).filter(|(_, u)| **u) {
        n += 1;
        let b = model.basis(s.x, s.y).map(f64::from);
        for i in 0..terms {
            for j in 0..terms {
                a[i][j] += b[i] * b[j];
            }
            for (r, v) in rhs.iter_mut().zip(s.px) {
                r[i] += b[i] * f64::from(v);
            }
        }
    }
    if n < terms * 4 {
        return None;
    }
    // Gaussian elimination with partial pivoting, all four right-hand sides at once.
    let mut m = a;
    for col in 0..terms {
        let piv = (col..terms).max_by(|&i, &j| m[i][col].abs().total_cmp(&m[j][col].abs()))?;
        if m[piv][col].abs() < 1e-9 * n as f64 {
            return None;
        }
        m.swap(col, piv);
        for r in rhs.iter_mut() {
            r.swap(col, piv);
        }
        for row in col + 1..terms {
            let f = m[row][col] / m[col][col];
            for k in col..terms {
                m[row][k] -= f * m[col][k];
            }
            for r in rhs.iter_mut() {
                r[row] -= f * r[col];
            }
        }
    }
    let mut coef = [[0f32; 6]; 4];
    for c in 0..4 {
        let mut x = [0f64; 6];
        for i in (0..terms).rev() {
            let s: f64 = (i + 1..terms).map(|k| m[i][k] * x[k]).sum();
            x[i] = (rhs[c][i] - s) / m[i][i];
        }
        for i in 0..terms {
            coef[c][i] = x[i] as f32;
        }
    }
    Some(coef)
}

/// A fitted model and how well it fits: the inliers' residual sigma and the share of the ring
/// that fits.
struct Fit {
    model: Model,
    sigma: f32,
    ring_fit: f32,
    /// Per channel: the residual standard deviation of the ring's inliers (the noise to match).
    noise: [f32; 4],
}

fn inlier_limit(sigma: f32) -> f32 {
    (3.0 * sigma).max(0.03)
}

/// Fit `terms` basis functions robustly: start from `start`'s inliers, refit on the samples within
/// [`inlier_limit`] of the last fit, a few times.
fn fit(base: &Model, samples: &[Sample], start: &[bool], terms: usize) -> Option<Fit> {
    let mut use_ = start.to_vec();
    let mut model = *base;
    model.terms = terms;
    for _ in 0..4 {
        model.coef = solve(&model, samples, &use_, terms)?;
        let res: Vec<f32> = samples.iter().map(|s| dev(&s.px, &model.at(s.x, s.y))).collect();
        let inl: Vec<f32> = res.iter().zip(&use_).filter(|(_, u)| **u).map(|(r, _)| *r).collect();
        let lim = inlier_limit(robust_sigma(&inl));
        use_ = res.iter().map(|r| *r <= lim).collect();
    }
    let res: Vec<f32> = samples.iter().map(|s| dev(&s.px, &model.at(s.x, s.y))).collect();
    let ring: Vec<f32> = samples.iter().zip(&res).filter(|(s, _)| s.ring).map(|(_, r)| *r).collect();
    let sigma_all = robust_sigma(&res.iter().zip(&use_).filter(|(_, u)| **u).map(|(r, _)| *r).collect::<Vec<_>>());
    let lim = inlier_limit(sigma_all);
    let ring_fit = if ring.is_empty() { 0.0 } else { ring.iter().filter(|r| **r <= lim).count() as f32 / ring.len() as f32 };
    let sigma = robust_sigma(&ring);
    let mut noise = [0f32; 4];
    let mut n = 0f32;
    for (s, r) in samples.iter().zip(&res) {
        if s.ring && *r <= lim {
            let m = model.at(s.x, s.y);
            for c in 0..4 {
                noise[c] += (s.px[c] - m[c]).powi(2);
            }
            n += 1.0;
        }
    }
    let noise = noise.map(|v| if n > 0.0 { (v / n).sqrt() } else { 0.0 });
    Some(Fit { model, sigma, ring_fit, noise })
}

/// What the command reads: the pixels it models and where they come from.
struct Source {
    /// Premultiplied RGBA over the interior's box, row-major.
    px: Vec<[f32; 4]>,
}

/// A small deterministic noise in about -1..1 (a sum of three uniform hashes: near Gaussian, sigma
/// about 1), the same for the same pixel and channel.
fn noise(x: i32, y: i32, c: usize) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77) ^ (c as u32).wrapping_mul(0xC2B2_AE3D);
    let mut sum = 0.0;
    for _ in 0..3 {
        h ^= h >> 15;
        h = h.wrapping_mul(0x2C1B_3C6D);
        h ^= h >> 12;
        h = h.wrapping_mul(0x297A_2D39);
        h ^= h >> 15;
        sum += (h as f32 / u32::MAX as f32) * 2.0 - 1.0;
    }
    sum
}

/// Parse a boolean parameter (absent or null: `default`).
fn flag(p: &Value, key: &str, default: bool) -> Result<bool> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(bad(format!("\"{key}\" must be true or false"))),
    }
}

/// The plan: what to paint, checked before the document changes.
struct Plan {
    region: Rect,
    /// Per pixel of `region`: the paint's weight (0 untouched, 0.5 the feather, 1 the text).
    weight: Vec<f32>,
    /// Per pixel of `region`: the fill (straight RGBA).
    fill: Vec<[f32; 4]>,
    pixels: usize,
    gradient: bool,
}

fn plan(interior: &Mask, src: &Source, tolerance: Option<f32>) -> Result<Plan> {
    let (w, h) = (interior.width, interior.height);
    let (x0, y0) = (interior.x0, interior.y0);
    // Distance (px) from each interior pixel to the nearest pixel outside it.
    let outside: Vec<bool> = interior.px.iter().map(|b| !b).collect();
    let dist = photocraft_algo::selection::edt(&outside, w, h);
    if dist.len() != w * h {
        return Err(EngineError::Other("the bubble is too large".into()));
    }
    let area = interior.px.iter().filter(|b| **b).count();
    let step = (((area as f64) / MAX_SAMPLES as f64).sqrt().ceil() as usize).max(1);
    let mut samples = Vec::new();
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            let k = y * w + x;
            if interior.px[k] && dist[k] > MARGIN {
                samples.push(Sample { x: (x0 + x as i32) as f32, y: (y0 + y as i32) as f32, px: src.px[k], ring: dist[k] <= MARGIN + RING });
            }
        }
    }
    // The ring alone, and denser: a small bubble's ring has few grid samples.
    for y in 0..h {
        for x in 0..w {
            let k = y * w + x;
            if interior.px[k] && dist[k] > MARGIN && dist[k] <= MARGIN + RING && (x % step != 0 || y % step != 0) && (x + y) % 2 == 0 {
                samples.push(Sample { x: (x0 + x as i32) as f32, y: (y0 + y as i32) as f32, px: src.px[k], ring: true });
            }
        }
    }
    if samples.iter().filter(|s| s.ring).count() < 24 {
        return Err(bad("noBubble (tooSmall): the bubble is too small"));
    }
    let base = Model { coef: [[0.0; 6]; 4], terms: 1, cx: x0 as f32 + w as f32 / 2.0, cy: y0 as f32 + h as f32 / 2.0, scale: (w.max(h) as f32 / 2.0).max(1.0) };
    // The ring's median colour: the solid model, and where the outliers (lettering) start.
    let ring: Vec<&Sample> = samples.iter().filter(|s| s.ring).collect();
    let mut solid = base;
    for c in 0..4 {
        let mut v: Vec<f32> = ring.iter().map(|s| s.px[c]).collect();
        solid.coef[c][0] = median(&mut v);
    }
    // A plane over the ring first (robust to a gradient across it), then every model over the
    // whole interior, starting from the plane's inliers.
    let ring_only: Vec<bool> = samples.iter().map(|s| s.ring).collect();
    let r0: Vec<f32> = samples.iter().map(|s| dev(&s.px, &solid.at(s.x, s.y))).collect();
    let ring_res: Vec<f32> = r0.iter().zip(&ring_only).filter(|(_, r)| **r).map(|(v, _)| *v).collect();
    let lim0 = inlier_limit(robust_sigma(&ring_res)).max(0.12);
    let start0: Vec<bool> = r0.iter().zip(&ring_only).map(|(v, r)| *r && *v <= lim0).collect();
    let plane_ring = fit(&base, &samples, &start0, 3);
    let start: Vec<bool> = match &plane_ring {
        Some(f) => samples.iter().map(|s| dev(&s.px, &f.model.at(s.x, s.y)) <= inlier_limit(f.sigma).max(0.06)).collect(),
        None => r0.iter().map(|v| *v <= lim0).collect(),
    };
    let fits: Vec<Fit> = [1usize, 3, 6].iter().filter_map(|&t| fit(&base, &samples, &start, t)).collect();
    let best = fits.iter().map(|f| f.sigma).fold(f32::INFINITY, f32::min);
    let chosen = fits.iter().find(|f| f.sigma <= best * 1.25 + 0.004 && f.ring_fit >= MIN_RING_FIT).or_else(|| fits.first());
    let Some(fit) = chosen else { return Err(EngineError::Other(format!("notPlain: {NOT_PLAIN}"))) };
    if fit.sigma.is_nan() || fit.sigma > MAX_NOISE || fit.ring_fit < MIN_RING_FIT {
        return Err(EngineError::Other(format!("notPlain: {NOT_PLAIN}")));
    }
    // The lettering: far from the model (core), and its weaker anti-aliased edge next to it.
    let sigma = fit.sigma;
    let (hi, lo) = match tolerance {
        Some(t) => (t, t * 0.35),
        None => ((6.0 * sigma).max(0.10), (3.0 * sigma).max(0.035)),
    };
    let model = fit.model;
    // A texture (screentone, grain) differs from the model all around the ring; lettering only
    // reaches it here and there.
    let mut sectors = [(0u32, 0u32); SECTORS];
    for s in samples.iter().filter(|s| s.ring) {
        let a = (s.y - model.cy).atan2(s.x - model.cx);
        let k = (((a / std::f32::consts::TAU + 0.5) * SECTORS as f32) as usize).min(SECTORS - 1);
        sectors[k].0 += 1;
        if dev(&s.px, &model.at(s.x, s.y)) > lo {
            sectors[k].1 += 1;
        }
    }
    let rough = sectors.iter().filter(|(n, o)| *n > 0 && *o as f32 > 0.06 * *n as f32).count();
    if rough >= TEXTURED_SECTORS {
        return Err(EngineError::Other(format!("notPlain: {NOT_PLAIN}")));
    }
    let mut d = vec![0f32; w * h];
    let mut core = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let k = y * w + x;
            if interior.px[k] && dist[k] > MARGIN {
                d[k] = dev(&src.px[k], &model.at((x0 + x as i32) as f32, (y0 + y as i32) as f32));
                core[k] = d[k] > hi;
            }
        }
    }
    let paintable = |k: usize| interior.px[k] && dist[k] > MARGIN;
    let grow = |m: &[bool], cond: &dyn Fn(usize) -> bool| -> Vec<bool> {
        let mut out = m.to_vec();
        for y in 0..h {
            for x in 0..w {
                let k = y * w + x;
                if m[k] || !cond(k) {
                    continue;
                }
                let near = (y.saturating_sub(1)..(y + 2).min(h)).any(|yy| (x.saturating_sub(1)..(x + 2).min(w)).any(|xx| m[yy * w + xx]));
                out[k] = near;
            }
        }
        out
    };
    let mut mask = core;
    for _ in 0..3 {
        mask = grow(&mask, &|k| paintable(k) && d[k] > lo);
    }
    // Dilated a pixel (the last of the anti-aliasing), never into the outline's band.
    mask = grow(&mask, &paintable);
    let pixels = mask.iter().filter(|m| **m).count();
    if pixels == 0 {
        return Err(EngineError::Other("noText: no text found in this bubble".into()));
    }
    if pixels as f32 > MAX_TEXT_SHARE * area as f32 {
        return Err(EngineError::Other(format!("notPlain: {NOT_PLAIN}")));
    }
    let feather = grow(&mask, &paintable);
    let mut weight = vec![0f32; w * h];
    let mut fill = vec![[0f32; 4]; w * h];
    for y in 0..h {
        for x in 0..w {
            let k = y * w + x;
            if !feather[k] {
                continue;
            }
            weight[k] = if mask[k] { 1.0 } else { 0.5 };
            let (dx, dy) = (x0 + x as i32, y0 + y as i32);
            let mut m = model.at(dx as f32, dy as f32);
            for (c, v) in m.iter_mut().enumerate() {
                *v += fit.noise[c] * noise(dx, dy, c);
            }
            let a = m[3].clamp(0.0, 1.0);
            let un = |v: f32| if a > 1e-6 { (v / a).clamp(0.0, 1.0) } else { 0.0 };
            fill[k] = [un(m[0]), un(m[1]), un(m[2]), a];
        }
    }
    Ok(Plan { region: Rect::new(x0, y0, x0 + w as i32, y0 + h as i32), weight, fill, pixels, gradient: fit.model.terms > 1 })
}

fn remove_bubble_text(s: &mut Session, p: &Value) -> Result<Value> {
    if !p.is_object() && !p.is_null() {
        return Err(bad("params must be a JSON object"));
    }
    let coord =
        |k: &str| p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite() && v.abs() < 1e9).ok_or_else(|| bad("\"x\" and \"y\" must be document pixels"));
    let (x, y) = (coord("x")?.floor() as i32, coord("y")?.floor() as i32);
    let tolerance = match p.get("tolerance") {
        None | Some(Value::Null) => None,
        Some(Value::String(a)) if a == "auto" => None,
        Some(v) => match v.as_f64() {
            Some(t) if (1.0..=255.0).contains(&t) => Some((t / 255.0) as f32),
            _ => return Err(bad("\"tolerance\" must be 1..255 (a colour difference) or \"auto\"")),
        },
    };
    let mut sample_all = flag(p, "sampleAll", false)?;
    let mut new_layer = flag(p, "newLayer", false)?;
    // The layer painted: the active pixel layer; a type, shape or other layer (or none) gets the
    // result on a new layer, modelled from what is seen.
    let (active, editable) = {
        let d = s.active().ok_or(EngineError::NoDocument)?;
        let active = d.active_layer;
        let editable = active.and_then(|id| d.doc.layer(id)).is_some_and(|l| matches!(l.content, LayerContent::Raster(_)));
        if editable && let Some(id) = active {
            let locks = d.doc.effective_locks(id);
            if !new_layer && (locks.all || locks.pixels) {
                return Err(EngineError::Other(format!("the layer \"{}\" is locked", d.doc.layer(id).map(|l| l.name.as_str()).unwrap_or(""))));
            }
        }
        (active, editable)
    };
    if !editable {
        sample_all = true;
        new_layer = true;
    }
    let page = crate::typer_place_cmds::page_image(s)?;
    let layer_fmt =
        s.active().ok_or(EngineError::NoDocument)?.doc.layer(active.unwrap_or(photocraft_doc::LayerId(u64::MAX))).and_then(|l| l.surface()).map(|s| s.format());
    let premul = |c: [f32; 4]| {
        let a = if c[3].is_finite() { c[3].clamp(0.0, 1.0) } else { 0.0 };
        let v = |x: f32| if x.is_finite() { x.clamp(0.0, 1.0) * a } else { 0.0 };
        [v(c[0]), v(c[1]), v(c[2]), a]
    };
    // The bubble: closed, else cut by the image's edge, else (it opens onto a panel border or the
    // gutter) the block of lettering under the click with a margin, its panel lines kept as walls.
    let plan_in = |interior: &Mask| -> Result<Plan> {
        let s = &*s;
        let region = Rect::new(interior.x0, interior.y0, interior.x0 + interior.width as i32, interior.y0 + interior.height as i32);
        let (w, h) = (interior.width, interior.height);
        let d = s.active().ok_or(EngineError::NoDocument)?;
        let layer_fmt = active.and_then(|id| d.doc.layer(id)).and_then(|l| l.surface()).map(|s| s.format());
        let layer_px = match (active, layer_fmt) {
            (Some(id), Some(_)) if editable => d.doc.layer(id).and_then(|l| l.surface()).map(|s| s.read_region(region)),
            _ => None,
        };
        let premul = |c: [f32; 4]| {
            let a = if c[3].is_finite() { c[3].clamp(0.0, 1.0) } else { 0.0 };
            let v = |x: f32| if x.is_finite() { x.clamp(0.0, 1.0) * a } else { 0.0 };
            [v(c[0]), v(c[1]), v(c[2]), a]
        };
        let src = if sample_all || layer_px.is_none() {
            // The page image (without type layers), the box clipped to it.
            let pa = page.area;
            let pw = pa.width() as usize;
            let mut px = vec![[0f32; 4]; w * h];
            for yy in 0..h {
                for xx in 0..w {
                    let (dx, dy) = (region.x0 + xx as i32, region.y0 + yy as i32);
                    if pa.contains(dx, dy) {
                        let q = page.img[(dy - pa.y0) as usize * pw + (dx - pa.x0) as usize];
                        px[yy * w + xx] = premul(q.map(|v| f32::from(v) / 255.0));
                    }
                }
            }
            Source { px }
        } else {
            let fmt = layer_fmt.ok_or_else(|| EngineError::Other("not a pixel layer".into()))?;
            let n = fmt.channels().max(1);
            let raw = layer_px.as_deref().unwrap_or(&[]);
            if raw.len() != w * h * n {
                return Err(EngineError::Other("could not read the layer's pixels".into()));
            }
            Source { px: raw.chunks_exact(n).map(|c| premul(to_rgba(&fmt, c))).collect() }
        };
        plan(interior, &src, tolerance)
    };
    let (plan, region_kind) = match bubble_find::find(&page.img, page.area, (x, y)) {
        Ok(found) => (plan_in(&found.interior)?, "bubble"),
        Err(miss) => {
            let mut first: Option<EngineError> = None;
            let mut out = None;
            if let Ok(found) = bubble_find::find_walled(&page.img, page.area, (x, y)) {
                match plan_in(&found.interior) {
                    Ok(p) => out = Some((p, "edge")),
                    Err(e) => first = Some(e),
                }
            }
            if out.is_none()
                && let Some(c) = photocraft_typer::text_cluster::find(&page.img, page.area, (x, y))
            {
                let m = crate::type_bubble_cmds::open_bubble(&page, x, y).map(|f| f.interior).unwrap_or_else(|| {
                    let mut m = c.walls.clone();
                    m.px.iter_mut().for_each(|v| *v = !*v);
                    m
                });
                match plan_in(&m) {
                    Ok(p) => out = Some((p, "textCluster")),
                    Err(e) => first = first.or(Some(e)),
                }
            }
            match out {
                Some(o) => o,
                None => return Err(first.unwrap_or_else(|| crate::type_bubble_cmds::no_bubble(miss))),
            }
        }
    };
    let Plan { region, weight, fill, pixels, gradient } = plan;
    let layer = s.edit(LABEL, |doc, act| {
        if new_layer {
            let base = layer_fmt.unwrap_or_else(|| PixelFormat { alpha: true, ..doc.pixel_format() });
            let mut l = Layer::raster(doc.next_layer_name("Bubble Clean"), PixelFormat { alpha: true, ..base });
            let lf = l.surface().map(|s| s.format()).unwrap_or(PixelFormat { alpha: true, ..base });
            let ln = lf.channels();
            let mut px = vec![0.0f32; weight.len() * ln];
            let mut enc = [0.0f32; 8];
            for (k, &wt) in weight.iter().enumerate() {
                if wt > 0.0 {
                    let f = fill[k];
                    from_rgba_into(&lf, [f[0], f[1], f[2], f[3] * wt], &mut enc);
                    px[k * ln..(k + 1) * ln].copy_from_slice(&enc[..ln]);
                }
            }
            if let Some(surf) = l.surface_mut() {
                surf.write_region(region, &px);
                surf.prune();
            }
            let nid = doc.insert_above(active, l);
            *act = Some(nid);
            return Ok(nid);
        }
        let id = active.ok_or(EngineError::Other("no active layer".into()))?;
        let surf = crate::edit_menu_cmds::writable_surface(doc, id)?;
        let fmt = surf.format();
        let n = fmt.channels();
        let mut out = surf.read_region(region);
        let mut enc = [0.0f32; 8];
        for (k, &wt) in weight.iter().enumerate() {
            if wt > 0.0 {
                let o = premul(to_rgba(&fmt, &out[k * n..(k + 1) * n]));
                let f = fill[k];
                let fp = [f[0] * f[3], f[1] * f[3], f[2] * f[3], f[3]];
                let m: [f32; 4] = std::array::from_fn(|c| o[c] + (fp[c] - o[c]) * wt);
                let a = m[3];
                let un = |v: f32| if a > 1e-6 { (v / a).clamp(0.0, 1.0) } else { 0.0 };
                from_rgba_into(&fmt, [un(m[0]), un(m[1]), un(m[2]), a], &mut enc);
                out[k * n..(k + 1) * n].copy_from_slice(&enc[..n]);
            }
        }
        surf.write_region(region, &out);
        Ok(id)
    })?;
    if let Some(st) = s.active_mut() {
        st.last_damage = Some(region);
    }
    let bounds = s.active().map_or(region, |d| region.intersect(&d.doc.bounds()));
    Ok(json!({
        "ok": true,
        "layer": layer.0,
        "bubbleBounds": [bounds.x0, bounds.y0, bounds.width(), bounds.height()],
        "pixels": pixels,
        "background": if gradient { "gradient" } else { "solid" },
        "region": region_kind,
        "newLayer": new_layer,
    }))
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document".into())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: ID,
        label: LABEL,
        menu: &[],
        shortcut: None,
        params: r##"{"x":px,"y":px (the click),"tolerance"?:1..255|"auto" (how far from the background, as a colour difference, a pixel counts as text; default auto: from the bubble's noise),"sampleAll"?:bool (model the background from every visible layer, not the active one; default false),"newLayer"?:bool (the result on a new layer above; default false)} → {"ok":true,"layer","bubbleBounds":[x,y,w,h],"pixels" (painted),"background":"solid"|"gradient","newLayer"}. The Bubble Text Remover tool's click: the text in the speech bubble, narration box or SFX bubble at the point is removed without AI. The bubble is found as for type.createInBubble (a click on a letter still finds it; a bubble cut by the page edge, or one that opens onto a panel border or the gutter, is bounded by the edge or by its lettering's block, its panel lines kept as walls; "region" says which: "bubble", "edge" or "textCluster"); its background is a solid colour or a smooth gradient fitted around the lettering, and only the pixels that differ from it (with their anti-aliased edge, a 1 px feather) are painted with it plus matching noise; a 2 px band along the outline is never touched. Errors: "noBubble (…)" with no bubble there, "notPlain: …" when the background isn't plain (screentone, art: use Remove with AI), "noText: …" when nothing in it differs; none changes anything. A type, shape or other non-pixel active layer gets the result on a new layer. One undo step"##,
        enabled,
        journal: true,
        run: remove_bubble_text,
    }]
}

#[cfg(test)]
#[path = "bubble_text_cmds/tests.rs"]
mod tests;
