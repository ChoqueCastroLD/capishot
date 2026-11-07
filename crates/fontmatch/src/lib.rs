//! Finds the font of lettering on a page by its shapes alone: no model, no OCR, the same zone gives
//! the same answer.
//!
//! [`index`] draws an alphabet of every face once. A query ([`find`]) takes the zone's letters
//! ([`segment`]), compares each with every glyph of every face on a small grid, so it doesn't need to
//! know the words, and compares the closest faces again by their edges ([`matcher`]).
//! [`look`] measures what else the lettering has: fill, outline, tilt and size.

pub mod glyph;
pub mod index;
pub mod look;
pub mod matcher;
pub mod pack;
pub mod segment;

pub use index::{Face, FaceSource, Library};
pub use look::Look;
pub use matcher::{Candidate, Scope, find, find_in};

/// A zone of a page: RGB, interleaved.
#[derive(Clone, Copy, Debug)]
pub struct Zone<'a> {
    pub rgb: &'a [u8],
    pub width: usize,
    pub height: usize,
}

/// A one-bit image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub data: Vec<bool>,
}

impl Bitmap {
    pub fn new(width: usize, height: usize) -> Self {
        Bitmap { width, height, data: vec![false; width * height] }
    }

    pub fn get(&self, x: usize, y: usize) -> bool {
        self.data[y * self.width + x]
    }

    pub fn area(&self) -> usize {
        self.data.iter().filter(|v| **v).count()
    }
}

/// The letter height everything is compared at, in pixels.
pub const CAP: usize = 56;
