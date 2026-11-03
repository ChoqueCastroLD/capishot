//! The colour controls' eyedropper (the pipette button next to a swatch): one press on the image
//! then sets that control's colour to the image's composite colour at that point, applied live
//! like the control's other edits. Efectos (gradient stops, shadow, glow), Contorno, the Color
//! Picker and the Layer Style dialog's colours have one.
//!
//! Armed, the next primary press on the canvas (a click, or a tap on a phone) is the pick, for
//! every tool, and never reaches the tool; the popover or dialog it came from stays open. A
//! phone's dialog steps aside while armed (it would cover the image) and comes back after the
//! pick. Esc, the button again or the hint bar's Cancel disarm without changing anything.

use egui::{Context, Id, Key, Pos2, Rect};

use crate::PhotocraftApp;

/// The colour an armed eyedropper sets.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// Efectos: colour `field` of the type target's `layer.layerStyle.<kind>`, in the popover's
    /// undo step `key`.
    TypeEffect { kind: String, field: String, key: String },
    /// Contorno: the type target's stroke colour, in the popover's undo step `key`.
    TypeStroke { key: String },
    /// The Color Picker dialog `id`'s new colour.
    ColorPicker { id: u64 },
    /// Colour `field` of effect `effect` in the Layer Style dialog `id`.
    LayerStyle { id: u64, effect: String, field: String },
}

impl Target {
    fn dialog(&self) -> Option<u64> {
        match self {
            Target::ColorPicker { id } | Target::LayerStyle { id, .. } => Some(*id),
            _ => None,
        }
    }
}

#[derive(Clone, Default)]
struct State {
    armed: Option<Target>,
    /// A pick's press is held: the rest of it (drag, release) is the eyedropper's too.
    held: bool,
}

fn state_id() -> Id {
    Id::unique("colour-eyedrop")
}

fn state(ctx: &Context) -> State {
    ctx.data(|d| d.get_temp::<State>(state_id())).unwrap_or_default()
}

fn set_state(ctx: &Context, s: State) {
    ctx.data_mut(|d| d.insert_temp(state_id(), s));
}

/// Arm the eyedropper for `target` (the same target again disarms it).
pub fn toggle(ctx: &Context, target: Target) {
    let mut s = state(ctx);
    s.armed = if s.armed.as_ref() == Some(&target) { None } else { Some(target) };
    set_state(ctx, s);
    ctx.request_repaint();
}

/// What the eyedropper is armed for.
pub fn armed(ctx: &Context) -> Option<Target> {
    state(ctx).armed
}

/// Disarm without changing anything.
pub fn cancel(ctx: &Context) {
    let mut s = state(ctx);
    s.armed = None;
    set_state(ctx, s);
}

/// Whether the eyedropper owns the pointer: armed, or a pick's press still held. Canvas
/// gestures (a Layer Style shadow drag, a tool) leave it alone meanwhile.
pub fn owns_pointer(ctx: &Context) -> bool {
    let s = state(ctx);
    s.armed.is_some() || s.held
}

/// Whether the dialog `id` steps aside for its armed eyedropper (a phone, where it covers the
/// image).
pub fn hides_dialog(ctx: &Context, id: u64) -> bool {
    ctx.content_rect().width() < crate::simple_ui::PHONE_BELOW && armed(ctx).and_then(|t| t.dialog()) == Some(id)
}

/// The pipette button for `target`, `side` points, lit while armed.
pub fn button(ui: &mut egui::Ui, target: Target, side: f32) -> egui::Response {
    let on = armed(ui.ctx()).as_ref() == Some(&target);
    let r = crate::icons::button(ui, "pipette", side, on, tl!("Pick a color from the image"));
    if r.clicked() {
        toggle(ui.ctx(), target);
    }
    r
}

/// Set `target`'s colour to `rgb` (sRGB 0..1), as an edit of that control.
pub fn apply(app: &mut PhotocraftApp, ctx: &Context, target: &Target, rgb: [f32; 3]) -> Result<(), String> {
    match target {
        Target::TypeEffect { kind, field, key } => crate::type_tool::effects::set_color(app, ctx, kind, field, key, rgb),
        Target::TypeStroke { key } => crate::type_tool::stroke::set_color(app, ctx, key, rgb),
        Target::ColorPicker { id } => crate::color_picker_ui::set_color(app, *id, rgb),
        Target::LayerStyle { id, effect, field } => crate::layer_style::color_picker::set_color(app, *id, effect, field, rgb),
    }
}

/// Pick at document point (x, y): the armed target takes the composite colour there and the
/// eyedropper disarms. Off the image (or over transparency) it stays armed. True when picked.
pub fn pick_at(app: &mut PhotocraftApp, ctx: &Context, x: f64, y: f64) -> bool {
    let Some(target) = armed(ctx) else { return false };
    let Some(rgb) = crate::canvas::composite_color(app, x, y) else { return false };
    cancel(ctx);
    if let Err(e) = apply(app, ctx, &target, rgb) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
    ctx.request_repaint();
    true
}

/// Once per UI pass, before the bars, popovers and dialogs: Esc disarms (and isn't seen by the
/// popover or dialog as their own Esc), a target whose dialog closed is forgotten, and the hint
/// bar shows while armed.
pub fn begin_frame(app: &PhotocraftApp, ctx: &Context) {
    let Some(target) = armed(ctx) else { return };
    if target.dialog().is_some_and(|id| !app.ui.dialogs.iter().any(|d| d.id == id)) {
        cancel(ctx);
        return;
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
        cancel(ctx);
        return;
    }
    let id = Id::unique("colour-eyedrop-hint");
    let t = crate::theme::Tokens::get(ctx);
    let screen = crate::safe_area::rect(ctx);
    let touch = ctx.content_rect().width() < crate::simple_ui::PHONE_BELOW;
    egui::Area::new(id).order(egui::Order::Foreground).pivot(egui::Align2::CENTER_TOP).fixed_pos(egui::pos2(screen.center().x, screen.top() + 8.0)).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).fill(t.chrome).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(if touch { tl!("Tap the image to pick a color") } else { tl!("Click the image to pick a color") });
                if ui.add_sized([72.0, if touch { 36.0 } else { 22.0 }], egui::Button::new(tl!("Cancel"))).clicked() {
                    cancel(ctx);
                }
            });
        });
    });
    // A bar beside the image, not an overlay a press on the image dismisses.
    crate::overlay_guard::non_modal(ctx, id);
}

/// The canvas's part (primary window, each frame): while armed, a primary press on the free
/// canvas `rect` picks there (`to_doc` maps it to the document). True while the eyedropper owns
/// the pointer, so the press, its drag and release reach no tool.
pub(crate) fn canvas_frame(app: &mut PhotocraftApp, ctx: &Context, rect: Rect, to_doc: impl Fn(Pos2) -> [f64; 2]) -> bool {
    let mut s = state(ctx);
    if s.held {
        if ctx.input(|i| i.pointer.any_down()) {
            return true;
        }
        s.held = false;
        set_state(ctx, s.clone());
    }
    if s.armed.is_none() {
        return false;
    }
    let dialogs = crate::dialogs::rects(ctx);
    let hint = ctx.memory(|m| m.area_rect(Id::unique("colour-eyedrop-hint")));
    let free = |p: Pos2| {
        rect.contains(p)
            && !dialogs.iter().any(|r| r.contains(p))
            && !hint.is_some_and(|r| r.contains(p))
            // Popovers and menus over the canvas (the one the eyedropper came from included).
            && ctx.layer_id_at(p).is_none_or(|l| !matches!(l.order, egui::Order::Foreground | egui::Order::Tooltip) || is_backdrop(ctx, l, p))
    };
    if let Some(p) = ctx.input(|i| i.pointer.hover_pos()).filter(|p| free(*p)) {
        crate::icons::cursor(ctx, "pipette", p, egui::vec2(2.0, 22.0) / 24.0, 20.0);
    }
    let pressed = ctx.input(|i| {
        i.events.iter().rev().find_map(|e| match e {
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, .. } => Some(*pos),
            _ => None,
        })
    });
    let Some(p) = pressed.filter(|p| free(*p)) else { return false };
    let d = to_doc(p);
    pick_at(app, ctx, d[0], d[1]);
    let mut s = state(ctx);
    s.held = true;
    set_state(ctx, s);
    true
}

/// A dialog's modal backdrop covers the whole window: a press on it outside every dialog is a
/// press on the image.
fn is_backdrop(ctx: &Context, layer: egui::LayerId, p: Pos2) -> bool {
    ctx.memory(|m| m.area_rect(layer.id)).is_some_and(|r| r.contains(p) && r.size() == ctx.content_rect().size())
}

#[cfg(test)]
#[path = "eyedrop_tests.rs"]
mod tests;
