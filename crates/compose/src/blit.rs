//! 8-bit blits for interactive moves: a cached image laid over a cached backdrop, and a cached
//! affine map of the layers above laid over that ([`crate::apply_affine`]'s arithmetic in 8 bits).
//! Rows of premultiplied RGBA8, in plain integer loops the compiler vectorises.

/// `(a * b + 127) / 255`, exact for 8-bit operands.
#[inline(always)]
fn mul255(a: u32, b: u32) -> u32 {
    let x = a * b + 128;
    (x + (x >> 8)) >> 8
}

/// `out = sprite over below` (both premultiplied): `s + b * (1 - s.a)`.
pub fn over_row(out: &mut [[u8; 4]], below: &[[u8; 4]], sprite: &[[u8; 4]]) {
    for ((o, b), s) in out.iter_mut().zip(below).zip(sprite) {
        let inv = 255 - u32::from(s[3]);
        for c in 0..4 {
            o[c] = (u32::from(s[c]) + mul255(u32::from(b[c]), inv)).min(255) as u8;
        }
    }
}

/// Lay the layers above, cached as their composite over opaque black (`k`) and over opaque white
/// (`w`), over `row` in place: `k + (w - k) * c` per channel. Every pixel of `row` must be opaque
/// (the map only holds there); returns `false`, leaving `row` as it was, when one isn't.
pub fn affine_row(row: &mut [[u8; 4]], k: &[[u8; 4]], w: &[[u8; 4]]) -> bool {
    if row.iter().any(|p| p[3] != 255) {
        return false;
    }
    for ((p, k), w) in row.iter_mut().zip(k).zip(w) {
        for c in 0..3 {
            let (kc, wc, v) = (i32::from(k[c]), i32::from(w[c]), i32::from(p[c]));
            let d = (wc - kc) * v;
            let d = if d >= 0 { (d + 127) / 255 } else { (d - 127) / 255 };
            p[c] = (kc + d).clamp(0, 255) as u8;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn over_and_affine_match_the_float_arithmetic() {
        let below = [[200u8, 100, 50, 255], [10, 20, 30, 255], [0, 0, 0, 0]];
        let sprite = [[0u8, 0, 0, 0], [128, 64, 0, 128], [255, 0, 0, 255]];
        let mut out = [[0u8; 4]; 3];
        over_row(&mut out, &below, &sprite);
        assert_eq!(out[0], below[0], "nothing over it");
        assert_eq!(out[1], [133, 74, 15, 255], "half over");
        assert_eq!(out[2], [255, 0, 0, 255], "opaque over nothing");
        // Above: a Normal layer of 50% grey at 50%: k = 64, w = 191 (over black / white).
        let k = [[64u8, 64, 64, 255]; 3];
        let w = [[191u8, 191, 191, 255]; 3];
        let mut row = [[0u8, 128, 255, 255]; 3];
        assert!(affine_row(&mut row, &k, &w));
        assert_eq!(row[0], [64, 128, 191, 255]);
        let mut transparent = [[0u8, 0, 0, 0]];
        assert!(!affine_row(&mut transparent, &k[..1], &w[..1]), "not opaque: left alone");
        assert_eq!(transparent[0], [0, 0, 0, 0]);
    }
}
