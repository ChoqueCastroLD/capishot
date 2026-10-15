//! Planar f32 images: what networks consume and produce.

use crate::{AiError, Result};

/// Largest image side accepted (input or output), in pixels.
pub const MAX_SIDE: usize = 65_536;
/// Largest sample count (`channels × width × height`) of one image: 4 GiB of f32.
pub const MAX_SAMPLES: usize = 1 << 30;

/// A planar (CHW) image of f32 samples, nominally `0.0..=1.0`. Channel `c`, row `y`, column `x`
/// is at `c * width * height + y * width + x`.
#[derive(Clone, Debug, PartialEq)]
pub struct Planar {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub data: Vec<f32>,
}

impl Planar {
    /// Checks the sizes against the data.
    pub fn new(width: usize, height: usize, channels: usize, data: Vec<f32>) -> Result<Self> {
        let n = samples(width, height, channels)?;
        if data.len() != n {
            return Err(AiError::Image(format!("{} samples for {width}×{height}×{channels}", data.len())));
        }
        Ok(Self { width, height, channels, data })
    }

    /// A black image.
    pub fn zeros(width: usize, height: usize, channels: usize) -> Result<Self> {
        let n = samples(width, height, channels)?;
        Ok(Self { width, height, channels, data: vec![0.0; n] })
    }

    /// From interleaved 8-bit samples (`channels` per pixel), scaled to `0..=1`.
    pub fn from_interleaved_u8(width: usize, height: usize, channels: usize, px: &[u8]) -> Result<Self> {
        let n = samples(width, height, channels)?;
        if px.len() != n {
            return Err(AiError::Image(format!("{} bytes for {width}×{height}×{channels}", px.len())));
        }
        let plane = width * height;
        let mut data = vec![0.0; n];
        for (i, p) in px.chunks_exact(channels).enumerate() {
            for (c, v) in p.iter().enumerate() {
                if let Some(d) = data.get_mut(c * plane + i) {
                    *d = f32::from(*v) / 255.0;
                }
            }
        }
        Ok(Self { width, height, channels, data })
    }

    /// To interleaved 8-bit samples, clamped and rounded.
    pub fn to_interleaved_u8(&self) -> Vec<u8> {
        let plane = self.width * self.height;
        let mut out = vec![0u8; self.data.len()];
        for (i, o) in out.chunks_exact_mut(self.channels.max(1)).enumerate() {
            for (c, v) in o.iter_mut().enumerate() {
                *v = to_u8(self.data.get(c * plane + i).copied().unwrap_or(0.0));
            }
        }
        out
    }

    /// One channel's samples.
    pub fn plane(&self, c: usize) -> &[f32] {
        let n = self.width * self.height;
        self.data.get(c * n..(c + 1) * n).unwrap_or(&[])
    }

    /// One channel's samples, mutable.
    pub fn plane_mut(&mut self, c: usize) -> &mut [f32] {
        let n = self.width * self.height;
        self.data.get_mut(c * n..(c + 1) * n).unwrap_or(&mut [])
    }

    /// The rectangle `x..x+w, y..y+h` (clipped to the image) as a new image.
    pub fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> Planar {
        let x1 = x.saturating_add(w).min(self.width);
        let y1 = y.saturating_add(h).min(self.height);
        let (x, y) = (x.min(x1), y.min(y1));
        let (w, h) = (x1 - x, y1 - y);
        let mut data = Vec::with_capacity(w * h * self.channels);
        for c in 0..self.channels {
            let p = self.plane(c);
            for row in y..y1 {
                data.extend_from_slice(p.get(row * self.width + x..row * self.width + x1).unwrap_or(&[]));
            }
        }
        Planar { width: w, height: h, channels: self.channels, data }
    }

    /// The same pixels with `n` channels: one channel is repeated, or the first `n` are kept.
    pub fn with_channels(&self, n: usize) -> Planar {
        let plane = self.width * self.height;
        let mut data = Vec::with_capacity(plane * n);
        for c in 0..n {
            let src = if self.channels == 1 { 0 } else { c.min(self.channels.saturating_sub(1)) };
            data.extend_from_slice(self.plane(src));
        }
        Planar { width: self.width, height: self.height, channels: n, data }
    }
}

fn samples(width: usize, height: usize, channels: usize) -> Result<usize> {
    if width == 0 || height == 0 || channels == 0 {
        return Err(AiError::Image(format!("empty image {width}×{height}×{channels}")));
    }
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(AiError::Image(format!("{width}×{height} is larger than {MAX_SIDE} pixels a side")));
    }
    width
        .checked_mul(height)
        .and_then(|p| p.checked_mul(channels))
        .filter(|&n| n <= MAX_SAMPLES)
        .ok_or_else(|| AiError::Image(format!("{width}×{height}×{channels} is too large")))
}

/// `0..=1` to a byte, rounding; NaN is 0.
pub fn to_u8(v: f32) -> u8 {
    if v.is_nan() {
        return 0;
    }
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaved_round_trip() {
        let px: Vec<u8> = (0..24).map(|i| (i * 10) as u8).collect();
        let p = Planar::from_interleaved_u8(4, 2, 3, &px).unwrap();
        assert_eq!(p.plane(1)[0], 10.0 / 255.0);
        assert_eq!(p.to_interleaved_u8(), px);
    }

    #[test]
    fn rejects_bad_sizes() {
        assert!(Planar::new(0, 1, 1, vec![]).is_err());
        assert!(Planar::new(2, 2, 1, vec![0.0; 3]).is_err());
        assert!(Planar::zeros(MAX_SIDE + 1, 1, 1).is_err());
        assert!(Planar::zeros(MAX_SIDE, MAX_SIDE, 3).is_err());
        assert!(Planar::from_interleaved_u8(2, 2, 3, &[0; 5]).is_err());
    }

    #[test]
    fn crop_and_channels() {
        let p = Planar::new(3, 2, 1, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]).unwrap();
        let c = p.crop(1, 1, 5, 5);
        assert_eq!((c.width, c.height, c.data.clone()), (2, 1, vec![4.0, 5.0]));
        let rgb = c.with_channels(3);
        assert_eq!(rgb.data, vec![4.0, 5.0, 4.0, 5.0, 4.0, 5.0]);
        assert_eq!(to_u8(f32::NAN), 0);
        assert_eq!(to_u8(2.0), 255);
    }
}
