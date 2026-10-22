//! The crop around the selection that Generative Fill works on, and the way back.
//!
//! One crop per run: the selection's bounds grown by context (a quarter of its long side, at
//! least [`MIN_CONTEXT`] px), clipped to the readable region, resampled to the working size
//! (long side = the tier's side, the other a multiple of 64 keeping the aspect). The generated
//! crop is resampled back to the crop's pixel size; the caller masks it with the selection.

use photocraft_ai_lama::tiles::resample;

use crate::{Result, SdError};

/// Smallest context margin around the selection (pixels of the source).
pub const MIN_CONTEXT: usize = 32;

/// A crop of the source region and its working size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crop {
    /// In the region's pixels: `x0, y0, width, height`.
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    /// Working size (multiples of 64).
    pub work_w: usize,
    pub work_h: usize,
}

fn round64(v: f64, lo: usize, hi: usize) -> usize {
    (((v / 64.0).round() as usize) * 64).clamp(lo, hi)
}

/// Plans the crop for `cover` (`w·h` coverage of the region) at working long side `side`
/// (a multiple of 64, 256..=1024).
pub fn plan(cover: &[f32], w: usize, h: usize, side: usize) -> Result<Crop> {
    if w == 0 || h == 0 || cover.len() != w * h {
        return Err(SdError::Input("coverage doesn't match the region".into()));
    }
    if !side.is_multiple_of(64) || !(128..=1024).contains(&side) {
        return Err(SdError::Input(format!("working side {side} must be a multiple of 64 in 128..=1024")));
    }
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (i, c) in cover.iter().enumerate() {
        if *c > 0.0 {
            let (x, y) = (i % w, i / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    if x0 == usize::MAX {
        return Err(SdError::Input("the selection is empty".into()));
    }
    let ctx = ((x1 - x0).max(y1 - y0) / 4).max(MIN_CONTEXT);
    let (cx0, cy0) = (x0.saturating_sub(ctx), y0.saturating_sub(ctx));
    let (cx1, cy1) = ((x1 + ctx).min(w), (y1 + ctx).min(h));
    let (cw, ch) = (cx1 - cx0, cy1 - cy0);
    let (work_w, work_h) = if cw >= ch {
        (side, round64(side as f64 * ch as f64 / cw as f64, 128.min(side), side))
    } else {
        (round64(side as f64 * cw as f64 / ch as f64, 128.min(side), side), side)
    };
    Ok(Crop { x: cx0, y: cy0, w: cw, h: ch, work_w, work_h })
}

impl Crop {
    /// The crop of an interleaved `ch`-channel region (`w` wide), resampled to the working size.
    pub fn extract(&self, src: &[f32], w: usize, ch: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.w * self.h * ch);
        for y in self.y..self.y + self.h {
            let row = (y * w + self.x) * ch;
            if let Some(s) = src.get(row..row + self.w * ch) {
                out.extend_from_slice(s);
            } else {
                out.extend(std::iter::repeat_n(0.0, self.w * ch));
            }
        }
        resample(&out, self.w, self.h, ch, self.work_w, self.work_h)
    }

    /// A generated working-size image (interleaved RGB) back at the crop's pixel size.
    pub fn restore(&self, generated: &[f32]) -> Vec<f32> {
        resample(generated, self.work_w, self.work_h, 3, self.w, self.h)
    }
}

/// Fills the masked pixels (`mask >= 0.5`) of interleaved RGB from their surroundings with a
/// pull–push pyramid (smooth membrane interpolation), so an empty-prompt fill starts from the
/// background's colours instead of noise.
pub fn prefill(rgb: &[f32], mask: &[f32], w: usize, h: usize) -> Vec<f32> {
    if w == 0 || h == 0 || rgb.len() != w * h * 3 || mask.len() != w * h {
        return rgb.to_vec();
    }
    // Level 0: premultiplied colour and weight.
    let mut levels: Vec<(usize, usize, Vec<[f32; 4]>)> = Vec::new();
    let base: Vec<[f32; 4]> = (0..w * h)
        .map(|i| {
            let k = if mask.get(i).copied().unwrap_or(0.0) >= 0.5 { 0.0 } else { 1.0 };
            let p = rgb.get(i * 3..i * 3 + 3).unwrap_or(&[0.0; 3]);
            [p[0] * k, p[1] * k, p[2] * k, k]
        })
        .collect();
    levels.push((w, h, base));
    // Pull: average 2×2 blocks.
    while let Some((lw, lh, px)) = levels.last() {
        if *lw <= 1 && *lh <= 1 {
            break;
        }
        let (nw, nh) = (lw.div_ceil(2), lh.div_ceil(2));
        let mut next = vec![[0f32; 4]; nw * nh];
        for y in 0..*lh {
            for x in 0..*lw {
                if let (Some(src), Some(dst)) = (px.get(y * lw + x), next.get_mut((y / 2) * nw + x / 2)) {
                    for c in 0..4 {
                        dst[c] += src[c];
                    }
                }
            }
        }
        // Normalise to weight ≤ 1 (keeps colour/weight ratios).
        for p in &mut next {
            if p[3] > 1.0 {
                let k = 1.0 / p[3];
                for c in p.iter_mut() {
                    *c *= k;
                }
            }
        }
        levels.push((nw, nh, next));
    }
    // Push: fill each level's missing weight from the coarser one, sampled bilinearly (nearest
    // sampling leaves 2ⁿ-pixel blocks the model then keeps).
    for l in (0..levels.len().saturating_sub(1)).rev() {
        let (cw, ch, coarse) = {
            let (cw, ch, c) = &levels[l + 1];
            (*cw, *ch, c.clone())
        };
        let colour = |x: usize, y: usize| -> Option<[f32; 3]> {
            let c = coarse.get(y.min(ch - 1) * cw + x.min(cw - 1))?;
            (c[3] > 0.0).then(|| [c[0] / c[3], c[1] / c[3], c[2] / c[3]])
        };
        let (lw, lh, px) = &mut levels[l];
        for y in 0..*lh {
            for x in 0..*lw {
                let Some(p) = px.get_mut(y * *lw + x) else { continue };
                let missing = (1.0 - p[3]).max(0.0);
                if missing <= 0.0 {
                    continue;
                }
                let fx = ((x as f32 + 0.5) / 2.0 - 0.5).max(0.0);
                let fy = ((y as f32 + 0.5) / 2.0 - 0.5).max(0.0);
                let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
                let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
                let (mut acc, mut wsum) = ([0f32; 3], 0f32);
                for (xx, yy, wgt) in
                    [(x0, y0, (1.0 - ax) * (1.0 - ay)), (x0 + 1, y0, ax * (1.0 - ay)), (x0, y0 + 1, (1.0 - ax) * ay), (x0 + 1, y0 + 1, ax * ay)]
                {
                    if let Some(c) = colour(xx, yy) {
                        for k in 0..3 {
                            acc[k] += c[k] * wgt;
                        }
                        wsum += wgt;
                    }
                }
                if wsum > 0.0 {
                    for k in 0..3 {
                        p[k] += acc[k] / wsum * missing;
                    }
                    p[3] += missing;
                }
            }
        }
    }
    let (_, _, px) = &levels[0];
    let mut out = rgb.to_vec();
    for (i, p) in px.iter().enumerate() {
        if mask.get(i).copied().unwrap_or(0.0) >= 0.5 && p[3] > 0.0 {
            for k in 0..3 {
                if let Some(o) = out.get_mut(i * 3 + k) {
                    *o = (p[k] / p[3]).clamp(0.0, 1.0);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_grows_by_context_and_keeps_aspect() {
        let (w, h) = (1000, 600);
        let mut cover = vec![0.0; w * h];
        for y in 200..300 {
            for x in 400..600 {
                cover[y * w + x] = 1.0;
            }
        }
        let c = plan(&cover, w, h, 512).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((c.x, c.y, c.w, c.h), (350, 150, 300, 200));
        assert_eq!((c.work_w, c.work_h), (512, 320));
        let img = vec![0.5; w * h * 3];
        let x = c.extract(&img, w, 3);
        assert_eq!(x.len(), 512 * 320 * 3);
        assert_eq!(c.restore(&x).len(), 300 * 200 * 3);
    }

    #[test]
    fn prefill_takes_the_surrounding_colour() {
        let (w, h) = (16, 16);
        let rgb: Vec<f32> = (0..w * h).flat_map(|i| if i % w < 8 { [1.0, 0.0, 0.0] } else { [0.0, 0.0, 1.0] }).collect();
        let mut mask = vec![0.0; w * h];
        for y in 4..12 {
            for x in 2..6 {
                mask[y * w + x] = 1.0;
            }
        }
        let out = prefill(&rgb, &mask, w, h);
        let p = &out[(8 * w + 3) * 3..(8 * w + 3) * 3 + 3];
        assert!(p[0] > 0.6 && p[2] < 0.4, "{p:?}");
        assert_eq!(&out[..3], &rgb[..3]);
    }

    #[test]
    fn bad_input_is_an_error() {
        assert!(plan(&[0.0; 4], 2, 2, 512).is_err());
        assert!(plan(&[1.0; 4], 2, 3, 512).is_err());
        assert!(plan(&[1.0; 4], 2, 2, 500).is_err());
        let c = plan(&[1.0; 4], 2, 2, 512).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((c.w, c.h, c.work_w, c.work_h), (2, 2, 512, 512));
    }
}
