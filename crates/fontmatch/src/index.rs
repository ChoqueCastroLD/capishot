//! Every face's alphabet, drawn once at one letter height ([`crate::CAP`]).

use skrifa::{FontRef, MetadataProvider};

use crate::glyph::{bitmap, size_for_cap};
use crate::{Bitmap, CAP};

/// What's compared: capitals, lower case and digits. Accents and signs add little and would make
/// every query look at more glyphs.
const ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
/// A coarse glyph's side.
pub const GRID: usize = 20;
/// A face needs this many glyphs to count: fewer is a symbol font.
const MIN_GLYPHS: usize = 30;

/// A face to index.
#[derive(Clone, Copy, Debug)]
pub struct FaceSource<'a> {
    pub family: &'a str,
    pub style: &'a str,
    pub data: &'a [u8],
    pub index: u32,
}

/// One glyph of one face.
#[derive(Clone, Debug)]
pub struct Glyph {
    pub ch: char,
    pub bitmap: Bitmap,
}

#[derive(Clone, Debug)]
pub struct Face {
    pub family: String,
    pub style: String,
    /// The font size in pixels whose capitals are [`CAP`] tall.
    pub size_for_cap: f32,
    pub glyphs: Vec<Glyph>,
    /// Hash of the capitals' shapes: faces that share Latin letters count as one look.
    pub look: u64,
}

/// Every face, and each glyph's coarse form in one array to scan.
#[derive(Clone, Debug, Default)]
pub struct Library {
    pub faces: Vec<Face>,
    /// [`GRID`]² values for each glyph.
    pub(crate) coarse: Vec<u8>,
    /// The glyph's face.
    pub(crate) owner: Vec<u32>,
    /// The glyph's width and height, over [`CAP`].
    pub(crate) sizes: Vec<[f32; 2]>,
    /// Each face's first glyph in the scan order, and the glyph behind it.
    pub(crate) glyph_of: Vec<(u32, u32)>,
}

/// `b` stretched to the grid, each cell its ink share in 0..=255.
pub(crate) fn coarse(b: &Bitmap, out: &mut Vec<u8>) {
    for gy in 0..GRID {
        let (y0, y1) = (gy * b.height / GRID, ((gy + 1) * b.height).div_ceil(GRID).max(gy * b.height / GRID + 1));
        for gx in 0..GRID {
            let (x0, x1) = (gx * b.width / GRID, ((gx + 1) * b.width).div_ceil(GRID).max(gx * b.width / GRID + 1));
            let mut on = 0u32;
            for y in y0..y1.min(b.height) {
                for x in x0..x1.min(b.width) {
                    on += u32::from(b.get(x, y));
                }
            }
            let n = ((y1.min(b.height) - y0) * (x1.min(b.width) - x0)).max(1) as u32;
            out.push((on * 255 / n) as u8);
        }
    }
}

fn hash(bytes: impl Iterator<Item = u8>) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

impl Library {
    /// The faces' alphabets; what can't be read is left out. `progress` gets 0..=1.
    pub fn build(sources: &[FaceSource<'_>], progress: &mut dyn FnMut(f32)) -> Library {
        let mut lib = Library::default();
        for (i, s) in sources.iter().enumerate() {
            if i % 16 == 0 {
                progress(i as f32 / sources.len().max(1) as f32);
            }
            lib.add(s);
        }
        lib
    }

    fn add(&mut self, s: &FaceSource<'_>) {
        let Ok(font) = FontRef::from_index(s.data, s.index) else { return };
        let Some(size) = size_for_cap(&font, CAP as f32) else { return };
        let map = font.charmap();
        let glyphs: Vec<Glyph> = ALPHABET
            .chars()
            .filter_map(|ch| {
                let id = map.map(ch)?;
                let b = bitmap(&font, id, size)?;
                (b.area() >= 6).then_some(Glyph { ch, bitmap: b })
            })
            .collect();
        if glyphs.len() < MIN_GLYPHS {
            return;
        }
        // The same box for every letter: no alphabet here.
        let mut shapes: Vec<&[bool]> = glyphs.iter().map(|g| g.bitmap.data.as_slice()).collect();
        shapes.sort_unstable();
        shapes.dedup();
        if shapes.len() * 10 < glyphs.len() * 7 {
            return;
        }
        let face = self.faces.len() as u32;
        for (k, g) in glyphs.iter().enumerate() {
            coarse(&g.bitmap, &mut self.coarse);
            self.owner.push(face);
            self.sizes.push([g.bitmap.width as f32 / CAP as f32, g.bitmap.height as f32 / CAP as f32]);
            self.glyph_of.push((face, k as u32));
        }
        let look = hash(glyphs.iter().filter(|g| g.ch.is_ascii_uppercase()).flat_map(|g| {
            let mut c = Vec::new();
            coarse(&g.bitmap, &mut c);
            // Rounded: drawings that differ by a pixel are one look.
            c.into_iter().map(|v| v / 32)
        }));
        self.faces.push(Face { family: s.family.to_string(), style: s.style.to_string(), size_for_cap: size, glyphs, look });
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }
}
