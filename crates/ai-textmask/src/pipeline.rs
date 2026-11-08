//! What a run does around the network, wherever it runs: the page at the size the network wants,
//! tiles and their blending, and the regions from the logits.

use crate::regions::{Detection, fill_bodies, fill_hollow, find_regions, merge_giant};

/// The network works on pages of about this height; other pages are resized for it only.
pub const MIN_HEIGHT: usize = 1500;
pub const MAX_HEIGHT: usize = 2600;
/// Giant mode (the file's `giant`, Shin3): every page also runs in one piece at this part of its
/// working height, where a giant effect looks like letters of the size the network knows best.
pub const GIANT: bool = true;
const GIANT_SCALE: f64 = 0.5;

/// The size a page reaches the network at, and whether that's a resize.
pub fn net_size(w: usize, h: usize) -> (usize, usize) {
    let scale = if h < MIN_HEIGHT { MIN_HEIGHT as f64 / h as f64 } else if h > MAX_HEIGHT { MAX_HEIGHT as f64 / h as f64 } else { 1.0 };
    if scale == 1.0 {
        return (w, h);
    }
    (((w as f64 * scale).round() as usize).max(1), ((h as f64 * scale).round() as usize).max(1))
}

/// The size of giant mode's half-size pass.
pub fn half_size(w: usize, h: usize) -> (usize, usize) {
    let hh = (h.clamp(MIN_HEIGHT, MAX_HEIGHT) as f64) * GIANT_SCALE;
    (((w as f64 * hh / h as f64).round() as usize).max(32), (hh.round() as usize).max(32))
}

/// `rgb` (`w × h`) padded with white to multiples of 32: the network's input in one piece, and
/// the padded size.
pub fn padded(rgb: &[u8], w: usize, h: usize) -> (Vec<u8>, usize, usize) {
    let (pw, ph) = (w.div_ceil(32) * 32, h.div_ceil(32) * 32);
    let mut canvas = vec![255u8; pw * ph * 3];
    for y in 0..h {
        canvas[y * pw * 3..(y * pw + w) * 3].copy_from_slice(&rgb[y * w * 3..(y * w + w) * 3]);
    }
    (canvas, pw, ph)
}

/// The logits of a padded run cut back to `w × h`.
pub fn unpadded(out: &[f32], pw: usize, w: usize, h: usize) -> Vec<f32> {
    let mut logits = vec![0f32; w * h];
    for y in 0..h {
        logits[y * w..y * w + w].copy_from_slice(&out[y * pw..y * pw + w]);
    }
    logits
}

/// Where tiles of `t` overlapping by `overlap` start along a side of `len`.
pub fn starts(len: usize, t: usize, overlap: usize) -> Vec<usize> {
    let mut v: Vec<usize> = (0..=len.saturating_sub(t)).step_by(t - overlap).collect();
    if v.last().is_some_and(|l| l + t < len) {
        v.push(len - t);
    }
    v
}

/// Fixed-size tiles, blended with linear ramps over the overlaps.
pub struct Blend {
    w: usize,
    h: usize,
    t: usize,
    ramp: Vec<f32>,
    total: Vec<f32>,
    weight: Vec<f32>,
}

impl Blend {
    pub fn new(w: usize, h: usize, t: usize, overlap: usize) -> Self {
        let ramp = (0..t).map(|i| ((i + 1).min(overlap).min(t - i)) as f32 / overlap as f32).collect();
        Blend { w, h, t, ramp, total: vec![0.0; w * h], weight: vec![0.0; w * h] }
    }

    /// The tile at `(x0, y0)` of `rgb` (the whole page, interleaved), on white past the page.
    pub fn tile(&self, rgb: &[u8], x0: usize, y0: usize) -> Vec<u8> {
        let (t, w) = (self.t, self.w);
        let (cw, ch) = (t.min(w - x0), t.min(self.h - y0));
        let mut tile = vec![255u8; t * t * 3];
        for y in 0..ch {
            tile[y * t * 3..(y * t + cw) * 3].copy_from_slice(&rgb[((y0 + y) * w + x0) * 3..((y0 + y) * w + x0 + cw) * 3]);
        }
        tile
    }

    /// Adds a tile's logits.
    pub fn add(&mut self, logits: &[f32], x0: usize, y0: usize) {
        let (t, w) = (self.t, self.w);
        let (cw, ch) = (t.min(w - x0), t.min(self.h - y0));
        for y in 0..ch {
            for x in 0..cw {
                let k = self.ramp[y] * self.ramp[x];
                self.total[(y0 + y) * w + x0 + x] += logits[y * t + x] * k;
                self.weight[(y0 + y) * w + x0 + x] += k;
            }
        }
    }

    pub fn logits(&self) -> Vec<f32> {
        self.total.iter().zip(&self.weight).map(|(a, b)| a / b.max(1e-6)).collect()
    }
}

/// Logits at `(nw, nh)` as probabilities at the page's size.
fn probabilities(logits: &[f32], nw: usize, nh: usize, w: usize, h: usize) -> Vec<f32> {
    let prob: Vec<f32> = logits.iter().map(|l| 1.0 / (1.0 + (-l).exp())).collect();
    if (nw, nh) == (w, h) { prob } else { crate::resize::plane(&prob, nw, nh, w, h) }
}

/// The page's regions from the logits at `net_size`, and in giant mode those of the half-size pass
/// at `half_size`.
pub fn regions(logits: &[f32], nw: usize, nh: usize, half: Option<(&[f32], usize, usize)>, rgb: &[u8], w: usize, h: usize) -> Detection {
    let mut prob = probabilities(logits, nw, nh, w, h);
    fill_hollow(&mut prob, rgb, w, h);
    if let Some((half, sw, sh)) = half {
        merge_giant(&mut prob, &probabilities(half, sw, sh, w, h), rgb, w, h);
    }
    let mut d = find_regions(&prob, w, h);
    if half.is_some() {
        fill_bodies(&mut d, rgb);
    }
    d
}

/// `rgb` as the network's input, planar in 0..=1.
pub fn planar(rgb: &[u8], w: usize, h: usize) -> Vec<f32> {
    let n = w * h;
    let mut x = vec![0f32; n * 3];
    for (i, px) in rgb.chunks_exact(3).enumerate() {
        for c in 0..3 {
            x[c * n + i] = f32::from(px[c]) / 255.0;
        }
    }
    x
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn tiles_cover_the_page_and_end_at_its_edge() {
        assert_eq!(starts(1000, 512, 64), vec![0, 448, 488]);
        assert_eq!(starts(300, 512, 64), vec![0], "smaller than a tile");
    }

    #[test]
    fn blending_flat_tiles_stays_flat_and_white_pads_past_the_page() {
        let (w, h) = (700, 600);
        let mut b = Blend::new(w, h, 512, 64);
        let rgb = vec![10u8; w * h * 3];
        for &y in &starts(h, 512, 64) {
            for &x in &starts(w, 512, 64) {
                b.add(&vec![2.0; 512 * 512], x, y);
            }
        }
        assert!(b.logits().iter().all(|v| (*v - 2.0).abs() < 1e-3));
        let t = b.tile(&rgb, 300, 200);
        assert_eq!(t[0], 10, "the page");
        let t = Blend::new(300, 200, 512, 64).tile(&vec![10u8; 300 * 200 * 3], 0, 0);
        assert_eq!(t[(511 * 512 + 511) * 3], 255, "past it");
    }

    #[test]
    fn a_page_reaches_the_network_at_its_working_height() {
        assert_eq!(net_size(1284, 1826), (1284, 1826));
        assert_eq!(net_size(500, 1000), (750, 1500));
        assert_eq!(net_size(1000, 5200), (500, 2600));
        assert_eq!(net_size(1725, 2475), (1725, 2475), "wide pages too");
    }

    #[test]
    fn giant_mode_runs_at_half_the_working_height() {
        assert_eq!(half_size(1284, 1826), (642, 913));
        assert_eq!(half_size(500, 1000), (375, 750));
        assert_eq!(half_size(1000, 5200), (250, 1300));
        assert_eq!(half_size(40, 3000), (32, 1300), "never under 32");
    }
}
