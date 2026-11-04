use super::{Affine, Arc, LayerContent, LayerId, PhotocraftApp, Point, TextLayout, text_layer};

/// Layout of a type layer (cached per document revision) and its text → document transform.
pub fn layout(app: &mut PhotocraftApp, id: LayerId) -> Option<(Arc<TextLayout>, Affine, String)> {
    let dragged = crate::type_transform::current_transform(app, id);
    // Borrows the document in place (no clone): the canvas hover hit-tests type every frame.
    let st = app.session.active()?;
    let t = text_layer(&st.doc, id)?;
    let transform = dragged.unwrap_or(t.transform);
    let key = (st.doc.id.0, st.revision, id.0);
    if let Some((k, l)) = &app.type_layout
        && *k == key
    {
        return Some((l.clone(), transform, t.text.clone()));
    }
    let l = Arc::new(photocraft_text::shared().lock().ok()?.layout(t, st.doc.resolution_dpi));
    app.type_layout = Some((key, l.clone()));
    Some((l, transform, t.text.clone()))
}

/// Type layer `id`'s perspective, when it has one (Type › Perspective): the drawn text is its
/// layout through the transform, then this map.
pub(crate) fn perspective(app: &PhotocraftApp, id: LayerId) -> Option<photocraft_doc::text::TextPerspective> {
    app.session.active().and_then(|st| text_layer(&st.doc, id)).and_then(|t| t.perspective).filter(|p| p.is_valid())
}

/// A document point on type layer `id` back before its perspective (where its layout's transform
/// puts the text): what a tap on perspective type hits.
pub(crate) fn unproject(app: &PhotocraftApp, id: LayerId, x: f64, y: f64) -> (f64, f64) {
    perspective(app, id).and_then(|p| p.unapply(x, y)).unwrap_or((x, y))
}

/// A point the layout's transform placed, through the layer's perspective (if any).
pub(crate) fn project(p: Option<&photocraft_doc::text::TextPerspective>, pt: Point) -> Point {
    match p.and_then(|p| p.apply(pt.x, pt.y)) {
        Some((x, y)) => Point::new(x, y),
        None => pt,
    }
}

pub(super) fn to_text(aff: &Affine, x: f64, y: f64) -> (f32, f32) {
    let p = aff.inverse().unwrap_or(Affine::IDENTITY).apply(Point::new(x, y));
    (p.x as f32, p.y as f32)
}

/// Topmost shown type layer (it and every group around it visible, #4320) whose laid-out text
/// contains the document point. The press and the canvas hover (`hover_path_target`) share it, so
/// the pointer never promises a path click that the press would turn into editing this layer.
pub(crate) fn hit_layer(app: &mut PhotocraftApp, x: f64, y: f64) -> Option<LayerId> {
    let slop = 6.0 / app.point_zoom().max(0.01);
    // A Type Mask session's text is a hidden layer; it is still the text being edited.
    let editing = app.ui.text_edit.as_ref().filter(|ed| ed.mask).map(|ed| LayerId(ed.layer));
    let mut ids: Vec<LayerId> = {
        let doc = &app.session.active()?.doc;
        doc.walk()
            .into_iter()
            .filter(|(path, _, l)| matches!(l.content, LayerContent::Text(_)) && (editing == Some(l.id) || doc.shown_at(path)))
            .map(|(_, _, l)| l.id)
            .collect()
    };
    ids.reverse(); // walk() is bottom-up; hit the topmost first
    ids.into_iter().find(|id| {
        // The pixels shown count too: a PSD's type keeps Photoshop's rendering until edited,
        // which can sit off our layout when fonts are substituted (#123).
        let shown = app
            .session
            .active()
            .and_then(|st| text_layer(&st.doc, *id))
            .and_then(|t| t.cache.as_ref())
            .map(photocraft_compose::bounds::content_bounds)
            .is_some_and(|r| {
                let s = f64::from(slop);
                !r.is_empty() && x >= f64::from(r.x0) - s && x <= f64::from(r.x1) + s && y >= f64::from(r.y0) - s && y <= f64::from(r.y1) + s
            });
        // The hover calls this every frame: skip the layout when the pixels already answer.
        if shown {
            return true;
        }
        let Some((l, aff, _)) = layout(app, *id) else { return false };
        let (ux, uy) = unproject(app, *id, x, y);
        let (tx, ty) = to_text(&aff, ux, uy);
        // Type in a path: a click anywhere inside its path edits it, as in Photoshop.
        let in_area = app
            .session
            .active()
            .and_then(|st| text_layer(&st.doc, *id))
            .and_then(|t| photocraft_text::in_path::Area::of(&t.shape))
            .is_some_and(|a| a.contains(tx, ty));
        in_area || photocraft_text::text_point_inside(&l, tx, ty, slop)
    })
}

pub(crate) use photocraft_engine::type_extra_cmds::shows_own_layout;

pub(super) fn hit_offset(app: &mut PhotocraftApp, id: LayerId, x: f64, y: f64) -> usize {
    offset_at(app, id, x, y)
}

/// The character offset of the caret position nearest document point (`x`, `y`) in layer `id`.
pub(crate) fn offset_at(app: &mut PhotocraftApp, id: LayerId, x: f64, y: f64) -> usize {
    let Some((l, aff, text)) = layout(app, id) else { return 0 };
    let (x, y) = unproject(app, id, x, y);
    let (tx, ty) = to_text(&aff, x, y);
    // Type on a path: the point's place along the path is the position in the (single) line.
    let shape = app.session.active().and_then(|s| text_layer(&s.doc, id).map(|t| t.shape.clone()));
    let (tx, ty) = match shape {
        Some(photocraft_doc::text::TextShape::Path { path, start, flip }) => {
            photocraft_text::warp::arc_length_at(&path, flip, f64::from(tx), f64::from(ty)).map_or((tx, ty), |s| ((s - f64::from(start)) as f32, -1.0))
        }
        _ => (tx, ty),
    };
    photocraft_text::hit_char(&l, &text, tx, ty).0
}
