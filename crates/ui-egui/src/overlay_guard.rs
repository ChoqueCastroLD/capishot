//! A press that dismisses an overlay is only that: it never reaches the canvas or a tool.
//!
//! Popovers, sheets, menus, the palette, phone dialogs and the long-press card close on a tap or
//! click outside them (or on their ✕). That press, its drag and its release are swallowed for
//! the canvas until the button or finger is up, for every tool, so closing a popover never paints
//! a dab, makes a selection or creates a layer. The rule is general: any interactable overlay
//! (an egui popup, or a Foreground or Tooltip area other than the always-shown bars and toasts)
//! that was showing when the press came, or a frame or two before (the touch router can carry a
//! quick tap's press into the next frame), makes the press a dismissal.

use egui::{Id, Order};

use crate::PhotocraftApp;

/// Frames after an overlay was last shown during which a press still counts as dismissing it.
const GRACE_FRAMES: u64 = 2;
/// The same in seconds: a browser follows a tap with emulated mouse events, which can arrive in
/// a later frame than the tap that closed the overlay.
const GRACE_SECONDS: f64 = 0.4;

/// Foreground areas that are always there while their mode is on (bars, the Capi button,
/// toasts, the AI progress card): a press beside them is a normal press.
const PERSISTENT: [&str; 8] =
    ["simple-capi", "touch-type-bar", "touch-transform-bar", "touch-show-ui", "photocraft-notices", "ai-progress", "ai-progress-error", "ai-progress-done"];

#[derive(Clone, Copy, Default)]
struct State {
    /// The last frame an overlay was showing, and when.
    open_frame: Option<u64>,
    open_time: f64,
    /// Where the last press while an overlay was showing landed: a browser's emulated mouse
    /// press for that tap comes at the same place.
    overlay_press: Option<egui::Pos2>,
    /// A dismissing press is held (until every button and finger is up).
    held: bool,
    /// The frame the hold last applied in (a press and release in one frame stay swallowed).
    consumed_frame: Option<u64>,
}

fn state_id() -> Id {
    Id::new("overlay-guard")
}

/// Whether a dismissable overlay is showing this frame (call after the overlays are drawn).
pub fn overlay_open(app: &PhotocraftApp, ctx: &egui::Context) -> bool {
    let narrow = ctx.content_rect().width() < crate::simple_ui::PHONE_BELOW;
    let dialogs: Vec<Id> = app.ui.dialogs.iter().map(|d| Id::new(("dialog", d.id))).collect();
    let persistent: Vec<Id> = PERSISTENT.iter().map(|s| Id::new(*s)).collect();
    if egui::Popup::is_any_open(ctx) {
        return true;
    }
    let visible = ctx.memory(|m| m.areas().visible_layer_ids());
    visible.into_iter().any(|l| {
        matches!(l.order, Order::Foreground | Order::Tooltip)
            && egui::AreaState::load(ctx, l.id).is_some_and(|a| a.interactable)
            && !persistent.contains(&l.id)
            // A desktop dialog stays open while the canvas is used (its eyedroppers);
            // on a phone a tap outside closes it.
            && (narrow || !dialogs.contains(&l.id))
    })
}

/// At the end of the frame: remember whether an overlay is showing.
pub fn end_frame(app: &PhotocraftApp, ctx: &egui::Context) {
    if overlay_open(app, ctx) {
        let (frame, now) = (ctx.cumulative_frame_nr(), ctx.input(|i| i.time));
        ctx.data_mut(|d| {
            let s = d.get_temp_mut_or_default::<State>(state_id());
            s.open_frame = Some(frame);
            s.open_time = now;
        });
    }
}

/// For the canvas: whether this frame's pointer input belongs to a press that dismissed an
/// overlay (and so must not reach the tools). `response` is the canvas's.
pub fn swallowed(response: &egui::Response) -> bool {
    let ctx = &response.ctx;
    let frame = ctx.cumulative_frame_nr();
    let now = ctx.input(|i| i.time);
    let (pressed, down, pos) = ctx.input(|i| (i.pointer.any_pressed(), i.pointer.any_down(), i.pointer.press_origin().or(i.pointer.interact_pos())));
    let on_canvas = response.contains_pointer() || ctx.input(|i| i.pointer.press_origin()).is_some_and(|p| response.rect.contains(p));
    ctx.data_mut(|d| {
        let s = d.get_temp_mut_or_default::<State>(state_id());
        let open_now = s.open_frame.is_some_and(|f| frame <= f + GRACE_FRAMES);
        let echo = pos.zip(s.overlay_press).is_some_and(|(p, q)| p.distance(q) < 30.0) && now - s.open_time < GRACE_SECONDS;
        if pressed && open_now {
            s.overlay_press = pos;
        }
        let recently_open = open_now || echo;
        if pressed && recently_open && on_canvas {
            s.held = true;
        }
        let consumed = s.held || s.consumed_frame == Some(frame);
        if consumed {
            s.held = down || pressed;
            s.consumed_frame = Some(frame);
        }
        consumed
    })
}
