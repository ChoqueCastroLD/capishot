//! Press and hold, then drag: a finger's way to drag a row of a scrolling list (the font menu's
//! folders, TypeR's styles), where a plain drag scrolls the list. The row calls [`start`] once
//! its press has been held still; the list stops scrolling while [`dragging`], and once a frame
//! (after drawing its drop targets) calls [`frame`], which paints the row's label at the finger
//! and, when the finger lifts after moving, says where it was dropped.

use egui::{Context, Id, Pos2};

#[derive(Clone, Debug)]
struct Drag {
    list: Id,
    payload: String,
    label: String,
    origin: Option<Pos2>,
    at: Option<Pos2>,
    moved: bool,
}

/// How far (points) the finger must move before the hold is a drag.
const SLOP: f32 = 10.0;

fn state_id() -> Id {
    Id::unique("touch-hold-drag")
}

fn get(ctx: &Context) -> Option<Drag> {
    ctx.data(|d| d.get_temp::<Option<Drag>>(state_id())).flatten()
}

fn put(ctx: &Context, d: Option<Drag>) {
    ctx.data_mut(|m| m.insert_temp(state_id(), d));
}

/// A row of `list` was pressed and held: the press now drags `payload` (shown as `label`).
pub fn start(ctx: &Context, list: Id, payload: &str, label: &str) {
    if get(ctx).is_some_and(|d| d.list == list && d.payload == payload) || !ctx.input(|i| i.pointer.any_down()) {
        return;
    }
    let at = ctx.input(|i| i.pointer.latest_pos());
    put(ctx, Some(Drag { list, payload: payload.into(), label: label.into(), origin: at, at, moved: false }));
}

/// A held row of `list` is being dragged (the list must not scroll meanwhile).
pub fn dragging(ctx: &Context, list: Id) -> bool {
    get(ctx).is_some_and(|d| d.list == list)
}

/// The payload of `list`'s drag once it moved, and where the finger is.
pub fn moving(ctx: &Context, list: Id) -> Option<(String, Pos2)> {
    get(ctx).filter(|d| d.list == list && d.moved).and_then(|d| Some((d.payload, d.at?)))
}

/// Once a frame for `list`: follows the finger and paints the dragged label; when the finger
/// lifts after moving, the payload and the drop point (once).
pub fn frame(ctx: &Context, list: Id) -> Option<(String, Pos2)> {
    let mut d = get(ctx).filter(|d| d.list == list)?;
    let (down, pos) = ctx.input(|i| (i.pointer.any_down(), i.pointer.latest_pos()));
    if !down {
        put(ctx, None);
        return d.moved.then_some(d.payload).zip(d.at);
    }
    if let Some(p) = pos {
        d.at = Some(p);
        if d.origin.is_some_and(|o| o.distance(p) > SLOP) {
            d.moved = true;
        }
    }
    if d.moved
        && let Some(p) = d.at
    {
        let t = crate::theme::Tokens::get(ctx);
        egui::Area::new(Id::unique("touch-hold-drag-ghost")).order(egui::Order::Tooltip).interactable(false).fixed_pos(p + egui::vec2(-20.0, -56.0)).show(
            ctx,
            |ui| {
                egui::Frame::popup(ui.style()).fill(t.accent_soft).show(ui, |ui| {
                    ui.label(egui::RichText::new(&d.label).strong());
                });
            },
        );
    }
    ctx.request_repaint();
    put(ctx, Some(d));
    None
}

/// Drop any drag (the list closed).
pub fn cancel(ctx: &Context) {
    put(ctx, None);
}

/// How long a press rests before it is a press and hold (seconds), and how far it may wander.
const HOLD_S: f64 = 0.55;

/// Whether the press on `resp` has been held still long enough to start a drag (a finger's
/// press and hold). Keeps frames coming while it may become one.
pub fn held_still(resp: &egui::Response) -> bool {
    if !resp.is_pointer_button_down_on() {
        return false;
    }
    let held = resp.ctx.input(|i| {
        let still = match (i.pointer.press_origin(), i.pointer.latest_pos()) {
            (Some(a), Some(b)) => a.distance(b) < SLOP,
            _ => false,
        };
        still && i.pointer.press_start_time().is_some_and(|t| i.time - t >= HOLD_S)
    });
    if !held {
        resp.ctx.request_repaint_after(std::time::Duration::from_millis(80));
    }
    held
}
