use super::commit::{cancel, commit, start_steps};
#[cfg(test)]
use super::draw::texture_quad;
use super::preview::{Above, preview_image};
use super::{
    Arc, BezierMesh, CAS_ONLY_SCALES, Color32, DocId, Document, Follow, Homography, LOCKED, LayerContent, LayerId, MadeLayer, PhotocraftApp, Several, Steps,
    TransformPreview, TransformSession, Warp, WarpStyle, corners, json,
};

/// Start Free Transform on the active layer (or its selected pixels), or on the targeted unlinked
/// layer mask, alpha channel or Quick Mask.
pub fn begin(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<(), String> {
    if let Some((params, path)) = crate::vector_ui::free_transform_path(app) {
        return begin_path(app, ctx, params, path);
    }
    if app.session.active().is_some_and(|st| photocraft_engine::float_cmds::floating(st).is_some()) {
        app.run("select.drop", json!({}))?;
    }
    let target = crate::canvas::paint_target(app);
    let st = app.session.active().ok_or("no document")?;
    let (doc, revision) = (st.doc.clone(), st.revision);
    let lone = photocraft_engine::transform_cmds::lone_target(&doc, st.active_layer, &json!({ "target": target })).map_err(|e| e.to_string())?.cloned();
    if let Some(surf) = lone {
        return begin_lone(app, ctx, doc, surf, target);
    }
    let id = st.active_layer.ok_or("no active layer")?;
    // Several layers selected: one box moves them all, as in Photoshop (#6501).
    let targets = photocraft_engine::transform_cmds::selection_targets(st);
    if targets != [id] {
        return begin_several(app, ctx, doc, revision, id, targets);
    }
    let layer = doc.layer(id).ok_or("no layer")?;
    if photocraft_engine::transform_cmds::locked_for_transform(&doc, id) {
        // Photoshop refuses before the box opens rather than when it is committed (#2585).
        app.ui.open_dialog(crate::state::DialogKind::Error, json!({"message": tl!(LOCKED)}).as_object().cloned().unwrap_or_default());
        return Err(LOCKED.into());
    }
    if matches!(layer.content, LayerContent::Adjustment(_)) && layer.mask.is_none() {
        return Err("Adjustment layers have nothing to transform".into());
    }
    // With a selection the box is the selection's (#5297).
    let b = photocraft_engine::transform_cmds::transform_frame(&doc, layer);
    if b.is_empty() {
        return Err("Could not transform: the layer is empty".into());
    }
    crate::type_tool::commit(app);
    let rect = [b.x0 as f64, b.y0 as f64, b.x1 as f64, b.y1 as f64];
    let session = app.ui.alloc_id();
    // Preview document: the moving pixels removed (layer hidden, or the selection lifted out).
    let mut pd = (*doc).clone();
    let mut lifted = None;
    // Type, shapes and smart objects move whole, selection or not (#2630): only pixels are lifted.
    if let Some(sel) = doc.selection.clone().filter(|_| !photocraft_engine::transform_cmds::moves_whole(layer) && layer.surface().is_some()) {
        if let Some(surf) = pd.layer_mut(id).and_then(|l| l.surface_mut()) {
            let (l, rest) = photocraft_engine::transform_cmds::split_selected(surf, &sel).map_err(|e| e.to_string())?;
            *surf = rest;
            lifted = Some(l);
        }
    } else if let Some(l) = pd.layer_mut(id) {
        l.visible = false;
    }
    // egui reports the renderer's limit in the app; offscreen harnesses only through the device.
    let max_side = ctx.input(|i| i.max_texture_side).max(app.gpu.as_ref().map_or(1, |g| g.max_texture_side()));
    // The layers covering the moving pixels draw over them instead of under them.
    let above = Above::new(ctx, format!("transform-{session}-above"), &doc, id, max_side);
    above.hide_in(&mut pd.layers);
    // A style is drawn again from the pixels as the box moves them, so they are read without it.
    // Not for the selected part of a layer: its style belongs to that part and the rest together.
    let styled = lifted.is_none() && photocraft_compose::effects::has_effects(layer);
    let (image, uv) = preview_image(&doc, id, lifted.as_ref(), b, max_side, styled);
    let texture = crate::transform_tex::PreviewTextures::new(ctx, format!("transform-{session}"), image, uv);
    app.transform_preview = Some(TransformPreview {
        session,
        doc: Arc::new(pd),
        texture,
        opacity: layer.opacity * layer.fill_opacity,
        above: Some(above),
        styled: styled.then(|| crate::transform_fx::StyledPreview::new(format!("transform-{session}-styled"))),
        follow: Some(Follow { revision, hidden: if lifted.is_none() { vec![id] } else { Vec::new() } }),
        committing: None,
        gesture: None,
        warp_drag: None,
        split_tool: None,
        split_pointer: None,
        split_placing: false,
        split_quick: false,
        steps: Steps::default(),
        tool: app.ui.tool,
        path: None,
        content_aware: None,
        several: None,
        vector_type: None,
    });
    app.ui.transform = Some(TransformSession {
        session,
        layer: id.0,
        rect,
        quad: corners(rect),
        pivot: [(rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0],
        interpolation: "bicubic".into(),
        warp: None,
        selection: false,
        target: None,
        path: None,
        made: None,
        mode: Default::default(),
        content_aware: None,
        skew_v: 0.0,
        layers: Vec::new(),
    });
    // Type already in perspective: the box is its quad and the text is drawn through it.
    if lifted.is_none() && matches!(&layer.content, LayerContent::Text(t) if t.perspective.is_some()) {
        make_vector_type(app);
    }
    start_steps(app);
    Ok(())
}

/// Turn the open box over a type layer into a vector one ([`super::VectorType`]): the box is the
/// text's quad (its perspective's, or its frame's corners), the preview document shows the layer
/// itself, re-drawn through the box as it moves, and nothing is resampled.
pub(crate) fn vectorize_type_box(app: &mut PhotocraftApp) {
    let single_type =
        app.ui.transform.as_ref().is_some_and(|t| t.layers.is_empty() && t.warp.is_none() && !t.selection && t.target.is_none() && t.path.is_none())
            && app.transform_preview.as_ref().is_some_and(|pv| pv.vector_type.is_none() && pv.several.is_none())
            && app
                .session
                .active()
                .zip(app.ui.transform.as_ref())
                .is_some_and(|(st, t)| matches!(st.doc.layer(LayerId(t.layer)).map(|l| &l.content), Some(LayerContent::Text(_))));
    // Only an untouched box: one moved already has its own source rectangle.
    if single_type && app.ui.transform.as_ref().is_some_and(|t| t.quad == corners(t.rect)) {
        make_vector_type(app);
    }
}

fn make_vector_type(app: &mut PhotocraftApp) {
    let (Some(t), Some(pv)) = (app.ui.transform.as_mut(), app.transform_preview.as_mut()) else { return };
    let Some(st) = app.session.active().filter(|st| st.doc.id == pv.doc.id) else { return };
    let Some(LayerContent::Text(text)) = st.doc.layer(LayerId(t.layer)).map(|l| &l.content) else { return };
    let quad = text.perspective.and_then(|p| p.quad()).unwrap_or_else(|| corners(t.rect));
    let (min, max) = quad.iter().fold(([f64::MAX; 2], [f64::MIN; 2]), |(lo, hi), q| ([lo[0].min(q[0]), lo[1].min(q[1])], [hi[0].max(q[0]), hi[1].max(q[1])]));
    t.rect = [min[0], min[1], max[0], max[1]];
    t.quad = quad;
    t.pivot = [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0];
    // The layer in its place in the stack: nothing hidden, nothing drawn over it apart.
    pv.doc = st.doc.clone();
    pv.above = None;
    if let Some(f) = pv.follow.as_mut() {
        f.hidden.clear();
    }
    pv.vector_type = Some(super::VectorType { from: quad, shown: None, drawn: 0 });
}

/// A vector type box moved (or the document changed under it): the preview document gets the
/// layer drawn through the box's map now. Called before the canvas reads the preview document.
pub(crate) fn refresh_vector_type(app: &mut PhotocraftApp) {
    let (Some(t), Some(pv)) = (app.ui.transform.as_ref(), app.transform_preview.as_mut()) else { return };
    let Some(vt) = pv.vector_type.as_mut() else { return };
    let Some(st) = app.session.active().filter(|st| st.doc.id == pv.doc.id) else { return };
    if vt.shown == Some((t.quad, st.revision)) {
        return;
    }
    vt.shown = Some((t.quad, st.revision));
    let id = LayerId(t.layer);
    let Some(LayerContent::Text(text)) = st.doc.layer(id).map(|l| &l.content) else { return };
    let unit = [0.0, 0.0, 1.0, 1.0];
    let map = Homography::rect_to_quad(unit, t.quad).zip(Homography::rect_to_quad(unit, vt.from).and_then(|h| h.inverse())).map(|(to, from)| to.mul(&from));
    let mut pd = (*st.doc).clone();
    // A box past the horizon (or collapsed) shows the text as it was.
    if let Some(h) = map
        && let Ok(drawn) = photocraft_engine::transform_cmds::type_in_perspective(&st.doc, text, &h)
        && let Some(l) = pd.layer_mut(id)
    {
        l.content = LayerContent::Text(drawn);
    }
    pv.doc = Arc::new(pd);
    vt.drawn = vt.drawn.wrapping_add(1);
}

/// Free Transform of the selected layers `ids` together (#6501): the box frames them all, and the
/// preview moves their composite, drawn over the layers they cover. `active` stays the session's
/// `layer`, so picking another layer still applies the box. The selected pixels of each pixel
/// layer move when there is a selection; type, shapes, smart objects and groups move whole.
fn begin_several(app: &mut PhotocraftApp, ctx: &egui::Context, doc: Arc<Document>, revision: u64, active: LayerId, ids: Vec<LayerId>) -> Result<(), String> {
    use photocraft_engine::transform_cmds as tc;
    if ids.iter().any(|id| tc::locked_for_transform(&doc, *id)) {
        app.ui.open_dialog(crate::state::DialogKind::Error, json!({"message": tl!(LOCKED)}).as_object().cloned().unwrap_or_default());
        return Err(LOCKED.into());
    }
    let b = tc::transform_frame_of(&doc, &ids);
    let top = *ids.last().ok_or("no layer")?;
    if b.is_empty() {
        return Err("Could not transform: the layers are empty".into());
    }
    crate::type_tool::commit(app);
    let rect = [b.x0 as f64, b.y0 as f64, b.x1 as f64, b.y1 as f64];
    let session = app.ui.alloc_id();
    // `under`: the canvas under the box, without the moving pixels. `moving`: those pixels alone.
    let mut under = (*doc).clone();
    let mut lifted = Vec::new();
    let mut hidden = Vec::new();
    for &id in &ids {
        let Some(l) = doc.layer(id) else { continue };
        match (doc.selection.as_ref().filter(|_| !tc::moves_whole(l)), l.surface()) {
            (Some(sel), Some(surf)) => {
                let (part, rest) = tc::split_selected(surf, sel).map_err(|e| e.to_string())?;
                if let Some(s) = under.layer_mut(id).and_then(|l| l.surface_mut()) {
                    *s = rest;
                }
                lifted.push((id, part));
            }
            _ => {
                if let Some(l) = under.layer_mut(id) {
                    l.visible = false;
                }
                hidden.push(id);
            }
        }
    }
    let max_side = ctx.input(|i| i.max_texture_side).max(app.gpu.as_ref().map_or(1, |g| g.max_texture_side()));
    let above = Above::new(ctx, format!("transform-{session}-above"), &doc, top, max_side);
    above.hide_in(&mut under.layers);
    // Opacity, fill, masks and styles are in the composite already.
    let mut several = Several {
        ctx: ctx.clone(),
        name: format!("transform-{session}"),
        max_side,
        moving: Several::shown(&doc, &ids, &lifted),
        lifted,
        ids: ids.clone(),
        frame: b,
        rect: b,
    };
    let texture = several.render();
    app.transform_preview = Some(TransformPreview {
        session,
        doc: Arc::new(under),
        texture,
        opacity: 1.0,
        above: Some(above),
        styled: None,
        follow: Some(Follow { revision, hidden }),
        committing: None,
        gesture: None,
        warp_drag: None,
        split_tool: None,
        split_pointer: None,
        split_placing: false,
        split_quick: false,
        steps: Steps::default(),
        tool: app.ui.tool,
        path: None,
        content_aware: None,
        several: Some(several),
        vector_type: None,
    });
    app.ui.transform = Some(TransformSession {
        session,
        layer: active.0,
        rect,
        quad: corners(rect),
        pivot: [(rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0],
        interpolation: "bicubic".into(),
        warp: None,
        selection: false,
        target: None,
        path: None,
        made: None,
        mode: Default::default(),
        content_aware: None,
        skew_v: 0.0,
        layers: ids.iter().map(|id| id.0).collect(),
    });
    start_steps(app);
    Ok(())
}

/// The moving texture's texel at document point `p` as the box opened (full resolution), and
/// where the texture's corners are drawn now: what a test of a box over several layers sees.
#[cfg(test)]
pub(crate) fn moving_texel(app: &PhotocraftApp, p: [i32; 2]) -> (Option<Color32>, [[f64; 2]; 4]) {
    let (Some(t), Some(pv)) = (&app.ui.transform, &app.transform_preview) else { return (None, [[0.0; 2]; 4]) };
    let r = pv.several.as_ref().map_or(photocraft_geom::Rect::new(t.rect[0] as i32, t.rect[1] as i32, t.rect[2] as i32, t.rect[3] as i32), |sv| sv.rect);
    let texel = pv.texture.level_image(0).and_then(|(image, _)| {
        let (x, y) = (usize::try_from(p[0] - r.x0).ok()?, usize::try_from(p[1] - r.y0).ok()?);
        (x < image.size[0] && y < image.size[1]).then(|| image.pixels[y * image.size[0] + x])
    });
    (texel, texture_quad(t, pv))
}

/// The layers the box moves: every layer of a several-layer session (#6501), else its one layer.
pub(crate) fn moving_layers(t: &TransformSession) -> Vec<LayerId> {
    if t.layers.is_empty() { vec![LayerId(t.layer)] } else { t.layers.iter().copied().map(LayerId).collect() }
}

/// Is any of the layers the box moves of this kind?
pub(super) fn moves_kind(app: &PhotocraftApp, t: &TransformSession, kind: fn(&LayerContent) -> bool) -> bool {
    app.session.active().is_some_and(|st| moving_layers(t).iter().any(|id| st.doc.layer(*id).is_some_and(|l| kind(&l.content))))
}

/// Why Warp refuses with several layers selected.
pub const WARP_ONE_LAYER: &str = "Warp works on one layer at a time: select a single layer";

/// Free Transform on a copy (⌥⌘T, no menu item): duplicates the active layer (with a selection,
/// its selected pixels, as Layer via Copy) and transforms the copy (#352).
pub fn begin_copy(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<(), String> {
    let selection = app.session.active().is_some_and(|d| d.doc.selection.is_some());
    let params = if selection { json!({}) } else { json!({"inPlace": true}) };
    app.run(if selection { "layer.new.layerViaCopy" } else { "layer.duplicate" }, params)?;
    let made = app.session.active().and_then(|st| st.active_layer.map(|layer| (st.doc.id, layer)));
    if let Err(e) = begin(app, ctx) {
        if let Some((document, layer)) = made {
            take_back_made(app, document, layer);
        }
        return Err(e);
    }
    if let Some(t) = app.ui.transform.as_mut() {
        t.made = Some(MadeLayer::Copy);
    }
    Ok(())
}

/// Free Transform on the layer a file dropped on the canvas just placed, like the reference app's
/// Place: Esc takes the place back, ↩ makes the place and the transform one Place Embedded step.
pub fn begin_placed(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<(), String> {
    begin(app, ctx)?;
    if let Some(t) = app.ui.transform.as_mut() {
        t.made = Some(MadeLayer::Place);
    }
    Ok(())
}

/// Edit › Content-Aware Scale (#3096): a scale-only box over `region` of the active pixel layer
/// (the selection's bounds, else the layer's content), cleared under the box as the command
/// clears it. OK runs `edit.contentAwareScale` (cas_transform.rs) instead of `edit.transform`.
pub(crate) fn begin_content_aware(
    app: &mut PhotocraftApp,
    ctx: &egui::Context,
    region: photocraft_geom::Rect,
    live: crate::cas_transform::Live,
) -> Result<(), String> {
    let st = app.session.active().ok_or("no document")?;
    let (doc, revision) = (st.doc.clone(), st.revision);
    let id = st.active_layer.ok_or("no active layer")?;
    let layer = doc.layer(id).ok_or("no layer")?;
    let surf = layer.surface().ok_or("Content-Aware Scale needs a pixel layer")?;
    if region.is_empty() {
        return Err("Could not scale: the layer is empty".into());
    }
    crate::type_tool::commit(app);
    let rect = [region.x0 as f64, region.y0 as f64, region.x1 as f64, region.y1 as f64];
    let session = app.ui.alloc_id();
    // The command clears the area before writing the result (a layer without transparency keeps it).
    let mut pd = (*doc).clone();
    if let Some(s) = pd.layer_mut(id).and_then(|l| l.surface_mut()).filter(|s| s.format().alpha) {
        let clear = vec![0.0; s.format().channels()];
        s.fill_rect(region, &clear);
        s.prune();
    }
    let max_side = ctx.input(|i| i.max_texture_side).max(app.gpu.as_ref().map_or(1, |g| g.max_texture_side()));
    let above = Above::new(ctx, format!("transform-{session}-above"), &doc, id, max_side);
    above.hide_in(&mut pd.layers);
    let (image, uv) = crate::transform_tex::read_surface(surf, None, region, max_side);
    let texture = crate::transform_tex::PreviewTextures::new(ctx, format!("transform-{session}"), image, uv);
    app.transform_preview = Some(TransformPreview {
        session,
        doc: Arc::new(pd),
        texture,
        opacity: layer.opacity * layer.fill_opacity,
        above: Some(above),
        follow: Some(Follow { revision, hidden: Vec::new() }),
        committing: None,
        gesture: None,
        warp_drag: None,
        split_tool: None,
        split_pointer: None,
        split_placing: false,
        split_quick: false,
        steps: Steps::default(),
        tool: app.ui.tool,
        path: None,
        content_aware: Some(live),
        styled: None,
        several: None,
        vector_type: None,
    });
    app.ui.transform = Some(TransformSession {
        session,
        layer: id.0,
        rect,
        quad: corners(rect),
        pivot: [(rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0],
        interpolation: "bicubic".into(),
        warp: None,
        selection: false,
        target: None,
        path: None,
        made: None,
        mode: Default::default(),
        content_aware: Some(Default::default()),
        skew_v: 0.0,
        layers: Vec::new(),
    });
    start_steps(app);
    Ok(())
}

/// Undoes the layer a cancelled or failed session made (⌥⌘T's copy, a placed file), leaving
/// nothing to redo.
pub(super) fn take_back_made(app: &mut PhotocraftApp, document: DocId, layer: LayerId) {
    // Tabs can move or close while transforming. Only undo the creation in its own document,
    // and only while it is still the latest step (it may already have been undone).
    let Some(index) = app.session.documents().iter().position(|st| {
        st.doc.id == document
            && st.doc.layer(layer).is_some()
            && st
                .history
                .recorded_before()
                .or_else(|| st.history.past_len().checked_sub(1).and_then(|i| st.history.state(i)))
                .is_some_and(|before| before.layer(layer).is_none())
    }) else {
        return;
    };
    let active = app.session.active_index();
    app.session.set_active(index);
    // Exactly, also in non-linear history (where an undo would land on a later state).
    app.session.rollback();
    if let Some(active) = active {
        app.session.set_active(active);
    }
    app.sync_views();
}

/// Type → Perspective (the type layer's action bar, its context menu): type can't be distorted,
/// so, as Photoshop asks, the type layer becomes a smart object, whose text stays editable
/// inside, and Free Transform opens on it in Perspective mode (corners mirror; "Free" switches
/// to Distort, each corner on its own). OK makes both one step; Cancel puts the type back. A
/// smart object (one made by an earlier Perspective) just opens the box.
pub fn begin_type_perspective(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<(), String> {
    use photocraft_doc::LayerContent;
    if app.ui.text_edit.is_some() {
        crate::type_tool::commit(app);
    }
    let (doc, layer, kind) = {
        let st = app.session.active().ok_or("no document")?;
        let id = st.active_layer.ok_or("no active layer")?;
        let l = st.doc.layer(id).ok_or("no active layer")?;
        (
            st.doc.id,
            id,
            match &l.content {
                LayerContent::Text(_) => 0,
                LayerContent::Smart(_) => 1,
                _ => 2,
            },
        )
    };
    if kind == 2 {
        return Err(tl!("Perspective is for type layers and smart objects").into());
    }
    let _ = (doc, layer);
    begin(app, ctx)?;
    // Type stays type: the box maps the text itself, drawn as vectors (sharp, still editable).
    if kind == 0 && app.transform_preview.as_ref().is_some_and(|pv| pv.vector_type.is_none()) {
        make_vector_type(app);
    }
    if let Some(t) = app.ui.transform.as_mut() {
        t.mode = crate::state::TransformMode::Perspective;
    }
    Ok(())
}

/// After the transform: the step that made the layer and the transform become one history step,
/// the transform's for a copy and Place Embedded for a placed file.
pub(super) fn fold_made(app: &mut PhotocraftApp, document: DocId, layer: LayerId, made: MadeLayer) {
    // A background commit lands in its own document, which need not be the active one by then.
    let Some(index) = app.session.documents().iter().position(|st| st.doc.id == document) else { return };
    let active = app.session.active_index();
    app.session.set_active(index);
    // Only when the step before the transform made the layer: a step taken while the box was
    // open (New Layer, which also ends the box, #5667) stays its own, as does the made layer's.
    if let Some(st) = app.session.active_mut()
        && st.history.past_len().checked_sub(2).and_then(|i| st.history.state(i)).is_some_and(|before| before.layer(layer).is_none())
    {
        st.history.purge_last();
        if made == MadeLayer::Place {
            st.history.set_current_label(photocraft_engine::file_cmds::PLACE_EMBEDDED);
        }
        if made == MadeLayer::SmartType {
            st.history.set_current_label("Perspective");
        }
    }
    if let Some(active) = active {
        app.session.set_active(active);
    }
}

/// Free Transform of a targeted unlinked layer mask, alpha channel or Quick Mask by itself: the
/// box frames its content (or the selection) and previews the moving values in grey over the
/// document with them vacated.
fn begin_lone(
    app: &mut PhotocraftApp,
    ctx: &egui::Context,
    doc: Arc<Document>,
    surf: photocraft_raster::Surface,
    target: serde_json::Value,
) -> Result<(), String> {
    use photocraft_engine::transform_cmds as tc;
    let b = tc::target_frame(&doc, &surf);
    if b.is_empty() {
        return Err("Could not transform: nothing is selected".into());
    }
    let whole;
    let sel = match &doc.selection {
        Some(s) => s,
        None => {
            let mut s = photocraft_raster::Surface::new(photocraft_color::PixelFormat::GRAY8);
            s.fill_rect(b, &[1.0]);
            whole = s;
            &whole
        }
    };
    let (lifted, rest) = tc::split_gray_selected(&surf, sel).map_err(|e| e.to_string())?.ok_or("this channel can't be transformed")?;
    crate::type_tool::commit(app);
    let layer = app.session.active().and_then(|st| st.active_layer).map_or(0, |l| l.0);
    let mut pd = (*doc).clone();
    let tp = json!({ "target": target });
    if let Ok(Some(s)) = tc::lone_target_mut(&mut pd, Some(LayerId(layer)), &tp) {
        *s = rest;
    }
    let rect = [b.x0 as f64, b.y0 as f64, b.x1 as f64, b.y1 as f64];
    let session = app.ui.alloc_id();
    let max_side = ctx.input(|i| i.max_texture_side).max(app.gpu.as_ref().map_or(1, |g| g.max_texture_side()));
    let (image, uv) = crate::transform_tex::read_surface(&lifted, None, b, max_side);
    let texture = crate::transform_tex::PreviewTextures::new(ctx, format!("transform-{session}"), image, uv);
    app.transform_preview = Some(TransformPreview {
        session,
        doc: Arc::new(pd),
        texture,
        opacity: 0.6,
        above: None,
        styled: None,
        follow: None,
        committing: None,
        gesture: None,
        warp_drag: None,
        split_tool: None,
        split_pointer: None,
        split_placing: false,
        split_quick: false,
        steps: Steps::default(),
        tool: app.ui.tool,
        path: None,
        content_aware: None,
        several: None,
        vector_type: None,
    });
    app.ui.transform = Some(TransformSession {
        session,
        layer,
        rect,
        quad: corners(rect),
        pivot: [(rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0],
        interpolation: "bicubic".into(),
        warp: None,
        selection: false,
        target: Some(target),
        path: None,
        made: None,
        mode: Default::default(),
        content_aware: None,
        skew_v: 0.0,
        layers: Vec::new(),
    });
    start_steps(app);
    Ok(())
}

/// Edit › Free Transform Path: the box frames the path's anchors and handles and draws the
/// path through it; OK commits one `path.transform` step.
fn begin_path(app: &mut PhotocraftApp, ctx: &egui::Context, params: serde_json::Value, path: photocraft_doc::vector::Path) -> Result<(), String> {
    let (x0, y0, x1, y1) = path.control_bounds().ok_or("the path has no points to transform")?;
    let st = app.session.active().ok_or("no document")?;
    let doc = st.doc.clone();
    let layer = st.active_layer.or_else(|| doc.layers.first().map(|l| l.id)).ok_or("no layer")?;
    crate::type_tool::commit(app);
    // A straight horizontal or vertical path still gets a box to turn.
    let rect = [x0, y0, x1.max(x0 + 1.0), y1.max(y0 + 1.0)];
    let session = app.ui.alloc_id();
    let texture =
        crate::transform_tex::PreviewTextures::new(ctx, format!("transform-{session}"), egui::ColorImage::new([1, 1], vec![Color32::TRANSPARENT]), [1.0, 1.0]);
    app.transform_preview = Some(TransformPreview {
        session,
        doc,
        texture,
        opacity: 1.0,
        styled: None,
        gesture: None,
        warp_drag: None,
        split_tool: None,
        split_pointer: None,
        split_placing: false,
        split_quick: false,
        steps: Steps::default(),
        tool: app.ui.tool,
        path: Some(path),
        content_aware: None,
        above: None,
        follow: None,
        committing: None,
        several: None,
        vector_type: None,
    });
    app.ui.transform = Some(TransformSession {
        session,
        layer: layer.0,
        rect,
        quad: corners(rect),
        pivot: [(rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0],
        interpolation: "bicubic".into(),
        warp: None,
        selection: false,
        target: None,
        path: Some(params),
        made: None,
        mode: Default::default(),
        content_aware: None,
        skew_v: 0.0,
        layers: Vec::new(),
    });
    start_steps(app);
    Ok(())
}

/// The affine map taking `rect`'s corners to `quad`'s; `None` when the box was distorted out of a
/// parallelogram (Distort, Perspective): the path then goes as a `quad`.
pub(super) fn rect_to_quad_affine(r: [f64; 4], q: [[f64; 2]; 4]) -> Option<photocraft_geom::Affine> {
    let (w, h) = (r[2] - r[0], r[3] - r[1]);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let tol = 1e-3 * (1.0 + w.max(h));
    if (q[0][0] + q[2][0] - q[1][0] - q[3][0]).abs() > tol || (q[0][1] + q[2][1] - q[1][1] - q[3][1]).abs() > tol {
        return None;
    }
    let (a, b) = ((q[1][0] - q[0][0]) / w, (q[1][1] - q[0][1]) / w);
    let (c, d) = ((q[3][0] - q[0][0]) / h, (q[3][1] - q[0][1]) / h);
    let m = [a, b, c, d, q[0][0] - a * r[0] - c * r[1], q[0][1] - b * r[0] - d * r[1]];
    m.iter().all(|v| v.is_finite()).then_some(photocraft_geom::Affine { m })
}

/// Start Select › Transform Selection: the same box over the selection's bounds, previewing the
/// selection mask (the marching ants are hidden meanwhile); commits `select.transformSelection`.
pub fn begin_selection(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<(), String> {
    let st = app.session.active().ok_or("no document")?;
    let doc = st.doc.clone();
    let sel = doc.selection.clone().ok_or("no selection")?;
    let b = sel.content_bounds();
    if b.is_empty() {
        return Err("no selection".into());
    }
    let layer = st.active_layer.or_else(|| doc.layers.first().map(|l| l.id)).ok_or("no layer")?;
    crate::type_tool::commit(app);
    let rect = [b.x0 as f64, b.y0 as f64, b.x1 as f64, b.y1 as f64];
    let session = app.ui.alloc_id();
    let (w, h) = (b.width() as usize, b.height() as usize);
    let k = w.max(h).div_ceil(2048).max(1);
    let (tw, th) = (w.div_ceil(k), h.div_ceil(k));
    let mut px = vec![Color32::TRANSPARENT; tw * th];
    for ty in 0..th {
        for tx in 0..tw {
            let m = sel.sample_channel(b.x0 + (tx * k) as i32, b.y0 + (ty * k) as i32, 0).clamp(0.0, 1.0);
            px[ty * tw + tx] = Color32::from_rgba_unmultiplied(120, 170, 255, (m * 110.0) as u8);
        }
    }
    let uv = [w as f32 / (tw * k) as f32, h as f32 / (th * k) as f32];
    let texture = crate::transform_tex::PreviewTextures::new(ctx, format!("transform-{session}"), egui::ColorImage::new([tw, th], px), uv);
    let mut pd = (*doc).clone();
    pd.selection = None;
    app.transform_preview = Some(TransformPreview {
        session,
        doc: Arc::new(pd),
        texture,
        opacity: 1.0,
        above: None,
        styled: None,
        follow: None,
        committing: None,
        gesture: None,
        warp_drag: None,
        split_tool: None,
        split_pointer: None,
        split_placing: false,
        split_quick: false,
        steps: Steps::default(),
        tool: app.ui.tool,
        path: None,
        content_aware: None,
        several: None,
        vector_type: None,
    });
    app.ui.transform = Some(TransformSession {
        session,
        layer: layer.0,
        rect,
        quad: corners(rect),
        pivot: [(rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0],
        interpolation: "bilinear".into(),
        warp: None,
        selection: true,
        target: None,
        path: None,
        made: None,
        mode: Default::default(),
        content_aware: None,
        skew_v: 0.0,
        layers: Vec::new(),
    });
    start_steps(app);
    Ok(())
}

/// Start (or switch an active Free Transform into) Warp mode.
pub fn begin_warp(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<(), String> {
    if app.ui.transform.as_ref().is_some_and(|t| t.content_aware.is_some()) {
        return Err(CAS_ONLY_SCALES.into());
    }
    if app.ui.transform.as_ref().is_some_and(|t| !t.layers.is_empty()) {
        return Err(WARP_ONE_LAYER.into());
    }
    let fresh = app.ui.transform.is_none();
    if fresh {
        begin(app, ctx)?;
        if app.ui.transform.as_ref().is_some_and(|t| !t.layers.is_empty()) {
            cancel(app);
            return Err(WARP_ONE_LAYER.into());
        }
    }
    enter_warp(app, ctx);
    // Started in Warp: that is where Undo stops. Switching an open transform to Warp is a step.
    if fresh {
        start_steps(app);
    }
    Ok(())
}

/// Switch to Warp mode: a smart object's existing warp (when the box is untouched), else a
/// custom mesh following the current box (so a transform made first carries over). Editable type
/// gets its own Warp Text style instead ([`enter_type_warp`]).
pub fn enter_warp(app: &mut PhotocraftApp, ctx: &egui::Context) {
    // Several layers keep the box (#6501), even when the active one is type.
    if app.ui.transform.as_ref().is_some_and(|t| !t.layers.is_empty()) {
        return;
    }
    if app.ui.transform.as_ref().is_some_and(|t| t.warp.is_none()) && warps_type(app) {
        return enter_type_warp(app, ctx);
    }
    let existing =
        app.session.active().and_then(|d| d.doc.layer(LayerId(app.ui.transform.as_ref()?.layer)).and_then(photocraft_engine::warp_cmds::smart_warp_doc_space));
    let Some(t) = app.ui.transform.as_mut() else { return };
    // Warp moves layers and paths (#3027): a lone mask or channel keeps the box, and so do
    // several layers (#6501).
    if t.warp.is_some() || t.target.is_some() || !t.layers.is_empty() {
        return;
    }
    if t.quad == corners(t.rect)
        && !t.selection
        && t.path.is_none()
        && let Some(w) = existing
    {
        t.rect = w.bounds;
        t.quad = corners(w.bounds);
        t.warp = Some(w);
        return;
    }
    let h = Homography::rect_to_quad(t.rect, t.quad).unwrap_or(Homography::IDENTITY);
    let mesh = BezierMesh::identity(t.rect, 1, 1).map_points(|p| {
        let (x, y) = h.apply(p[0], p[1]);
        [x, y]
    });
    t.warp = Some(Warp::custom(mesh, t.rect));
}

/// The open transform moves an editable type layer as a whole (not a lone mask, channel, path or
/// the selection). Its Warp is Warp Text: only the styles, and the type stays editable (#3004).
pub fn warps_type(app: &PhotocraftApp) -> bool {
    let Some(t) = &app.ui.transform else { return false };
    if t.selection || t.target.is_some() || t.path.is_some() {
        return false;
    }
    app.session.active().and_then(|st| st.doc.layer(LayerId(t.layer))).is_some_and(|l| matches!(l.content, LayerContent::Text(_)))
}

/// Warp on editable type, as in Photoshop: no rasterizing, no mesh to drag, just the warp styles
/// of the options bar (Warp Text's). A box scaled or rotated first is applied first (that keeps
/// type editable). The preview shows the type without its current Warp Text, under that style.
fn enter_type_warp(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.ui.transform.as_ref().is_some_and(|t| t.quad != corners(t.rect)) {
        commit(app);
        if app.ui.transform.is_some() {
            return; // refused: the box stays as it was
        }
        if let Err(e) = begin(app, ctx) {
            app.ui.status = e;
            return;
        }
    }
    let (Some(t), Some(st)) = (app.ui.transform.as_ref(), app.session.active()) else { return };
    let (id, session) = (LayerId(t.layer), t.session);
    let doc = st.doc.clone();
    let Some(LayerContent::Text(text)) = doc.layer(id).map(|l| &l.content) else { return };
    let current = text.warp.clone();
    let mut plain = (*doc).clone();
    if let Some(LayerContent::Text(tt)) = plain.layer_mut(id).map(|l| &mut l.content)
        && current.is_some()
    {
        tt.warp = None;
        photocraft_engine::type_cmds::refresh(&doc, tt);
    }
    let Some(b) = plain.layer(id).map(|l| photocraft_engine::transform_cmds::transform_bounds(&plain, l)).filter(|b| !b.is_empty()) else { return };
    let rect = [b.x0 as f64, b.y0 as f64, b.x1 as f64, b.y1 as f64];
    if current.is_some()
        && let Some(pv) = app.transform_preview.as_mut()
    {
        let max_side = ctx.input(|i| i.max_texture_side).max(app.gpu.as_ref().map_or(1, |g| g.max_texture_side()));
        // Read as the session's texels were: without the style when the box draws it itself.
        let (image, uv) = preview_image(&plain, id, None, b, max_side, pv.styled.is_some());
        pv.texture = crate::transform_tex::PreviewTextures::new(ctx, format!("transform-{session}"), image, uv);
    }
    let warp =
        current.as_ref().and_then(|w| WarpStyle::parse(&w.style).filter(|s| s.is_preset()).map(|s| (w, s))).map_or(Warp::none(rect), |(w, style)| Warp {
            style,
            bend: f64::from(w.value),
            h_distort: f64::from(w.horizontal_distortion),
            v_distort: f64::from(w.vertical_distortion),
            vertical: !w.horizontal,
            ..Warp::none(rect)
        });
    if let Some(t) = app.ui.transform.as_mut() {
        t.rect = rect;
        t.quad = corners(rect);
        t.pivot = [(rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0];
        t.warp = Some(warp);
    }
}

/// Leave Warp mode (back to the box; warp edits are dropped, like Photoshop's mode toggle on an
/// untouched warp).
pub fn leave_warp(app: &mut PhotocraftApp) {
    if let Some(t) = app.ui.transform.as_mut() {
        t.warp = None;
    }
    if let Some(pv) = app.transform_preview.as_mut() {
        pv.split_tool = None;
        pv.split_pointer = None;
        pv.split_placing = false;
        pv.split_quick = false;
    }
}
