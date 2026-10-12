//! The working pixels a live retouching stroke edits as it goes: a copy of the target layer, or of
//! the visible composite (Sample All Layers, Current & Below) rendered one 256-pixel tile at a time
//! the first time a dab reaches it. Live strokes never composite or scan the whole surface: on a
//! 4000 × 6000 page a stroke touches a handful of tiles, and only those are rendered, once.

use std::collections::HashSet;
use std::sync::Arc;

use photocraft_color::{PixelFormat, SampleType};
use photocraft_doc::{Document, LayerId};
use photocraft_geom::{Rect, TileCoord};
use photocraft_paint::retouch::Region;
use photocraft_raster::Surface;

use super::{SampleLayers, composite_region};

/// Pixels a live stroke reads and writes, in the target's channels (f32 working values).
pub(super) struct Scratch {
    surf: Surface,
    /// What to composite into tiles not rendered yet (`None`: `surf` already holds every pixel).
    lazy: Option<Lazy>,
}

struct Lazy {
    pre: Arc<Document>,
    id: Option<LayerId>,
    which: SampleLayers,
    /// The target's own format (composites are encoded in its channels).
    fmt: PixelFormat,
    done: HashSet<TileCoord>,
}

impl Scratch {
    /// The target surface itself (a copy-on-write clone: nothing is copied until written).
    pub(super) fn layer(surf: &Surface) -> Self {
        Self { surf: surf.clone(), lazy: None }
    }

    /// What `which` sees of `pre` (the composite of every layer, or of the target and those
    /// below it), rendered tile by tile on demand. Stored as 32-bit floats so the working copy
    /// is not quantised between dabs, as the commit's in-memory region isn't.
    pub(super) fn composite(pre: Arc<Document>, id: Option<LayerId>, which: SampleLayers, fmt: PixelFormat) -> Self {
        let work = PixelFormat { sample: SampleType::F32, ..fmt };
        Self { surf: Surface::new(work), lazy: Some(Lazy { pre, id, which, fmt, done: HashSet::new() }) }
    }

    /// Render the tiles of `r` not rendered yet.
    fn ensure(&mut self, r: Rect) {
        let Some(l) = self.lazy.as_mut() else { return };
        if r.is_empty() {
            return;
        }
        for t in r.tiles() {
            if l.done.insert(t) {
                let tr = t.rect();
                let reg = composite_region(&l.pre, l.id, l.which, tr, l.fmt);
                self.surf.write_region(tr, &reg.data);
            }
        }
    }

    /// The working pixels of `r`.
    pub(super) fn read(&mut self, r: Rect) -> Region {
        self.ensure(r);
        Region::read(&self.surf, r)
    }

    /// Store `reg` (its tiles are rendered first, so pixels around it stay the composite's).
    pub(super) fn write(&mut self, reg: &Region) {
        self.ensure(reg.rect);
        reg.write(&mut self.surf);
    }

    /// Tiles composited so far (tests: a live stroke must stay local).
    #[cfg(test)]
    pub(super) fn rendered_tiles(&self) -> usize {
        self.lazy.as_ref().map_or(0, |l| l.done.len())
    }
}
