//! Sparse, copy-on-write tiled surfaces in any [`PixelFormat`].
//!
//! A [`Surface`] is an infinite plane of 256×256 tiles. Missing tiles read as the surface's
//! default pixel (transparent for layers, white for "reveal all" masks). Tiles are `Arc`-shared,
//! so cloning a surface is O(tiles) and mutation copies only the touched tiles. That makes
//! undo snapshots, background jobs and autosave cheap.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod interrupt;
pub use interrupt::{Cancelled, Interrupt};

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use photocraft_color::{ColorMode, PixelFormat, SampleType, read_sample, write_sample};
use photocraft_geom::{Rect, TILE_SIZE, TileCoord};

/// A checked surface allocation could not be represented or reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocationError {
    SizeOverflow,
    NotEnoughMemory { bytes: usize },
}

impl fmt::Display for AllocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SizeOverflow => f.write_str("requested surface allocation is too large"),
            Self::NotEnoughMemory { bytes } => write!(f, "not enough memory to allocate {} bytes", bytes),
        }
    }
}

impl std::error::Error for AllocationError {}

/// Pixel storage for one tile: `TILE_SIZE² × bytes_per_pixel`, row-major, interleaved channels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tile {
    data: Box<[u8]>,
}

impl Tile {
    fn try_filled(format: &PixelFormat, pixel: &[u8]) -> Result<Self, AllocationError> {
        let n = (TILE_SIZE * TILE_SIZE) as usize;
        let bytes = n.checked_mul(format.bytes_per_pixel()).ok_or(AllocationError::SizeOverflow)?;
        let mut data = Vec::new();
        data.try_reserve_exact(bytes).map_err(|_| AllocationError::NotEnoughMemory { bytes })?;
        data.resize(bytes, 0);
        if pixel.iter().any(|&b| b != 0) {
            for px in data.chunks_exact_mut(pixel.len()) {
                px.copy_from_slice(pixel);
            }
        }
        Ok(Tile { data: data.into_boxed_slice() })
    }

    /// Clone this tile's pixel buffer with a fallible reservation.
    pub fn try_clone(&self) -> Result<Self, AllocationError> {
        let bytes = self.data.len();
        let mut data = Vec::new();
        data.try_reserve_exact(bytes).map_err(|_| AllocationError::NotEnoughMemory { bytes })?;
        data.extend_from_slice(&self.data);
        Ok(Tile { data: data.into_boxed_slice() })
    }

    fn filled(format: &PixelFormat, pixel: &[u8]) -> Self {
        let row_len = TILE_SIZE as usize * format.bytes_per_pixel();
        let mut data = vec![0u8; row_len * TILE_SIZE as usize];
        if pixel.iter().any(|&b| b != 0) {
            // One prebuilt row copied per tile row: 256 bulk copies instead of one
            // copy_from_slice per pixel.
            let mut row = Vec::with_capacity(row_len);
            for _ in 0..TILE_SIZE {
                row.extend_from_slice(pixel);
            }
            for out in data.chunks_mut(row_len) {
                out.copy_from_slice(&row);
            }
        }
        Tile { data: data.into_boxed_slice() }
    }
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

/// Regions at least this many pixels read and write their tiles in parallel (smaller ones are
/// dabs and spans, where waking the pool costs more than it saves).
#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
const PARALLEL_PIXELS: u64 = 1 << 18;

/// Whether `r` is large enough, and spans more than one tile, to be worked in parallel.
#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
fn parallel(r: Rect) -> bool {
    !r.is_empty() && u64::from(r.width()) * u64::from(r.height()) >= PARALLEL_PIXELS && r.tiles().nth(1).is_some()
}

/// Run `f(band, rows)` for each tile-row band of `r` (its rows of `out`, which holds `r` row by
/// row, `row_len` elements a row), in parallel for a large `r`. Each band is what the serial
/// pass over `r` writes for those rows, so the result is the same.
fn bands<T: Send>(r: Rect, out: &mut [T], row_len: usize, f: impl Fn(Rect, &mut [T]) + Sync) {
    #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
    if parallel(r) && r.y0.div_euclid(TILE_SIZE) != (r.y1 - 1).div_euclid(TILE_SIZE) {
        use rayon::prelude::*;
        let mut parts = Vec::new();
        let mut rest = out;
        let mut y = r.y0;
        while y < r.y1 {
            let y1 = ((y.div_euclid(TILE_SIZE) + 1) * TILE_SIZE).min(r.y1);
            let len = ((y1 - y) as usize * row_len).min(rest.len());
            let (band, tail) = std::mem::take(&mut rest).split_at_mut(len);
            parts.push((Rect::new(r.x0, y, r.x1, y1), band));
            rest = tail;
            y = y1;
        }
        parts.into_par_iter().for_each(|(b, band)| f(b, band));
        return;
    }
    #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
    let _ = row_len;
    f(r, out);
}

#[derive(Clone, Debug)]
pub struct Surface {
    format: PixelFormat,
    default_pixel: Box<[u8]>,
    tiles: BTreeMap<TileCoord, Arc<Tile>>,
}

impl PartialEq for Surface {
    fn eq(&self, other: &Self) -> bool {
        self.format == other.format && self.default_pixel == other.default_pixel && self.tiles == other.tiles
    }
}

impl Surface {
    /// Reads a region into a newly allocated buffer, returning an error instead of aborting if
    /// the requested pixel buffer cannot be represented or reserved.
    pub fn try_read_region(&self, r: Rect) -> Result<Vec<f32>, AllocationError> {
        let len = (r.width() as usize).checked_mul(r.height() as usize).and_then(|n| n.checked_mul(self.channels())).ok_or(AllocationError::SizeOverflow)?;
        let bytes = len.checked_mul(std::mem::size_of::<f32>()).ok_or(AllocationError::SizeOverflow)?;
        let mut out = Vec::new();
        out.try_reserve_exact(len).map_err(|_| AllocationError::NotEnoughMemory { bytes })?;
        out.resize(len, 0.0);
        self.read_region_into(r, &mut out);
        Ok(out)
    }

    /// Reads a region as encoded interleaved bytes with fallible allocation.
    pub fn try_to_interleaved(&self, r: Rect) -> Result<Vec<u8>, AllocationError> {
        let bpp = self.format.bytes_per_pixel();
        let len = (r.width() as usize).checked_mul(r.height() as usize).and_then(|n| n.checked_mul(bpp)).ok_or(AllocationError::SizeOverflow)?;
        let mut out = Vec::new();
        out.try_reserve_exact(len).map_err(|_| AllocationError::NotEnoughMemory { bytes: len })?;
        out.resize(len, 0);
        let w = r.width() as usize;
        for tc in r.tiles() {
            let tr = tc.rect().intersect(&r);
            let span = tr.width() as usize * bpp;
            for y in tr.y0..tr.y1 {
                let dst = (((y - r.y0) as usize) * w + (tr.x0 - r.x0) as usize) * bpp;
                match self.tiles.get(&tc) {
                    Some(t) => {
                        let src = (((y - tc.ty * TILE_SIZE) as usize) * TILE_SIZE as usize + (tr.x0 - tc.tx * TILE_SIZE) as usize) * bpp;
                        out[dst..dst + span].copy_from_slice(&t.data[src..src + span]);
                    }
                    None => {
                        for px in out[dst..dst + span].chunks_exact_mut(bpp) {
                            px.copy_from_slice(&self.default_pixel);
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Transparent/zero surface.
    pub fn new(format: PixelFormat) -> Self {
        Self { format, default_pixel: vec![0u8; format.bytes_per_pixel()].into_boxed_slice(), tiles: BTreeMap::new() }
    }

    /// Surface whose untouched pixels read as `pixel` (normalised channel values).
    pub fn with_default(format: PixelFormat, pixel: &[f32]) -> Self {
        let mut s = Self::new(format);
        encode_pixel(&format, pixel, &mut s.default_pixel);
        s
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }
    pub fn channels(&self) -> usize {
        self.format.channels()
    }
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }
    pub fn tiles(&self) -> impl Iterator<Item = (&TileCoord, &Arc<Tile>)> {
        self.tiles.iter()
    }
    pub fn tile(&self, c: TileCoord) -> Option<&Arc<Tile>> {
        self.tiles.get(&c)
    }
    pub fn default_pixel(&self) -> Vec<f32> {
        decode_pixel(&self.format, &self.default_pixel)
    }

    fn default_samples(&self) -> [f32; 8] {
        let mut px = [0.0; 8];
        for (i, v) in px.iter_mut().enumerate().take(self.channels()) {
            *v = read_sample(&self.default_pixel, self.format.sample, i);
        }
        px
    }

    /// Mutable access to a tile, allocating it (filled with the default pixel) or un-sharing it.
    pub fn tile_mut(&mut self, c: TileCoord) -> &mut Tile {
        let fmt = self.format;
        let dp = &self.default_pixel;
        let arc = self.tiles.entry(c).or_insert_with(|| Arc::new(Tile::filled(&fmt, dp)));
        Arc::make_mut(arc)
    }

    /// Fallible counterpart to [`Surface::tile_mut`]. Pixel storage is reserved before changing
    /// the surface, and a shared tile is copied only after its replacement has been allocated.
    pub fn try_tile_mut(&mut self, c: TileCoord) -> Result<&mut Tile, AllocationError> {
        if !self.tiles.contains_key(&c) {
            let tile = Arc::new(Tile::try_filled(&self.format, &self.default_pixel)?);
            self.tiles.insert(c, tile);
        } else if self.tiles.get(&c).is_some_and(|t| Arc::strong_count(t) > 1) {
            let original = self.tiles.get(&c).ok_or(AllocationError::SizeOverflow)?;
            let tile = Arc::new(original.try_clone()?);
            self.tiles.insert(c, tile);
        }
        self.tiles.get_mut(&c).and_then(Arc::get_mut).ok_or(AllocationError::SizeOverflow)
    }

    /// Writes a region using fallible pixel-tile allocations. Callers that need transactional
    /// document updates should perform this on their staged document copy and commit on success.
    pub fn try_write_region(&mut self, r: Rect, data: &[f32]) -> Result<(), AllocationError> {
        let n = self.channels();
        let sample = self.format.sample;
        let expected = (r.width() as usize).checked_mul(r.height() as usize).and_then(|v| v.checked_mul(n)).ok_or(AllocationError::SizeOverflow)?;
        if data.len() != expected {
            return Err(AllocationError::SizeOverflow);
        }
        for tc in r.tiles() {
            let tr = tc.rect().intersect(&r);
            let t = self.try_tile_mut(tc)?;
            let span = tr.width() as usize * n;
            for y in tr.y0..tr.y1 {
                let src = (((y - r.y0) as usize) * r.width() as usize + (tr.x0 - r.x0) as usize) * n;
                let dst = (((y - tc.ty * TILE_SIZE) as usize) * TILE_SIZE as usize + (tr.x0 - tc.tx * TILE_SIZE) as usize) * n;
                encode_span(&mut t.data, sample, dst, &data[src..src + span]);
            }
        }
        Ok(())
    }

    /// Coarse bounds: union of allocated tile rects.
    pub fn tile_bounds(&self) -> Rect {
        self.tiles.keys().fold(Rect::EMPTY, |r, c| r.union(&c.rect()))
    }

    /// Exact bounds of pixels that differ from the default pixel.
    pub fn content_bounds(&self) -> Rect {
        let bpp = self.format.bytes_per_pixel();
        if self.tiles.is_empty() {
            return Rect::EMPTY;
        }
        // Scan tiles from the outside of the tile grid inwards; a tile lying entirely inside the
        // bounds found so far cannot extend them and is skipped (so a full-canvas selection scans
        // roughly its outer ring of tiles instead of every pixel).
        let (mut gx0, mut gy0, mut gx1, mut gy1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for c in self.tiles.keys() {
            (gx0, gy0, gx1, gy1) = (gx0.min(c.tx), gy0.min(c.ty), gx1.max(c.tx), gy1.max(c.ty));
        }
        let mut order: Vec<(&TileCoord, &Arc<Tile>)> = self.tiles.iter().collect();
        order.sort_by_key(|(c, _)| (c.tx - gx0).min(gx1 - c.tx).min(c.ty - gy0).min(gy1 - c.ty));
        let dp = &*self.default_pixel;
        let ts = TILE_SIZE as usize;
        let mut out = Rect::EMPTY;
        for (c, t) in order {
            let origin = c.rect();
            if !out.is_empty() && out.contains_rect(&origin) {
                continue;
            }
            let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
            for (y, row) in t.data.chunks_exact(ts * bpp).enumerate() {
                let Some(first) = row.chunks_exact(bpp).position(|px| px != dp) else { continue };
                let last = ts - 1 - row.chunks_exact(bpp).rev().position(|px| px != dp).unwrap_or(0);
                x0 = x0.min(first);
                x1 = x1.max(last + 1);
                y0 = y0.min(y);
                y1 = y + 1;
            }
            if x0 != usize::MAX {
                out = out.union(&Rect::new(x0 as i32, y0 as i32, x1 as i32, y1 as i32).translate(origin.x0, origin.y0));
            }
        }
        out
    }

    /// Drop tiles that are entirely the default pixel.
    pub fn prune(&mut self) {
        let dp = self.default_pixel.as_ref();
        let bpp = dp.len();
        self.tiles.retain(|_, t| t.data.chunks_exact(bpp).any(|px| px != dp));
    }

    #[inline]
    fn locate(&self, x: i32, y: i32) -> (TileCoord, usize) {
        let c = TileCoord::containing(x, y);
        let lx = x.rem_euclid(TILE_SIZE) as usize;
        let ly = y.rem_euclid(TILE_SIZE) as usize;
        (c, (ly * TILE_SIZE as usize + lx) * self.format.channels())
    }

    /// Read one pixel as normalised channel values (`channels()` floats).
    pub fn pixel(&self, x: i32, y: i32) -> Vec<f32> {
        let mut out = vec![0.0; self.channels()];
        self.read_pixel(x, y, &mut out);
        out
    }

    pub fn read_pixel(&self, x: i32, y: i32, out: &mut [f32]) {
        let (c, base) = self.locate(x, y);
        let n = self.channels();
        match self.tiles.get(&c) {
            Some(t) => {
                for (i, o) in out.iter_mut().enumerate().take(n) {
                    *o = read_sample(&t.data, self.format.sample, base + i);
                }
            }
            None => {
                for (i, o) in out.iter_mut().enumerate().take(n) {
                    *o = read_sample(&self.default_pixel, self.format.sample, i);
                }
            }
        }
    }

    pub fn write_pixel(&mut self, x: i32, y: i32, px: &[f32]) {
        let (c, base) = self.locate(x, y);
        let n = self.channels();
        let sample = self.format.sample;
        let t = self.tile_mut(c);
        for (i, v) in px.iter().enumerate().take(n) {
            write_sample(&mut t.data, sample, base + i, *v);
        }
    }

    /// One channel of one pixel, without allocating.
    #[inline]
    pub fn sample_channel(&self, x: i32, y: i32, c: usize) -> f32 {
        let (tc, base) = self.locate(x, y);
        match self.tiles.get(&tc) {
            Some(t) => read_sample(&t.data, self.format.sample, base + c),
            None => read_sample(&self.default_pixel, self.format.sample, c),
        }
    }

    /// Channel `c` at `(x0 + i·step, y)` for `i in 0..out.len()`, looking each tile up once per run
    /// (much faster than per-sample [`Surface::sample_channel`] for strided scans).
    pub fn sample_row_strided(&self, y: i32, x0: i32, step: i32, c: usize, out: &mut [f32]) {
        let step = step.max(1);
        let dp = read_sample(&self.default_pixel, self.format.sample, c);
        let n = self.format.channels();
        let ty = y.div_euclid(TILE_SIZE);
        let ly = y.rem_euclid(TILE_SIZE) as usize;
        let mut i = 0;
        while i < out.len() {
            let x = x0 + i as i32 * step;
            let tx = x.div_euclid(TILE_SIZE);
            // Samples that fall in this tile.
            let tile_end = (tx + 1) * TILE_SIZE;
            let count = (((tile_end - x) + step - 1) / step).max(1) as usize;
            let end = (i + count).min(out.len());
            match self.tiles.get(&TileCoord { tx, ty }) {
                None => out[i..end].fill(dp),
                Some(t) => {
                    for (k, o) in out[i..end].iter_mut().enumerate() {
                        let lx = (x + k as i32 * step - tx * TILE_SIZE) as usize;
                        *o = read_sample(&t.data, self.format.sample, (ly * TILE_SIZE as usize + lx) * n + c);
                    }
                }
            }
            i = end;
        }
    }

    /// [`Surface::read_region`] into a reusable buffer (resized to fit).
    pub fn read_region_into(&self, r: Rect, out: &mut Vec<f32>) {
        let n = self.channels();
        let w = r.width() as usize;
        out.clear();
        out.resize(w * r.height() as usize * n, 0.0);
        bands(r, out, w * n, |b, band| self.read_region_slice(b, band));
    }

    /// [`Self::read_region_into`] into `out`, already `r`-sized.
    fn read_region_slice(&self, r: Rect, out: &mut [f32]) {
        let n = self.channels();
        let w = r.width() as usize;
        let dp = self.default_samples();
        let dp = &dp[..n];
        let zero = self.default_pixel.iter().all(|&b| b == 0);
        for tc in r.tiles() {
            let tr = tc.rect().intersect(&r);
            let tile = self.tiles.get(&tc);
            if tile.is_none() && zero {
                continue;
            }
            for y in tr.y0..tr.y1 {
                let o = (((y - r.y0) as usize) * w + (tr.x0 - r.x0) as usize) * n;
                let span = tr.width() as usize * n;
                let dst = &mut out[o..o + span];
                match tile {
                    Some(t) => {
                        let base = (((y - tc.ty * TILE_SIZE) as usize) * TILE_SIZE as usize + (tr.x0 - tc.tx * TILE_SIZE) as usize) * n;
                        decode_span(&t.data, self.format.sample, base, dst);
                    }
                    None if n == 1 => dst.fill(dp[0]),
                    None => {
                        for px in dst.chunks_exact_mut(n) {
                            px.copy_from_slice(dp);
                        }
                    }
                }
            }
        }
    }

    /// Reads `r` as straight RGBA floats (converting from the surface's
    /// model) into `out` (`w*h` entries), without per-pixel allocation.
    /// Fast paths for RGB(A) and gray(A) at 8/16/32 bits read tile bytes
    /// directly; missing tiles are filled with the default pixel.
    pub fn read_rgba_into(&self, r: Rect, out: &mut [[f32; 4]]) {
        let w = r.width() as usize;
        debug_assert_eq!(out.len(), w * r.height() as usize);
        let fmt = self.format;
        let n = fmt.channels();
        let dp = to_rgba(&fmt, &self.default_samples());
        for tc in r.tiles() {
            let tr = tc.rect().intersect(&r);
            let tile = self.tiles.get(&tc);
            for y in tr.y0..tr.y1 {
                let o = ((y - r.y0) as usize) * w + (tr.x0 - r.x0) as usize;
                let dst = &mut out[o..o + tr.width() as usize];
                let Some(t) = tile else {
                    dst.fill(dp);
                    continue;
                };
                let base = (((y - tc.ty * TILE_SIZE) as usize) * TILE_SIZE as usize + (tr.x0 - tc.tx * TILE_SIZE) as usize) * n;
                match (fmt.mode, fmt.sample, fmt.alpha) {
                    (ColorMode::Rgb, SampleType::U8, true) => {
                        let src = &t.data[base..base + dst.len() * 4];
                        for (d, s) in dst.iter_mut().zip(src.as_chunks::<4>().0) {
                            *d = [s[0] as f32 / 255.0, s[1] as f32 / 255.0, s[2] as f32 / 255.0, s[3] as f32 / 255.0];
                        }
                    }
                    (ColorMode::Grayscale, SampleType::U8, true) => {
                        let src = &t.data[base..base + dst.len() * 2];
                        for (d, s) in dst.iter_mut().zip(src.as_chunks::<2>().0) {
                            let g = s[0] as f32 / 255.0;
                            *d = [g, g, g, s[1] as f32 / 255.0];
                        }
                    }
                    (ColorMode::Cmyk, SampleType::U8, _) => {
                        // Identical ink bytes share the exact ICC result; alpha is independent.
                        let mut previous: Option<([u8; 4], [f32; 3])> = None;
                        let src = &t.data[base..base + dst.len() * n];
                        for (d, s) in dst.iter_mut().zip(src.chunks_exact(n)) {
                            let ink = [s[0], s[1], s[2], s[3]];
                            let rgb = match previous {
                                Some((key, rgb)) if key == ink => rgb,
                                _ => {
                                    let rgb = photocraft_color::convert::cmyk_to_rgb(ink.map(|v| v as f32 / 255.0));
                                    previous = Some((ink, rgb));
                                    rgb
                                }
                            };
                            *d = [rgb[0], rgb[1], rgb[2], s.get(4).map_or(1.0, |v| *v as f32 / 255.0)];
                        }
                    }
                    _ => {
                        let mut px = [0.0f32; 8];
                        for (i, d) in dst.iter_mut().enumerate() {
                            for (c, v) in px.iter_mut().enumerate().take(n) {
                                *v = read_sample(&t.data, fmt.sample, base + i * n + c);
                            }
                            *d = to_rgba(&fmt, &px[..n]);
                        }
                    }
                }
            }
        }
    }

    /// Reads `r` as straight 8-bit RGBA into `out` (`w*h` entries). RGBA8 surfaces copy tile bytes
    /// directly; other formats convert through [`Surface::read_rgba_into`] one tile row at a time.
    pub fn read_rgba8_into(&self, r: Rect, out: &mut [[u8; 4]]) {
        let w = r.width() as usize;
        debug_assert_eq!(out.len(), w * r.height() as usize);
        let fmt = self.format;
        let rgba8 = matches!((fmt.mode, fmt.sample, fmt.alpha), (ColorMode::Rgb, SampleType::U8, true));
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let dp = to_rgba(&fmt, &self.default_samples());
        let dp8 = [q(dp[0]), q(dp[1]), q(dp[2]), q(dp[3])];
        let mut row = Vec::new();
        for tc in r.tiles() {
            let tr = tc.rect().intersect(&r);
            let tile = self.tiles.get(&tc);
            for y in tr.y0..tr.y1 {
                let o = ((y - r.y0) as usize) * w + (tr.x0 - r.x0) as usize;
                let dst = &mut out[o..o + tr.width() as usize];
                match tile {
                    None => dst.fill(dp8),
                    Some(t) if rgba8 => {
                        let base = (((y - tc.ty * TILE_SIZE) as usize) * TILE_SIZE as usize + (tr.x0 - tc.tx * TILE_SIZE) as usize) * 4;
                        let len = dst.len();
                        for (d, s) in dst.iter_mut().zip(t.data[base..base + len * 4].as_chunks::<4>().0) {
                            *d = [s[0], s[1], s[2], s[3]];
                        }
                    }
                    Some(_) => {
                        row.resize(dst.len(), [0.0f32; 4]);
                        self.read_rgba_into(Rect::new(tr.x0, y, tr.x1, y + 1), &mut row);
                        for (d, p) in dst.iter_mut().zip(&row) {
                            *d = [q(p[0]), q(p[1]), q(p[2]), q(p[3])];
                        }
                    }
                }
            }
        }
    }

    /// `true` if any allocated tile intersects `r` (else `r` reads as the
    /// default pixel everywhere).
    pub fn has_tiles_in(&self, r: Rect) -> bool {
        r.tiles().any(|tc| self.tiles.contains_key(&tc))
    }

    /// Read a rectangle as interleaved normalised floats (`w*h*channels`).
    pub fn read_region(&self, r: Rect) -> Vec<f32> {
        let mut out = Vec::new();
        self.read_region_into(r, &mut out);
        out
    }

    /// Write interleaved normalised floats into a rectangle.
    pub fn write_region(&mut self, r: Rect, data: &[f32]) {
        let n = self.channels();
        let w = r.width() as usize;
        assert_eq!(data.len(), w * r.height() as usize * n, "region data length mismatch");
        let sample = self.format.sample;
        self.edit_tiles(r, |tc, t| {
            let tr = tc.rect().intersect(&r);
            let span = tr.width() as usize * n;
            for y in tr.y0..tr.y1 {
                let o = (((y - r.y0) as usize) * w + (tr.x0 - r.x0) as usize) * n;
                let base = (((y - tc.ty * TILE_SIZE) as usize) * TILE_SIZE as usize + (tr.x0 - tc.tx * TILE_SIZE) as usize) * n;
                encode_span(&mut t.data, sample, base, &data[o..o + span]);
            }
        });
    }

    /// Run `f` on every tile covering `r`, made writable as [`Self::tile_mut`] makes it (a missing
    /// one filled with the default pixel, a shared one copied first). A large `r` runs the tiles
    /// in parallel: they are taken out of the surface, edited and put back, which leaves exactly
    /// what editing them one by one does.
    fn edit_tiles(&mut self, r: Rect, f: impl Fn(TileCoord, &mut Tile) + Sync) {
        #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
        if parallel(r) {
            use rayon::prelude::*;
            let (fmt, dp) = (self.format, &self.default_pixel);
            let mut work: Vec<(TileCoord, Arc<Tile>)> =
                r.tiles().map(|c| (c, self.tiles.remove(&c).unwrap_or_else(|| Arc::new(Tile::filled(&fmt, dp))))).collect();
            work.par_iter_mut().for_each(|(c, t)| f(*c, Arc::make_mut(t)));
            self.tiles.extend(work);
            return;
        }
        for tc in r.tiles() {
            f(tc, self.tile_mut(tc));
        }
    }

    /// Lock transparency after an edit: give every pixel of the tiles that changed since `old`
    /// (tiles not shared with it, and tiles only `old` has) its alpha in `old`, keeping its other
    /// channels. Copies the alpha samples' bytes, which is what decoding, replacing the alpha and
    /// re-encoding would store. Then prunes. Nothing happens unless both have the same format with
    /// alpha.
    pub fn copy_alpha_from(&mut self, old: &Surface) {
        if old.format != self.format || !self.format.alpha {
            return;
        }
        let size = self.format.sample.bytes();
        let bpp = self.format.bytes_per_pixel();
        let at = bpp - size;
        let mut coords: Vec<TileCoord> = self.tiles.iter().filter(|(c, t)| old.tiles.get(c).is_none_or(|o| !Arc::ptr_eq(o, t))).map(|(c, _)| *c).collect();
        coords.extend(old.tiles.keys().filter(|c| !self.tiles.contains_key(c)));
        // The tiles are independent: take them out, fix them in parallel, put them back.
        let mut work: Vec<(TileCoord, Arc<Tile>)> = coords
            .into_iter()
            .map(|c| {
                let t = self.tiles.remove(&c).unwrap_or_else(|| Arc::new(Tile::filled(&self.format, &self.default_pixel)));
                (c, t)
            })
            .collect();
        let default = &old.default_pixel[at..];
        let fix = |(c, t): &mut (TileCoord, Arc<Tile>)| {
            let t = Arc::make_mut(t);
            match old.tiles.get(c) {
                Some(o) => match size {
                    1 => t.data.iter_mut().skip(at).step_by(bpp).zip(o.data.iter().skip(at).step_by(bpp)).for_each(|(d, s)| *d = *s),
                    _ => t.data.chunks_exact_mut(bpp).zip(o.data.chunks_exact(bpp)).for_each(|(d, s)| d[at..].copy_from_slice(&s[at..])),
                },
                None => t.data.chunks_exact_mut(bpp).for_each(|d| d[at..].copy_from_slice(default)),
            }
        };
        #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
        {
            use rayon::prelude::*;
            work.par_iter_mut().for_each(fix);
        }
        #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
        work.iter_mut().for_each(fix);
        self.tiles.extend(work);
        self.prune();
    }

    /// Take the tiles covering `r` out of the surface (missing ones filled with the default
    /// pixel), so they can be edited elsewhere, e.g. in parallel: `Arc::unwrap_or_clone` copies a
    /// tile only if an undo snapshot still shares it. Give them back with [`Surface::put_tiles`].
    pub fn take_tiles(&mut self, r: Rect) -> Vec<(TileCoord, Arc<Tile>)> {
        let fmt = self.format;
        let mut blank: Option<Arc<Tile>> = None;
        r.tiles()
            .map(|c| {
                let t = self.tiles.remove(&c).unwrap_or_else(|| blank.get_or_insert_with(|| Arc::new(Tile::filled(&fmt, &self.default_pixel))).clone());
                (c, t)
            })
            .collect()
    }

    /// Fallible counterpart to [`Surface::take_tiles`]. Reserves the complete result and any
    /// default tile before removing a tile, so a reservation failure leaves the surface intact.
    pub fn try_take_tiles(&mut self, r: Rect) -> Result<Vec<(TileCoord, Arc<Tile>)>, AllocationError> {
        let count = r.tiles().count();
        let bytes = count.checked_mul(std::mem::size_of::<(TileCoord, Arc<Tile>)>()).ok_or(AllocationError::SizeOverflow)?;
        let mut out = Vec::new();
        out.try_reserve_exact(count).map_err(|_| AllocationError::NotEnoughMemory { bytes })?;
        let has_missing = r.tiles().any(|c| !self.tiles.contains_key(&c));
        let blank = if has_missing { Some(Arc::new(Tile::try_filled(&self.format, &self.default_pixel)?)) } else { None };
        for c in r.tiles() {
            let tile = self.tiles.remove(&c).or_else(|| blank.clone());
            if let Some(tile) = tile {
                out.push((c, tile));
            }
        }
        Ok(out)
    }

    /// Put tiles back (see [`Surface::take_tiles`]); each replaces the tile at its coordinate.
    pub fn put_tiles(&mut self, tiles: impl IntoIterator<Item = (TileCoord, Arc<Tile>)>) {
        for (c, t) in tiles {
            self.tiles.insert(c, t);
        }
    }

    /// A tile filled with the default pixel (what a missing tile reads as), allocated fallibly.
    pub fn try_default_tile(&self) -> Result<Tile, AllocationError> {
        Tile::try_filled(&self.format, &self.default_pixel)
    }

    /// A tile of this surface's format filled with `px` (normalised channel values). Sharing one
    /// across many coordinates fills them without copying (tiles are copy-on-write).
    pub fn solid_tile(&self, px: &[f32]) -> Arc<Tile> {
        let mut enc = vec![0u8; self.format.bytes_per_pixel()];
        encode_pixel(&self.format, px, &mut enc);
        Arc::new(Tile::filled(&self.format, &enc))
    }

    /// The default pixel's encoded bytes.
    pub fn default_bytes(&self) -> &[u8] {
        &self.default_pixel
    }

    /// Fill a rectangle with one pixel value.
    pub fn fill_rect(&mut self, r: Rect, px: &[f32]) {
        let n = self.channels();
        let mut enc = vec![0u8; self.format.bytes_per_pixel()];
        encode_pixel(&self.format, &px[..n], &mut enc);
        let bpp = enc.len();
        // Fully covered tiles share one Arc: a solid fill costs one tile however large `r` is,
        // and copy-on-write splits a tile off when it is edited (#705).
        let mut full: Option<Arc<Tile>> = None;
        for tc in r.tiles() {
            let tr = tc.rect().intersect(&r);
            if tr == tc.rect() {
                let fmt = self.format;
                let t = full.get_or_insert_with(|| Arc::new(Tile::filled(&fmt, &enc)));
                self.tiles.insert(tc, t.clone());
                continue;
            }
            let t = self.tile_mut(tc);
            for y in tr.y0..tr.y1 {
                let ly = (y - tc.ty * TILE_SIZE) as usize;
                let lx0 = (tr.x0 - tc.tx * TILE_SIZE) as usize;
                let lx1 = (tr.x1 - tc.tx * TILE_SIZE) as usize;
                let row = &mut t.data[(ly * TILE_SIZE as usize + lx0) * bpp..(ly * TILE_SIZE as usize + lx1) * bpp];
                for p in row.chunks_exact_mut(bpp) {
                    p.copy_from_slice(&enc);
                }
            }
        }
    }

    /// Build from interleaved *encoded* bytes of `format` covering `r`.
    pub fn from_interleaved(format: PixelFormat, r: Rect, bytes: &[u8]) -> Self {
        let mut s = Surface::new(format);
        s.write_interleaved(r, bytes);
        s
    }

    /// Write interleaved encoded bytes (same format as the surface) into `r`.
    pub fn write_interleaved(&mut self, r: Rect, bytes: &[u8]) {
        let bpp = self.format.bytes_per_pixel();
        let w = r.width() as usize;
        assert_eq!(bytes.len(), w * r.height() as usize * bpp, "interleaved length mismatch");
        self.edit_tiles(r, |tc, t| {
            let tr = tc.rect().intersect(&r);
            let span = tr.width() as usize * bpp;
            for y in tr.y0..tr.y1 {
                let src = (((y - r.y0) as usize) * w + (tr.x0 - r.x0) as usize) * bpp;
                let dst = (((y - tc.ty * TILE_SIZE) as usize) * TILE_SIZE as usize + (tr.x0 - tc.tx * TILE_SIZE) as usize) * bpp;
                t.data[dst..dst + span].copy_from_slice(&bytes[src..src + span]);
            }
        });
    }

    /// Read `r` as interleaved encoded bytes in the surface's format.
    pub fn to_interleaved(&self, r: Rect) -> Vec<u8> {
        let bpp = self.format.bytes_per_pixel();
        let w = r.width() as usize;
        let mut out = vec![0u8; w * r.height() as usize * bpp];
        bands(r, &mut out, w * bpp, |b, band| self.to_interleaved_slice(b, band));
        out
    }

    /// [`Self::to_interleaved`] into `out`, already `r`-sized.
    fn to_interleaved_slice(&self, r: Rect, out: &mut [u8]) {
        let bpp = self.format.bytes_per_pixel();
        let w = r.width() as usize;
        for tc in r.tiles() {
            let tr = tc.rect().intersect(&r);
            let span = tr.width() as usize * bpp;
            for y in tr.y0..tr.y1 {
                let dst = (((y - r.y0) as usize) * w + (tr.x0 - r.x0) as usize) * bpp;
                match self.tiles.get(&tc) {
                    Some(t) => {
                        let src = (((y - tc.ty * TILE_SIZE) as usize) * TILE_SIZE as usize + (tr.x0 - tc.tx * TILE_SIZE) as usize) * bpp;
                        out[dst..dst + span].copy_from_slice(&t.data[src..src + span]);
                    }
                    None => {
                        for p in out[dst..dst + span].chunks_exact_mut(bpp) {
                            p.copy_from_slice(&self.default_pixel);
                        }
                    }
                }
            }
        }
    }

    /// The surface moved by whole pixels: its encoded samples copied tile to tile (no float
    /// round trip, no intermediate buffer); moves by whole tiles share the tiles themselves.
    /// `content` must hold every pixel that isn't the default one (`content_bounds`, say): only
    /// tiles it reaches are kept.
    pub fn translated(&self, dx: i32, dy: i32, content: Rect) -> Surface {
        let mut out = Surface { format: self.format, default_pixel: self.default_pixel.clone(), tiles: BTreeMap::new() };
        let sources = || self.tiles.iter().filter(move |(c, _)| !c.rect().intersect(&content).is_empty());
        if dx % TILE_SIZE == 0 && dy % TILE_SIZE == 0 {
            let (tx, ty) = (dx / TILE_SIZE, dy / TILE_SIZE);
            out.tiles = sources().map(|(c, t)| (TileCoord::new(c.tx.saturating_add(tx), c.ty.saturating_add(ty)), t.clone())).collect();
            return out;
        }
        let bpp = self.format.bytes_per_pixel();
        let moved = content.translate(dx, dy);
        let targets: std::collections::BTreeSet<TileCoord> = sources().flat_map(|(c, _)| c.rect().translate(dx, dy).intersect(&moved).tiles()).collect();
        // One row of the default pixel, so the regions a shift vacates fill with bulk copies.
        let default_row: Vec<u8> = if self.default_pixel.iter().any(|&b| b != 0) {
            let mut row = Vec::with_capacity(TILE_SIZE as usize * bpp);
            for _ in 0..TILE_SIZE {
                row.extend_from_slice(&self.default_pixel);
            }
            row
        } else {
            Vec::new()
        };
        for dc in targets {
            let mut tile = Tile { data: vec![0u8; (TILE_SIZE * TILE_SIZE) as usize * bpp].into_boxed_slice() };
            let (tx0, ty0) = (i64::from(dc.tx) * i64::from(TILE_SIZE), i64::from(dc.ty) * i64::from(TILE_SIZE));
            let row_len = TILE_SIZE as usize * bpp;
            // Per stored row: copy the source span that falls inside the content in as few
            // memcpys as the tile grid allows, and fill the vacated edges from the default
            // row — no whole-tile pre-fill, which rewrote nearly every byte twice.
            for local_y in 0..i64::from(TILE_SIZE) {
                let y = ty0 + local_y;
                let Some(row) = tile.data.get_mut(local_y as usize * row_len..(local_y as usize + 1) * row_len) else { continue };
                let in_content_y = y - i64::from(dy) >= i64::from(content.y0) && y - i64::from(dy) < i64::from(content.y1);
                // The row's target columns whose SOURCE lies inside the content are copied
                // (the copy loop crosses source tiles as needed); the rest stays default —
                // exactly the pixels the shift vacated.
                let cx0 = (i64::from(content.x0) + i64::from(dx)).max(tx0);
                let cx1 = (i64::from(content.x1) + i64::from(dx)).min(tx0 + i64::from(TILE_SIZE));
                if !in_content_y || cx1 <= cx0 {
                    if !default_row.is_empty() {
                        row.copy_from_slice(&default_row);
                    }
                    continue;
                }
                let left = (cx0.max(tx0) - tx0) as usize * bpp;
                let copied_end = (cx1.min(tx0 + i64::from(TILE_SIZE)) - tx0) as usize * bpp;
                if !default_row.is_empty() {
                    if left > 0 {
                        row[..left].copy_from_slice(&default_row[..left]);
                    }
                    if copied_end < row_len {
                        row[copied_end..].copy_from_slice(&default_row[copied_end..]);
                    }
                }
                let mut x = cx0.max(tx0);
                let x_end = cx1.min(tx0 + i64::from(TILE_SIZE));
                // Rows run over the same one or two source tiles: cache the last lookup.
                let mut cached: Option<(i64, i64, &std::sync::Arc<Tile>)> = None;
                while x < x_end {
                    // The source document position this target column reads, and the source
                    // tile it lives in (the shift crosses source tiles as x runs).
                    let (sx, sy) = (x - i64::from(dx), y - i64::from(dy));
                    let (tx, ty) = (sx.div_euclid(i64::from(TILE_SIZE)), sy.div_euclid(i64::from(TILE_SIZE)));
                    // A missing source tile inside the content holds the default pixel, not
                    // zeros: fill the span from the default row instead of skipping it
                    // (echelon's white-mask-with-sparse-tiles case).
                    let st = match cached.take() {
                        Some((ctx, cty, t)) if ctx == tx && cty == ty => {
                            cached = Some((ctx, cty, t));
                            t
                        }
                        _ => match self.tiles.get(&TileCoord::new(tx as i32, ty as i32)) {
                            Some(t) => {
                                cached = Some((tx, ty, t));
                                t
                            }
                            None => {
                                let span_end = ((tx + 1) * i64::from(TILE_SIZE) + i64::from(dx)).min(x_end).max(x + 1);
                                if !default_row.is_empty() {
                                    let at = (x - tx0) as usize * bpp;
                                    let len = (span_end - x) as usize * bpp;
                                    if let Some(d) = row.get_mut(at..at + len) {
                                        d.copy_from_slice(&default_row[at..at + len]);
                                    }
                                }
                                x = span_end;
                                continue;
                            }
                        },
                    };
                    let span_end = (((tx + 1) * i64::from(TILE_SIZE) + i64::from(dx)).min(x_end)).max(x + 1);
                    let span = (span_end - x) as usize * bpp;
                    let src = ((sy - ty * i64::from(TILE_SIZE)) * row_len as i64 + (sx - tx * i64::from(TILE_SIZE)) * bpp as i64) as usize;
                    let dst = (x - tx0) as usize * bpp;
                    if let (Some(d), Some(sr)) = (row.get_mut(dst..dst + span), st.data.get(src..src + span)) {
                        d.copy_from_slice(sr);
                    }
                    x = span_end;
                }
            }
            out.tiles.insert(dc, Arc::new(tile));
        }
        out
    }

    /// Convert to another pixel format (depth and/or model; colour models via [`convert_pixel`]).
    pub fn convert(&self, to: PixelFormat) -> Surface {
        let mut out = Surface::with_default(to, &convert_pixel(&self.format, &to, &self.default_pixel()));
        let from_n = self.channels();
        let mut src = vec![0.0f32; from_n];
        for (c, t) in &self.tiles {
            let dst = out.tile_mut(*c);
            let px_count = (TILE_SIZE * TILE_SIZE) as usize;
            for i in 0..px_count {
                for (k, v) in src.iter_mut().enumerate() {
                    *v = read_sample(&t.data, self.format.sample, i * from_n + k);
                }
                let px = convert_pixel(&self.format, &to, &src);
                for (k, v) in px.iter().enumerate() {
                    write_sample(&mut dst.data, to.sample, i * to.channels() + k, *v);
                }
            }
        }
        out
    }

    /// Convert a surface while reserving each replacement tile fallibly.
    pub fn try_convert(&self, to: PixelFormat) -> Result<Surface, AllocationError> {
        let mut out = Surface::with_default(to, &convert_pixel(&self.format, &to, &self.default_pixel()));
        let from_n = self.channels();
        let mut src = Vec::new();
        src.try_reserve_exact(from_n).map_err(|_| AllocationError::NotEnoughMemory { bytes: from_n * std::mem::size_of::<f32>() })?;
        src.resize(from_n, 0.0f32);
        for (c, t) in &self.tiles {
            let dst = out.try_tile_mut(*c)?;
            let px_count = (TILE_SIZE * TILE_SIZE) as usize;
            for i in 0..px_count {
                for (k, v) in src.iter_mut().enumerate() {
                    *v = read_sample(&t.data, self.format.sample, i * from_n + k);
                }
                let px = convert_pixel(&self.format, &to, &src);
                for (k, v) in px.iter().enumerate() {
                    write_sample(&mut dst.data, to.sample, i * to.channels() + k, *v);
                }
            }
        }
        Ok(out)
    }

    /// Composite-friendly RGBA read (converts from the surface's model).
    pub fn rgba(&self, x: i32, y: i32) -> [f32; 4] {
        let p = self.pixel(x, y);
        to_rgba(&self.format, &p)
    }
}

/// Decode `out.len()` consecutive samples starting at sample `first` of `bytes`, with the same
/// arithmetic as [`read_sample`] but one type dispatch per span, so the loop vectorises.
fn decode_span(bytes: &[u8], sample: SampleType, first: usize, out: &mut [f32]) {
    let size = sample.bytes();
    let Some(src) = bytes.get(first * size..(first + out.len()) * size) else {
        // Out of range: the per-sample path behaves as before.
        for (i, o) in out.iter_mut().enumerate() {
            *o = read_sample(bytes, sample, first + i);
        }
        return;
    };
    match sample {
        SampleType::U8 => out.iter_mut().zip(src).for_each(|(o, &b)| *o = b as f32 / 255.0),
        SampleType::U16 => out.iter_mut().zip(src.as_chunks::<2>().0).for_each(|(o, b)| *o = u16::from_ne_bytes(*b) as f32 / 65535.0),
        SampleType::F32 => out.iter_mut().zip(src.as_chunks::<4>().0).for_each(|(o, b)| *o = f32::from_ne_bytes(*b)),
    }
}

/// Encode `src` into consecutive samples starting at sample `first` of `bytes`, with the same
/// clamping and rounding as [`write_sample`] but one type dispatch per span.
fn encode_span(bytes: &mut [u8], sample: SampleType, first: usize, src: &[f32]) {
    let size = sample.bytes();
    let Some(dst) = bytes.get_mut(first * size..(first + src.len()) * size) else {
        for (i, v) in src.iter().enumerate() {
            write_sample(bytes, sample, first + i, *v);
        }
        return;
    };
    match sample {
        SampleType::U8 => dst.iter_mut().zip(src).for_each(|(d, &v)| *d = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8),
        SampleType::U16 => dst.as_chunks_mut::<2>().0.iter_mut().zip(src).for_each(|(d, &v)| *d = ((v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).to_ne_bytes()),
        SampleType::F32 => dst.as_chunks_mut::<4>().0.iter_mut().zip(src).for_each(|(d, &v)| *d = v.to_ne_bytes()),
    }
}

/// Encode normalised floats into one pixel's bytes.
pub fn encode_pixel(format: &PixelFormat, px: &[f32], out: &mut [u8]) {
    for (i, v) in px.iter().enumerate().take(format.channels()) {
        write_sample(out, format.sample, i, *v);
    }
}

pub fn decode_pixel(format: &PixelFormat, bytes: &[u8]) -> Vec<f32> {
    (0..format.channels()).map(|i| read_sample(bytes, format.sample, i)).collect()
}

/// Convert one pixel between formats via sRGB RGBA (CMYK through the built-in CMYK profile;
/// the engine's mode conversions use the document's profiles instead).
pub fn convert_pixel(from: &PixelFormat, to: &PixelFormat, px: &[f32]) -> Vec<f32> {
    if from.mode == to.mode {
        let n = from.mode.color_channels();
        let mut out: Vec<f32> = px[..n].to_vec();
        if to.alpha {
            out.push(if from.alpha { px[n] } else { 1.0 });
        }
        return out;
    }
    let rgba = to_rgba(from, px);
    from_rgba(to, rgba)
}

pub fn to_rgba(format: &PixelFormat, px: &[f32]) -> [f32; 4] {
    let n = format.mode.color_channels();
    let a = if format.alpha { px[n] } else { 1.0 };
    let rgb = match format.mode {
        ColorMode::Grayscale | ColorMode::Bitmap | ColorMode::Duotone => [px[0]; 3],
        ColorMode::Cmyk => photocraft_color::convert::cmyk_to_rgb([px[0], px[1], px[2], px[3]]),
        ColorMode::Lab => photocraft_color::convert::lab_to_srgb([px[0] * 100.0, px[1] * 255.0 - 128.0, px[2] * 255.0 - 128.0]),
        _ => [px[0], px[1], px[2]],
    };
    [rgb[0], rgb[1], rgb[2], a]
}

pub fn from_rgba(format: &PixelFormat, rgba: [f32; 4]) -> Vec<f32> {
    let rgb = [rgba[0], rgba[1], rgba[2]];
    let mut out: Vec<f32> = match format.mode {
        ColorMode::Grayscale | ColorMode::Bitmap | ColorMode::Duotone => vec![photocraft_color::convert::rgb_to_gray(rgb)],
        ColorMode::Cmyk => photocraft_color::convert::rgb_to_cmyk(rgb).to_vec(),
        ColorMode::Lab => {
            let l = photocraft_color::convert::srgb_to_lab(rgb);
            vec![l[0] / 100.0, (l[1] + 128.0) / 255.0, (l[2] + 128.0) / 255.0]
        }
        _ => rgb.to_vec(),
    };
    if format.alpha {
        out.push(rgba[3]);
    }
    out
}

/// [`from_rgba`] without allocating: writes `format.channels()` values into
/// `out` and returns how many were written.
#[inline]
pub fn from_rgba_into(format: &PixelFormat, rgba: [f32; 4], out: &mut [f32]) -> usize {
    let rgb = [rgba[0], rgba[1], rgba[2]];
    let n = match format.mode {
        ColorMode::Grayscale | ColorMode::Bitmap | ColorMode::Duotone => {
            out[0] = photocraft_color::convert::rgb_to_gray(rgb);
            1
        }
        ColorMode::Cmyk => {
            out[..4].copy_from_slice(&photocraft_color::convert::rgb_to_cmyk(rgb));
            4
        }
        ColorMode::Lab => {
            let l = photocraft_color::convert::srgb_to_lab(rgb);
            out[..3].copy_from_slice(&[l[0] / 100.0, (l[1] + 128.0) / 255.0, (l[2] + 128.0) / 255.0]);
            3
        }
        _ => {
            out[..3].copy_from_slice(&rgb);
            3
        }
    };
    if format.alpha {
        out[n] = rgba[3];
        n + 1
    } else {
        n
    }
}

/// A simple owned RGBA8 image, used for display and thumbnails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba8Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Rgba8Image {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, pixels: vec![0; width as usize * height as usize * 4] }
    }
}

/// Is this sample type able to hold values above 1.0 (HDR)?
pub fn is_hdr(sample: SampleType) -> bool {
    matches!(sample, SampleType::F32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::PixelFormat;
    use proptest::prelude::*;

    /// Large regions read and write their tiles in parallel (tile-row bands, tiles taken out and
    /// put back): every value, every tile and every shared tile must be what row-by-row (serial)
    /// calls leave, for every depth, a non-zero default pixel, missing and shared tiles.
    #[test]
    fn parallel_region_reads_and_writes_match_row_by_row_ones() {
        let r = Rect::new(-37, -300, 1150, 590);
        assert!(parallel(r));
        for (fmt, default) in [
            (PixelFormat::RGBA8, vec![0.0, 0.0, 0.0, 0.0]),
            (PixelFormat::new(ColorMode::Rgb, SampleType::U16, false), vec![1.0, 0.5, 0.25]),
            (PixelFormat::new(ColorMode::Grayscale, SampleType::F32, true), vec![0.75, 1.0]),
        ] {
            let n = fmt.channels();
            let mut base = Surface::with_default(fmt, &default);
            // Some tiles present (a patch), most missing.
            let patch = Rect::new(100, -40, 700, 300);
            let pv: Vec<f32> = (0..patch.width() as usize * patch.height() as usize * n).map(|i| ((i * 37) % 101) as f32 / 100.0).collect();
            base.write_region(patch, &pv);
            let snapshot = base.clone();
            let before = base.read_region(patch);
            let rows = |s: &Surface| -> Vec<f32> { (r.y0..r.y1).flat_map(|y| s.read_region(Rect::new(r.x0, y, r.x1, y + 1))).collect() };
            let bytes = |s: &Surface| -> Vec<u8> { (r.y0..r.y1).flat_map(|y| s.to_interleaved(Rect::new(r.x0, y, r.x1, y + 1))).collect() };
            assert_eq!(base.read_region(r), rows(&base), "{fmt:?}: parallel read");
            assert_eq!(base.to_interleaved(r), bytes(&base), "{fmt:?}: parallel interleaved read");
            let data: Vec<f32> = (0..r.width() as usize * r.height() as usize * n).map(|i| ((i * 7919) % 1009) as f32 / 1008.0).collect();
            let (mut par, mut ser) = (base.clone(), base.clone());
            par.write_region(r, &data);
            let w = r.width() as usize * n;
            for (k, y) in (r.y0..r.y1).enumerate() {
                ser.write_region(Rect::new(r.x0, y, r.x1, y + 1), &data[k * w..(k + 1) * w]);
            }
            assert_eq!(par.tiles.keys().collect::<Vec<_>>(), ser.tiles.keys().collect::<Vec<_>>(), "{fmt:?}: same tiles");
            for ((c, a), (_, b)) in par.tiles.iter().zip(&ser.tiles) {
                assert_eq!(a.data, b.data, "{fmt:?}: tile {c:?}");
            }
            let (mut par2, mut ser2) = (base.clone(), base.clone());
            let enc = par.to_interleaved(r);
            par2.write_interleaved(r, &enc);
            let bw = r.width() as usize * fmt.bytes_per_pixel();
            for (k, y) in (r.y0..r.y1).enumerate() {
                ser2.write_interleaved(Rect::new(r.x0, y, r.x1, y + 1), &enc[k * bw..(k + 1) * bw]);
            }
            for ((c, a), (_, b)) in par2.tiles.iter().zip(&ser2.tiles) {
                assert_eq!(a.data, b.data, "{fmt:?}: interleaved tile {c:?}");
            }
            // The snapshot taken before the writes still has its own pixels (shared tiles copied).
            assert!(snapshot.read_region(patch) == before && pv.len() == before.len(), "{fmt:?}: copy-on-write kept the snapshot");
            // Tiles outside the written region stay shared with the snapshot.
            let outside = Rect::new(-512, -1024, -256, -768);
            let mut far = base.clone();
            far.write_region(outside, &vec![0.5; 256 * 256 * n]);
            far.write_region(r, &data);
            assert!(far.tiles.iter().filter(|(c, _)| c.rect().intersect(&r).is_empty()).all(|(c, t)| base.tiles.get(c).is_none_or(|b| Arc::ptr_eq(b, t)) || c.rect() == outside));
        }
    }

    #[test]
    fn from_rgba_into_matches_from_rgba() {
        for fmt in [PixelFormat::RGBA8, PixelFormat::GRAY8, PixelFormat::GRAYA8, PixelFormat::CMYKA8, PixelFormat::new(ColorMode::Lab, SampleType::F32, true)] {
            let mut buf = [0.0f32; 8];
            let n = from_rgba_into(&fmt, [0.2, 0.5, 0.7, 0.4], &mut buf);
            assert_eq!(&buf[..n], &from_rgba(&fmt, [0.2, 0.5, 0.7, 0.4])[..]);
        }
    }

    /// The span conversions give exactly what the per-sample ones do, for every sample type,
    /// out-of-range and in-between values, and any start.
    #[test]
    fn span_conversions_match_per_sample() {
        let vals: Vec<f32> = (0..97).map(|i| (i as f32 - 10.0) / 77.0).chain([f32::NAN, -0.0, 1.0, 0.5 / 255.0, 0.5 / 65535.0]).collect();
        for sample in [SampleType::U8, SampleType::U16, SampleType::F32] {
            for first in [0, 1, 3] {
                let len = (first + vals.len()) * sample.bytes();
                let (mut a, mut b) = (vec![0u8; len], vec![0u8; len]);
                encode_span(&mut a, sample, first, &vals);
                for (i, v) in vals.iter().enumerate() {
                    write_sample(&mut b, sample, first + i, *v);
                }
                assert_eq!(a, b, "{sample:?} encode from {first}");
                let mut got = vec![0.0f32; vals.len()];
                decode_span(&a, sample, first, &mut got);
                let want: Vec<f32> = (0..vals.len()).map(|i| read_sample(&a, sample, first + i)).collect();
                assert_eq!(
                    got.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
                    want.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
                    "{sample:?} decode from {first}"
                );
            }
        }
    }

    /// The old decode-replace-encode way of putting `old`'s alpha back into the tiles that changed.
    fn copy_alpha_reference(old: &Surface, new: &mut Surface) {
        if old.format() != new.format() || !new.format().alpha {
            return;
        }
        let mut coords: Vec<_> = new.tiles().filter(|(c, t)| old.tile(**c).is_none_or(|o| !Arc::ptr_eq(o, t))).map(|(c, _)| *c).collect();
        coords.extend(old.tiles().filter(|(c, _)| new.tile(**c).is_none()).map(|(c, _)| *c));
        let n = new.channels();
        for c in coords {
            let r = c.rect();
            let o = old.read_region(r);
            let mut v = new.read_region(r);
            for (pn, po) in v.chunks_exact_mut(n).zip(o.chunks_exact(n)) {
                if let (Some(a), Some(b)) = (pn.last_mut(), po.last()) {
                    *a = *b;
                }
            }
            new.write_region(r, &v);
        }
        new.prune();
    }

    /// Byte copies of the alpha give exactly the old result: changed, shared, new-only and
    /// old-only tiles, non-zero default pixels, every depth, colour models, out-of-range floats.
    #[test]
    fn copy_alpha_matches_decode_and_encode() {
        let cmyk16 = PixelFormat::new(ColorMode::Cmyk, SampleType::U16, true);
        let lab32 = PixelFormat::new(ColorMode::Lab, SampleType::F32, true);
        for fmt in [
            PixelFormat::RGBA8,
            PixelFormat::RGBA16,
            PixelFormat::RGBA32F,
            PixelFormat::GRAYA8,
            PixelFormat::CMYKA8,
            cmyk16,
            lab32,
            PixelFormat::new(ColorMode::Rgb, SampleType::U8, false),
        ] {
            let n = fmt.channels();
            let mut seed = 9u32;
            let mut val = || {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed % 1000) as f32 / 900.0 - 0.05
            };
            let mut old = Surface::with_default(fmt, &vec![0.3; n]);
            for (x, y) in [(5, 5), (300, 10), (700, 600), (-40, -3)] {
                let px: Vec<f32> = (0..n).map(|_| val()).collect();
                old.fill_rect(Rect::new(x, y, x + 40, y + 30), &px);
            }
            let mut new = old.clone();
            new.default_pixel.copy_from_slice(&{
                let mut b = vec![0u8; fmt.bytes_per_pixel()];
                encode_pixel(&fmt, &vec![0.6; n], &mut b);
                b
            });
            // Change some tiles (one shared tile stays as it is), add new ones, drop one.
            for (x, y) in [(10, 8), (900, 900), (-30, 0)] {
                let px: Vec<f32> = (0..n).map(|_| val()).collect();
                new.fill_rect(Rect::new(x, y, x + 25, y + 20), &px);
            }
            new.tiles.remove(&TileCoord { tx: 2, ty: 2 });
            let mut want = new.clone();
            copy_alpha_reference(&old, &mut want);
            new.copy_alpha_from(&old);
            assert!(new == want, "{fmt:?}");
        }
    }

    #[test]
    fn zero_alloc_accessors_match_read_region() {
        for (fmt, default) in [
            PixelFormat::RGBA8,
            PixelFormat::RGBA16,
            PixelFormat::RGBA32F,
            PixelFormat::GRAYA8,
            PixelFormat::CMYKA8,
            PixelFormat::GRAY8,
            PixelFormat::GRAY8.with_sample(SampleType::F32),
            PixelFormat::new(ColorMode::Lab, SampleType::U16, true),
        ]
        .into_iter()
        .flat_map(|fmt| [0.0, 0.25, -0.0].map(|default| (fmt, default)))
        {
            let mut s = Surface::with_default(fmt, &vec![default; fmt.channels()]);
            let r = Rect::new(-300, -20, 300, 40);
            for (i, (x, y)) in [(-299, -19), (0, 0), (255, 39), (256, 10), (299, 0)].into_iter().enumerate() {
                let px: Vec<f32> = (0..fmt.channels()).map(|c| ((i * 3 + c) % 5) as f32 / 4.0).collect();
                s.write_pixel(x, y, &px);
            }
            let region = s.read_region(r);
            let mut into = vec![9.0; 3];
            s.read_region_into(r, &mut into);
            assert_eq!(region, into);
            let mut rgba = vec![[0.0; 4]; (r.width() * r.height()) as usize];
            s.read_rgba_into(r, &mut rgba);
            let mut rgba8 = vec![[0; 4]; rgba.len()];
            s.read_rgba8_into(r, &mut rgba8);
            let n = fmt.channels();
            for (i, p) in region.chunks_exact(n).enumerate() {
                let want = to_rgba(&fmt, p);
                assert_eq!(rgba8[i], want.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
                for c in 0..4 {
                    assert!((rgba[i][c] - want[c]).abs() < 1e-6, "{fmt:?} px {i}");
                }
                let (x, y) = (r.x0 + (i % r.width() as usize) as i32, r.y0 + (i / r.width() as usize) as i32);
                for c in 0..n {
                    let bits = s.sample_channel(x, y, c).to_bits();
                    assert_eq!(bits, p[c].to_bits());
                    assert_eq!(bits, into[i * n + c].to_bits());
                }
            }
            assert!(s.has_tiles_in(Rect::new(0, 0, 1, 1)));
            assert!(!s.has_tiles_in(Rect::new(1000, 1000, 1001, 1001)));
        }
    }

    #[test]
    fn fallible_region_reads_report_size_overflow() {
        let surface = Surface::new(PixelFormat::RGBA8);
        let huge = Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
        assert_eq!(surface.try_read_region(huge), Err(AllocationError::SizeOverflow));
        assert_eq!(surface.try_to_interleaved(huge), Err(AllocationError::SizeOverflow));
    }

    #[test]
    fn fallible_region_write_rejects_bad_input_without_mutating_surface() {
        let mut surface = Surface::new(PixelFormat::RGBA8);
        let before = surface.clone();
        assert_eq!(surface.try_write_region(Rect::new(0, 0, 2, 2), &[1.0]), Err(AllocationError::SizeOverflow));
        assert_eq!(surface, before);
    }

    #[test]
    fn fill_rect_shares_covered_tiles_and_edits_stay_local() {
        // A 300000² background used to materialize ~360 GB of tiles (#705).
        let mut s = Surface::new(PixelFormat::RGBA8);
        s.fill_rect(Rect::new(0, 0, 300_000, 300_000), &[1.0, 0.5, 0.0, 1.0]);
        let t = |s: &Surface, x, y| s.tile(TileCoord::new(x, y)).unwrap().clone();
        assert!(Arc::ptr_eq(&t(&s, 0, 0), &t(&s, 1000, 900)));
        // 300000 is not a multiple of 256: the edge tile is partial and stays its own.
        let edge = 300_000 / TILE_SIZE;
        assert!(!Arc::ptr_eq(&t(&s, 0, 0), &t(&s, edge, 0)));
        assert_eq!(s.pixel(299_999, 299_999), vec![1.0, 128.0 / 255.0, 0.0, 1.0]);
        assert_eq!(s.pixel(300_000, 0), vec![0.0; 4]);
        s.write_pixel(5, 5, &[0.0, 0.0, 1.0, 1.0]);
        assert_eq!(s.pixel(5, 5), vec![0.0, 0.0, 1.0, 1.0]);
        assert_eq!(s.pixel(300, 5), vec![1.0, 128.0 / 255.0, 0.0, 1.0]);
        assert!(!Arc::ptr_eq(&t(&s, 0, 0), &t(&s, 1, 0)));
    }

    #[test]
    fn empty_surface_reads_default() {
        let s = Surface::new(PixelFormat::RGBA8);
        assert_eq!(s.pixel(10, -500), vec![0.0; 4]);
        let m = Surface::with_default(PixelFormat::GRAY8, &[1.0]);
        assert_eq!(m.pixel(123_456, -9), vec![1.0]);
        assert_eq!(m.tile_count(), 0);
    }

    #[test]
    fn write_read_pixel_every_depth() {
        for sample in SampleType::ALL {
            let f = PixelFormat::RGBA8.with_sample(sample);
            let mut s = Surface::new(f);
            s.write_pixel(-3, 700, &[0.2, 0.4, 0.6, 1.0]);
            let p = s.pixel(-3, 700);
            for (a, b) in p.iter().zip([0.2, 0.4, 0.6, 1.0]) {
                assert!((a - b).abs() <= 1.0 / 255.0, "{sample:?}");
            }
            assert_eq!(s.tile_count(), 1);
            assert_eq!(s.tile_bounds(), TileCoord::containing(-3, 700).rect());
        }
    }

    #[test]
    fn copy_on_write_shares_untouched_tiles() {
        let mut a = Surface::new(PixelFormat::RGBA8);
        a.fill_rect(Rect::new(0, 0, 512, 512), &[1.0, 0.0, 0.0, 1.0]);
        let mut b = a.clone();
        b.write_pixel(10, 10, &[0.0, 1.0, 0.0, 1.0]);
        // a unchanged
        assert_eq!(a.pixel(10, 10), vec![1.0, 0.0, 0.0, 1.0]);
        assert_eq!(b.pixel(10, 10), vec![0.0, 1.0, 0.0, 1.0]);
        // only tile (0,0) was copied
        let shared = a.tiles().filter(|(c, t)| Arc::ptr_eq(t, b.tile(**c).unwrap())).count();
        assert_eq!(shared, 3);
    }

    #[test]
    fn content_bounds_and_prune() {
        let mut s = Surface::new(PixelFormat::RGBA8);
        s.fill_rect(Rect::new(300, 10, 310, 20), &[1.0, 1.0, 1.0, 1.0]);
        s.write_pixel(-5, -5, &[0.0, 0.0, 0.0, 1.0]);
        assert_eq!(s.content_bounds(), Rect::new(-5, -5, 310, 20));
        // erase the single pixel -> its tile becomes default and is pruned
        s.write_pixel(-5, -5, &[0.0, 0.0, 0.0, 0.0]);
        s.prune();
        assert_eq!(s.tile_count(), 1);
        assert_eq!(s.content_bounds(), Rect::new(300, 10, 310, 20));
    }

    #[test]
    fn interleaved_roundtrip_crossing_tiles() {
        let r = Rect::new(-20, 250, 280, 262);
        let bpp = PixelFormat::RGBA16.bytes_per_pixel();
        let bytes: Vec<u8> = (0..r.width() as usize * r.height() as usize * bpp).map(|i| (i * 31 % 251) as u8).collect();
        let s = Surface::from_interleaved(PixelFormat::RGBA16, r, &bytes);
        assert_eq!(s.to_interleaved(r), bytes);
        assert_eq!(s.tile_count(), 6); // x tiles -1..=1, y tiles 0..=1
    }

    #[test]
    fn region_outside_reads_default() {
        let s = Surface::with_default(PixelFormat::GRAY8, &[1.0]);
        let v = s.read_region(Rect::new(0, 0, 3, 3));
        assert_eq!(v, vec![1.0; 9]);
    }

    #[test]
    fn convert_depths_and_models() {
        let mut s = Surface::new(PixelFormat::RGBA8);
        s.write_pixel(1, 1, &[1.0, 1.0, 1.0, 1.0]);
        let g = s.convert(PixelFormat::GRAYA8);
        assert!((g.pixel(1, 1)[0] - 1.0).abs() < 1e-2);
        let f = s.convert(PixelFormat::RGBA32F);
        assert_eq!(f.pixel(1, 1), vec![1.0, 1.0, 1.0, 1.0]);
        let c = s.convert(PixelFormat::CMYKA8);
        let p = c.pixel(1, 1);
        assert!(p[3] < 0.01 && (p[4] - 1.0).abs() < 1e-6);
        let back = c.convert(PixelFormat::RGBA8);
        assert_eq!(back.pixel(1, 1), vec![1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn rgba_from_gray_and_cmyk() {
        let mut g = Surface::new(PixelFormat::GRAYA8);
        g.write_pixel(0, 0, &[0.5, 1.0]);
        let v = g.rgba(0, 0);
        assert!((v[0] - v[2]).abs() < 1e-6 && (v[3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn repeated_cmyk_reads_match_individual_pixels_bitwise() {
        let rect = Rect::new(-2, -1, 515, 2);
        for depth in SampleType::ALL {
            for alpha in [false, true] {
                let fmt = PixelFormat::new(ColorMode::Cmyk, depth, alpha);
                let mut s = Surface::with_default(fmt, &[0.2, 0.4, 0.6, 0.3, 0.7]);
                for (i, x) in (-1..514).enumerate() {
                    let c = if i / 2 % 256 == 0 { -0.0 } else { (i / 2 % 256) as f32 / 255.0 };
                    s.write_pixel(x, 0, &[c, 0.4, 0.6, 0.3, (i % 9) as f32 / 8.0]);
                }
                let mut out = vec![[0.0; 4]; (rect.width() * rect.height()) as usize];
                s.read_rgba_into(rect, &mut out);
                for (i, actual) in out.iter().enumerate() {
                    let expected = s.rgba(rect.x0 + (i % rect.width() as usize) as i32, rect.y0 + (i / rect.width() as usize) as i32);
                    assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits), "{depth:?}, alpha={alpha}, pixel={i}");
                }
            }
        }
    }

    #[test]
    fn translated_matches_reading_the_moved_pixels() {
        // Any offset (whole tiles, negative, across tile edges), every depth, a non-zero default.
        for fmt in [PixelFormat::RGBA8, PixelFormat::RGBA16, PixelFormat::RGBA32F, PixelFormat::GRAY8] {
            let mut s = Surface::with_default(fmt, &vec![0.25; fmt.channels()]);
            let r = Rect::new(-37, 200, 530, 300);
            let v: Vec<f32> = (0..r.width() * r.height() * fmt.channels() as u32).map(|i| (i % 251) as f32 / 250.0).collect();
            s.write_region(r, &v);
            for (dx, dy) in [(0, 0), (256, -512), (1, 0), (-3, 7), (300, -257), (-1000, 999)] {
                let t = s.translated(dx, dy, s.content_bounds());
                let moved = r.translate(dx, dy).inflate(3);
                assert_eq!(t.read_region(moved), s.read_region(moved.translate(-dx, -dy)), "{fmt:?} by ({dx}, {dy})");
                assert_eq!(t.default_pixel(), s.default_pixel());
                assert_eq!(t.to_interleaved(r.translate(dx, dy)), s.to_interleaved(r), "{fmt:?} by ({dx}, {dy}): not an exact copy");
            }
            // Whole tiles: the tiles are shared, not copied.
            let t = s.translated(512, -256, s.content_bounds());
            assert!(s.tiles().zip(t.tiles()).all(|((_, a), (_, b))| Arc::ptr_eq(a, b)));
            // Tiles without content (allocated, default pixels) are left behind.
            let mut padded = s.clone();
            padded.fill_rect(Rect::new(2000, 2000, 2100, 2100), &s.default_pixel());
            assert_eq!(padded.translated(5, 5, s.content_bounds()).tile_count(), s.translated(5, 5, s.content_bounds()).tile_count());
        }
        // Offsets at the limits don't panic.
        let mut s = Surface::new(PixelFormat::GRAY8);
        s.write_pixel(5, 5, &[1.0]);
        let all = Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
        let _ = s.translated(i32::MAX, i32::MIN, all);
        let _ = s.translated(i32::MIN + 1, 3, all);
    }

    proptest! {
        #[test]
        fn region_write_read_roundtrip(x in -600i32..600, y in -600i32..600, w in 1u32..90, h in 1u32..90, seed in any::<u32>()) {
            let r = Rect::from_xywh(x, y, w, h);
            let n = 4usize;
            let data: Vec<f32> = (0..(w * h) as usize * n).map(|i| ((i as u32).wrapping_mul(2654435761).wrapping_add(seed) % 256) as f32 / 255.0).collect();
            let mut s = Surface::new(PixelFormat::RGBA8);
            s.write_region(r, &data);
            let back = s.read_region(r);
            for (a, b) in back.iter().zip(&data) {
                prop_assert!((a - b).abs() < 1e-6);
            }
            prop_assert_eq!(s.content_bounds().union(&r), r);
        }

        #[test]
        fn clone_mutation_isolated(x in -300i32..300, y in -300i32..300) {
            let mut a = Surface::new(PixelFormat::RGBA8);
            a.fill_rect(Rect::new(-256, -256, 256, 256), &[0.5, 0.5, 0.5, 1.0]);
            let snapshot = a.clone();
            a.write_pixel(x, y, &[1.0, 0.0, 0.0, 1.0]);
            let expect = if Rect::new(-256, -256, 256, 256).contains(x, y) { vec![0.5f32, 0.5, 0.5, 1.0] } else { vec![0.0; 4] };
            let got = snapshot.pixel(x, y);
            for (g, e) in got.iter().zip(&expect) {
                prop_assert!((g - e).abs() <= 1.0 / 255.0);
            }
        }
    }

    #[test]
    fn strided_row_sampling_matches_sample_channel() {
        let mut s = Surface::with_default(PixelFormat::GRAY8, &[0.25]);
        s.fill_rect(Rect::new(-300, 10, 200, 40), &[1.0]);
        s.fill_rect(Rect::new(250, 10, 700, 20), &[0.5]);
        for (y, x0, step) in [(15, -520, 7), (15, -1, 1), (30, 3, 64), (500, 0, 9)] {
            let mut out = vec![0.0; 120];
            s.sample_row_strided(y, x0, step, 0, &mut out);
            for (i, v) in out.iter().enumerate() {
                assert_eq!(*v, s.sample_channel(x0 + i as i32 * step, y, 0), "y={y} x0={x0} step={step} i={i}");
            }
        }
    }

    #[test]
    fn content_bounds_with_tile_skipping() {
        // Interior content only; ring tiles present but all-default (unpruned).
        let mut s = Surface::new(PixelFormat::GRAY8);
        s.fill_rect(Rect::new(0, 0, 1024, 1024), &[0.0]);
        s.fill_rect(Rect::new(300, 310, 700, 720), &[1.0]);
        assert_eq!(s.content_bounds(), Rect::new(300, 310, 700, 720));
        // Full-canvas content plus a far-away speck.
        let mut s = Surface::new(PixelFormat::GRAY8);
        s.fill_rect(Rect::new(0, 0, 2000, 1500), &[1.0]);
        s.fill_rect(Rect::new(-700, 4000, -699, 4001), &[0.5]);
        assert_eq!(s.content_bounds(), Rect::new(-700, 0, 2000, 4001));
    }
}

#[cfg(test)]
mod echelon_case {
    use super::*;

    #[test]
    fn sparse_missing_tiles_read_the_default_after_a_shift() {
        let mut s = Surface::with_default(PixelFormat::GRAY8, &[1.0]);
        s.fill_rect(Rect::new(0, 0, 10, 10), &[0.0]);
        s.fill_rect(Rect::new(600, 0, 610, 10), &[0.0]);
        let t = s.translated(5, 0, Rect::new(0, 0, 768, 256));
        assert_eq!(t.pixel(305, 20), vec![1.0]);
    }

    /// The rewrite must not change output: the misaligned path is compared against a
    /// straightforward reference gather (read each pixel, write it shifted) over randomized
    /// formats, default pixels, sparse tiles and shifts that cross tile boundaries.
    #[test]
    fn randomized_shifts_match_a_reference_gather() {
        let mut rng: u64 = 0x1234_5678_9abc_def0;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for case in 0..200u32 {
            let fmt = [PixelFormat::RGBA8, PixelFormat::RGBA16, PixelFormat::RGBA32F, PixelFormat::GRAY8, PixelFormat::GRAYA8][(next() % 5) as usize];
            let def: Vec<f32> = (0..fmt.channels()).map(|i| if case % 3 == 0 { 0.0 } else { ((i + case as usize) % 7) as f32 / 6.0 }).collect();
            let mut s = Surface::with_default(fmt, &def);
            // Sparse content: a few small patches at random positions.
            for _ in 0..(1 + next() % 4) {
                let x = (next() % 1600) as i32 - 800;
                let y = (next() % 1200) as i32 - 600;
                let w = 1 + (next() % 90) as i32;
                let h = 1 + (next() % 70) as i32;
                let v: Vec<f32> = (0..fmt.channels()).map(|i| ((i as u64 + next()) % 251) as f32 / 250.0).collect();
                s.fill_rect(Rect::new(x, y, x + w, y + h), &v);
            }
            let content = Rect::new(-900, -700, 1700, 1300);
            let (dx, dy) = ((next() % 1200) as i32 - 600, (next() % 900) as i32 - 450);
            let t = s.translated(dx, dy, content);
            // Reference: every pixel of the moved extent reads the source at (x-dx, y-dy),
            // inside the content, through the surface's default-aware reader.
            for _ in 0..64 {
                let x = (next() % 1500) as i32 - 750 + dx;
                let y = (next() % 1100) as i32 - 550 + dy;
                if !content.translate(dx, dy).contains(x, y) {
                    continue;
                }
                let want = if content.contains(x - dx, y - dy) { s.pixel(x - dx, y - dy) } else { s.default_pixel() };
                let got = t.pixel(x, y);
                assert_eq!(got, want, "case {case} fmt {fmt:?} shift ({dx},{dy}) at ({x},{y})");
            }
        }
    }
}
