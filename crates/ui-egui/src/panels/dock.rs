use super::history::history;
use super::info::info_panel;
use super::layers::{channels, layers};
use super::navigator::navigator;
use super::properties::{adjustments_grid, properties_body};
use super::{PhotocraftApp, Rect, Sense, Stroke, Tokens, icons, pos2, vec2};

// ----------------------------------------------------------------------------- dock

/// Below this screen width (points: phones, small tablets in portrait) the dock doesn't keep
/// its column: at 250 points or more it left a phone's canvas a sliver. It opens from the icon
/// rail (tap a panel's icon) and closes with the same icon.
pub(crate) const NARROW_SCREEN: f32 = 720.0;

fn narrow_dock_id() -> egui::Id {
    egui::Id::unique("narrow-screen-dock-open")
}

/// On a narrow screen ([`NARROW_SCREEN`]): whether the dock is open. `None` on a wide screen.
pub(crate) fn narrow_dock_open(ctx: &egui::Context) -> Option<bool> {
    (ctx.content_rect().width() < NARROW_SCREEN).then(|| ctx.data(|d| d.get_temp::<bool>(narrow_dock_id())).unwrap_or(false))
}

pub fn right_dock(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    // A custom docking workspace: craft-ui draws it whole, the canvas as its protected panel.
    if app.ui.docking.is_some() {
        let t = Tokens::get(ui.ctx());
        if app.ui.panels.rail && !t.pro {
            rail(app, ui, &t);
        }
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| crate::panel_docking::show(app, ui));
        crate::dock::persist(app, ui.ctx());
        return;
    }
    // Even legacy destinations must discard gestures captured by an earlier workspace.
    let customized = crate::panel_docking::show(app, ui);
    // Floating panels docked or floated by their headers or a window dropped on the dock.
    crate::floaters::frame(app, ui.ctx());
    let t = Tokens::get(ui.ctx());
    let p = app.ui.panels.clone();
    let narrow = if customized { None } else { narrow_dock_open(ui.ctx()) };
    let dock_shown = narrow != Some(false);
    if !dock_shown {
        crate::dock::forget_rects(ui.ctx());
    }
    if t.pro && !customized && dock_shown {
        dock_column(app, ui, crate::dock::Side::Right, narrow.is_some(), &t);
    }
    // Narrow icon rail, unless an embedding app hides it: shows, expands or collapses panel groups.
    // A right column collapsed to icons is that strip itself.
    let iconic = !customized && narrow.is_none() && app.ui.dock.iconic.contains(&crate::dock::Side::Right);
    if p.rail && !iconic {
        let (rw, rb) = if t.pro { (36.0, 28.0) } else { (44.0, 32.0) };
        crate::direction::end_panel("rail")
            .resizable(false)
            .exact_size(rw)
            .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(4, 8)))
            .show(ui, |ui| {
                // The separator is on the canvas side.
                let r = ui.max_rect();
                let x = if crate::direction::is_rtl() { r.right() + 6.0 } else { r.left() - 6.0 };
                ui.painter().line_segment([pos2(x, r.top() - 8.0), pos2(x, r.bottom() + 8.0)], Stroke::new(1.0, t.separator));
                ui.spacing_mut().item_spacing.y = 4.0;
                use crate::dock::Group;
                let entries: [(&str, &str, Group); 6] = [
                    ("sliders-horizontal", tl!("Properties"), Group::Properties),
                    ("navigation", tl!("Navigator"), Group::Navigator),
                    ("palette", tl!("Color & Swatches"), Group::Color),
                    ("layers", tl!("Layers"), Group::Layers),
                    ("clock", tl!("History"), Group::History),
                    ("brush", tl!("Brushes"), Group::Brushes),
                ];
                for (icon, name, g) in entries {
                    // Studio floats Properties outside the dock.
                    let docked = t.pro || g != Group::Properties;
                    let on =
                        if customized { crate::panel_docking::rail_on(app, g) } else { dock_shown && g.shown(&p) && !(docked && app.ui.dock.is_collapsed(g)) };
                    if icons::rail_button(ui, icon, rb, on, name).clicked() {
                        if customized {
                            crate::panel_docking::rail_click(app, g);
                        } else if narrow.is_some() {
                            // Narrow screen: the icon opens the dock on its panel, and closes it
                            // when that panel is what it shows.
                            let open = !on;
                            if open {
                                crate::dock::reveal(app, g);
                            }
                            ui.ctx().data_mut(|d| d.insert_temp(narrow_dock_id(), open));
                        } else {
                            crate::dock::rail_click(app, g, docked);
                        }
                    }
                }
            });
    }
    if !t.pro && !customized && dock_shown {
        dock_column(app, ui, crate::dock::Side::Right, narrow.is_some(), &t);
    }
    crate::dock::persist(app, ui.ctx());
}

/// The dock columns and the canvas in the space left by the menus, bars and toolbar (as the
/// app's frame lays them out): the right dock, the column beside the toolbar, then the canvas.
pub fn dock_area(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let custom = app.ui.docking.is_some();
    right_dock(app, ui);
    if custom {
        // The custom workspace drew the canvas as its own panel.
        return;
    }
    left_dock(app, ui);
    let t = Tokens::get(ui.ctx());
    let backdrop = if app.ui.view.hides_chrome() { egui::Color32::BLACK } else { crate::prefs_ui::pasteboard_color(app).unwrap_or(t.canvas) };
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(backdrop)).show(ui, |ui| crate::canvas::document_area(app, ui));
}

/// The docking layout's protected canvas panel: the document area, with the icon rail on its end
/// side, beside the dock, in the Pro themes.
pub(crate) fn canvas_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let backdrop = if app.ui.view.hides_chrome() { egui::Color32::BLACK } else { crate::prefs_ui::pasteboard_color(app).unwrap_or(t.canvas) };
    if app.ui.panels.rail && t.pro {
        rail(app, ui, &t);
    }
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(backdrop)).show(ui, |ui| crate::canvas::document_area(app, ui));
}

/// Narrow icon rail, unless an embedding app hides it: shows, expands or collapses panel groups.
fn rail(app: &mut PhotocraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let customized = app.ui.docking.is_some();
    let p = app.ui.panels.clone();
    let (rw, rb) = if t.pro { (36.0, 28.0) } else { (44.0, 32.0) };
    crate::direction::end_panel("rail")
        .resizable(false)
        .exact_size(rw)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(4, 8)))
        .show(ui, |ui| {
            // The separator is on the canvas side.
            let r = ui.max_rect();
            let x = if crate::direction::is_rtl() { r.right() + 6.0 } else { r.left() - 6.0 };
            ui.painter().line_segment([pos2(x, r.top() - 8.0), pos2(x, r.bottom() + 8.0)], Stroke::new(1.0, t.separator));
            ui.spacing_mut().item_spacing.y = 4.0;
            use crate::dock::Group;
            let entries: [(&str, &str, Group); 6] = [
                ("sliders-horizontal", tl!("Properties"), Group::Properties),
                ("navigation", tl!("Navigator"), Group::Navigator),
                ("palette", tl!("Color & Swatches"), Group::Color),
                ("layers", tl!("Layers"), Group::Layers),
                ("clock", tl!("History"), Group::History),
                ("brush", tl!("Brushes"), Group::Brushes),
            ];
            for (icon, name, g) in entries {
                // Studio floats Properties outside the dock.
                let docked = t.pro || g != Group::Properties;
                let on = if customized { crate::panel_docking::rail_on(app, g) } else { g.shown(&p) && !(docked && app.ui.dock.is_collapsed(g)) };
                if icons::rail_button(ui, icon, rb, on, name).clicked() {
                    if customized {
                        crate::panel_docking::rail_click(app, g);
                    } else {
                        crate::dock::rail_click(app, g, docked);
                    }
                }
            }
        });
}

/// The dock's narrowest width (points). It is the right dock, on the left in a right-to-left UI.
pub(crate) const DOCK_MIN_WIDTH: f32 = 250.0;

/// What the dock leaves the toolbar and the canvas when it is widest (points).
const DOCK_LEAVES: f32 = 400.0;

/// The dock's width range (points) in a window `window_width` wide: from [`DOCK_MIN_WIDTH`] to
/// everything but [`DOCK_LEAVES`], and never capped below the old 520, so two-column layouts can
/// be as wide as the window allows (#7896).
pub(crate) fn dock_width_range(window_width: f32) -> std::ops::RangeInclusive<f32> {
    let max = if window_width.is_finite() { (window_width - DOCK_LEAVES).max(520.0) } else { 520.0 };
    DOCK_MIN_WIDTH..=max
}

pub(crate) fn dock_width_id() -> egui::Id {
    egui::Id::unique("dock-width-request")
}

/// Set the right dock's width on the next frame (`ui.set {dockWidth}`); clamped to its range.
pub fn request_dock_width(ctx: &egui::Context, w: f32) {
    let range = dock_width_range(ctx.content_rect().width());
    let w = if w.is_finite() { w.clamp(*range.start(), *range.end()) } else { *range.start() };
    ctx.data_mut(|d| d.insert_temp(dock_width_id(), w));
}

/// The dock groups shown in column `side` (all docked groups on a narrow screen, which has one).
fn column_groups(app: &PhotocraftApp, p: &crate::state::Panels, t: &Tokens, side: Option<crate::dock::Side>) -> Vec<crate::dock::Group> {
    use crate::dock::Group;
    // Floating in Studio, Properties docks only in Pro (Photoshop).
    [
        (Group::Color, p.color),
        (Group::Properties, t.pro && p.properties),
        (Group::Character, p.character),
        (Group::TypeR, p.typer),
        (Group::Navigator, p.navigator),
        (Group::History, p.history),
        (Group::Layers, p.layers),
        (Group::Brushes, p.brushes),
    ]
    .into_iter()
    // Docked floating panels (`crate::floaters`).
    .chain(Group::FLOATERS.into_iter().map(|g| (g, g.shown(p))))
    .filter_map(|(g, on)| on.then_some(g))
    .filter(|g| {
        let l = &app.ui.dock;
        let host = l.host(*g);
        // A guest goes where its host is; a guest of a hidden host stands alone.
        let at = if host != *g && host.shown(p) { host } else { *g };
        !l.floating.contains_key(&at) && side.is_none_or(|s| l.side(at) == s)
    })
    .collect()
}

/// The icon of a dock group (columns collapsed to icons, Photoshop's icon strips).
pub(crate) fn group_icon(g: crate::dock::Group) -> &'static str {
    use crate::dock::Group;
    match g {
        Group::Color => "palette",
        Group::Properties => "sliders-horizontal",
        Group::Character => "type",
        Group::TypeR => "message-square",
        Group::Navigator => "navigation",
        Group::History => "clock",
        Group::Layers => "layers",
        Group::BrushSettings => "brush",
        Group::GradientEditor => "blend",
        Group::TypeStyles => "align-left",
        Group::Glyphs => "text-cursor",
        Group::AiModels => "sparkles",
        Group::Styles => "star",
        Group::Shapes => "pentagon",
        Group::ToolPresets => "settings",
        Group::CloneSource => "stamp",
        Group::MeasurementLog => "ruler",
        Group::Notes => "file-text",
        Group::ModifierKeys => "keyboard",
        Group::Brushes => "brush",
    }
}

fn flyout_id() -> egui::Id {
    egui::Id::unique("dock-icon-flyout")
}

/// The group opened from a column collapsed to icons, and where its icon is.
fn flyout(ctx: &egui::Context) -> Option<(crate::dock::Side, crate::dock::Group, Rect)> {
    ctx.data(|d| d.get_temp(flyout_id()))
}

/// A column's header: « / » collapses it to an icon strip or expands it again.
fn column_header(app: &mut PhotocraftApp, ui: &mut egui::Ui, side: crate::dock::Side, iconic: bool, t: &Tokens) {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 14.0), Sense::click());
    let out = (side == crate::dock::Side::Right) != crate::direction::is_rtl();
    // Points the way the column goes: out to icons, or back in.
    let icon = if out != iconic { "chevrons-right" } else { "chevrons-left" };
    let at = if out { Rect::from_min_size(pos2(r.right() - 16.0, r.top()), vec2(14.0, 14.0)) } else { Rect::from_min_size(r.min, vec2(14.0, 14.0)) };
    let c = if iconic { Rect::from_center_size(r.center(), vec2(14.0, 14.0)) } else { at };
    icons::paint(ui, c, icon, 11.0, if resp.hovered() { t.text } else { t.text_faint });
    let label = if iconic { tl!("Expand Panels") } else { tl!("Collapse to Icons") };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if resp.on_hover_text(label).clicked() {
        let l = &mut app.ui.dock;
        if l.iconic.contains(&side) {
            l.iconic.retain(|s| *s != side);
        } else {
            l.iconic.push(side);
        }
        ui.ctx().data_mut(|d| d.remove::<(crate::dock::Side, crate::dock::Group, Rect)>(flyout_id()));
    }
}

/// Dock column `side` (Photoshop's panel columns): its groups, or their icons when the column is
/// collapsed. On a narrow screen there is one column, the right one, with every docked group.
fn dock_column(app: &mut PhotocraftApp, ui: &mut egui::Ui, side: crate::dock::Side, narrow: bool, t: &Tokens) {
    use crate::dock::{Place, Side};
    let p = app.ui.panels.clone();
    let shown = column_groups(app, &p, t, (!narrow).then_some(side));
    if shown.is_empty() {
        return;
    }
    let panel = |id: &str| if side == Side::Right { crate::direction::end_panel(id.to_string()) } else { crate::direction::start_panel(id.to_string()) };
    let margin = if t.pro { 2 } else { 8 };
    if !narrow && app.ui.dock.iconic.contains(&side) {
        // Icons only: one per stack of groups; a click opens the group beside the strip.
        let hosts: Vec<crate::dock::Group> = app.ui.dock.order().into_iter().filter(|g| shown.contains(g) && app.ui.dock.host(*g) == *g).collect();
        panel(if side == Side::Right { "dock-icons" } else { "dock-left-icons" })
            .resizable(false)
            .exact_size(if t.pro { 36.0 } else { 44.0 })
            .frame(egui::Frame::NONE.fill(t.dock).inner_margin(egui::Margin::symmetric(4, 4)))
            .show(ui, |ui| {
                crate::dock::note_column(ui.ctx(), side, ui.max_rect());
                column_header(app, ui, side, true, t);
                ui.spacing_mut().item_spacing.y = 4.0;
                let open = flyout(ui.ctx());
                for g in hosts {
                    let name = tl!(g.tabs(t.pro).first().copied().unwrap_or(""));
                    let on = open.is_some_and(|(s, f, _)| s == side && f == g);
                    let r = icons::rail_button(ui, group_icon(g), if t.pro { 28.0 } else { 32.0 }, on, name);
                    if r.clicked() {
                        ui.ctx().data_mut(|d| {
                            if on {
                                d.remove::<(Side, crate::dock::Group, Rect)>(flyout_id());
                            } else {
                                d.insert_temp(flyout_id(), (side, g, r.rect));
                            }
                        });
                    }
                }
            });
        return;
    }
    let id = if side == Side::Right { "dock" } else { "dock-left" };
    let mut panel = panel(id).resizable(true).default_size(if t.pro { 290.0 } else { 300.0 }).size_range(dock_width_range(ui.ctx().content_rect().width()));
    if side == Side::Right
        && let Some(w) = ui.ctx().data_mut(|d| d.remove_temp::<f32>(dock_width_id()))
    {
        panel = panel.exact_size(w);
    }
    panel.frame(egui::Frame::NONE.fill(t.dock).inner_margin(egui::Margin::same(margin))).show(ui, |ui| {
        crate::dock::note_column(ui.ctx(), side, ui.max_rect());
        if !narrow {
            column_header(app, ui, side, false, t);
        }
        // Groups keep their heights whatever they show (#88): see `dock`.
        crate::dock::show(app, ui, Place::Column(side), &shown, dock_body);
    });
}

/// The column beside the toolbar (Photoshop's second panel column): the groups docked there.
/// Empty, it is a drop target beside the toolbar while a group is dragged. Not on a narrow
/// screen (one column) or with a customized panel layout.
pub fn left_dock(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    if app.ui.docking.is_some() || narrow_dock_open(ui.ctx()).is_some() || !app.ui.panels.dock {
        crate::dock::note_left_slot(ui.ctx(), None);
        return;
    }
    let p = app.ui.panels.clone();
    if column_groups(app, &p, &t, Some(crate::dock::Side::Left)).is_empty() {
        let r = ui.available_rect_before_wrap();
        crate::dock::note_left_slot(ui.ctx(), Some(Rect::from_min_size(r.min, vec2(56.0, r.height()))));
        return;
    }
    crate::dock::note_left_slot(ui.ctx(), None);
    dock_column(app, ui, crate::dock::Side::Left, false, &t);
}

/// Dock groups floated out of the dock (each in its own window), the group a column collapsed to
/// icons opened, and the drop zones while a group is dragged. Drawn above the canvas.
pub fn floating_groups(app: &mut PhotocraftApp, ctx: &egui::Context) {
    use crate::dock::{Place, Side};
    let t = Tokens::get(ctx);
    if app.ui.docking.is_none() {
        let p = app.ui.panels.clone();
        let floating: Vec<(crate::dock::Group, [f32; 2])> =
            app.ui.dock.floating.iter().map(|(g, at)| (*g, *at)).filter(|(g, _)| g.shown(&p) && app.ui.dock.host(*g) == *g).collect();
        for (g, at) in floating {
            let shown: Vec<crate::dock::Group> = std::iter::once(g).chain(app.ui.dock.guests_of(g).into_iter().filter(|m| m.shown(&p))).collect();
            let h = (g.default_height() + 40.0).min(ctx.content_rect().height() - 60.0).max(120.0);
            egui::Window::new(g.key())
                .id(egui::Id::unique(("dock-floating", g.key())))
                .title_bar(false)
                .resizable(false)
                .movable(false)
                .fixed_pos(pos2(at[0], at[1]))
                .fixed_size(vec2(270.0, h))
                .frame(
                    egui::Frame::NONE
                        .fill(t.dock)
                        .stroke(Stroke::new(1.0, t.card_border))
                        .shadow(egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: t.shadow })
                        .inner_margin(egui::Margin::same(2)),
                )
                .show(ctx, |ui| {
                    ui.set_min_size(vec2(270.0, h));
                    crate::dock::show(app, ui, Place::Floating, &shown, dock_body);
                });
        }
        // A collapsed column's group, beside its icon.
        if let Some((side, g, icon)) = flyout(ctx) {
            if !app.ui.dock.iconic.contains(&side) || !g.shown(&p) {
                ctx.data_mut(|d| d.remove::<(Side, crate::dock::Group, Rect)>(flyout_id()));
            } else {
                let w = 270.0;
                let h = (g.default_height() + 40.0).min(ctx.content_rect().height() - icon.top() - 8.0).max(120.0);
                let x = if (side == Side::Right) != crate::direction::is_rtl() { icon.left() - w - 6.0 } else { icon.right() + 6.0 };
                let shown: Vec<crate::dock::Group> = std::iter::once(g).chain(app.ui.dock.guests_of(g).into_iter().filter(|m| m.shown(&p))).collect();
                let area =
                    egui::Area::new(egui::Id::unique(("dock-flyout", g.key()))).order(egui::Order::Foreground).fixed_pos(pos2(x, icon.top())).show(ctx, |ui| {
                        egui::Frame::NONE.fill(t.dock).stroke(Stroke::new(1.0, t.card_border)).inner_margin(egui::Margin::same(2)).show(ui, |ui| {
                            ui.set_min_size(vec2(w, h));
                            ui.set_max_size(vec2(w, h));
                            crate::dock::show(app, ui, Place::Flyout(side), &shown, dock_body);
                        });
                    });
                // A click elsewhere (not on the icon strip) closes it, as in Photoshop.
                let strip = crate::dock::columns(ctx).into_iter().find(|(s, _)| *s == side).map(|(_, r)| r);
                if ctx.input(|i| i.pointer.any_pressed())
                    && let Some(at) = ctx.input(|i| i.pointer.interact_pos())
                    && !area.response.rect.contains(at)
                    && !strip.is_some_and(|r| r.contains(at))
                    && !ctx.any_popup_open()
                {
                    ctx.data_mut(|d| d.remove::<(Side, crate::dock::Group, Rect)>(flyout_id()));
                }
            }
        }
    }
    crate::dock::paint_drop_hint(app, ctx);
}

/// One dock group's tab content; `panel_docking` bounds it and scrolls it when it's taller.
pub(crate) fn dock_body(app: &mut PhotocraftApp, ui: &mut egui::Ui, group: crate::dock::Group, tab: usize) {
    use crate::dock::Group;
    // A docked floating panel draws itself over this area later in the frame.
    if group.is_floater() {
        crate::floaters::reserve(ui, group);
        return;
    }
    let pro = Tokens::get(ui.ctx()).pro;
    match (group, tab) {
        (Group::Color, 2) => crate::preset_panels::gradients_panel(app, ui),
        (Group::Color, 3) => crate::preset_panels::patterns_panel(app, ui),
        // The Color tab is the same in every theme, in the mode picked from its panel menu
        // (#6000); only the tab order differs between themes.
        (Group::Color, tab) if group.tabs(pro).get(tab) == Some(&"Color") => crate::color_panel_ui::panel(app, ui),
        (Group::Color, _) => crate::swatches_ui::panel(app, ui),
        (Group::Properties, 0) => properties_body(app, ui),
        (Group::Properties, _) => adjustments_grid(app, ui),
        (Group::Character, tab) => crate::type_tool::character_panel(app, ui, tab == 1),
        (Group::TypeR, _) => crate::typer::panel(app, ui),
        (Group::Navigator, 0) => navigator(app, ui),
        (Group::Navigator, 1) => crate::histogram_panel::show(app, ui),
        (Group::Navigator, _) => info_panel(app, ui),
        (Group::History, 0) => history(app, ui),
        (Group::History, 1) => crate::actions::panel(app, ui),
        (Group::History, _) => crate::comps_ui::panel(app, ui),
        (Group::Layers, 0) => layers(app, ui),
        (Group::Layers, 1) => channels(app, ui),
        (Group::Layers, _) => crate::vector_ui::paths_panel(app, ui),
        (Group::Brushes, _) => crate::brushes_tab::show(app, ui),
        // Docked floating panels returned above.
        _ => {}
    }
}
