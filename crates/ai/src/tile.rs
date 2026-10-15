//! Tiling with overlap and blending, so images of any size run in bounded memory.
//!
//! [`plan`] splits the input into a grid of near-equal *cores* (at most `tile` pixels a side).
//! Each tile's input is its core grown by `overlap` pixels of context on every side that has a
//! neighbour (clipped to the image). [`Blender`] puts the scaled results together: across each
//! seam the two tiles cross-fade with complementary smoothstep weights over the middle half of
//! the shared context, so the weights sum to exactly 1 everywhere and the outermost context
//! pixels (the least accurate ones) are never used.

use crate::{AiError, Planar, Result};

/// How to split an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileOptions {
    /// Largest core size in input pixels (each tile adds `overlap` context on inner sides).
    pub tile: usize,
    /// Context in input pixels added on each inner side.
    pub overlap: usize,
}

impl Default for TileOptions {
    fn default() -> Self {
        Self { tile: 128, overlap: 16 }
    }
}

/// One tile: the input rectangle to run (`x0..x1`, `y0..y1`, context included) and where its
/// seams with the neighbouring tiles are (input coordinates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tile {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
    pub seam_left: Option<usize>,
    pub seam_right: Option<usize>,
    pub seam_top: Option<usize>,
    pub seam_bottom: Option<usize>,
}

impl Tile {
    pub fn width(&self) -> usize {
        self.x1 - self.x0
    }
    pub fn height(&self) -> usize {
        self.y1 - self.y0
    }
}

/// A tiling of a `width × height` image, plus the overlap actually used (never more than a
/// core, so seams' blend zones never meet).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub tiles: Vec<Tile>,
    pub overlap: usize,
}

/// Splits `width × height` into tiles.
pub fn plan(width: usize, height: usize, opts: TileOptions) -> Result<Plan> {
    if width == 0 || height == 0 {
        return Err(AiError::Image("empty image".into()));
    }
    if opts.tile == 0 {
        return Err(AiError::Image("tile size must be at least 1".into()));
    }
    let (xs, cx) = cores(width, opts.tile);
    let (ys, cy) = cores(height, opts.tile);
    let overlap = opts.overlap.min(cx).min(cy);
    let mut tiles = Vec::with_capacity(xs.len().saturating_mul(ys.len()));
    for &(cy0, cy1) in &ys {
        for &(cx0, cx1) in &xs {
            let seam_left = (cx0 > 0).then_some(cx0);
            let seam_right = (cx1 < width).then_some(cx1);
            let seam_top = (cy0 > 0).then_some(cy0);
            let seam_bottom = (cy1 < height).then_some(cy1);
            tiles.push(Tile {
                x0: cx0.saturating_sub(if seam_left.is_some() { overlap } else { 0 }),
                x1: (cx1 + if seam_right.is_some() { overlap } else { 0 }).min(width),
                y0: cy0.saturating_sub(if seam_top.is_some() { overlap } else { 0 }),
                y1: (cy1 + if seam_bottom.is_some() { overlap } else { 0 }).min(height),
                seam_left,
                seam_right,
                seam_top,
                seam_bottom,
            });
        }
    }
    Ok(Plan { tiles, overlap })
}

/// Near-equal cores of at most `tile` along a side of `n`, and the smallest core size.
fn cores(n: usize, tile: usize) -> (Vec<(usize, usize)>, usize) {
    let count = n.div_ceil(tile).max(1);
    let size = n.div_ceil(count);
    let v: Vec<(usize, usize)> = (0..count).map(|i| (i * size, ((i + 1) * size).min(n))).filter(|(a, b)| a < b).collect();
    let smallest = v.iter().map(|(a, b)| b - a).min().unwrap_or(n);
    (v, smallest)
}

/// Collects scaled tile results into the output image.
pub struct Blender {
    out: Planar,
    scale: usize,
    overlap: usize,
}

impl Blender {
    /// An output for a `width × height` input at `scale`, with `channels` channels.
    pub fn new(width: usize, height: usize, channels: usize, scale: usize, plan: &Plan) -> Result<Self> {
        let w = width.checked_mul(scale).ok_or_else(|| AiError::Image("output too large".into()))?;
        let h = height.checked_mul(scale).ok_or_else(|| AiError::Image("output too large".into()))?;
        Ok(Self { out: Planar::zeros(w, h, channels)?, scale, overlap: plan.overlap })
    }

    /// Adds one tile's result (its input rectangle scaled by `scale`).
    pub fn add(&mut self, tile: &Tile, result: &Planar) -> Result<()> {
        let s = self.scale;
        if result.width != tile.width() * s || result.height != tile.height() * s || result.channels != self.out.channels {
            return Err(AiError::Inference(format!(
                "tile result is {}×{}×{}, expected {}×{}×{}",
                result.width,
                result.height,
                result.channels,
                tile.width() * s,
                tile.height() * s,
                self.out.channels
            )));
        }
        let o = (self.overlap * s) as f32;
        let wx = weights(tile.x0 * s, result.width, tile.seam_left.map(|p| p * s), tile.seam_right.map(|p| p * s), o);
        let wy = weights(tile.y0 * s, result.height, tile.seam_top.map(|p| p * s), tile.seam_bottom.map(|p| p * s), o);
        let ow = self.out.width;
        for c in 0..result.channels {
            let src = result.plane(c);
            let dst = self.out.plane_mut(c);
            for (ty, &wyv) in wy.iter().enumerate() {
                if wyv == 0.0 {
                    continue;
                }
                let srow = src.get(ty * result.width..(ty + 1) * result.width).unwrap_or(&[]);
                let start = (tile.y0 * s + ty) * ow + tile.x0 * s;
                let Some(drow) = dst.get_mut(start..start + result.width) else { continue };
                for ((d, &v), &wxv) in drow.iter_mut().zip(srow).zip(&wx) {
                    *d += v * wxv * wyv;
                }
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Planar {
        self.out
    }
}

/// Per-pixel weights along one side of a tile that starts at output coordinate `start` and is
/// `len` long, with optional seams before and after (output coordinates), blend zone `o`.
fn weights(start: usize, len: usize, before: Option<usize>, after: Option<usize>, o: f32) -> Vec<f32> {
    (0..len)
        .map(|i| {
            let x = (start + i) as f32 + 0.5;
            let mut w = 1.0;
            if let Some(p) = before {
                w *= ramp(x, p as f32, o);
            }
            if let Some(q) = after {
                w *= 1.0 - ramp(x, q as f32, o);
            }
            w
        })
        .collect()
}

/// 0 well before the seam at `p`, 1 well after, smoothstep across `p - o/2 .. p + o/2`: the
/// middle half of the `p - o .. p + o` context both tiles share. A hard step without overlap.
fn ramp(x: f32, p: f32, o: f32) -> f32 {
    let half = o / 2.0;
    if half <= 0.0 {
        return if x >= p { 1.0 } else { 0.0 };
    }
    let t = ((x - (p - half)) / (2.0 * half)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_sum_to_one_and_cover() {
        for &(w, h, tile, ov) in &[(10, 7, 4, 2), (300, 123, 128, 16), (5, 5, 8, 16), (64, 64, 16, 0), (33, 17, 5, 9)] {
            let p = plan(w, h, TileOptions { tile, overlap: ov }).unwrap();
            for t in &p.tiles {
                assert!(t.x0 < t.x1 && t.x1 <= w && t.y0 < t.y1 && t.y1 <= h);
            }
            for s in [1, 2, 4] {
                let mut b = Blender::new(w, h, 1, s, &p).unwrap();
                for t in &p.tiles {
                    let r = Planar::new(t.width() * s, t.height() * s, 1, vec![1.0; t.width() * t.height() * s * s]).unwrap();
                    b.add(t, &r).unwrap();
                }
                let out = b.finish();
                for v in &out.data {
                    assert!((v - 1.0).abs() < 1e-5, "{w}x{h} tile {tile} ov {ov} s {s}: {v}");
                }
            }
        }
    }

    #[test]
    fn identity_network_reproduces_the_image() {
        let (w, h) = (37, 29);
        let img = Planar::new(w, h, 2, (0..w * h * 2).map(|i| (i % 97) as f32 / 97.0).collect()).unwrap();
        let p = plan(w, h, TileOptions { tile: 10, overlap: 4 }).unwrap();
        assert!(p.tiles.len() > 4);
        let mut b = Blender::new(w, h, 2, 1, &p).unwrap();
        for t in &p.tiles {
            b.add(t, &img.crop(t.x0, t.y0, t.width(), t.height())).unwrap();
        }
        let out = b.finish();
        for (a, b) in out.data.iter().zip(&img.data) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn bad_input_is_an_error() {
        assert!(plan(0, 5, TileOptions::default()).is_err());
        assert!(plan(5, 5, TileOptions { tile: 0, overlap: 0 }).is_err());
        let p = plan(8, 8, TileOptions::default()).unwrap();
        let mut b = Blender::new(8, 8, 1, 2, &p).unwrap();
        assert!(b.add(&p.tiles[0], &Planar::zeros(3, 3, 1).unwrap()).is_err());
    }
}
