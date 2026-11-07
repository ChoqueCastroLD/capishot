//! Resizing for the network, which works on pages of a certain height, and back.

/// `rgb` resized to `(nw, nh)`: an area average going down, Catmull-Rom going up.
pub fn rgb(src: &[u8], w: usize, h: usize, nw: usize, nh: usize) -> Vec<u8> {
    let mut out = vec![0u8; nw * nh * 3];
    let xs = weights(w, nw);
    let ys = weights(h, nh);
    let mut row = vec![0f32; nw * 3];
    let mut lines: Vec<Vec<f32>> = Vec::with_capacity(h);
    for y in 0..h {
        for (nx, ws) in xs.iter().enumerate() {
            for c in 0..3 {
                row[nx * 3 + c] = ws.iter().map(|(x, k)| f32::from(src[(y * w + x) * 3 + c]) * k).sum();
            }
        }
        lines.push(row.clone());
    }
    for (ny, ws) in ys.iter().enumerate() {
        for v in 0..nw * 3 {
            let s: f32 = ws.iter().map(|(y, k)| lines[*y][v] * k).sum();
            out[ny * nw * 3 + v] = s.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Each new pixel's sources and weights along an axis.
fn weights(from: usize, to: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = from as f32 / to as f32;
    (0..to)
        .map(|i| {
            let mut w: Vec<(usize, f32)> = if scale >= 1.0 {
                let (a, b) = (i as f32 * scale, (i + 1) as f32 * scale);
                (a.floor() as usize..(b.ceil() as usize).min(from)).map(|s| (s, ((s + 1) as f32).min(b) - (s as f32).max(a))).collect()
            } else {
                let c = (i as f32 + 0.5) * scale - 0.5;
                let base = c.floor() as i64;
                (base - 1..=base + 2)
                    .map(|s| {
                        let t = (c - s as f32).abs();
                        let k = if t < 1.0 { 1.5 * t * t * t - 2.5 * t * t + 1.0 } else { -0.5 * t * t * t + 2.5 * t * t - 4.0 * t + 2.0 };
                        (s.clamp(0, from as i64 - 1) as usize, k)
                    })
                    .collect()
            };
            let sum: f32 = w.iter().map(|v| v.1).sum();
            for v in &mut w {
                v.1 /= sum;
            }
            w
        })
        .collect()
}

/// A probability map back to the page's size, bilinear.
pub fn plane(src: &[f32], w: usize, h: usize, nw: usize, nh: usize) -> Vec<f32> {
    let mut out = vec![0f32; nw * nh];
    let (sx, sy) = (w as f32 / nw as f32, h as f32 / nh as f32);
    for ny in 0..nh {
        let fy = ((ny as f32 + 0.5) * sy - 0.5).clamp(0.0, (h - 1) as f32);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        let y1 = (y0 + 1).min(h - 1);
        for nx in 0..nw {
            let fx = ((nx as f32 + 0.5) * sx - 0.5).clamp(0.0, (w - 1) as f32);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let x1 = (x0 + 1).min(w - 1);
            let top = src[y0 * w + x0] * (1.0 - tx) + src[y0 * w + x1] * tx;
            let bottom = src[y1 * w + x0] * (1.0 - tx) + src[y1 * w + x1] * tx;
            out[ny * nw + nx] = top * (1.0 - ty) + bottom * ty;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_stays_flat_both_ways() {
        let src = vec![200u8; 8 * 8 * 3];
        assert!(rgb(&src, 8, 8, 4, 4).iter().all(|v| *v == 200));
        assert!(rgb(&src, 8, 8, 16, 16).iter().all(|v| *v == 200));
        assert!(plane(&vec![0.5; 16], 4, 4, 9, 7).iter().all(|v| (*v - 0.5).abs() < 1e-5));
    }
}
