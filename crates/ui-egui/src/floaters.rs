//! Floating panels that dock (Brush Settings, Gradient Editor, Type Styles, Glyphs, AI Models,
//! Styles, Shapes, Tool Presets, Clone Source, Measurement Log, Notes, Modifier Keys).
//!
//! Each opens as its own window, as before. Its header's dock button, or dropping the window on
//! the right dock, docks it: from then on it opens in the dock as a group of its own
//! ([`crate::dock::Group::FLOATERS`]), which collapses, resizes, moves and closes like the dock's
//! other groups, until its panel menu's Float Panel (or the header button) floats it again. Where
//! each one opens is saved with the workspace (`Panels::dock_home`).
//!
//! The dock reserves the docked panel's group area while it is drawn ([`reserve`]); the panel's
//! own code then draws its content there instead of in its window ([`Host`]), so a panel is the
//! same code in both places. Dialogs (Check Spelling, TypeR's editors, AI Options…) stay modal or
//! floating, and a customized docking layout keeps these panels floating.

use egui::{Context, Id, Rect, Sense, Ui};

use crate::PhotocraftApp;
use crate::dock::Group;

/// A floating panel that can dock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Floater(Group);

impl Floater {
    /// The floating panel of dock group `g`.
    pub fn of(g: Group) -> Option<Floater> {
        g.is_floater().then_some(Floater(g))
    }

    pub fn group(self) -> Group {
        self.0
    }

    /// Every floating panel.
    pub fn all() -> impl Iterator<Item = Floater> {
        Group::FLOATERS.into_iter().map(Floater)
    }

    /// Whether the panel is open (wherever it is).
    pub fn is_open(self, app: &PhotocraftApp) -> bool {
        match self.0 {
            Group::BrushSettings => app.ui.panels.brush_settings,
            Group::GradientEditor => app.ui.panels.gradient_editor,
            Group::TypeStyles => app.ui.type_panels.styles,
            Group::Glyphs => app.ui.type_panels.glyphs,
            Group::AiModels => app.ui.ai_models.open,
            Group::Styles => app.ui.presets_ui.styles,
            Group::Shapes => app.ui.presets_ui.shapes,
            Group::ToolPresets => app.ui.presets_ui.tool_presets,
            Group::CloneSource => app.ui.presets_ui.clone_source,
            Group::MeasurementLog => app.ui.analysis.measurement_log,
            Group::Notes => app.ui.analysis.notes,
            Group::ModifierKeys => app.ui.shell.modifier_keys,
            _ => false,
        }
    }

    /// Open or close the panel.
    pub fn set_open(self, app: &mut PhotocraftApp, on: bool) {
        let flag = match self.0 {
            Group::BrushSettings => &mut app.ui.panels.brush_settings,
            Group::GradientEditor => &mut app.ui.panels.gradient_editor,
            Group::TypeStyles => &mut app.ui.type_panels.styles,
            Group::Glyphs => &mut app.ui.type_panels.glyphs,
            Group::AiModels => &mut app.ui.ai_models.open,
            Group::Styles => &mut app.ui.presets_ui.styles,
            Group::Shapes => &mut app.ui.presets_ui.shapes,
            Group::ToolPresets => &mut app.ui.presets_ui.tool_presets,
            Group::CloneSource => &mut app.ui.presets_ui.clone_source,
            Group::MeasurementLog => &mut app.ui.analysis.measurement_log,
            Group::Notes => &mut app.ui.analysis.notes,
            Group::ModifierKeys => &mut app.ui.shell.modifier_keys,
            _ => return,
        };
        *flag = on;
    }
}

/// Whether panel `f` opens in the dock (docked, and the dock is the classic column: a customized
/// docking layout keeps these panels floating).
pub fn docked(app: &PhotocraftApp, f: Floater) -> bool {
    app.ui.docking.is_none() && app.ui.panels.dock_home.iter().any(|k| k == f.group().key())
}

/// Dock panel `f`: it opens in the dock from now on, expanded.
pub fn dock(app: &mut PhotocraftApp, f: Floater) {
    let key = f.group().key().to_string();
    if !app.ui.panels.dock_home.contains(&key) {
        app.ui.panels.dock_home.push(key);
    }
    f.set_open(app, true);
    app.ui.dock.set_collapsed(f.group(), false);
    sync_docked(app);
}

/// Float docked panel `f` again: it opens in its window from now on.
pub fn float(app: &mut PhotocraftApp, f: Floater) {
    app.ui.panels.dock_home.retain(|k| k != f.group().key());
    f.set_open(app, true);
    sync_docked(app);
}

/// `Panels::docked_open`: the docked panels that are open.
pub fn sync_docked(app: &mut PhotocraftApp) {
    let open: Vec<String> = Floater::all().filter(|f| f.is_open(app) && docked(app, *f)).map(|f| f.group().key().to_string()).collect();
    if app.ui.panels.docked_open != open {
        app.ui.panels.docked_open = open;
    }
}

/// Once per frame before the dock is drawn: what the panels' headers and window drags asked for
/// last frame ([`Host::header_button`], a window dropped on the dock), then [`sync_docked`].
pub fn frame(app: &mut PhotocraftApp, ctx: &Context) {
    let requests = ctx.data_mut(|d| d.remove_temp::<Vec<(Group, Request)>>(requests_id())).unwrap_or_default();
    for (g, req) in requests {
        let Some(f) = Floater::of(g) else { continue };
        if app.session.prefs().workspace_locked {
            break;
        }
        match req {
            Request::Dock => dock(app, f),
            Request::Float => float(app, f),
            // Its window dropped on a column or another group's strip.
            Request::Drop(p) => match crate::dock::drop_target(app, ctx, g, p) {
                crate::dock::DropAt::Float(_) => {}
                at => crate::dock::land(app, ctx, g, at, egui::Vec2::ZERO),
            },
        }
    }
    sync_docked(app);
}

fn requests_id() -> Id {
    Id::new("floaters-requests")
}

#[derive(Clone, Copy, Debug)]
enum Request {
    Dock,
    Float,
    /// The panel's window was dropped there.
    Drop(egui::Pos2),
}

fn request(ctx: &Context, g: Group, req: Request) {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<(Group, Request)>>(requests_id()).push((g, req)));
}

fn reserved_id(g: Group) -> Id {
    Id::new(("floater-docked-area", g.key()))
}

/// In the dock: take the group's content area for docked panel `g`, which draws itself there
/// later this frame ([`Host`]).
pub fn reserve(ui: &mut Ui, g: Group) {
    let r = ui.available_rect_before_wrap();
    ui.allocate_rect(r, Sense::hover());
    let frame = ui.ctx().cumulative_frame_nr();
    // The visible part only: a group partly scrolled or clipped doesn't spill over its neighbours.
    let r = r.intersect(ui.clip_rect());
    // Above the window or flyout holding the group, when it isn't the dock itself.
    let above = match ui.layer_id().order {
        egui::Order::Background => None,
        o => Some(o),
    };
    ui.ctx().data_mut(|d| d.insert_temp(reserved_id(g), (frame, r, above)));
}

/// Where a floating panel draws this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Host {
    /// In its own window.
    Window(Group),
    /// Over its dock group's content area (and above the window or flyout holding it).
    Docked(Group, Rect, Option<egui::Order>),
    /// Docked, but its group isn't drawn this frame (collapsed, or the dock is hidden).
    Hidden,
}

impl Host {
    /// Where panel `g` draws this frame.
    pub fn of(app: &PhotocraftApp, ctx: &Context, g: Group) -> Host {
        let Some(f) = Floater::of(g) else { return Host::Window(g) };
        // A phone's closed dock (`panels::narrow_dock_open`): the panel floats rather than vanish.
        if !docked(app, f) || crate::panels::narrow_dock_open(ctx) == Some(false) {
            return Host::Window(g);
        }
        let frame = ctx.cumulative_frame_nr();
        // While a canvas gesture holds the chrome still (`chrome_freeze`), the dock isn't laid
        // out: its groups stay where they were.
        let fresh = |at: u64| at == frame || (app.chrome_freeze.is_frozen() && frame.saturating_sub(at) <= u64::from(crate::chrome_freeze::REFRESH_FRAMES) + 1);
        match ctx.data(|d| d.get_temp::<(u64, Rect, Option<egui::Order>)>(reserved_id(g))) {
            Some((at, r, above)) if fresh(at) && r.is_positive() => Host::Docked(g, r, above),
            _ => Host::Hidden,
        }
    }

    /// The width a panel laid out at `w` points gets: `w` in its window, at most its dock
    /// group's width when docked.
    pub fn width(&self, w: f32) -> f32 {
        match *self {
            Host::Docked(_, r, _) => w.min((r.width() - 14.0).max(80.0)),
            _ => w,
        }
    }

    pub fn is_docked(&self) -> bool {
        !matches!(self, Host::Window(_))
    }

    /// Draw the panel: `window` floating, or `add` over its dock area. `id` names the panel's
    /// window (the docked area takes an id derived from it).
    pub fn show<R>(&self, ctx: &Context, id: Id, window: egui::Window<'_>, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
        match *self {
            Host::Window(g) => {
                let shown = window.show(ctx, add)?;
                // Dragged: the dock's drop zones show; dropped on one, it docks there.
                if shown.response.dragged() {
                    crate::dock::note_dragging(ctx, g);
                }
                if shown.response.drag_stopped()
                    && let Some(p) = ctx.pointer_interact_pos().or_else(|| ctx.pointer_latest_pos())
                {
                    request(ctx, g, Request::Drop(p));
                }
                shown.inner
            }
            Host::Docked(g, r, above) => Some(
                egui::Area::new(id.with("docked"))
                    .fixed_pos(r.min)
                    .order(above.unwrap_or(egui::Order::Middle))
                    .constrain(false)
                    .show(ctx, |ui| {
                        ui.set_clip_rect(r);
                        ui.set_min_size(r.size());
                        ui.set_max_size(r.size());
                        egui::ScrollArea::vertical()
                            .id_salt(("floater-docked-scroll", g.key()))
                            .max_height(r.height())
                            .auto_shrink([false, false])
                            .show(ui, add)
                            .inner
                    })
                    .inner,
            )
            .inspect(|_| {
                if let Some(order) = above {
                    ctx.move_to_top(egui::LayerId::new(order, id.with("docked")));
                }
            }),
            Host::Hidden => None,
        }
    }

    /// The panel header's dock / float button (none in a customized docking layout, or for a
    /// window the panel isn't).
    pub fn header_button(&self, ui: &mut Ui) {
        let (g, docked) = match *self {
            Host::Window(g) => (g, false),
            Host::Docked(g, ..) => (g, true),
            Host::Hidden => return,
        };
        if !g.is_floater() {
            return;
        }
        let (icon, tip) = if docked { ("app-window", tl!("Float Panel")) } else { ("panel-right", tl!("Dock to the Panels")) };
        if crate::icons::button(ui, icon, 20.0, false, tip).clicked() {
            request(ui.ctx(), g, if docked { Request::Float } else { Request::Dock });
        }
    }
}

fn current_id() -> Id {
    Id::new("floater-current-host")
}

/// Run `body` with `host` as the panel being drawn, for headers drawn by shared helpers
/// (`analysis_ui::title_row`) to add [`Host::header_button`].
pub fn with_current<R>(ctx: &Context, host: Host, body: impl FnOnce() -> R) -> R {
    ctx.data_mut(|d| d.insert_temp(current_id(), host));
    let r = body();
    ctx.data_mut(|d| d.remove::<Host>(current_id()));
    r
}

/// The panel [`with_current`] is drawing, if any.
pub fn current(ctx: &Context) -> Option<Host> {
    ctx.data(|d| d.get_temp::<Host>(current_id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_floater_opens_closes_and_docks() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        for f in Floater::all() {
            assert!(!f.is_open(&app), "{:?} starts closed", f.group());
            f.set_open(&mut app, true);
            assert!(f.is_open(&app));
            assert!(!f.group().shown(&app.ui.panels), "floating, not in the dock");
            dock(&mut app, f);
            assert!(docked(&app, f) && f.group().shown(&app.ui.panels), "{:?} docked", f.group());
            crate::dock::set_shown(&mut app, f.group(), false);
            assert!(!f.is_open(&app) && !f.group().shown(&app.ui.panels), "closing the group closes the panel");
            f.set_open(&mut app, true);
            sync_docked(&mut app);
            assert!(f.group().shown(&app.ui.panels), "reopened where it was: docked");
            float(&mut app, f);
            assert!(f.is_open(&app) && !docked(&app, f) && !f.group().shown(&app.ui.panels), "floated");
        }
        // Group keys round-trip (workspaces store them).
        for g in Group::EVERY {
            assert_eq!(Group::from_key(g.key()), Some(g));
            assert_eq!(serde_json::to_value(g).unwrap(), serde_json::json!(g.key()));
        }
    }

    #[test]
    fn a_docked_panel_draws_only_where_its_group_was_reserved_this_frame() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let ctx = Context::default();
        let f = Floater::of(Group::Notes).unwrap();
        dock(&mut app, f);
        let mut hosts = Vec::new();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            hosts.push(Host::of(&app, ui.ctx(), Group::Notes));
            reserve(ui, Group::Notes);
            hosts.push(Host::of(&app, ui.ctx(), Group::Notes));
            hosts.push(Host::of(&app, ui.ctx(), Group::Styles));
        });
        out.textures_delta.clear();
        assert_eq!(hosts[0], Host::Hidden, "not reserved yet");
        assert!(matches!(hosts[1], Host::Docked(Group::Notes, r, _) if r.is_positive()));
        assert_eq!(hosts[2], Host::Window(Group::Styles));
        // Next frame without the dock drawing it (collapsed, hidden): nowhere.
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| hosts.push(Host::of(&app, ui.ctx(), Group::Notes)));
        out.textures_delta.clear();
        assert_eq!(hosts[3], Host::Hidden);
    }
}
