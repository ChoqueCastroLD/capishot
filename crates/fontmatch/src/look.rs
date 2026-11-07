//! What the lettering looks like besides its face: fill, outline, tilt and size.

use crate::segment::{Letters, gray};
use crate::{Bitmap, CAP, Zone};

/// The look measured from the letters.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    /// The letters' colour, RGB.
    pub fill: [u8; 3],
    /// The ground's colour, RGB.
    pub ground: [u8; 3],
    /// An outline's colour and width in pixels.
    pub outline: Option<([u8; 3], f32)>,
    /// The fill from the letters' top to their bottom, when it changes.
    pub gradient: Option<([u8; 3], [u8; 3])>,
    /// A hard shadow: its colour and where it falls from the letters, in pixels, y down.
    pub shadow: Option<([u8; 3], [f32; 2])>,
    /// The zone held other lettering too; this is the largest.
    pub mixed: bool,
    /// Degrees the lettering is turned, positive counter-clockwise.
    pub tilt: f32,
    /// The letters' height, in pixels.
    pub cap: f32,
    /// The font size in pixels that draws this face at that height.
    pub size: f32,
}

impl Look {
    pub(crate) fn for_face(&self, size_for_cap: f32) -> Look {
        Look { size: size_for_cap * self.cap / CAP as f32, ..self.clone() }
    }
}

/// The median colour of the pixels `pick` chooses.
fn median(z: &Zone<'_>, pick: impl Fn(usize) -> bool) -> Option<[u8; 3]> {
    let mut c: [Vec<u8>; 3] = Default::default();
    for (i, px) in z.rgb.chunks_exact(3).enumerate() {
        if pick(i) {
            for k in 0..3 {
                c[k].push(px[k]);
            }
        }
    }
    if c[0].is_empty() {
        return None;
    }
    let mut out = [0u8; 3];
    for k in 0..3 {
        c[k].sort_unstable();
        out[k] = c[k][c[k].len() / 2];
    }
    Some(out)
}

/// A fill's change this large, summed over the channels, is a gradient and not noise.
const GRADIENT: u32 = 60;

/// The letters' colour in the top and the bottom third of each, when they differ.
fn gradient_of(z: &Zone<'_>, found: &Letters) -> Option<([u8; 3], [u8; 3])> {
    let ink = &found.ink;
    let (labels, comps) = crate::segment::components(ink, true);
    let third = |i: usize, top: bool| -> bool {
        let l = labels[i];
        if l == 0 {
            return false;
        }
        let c = &comps[l as usize - 1];
        let (x, y) = (i % ink.width, i / ink.width);
        // Off the edge, and in the letters of full height.
        let h = c.y1 - c.y0;
        if (h as f32) < found.cap * 0.8 || x == 0 || y == 0 || x + 1 >= ink.width || y + 1 >= ink.height || !ink.get(x - 1, y) || !ink.get(x + 1, y) || !ink.get(x, y - 1) || !ink.get(x, y + 1) {
            return false;
        }
        if top { y < c.y0 + h / 3 } else { y >= c.y1 - h / 3 }
    };
    let (a, b) = (median(z, |i| third(i, true))?, median(z, |i| third(i, false))?);
    let change: u32 = (0..3).map(|k| u32::from(a[k].abs_diff(b[k]))).sum();
    (change >= GRADIENT).then_some((a, b))
}

/// Colours closer than this, summed over the channels, are one.
const SAME_COLOUR: u32 = 70;

/// A hard shadow: walking away from the letters' fill, in some direction a third colour lasts longer
/// than the opposite way. An outline lasts the same both ways, which is why this looks at the
/// difference and takes the outline's width off.
fn shadow_of(z: &Zone<'_>, fill: [u8; 3], ground: [u8; 3], cap: f32) -> Option<([u8; 3], [f32; 2])> {
    let (w, h) = (z.width, z.height);
    let near = |i: usize, c: [u8; 3]| -> bool { (0..3).map(|k| u32::from(z.rgb[i * 3 + k].abs_diff(c[k]))).sum::<u32>() < SAME_COLOUR };
    if (0..3).map(|k| u32::from(fill[k].abs_diff(ground[k]))).sum::<u32>() < SAME_COLOUR {
        return None;
    }
    const DIRS: [(i32, i32); 8] = [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)];
    let limit = (cap / 2.0).max(4.0) as i32;
    let mut sum = [0f32; 8];
    let mut count = [0u32; 8];
    let mut third: Vec<[u8; 3]> = Vec::new();
    for y in 1..h as i32 - 1 {
        for x in 1..w as i32 - 1 {
            let i = y as usize * w + x as usize;
            if !near(i, fill) {
                continue;
            }
            for (d, (dx, dy)) in DIRS.iter().enumerate() {
                let next = ((y + dy) as usize) * w + (x + dx) as usize;
                if near(next, fill) {
                    continue;
                }
                // An edge of the fill, facing this way: how far until the ground?
                let mut run = 0;
                let (mut px, mut py) = (x + dx, y + dy);
                while run < limit && px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                    let j = py as usize * w + px as usize;
                    if near(j, ground) || near(j, fill) {
                        break;
                    }
                    if run == 1 {
                        third.push([z.rgb[j * 3], z.rgb[j * 3 + 1], z.rgb[j * 3 + 2]]);
                    }
                    run += 1;
                    px += dx;
                    py += dy;
                }
                sum[d] += run as f32;
                count[d] += 1;
            }
        }
    }
    if count.iter().any(|c| *c == 0) || third.is_empty() {
        return None;
    }
    let mean: Vec<f32> = sum.iter().zip(&count).map(|(s, c)| s / *c as f32).collect();
    // The offset, as the imbalance of opposite directions.
    let (mut ox, mut oy) = (0f32, 0f32);
    for k in 0..4 {
        let d = (mean[k] - mean[k + 4]) / 2.0;
        let (dx, dy) = DIRS[k];
        let norm = ((dx * dx + dy * dy) as f32).sqrt();
        ox += d * dx as f32 / norm;
        oy += d * dy as f32 / norm;
    }
    (ox, oy) = (ox / 2.0, oy / 2.0);
    if ox.hypot(oy) < 1.0_f32.max(cap / 40.0) {
        return None;
    }
    let mut c: [Vec<u8>; 3] = Default::default();
    for t in &third {
        for k in 0..3 {
            c[k].push(t[k]);
        }
    }
    let mut colour = [0u8; 3];
    for k in 0..3 {
        c[k].sort_unstable();
        colour[k] = c[k][c[k].len() / 2];
    }
    Some((colour, [ox, oy]))
}

pub(crate) fn measure(z: &Zone<'_>, found: &Letters) -> Look {
    let ink: &Bitmap = &found.ink;
    let lum = gray(z);
    // Off the edges of the letters, where antialiasing and JPEG mix colours.
    let inside = |i: usize| {
        let (x, y) = (i % ink.width, i / ink.width);
        ink.data[i] && x > 0 && y > 0 && x + 1 < ink.width && y + 1 < ink.height && ink.get(x - 1, y) && ink.get(x + 1, y) && ink.get(x, y - 1) && ink.get(x, y + 1)
    };
    let fill = median(z, inside).or_else(|| median(z, |i| ink.data[i])).unwrap_or([0, 0, 0]);
    let fill_lum = i32::from(((u32::from(fill[0]) * 77 + u32::from(fill[1]) * 150 + u32::from(fill[2]) * 29) >> 8) as u8);
    let outline = found.ring.map(|width| {
        // The ring: right around the bodies, and clearly not their colour.
        let outside = Bitmap { width: ink.width, height: ink.height, data: ink.data.iter().map(|v| !*v).collect() };
        let far = crate::segment::distance(&outside);
        let near = |i: usize| !ink.data[i] && far[i] <= width && (i32::from(lum[i]) - fill_lum).abs() > 60;
        (median(z, near).unwrap_or([0, 0, 0]), width)
    });
    let ground = median(z, |i| {
        let (x, y) = (i % ink.width, i / ink.width);
        !ink.data[i] && (x < 2 || y < 2 || x + 2 >= ink.width || y + 2 >= ink.height)
    })
    .unwrap_or([255, 255, 255]);
    let shadow = shadow_of(z, fill, ground, found.cap);
    Look { fill, ground, outline, gradient: gradient_of(z, found), shadow, mixed: found.mixed, tilt: found.tilt, cap: found.cap, size: found.cap }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::segment::letters;

    /// White bars on black, each with a grey copy at `(ox, oy)`, drawn under them.
    fn page(ox: usize, oy: usize) -> (Vec<u8>, usize, usize) {
        let (w, h) = (240, 90);
        let mut rgb = vec![0u8; w * h * 3];
        let mut put = |x0: usize, y0: usize, c: u8| {
            for y in y0..y0 + 40 {
                for x in x0..x0 + 16 {
                    rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(c);
                }
            }
        };
        for k in 0..5 {
            if ox + oy > 0 {
                put(20 + k * 40 + ox, 20 + oy, 120);
            }
        }
        for k in 0..5 {
            put(20 + k * 40, 20, 255);
        }
        (rgb, w, h)
    }

    #[test]
    fn a_hard_shadow_is_measured_with_where_it_falls() {
        let (rgb, width, height) = page(4, 4);
        let z = Zone { rgb: &rgb, width, height };
        let (colour, at) = shadow_of(&z, [255, 255, 255], [0, 0, 0], 40.0).unwrap();
        assert!((i32::from(colour[0]) - 120).abs() < 20, "{colour:?}");
        assert!(at[0] > 1.0 && at[1] > 1.0, "down and to the right: {at:?}");
        let found = letters(&z).unwrap();
        assert!(!found.letters.is_empty());
    }

    #[test]
    fn plain_letters_have_no_shadow() {
        let (rgb, width, height) = page(0, 0);
        assert!(shadow_of(&Zone { rgb: &rgb, width, height }, [255, 255, 255], [0, 0, 0], 40.0).is_none());
    }
}
