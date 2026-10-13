//! Floating selection: the selected pixels of a layer cut out and moved as a piece, without
//! touching the document until the piece is dropped.
//!
//! - `select.float {"dx","dy","copy"}` cuts the selected pixels of the active layer into a floating
//!   piece (the first time; `copy` leaves them and floats a duplicate) and moves it by whole
//!   pixels. Further calls move the same piece; nothing new is cut. The Move tool, the marquee and
//!   lasso ⌘-drags and the Move tool's arrow keys all move selected pixels this way.
//! - `select.drop` drops the piece into its layer and moves the selection with it, as one history
//!   step.
//! - Any other command drops the piece first (so it lands exactly where it was shown), except Undo,
//!   which puts it back where it was cut (nothing was changed yet).
//!
//! While floating, the document is unchanged; [`displayed`] gives the document with the piece at
//! its offset, for the canvas. The selection stays where it was until the drop.

use std::sync::Arc;

use photocraft_color::PixelFormat;
use photocraft_doc::{Document, LayerContent, LayerId};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{DocState, EngineError, Result, Session};

/// A layer split by its selection: the layer with the selected pixels cut out, and the cut pixels
/// (cropped to the selection). Splitting touches only the selection's area, and placing the piece
/// ([`CutParts::moved`]) only the piece's: the cut-out layer's other tiles are shared.
#[derive(Clone, Debug)]
pub struct CutParts {
    pub layer: LayerId,
    rest: Surface,
    piece: Surface,
    /// `piece`'s content bounds, scanned once: a drag places the piece at a new offset every frame.
    piece_bounds: Rect,
    /// The piece's own layer in [`CutParts::split`] documents: one id for the whole float, so the
    /// GPU canvas keeps its pixels resident while it moves.
    piece_layer: LayerId,
    /// The pieces are the layer's mask (`"target":"mask"`), not its pixels.
    mask: bool,
}

impl CutParts {
    /// Split `layer` of `doc` by its selection. With `copy` the selected pixels stay in the layer
    /// too (the piece is a duplicate). Without, they leave a transparent hole, or on the Background,
    /// which can't hold transparency, one filled with `background` (Photoshop). Errors without a
    /// selection or pixels. With `mask` the layer's mask is split instead of its pixels; the hole it
    /// leaves takes the background colour (as on the Background layer).
    pub fn new(doc: &Document, layer: LayerId, copy: bool, background: [f32; 4], mask: bool) -> Result<Self> {
        let sel = doc.selection.as_ref().ok_or_else(|| EngineError::Other("no selection".into()))?;
        let l = doc.layer(layer).ok_or(EngineError::NoLayer(layer))?;
        let surf = if mask {
            &l.mask.as_ref().ok_or_else(|| EngineError::Other("the layer has no mask".into()))?.surface
        } else {
            let LayerContent::Raster(surf) = &l.content else {
                return Err(EngineError::Other("the layer has no pixels to move".into()));
            };
            surf
        };
        let fmt = surf.format();
        let with_alpha = PixelFormat::new(fmt.mode, fmt.sample, true);
        let full = if fmt == with_alpha { surf.clone() } else { surf.convert(with_alpha) };
        // A mask has no alpha: it keeps its format, and the cut leaves background colour.
        let mut rest = if mask { surf.clone() } else { full.clone() };
        let mut piece = Surface::new(with_alpha);
        let fill = !copy && (mask || crate::extra_cmds::is_background(l));
        // A mask's untouched pixels (its default, e.g. white) move too, so take the whole canvas.
        // Bounds cached per tile: the canvas has usually scanned these tiles already, and a full
        // page layer's scan was most of the cut (#578).
        let bounds = photocraft_compose::bounds::content_bounds;
        let b = bounds(sel).intersect(&if mask { doc.bounds() } else { bounds(surf) });
        if !b.is_empty() {
            let n = with_alpha.channels();
            let a = n - 1;
            let mut rp = full.read_region(b);
            let mut pp = rp.clone();
            let w = b.width() as usize;
            // The selection's coverage over `b`, a row at a time (one tile lookup per run).
            let mut cover = vec![0.0f32; w];
            for (i, (p, r)) in pp.chunks_exact_mut(n).zip(rp.chunks_exact_mut(n)).enumerate() {
                if i % w == 0 {
                    sel.sample_row_strided(b.y0 + (i / w) as i32, b.x0, 1, 0, &mut cover);
                }
                let k = cover.get(i % w).copied().unwrap_or(0.0);
                if k <= 0.0 {
                    p.fill(0.0);
                } else {
                    p[a] *= k;
                }
                if !copy && !fill {
                    r[a] *= 1.0 - k;
                }
            }
            if !mask {
                rest.write_region(b, &rp);
            }
            if fill {
                crate::pixels::try_fill_surface(&mut rest, b, background, Some(sel), true)?;
            }
            rest.prune();
            piece.write_region(b, &pp);
            piece.prune();
        }
        let piece_bounds = piece.content_bounds();
        Ok(Self { layer, rest, piece, piece_bounds, piece_layer: LayerId::fresh(), mask })
    }

    /// The cut pixels where they were cut (straight alpha).
    pub fn piece(&self) -> &Surface {
        &self.piece
    }

    /// The id of the piece's layer in [`Self::split`] documents.
    pub fn piece_layer(&self) -> LayerId {
        self.piece_layer
    }

    /// The piece moved by whole pixels (dx, dy).
    fn piece_at(&self, dx: i32, dy: i32) -> Surface {
        self.piece.translated(dx, dy, self.piece_bounds)
    }

    /// `doc` as [`Self::moved`] shows it, but with the piece (moved by dx, dy) as a layer of its own
    /// ([`Self::piece_layer`]) right above the cut-out layer instead of composited into it, when
    /// the two composite exactly alike: a visible pixel layer in Normal mode at full opacity and
    /// fill, without masks, effects, clipping, Blend If, channel or advanced blending options, and
    /// no clipped layer above it to clip to the piece instead. The cut-out layer is then the same
    /// surface at every offset and only the piece moves, so a canvas that keeps layer pixels
    /// resident (the GPU canvas: `photocraft_gpu::Translated`) uploads nothing new while it is
    /// dragged. `None` otherwise (the caller shows [`Self::moved`]).
    pub fn split(&self, doc: &Document, dx: i32, dy: i32) -> Option<Document> {
        if self.mask {
            return None;
        }
        let path = doc.path_of(self.layer)?;
        let l = doc.layer_at(&path)?;
        let plain = l.visible
            && matches!(l.content, LayerContent::Raster(_))
            && l.blend == photocraft_color::BlendMode::Normal
            && l.opacity >= 1.0
            && l.fill_opacity >= 1.0
            && !l.clipped
            && l.mask.is_none()
            && l.vector_mask.is_none()
            && !photocraft_compose::effects::has_effects(l)
            && l.excluded_channels == 0
            && l.blend_if == photocraft_doc::BlendIf::default()
            && l.advanced == photocraft_doc::AdvancedBlending::default()
            && l.video.is_none();
        let (&last, parent) = path.split_last()?;
        let siblings = if parent.is_empty() { &doc.layers[..] } else { doc.layer_at(parent)?.children()? };
        let clips_above = siblings.get(last + 1).is_some_and(|above| above.clipped);
        if !plain || clips_above {
            return None;
        }
        let mut d = doc.clone();
        *d.layer_mut(self.layer)?.surface_mut()? = self.rest.clone();
        let mut piece = photocraft_doc::Layer::new(l.name.clone(), LayerContent::Raster(self.piece_at(dx, dy)));
        piece.id = self.piece_layer;
        d.insert_above(Some(self.layer), piece);
        Some(d)
    }

    /// `doc` with the piece moved by whole pixels (dx, dy) over the cut-out layer.
    pub fn moved(&self, doc: &Document, dx: i32, dy: i32) -> Result<Document> {
        let mut d = doc.clone();
        self.place(&mut d, dx, dy)?;
        Ok(d)
    }

    /// Put the piece, moved by (dx, dy), over the cut-out layer (or mask) of `doc`.
    fn place(&self, doc: &mut Document, dx: i32, dy: i32) -> Result<()> {
        let l = doc.layer_mut(self.layer).ok_or(EngineError::NoLayer(self.layer))?;
        let surf = if self.mask {
            l.mask.as_mut().map(|m| &mut m.surface).ok_or_else(|| EngineError::Other("the layer has no mask".into()))?
        } else {
            l.surface_mut().ok_or_else(|| EngineError::Other("the layer has no pixels to move".into()))?
        };
        let mut out = self.rest.clone();
        let piece = self.piece_at(dx, dy);
        // The moved piece's content bounds: its scanned ones, moved.
        let b = if self.piece_bounds.is_empty() { Rect::EMPTY } else { self.piece_bounds.translate(dx, dy) };
        if self.mask {
            crate::fill_cmds::composite_over(&mut out, &piece, b, None)?;
        } else {
            crate::transform_cmds::composite_over_in(&mut out, &piece, b);
        }
        *surf = out;
        Ok(())
    }
}

/// A piece floating `offset` whole pixels from where it was cut, over document `revision`.
#[derive(Clone, Debug)]
pub struct Floating {
    pub layer: LayerId,
    pub offset: (i32, i32),
    pub revision: u64,
    pub parts: Arc<CutParts>,
}

/// The floating piece of `st`, if it is still valid (the document hasn't changed under it).
pub fn floating(st: &DocState) -> Option<&Floating> {
    st.floating.as_ref().filter(|f| f.revision == st.revision && st.doc.selection.is_some())
}

/// The document as shown while a piece floats `extra` further than its offset (a drag of it in
/// progress); `None` when nothing floats.
pub fn displayed(st: &DocState, extra: (i32, i32)) -> Option<Document> {
    let f = floating(st)?;
    f.parts.moved(&st.doc, f.offset.0 + extra.0, f.offset.1 + extra.1).ok()
}

/// [`displayed`] for the canvas: the piece as a layer of its own over the cut-out layer
/// ([`CutParts::split`]) where that looks the same, so each frame of a drag only moves the piece.
pub fn displayed_split(st: &DocState, extra: (i32, i32)) -> Option<Document> {
    let f = floating(st)?;
    let (dx, dy) = (f.offset.0.saturating_add(extra.0), f.offset.1.saturating_add(extra.1));
    f.parts.split(&st.doc, dx, dy).or_else(|| f.parts.moved(&st.doc, dx, dy).ok())
}

fn int_param(p: &Value, key: &str) -> i32 {
    p.get(key).and_then(Value::as_f64).filter(|v| v.is_finite()).map_or(0, |v| v.round().clamp(-1e7, 1e7) as i32)
}

fn can_float(s: &Session) -> std::result::Result<(), String> {
    let st = s.active().ok_or("no document")?;
    if floating(st).is_some() {
        return Ok(());
    }
    if st.doc.selection.is_none() {
        return Err("there is no selection".into());
    }
    let l = st.active_layer.and_then(|id| st.doc.layer(id)).ok_or("no active layer")?;
    if !matches!(l.content, LayerContent::Raster(_)) {
        return Err("the active layer has no pixels to move".into());
    }
    if locked_for_float(&st.doc, l) {
        return Err(format!("layer \"{}\" is locked", l.name));
    }
    Ok(())
}

/// Do `l`'s locks (its own, or a locked group's: `effective_locks`) stop its selected pixels
/// floating? The Background is position-locked, but its selected pixels move (Photoshop).
pub fn locked_for_float(doc: &Document, l: &photocraft_doc::Layer) -> bool {
    let locks = doc.effective_locks(l.id);
    locks.all || (locks.position && !crate::extra_cmds::is_background(l))
}

fn float(s: &mut Session, p: &Value) -> Result<Value> {
    let (dx, dy) = (int_param(p, "dx"), int_param(p, "dy"));
    let copy = p.get("copy").and_then(Value::as_bool).unwrap_or(false);
    let background = s.tools.background;
    let st = s.active_mut().ok_or(EngineError::NoDocument)?;
    if floating(st).is_none() {
        let layer = st.active_layer.ok_or_else(|| EngineError::Other("no active layer".into()))?;
        let parts = CutParts::new(&st.doc, layer, copy, background, crate::commands::is_mask_target(p))?;
        st.floating = Some(Floating { layer, offset: (0, 0), revision: st.revision, parts: Arc::new(parts) });
    }
    let f = st.floating.as_mut().ok_or(EngineError::NoDocument)?;
    f.offset = (f.offset.0.saturating_add(dx), f.offset.1.saturating_add(dy));
    Ok(json!({"layer": f.layer.0, "offset": [f.offset.0, f.offset.1]}))
}

/// Drop the floating piece into its layer and move the selection with it (one history step).
pub fn drop_floating(s: &mut Session) -> Result<Value> {
    let Some(st) = s.active_mut() else { return Ok(json!({"dropped": false})) };
    let Some(f) = floating(st).cloned() else {
        st.floating = None;
        return Ok(json!({"dropped": false}));
    };
    st.floating = None;
    let (dx, dy) = f.offset;
    if (dx, dy) == (0, 0) {
        return Ok(json!({"dropped": true, "offset": [0, 0]}));
    }
    s.edit("Move", |doc, _| {
        f.parts.place(doc, dx, dy)?;
        doc.selection = doc.selection.as_ref().map(|sel| photocraft_algo::resample::translate_surface(sel, dx, dy));
        Ok(())
    })?;
    Ok(json!({"dropped": true, "offset": [dx, dy]}))
}

/// Commands that leave a floating piece floating: its own, and ones that don't touch documents.
fn keeps_floating(id: &str) -> bool {
    matches!(id, "select.float" | "select.drop") || ["view.", "window.", "help.", "prefs."].iter().any(|p| id.starts_with(p))
}

/// Before any command (`Session::dispatch`): Undo puts a floating piece back (and is used up:
/// returns its result); every other command but the floating ones drops it first.
pub(crate) fn before_command(s: &mut Session, id: &str) -> Result<Option<Value>> {
    let Some(st) = s.active_mut() else { return Ok(None) };
    if floating(st).is_none() {
        st.floating = None;
        return Ok(None);
    }
    match id {
        "edit.undo" | "edit.stepBackward" => {
            st.floating = None;
            Ok(Some(json!({"floating": "returned"})))
        }
        _ if keeps_floating(id) => Ok(None),
        _ => drop_floating(s).map(|_| None),
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "select.float",
            label: "Float Selection",
            menu: &[],
            shortcut: None,
            params: r##"{"dx":px=0,"dy":px=0,"copy":bool=false,"target":"pixels"|"mask"="pixels" (mask: float the selected part of the layer's mask, the hole takes the background colour)} → {layer, offset} (cuts the selected pixels of the active layer into a floating piece the first time, or with copy leaves them and floats a duplicate; on the Background the hole takes the background colour; then moves it by whole pixels; dropped by select.drop or any other command, put back by edit.undo)"##,
            enabled: can_float,
            journal: true,
            run: float,
        },
        CommandSpec {
            id: "select.drop",
            label: "Drop Floating Selection",
            menu: &[],
            shortcut: None,
            params: r##"{} → {dropped, offset} (drops the floating piece into its layer and moves the selection with it: one history step)"##,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document".into()),
            journal: true,
            run: |s, _| drop_floating(s),
        },
    ]
}

#[cfg(test)]
mod tests;
