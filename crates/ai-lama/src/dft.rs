//! Real 2-D DFT as matrix products: the `rfft2`/`irfft2` pair (orthonormal) of LaMa's Fourier
//! unit, for any feature-map size and on any backend that has a matmul.
//!
//! For an `H×W` real map `X` with `Wc = W/2 + 1` kept frequencies:
//!
//! * [`Dft2::fw`] (`W × 2Wc`) = `[C_W | −S_W] / √W`: `X · fw` gives the real and imaginary
//!   parts `[A | B]` of the row transforms.
//! * [`Dft2::fh`] (`2H × 2H`) = `[[C_H, −S_H], [S_H, C_H]] / √H` acts on the transposed
//!   columns: `[Aᵀ | Bᵀ] · fh = [Reᵀ | Imᵀ]` (`C_H`, `S_H` are symmetric).
//! * [`Dft2::ih`] = `[[C_H, S_H], [−S_H, C_H]] / √H` inverts the column transform.
//! * [`Dft2::iw`] (`2Wc × W`) is the half-spectrum inverse (`irfft`): each kept frequency is
//!   weighted twice except DC and (for even `W`) Nyquist, whose imaginary parts drop out
//!   because their sines are zero.
//!
//! Built in `f64` and rounded once to `f32`; matches `torch.fft.rfftn/irfftn(norm="ortho")` to
//! about 1e-6.

use std::f64::consts::PI;

/// The four matrices for one `H×W` size, row-major `f32`.
#[derive(Debug, Clone, PartialEq)]
pub struct Dft2 {
    /// Feature-map height.
    pub h: usize,
    /// Feature-map width.
    pub w: usize,
    /// `W × 2Wc` forward row transform.
    pub fw: Vec<f32>,
    /// `2H × 2H` forward column transform (on transposed data).
    pub fh: Vec<f32>,
    /// `2H × 2H` inverse column transform (on transposed data).
    pub ih: Vec<f32>,
    /// `2Wc × W` inverse half-spectrum row transform.
    pub iw: Vec<f32>,
}

impl Dft2 {
    /// Kept frequencies along the width.
    pub fn wc(&self) -> usize {
        self.w / 2 + 1
    }

    /// Builds the matrices; `None` when a side is zero or absurdly large (> 4096).
    pub fn new(h: usize, w: usize) -> Option<Self> {
        if h == 0 || w == 0 || h > 4096 || w > 4096 {
            return None;
        }
        let wc = w / 2 + 1;
        let (sw, sh) = ((w as f64).sqrt(), (h as f64).sqrt());
        // Angles reduced modulo the period in integers keep large k·n products exact.
        let ang = |k: usize, n: usize, len: usize| 2.0 * PI * ((k * n) % len) as f64 / len as f64;

        let mut fw = vec![0f32; w * 2 * wc];
        for n in 0..w {
            for k in 0..wc {
                let a = ang(k, n, w);
                fw[n * 2 * wc + k] = (a.cos() / sw) as f32;
                fw[n * 2 * wc + wc + k] = (-a.sin() / sw) as f32;
            }
        }
        let mut iw = vec![0f32; 2 * wc * w];
        for k in 0..wc {
            let weight = if k == 0 || (w.is_multiple_of(2) && k == w / 2) { 1.0 } else { 2.0 };
            for n in 0..w {
                let a = ang(k, n, w);
                iw[k * w + n] = (weight * a.cos() / sw) as f32;
                iw[(wc + k) * w + n] = (-weight * a.sin() / sw) as f32;
            }
        }
        let n2 = 2 * h;
        let mut fh = vec![0f32; n2 * n2];
        let mut ih = vec![0f32; n2 * n2];
        for i in 0..h {
            for j in 0..h {
                let a = ang(i, j, h);
                let (c, s) = ((a.cos() / sh) as f32, (a.sin() / sh) as f32);
                // fh = [[C, -S], [S, C]]
                fh[i * n2 + j] = c;
                fh[i * n2 + h + j] = -s;
                fh[(h + i) * n2 + j] = s;
                fh[(h + i) * n2 + h + j] = c;
                // ih = [[C, S], [-S, C]]
                ih[i * n2 + j] = c;
                ih[i * n2 + h + j] = s;
                ih[(h + i) * n2 + j] = -s;
                ih[(h + i) * n2 + h + j] = c;
            }
        }
        Some(Self { h, w, fw, fh, ih, iw })
    }

    /// Reference forward transform of one `H×W` map on the CPU, in the layout the model uses:
    /// `[re | im]` per column, transposed (`Wc` rows of `2H`). For tests.
    pub fn forward_ref(&self, x: &[f32]) -> Option<Vec<f32>> {
        let (h, w, wc) = (self.h, self.w, self.wc());
        if x.len() != h * w {
            return None;
        }
        let rows = matmul(x, h, w, &self.fw, 2 * wc)?; // h × 2wc
        // transpose to wc × 2h: [A^T | B^T]
        let mut t = vec![0f32; wc * 2 * h];
        for r in 0..h {
            for k in 0..wc {
                t[k * 2 * h + r] = rows[r * 2 * wc + k];
                t[k * 2 * h + h + r] = rows[r * 2 * wc + wc + k];
            }
        }
        matmul(&t, wc, 2 * h, &self.fh, 2 * h)
    }

    /// Inverse of [`Dft2::forward_ref`]. For tests.
    pub fn inverse_ref(&self, spec: &[f32]) -> Option<Vec<f32>> {
        let (h, w, wc) = (self.h, self.w, self.wc());
        if spec.len() != wc * 2 * h {
            return None;
        }
        let g = matmul(spec, wc, 2 * h, &self.ih, 2 * h)?; // wc × 2h = [Gr^T | Gi^T]
        let mut rows = vec![0f32; h * 2 * wc];
        for k in 0..wc {
            for r in 0..h {
                rows[r * 2 * wc + k] = g[k * 2 * h + r];
                rows[r * 2 * wc + wc + k] = g[k * 2 * h + h + r];
            }
        }
        matmul(&rows, h, 2 * wc, &self.iw, w)
    }
}

/// Plain row-major matmul `a (m×k) · b (k×n)`; `None` on a size mismatch.
fn matmul(a: &[f32], m: usize, k: usize, b: &[f32], n: usize) -> Option<Vec<f32>> {
    if a.len() != m * k || b.len() != k * n {
        return None;
    }
    let mut out = vec![0f32; m * n];
    for i in 0..m {
        for p in 0..k {
            let av = a[i * k + p];
            for j in 0..n {
                out[i * n + j] += av * b[p * n + j];
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Direct O(N⁴) orthonormal rfft2, same layout as `forward_ref`.
    fn naive_rfft2(x: &[f32], h: usize, w: usize) -> Vec<f32> {
        let wc = w / 2 + 1;
        let mut out = vec![0f32; wc * 2 * h];
        let norm = ((h * w) as f64).sqrt();
        for kh in 0..h {
            for kw in 0..wc {
                let (mut re, mut im) = (0f64, 0f64);
                for y in 0..h {
                    for xx in 0..w {
                        let a = -2.0 * PI * ((kh * y) as f64 / h as f64 + (kw * xx) as f64 / w as f64);
                        re += x[y * w + xx] as f64 * a.cos();
                        im += x[y * w + xx] as f64 * a.sin();
                    }
                }
                out[kw * 2 * h + kh] = (re / norm) as f32;
                out[kw * 2 * h + h + kh] = (im / norm) as f32;
            }
        }
        out
    }

    fn signal(h: usize, w: usize) -> Vec<f32> {
        (0..h * w).map(|i| ((i * 7919 % 113) as f32 / 113.0 - 0.5) * 2.0).collect()
    }

    #[test]
    fn forward_matches_naive_dft_for_odd_and_even_sizes() {
        for (h, w) in [(4, 4), (5, 7), (8, 6), (3, 9), (16, 13), (1, 1)] {
            let d = Dft2::new(h, w).unwrap();
            let x = signal(h, w);
            let got = d.forward_ref(&x).unwrap();
            let want = naive_rfft2(&x, h, w);
            for (g, e) in got.iter().zip(&want) {
                assert!((g - e).abs() < 1e-4, "{h}x{w}: {g} vs {e}");
            }
        }
    }

    #[test]
    fn inverse_round_trips() {
        for (h, w) in [(8, 8), (5, 7), (6, 10), (64, 64), (37, 50)] {
            let d = Dft2::new(h, w).unwrap();
            let x = signal(h, w);
            let back = d.inverse_ref(&d.forward_ref(&x).unwrap()).unwrap();
            for (a, b) in x.iter().zip(&back) {
                assert!((a - b).abs() < 1e-4, "{h}x{w}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn inverse_ignores_imaginary_dc_and_nyquist_like_irfft() {
        let (h, w) = (4, 6);
        let d = Dft2::new(h, w).unwrap();
        let wc = d.wc();
        let mut spec = vec![0f32; wc * 2 * h];
        // Imaginary part of the DC column (kw = 0) and the Nyquist column (kw = 3), row kh = 0.
        spec[h] = 1.0;
        spec[(wc - 1) * 2 * h + h] = 1.0;
        let out = d.inverse_ref(&spec).unwrap();
        assert!(out.iter().all(|v| v.abs() < 1e-6), "{out:?}");
    }

    #[test]
    fn bad_sizes_are_none() {
        assert!(Dft2::new(0, 4).is_none());
        assert!(Dft2::new(4, 5000).is_none());
        let d = Dft2::new(4, 4).unwrap();
        assert!(d.forward_ref(&[0.0; 3]).is_none());
        assert!(d.inverse_ref(&[0.0; 3]).is_none());
    }
}
