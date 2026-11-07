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
    Look { fill, ground, outline, tilt: found.tilt, cap: found.cap, size: found.cap }
}
