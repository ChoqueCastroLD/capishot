//! The touch interface's pieces over the canvas (the simple interface, and any window a finger
//! has touched: tablets): the touch modifier button, the quick size and opacity sliders, the
//! selection's action bar, the transform's Done and Cancel, the type bar above the keyboard, the
//! color loupe, and what the multi-finger taps do. A desktop driven by a mouse sees none of it.

use egui::{Align2, Color32, CornerRadius, Id, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};
use serde_json::json;

use crate::PhotocraftApp;
use crate::theme::Tokens;
use crate::touch_input::{BUTTON_RADIUS, StylusMode, TouchAction};

/// The touch interface applies: the simple interface is on, or a finger has touched the window.
pub fn active(app: &PhotocraftApp) -> bool {
    app.simple || app.touch.touch_seen
}

/// Before the frame: route the fingers (touch_input.rs) with this frame's preferences.
pub fn route(app: &mut PhotocraftApp, ctx: &egui::Context, raw: &mut egui::RawInput) {
    let p = &app.session.prefs().enhanced_controls;
    app.touch.mode = StylusMode::from_pref(p.stylus_input);
    app.touch.gestures = p.touch_gestures;
    app.touch.button_enabled = p.touch_shortcut && active(app) && app.session.active().is_some() && !app.ui_hidden && app.ui.text_edit.is_none();
    app.touch.precision = if active(app) { precision(app.ui.tool) } else { crate::touch_input::Precision::Direct };
    let pen_down = app.stylus.feed.get().is_some();
    // The offset cursor is for fingers that land on the canvas itself, not on the bars over it.
    let canvas = app.last_canvas_rect;
    let on_canvas = |p: Pos2| canvas.contains(p) && ctx.layer_id_at(p).is_none_or(|l| l.order == egui::Order::Background) && !covers(ctx, p);
    app.touch.route_with(raw, pen_down, &on_canvas);
    if app.touch.wants_repaint() {
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
    }
}

/// The tools with an offset cursor (and its loupe) on a touch screen.
pub fn precision(tool: crate::state::Tool) -> crate::touch_input::Precision {
    use crate::state::Tool as T;
    use crate::touch_input::Precision as P;
    match tool {
        T::Lasso | T::CloneStamp | T::Healing | T::SpotHealing | T::Eyedropper => P::Drag,
        T::MagicWand | T::PolygonLasso | T::ColorSampler => P::Aim,
        _ => P::Direct,
    }
}

/// At the start of the frame: what the multi-finger taps asked for.
/// Whether the touch interface is on this frame, for widgets that have no app to ask.
pub fn flagged(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(Id::new("touch-ui-flag"))).unwrap_or(false)
}

pub fn begin_frame(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let on = active(app);
    ctx.data_mut(|d| d.insert_temp(Id::new("touch-ui-flag"), on));
    // A finger's double tap is slower than a mouse's double click (and a phone's frames slower
    // still): the Polygonal Lasso's double tap to close missed it.
    let delay = if on { 0.5 } else { 0.3 };
    if ctx.options(|o| o.input_options.max_double_click_delay) != delay {
        ctx.options_mut(|o| o.input_options.max_double_click_delay = delay);
    }
    for a in app.touch.take_actions() {
        match a {
            TouchAction::Undo | TouchAction::Redo => {
                // Never while a tool is mid-gesture (the canvas has dropped the stroke by now).
                let id = if a == TouchAction::Undo { "edit.undo" } else { "edit.redo" };
                let enabled = app.session.active().is_some_and(|d| if a == TouchAction::Undo { d.history.can_undo() } else { d.history.can_redo() });
                if enabled && let Err(e) = crate::menus::invoke(app, ctx, id, json!({})) {
                    app.ui.status = e;
                }
            }
            TouchAction::ToggleUi => {
                app.ui_hidden = !app.ui_hidden;
                crate::simple_ui::set_sheet(ctx, crate::simple_ui::Sheet::Closed);
            }
        }
    }
    if app.session.active().is_none() {
        app.ui_hidden = false;
    }
}

/// Note where a touch piece drawn elsewhere is (the type bar's buttons), for `ui.inspect`.
pub fn note_rect(ctx: &egui::Context, label: &str, rect: Rect) {
    note(ctx, label, rect);
}

fn note(ctx: &egui::Context, label: &str, rect: Rect) {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<(String, [f32; 4])>>(rects_id()).push((label.to_string(), [rect.min.x, rect.min.y, rect.max.x, rect.max.y])));
}

fn rects_id() -> Id {
    Id::new("touch-ui-rects")
}

fn prev_rects_id() -> Id {
    Id::new("touch-ui-rects-prev")
}

/// `ui.inspect`'s `touch`: the router's state and where the touch pieces were drawn.
pub fn inspect(app: &PhotocraftApp, ctx: &egui::Context) -> serde_json::Value {
    let rects: Vec<(String, [f32; 4])> = ctx.data(|d| d.get_temp(rects_id())).unwrap_or_default();
    let b = app.touch.button;
    json!({
        "active": active(app),
        "touchSeen": app.touch.touch_seen,
        "penSeen": app.touch.pen_seen,
        "fingersPaint": app.touch.fingers_paint(),
        "uiHidden": app.ui_hidden,
        "button": {"center": b.center.map(|c| [c.x, c.y]), "held": b.held, "alt": b.alt, "moving": b.moving, "enabled": app.touch.button_enabled},
        "modifiers": {"shift": app.touch.held_modifiers().shift, "alt": app.touch.held_modifiers().alt},
        "lastFinger": app.touch.last_start,
        "fingers": app.touch.finger_count(),
        "eguiTouches": ctx.input(|i| i.any_touches()),
        "pointerDown": ctx.input(|i| i.pointer.primary_down()),
        "rects": rects.into_iter().map(|(l, r)| json!({"label": l, "rect": r})).collect::<Vec<_>>(),
    })
}

/// Height the simple interface's floating bars take at the bottom (the button sits above them).
fn bottom_reserve(app: &PhotocraftApp) -> f32 {
    if app.simple { 8.0 + 54.0 + 6.0 + 54.0 } else { 24.0 }
}

/// The touch pieces over the canvas, after it and the simple interface's bars.
pub fn overlays(app: &mut PhotocraftApp, ctx: &egui::Context) {
    // Last frame's pieces stay known (`covers`) while this frame draws them again.
    ctx.data_mut(|d| {
        let last = d.remove_temp::<Vec<(String, [f32; 4])>>(rects_id()).unwrap_or_default();
        d.insert_temp(prev_rects_id(), last);
    });
    if !active(app) || app.session.active().is_none() {
        return;
    }
    if app.ui_hidden {
        show_ui_button(app, ctx);
        return;
    }
    if app.simple {
        status_toast(app, ctx);
    }
    // A tool drawing on the canvas: the selection bar waits (a tap on the bar is no drawing).
    let busy = app.drag.is_some() || app.touch.tool_finger().is_some_and(|p| app.last_canvas_rect.contains(p) && !covers(ctx, p)) && !slider_held(ctx);
    if app.ui.transform.is_some() {
        transform_bar(app, ctx);
    } else if app.ui.text_edit.is_some() {
        type_bar(app, ctx);
    } else {
        if quick_slider_tool(app) {
            quick_sliders(app, ctx);
        }
        if !busy && has_selection(app) && crate::simple_ui::sheet(ctx) == crate::simple_ui::Sheet::Closed && !app.ui.palette_open {
            selection_bar(app, ctx);
        }
    }
    if app.touch.button_enabled {
        modifier_button(app, ctx);
    }
    // The offset cursor's loupe while a precision tool's finger is down, or a long press's pick.
    let pick = pick_point(app, ctx);
    if let Some(at) = pick.or(app.touch.cursor) {
        loupe(app, ctx, at);
    }
}

fn has_selection(app: &PhotocraftApp) -> bool {
    app.session.active().is_some_and(|d| d.doc.selection.is_some())
}

/// The tools the quick size and opacity sliders are for: those that paint with a brush.
pub fn quick_slider_tool(app: &PhotocraftApp) -> bool {
    app.ui.tool.is_brushlike()
}

/// The canvas's free area: below the top bar and options row, above the floating bars.
fn canvas_area(app: &PhotocraftApp, ctx: &egui::Context) -> Rect {
    let screen = crate::safe_area::rect(ctx);
    let top = if app.simple { 44.0 + if app.ui.panels.options_bar { 44.0 } else { 0.0 } } else { 80.0 };
    Rect::from_min_max(pos2(screen.left(), screen.top() + top), pos2(screen.right(), screen.bottom() - bottom_reserve(app)))
}

/// A touch-sized round button with an icon (or text), drawn at `rect`.
fn round_button(ui: &mut egui::Ui, rect: Rect, icon: Option<&str>, text: &str, selected: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let resp = ui.allocate_rect(rect, Sense::click());
    let fill = if selected { t.accent } else if resp.is_pointer_button_down_on() { t.hover } else { t.card };
    ui.painter().rect(rect, CornerRadius::same((rect.height() / 2.0) as u8), fill, Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
    let fg = if selected { Color32::WHITE } else { t.text };
    match icon {
        Some(i) => crate::icons::paint(ui, rect, i, 20.0, fg),
        None => {
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, crate::theme::semibold(13.0), fg);
        }
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text));
    resp
}

/// The selection's action bar: what a phone does next with a selection, one tap away. Each
/// button has its name (short) under the icon; the full name is its accessible label.
const SELECTION_ACTIONS: [(&str, &str, &str, &str); 9] = [
    ("select.deselect", "Deselect", "Deselect", "x"),
    ("select.inverse", "Invert", "Invert", "contrast"),
    ("edit.fill", "Fill", "Fill", "paint-bucket"),
    ("edit.inpaintAi", "Clean with AI", "AI clean", "sparkles"),
    ("edit.copy", "Copy", "Copy", "copy"),
    ("edit.cut", "Cut", "Cut", "scissors"),
    ("edit.freeTransform", "Transform", "Transform", "scaling"),
    ("select.modify.feather", "Feather", "Feather", "circle-dashed"),
    ("edit.generativeFill", "Generative Fill", "Gen. Fill", "wand-sparkles"),
];

/// A selection bar button: the icon in a circle over its name (two short lines at most).
const SEL_CELL: Vec2 = vec2(60.0, 66.0);

fn selection_bar(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    let area = canvas_area(app, ctx);
    let gap = 2.0;
    let n = SELECTION_ACTIONS.len() as f32;
    let full = n * SEL_CELL.x + (n - 1.0) * gap + 12.0;
    let width = full.min(area.width() - 16.0);
    let r = Rect::from_min_size(pos2(area.center().x - width / 2.0, area.top() + 8.0), vec2(width, SEL_CELL.y + 12.0));
    let mut run = None;
    egui::Area::new(Id::new("touch-selection-bar")).order(egui::Order::Middle).fixed_pos(r.min).show(ctx, |ui| {
        egui::Frame::NONE.fill(t.card).stroke(Stroke::new(1.0, t.card_border)).corner_radius(CornerRadius::same(18)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
            ui.set_width(width - 12.0);
            let out = egui::ScrollArea::horizontal().id_salt("touch-selection-scroll").scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for (id, name, short, icon) in SELECTION_ACTIONS {
                        let (rect, resp) = ui.allocate_exact_size(SEL_CELL, Sense::click());
                        let name = crate::i18n::t(name);
                        let p = ui.painter();
                        let c = pos2(rect.center().x, rect.top() + 20.0);
                        p.circle(c, 19.0, if resp.is_pointer_button_down_on() { t.hover } else { t.chrome }, Stroke::new(1.0, t.card_border));
                        crate::icons::paint(ui, Rect::from_center_size(c, Vec2::splat(30.0)), icon, 19.0, t.text);
                        // One line, a little smaller for a long word ("Deseleccionar"), else two.
                        let text = crate::i18n::t(short).to_string();
                        let room = SEL_CELL.x - 2.0;
                        let one = ui.painter().layout_no_wrap(text.clone(), crate::theme::medium(10.0), t.text_dim).size().x;
                        let size = if one > room { (10.0 * room / one).max(8.0) } else { 10.0 };
                        let mut job = egui::text::LayoutJob::simple(text, crate::theme::medium(size), t.text_dim, room);
                        job.wrap.max_rows = 2;
                        job.halign = egui::Align::Center;
                        let g = ui.painter().layout_job(job);
                        ui.painter().galley(pos2(rect.center().x, rect.top() + 42.0), g, t.text_dim);
                        let resp = resp.on_hover_text(name);
                        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
                        note(ctx, &format!("sel:{id}"), rect);
                        if resp.clicked() {
                            run = Some(id);
                        }
                    }
                });
            });
            // More buttons than room: a fade at the edge that has more, so the bar reads as one
            // that scrolls.
            let view = out.inner_rect;
            let x = out.state.offset.x;
            let more_right = x + view.width() < out.content_size.x - 1.0;
            let more_left = x > 1.0;
            let fade = |ui: &egui::Ui, right: bool| {
                let steps = 12;
                let w = 28.0;
                for k in 0..steps {
                    let a = ((k + 1) as f32 / steps as f32 * 230.0) as u8;
                    let xk = if right { view.right() - w + w * k as f32 / steps as f32 } else { view.left() + w - w * (k + 1) as f32 / steps as f32 };
                    let col = Color32::from_rgba_unmultiplied(t.card.r(), t.card.g(), t.card.b(), a);
                    ui.painter().rect_filled(Rect::from_min_max(pos2(xk, view.top()), pos2(xk + w / steps as f32 + 0.5, view.bottom())), 0.0, col);
                }
            };
            if more_right {
                fade(ui, true);
                note(ctx, "selectionBarMore", Rect::from_min_max(pos2(view.right() - 28.0, view.top()), view.right_bottom()));
            }
            if more_left {
                fade(ui, false);
            }
        });
    });
    note(ctx, "selectionBar", r);
    if let Some(id) = run
        && let Err(e) = crate::menus::invoke(app, ctx, id, json!({}))
        && e != crate::file_dialog::CANCELLED
    {
        app.ui.status = e;
    }
}

/// The transform's Done and Cancel, big, at the top of the canvas.
fn transform_bar(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let area = canvas_area(app, ctx);
    let (w, h) = (104.0, 44.0);
    let cancel = Rect::from_min_size(pos2(area.left() + 10.0, area.top() + 8.0), vec2(w, h));
    let done = Rect::from_min_size(pos2(area.right() - 10.0 - w, area.top() + 8.0), vec2(w, h));
    let mut act = None;
    egui::Area::new(Id::new("touch-transform-bar")).order(egui::Order::Foreground).fixed_pos(area.min).show(ctx, |ui| {
        ui.set_min_size(Vec2::ZERO);
        if round_button(ui, cancel, None, crate::i18n::t("Cancel"), false).clicked() {
            act = Some(false);
        }
        if round_button(ui, done, None, crate::i18n::t("Done"), true).clicked() {
            act = Some(true);
        }
    });
    note(ctx, "transformCancel", cancel);
    note(ctx, "transformDone", done);
    match act {
        Some(true) => crate::transform_tool::commit(app),
        Some(false) => crate::transform_tool::cancel(app),
        None => {}
    }
}

fn slider_id() -> Id {
    Id::new("touch-quick-slider")
}

/// A quick slider is being dragged (the brush preview shows, the selection bar waits).
fn slider_held(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<u8>(slider_id())).is_some_and(|k| k > 0)
}

/// Brush size on the quick slider: a cube curve, so small sizes get most of the travel.
const SIZE_MAX: f32 = 1000.0;
pub fn size_to_t(size: f32) -> f32 {
    ((size.clamp(1.0, SIZE_MAX) - 1.0) / (SIZE_MAX - 1.0)).cbrt()
}
pub fn t_to_size(t: f32) -> f32 {
    (1.0 + t.clamp(0.0, 1.0).powi(3) * (SIZE_MAX - 1.0)).round()
}

/// The quick size and opacity sliders down the left edge (Procreate, ibis Paint): a finger
/// drags either; while it does, the brush shows at its size in the middle of the canvas.
fn quick_sliders(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    let area = canvas_area(app, ctx);
    let w = 44.0;
    let button_top = app.touch.button.center.map_or(area.bottom(), |c| c.y - BUTTON_RADIUS - 12.0);
    let avail = (button_top.min(area.bottom()) - area.top() - 70.0).max(160.0);
    let h = ((avail - 16.0) / 2.0).clamp(90.0, 200.0);
    let top = area.top() + 64.0;
    let size_r = Rect::from_min_size(pos2(area.left() + 6.0, top), vec2(w, h));
    let opac_r = size_r.translate(vec2(0.0, h + 16.0));
    crate::paint_mouse::sync_tool_brush(app);
    let before = app.session.tools.brush.clone();
    let mut b = before.clone();
    let mut held = 0u8;
    egui::Area::new(Id::new("touch-quick-sliders")).order(egui::Order::Middle).fixed_pos(size_r.min).show(ctx, |ui| {
        for (k, rect) in [(1u8, size_r), (2u8, opac_r)] {
            let resp = ui.allocate_rect(rect, Sense::click_and_drag());
            let v = if k == 1 { size_to_t(b.size) } else { b.opacity.clamp(0.0, 1.0) };
            let p = ui.painter();
            p.rect(rect, CornerRadius::same(22), t.card, Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
            let fill_h = (rect.height() - 8.0) * v;
            let fill = Rect::from_min_max(pos2(rect.left() + 4.0, rect.bottom() - 4.0 - fill_h), pos2(rect.right() - 4.0, rect.bottom() - 4.0));
            p.rect_filled(fill, CornerRadius::same(18), t.accent_soft);
            let knob = pos2(rect.center().x, rect.bottom() - 4.0 - fill_h);
            p.circle(knob, 9.0, t.accent, Stroke::new(2.0, Color32::WHITE));
            let label = if k == 1 { format!("{}", b.size.round() as i64) } else { format!("{}%", (b.opacity * 100.0).round() as i64) };
            // The value at the end away from the knob, so the knob never covers it.
            let ly = if v > 0.6 { rect.bottom() - 14.0 } else { rect.top() + 14.0 };
            p.text(pos2(rect.center().x, ly), Align2::CENTER_CENTER, label, crate::theme::semibold(11.0), t.text);
            let name = if k == 1 { tl!("Brush size slider") } else { tl!("Brush opacity slider") };
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, name));
            note(ctx, if k == 1 { "sizeSlider" } else { "opacitySlider" }, rect);
            if (resp.dragged() || resp.is_pointer_button_down_on())
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let tv = ((rect.bottom() - 4.0 - pos.y) / (rect.height() - 8.0)).clamp(0.0, 1.0);
                if k == 1 {
                    b.size = t_to_size(tv).min(photocraft_engine::paint::MAX_BRUSH_SIZE);
                } else {
                    b.opacity = (tv * 100.0).round().max(1.0) / 100.0;
                }
                held = k;
            }
        }
    });
    ctx.data_mut(|d| d.insert_temp(slider_id(), held));
    if held > 0 {
        brush_preview(app, ctx, &b, area);
    }
    crate::brush_panel::commit_gesture(app, ctx, &before, &b);
}

/// The brush at its size on screen (and its opacity) in the middle of the canvas, while a quick
/// slider moves.
fn brush_preview(app: &PhotocraftApp, ctx: &egui::Context, b: &photocraft_engine::paint::BrushSettings, area: Rect) {
    let t = Tokens::get(ctx);
    let zoom = app.point_zoom().max(0.001);
    let r = (b.size * zoom / 2.0).clamp(1.0, area.width().min(area.height()) * 0.45);
    let c = area.center();
    egui::Area::new(Id::new("touch-brush-preview")).order(egui::Order::Foreground).interactable(false).fixed_pos(area.min).show(ctx, |ui| {
        ui.allocate_exact_size(Vec2::ZERO, Sense::hover());
        let p = ui.painter();
        let fg = app.session.tools.foreground;
        let col = Color32::from_rgba_unmultiplied((fg[0] * 255.0) as u8, (fg[1] * 255.0) as u8, (fg[2] * 255.0) as u8, (b.opacity.clamp(0.0, 1.0) * 255.0) as u8);
        // The brush's colour inside a dark and a light ring: visible over any image.
        p.circle_filled(c, r, col);
        p.circle_stroke(c, r + 1.0, Stroke::new(1.5, Color32::from_black_alpha(200)));
        p.circle_stroke(c, r + 2.5, Stroke::new(1.5, Color32::from_white_alpha(220)));
        let _ = t;
    });
    note(ctx, "brushPreview", Rect::from_center_size(c, Vec2::splat(2.0 * r)));
}

/// While the interface is hidden: a small button in the corner brings it back (as four fingers do).
fn show_ui_button(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let r = Rect::from_min_size(pos2(screen.right() - 52.0, screen.top() + 8.0), Vec2::splat(44.0));
    egui::Area::new(Id::new("touch-show-ui")).order(egui::Order::Foreground).fixed_pos(r.min).show(ctx, |ui| {
        let (rect, resp) = ui.allocate_exact_size(r.size(), Sense::click());
        ui.painter().circle_filled(rect.center(), 20.0, Color32::from_black_alpha(110));
        crate::icons::paint(ui, rect, "maximize-2", 18.0, t.text);
        let label = tl!("Show the interface");
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        note(ctx, "showUi", rect);
        if resp.clicked() {
            app.ui_hidden = false;
        }
    });
}

/// The touch modifier button: a circle bottom left. The router (touch_input.rs) reads the finger
/// on it; this draws it: the inner ring is Shift, the outer Alt, and a press held alone lifts it
/// to be moved.
fn modifier_button(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let default = crate::touch_input::default_button_center(screen, bottom_reserve(app));
    app.touch.place_button(screen, default);
    let b = app.touch.button;
    let Some(c) = b.center else { return };
    let outer = crate::touch_input::ALT_RADIUS;
    let size = Vec2::splat(2.0 * outer + 8.0);
    egui::Area::new(Id::new("touch-modifier")).order(egui::Order::Foreground).interactable(false).fixed_pos(c - size / 2.0).show(ctx, |ui| {
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let p = ui.painter();
        let center = rect.center();
        if b.held && !b.moving {
            // The rings while held: Shift inside, Alt outside, the active one lit.
            p.circle(center, outer, if b.alt { t.accent } else { Color32::from_black_alpha(170) }, Stroke::new(1.5, Color32::from_white_alpha(120)));
            p.text(center + vec2(0.0, -outer + 11.0), Align2::CENTER_CENTER, "⌥ Alt", crate::theme::semibold(11.0), Color32::WHITE);
        }
        let lifted = b.moving;
        let fill = if b.held && !b.alt && !lifted { t.accent } else { t.card };
        p.add(egui::epaint::CircleShape { center: center + vec2(0.0, if lifted { 6.0 } else { 3.0 }), radius: BUTTON_RADIUS, fill: Color32::from_black_alpha(if lifted { 110 } else { 70 }), stroke: Stroke::NONE });
        p.circle(center, BUTTON_RADIUS, fill, Stroke::new(2.0, if b.held { t.accent } else { t.card_border }));
        let text = if b.held && !b.alt && !lifted { Color32::WHITE } else { t.text };
        p.text(center, Align2::CENTER_CENTER, if lifted { "✥" } else { "⇧" }, crate::theme::semibold(20.0), text);
        note(ctx, "modifier", Rect::from_center_size(center, Vec2::splat(2.0 * BUTTON_RADIUS)));
    });
}

/// Whether `p` (screen points) is on a touch piece, so the canvas leaves it alone.
pub fn covers(ctx: &egui::Context, p: Pos2) -> bool {
    let mut rects: Vec<(String, [f32; 4])> = ctx.data(|d| d.get_temp(prev_rects_id())).unwrap_or_default();
    rects.extend(ctx.data(|d| d.get_temp::<Vec<(String, [f32; 4])>>(rects_id())).unwrap_or_default());
    rects.iter().any(|(_, r)| Rect::from_min_max(pos2(r[0], r[1]), pos2(r[2], r[3])).contains(p))
}


/// Pixels across the loupe (an odd count: the middle one is the sampled pixel).
const LOUPE_PX: i32 = 15;
/// The loupe's diameter (points).
const LOUPE: f32 = 120.0;

fn loupe_cache_id() -> Id {
    Id::new("touch-loupe-cache")
}

/// The pixels around document pixel (`x`, `y`) of the active document's composite, row by row,
/// for the loupe (cached for the revision and pixel, so a still finger renders nothing).
fn loupe_pixels(app: &PhotocraftApp, ctx: &egui::Context, x: i32, y: i32) -> Option<Vec<Color32>> {
    let st = app.session.active()?;
    let key = (st.doc.id.0, st.revision, x, y);
    if let Some((k, px)) = ctx.data(|d| d.get_temp::<((u64, u64, i32, i32), std::sync::Arc<Vec<Color32>>)>(loupe_cache_id()))
        && k == key
    {
        return Some(px.as_ref().clone());
    }
    let half = LOUPE_PX / 2;
    let rect = photocraft_geom::Rect { x0: x - half, y0: y - half, x1: x + half + 1, y1: y + half + 1 };
    let buf = photocraft_compose::render(&st.doc, rect);
    let n = (LOUPE_PX * LOUPE_PX) as usize;
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let px: Vec<Color32> = (0..n)
        .map(|i| {
            let p = buf.px.get(i).copied().unwrap_or([0.0; 4]);
            // Over a checkerboard grey, like the canvas's transparency.
            let a = p[3].clamp(0.0, 1.0);
            let bg = 0.8;
            Color32::from_rgb(to8(p[0] * a + bg * (1.0 - a)), to8(p[1] * a + bg * (1.0 - a)), to8(p[2] * a + bg * (1.0 - a)))
        })
        .collect();
    ctx.data_mut(|d| d.insert_temp(loupe_cache_id(), (key, std::sync::Arc::new(px.clone()))));
    Some(px)
}

/// The magnifier over the point a finger aims at (`at`, screen points): the pixels around it
/// enlarged, the middle one framed, and a ring in the colour under it. Shown above the point (below
/// when there is no room) so neither the finger nor the loupe hides it.
pub fn loupe(app: &PhotocraftApp, ctx: &egui::Context, at: Pos2) {
    let Some(d) = crate::canvas::doc_point_at(app, at) else { return };
    let (x, y) = (d[0].floor() as i32, d[1].floor() as i32);
    let Some(px) = loupe_pixels(app, ctx, x, y) else { return };
    let t = Tokens::get(ctx);
    let screen = crate::safe_area::rect(ctx);
    let above = at.y - 70.0 - LOUPE / 2.0;
    let cy = if above - LOUPE / 2.0 > screen.top() + 8.0 { above } else { at.y + 70.0 + LOUPE / 2.0 };
    let center = pos2(at.x.clamp(screen.left() + LOUPE / 2.0 + 4.0, screen.right() - LOUPE / 2.0 - 4.0), cy);
    let mid = px[(LOUPE_PX * LOUPE_PX / 2) as usize];
    egui::Area::new(Id::new("touch-loupe")).order(egui::Order::Tooltip).interactable(false).fixed_pos(center - Vec2::splat(LOUPE / 2.0 + 6.0)).show(ctx, |ui| {
        ui.allocate_exact_size(Vec2::splat(LOUPE + 12.0), Sense::hover());
        let p = ui.painter();
        let cell = LOUPE / LOUPE_PX as f32;
        let origin = center - Vec2::splat(LOUPE / 2.0);
        // The pixel grid, clipped to the circle by painting cells whose centre is inside it.
        for row in 0..LOUPE_PX {
            for col in 0..LOUPE_PX {
                let c = origin + vec2(col as f32 * cell, row as f32 * cell);
                if (c + Vec2::splat(cell / 2.0) - center).length() > LOUPE / 2.0 - 2.0 {
                    continue;
                }
                p.rect_filled(Rect::from_min_size(c, Vec2::splat(cell + 0.5)), 0.0, px[(row * LOUPE_PX + col) as usize]);
            }
        }
        p.circle_stroke(center, LOUPE / 2.0 + 3.0, Stroke::new(6.0, mid));
        p.circle_stroke(center, LOUPE / 2.0 + 6.0, Stroke::new(1.5, t.card_border));
        p.circle_stroke(center, LOUPE / 2.0, Stroke::new(1.5, Color32::from_black_alpha(140)));
        let m = Rect::from_center_size(center, Vec2::splat(cell));
        p.rect_stroke(m, 0.0, Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Outside);
        p.rect_stroke(m.expand(1.5), 0.0, Stroke::new(1.0, Color32::BLACK), egui::StrokeKind::Outside);
    });
    note(ctx, "loupe", Rect::from_center_size(center, Vec2::splat(LOUPE)));
    // The cursor itself: a crosshair where the tool acts.
    egui::Area::new(Id::new("touch-cursor")).order(egui::Order::Tooltip).interactable(false).fixed_pos(at - Vec2::splat(14.0)).show(ctx, |ui| {
        ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
        let p = ui.painter();
        for (w, c) in [(3.0, Color32::BLACK), (1.5, Color32::WHITE)] {
            p.line_segment([at - vec2(12.0, 0.0), at - vec2(4.0, 0.0)], Stroke::new(w, c));
            p.line_segment([at + vec2(4.0, 0.0), at + vec2(12.0, 0.0)], Stroke::new(w, c));
            p.line_segment([at - vec2(0.0, 12.0), at - vec2(0.0, 4.0)], Stroke::new(w, c));
            p.line_segment([at + vec2(0.0, 4.0), at + vec2(0.0, 12.0)], Stroke::new(w, c));
        }
    });
}

fn pick_id() -> Id {
    Id::new("touch-long-press-pick")
}

/// A long press on the canvas with a painting tool picks a color (or the Clone Stamp's source)
/// while the finger stays down: the canvas calls this when the press fires.
pub fn start_pick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    app.touch.take_over();
    ctx.data_mut(|d| d.insert_temp(pick_id(), true));
    ctx.request_repaint();
}

pub fn picking(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(pick_id())).unwrap_or(false)
}

/// The long press's pick point, while it lasts; it samples as the finger moves and ends when it lifts.
fn pick_point(app: &mut PhotocraftApp, ctx: &egui::Context) -> Option<Pos2> {
    if !picking(ctx) {
        return None;
    }
    let Some(finger) = app.touch.tool_finger() else {
        ctx.data_mut(|d| d.remove::<bool>(pick_id()));
        return None;
    };
    // With the offset cursor (the Clone Stamp, the healing brushes), where it points.
    let at = app.touch.cursor.unwrap_or(finger);
    if let Some(d) = crate::canvas::doc_point_at(app, at) {
        if app.ui.tool == crate::state::Tool::CloneStamp || app.ui.tool == crate::state::Tool::Healing {
            crate::retouch_ui::set_source(app, d[0], d[1]);
        } else {
            crate::canvas::sample_eyedropper(app, d[0], d[1], egui::Modifiers::NONE);
        }
    }
    ctx.request_repaint();
    Some(at)
}

/// Two fingers whose pinch began inside the Free Transform box: their pinch scales, their twist
/// turns and their pan moves the box's contents (Photoshop on the iPad). A pinch begun outside
/// navigates. Returns whether this frame's fingers are the box's.
pub fn transform_pinch(app: &mut PhotocraftApp, ctx: &egui::Context, xf: &crate::canvas::ViewXform) -> bool {
    let id = Id::new("touch-box-pinch");
    let m = ctx.input(|i| i.multi_touch()).filter(|m| m.num_touches >= 2);
    let Some(m) = m.filter(|_| active(app) && app.ui.transform.as_ref().is_some_and(|t| t.warp.is_none())) else {
        ctx.data_mut(|d| d.remove::<bool>(id));
        return false;
    };
    let inside = match ctx.data(|d| d.get_temp::<bool>(id)) {
        Some(v) => v,
        None => {
            let quad = app.ui.transform.as_ref().map(|t| t.quad).unwrap_or_default();
            let pts: Vec<Pos2> = quad.iter().map(|q| xf.to_screen(q[0] as f32, q[1] as f32)).collect();
            // The fingers' centre when the pinch is first seen (egui's `start_pos` is the
            // first finger's place, which a small box doesn't contain).
            let v = point_in_polygon(&pts, m.center_pos) || point_in_polygon(&pts, m.start_pos);
            ctx.data_mut(|d| d.insert_temp(id, v));
            v
        }
    };
    if !inside {
        return false;
    }
    let prev = m.center_pos - m.translation_delta;
    let (c, p0) = (xf.to_doc(m.center_pos), xf.to_doc(prev));
    let move_by = [c[0] - p0[0], c[1] - p0[1]];
    let s = f64::from(m.zoom_delta).clamp(0.5, 2.0);
    let a = f64::from(m.rotation_delta) * if xf.flip { -1.0 } else { 1.0 };
    let (sin, cos) = a.sin_cos();
    let map = |q: [f64; 2]| {
        let (x, y) = (q[0] + move_by[0] - c[0], q[1] + move_by[1] - c[1]);
        [c[0] + s * (cos * x - sin * y), c[1] + s * (sin * x + cos * y)]
    };
    if let Some(t) = app.ui.transform.as_mut()
        && s.is_finite()
        && a.is_finite()
        && move_by.iter().all(|v| v.is_finite())
    {
        for q in &mut t.quad {
            *q = map(*q);
        }
        t.pivot = map(t.pivot);
    }
    ctx.request_repaint();
    true
}

fn point_in_polygon(pts: &[Pos2], p: Pos2) -> bool {
    let mut c = false;
    let n = pts.len();
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + n - 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            c = !c;
        }
    }
    c
}

/// Degrees within which a two-finger twist let go snaps the view upright.
pub const SNAP_DEG: f32 = 6.0;

/// When two fingers lift after turning the view, a turn within [`SNAP_DEG`] of upright snaps to 0.
pub fn snap_rotation(ctx: &egui::Context, rotation: &mut f32) {
    let id = Id::new("touch-turn-active");
    let multi = ctx.input(|i| i.multi_touch().is_some_and(|m| m.num_touches >= 2));
    let was = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    if multi != was {
        ctx.data_mut(|d| d.insert_temp(id, multi));
    }
    if was && !multi {
        let d = rotation.rem_euclid(360.0);
        if d != 0.0 && (d <= SNAP_DEG || d >= 360.0 - SNAP_DEG) {
            *rotation = 0.0;
        }
    }
}

/// While text is edited: the type bar at the bottom of the visible window, which the keyboard
/// shrinks (`interactive-widget=resizes-content`, and the web shell fits the page to the visual
/// viewport on iOS), so it sits right above the keyboard.
fn type_bar(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    let screen = crate::safe_area::rect(ctx);
    let h = 56.0;
    let r = Rect::from_min_size(pos2(screen.left() + 6.0, screen.bottom() - h - 6.0), vec2(screen.width() - 12.0, h));
    egui::Area::new(Id::new("touch-type-bar")).order(egui::Order::Foreground).fixed_pos(r.min).show(ctx, |ui| {
        egui::Frame::NONE.fill(t.card).stroke(Stroke::new(1.0, t.card_border)).corner_radius(CornerRadius::same(18)).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            ui.set_width(r.width() - 16.0);
            ui.horizontal_centered(|ui| crate::type_tool::touch_bar(app, ui));
        });
    });
    note(ctx, "typeBar", r);
}

#[cfg(test)]
#[path = "touch_ui_tests.rs"]
mod tests;

/// Seconds a status message stays up as a toast.
const TOAST_S: f64 = 3.0;

/// The simple interface has no status bar: a new status message (an error, a hint such as the
/// Clone Stamp's "Alt-click to define a source point") shows for a few seconds as a pill at the
/// top of the canvas, so nothing fails silently.
fn status_toast(app: &PhotocraftApp, ctx: &egui::Context) {
    let id = Id::new("touch-status-toast");
    let now = ctx.input(|i| i.time);
    let text = app.ui.status.trim().to_string();
    let (last, since): (String, f64) = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    let since = if text != last {
        ctx.data_mut(|d| d.insert_temp(id, (text.clone(), now)));
        now
    } else {
        since
    };
    // A notice already says it (top of the window).
    if text.is_empty() || now - since > TOAST_S || !app.ui.notices.is_empty() {
        return;
    }
    let t = Tokens::get(ctx);
    let area = canvas_area(app, ctx);
    let font = crate::theme::medium(13.0);
    let max_w = area.width() - 40.0;
    let galley = ctx.fonts_mut(|f| f.layout(text.clone(), font, if app.ui.status_error { t.warning } else { t.text }, max_w));
    let size = galley.size() + vec2(28.0, 16.0);
    let pos = pos2(area.center().x - size.x / 2.0, area.top() + 70.0);
    egui::Area::new(id).order(egui::Order::Foreground).interactable(false).fixed_pos(pos).show(ctx, |ui| {
        let (r, _) = ui.allocate_exact_size(size, Sense::hover());
        ui.painter().rect(r, CornerRadius::same(14), t.card, Stroke::new(1.0, if app.ui.status_error { t.warning } else { t.card_border }), egui::StrokeKind::Inside);
        ui.painter().galley(r.min + vec2(14.0, 8.0), galley, t.text);
    });
    note(ctx, "toast", Rect::from_min_size(pos, size));
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(TOAST_S - (now - since) + 0.05));
}
