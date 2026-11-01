use super::selection::floating_outline;
use super::view_xform::ViewXform;
use super::{Color32, LayerContent, PhotocraftApp, Pos2, Rect, Stroke, Tool, Vec2, pos2, vec2};

/// View › Show › Layer Edges: the colour of the active layer's outline.
pub(crate) const LAYER_EDGES: Color32 = Color32::from_rgb(0x2d, 0x8c, 0xeb);

/// View › Show › Layer Edges: the active layer's content bounds (cached per tile) where the canvas
/// shows them. During a Move drag that is at the offset the image was drawn with this frame, so
/// the outline moves in lockstep with the layer's pixels (#2413).
pub(crate) fn layer_edges_rect(app: &PhotocraftApp, idx: usize) -> Option<photocraft_geom::Rect> {
    if !app.ui.view.shows(app.ui.view.show.layer_edges) {
        return None;
    }
    let st = app.session.documents().get(idx)?;
    let id = st.active_layer?;
    let b = st.doc.layer(id)?.surface().map(photocraft_compose::bounds::content_bounds)?;
    if b.is_empty() {
        return None;
    }
    let (dx, dy) = crate::move_ui::shown_layer_offset(app, st.doc.id, st.revision, id).unwrap_or((0, 0));
    Some(b.translate(dx, dy))
}

/// Move › Show Transform Controls: the option, or a phone (the simple interface), where the box
/// on the selected layer is how it is scaled and turned (no keys, no menus at hand).
pub(crate) fn shows_controls(app: &PhotocraftApp) -> bool {
    app.ui.tool_options.move_show_transform || app.simple
}

/// On a touch screen the controls are finger-sized: four corners grabbed within
/// [`crate::transform_tool::TOUCH_HANDLE_PX`] and a rotate handle above the box.
fn touch(app: &PhotocraftApp) -> bool {
    crate::touch_ui::active(app)
}

/// How far above the box's top edge the touch rotate handle sits (screen points).
pub(crate) const ROTATE_HANDLE_GAP: f32 = 36.0;

/// The touch rotate handle's centre for the box `r`.
pub(crate) fn rotate_handle(r: Rect) -> Pos2 {
    r.center_top() - vec2(0.0, ROTATE_HANDLE_GAP)
}

/// A touch press on the controls: a corner (scales) or the rotate handle (turns).
fn touch_hit(r: Rect, p: Pos2) -> bool {
    let grab = crate::transform_tool::TOUCH_HANDLE_PX as f32;
    [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), rotate_handle(r)].into_iter().any(|h| h.distance(p) <= grab)
}

/// The layers Move › Show Transform Controls frames: every selected layer but the Background.
/// A corner radius drag hides the box, as in Photoshop. So does View › Extras (⌘H, #6699): the
/// controls are an Extra, hidden and inert until Extras comes back, while Show Transform Controls
/// keeps its setting.
pub(super) fn transform_controls_layers(app: &PhotocraftApp) -> Vec<&photocraft_doc::Layer> {
    if app.ui.tool != Tool::Move
        || !shows_controls(app)
        || !app.ui.view.extras
        || app.ui.transform.is_some()
        || app.drag.is_some()
        || app.ui.corner_radius.is_some()
    {
        return Vec::new();
    }
    let Some(st) = app.session.active() else { return Vec::new() };
    st.selected_layers().into_iter().filter_map(|id| st.doc.layer(id)).filter(|l| !crate::doc_props_ui::is_background(&st.doc, l)).collect()
}

/// Move tool › Show Transform Controls: the boxes Free Transform would open on the selected
/// layers, joined into one.
pub(super) fn transform_controls_rect(app: &PhotocraftApp, xf: &ViewXform) -> Option<Rect> {
    let st = app.session.active()?;
    let layers = transform_controls_layers(app);
    let b = layers.iter().map(|l| photocraft_engine::transform_cmds::transform_frame(&st.doc, l)).filter(|b| !b.is_empty()).reduce(|a, b| a.union(&b))?;
    // Floating selected pixels are framed by their outline, as their ants are (#7538), not by the
    // selection they were cut with; a frame round other selected layers too keeps them all.
    let alone = matches!(layers.as_slice(), [l] if photocraft_engine::float_cmds::floating(st).is_some_and(|f| f.layer == l.id));
    let b = app.session.active_index().filter(|_| alone).and_then(|idx| floating_outline(app, idx)).map_or(b, |o| o.content_bounds());
    let (dx, dy) = crate::move_ui::floating_offset(app).unwrap_or((0, 0));
    let b = b.translate(dx, dy);
    if b.is_empty() {
        return None;
    }
    Some(Rect::from_two_pos(xf.to_screen(b.x0 as f32, b.y0 as f32), xf.to_screen(b.x1 as f32, b.y1 as f32)))
}

/// The Move tool shows transform handles even before a Free Transform session exists.
/// Use the same hit areas for pointer-down and hover, so a control never behaves like
/// resize/rotate while showing the ordinary four-arrow Move cursor (#2871).
pub(super) fn transform_controls_cursor(r: Rect, p: Pos2) -> Option<egui::CursorIcon> {
    let handles = [r.left_top(), r.center_top(), r.right_top(), r.right_center(), r.right_bottom(), r.center_bottom(), r.left_bottom(), r.left_center()];
    let grab = crate::transform_tool::HANDLE_PX as f32;
    for (i, handle) in handles.into_iter().enumerate() {
        if handle.distance(p) <= grab {
            return Some(match i {
                0 | 4 => egui::CursorIcon::ResizeNwSe,
                2 | 6 => egui::CursorIcon::ResizeNeSw,
                1 | 5 => egui::CursorIcon::ResizeVertical,
                _ => egui::CursorIcon::ResizeHorizontal,
            });
        }
    }
    // The rotation band: the pointer gives way to the curved rotate arrow (`transform_controls_turn`).
    in_rotation_band(r, p).then_some(egui::CursorIcon::None)
}

fn in_rotation_band(r: Rect, p: Pos2) -> bool {
    r.expand(18.0).contains(p) && !r.expand(5.0).contains(p)
}

/// In the rotation band just outside the Move tool's transform controls, the screen direction the
/// curved rotate arrow bends round: into the box along the nearest corner's bisector, as Free
/// Transform draws it (#4658). Before, the band showed a plain alias pointer, so nothing said a
/// drag there rotates.
pub(super) fn transform_controls_turn(r: Rect, p: Pos2) -> Option<Vec2> {
    if !in_rotation_band(r, p)
        || [r.left_top(), r.center_top(), r.right_top(), r.right_center(), r.right_bottom(), r.center_bottom(), r.left_bottom(), r.left_center()]
            .into_iter()
            .any(|h| h.distance(p) <= crate::transform_tool::HANDLE_PX as f32)
    {
        return None;
    }
    let corner = pos2(if p.x < r.center().x { r.left() } else { r.right() }, if p.y < r.center().y { r.top() } else { r.bottom() });
    Some(vec2((r.center().x - corner.x).signum(), (r.center().y - corner.y).signum()).normalized() * 16.0)
}

/// A visible handle starts scaling; the narrow band just outside the box starts rotation.
/// The interior stays the normal Move-tool drag target.
pub(super) fn transform_controls_hit(r: Rect, p: Pos2) -> bool {
    transform_controls_cursor(r, p).is_some()
}

/// Enter the existing Free Transform session when a Move-tool transform control is pressed: on
/// every selected layer, as the frame shows them (#6501).
pub(super) fn begin_transform_controls_at(app: &mut PhotocraftApp, ctx: &egui::Context, xf: &ViewXform, p: Pos2) -> bool {
    let Some(r) = transform_controls_rect(app, xf) else { return false };
    let finger = touch(app);
    if !(if finger { touch_hit(r, p) } else { transform_controls_hit(r, p) }) {
        return false;
    }
    match crate::transform_tool::begin(app, ctx) {
        Ok(()) => {
            // A finger's handle drag is a quick transform: it commits when the finger lifts.
            if finger && let Some(t) = app.ui.transform.as_ref() {
                crate::touch_ui::commit_when_lifted(ctx, t.session);
            }
            true
        }
        Err(e) => {
            app.ui.status = e;
            false
        }
    }
}

/// Move tool › Show Transform Controls: the selected layers' bounding box, with its eight handles,
/// and the outline of every selected shape layer, as in Photoshop.
pub(super) fn draw_transform_controls(app: &mut PhotocraftApp, painter: &egui::Painter, xf: &ViewXform) {
    let Some(r) = transform_controls_rect(app, xf) else { return };
    let accent = crate::theme::Tokens::get(painter.ctx()).accent;
    let to_scr = |q: [f64; 2]| xf.to_screen(q[0] as f32, q[1] as f32);
    for l in transform_controls_layers(app) {
        if let LayerContent::Shape(sh) = &l.content {
            crate::vector_ui::draw_outline(painter, &sh.path, &to_scr, accent);
        }
    }
    painter.rect_stroke(r, 0.0, Stroke::new(1.0, accent), egui::StrokeKind::Middle);
    if touch(app) {
        // Finger-sized: a 44 pt ring at each corner and a rotate handle above the box.
        for p in [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()] {
            painter.circle(p, crate::transform_tool::TOUCH_HANDLE_PX as f32, Color32::from_rgba_unmultiplied(255, 255, 255, 50), Stroke::new(2.0, accent));
            painter.circle(p, 6.0, Color32::WHITE, Stroke::new(1.5, accent));
        }
        let h = rotate_handle(r);
        painter.line_segment([r.center_top(), h], Stroke::new(1.5, accent));
        painter.circle(h, 14.0, Color32::WHITE, Stroke::new(1.5, accent));
        // A turning arrow: three quarters of a circle with a head.
        let arc: Vec<Pos2> = (0..=18)
            .map(|k| {
                let a = -1.2 + 4.4 * k as f32 / 18.0;
                h + vec2(a.cos(), a.sin()) * 7.0
            })
            .collect();
        if let Some(&end) = arc.last() {
            painter.add(egui::Shape::line(arc, Stroke::new(1.8, accent)));
            painter.line_segment([end, end + vec2(4.0, -1.0)], Stroke::new(1.8, accent));
            painter.line_segment([end, end + vec2(-0.5, -4.0)], Stroke::new(1.8, accent));
        }
        crate::touch_ui::note_rect(painter.ctx(), "layerRotate", Rect::from_center_size(h, vec2(28.0, 28.0)));
        return;
    }
    for p in [r.left_top(), r.center_top(), r.right_top(), r.right_center(), r.right_bottom(), r.center_bottom(), r.left_bottom(), r.left_center()] {
        let h = Rect::from_center_size(p, vec2(7.0, 7.0));
        painter.rect_filled(h, 0.0, Color32::WHITE);
        painter.rect_stroke(h, 0.0, Stroke::new(1.0, accent), egui::StrokeKind::Inside);
    }
}
