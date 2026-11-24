//! The region an AI fill works on, kept in blocks of the document's tile size instead of whole.
//!
//! A fill used to hold its region as floats several times over (the layer's pixels, their RGB,
//! the coverage three times, the result: some 60 bytes a pixel), so cleaning a large area on a
//! 4 GB machine took gigabytes beside the model. Here, as an editor's tiled image does, a block is
//! read from its [`Source`] when the first tile that reads it starts, and handed to its [`Sink`]
//! (written into the result, in the document's format) as soon as no later tile of the plan reads
//! it, then dropped. What stays for the whole run is one flag a pixel for the hole (the plan needs
//! all of it) and the blocks under the tiles being worked on: the plan goes top to bottom, so that
//! is a band about a tile high.

use photocraft_ai_lama::tiles::{Rect, Tile};

use crate::Result;

/// A block's side: the document's tile, so a block written back touches whole tiles.
pub(crate) const BLOCK: usize = photocraft_geom::TILE_SIZE as usize;

/// Where the region's pixels come from (coordinates within the region).
pub(crate) trait Source: Send {
    /// The hole's coverage (0..=1) over `r`, row by row, into `out` (cleared first).
    fn cover(&mut self, r: Rect, out: &mut Vec<f32>);
    /// What the model sees over `r`: interleaved RGB in 0..=1, into `out` (cleared first).
    fn rgb(&mut self, r: Rect, out: &mut Vec<f32>);
}

/// Where the filled blocks go (coordinates within the region).
pub(crate) trait Sink: Send {
    /// Block `r` is done: `rgb` its pixels with the fill in the hole, `cover` the hole's coverage.
    /// A run that starts its tiles over (another model after a GPU failure) hands a block again:
    /// the newer one replaces it.
    fn put(&mut self, r: Rect, rgb: &[f32], cover: &[f32]) -> Result<()>;
}

struct Block {
    rgb: Vec<f32>,
    cover: Vec<f32>,
}

/// See the module docs.
pub(crate) struct Work {
    w: usize,
    h: usize,
    /// Where block edges fall: region x `- ox` is a multiple of [`BLOCK`] at each.
    ox: usize,
    oy: usize,
    cols: usize,
    hole: Vec<bool>,
    blocks: Vec<Option<Block>>,
    /// The last tile of the plan that reads each block.
    last: Vec<Option<usize>>,
    source: Box<dyn Source>,
    sink: Box<dyn Sink>,
    /// Blocks held now, and the most at once.
    held: usize,
    peak: usize,
}

impl Work {
    /// A `w×h` region whose blocks start `phase` pixels before its corner (the document's tile
    /// grid), reading its hole from `source` now.
    pub fn new(w: usize, h: usize, phase: (usize, usize), mut source: Box<dyn Source>, sink: Box<dyn Sink>) -> Self {
        let (ox, oy) = (phase.0 % BLOCK, phase.1 % BLOCK);
        let (cols, rows) = ((w + ox).div_ceil(BLOCK), (h + oy).div_ceil(BLOCK));
        let mut hole = vec![false; w * h];
        let mut cover = Vec::new();
        // A band of block rows at a time: never the whole region's coverage.
        for by in 0..rows {
            let y0 = (by * BLOCK).saturating_sub(oy);
            let y1 = ((by + 1) * BLOCK - oy).min(h);
            source.cover(Rect { x: 0, y: y0, w, h: y1 - y0 }, &mut cover);
            for (dst, c) in hole[y0 * w..y1 * w].iter_mut().zip(&cover) {
                *dst = *c > 0.0;
            }
        }
        let n = cols * rows;
        Self { w, h, ox, oy, cols, hole, blocks: (0..n).map(|_| None).collect(), last: vec![None; n], source, sink, held: 0, peak: 0 }
    }

    /// The region's size.
    pub fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    /// Where the hole is, a flag a pixel.
    pub fn hole(&self) -> &[bool] {
        &self.hole
    }

    fn block_rect(&self, b: usize) -> Rect {
        let (bx, by) = (b % self.cols, b / self.cols);
        let x0 = (bx * BLOCK).saturating_sub(self.ox);
        let y0 = (by * BLOCK).saturating_sub(self.oy);
        Rect { x: x0, y: y0, w: ((bx + 1) * BLOCK - self.ox).min(self.w) - x0, h: ((by + 1) * BLOCK - self.oy).min(self.h) - y0 }
    }

    fn blocks_of(&self, r: Rect) -> impl Iterator<Item = usize> + use<> {
        let (ox, oy, cols) = (self.ox, self.oy, self.cols);
        let (bx0, bx1) = ((r.x + ox) / BLOCK, (r.x + r.w + ox - 1) / BLOCK);
        let (by0, by1) = ((r.y + oy) / BLOCK, (r.y + r.h + oy - 1) / BLOCK);
        (by0..=by1).flat_map(move |by| (bx0..=bx1).map(move |bx| by * cols + bx))
    }

    /// The plan the tiles run in, from the first: each block is let go after the last that reads it.
    pub fn uses(&mut self, plan: &[Tile]) {
        self.last.fill(None);
        for (i, t) in plan.iter().enumerate() {
            for b in self.blocks_of(t.rect).collect::<Vec<_>>() {
                self.last[b] = Some(i);
            }
        }
    }

    fn load(&mut self, b: usize) {
        if self.blocks[b].is_some() {
            return;
        }
        let r = self.block_rect(b);
        let (mut rgb, mut cover) = (Vec::with_capacity(r.w * r.h * 3), Vec::with_capacity(r.w * r.h));
        self.source.rgb(r, &mut rgb);
        self.source.cover(r, &mut cover);
        rgb.resize(r.w * r.h * 3, 0.0);
        cover.resize(r.w * r.h, 0.0);
        self.blocks[b] = Some(Block { rgb, cover });
        self.held += 1;
        self.peak = self.peak.max(self.held);
    }

    /// The RGB and coverage of `r` (a tile's crop), as buffers of just `r`.
    pub fn cut(&mut self, r: Rect) -> (Vec<f32>, Vec<f32>) {
        let (mut rgb, mut cover) = (vec![0.0; r.w * r.h * 3], vec![0.0; r.w * r.h]);
        for b in self.blocks_of(r).collect::<Vec<_>>() {
            self.load(b);
            let br = self.block_rect(b);
            let Some(blk) = &self.blocks[b] else { continue };
            for y in r.y.max(br.y)..(r.y + r.h).min(br.y + br.h) {
                let (x0, x1) = (r.x.max(br.x), (r.x + r.w).min(br.x + br.w));
                let (s, d) = ((y - br.y) * br.w + x0 - br.x, (y - r.y) * r.w + x0 - r.x);
                let n = x1 - x0;
                rgb[d * 3..(d + n) * 3].copy_from_slice(&blk.rgb[s * 3..(s + n) * 3]);
                cover[d..d + n].copy_from_slice(&blk.cover[s..s + n]);
            }
        }
        (rgb, cover)
    }

    /// Writes `rgb` (of `r`, after a tile's paste) back into the blocks.
    pub fn put_back(&mut self, r: Rect, rgb: &[f32]) {
        for b in self.blocks_of(r).collect::<Vec<_>>() {
            let br = self.block_rect(b);
            let Some(blk) = &mut self.blocks[b] else { continue };
            for y in r.y.max(br.y)..(r.y + r.h).min(br.y + br.h) {
                let (x0, x1) = (r.x.max(br.x), (r.x + r.w).min(br.x + br.w));
                let (d, s) = ((y - br.y) * br.w + x0 - br.x, (y - r.y) * r.w + x0 - r.x);
                let n = x1 - x0;
                blk.rgb[d * 3..(d + n) * 3].copy_from_slice(&rgb[s * 3..(s + n) * 3]);
            }
        }
    }

    /// Hands the sink every held block no tile after `done` reads.
    pub fn retire(&mut self, done: usize) -> Result<()> {
        for b in 0..self.blocks.len() {
            if self.last[b].is_none_or(|l| l <= done) {
                self.hand_over(b)?;
            }
        }
        Ok(())
    }

    fn hand_over(&mut self, b: usize) -> Result<()> {
        let Some(blk) = self.blocks[b].take() else { return Ok(()) };
        self.held -= 1;
        let r = self.block_rect(b);
        self.sink.put(r, &blk.rgb, &blk.cover)
    }

    /// Hands the sink every block still held: the fill is done.
    pub fn finish(mut self) -> Result<usize> {
        for b in 0..self.blocks.len() {
            self.hand_over(b)?;
        }
        Ok(self.peak)
    }
}

/// A region already in memory (`rgb` interleaved, `cover`), for a fill of a small one.
pub(crate) struct Buffers {
    pub rgb: Vec<f32>,
    pub cover: Vec<f32>,
    pub w: usize,
}

impl Source for Buffers {
    fn cover(&mut self, r: Rect, out: &mut Vec<f32>) {
        out.clear();
        for y in r.y..r.y + r.h {
            out.extend_from_slice(&self.cover[y * self.w + r.x..y * self.w + r.x + r.w]);
        }
    }
    fn rgb(&mut self, r: Rect, out: &mut Vec<f32>) {
        out.clear();
        for y in r.y..r.y + r.h {
            out.extend_from_slice(&self.rgb[(y * self.w + r.x) * 3..(y * self.w + r.x + r.w) * 3]);
        }
    }
}

/// Drops what it is handed (a run for its own sake: `ai.warm`).
pub(crate) struct Discard;

impl Sink for Discard {
    fn put(&mut self, _: Rect, _: &[f32], _: &[f32]) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Collects what it is handed into a whole-region RGB buffer.
    struct Collect(Arc<Mutex<(Vec<f32>, usize)>>);

    impl Sink for Collect {
        fn put(&mut self, r: Rect, rgb: &[f32], _cover: &[f32]) -> Result<()> {
            let mut g = self.0.lock().unwrap();
            let w = g.1;
            for y in 0..r.h {
                let d = ((r.y + y) * w + r.x) * 3;
                g.0[d..d + r.w * 3].copy_from_slice(&rgb[y * r.w * 3..(y + 1) * r.w * 3]);
            }
            Ok(())
        }
    }

    fn region(w: usize, h: usize) -> Buffers {
        let rgb = (0..w * h * 3).map(|i| (i % 97) as f32 / 97.0).collect();
        let cover = (0..w * h).map(|i| if (i % w) % 7 == 0 { 1.0 } else { 0.0 }).collect();
        Buffers { rgb, cover, w }
    }

    /// Blocks are let go as the plan moves down: a tall region holds a band, not all of it, and
    /// what the sink gets is what the tiles wrote.
    #[test]
    fn a_tall_region_holds_a_band_of_blocks_and_hands_every_one_over() {
        let (w, h) = (600, 3000);
        let src = region(w, h);
        let want_in = src.rgb.clone();
        let out = Arc::new(Mutex::new((vec![-1.0; w * h * 3], w)));
        let mut work = Work::new(w, h, (10, 20), Box::new(src), Box::new(Collect(out.clone())));
        assert_eq!(work.hole().iter().filter(|h| **h).count(), w.div_ceil(7) * h);
        let tile = |y: usize| Tile {
            rect: Rect { x: 0, y, w, h: 300 },
            core: Rect { x: 0, y, w, h: 300 },
            work_w: 600,
            work_h: 304,
            fit: photocraft_ai_lama::tiles::Fit::Pad,
            split: false,
        };
        let plan: Vec<Tile> = (0..10).map(|i| tile(i * 300)).collect();
        work.uses(&plan);
        for (i, t) in plan.iter().enumerate() {
            let (mut rgb, _) = work.cut(t.rect);
            rgb.iter_mut().for_each(|v| *v = 1.0 - *v);
            work.put_back(t.rect, &rgb);
            work.retire(i).unwrap();
        }
        let peak = work.finish().unwrap();
        assert!(peak <= 3 * 3, "at most the blocks under one tile and its neighbours: {peak}");
        let got = out.lock().unwrap().0.clone();
        assert!(got.iter().zip(&want_in).all(|(g, v)| (g - (1.0 - v)).abs() < 1e-6), "every block came back once, as written");
    }
}
