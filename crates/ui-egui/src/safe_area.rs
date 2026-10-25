//! The screen's safe area on phones: the notch and the camera at the top, the home indicator at
//! the bottom, rounded corners. The web shell reads the browser's `env(safe-area-inset-*)` (with
//! `viewport-fit=cover`) and sets them here; the simple interface keeps its bars inside.

use egui::{Context, Id, Rect, pos2};

/// Insets in points: top, right, bottom, left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

fn id() -> Id {
    Id::new("safe-area-insets")
}

pub fn set(ctx: &Context, insets: Insets) {
    let clean = |v: f32| if v.is_finite() { v.clamp(0.0, 200.0) } else { 0.0 };
    let i = Insets { top: clean(insets.top), right: clean(insets.right), bottom: clean(insets.bottom), left: clean(insets.left) };
    if get(ctx) != i {
        ctx.data_mut(|d| d.insert_persisted(id(), [i.top, i.right, i.bottom, i.left]));
        ctx.request_repaint();
    }
}

pub fn get(ctx: &Context) -> Insets {
    let [top, right, bottom, left] = ctx.data_mut(|d| d.get_persisted::<[f32; 4]>(id())).unwrap_or_default();
    Insets { top, right, bottom, left }
}

/// The window's content rect inside the safe area.
pub fn rect(ctx: &Context) -> Rect {
    let r = ctx.content_rect();
    let i = get(ctx);
    Rect::from_min_max(pos2(r.left() + i.left, r.top() + i.top), pos2((r.right() - i.right).max(r.left() + i.left), (r.bottom() - i.bottom).max(r.top() + i.top)))
}
