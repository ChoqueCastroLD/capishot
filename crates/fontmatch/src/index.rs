//! Every face's alphabet, drawn once at one letter height ([`crate::CAP`]).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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

/// A glyph's drawing at a bit a pixel: a library holds thousands of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packed {
    pub width: usize,
    pub height: usize,
    bits: Box<[u64]>,
}

impl Packed {
    fn new(b: &Bitmap) -> Packed {
        let mut bits = vec![0u64; b.data.len().div_ceil(64)];
        for (i, _) in b.data.iter().enumerate().filter(|(_, on)| **on) {
            bits[i / 64] |= 1 << (i % 64);
        }
        Packed { width: b.width, height: b.height, bits: bits.into() }
    }

    pub fn unpack(&self) -> Bitmap {
        let mut b = Bitmap::new(self.width, self.height);
        for (i, on) in b.data.iter_mut().enumerate() {
            *on = self.bits[i / 64] >> (i % 64) & 1 == 1;
        }
        b
    }
}

/// One glyph of one face.
#[derive(Clone, Debug)]
pub struct Glyph {
    pub ch: char,
    pub bitmap: Packed,
}

#[derive(Clone, Debug)]
pub struct Face {
    pub family: String,
    pub style: String,
    /// The font size in pixels whose capitals are [`CAP`] tall.
    pub size_for_cap: f32,
    pub glyphs: Arc<[Glyph]>,
    /// Hash of the capitals' shapes: faces that share Latin letters count as one look.
    pub look: u64,
    /// The capitals' coarse forms, to tell near copies apart.
    pub(crate) capitals: Arc<[u8]>,
    /// Which file's face this is: its name and index, and the file's size and ends.
    pub key: u64,
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

/// The mean difference of two faces' capitals, in 0..=255, below which they're copies of one design.
const NEAR_COPY: u64 = 9;

impl Face {
    /// Another drawing of nearly the same letters.
    pub fn near_copy(&self, other: &Face) -> bool {
        if self.look == other.look {
            return true;
        }
        if self.capitals.len() != other.capitals.len() || self.capitals.is_empty() {
            return false;
        }
        let sum: u64 = self.capitals.iter().zip(other.capitals.iter()).map(|(a, b)| u64::from(a.abs_diff(*b))).sum();
        sum / self.capitals.len() as u64 <= NEAR_COPY
    }

    /// The family without its weights, slopes and version marks: `CC Wild Words Int Italic` is
    /// `wild words`. Faces with one such name are one choice.
    pub fn base_name(&self) -> String {
        const MARKS: [&str; 26] = [
            "regular", "bold", "italic", "oblique", "light", "medium", "semibold", "demibold", "extrabold", "black", "heavy", "thin", "book", "roman", "int", "cc", "bb", "dk", "lower", "upper", "new", "pro", "std", "mt", "w01", "w00",
        ];
        let lower = self.family.to_lowercase().replace(['-', '_', '.'], " ");
        let words: Vec<&str> = lower.split_whitespace().filter(|w| !MARKS.contains(w) && !w.chars().all(|c| c.is_ascii_digit())).collect();
        if words.is_empty() { lower.trim().to_string() } else { words.join(" ") }
    }
}

/// Which face of which file: reading a file's ends is enough to know it hasn't changed.
fn key_of(s: &FaceSource<'_>) -> u64 {
    const END: usize = 4096;
    let (head, tail) = (&s.data[..s.data.len().min(END)], &s.data[s.data.len().saturating_sub(END)..]);
    let sizes = [u64::from(s.index), s.data.len() as u64].map(u64::to_le_bytes);
    hash(s.family.bytes().chain([0]).chain(s.style.bytes()).chain([0]).chain(sizes.into_iter().flatten()).chain(head.iter().copied()).chain(tail.iter().copied()))
}

impl Face {
    /// The face's alphabet; `None` when it can't be read or has no alphabet to compare.
    pub fn read(s: &FaceSource<'_>) -> Option<Face> {
        let font = FontRef::from_index(s.data, s.index).ok()?;
        let size = size_for_cap(&font, CAP as f32)?;
        let map = font.charmap();
        let drawn: Vec<(char, Bitmap)> = ALPHABET
            .chars()
            .filter_map(|ch| {
                let id = map.map(ch)?;
                let b = bitmap(&font, id, size)?;
                (b.area() >= 6).then_some((ch, b))
            })
            .collect();
        if drawn.len() < MIN_GLYPHS {
            return None;
        }
        // The same box for every letter: no alphabet here.
        let mut shapes: Vec<&[bool]> = drawn.iter().map(|(_, b)| b.data.as_slice()).collect();
        shapes.sort_unstable();
        shapes.dedup();
        if shapes.len() * 10 < drawn.len() * 7 {
            return None;
        }
        let mut capitals = Vec::new();
        for (_, b) in drawn.iter().filter(|(ch, _)| ch.is_ascii_uppercase()) {
            coarse(b, &mut capitals);
        }
        // Rounded: drawings that differ by a pixel are one look.
        let look = hash(capitals.iter().map(|v| v / 32));
        let glyphs = drawn.iter().map(|(ch, b)| Glyph { ch: *ch, bitmap: Packed::new(b) }).collect();
        Some(Face { family: s.family.to_string(), style: s.style.to_string(), size_for_cap: size, glyphs, look, capitals: capitals.into(), key: key_of(s) })
    }
}

impl Library {
    /// The faces' alphabets; what can't be read is left out. `progress` gets 0..=1.
    pub fn build(sources: &[FaceSource<'_>], progress: &mut dyn FnMut(f32)) -> Library {
        Library::build_from(sources, None, &[], progress)
    }

    /// [`Library::build`] that takes from `known` the faces it already drew, and adds `extra`:
    /// faces read from files that aren't among the sources. A source's face wins over an extra one
    /// of the same name.
    pub fn build_from(sources: &[FaceSource<'_>], known: Option<&Library>, extra: &[Face], progress: &mut dyn FnMut(f32)) -> Library {
        let known: HashMap<u64, &Face> = known.map(|l| l.faces.iter().map(|f| (f.key, f)).collect()).unwrap_or_default();
        let mut faces: Vec<Face> = Vec::with_capacity(sources.len() + extra.len());
        let mut names: HashSet<(&str, &str)> = HashSet::with_capacity(sources.len());
        for (i, s) in sources.iter().enumerate() {
            if i % 16 == 0 {
                progress(i as f32 / sources.len().max(1) as f32);
            }
            names.insert((s.family, s.style));
            faces.extend(known.get(&key_of(s)).map(|f| (*f).clone()).or_else(|| Face::read(s)));
        }
        faces.extend(extra.iter().filter(|f| !names.contains(&(f.family.as_str(), f.style.as_str()))).cloned());
        // The order breaks ties: the same fonts, however they arrived, give the same answer.
        faces.sort_by(|a, b| (&a.family, &a.style, a.key).cmp(&(&b.family, &b.style, b.key)));
        let mut lib = Library::default();
        for face in faces {
            lib.push(face);
        }
        lib
    }

    fn push(&mut self, face: Face) {
        let at = self.faces.len() as u32;
        for (k, g) in face.glyphs.iter().enumerate() {
            coarse(&g.bitmap.unpack(), &mut self.coarse);
            self.owner.push(at);
            self.sizes.push([g.bitmap.width as f32 / CAP as f32, g.bitmap.height as f32 / CAP as f32]);
            self.glyph_of.push((at, k as u32));
        }
        self.faces.push(face);
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }
}
