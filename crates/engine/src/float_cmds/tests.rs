use photocraft_geom::Rect;
use serde_json::json;

use super::*;

/// An 80×60 transparent document whose layer is painted red over (10..30)²; (10..30)² selected.
fn session() -> (Session, LayerId) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 80, "height": 60, "background": "transparent"})).unwrap();
    let id = s.active().unwrap().active_layer.unwrap();
    s.edit("paint", |doc, _| {
        doc.layer_mut(id).unwrap().surface_mut().unwrap().fill_rect(Rect::new(10, 10, 30, 30), &[1.0, 0.0, 0.0, 1.0]);
        Ok(())
    })
    .unwrap();
    s.execute("select.rect", json!({"x": 10, "y": 10, "width": 20, "height": 20})).unwrap();
    (s, id)
}

fn alpha(d: &Document, id: LayerId, x: i32, y: i32) -> f32 {
    d.layer(id).unwrap().surface().unwrap().rgba(x, y)[3]
}

#[test]
fn float_moves_a_cut_piece_without_touching_the_document_until_dropped() {
    let (mut s, id) = session();
    let steps = s.active().unwrap().history.past_len();
    assert_eq!(s.execute("select.float", json!({"dx": 15, "dy": 0})).unwrap()["offset"], json!([15, 0]));
    // Moving it again doesn't cut anything new.
    s.execute("select.float", json!({"dx": 0, "dy": 10})).unwrap();
    let st = s.active().unwrap();
    assert_eq!(floating(st).unwrap().offset, (15, 10));
    assert_eq!(alpha(&st.doc, id, 12, 12), 1.0, "the document is untouched while floating");
    assert_eq!(st.history.past_len(), steps);
    let shown = displayed(st, (0, 0)).unwrap();
    assert!(alpha(&shown, id, 12, 12) == 0.0 && alpha(&shown, id, 27, 22) == 1.0, "shown cut and moved");
    // Drop: one history step, pixels and selection where they were shown.
    s.execute("select.drop", json!({})).unwrap();
    let st = s.active().unwrap();
    assert!(floating(st).is_none());
    assert_eq!(st.history.past_len(), steps + 1);
    for (x, y) in [(12, 12), (27, 22), (44, 39), (40, 40)] {
        assert_eq!(alpha(&st.doc, id, x, y), alpha(&shown, id, x, y), "dropped as shown at ({x}, {y})");
    }
    assert_eq!(st.doc.selection.as_ref().unwrap().content_bounds(), Rect::new(25, 20, 45, 40));
    // Undo takes the whole move back.
    s.undo();
    assert_eq!(alpha(&s.active().unwrap().doc, id, 12, 12), 1.0);
}

#[test]
fn any_other_command_drops_it_and_undo_puts_it_back() {
    let (mut s, id) = session();
    let steps = s.active().unwrap().history.past_len();
    s.execute("select.float", json!({"dx": 20, "dy": 0})).unwrap();
    // Undo while floating: the piece goes back; nothing in the document or history changed.
    assert_eq!(s.execute("edit.undo", json!({})).unwrap()["floating"], json!("returned"));
    let st = s.active().unwrap();
    assert!(floating(st).is_none() && alpha(&st.doc, id, 12, 12) == 1.0 && st.history.past_len() == steps);
    // Float again, then deselect: dropped first (where it was shown), then deselected.
    s.execute("select.float", json!({"dx": 20, "dy": 0})).unwrap();
    s.execute("select.deselect", json!({})).unwrap();
    let st = s.active().unwrap();
    assert!(st.doc.selection.is_none() && floating(st).is_none());
    assert!(alpha(&st.doc, id, 12, 12) == 0.0 && alpha(&st.doc, id, 35, 20) == 1.0);
}

/// The History panel's Step Backward / Step Forward treat a floating piece like Edit › Undo /
/// Redo: no Move step is recorded and the redo states survive.
#[test]
fn history_panel_steps_put_the_piece_back_and_keep_the_redo_states() {
    for back in ["edit.undo", "history.stepBackward"] {
        let (mut s, id) = session();
        let steps = s.active().unwrap().history.past_len();
        s.execute("select.float", json!({"dx": 20, "dy": 0})).unwrap();
        assert_eq!(s.execute(back, json!({})).unwrap()["floating"], json!("returned"), "{back}");
        let st = s.active().unwrap();
        assert!(floating(st).is_none() && alpha(&st.doc, id, 12, 12) == 1.0, "{back}: piece back");
        assert_eq!((st.history.past_len(), st.history.can_redo()), (steps, false), "{back}: no Move step");
    }
    for forward in ["edit.redo", "history.stepForward"] {
        let (mut s, id) = session();
        // A redo state (the deselection), then a moved piece over the selection undo restored.
        s.execute("select.deselect", json!({})).unwrap();
        s.undo();
        let steps = s.active().unwrap().history.past_len();
        s.execute("select.float", json!({"dx": 20, "dy": 0})).unwrap();
        s.execute(forward, json!({})).unwrap();
        let st = s.active().unwrap();
        assert!(floating(st).is_none(), "{forward}: piece back");
        assert_eq!(alpha(&st.doc, id, 12, 12), 1.0, "{forward}: the move never reached the document");
        assert!(st.doc.selection.is_none(), "{forward}: stepped forward to the deselection");
        assert_eq!((st.history.past_len(), st.history.can_redo()), (steps + 1, false), "{forward}: no Move step");
        // Nothing to step forward to: refused, and the piece keeps floating.
        let (mut s, _) = session();
        s.execute("select.float", json!({"dx": 20, "dy": 0})).unwrap();
        let steps = s.active().unwrap().history.past_len();
        assert!(s.execute(forward, json!({})).is_err(), "{forward}: nothing to redo");
        let st = s.active().unwrap();
        assert_eq!(floating(st).map(|f| f.offset), Some((20, 0)), "{forward}: still floating");
        assert_eq!(st.history.past_len(), steps);
    }
}

#[test]
fn needs_a_selection_and_an_unlocked_pixel_layer() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 20, "height": 20})).unwrap();
    assert!(!s.is_enabled("select.float"), "no selection");
    s.execute("select.all", json!({})).unwrap();
    assert!(s.is_enabled("select.float"), "the Background's selected pixels move");
    s.execute("layer.new.layer", json!({})).unwrap();
    assert!(s.is_enabled("select.float"));
    s.execute("layer.lockLayers", json!({"position": true})).unwrap();
    assert!(!s.is_enabled("select.float"), "a position-locked layer");
}

/// On the Background, which can't hold transparency, the hole takes the background colour.
#[test]
fn on_the_background_the_hole_takes_the_background_colour() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 40, "height": 40})).unwrap();
    s.tools.background = [0.0, 0.0, 1.0, 1.0];
    let id = s.active().unwrap().active_layer.unwrap();
    s.execute("select.rect", json!({"x": 10, "y": 10, "width": 10, "height": 10})).unwrap();
    s.execute("select.float", json!({"dx": 15, "dy": 0})).unwrap();
    s.execute("select.drop", json!({})).unwrap();
    let px = |x, y| s.active().unwrap().doc.layer(id).unwrap().surface().unwrap().rgba(x, y);
    assert_eq!(px(12, 12), [0.0, 0.0, 1.0, 1.0], "the hole");
    assert_eq!(px(27, 12), [1.0, 1.0, 1.0, 1.0], "the moved pixels");
    assert_eq!(px(5, 5), [1.0, 1.0, 1.0, 1.0], "the rest");
}

/// Copying leaves the original pixels, at every depth; the move is one undo step.
#[test]
fn a_copy_leaves_the_original_at_every_depth_in_one_undo_step() {
    for depth in [8, 16, 32] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 80, "height": 60, "background": "transparent", "depth": depth})).unwrap();
        let id = s.active().unwrap().active_layer.unwrap();
        s.execute("select.rect", json!({"x": 10, "y": 10, "width": 20, "height": 20})).unwrap();
        s.execute("edit.fill", json!({"color": "#ff0000"})).unwrap();
        let steps = s.active().unwrap().history.past_len();
        s.execute("select.float", json!({"dx": 30, "dy": 0, "copy": true})).unwrap();
        s.execute("select.drop", json!({})).unwrap();
        let st = s.active().unwrap();
        assert!(alpha(&st.doc, id, 12, 12) == 1.0 && alpha(&st.doc, id, 42, 12) == 1.0, "{depth}-bit: both");
        assert_eq!((st.history.past_len(), st.history.undo_label()), (steps + 1, Some("Move")), "{depth}-bit");
        s.undo();
        let st = s.active().unwrap();
        assert!(alpha(&st.doc, id, 12, 12) == 1.0 && alpha(&st.doc, id, 42, 12) == 0.0, "{depth}-bit: undone");
    }
}

/// `cargo test --release -p photocraft-engine float_drag_bench -- --ignored --nocapture`
#[test]
#[ignore]
fn float_drag_bench() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 6000, "height": 4000, "background": "transparent"})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("paint", |doc, a| {
        doc.layer_mut(a.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(0, 0, 6000, 4000), &[0.2, 0.4, 0.6, 1.0]);
        Ok(())
    })
    .unwrap();
    s.execute("select.rect", json!({"x": 1000, "y": 1000, "width": 500, "height": 400})).unwrap();
    let t = std::time::Instant::now();
    s.execute("select.float", json!({})).unwrap();
    let cut = t.elapsed();
    let t = std::time::Instant::now();
    for i in 0..20 {
        displayed(s.active().unwrap(), (i * 7, i * 3)).unwrap();
    }
    eprintln!("6000×4000 layer, 500×400 piece: cut {cut:?}, then {:?} per pointer move", t.elapsed() / 20);
}

#[test]
fn copy_floats_a_duplicate_and_leaves_the_original() {
    let (mut s, id) = session();
    s.execute("select.float", json!({"dx": 30, "dy": 0, "copy": true})).unwrap();
    let shown = displayed(s.active().unwrap(), (0, 0)).unwrap();
    assert!(alpha(&shown, id, 12, 12) == 1.0 && alpha(&shown, id, 42, 12) == 1.0, "original kept, copy shown");
    s.execute("select.drop", json!({})).unwrap();
    let doc = &s.active().unwrap().doc;
    assert!(alpha(doc, id, 12, 12) == 1.0 && alpha(doc, id, 42, 12) == 1.0, "dropped: both");
}

#[test]
fn float_with_the_mask_target_moves_the_mask_not_the_layer() {
    let (mut s, id) = session();
    s.edit("mask", |doc, _| {
        let l = doc.layer_mut(id).unwrap();
        let mut m = photocraft_doc::LayerMask::reveal_all();
        m.surface.fill_rect(Rect::new(10, 10, 30, 30), &[0.0]);
        l.mask = Some(m);
        Ok(())
    })
    .unwrap();
    let mask_at = |s: &Session, x, y| s.active().unwrap().doc.layer(id).unwrap().mask.as_ref().unwrap().surface.sample_channel(x, y, 0);
    s.execute("select.float", json!({"dx": 25, "dy": 0, "copy": true, "target": "mask"})).unwrap();
    s.execute("select.drop", json!({})).unwrap();
    assert_eq!(mask_at(&s, 12, 12), 0.0, "the copy leaves the mask where it was");
    assert_eq!(mask_at(&s, 37, 12), 0.0, "the mask's selected part moved");
    assert_eq!(mask_at(&s, 60, 12), 1.0);
    assert_eq!(alpha(&s.active().unwrap().doc, id, 37, 12), 0.0, "the layer's pixels did not move");
    assert_eq!(alpha(&s.active().unwrap().doc, id, 12, 12), 1.0);
}

#[test]
fn cutting_an_untouched_part_of_the_mask_moves_it_and_leaves_the_background_colour() {
    let (mut s, id) = session();
    s.edit("mask", |doc, _| {
        let mut m = photocraft_doc::LayerMask::hide_all();
        m.surface.fill_rect(Rect::new(50, 10, 70, 30), &[1.0]);
        doc.layer_mut(id).unwrap().mask = Some(m);
        Ok(())
    })
    .unwrap();
    s.tools.background = [0.5, 0.5, 0.5, 1.0];
    let mask_at = |s: &Session, x, y| s.active().unwrap().doc.layer(id).unwrap().mask.as_ref().unwrap().surface.sample_channel(x, y, 0);
    // (10..30)² is the mask's untouched black; it moves over the white at (50..70, 10..30).
    s.execute("select.float", json!({"dx": 40, "dy": 0, "target": "mask"})).unwrap();
    s.execute("select.drop", json!({})).unwrap();
    assert_eq!(mask_at(&s, 55, 12), 0.0, "the untouched black moved");
    assert!((mask_at(&s, 12, 12) - 0.5).abs() < 0.01, "the hole takes the background colour");
    assert_eq!(mask_at(&s, 40, 12), 0.0);
}

/// #7655: a selection loaded from the layer's own transparency lifts its half-transparent pixels
/// whole, leaving nothing behind (Photoshop); a half selection of an opaque pixel still splits it.
#[test]
fn a_layers_own_transparency_floats_its_soft_pixels_whole() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 80, "height": 60, "background": "transparent"})).unwrap();
    let id = s.active().unwrap().active_layer.unwrap();
    s.edit("paint", |doc, _| {
        let surf = doc.layer_mut(id).unwrap().surface_mut().unwrap();
        surf.fill_rect(Rect::new(10, 10, 20, 30), &[1.0, 0.0, 0.0, 0.5]);
        surf.fill_rect(Rect::new(20, 10, 30, 30), &[1.0, 0.0, 0.0, 1.0]);
        Ok(())
    })
    .unwrap();
    s.execute("select.loadSelection", json!({"channel": "transparency"})).unwrap();
    s.execute("select.float", json!({"dx": 0, "dy": 25})).unwrap();
    s.execute("select.drop", json!({})).unwrap();
    let d = &s.active().unwrap().doc;
    assert_eq!(alpha(d, id, 15, 20), 0.0, "no ghost where the soft pixels were");
    assert!((alpha(d, id, 15, 45) - 0.5).abs() < 0.01, "they moved whole: {}", alpha(d, id, 15, 45));
    assert_eq!((alpha(d, id, 25, 20), alpha(d, id, 25, 45)), (0.0, 1.0));

    // A copy (⌥) of them is as solid as they are, as Photoshop's Layer via Copy of the same
    // selection is, and leaves them where they were.
    s.undo();
    s.execute("select.loadSelection", json!({"channel": "transparency"})).unwrap();
    s.execute("select.float", json!({"dx": 0, "dy": 25, "copy": true})).unwrap();
    s.execute("select.drop", json!({})).unwrap();
    let d = &s.active().unwrap().doc;
    assert!((alpha(d, id, 15, 20) - 0.5).abs() < 0.01 && (alpha(d, id, 15, 45) - 0.5).abs() < 0.01, "{} {}", alpha(d, id, 15, 20), alpha(d, id, 15, 45));

    // A half selection of an opaque pixel splits it half and half, as before.
    let (mut s, id) = session();
    s.execute("select.all", json!({})).unwrap();
    s.edit("half", |doc, _| {
        let sel = doc.selection.as_mut().unwrap();
        sel.fill_rect(doc_rect(80, 60), &[0.5]);
        Ok(())
    })
    .unwrap();
    s.execute("select.float", json!({"dx": 40, "dy": 0})).unwrap();
    s.execute("select.drop", json!({})).unwrap();
    let d = &s.active().unwrap().doc;
    assert!((alpha(d, id, 15, 15) - 0.5).abs() < 0.01 && (alpha(d, id, 55, 15) - 0.5).abs() < 0.01);
}

fn doc_rect(w: i32, h: i32) -> Rect {
    Rect::new(0, 0, w, h)
}

#[test]
fn float_shares_interior_tiles_on_cut_and_place() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 1024, "height": 1024})).unwrap();
    s.tools.background = [0.2, 0.4, 0.8, 1.0];
    let id = s.active().unwrap().active_layer.unwrap();
    // Paint non-uniform pixels so original tiles are distinct.
    s.edit("pattern", |doc, _| {
        let surf = doc.layer_mut(id).unwrap().surface_mut().unwrap();
        for ty in 0..4 {
            for tx in 0..4 {
                surf.write_pixel(tx * 256 + 5, ty * 256 + 5, &[tx as f32 * 0.25, ty as f32 * 0.25, 0.5, 1.0]);
            }
        }
        Ok(())
    })
    .unwrap();
    // Select (100..900)^2, which fully covers interior tiles (1, 1) and (2, 2).
    s.execute("select.rect", json!({"x": 100, "y": 100, "width": 800, "height": 800})).unwrap();
    s.execute("select.float", json!({})).unwrap();
    let st = s.active().unwrap();
    let orig = st.doc.layer(id).unwrap().surface().unwrap();
    let parts = &floating(st).unwrap().parts;
    let tc1 = photocraft_geom::TileCoord { tx: 1, ty: 1 };
    let tc2 = photocraft_geom::TileCoord { tx: 2, ty: 2 };
    // Interior tiles of `piece` share the original layer's Arc<Tile>.
    assert!(Arc::ptr_eq(orig.tile(tc1).unwrap(), parts.piece.tile(tc1).unwrap()));
    assert!(Arc::ptr_eq(orig.tile(tc2).unwrap(), parts.piece.tile(tc2).unwrap()));
    // Interior tiles of the Background hole in `rest` share one solid fill Arc<Tile>.
    assert!(Arc::ptr_eq(parts.rest.tile(tc1).unwrap(), parts.rest.tile(tc2).unwrap()));
    // Moving by a whole tile (256, 0) shares the interior tile directly in the preview document.
    let shown = displayed(st, (256, 0)).unwrap();
    let shown_surf = shown.layer(id).unwrap().surface().unwrap();
    let moved_tc = photocraft_geom::TileCoord { tx: 2, ty: 1 };
    assert!(Arc::ptr_eq(orig.tile(tc1).unwrap(), shown_surf.tile(moved_tc).unwrap()));
}

#[test]
fn float_feathered_selection_preserves_partial_alpha_across_depths() {
    for depth in [8, 16, 32] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 120, "height": 80, "background": "transparent", "depth": depth})).unwrap();
        let id = s.active().unwrap().active_layer.unwrap();
        s.edit("paint", |doc, _| {
            doc.layer_mut(id).unwrap().surface_mut().unwrap().fill_rect(Rect::new(0, 0, 55, 80), &[1.0, 0.0, 0.0, 1.0]);
            Ok(())
        })
        .unwrap();
        s.execute("select.rect", json!({"x": 20, "y": 20, "width": 30, "height": 30, "feather": 4.0})).unwrap();
        s.execute("select.float", json!({"dx": 40, "dy": 0})).unwrap();
        s.execute("select.drop", json!({})).unwrap();
        let surf = s.active().unwrap().doc.layer(id).unwrap().surface().unwrap();
        // Center of moved piece is opaque red; center of hole is transparent; feathered edge has partial alpha.
        assert!(surf.rgba(75, 35)[3] > 0.95, "{depth}-bit moved center");
        assert!(surf.rgba(35, 35)[3] < 0.05, "{depth}-bit hole center");
        let edge_a = surf.rgba(60, 35)[3];
        assert!(edge_a > 0.1 && edge_a < 0.9, "{depth}-bit feathered edge alpha={edge_a}");
    }
}

/// Worst channel difference between two composites of the canvas.
fn worst(a: &Document, b: &Document) -> f32 {
    let (a, b) = (photocraft_compose::flatten(a), photocraft_compose::flatten(b));
    assert_eq!(a.rect, b.rect);
    a.px.iter().zip(&b.px).map(|(p, q)| (0..4).map(|k| (p[k] - q[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max)
}

/// A `depth`-bit document with a Background and a painted layer (a ramp, half transparent in a
/// band) over it; (12..52)×(8..40) selected, feathered when `feather`.
fn layered(depth: u32, feather: bool, background: bool) -> (Session, LayerId) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 96, "height": 64, "depth": depth})).unwrap();
    if !background {
        s.execute("layer.new.layer", json!({})).unwrap();
    }
    let id = s.active().unwrap().active_layer.unwrap();
    s.edit("paint", |doc, _| {
        let surf = doc.layer_mut(id).unwrap().surface_mut().unwrap();
        for x in 0..96 {
            surf.fill_rect(Rect::new(x, 0, x + 1, 64), &photocraft_raster::from_rgba(&surf.format(), [x as f32 / 95.0, 0.3, 1.0 - x as f32 / 95.0, 1.0]));
        }
        if surf.format().alpha {
            surf.fill_rect(Rect::new(0, 20, 96, 30), &photocraft_raster::from_rgba(&surf.format(), [0.9, 0.9, 0.1, 0.5]));
        }
        Ok(())
    })
    .unwrap();
    s.execute("select.rect", json!({"x": 12, "y": 8, "width": 40, "height": 32})).unwrap();
    if feather {
        s.execute("select.modify.feather", json!({"radius": 5})).unwrap();
    }
    (s, id)
}

/// The canvas shows a floating piece as a layer of its own over the cut-out layer: the same
/// pixels as compositing it into the layer, at every depth, cut or copied, feathered or not, on
/// the Background too; the cut-out layer is one surface at every offset (nothing to upload again)
/// and only the piece's layer moves.
#[test]
fn split_shows_what_the_composited_piece_shows() {
    for depth in [8, 16, 32] {
        for (feather, copy, background) in [(false, false, false), (true, false, false), (true, true, false), (false, false, true), (true, false, true)] {
            let what = format!("{depth}-bit feather={feather} copy={copy} background={background}");
            let (mut s, id) = layered(depth, feather, background);
            s.execute("select.float", json!({"dx": 0, "dy": 0, "copy": copy})).unwrap();
            let st = s.active().unwrap();
            let f = floating(st).unwrap();
            let mut rest = None;
            for (dx, dy) in [(0, 0), (7, 3), (-15, 21), (64, -2), (300, 300)] {
                let split = f.parts.split(&st.doc, dx, dy).unwrap_or_else(|| panic!("{what}: a plain layer splits"));
                let moved = f.parts.moved(&st.doc, dx, dy).unwrap();
                let w = worst(&split, &moved);
                assert!(w <= 1.0 / 255.0 + 1e-6, "{what} ({dx}, {dy}): max diff {:.3}/255", w * 255.0);
                let piece = split.layer(f.parts.piece_layer()).unwrap_or_else(|| panic!("{what}: the piece's layer"));
                let path = |id| split.path_of(id).unwrap();
                let (p, l) = (path(f.parts.piece_layer()), path(id));
                assert_eq!((p.len(), p.last().copied()), (l.len(), l.last().map(|i| i + 1)), "{what}: right above the cut-out layer");
                assert_eq!(piece.surface().unwrap().content_bounds(), f.parts.piece().content_bounds().translate(dx, dy), "{what}");
                let cut = split.layer(id).unwrap().surface().unwrap().clone();
                if let Some(r) = &rest {
                    assert!(
                        cut == *r && cut.tiles().zip(r.tiles()).all(|(a, b)| std::sync::Arc::ptr_eq(a.1, b.1)),
                        "{what}: the cut-out layer's tiles are shared"
                    );
                }
                rest = Some(cut);
            }
            // What the canvas shows is what the drop leaves.
            s.execute("select.float", json!({"dx": 9, "dy": -4})).unwrap();
            let shown = displayed_split(s.active().unwrap(), (0, 0)).unwrap();
            s.execute("select.drop", json!({})).unwrap();
            let w = worst(&shown, &s.active().unwrap().doc);
            assert!(w <= 1.0 / 255.0 + 1e-6, "{what}: dropped as shown, max diff {:.3}/255", w * 255.0);
        }
    }
}

/// Layers whose piece wouldn't composite like a layer of its own over them show the piece
/// composited into the layer, as before.
#[test]
fn layers_that_dont_split_show_the_composited_piece() {
    type Setup = fn(&mut Document, LayerId);
    let cases: [(&str, Setup); 6] = [
        ("opacity", |d, id| d.layer_mut(id).unwrap().opacity = 0.5),
        ("fill", |d, id| d.layer_mut(id).unwrap().fill_opacity = 0.5),
        ("multiply", |d, id| d.layer_mut(id).unwrap().blend = photocraft_color::BlendMode::Multiply),
        ("mask", |d, id| d.layer_mut(id).unwrap().mask = Some(photocraft_doc::LayerMask::reveal_all())),
        ("clipped", |d, id| d.layer_mut(id).unwrap().clipped = true),
        ("clipping base", |d, id| {
            let mut above = photocraft_doc::Layer::raster("above", d.pixel_format());
            above.clipped = true;
            above.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 96, 64), &[0.0, 1.0, 0.0, 1.0]);
            d.insert_above(Some(id), above);
        }),
    ];
    for (what, setup) in cases {
        let (mut s, id) = layered(8, true, false);
        s.edit("setup", |doc, _| {
            setup(doc, id);
            Ok(())
        })
        .unwrap();
        s.execute("select.float", json!({"dx": 11, "dy": 5})).unwrap();
        let st = s.active().unwrap();
        let f = floating(st).unwrap();
        assert!(f.parts.split(&st.doc, 11, 5).is_none(), "{what}");
        let shown = displayed_split(st, (0, 0)).unwrap();
        assert!(shown.layer(f.parts.piece_layer()).is_none(), "{what}");
        assert_eq!(worst(&shown, &displayed(st, (0, 0)).unwrap()), 0.0, "{what}");
    }
}

/// The mask target never splits (the piece is mask values, not pixels).
#[test]
fn a_floating_mask_piece_does_not_split() {
    let (mut s, id) = layered(8, false, false);
    s.edit("mask", |doc, _| {
        doc.layer_mut(id).unwrap().mask = Some(photocraft_doc::LayerMask::reveal_all());
        Ok(())
    })
    .unwrap();
    s.execute("select.float", json!({"dx": 4, "dy": 4, "target": "mask"})).unwrap();
    let st = s.active().unwrap();
    assert!(floating(st).unwrap().parts.split(&st.doc, 4, 4).is_none());
}
