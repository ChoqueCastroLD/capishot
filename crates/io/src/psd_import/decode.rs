//! Layer pixels decoded apart from the layer walk (#psd-open): a record's channels become a
//! [`Surface`] with no access to the import's shared state, so the records of a file can be
//! decoded side by side ([`decode_records`], the threads builds and the desktop) or a band of rows
//! at a time between frames ([`Stepper`], the single-threaded browser build), and the walk
//! ([`super::Ctx::record_surface`]) then takes them as they are. What a damaged record reports is
//! kept with it and reported by the walk in its place, so warnings and failures read exactly as
//! when the walk decoded the record itself.

use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_geom::{Rect, TILE_SIZE};
use photocraft_psd::compression::Compression;
use photocraft_psd::layer::CHANNEL_TRANSPARENCY;
use photocraft_psd::{LayerNode, LayerRecord, PsdError, PsdFile};
use photocraft_raster::{Surface, UniformAt};

use crate::adjust_map::ADJUSTMENT_KEYS;
use crate::pixels::{interleave, max_sample, zero_sample};

/// A record's pixels, or what stopped them.
pub(crate) enum Decoded {
    /// The pixels, pruned, and their tiles that are one value throughout (shared across layers
    /// by the walk, which holds the import's interner).
    Pixels(Surface, UniformAt),
    /// Channels that could not be decoded (id and error, in the order found): the layer is empty.
    Failed(Vec<(i16, PsdError)>),
    /// Bounds of `w`×`h` pixels without any channel data: the layer is empty.
    NoData(usize, usize),
    /// No pixels to decode (empty or unusable bounds).
    Empty,
}

/// What a record's pixels are decoded into: the document's pixel format and the file's sample
/// layout.
#[derive(Clone, Copy)]
pub(crate) struct Layout {
    pub fmt: PixelFormat,
    /// Colour channels (the transparency comes after them).
    pub cc: usize,
    /// CMYK samples are stored inverted.
    pub cmyk: bool,
    pub depth: u16,
    pub version: photocraft_psd::Version,
}

impl Layout {
    pub(crate) fn of(file: &PsdFile, fmt: PixelFormat) -> Self {
        let cc = fmt.mode.color_channels();
        Layout { fmt, cc, cmyk: fmt.mode == ColorMode::Cmyk, depth: file.header.depth, version: file.header.version }
    }

    fn ids(&self) -> Vec<i16> {
        (0..self.cc as i16).chain([CHANNEL_TRANSPARENCY]).collect()
    }
}

/// Bytes of interleaved pixels a band decoded into tiles at once aims at: whole layers up to this
/// size decode in one band, larger ones in bands of whole tile rows.
pub(crate) const BAND_BYTES: usize = 32 << 20;

/// [`BAND_BYTES`] when records are decoded side by side: every worker holds a band at once.
const PARALLEL_BAND_BYTES: usize = 4 << 20;

/// Row ranges (`0..h`, relative to a layer whose first row is at document row `top`) to decode a
/// layer in: each ends on a tile row boundary, so no tile is written by two bands, and holds
/// about `band_bytes` of `row_bytes`-byte rows.
pub(crate) fn band_rows(top: i32, h: usize, row_bytes: usize, band_bytes: usize) -> Vec<(usize, usize)> {
    let tile = TILE_SIZE as usize;
    let rows = (band_bytes / row_bytes.max(1)).max(tile) / tile * tile;
    // The first band ends at the first tile boundary at least `rows` rows in.
    let into_tile = usize::try_from(top.rem_euclid(TILE_SIZE)).unwrap_or(0);
    let mut out = Vec::new();
    let mut r0 = 0;
    let mut r1 = rows - into_tile;
    while r0 < h {
        let end = r1.min(h);
        out.push((r0, end));
        r0 = end;
        r1 = end + rows;
    }
    out
}

/// A record whose pixels are being decoded, band by band.
struct Partial<'a> {
    rec: &'a LayerRecord,
    lay: Layout,
    w: usize,
    /// A decoder per channel present.
    decoders: Vec<Option<photocraft_psd::RowDecoder<'a>>>,
    surf: Surface,
    uniform: UniformAt,
    bands: Vec<(usize, usize)>,
    next: usize,
}

/// Row ranges (`0..h`) of about `band_bytes` of `row_bytes`-byte rows each, not aligned to tile
/// rows: a step's band can be a fraction of a tile row of a wide layer.
fn step_rows(h: usize, row_bytes: usize, band_bytes: usize) -> Vec<(usize, usize)> {
    let rows = (band_bytes / row_bytes.max(1)).max(1);
    (0..h).step_by(rows).map(|r0| (r0, (r0 + rows).min(h))).collect()
}

impl<'a> Partial<'a> {
    /// Starts `rec`: checks that its channels can be decoded, or says why not.
    /// `aligned`: bands end on tile rows (else [`step_rows`]).
    fn start(rec: &'a LayerRecord, lay: Layout, band_bytes: usize, aligned: bool, parallel: bool) -> Result<Self, Decoded> {
        let r = rec.rect;
        if r.is_empty() || r.size().is_err() {
            return Err(Decoded::Empty);
        }
        let (w, h) = r.size().unwrap_or((0, 0));
        let ids = lay.ids();
        // A decoder per channel present (a ZIP channel is inflated here); a channel that is in the
        // file but cannot be decoded leaves the layer empty, as the warning says. Filling in for
        // it would paint an opaque black layer over the document.
        let decoders = crate::pixels::par_map_if(parallel, ids.clone(), |id| rec.channel(id).map(|ch| ch.row_decoder(w, h, lay.depth, lay.version)));
        let mut failed = Vec::new();
        let mut ok = Vec::with_capacity(decoders.len());
        for (&id, d) in ids.iter().zip(decoders) {
            match d {
                Some(Err(e)) => failed.push((id, e)),
                Some(Ok(d)) => ok.push(Some(d)),
                None => ok.push(None),
            }
        }
        if !failed.is_empty() {
            return Err(Decoded::Failed(failed));
        }
        // Only decoded channel data justifies a buffer: the decoders bound their output by the
        // bytes in the file, but a bare rectangle can declare 300000² pixels in a few bytes (#755).
        if ok.iter().all(Option::is_none) {
            return Err(Decoded::NoData(w, h));
        }
        let row_bytes = w.saturating_mul(lay.cc + 1).saturating_mul(lay.fmt.sample.bytes());
        Ok(Partial { rec, lay, w, decoders: ok, surf: Surface::new(lay.fmt), uniform: UniformAt::default(), bands: if aligned { band_rows(r.top, h, row_bytes, band_bytes) } else { step_rows(h, row_bytes, band_bytes) },
            next: 0,
        })
    }

    /// Whether every band is in.
    fn done(&self) -> bool {
        self.next >= self.bands.len()
    }

    /// Decodes the next `n` bands (rows `b0..b1` of each, relative to the layer).
    fn bands(&mut self, n: usize, parallel: bool) -> Result<(), Decoded> {
        let (rec, lay, w) = (self.rec, self.lay, self.w);
        let h = self.bands.last().map_or(0, |b| b.1);
        let s = lay.fmt.sample;
        let ids = lay.ids();
        let mut fill: Vec<Vec<u8>> = vec![zero_sample(s); lay.cc];
        fill.push(max_sample(s));
        let mut invert = vec![lay.cmyk; lay.cc];
        invert.push(false);
        let end = self.next.saturating_add(n).min(self.bands.len());
        for i in self.next..end {
            let Some(&(b0, b1)) = self.bands.get(i) else { break };
            let planes: Vec<Option<photocraft_psd::Result<Vec<u8>>>> =
                crate::pixels::par_map_if(parallel, self.decoders.iter().collect(), |d| d.as_ref().map(|d| d.rows(b0, b1)));
            let mut refs: Vec<Option<&[u8]>> = Vec::with_capacity(planes.len());
            for (&id, p) in ids.iter().zip(&planes) {
                match p {
                    Some(Err(e)) => return Err(Decoded::Failed(vec![(id, e.clone())])),
                    Some(Ok(p)) => refs.push(Some(p)),
                    None => refs.push(None),
                }
            }
            let mut bytes = interleave(&refs, &fill, w * (b1 - b0), s, &invert);
            if lay.fmt.mode == ColorMode::Lab && s == SampleType::U16 {
                crate::pixels::lab16_chroma(&mut bytes, lay.cc + 1, true);
            }
            let r = rec.rect;
            self.surf.write_interleaved(Rect::new(r.left, r.top + b0 as i32, r.right, r.top + b1 as i32), &bytes);
            // The tile rows this band completed are pruned now, rather than the whole layer
            // scanned again at the end; a tile row a band ends inside waits for the next band.
            let from = if b0 == 0 { r.top } else { (r.top + b0 as i32).div_euclid(TILE_SIZE) * TILE_SIZE };
            let to = if b1 >= h { r.bottom } else { (r.top + b1 as i32).div_euclid(TILE_SIZE) * TILE_SIZE };
            if to > from {
                self.uniform.extend(self.surf.prune_in(Rect::new(r.left, from.max(r.top), r.right, to)));
            }
        }
        self.next = end;
        Ok(())
    }

    fn finish(self) -> Decoded {
        Decoded::Pixels(self.surf, self.uniform)
    }
}

/// `rec`'s pixels in one go; `parallel` decodes its channels side by side.
pub(crate) fn decode_record(rec: &LayerRecord, lay: Layout, parallel: bool) -> Decoded {
    decode_record_in(rec, lay, parallel, BAND_BYTES)
}

fn decode_record_in(rec: &LayerRecord, lay: Layout, parallel: bool, band_bytes: usize) -> Decoded {
    let mut p = match Partial::start(rec, lay, band_bytes, true, parallel) {
        Ok(p) => p,
        Err(d) => return d,
    };
    let n = p.bands.len();
    match p.bands(n, parallel) {
        Ok(()) => p.finish(),
        Err(d) => d,
    }
}

/// The records the layer walk will take pixels from, in walk order: every layer (not group) of
/// `nodes` with bounds that isn't an adjustment layer (whose records are never decoded). Type,
/// smart object, fill, shape and pixel layers all start from their record's pixels.
pub(crate) fn records_with_pixels(file: &PsdFile, nodes: &[LayerNode]) -> Vec<usize> {
    let layers = file.layers();
    let mut out = Vec::new();
    // The tree's depth is capped when it is built (`layer_tree_max_depth`); a stack keeps the
    // walk iterative all the same.
    let mut stack: Vec<std::slice::Iter<'_, LayerNode>> = vec![nodes.iter()];
    while let Some(top) = stack.last_mut() {
        match top.next() {
            None => {
                stack.pop();
            }
            Some(LayerNode::Layer { index }) => {
                if layers.get(*index).is_some_and(has_pixels) {
                    out.push(*index);
                }
            }
            Some(LayerNode::Group { children, .. }) => stack.push(children.iter()),
        }
    }
    out
}

/// Whether the walk decodes `rec`'s pixels (see [`records_with_pixels`]).
fn has_pixels(rec: &LayerRecord) -> bool {
    !rec.rect.is_empty()
        && rec.rect.size().is_ok()
        && !ADJUSTMENT_KEYS.into_iter().any(|k| rec.block(k).is_some())
        && super::unknown_adjustment_key(rec).is_none()
}

/// The pixels of `file`'s records `indices`, decoded side by side (on the rayon pool; in order on
/// the single-threaded browser build), each `Some` unless `cancelled` stopped it first.
pub(crate) fn decode_records(file: &PsdFile, lay: Layout, indices: &[usize], cancelled: &(dyn Fn() -> bool + Sync), done: &(dyn Fn() + Sync)) -> Vec<Option<Decoded>> {
    let layers = file.layers();
    crate::pixels::pool_map(indices.to_vec(), |i| {
        if cancelled() {
            return None;
        }
        let d = layers.get(i).map(|rec| decode_record_in(rec, lay, true, PARALLEL_BAND_BYTES));
        done();
        d
    })
}

/// Decodes a file's records a little at a time ([`Self::step`]), for an open that must hand the
/// thread back between frames.
pub(crate) struct Stepper {
    lay: Layout,
    /// Records still to decode (`indices[next..]`).
    indices: Vec<usize>,
    next: usize,
    /// The record in progress: its index, pixels so far, bands and bands done. Held as plain data:
    /// its channels' decoders (which borrow the file) are made again on each step, which for RLE
    /// only reads the row table; a record with a ZIP channel (inflated whole) goes in one step.
    current: Option<InProgress>,
    /// Decoded records, by record index.
    pub(crate) out: Vec<Option<Decoded>>,
}

/// A record a [`Stepper`] is part way through: its index, and its [`Partial`]'s state.
struct InProgress {
    index: usize,
    surf: Surface,
    uniform: UniformAt,
    bands: Vec<(usize, usize)>,
    next: usize,
}

/// Rows a step decodes of a layer at a time aim at this many bytes (of interleaved pixels): a
/// fraction of a millisecond, a few in the browser.
const STEP_BAND_BYTES: usize = 1 << 20;

impl Stepper {
    pub(crate) fn new(file: &PsdFile, lay: Layout, indices: Vec<usize>) -> Self {
        let n = file.layers().len();
        let mut out = Vec::new();
        out.resize_with(n, || None);
        Stepper { lay, indices, next: 0, current: None, out }
    }

    /// Records decoded so far, out of how many.
    pub(crate) fn progress(&self) -> (usize, usize) {
        (self.next, self.indices.len())
    }

    pub(crate) fn done(&self) -> bool {
        self.next >= self.indices.len()
    }

    pub(crate) fn layout(&self) -> Layout {
        self.lay
    }

    /// The records not decoded yet (a record in progress starts over).
    pub(crate) fn pending(&self) -> &[usize] {
        self.indices.get(self.next..).unwrap_or_default()
    }

    /// Records `indices` decoded elsewhere ([`decode_records`]): all pending ones are now in
    /// (`None`, cancelled, stays to be decoded by the walk).
    pub(crate) fn put(&mut self, indices: &[usize], decoded: Vec<Option<Decoded>>) {
        for (&i, d) in indices.iter().zip(decoded) {
            if let Some(slot) = self.out.get_mut(i) {
                *slot = d;
            }
        }
        self.current = None;
        self.next = self.indices.len();
    }

    /// The decoded records, by record index.
    pub(crate) fn into_decoded(self) -> Vec<Option<Decoded>> {
        self.out
    }

    /// Decodes bands of rows of `file`'s records until `more` says to stop (checked after each
    /// band, so a step does at least one) or all are in. True once all are.
    pub(crate) fn step(&mut self, file: &PsdFile, more: &dyn Fn() -> bool) -> bool {
        let layers = file.layers();
        loop {
            let Some(&index) = self.indices.get(self.next) else { return true };
            let Some(rec) = layers.get(index) else {
                self.next += 1;
                continue;
            };
            // ZIP channels are inflated whole by their decoders: such a record goes in one band.
            let zip = rec.channels.iter().any(|c| matches!(c.compression, Some(Compression::Zip | Compression::ZipPrediction)));
            let band_bytes = if zip { usize::MAX } else { STEP_BAND_BYTES };
            let resumed = match self.current.take() {
                Some(InProgress { index: i, surf, uniform, bands, next }) if i == index => {
                    Partial::start(rec, self.lay, band_bytes, zip, false).map(|p| Partial { surf, uniform, bands, next, ..p })
                }
                _ => Partial::start(rec, self.lay, band_bytes, zip, false),
            };
            let decoded = match resumed {
                Err(d) => Some(d),
                Ok(mut p) => match p.bands(1, false) {
                    Err(d) => Some(d),
                    Ok(()) if p.done() => Some(p.finish()),
                    Ok(()) => {
                        self.current = Some(InProgress { index, surf: p.surf, uniform: p.uniform, bands: p.bands, next: p.next });
                        None
                    }
                },
            };
            if let Some(d) = decoded {
                if let Some(slot) = self.out.get_mut(index) {
                    *slot = Some(d);
                }
                self.next += 1;
            }
            if self.done() {
                return true;
            }
            if !more() {
                return false;
            }
        }
    }
}
