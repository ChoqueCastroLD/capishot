//! What Shift, Alt and Ctrl do for each tool, as touch controls (the simple interface's tool
//! row). A phone has no keys and the floating modifier button is opt-in, so each tool that reads
//! a modifier gets a chip for it: Square / Circle and From Center for the marquees, Duplicate and
//! Axis for Move, Proportional and From Center for Free Transform, Straight Line for the brushes,
//! Set Source for the Clone Stamp and Healing Brush, Fixed Ratio and From Center for the shapes,
//! 45° Angle for the Gradient. A chip that is on acts as its key held (`apply`, through
//! `workspace_ui::sticky_mods`), only once a drag has begun where the key at the press would mean
//! something else (a marquee's Shift at the press adds to the selection). Duplicate and Set
//! Source last one gesture. The Crop tool's ratio presets show in the row too (not a key).

use egui::{CornerRadius, Rect, Sense, Stroke, Vec2};

use crate::PhotocraftApp;
use crate::state::Tool;
use crate::theme::Tokens;

/// The touch controls that stand in for held keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TouchLocks {
    /// Marquee square / circle, shape fixed ratio (Shift once dragging).
    pub constrain: bool,
    /// From the centre: marquees, shapes, Free Transform (Alt).
    pub from_center: bool,
    /// Move: the next drag moves a copy (Alt at the press). One gesture.
    pub duplicate: bool,
    /// Move: the drag keeps to a horizontal, vertical or 45° line (Shift once dragging).
    pub axis: bool,
    /// Brush, Pencil, Eraser: a tap joins the last point with a straight line, a drag keeps to 0,
    /// 45 or 90° (Shift).
    pub straight: bool,
    /// Clone Stamp, Healing Brush: the next tap picks the source (Alt). One gesture.
    pub set_source: bool,
    /// Gradient: the angle snaps to 45° steps (Shift once dragging).
    pub angle45: bool,
}

/// A chip: what it toggles, its icon and its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lock {
    Constrain,
    FromCenter,
    Duplicate,
    Axis,
    Proportional,
    Straight,
    SetSource,
    Angle45,
}

impl Lock {
    /// The chip's name for `tool` (the marquee's Shift makes a square or a circle).
    pub fn name(self, tool: Tool) -> &'static str {
        match self {
            Lock::Constrain if tool == Tool::EllipseMarquee => tl!("Circle"),
            Lock::Constrain if tool == Tool::RectMarquee => tl!("Square"),
            Lock::Constrain => tl!("Fixed ratio"),
            Lock::FromCenter => tl!("From center"),
            Lock::Duplicate => tl!("Duplicate"),
            Lock::Axis => tl!("Axis"),
            Lock::Proportional => tl!("Proportional"),
            Lock::Straight => tl!("Straight line"),
            Lock::SetSource => tl!("Set source"),
            Lock::Angle45 => tl!("45° angle"),
        }
    }

    fn icon(self, tool: Tool) -> &'static str {
        match self {
            Lock::Constrain if tool == Tool::EllipseMarquee || tool == Tool::EllipseShape => "circle",
            Lock::Constrain => "square",
            Lock::FromCenter => "focus",
            Lock::Duplicate => "copy",
            Lock::Axis => "arrow-left-right",
            Lock::Proportional => "link",
            Lock::Straight => "slash",
            Lock::SetSource => "scan",
            Lock::Angle45 => "rotate-cw",
        }
    }

    /// The id `ui.inspect` and the tests know the chip by.
    pub fn key(self) -> &'static str {
        match self {
            Lock::Constrain => "constrain",
            Lock::FromCenter => "fromCenter",
            Lock::Duplicate => "duplicate",
            Lock::Axis => "axis",
            Lock::Proportional => "proportional",
            Lock::Straight => "straight",
            Lock::SetSource => "setSource",
            Lock::Angle45 => "angle45",
        }
    }
}

/// The painting tools with a Straight Line chip.
fn paints(tool: Tool) -> bool {
    matches!(tool, Tool::Brush | Tool::Pencil | Tool::Eraser | Tool::MixerBrush)
}

/// The chips the current tool (or an open Free Transform) has, in order.
pub fn chips(app: &PhotocraftApp) -> Vec<Lock> {
    if app.ui.transform.is_some() {
        return vec![Lock::Proportional, Lock::FromCenter];
    }
    let tool = app.ui.tool;
    match tool {
        Tool::RectMarquee | Tool::EllipseMarquee => vec![Lock::Constrain, Lock::FromCenter],
        Tool::Move => vec![Lock::Duplicate, Lock::Axis],
        t if paints(t) => vec![Lock::Straight],
        Tool::CloneStamp | Tool::Healing => vec![Lock::SetSource],
        t if crate::vector_ui::is_shape_tool(t) => vec![Lock::Constrain, Lock::FromCenter],
        Tool::Gradient => vec![Lock::Angle45],
        _ => Vec::new(),
    }
}

/// Whether `lock` is on.
pub fn is_on(app: &PhotocraftApp, lock: Lock) -> bool {
    let l = &app.touch.locks;
    match lock {
        Lock::Constrain => l.constrain,
        Lock::FromCenter => l.from_center,
        Lock::Duplicate => l.duplicate,
        Lock::Axis => l.axis,
        Lock::Proportional => !app.ui.transform_unlinked,
        Lock::Straight => l.straight,
        Lock::SetSource => l.set_source,
        Lock::Angle45 => l.angle45,
    }
}

/// Turn `lock` on or off.
pub fn set(app: &mut PhotocraftApp, lock: Lock, on: bool) {
    let l = &mut app.touch.locks;
    match lock {
        Lock::Constrain => l.constrain = on,
        Lock::FromCenter => l.from_center = on,
        Lock::Duplicate => l.duplicate = on,
        Lock::Axis => l.axis = on,
        Lock::Proportional => app.ui.transform_unlinked = !on,
        Lock::Straight => l.straight = on,
        Lock::SetSource => l.set_source = on,
        Lock::Angle45 => l.angle45 = on,
    }
}

/// The chips that are on, added to the keys held (`workspace_ui::sticky_mods`). Only with the
/// touch interface, and for the current tool.
pub fn apply(app: &PhotocraftApp, mut m: egui::Modifiers) -> egui::Modifiers {
    if !crate::touch_ui::active(app) {
        return m;
    }
    let l = app.touch.locks;
    let dragging = app.drag.is_some();
    if app.ui.transform.is_some() {
        m.alt |= l.from_center;
        return m;
    }
    match app.ui.tool {
        Tool::RectMarquee | Tool::EllipseMarquee if dragging => {
            m.shift |= l.constrain;
            m.alt |= l.from_center;
        }
        Tool::Move => {
            m.alt |= l.duplicate;
            m.shift |= l.axis && dragging;
        }
        t if paints(t) => m.shift |= l.straight,
        Tool::CloneStamp | Tool::Healing => m.alt |= l.set_source,
        t if crate::vector_ui::is_shape_tool(t) && dragging => {
            m.shift |= l.constrain;
            m.alt |= l.from_center;
        }
        Tool::Gradient if dragging => m.shift |= l.angle45,
        _ => {}
    }
    m
}

/// After a finger lifted from the canvas: the one-gesture chips (Duplicate, Set Source) go off.
pub fn gesture_ended(app: &mut PhotocraftApp) {
    app.touch.locks.duplicate = false;
    app.touch.locks.set_source = false;
}

/// Width of a chip in the tool row.
pub const CHIP: f32 = 34.0;

/// The current tool's chips in the phone's tool row: with their names when they fit in what
/// the row has left after `reserve` points (the brush and the options button), else icons. A tap
/// toggles one and says so in the status toast.
pub fn row(app: &mut PhotocraftApp, ui: &mut egui::Ui, reserve: f32) {
    let tool = app.ui.tool;
    let locks = chips(app);
    let font = crate::theme::medium(12.5);
    let gap = ui.spacing().item_spacing.x;
    let widths: Vec<f32> =
        locks.iter().map(|l| CHIP + 2.0 + ui.painter().layout_no_wrap(l.name(tool).to_string(), font.clone(), egui::Color32::WHITE).size().x + 10.0).collect();
    let named = widths.iter().map(|w| w + gap).sum::<f32>() <= ui.available_width() - reserve;
    for (lock, w) in locks.into_iter().zip(widths) {
        let t = Tokens::get(ui.ctx());
        let on = is_on(app, lock);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(if named { w } else { CHIP }, CHIP), Sense::click());
        let bg = if on {
            t.accent
        } else if resp.is_pointer_button_down_on() {
            t.hover
        } else {
            t.chrome
        };
        ui.painter().rect(rect.shrink(2.0), CornerRadius::same(10), bg, Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
        let fg = if on { egui::Color32::WHITE } else { t.text };
        let icon = Rect::from_min_size(rect.min, Vec2::splat(CHIP));
        crate::icons::paint(ui, icon, lock.icon(tool), 17.0, fg);
        let name = lock.name(tool);
        if named {
            ui.painter().text(egui::pos2(icon.right() - 2.0, rect.center().y), egui::Align2::LEFT_CENTER, name, font.clone(), fg);
        }
        let resp = resp.on_hover_text(name);
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, name));
        crate::touch_ui::note_rect(ui.ctx(), &format!("lock:{}", lock.key()), rect);
        if resp.clicked() {
            set(app, lock, !on);
            let state = if on { tl!("off") } else { tl!("on") };
            app.ui.status = format!("{name}: {state}");
            app.ui.status_error = false;
        }
    }
}
