//! The simple interface for phones (Preferences › Interface › Simple UI, Window › Simple UI):
//! a compact top bar (menus folded into ☰, the document's name, Undo, Redo and Search), the
//! canvas across the whole window, and floating over it the recent commands, the favorites bar
//! and the Capi button, which opens a sheet with every tool, the main panels and the file
//! actions. The desktop chrome (toolbar, options bar, docks, status bar) is hidden, not closed:
//! turning the simple interface off brings it back as it was.

use egui::{Align2, Color32, CornerRadius, Id, Rect, Response, RichText, Sense, Stroke, Ui, Vec2, pos2, vec2};
use photocraft_engine::prefs::SimpleUi;
use serde_json::json;

use crate::PhotocraftApp;
use crate::dock::Group;
use crate::state::Tool;
use crate::theme::Tokens;

/// Windows narrower than this (points) get the simple interface on `auto`: phones held upright.
/// The width alone, as for the menu bar's ☰: a short laptop window at a high scale keeps the
/// desktop interface.
pub const PHONE_BELOW: f32 = 600.0;

/// The favorites bar when the user hasn't picked any.
pub const DEFAULT_FAVORITES: [&str; 6] = ["tool:Move", "tool:Lasso", "tool:Brush", "tool:Eraser", "tool:Type", "tool:CloneStamp"];

/// The recent-tools bar before any tool has been used.
pub const DEFAULT_RECENT_TOOLS: [Tool; 6] = [Tool::RectMarquee, Tool::SpotHealing, Tool::Eyedropper, Tool::PaintBucket, Tool::Hand, Tool::Zoom];

/// Each bar's slots: three left of the Capi button, three right.
pub const SLOTS: usize = 6;

/// Height of the top bar and the options row, of each floating bar, the Capi button's side, the
/// margin around the bars and the gap between them (points).
pub(crate) const TOP_BAR: f32 = 44.0;
const BAR: f32 = 54.0;
const CAPI: f32 = 66.0;
const MARGIN: f32 = 8.0;
const GAP: f32 = 6.0;
/// A tool slot's side.
const SLOT: f32 = 44.0;

/// Whether the simple interface is on for this window.
pub fn active(app: &PhotocraftApp, ctx: &egui::Context) -> bool {
    match app.session.prefs().interface.simple_ui {
        SimpleUi::On => true,
        SimpleUi::Off => false,
        SimpleUi::Auto => ctx.content_rect().width() < PHONE_BELOW,
    }
}

/// Window › Simple UI: on if it is off here, off if it is on (the preference, so it stays).
pub fn toggle(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<(), String> {
    let next = if active(app, ctx) { "off" } else { "on" };
    app.run("prefs.set", json!({"values": {"interface.simpleUi": next}})).map(|_| ()).map_err(|e| e.to_string())
}

/// What the Capi button has open.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Sheet {
    #[default]
    Closed,
    /// Tools, panels and actions.
    Master,
    /// One panel group (its tab) across the bottom of the window.
    Panel(Group, usize),
}

fn sheet_id() -> Id {
    Id::new("simple-ui-sheet")
}

fn rects_id() -> Id {
    Id::new("simple-ui-rects")
}

/// Where a button of the simple interface was drawn this frame (for `ui.inspect`: automation
/// and tests tap it there).
fn note(ctx: &egui::Context, label: &str, rect: Rect) {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Vec<(String, [f32; 4])>>(rects_id()).push((label.to_string(), [rect.min.x, rect.min.y, rect.max.x, rect.max.y]))
    });
}

/// `ui.inspect`'s `simpleUi`: whether it is on, the open sheet, and the buttons' rects by label.
pub fn inspect(app: &PhotocraftApp, ctx: &egui::Context) -> serde_json::Value {
    let rects: Vec<(String, [f32; 4])> = ctx.data(|d| d.get_temp(rects_id())).unwrap_or_default();
    let sheet = match sheet(ctx) {
        Sheet::Closed => json!(null),
        Sheet::Master => json!("master"),
        Sheet::Panel(g, tab) => json!({"panel": g.key(), "tab": tab}),
    };
    json!({"active": app.simple, "sheet": sheet, "buttons": rects.into_iter().map(|(l, r)| json!({"label": l, "rect": r})).collect::<Vec<_>>()})
}

/// Forget where the buttons were (the interface is hidden this frame).
pub fn clear_rects(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<Vec<(String, [f32; 4])>>(rects_id()));
}

pub fn sheet(ctx: &egui::Context) -> Sheet {
    ctx.data(|d| d.get_temp(sheet_id())).unwrap_or_default()
}

pub fn set_sheet(ctx: &egui::Context, s: Sheet) {
    ctx.data_mut(|d| d.insert_temp(sheet_id(), s));
}

/// The favorites bar's entries (tool names as `tool:Name`, command ids).
pub fn favorites(app: &PhotocraftApp) -> Vec<String> {
    let f = &app.session.prefs().interface.favorites;
    if f.is_empty() { DEFAULT_FAVORITES.iter().map(|s| s.to_string()).collect() } else { f.clone() }
}

/// Add `id` to the favorites, or take it out.
pub fn toggle_favorite(app: &mut PhotocraftApp, id: &str) {
    let mut f = favorites(app);
    if let Some(i) = f.iter().position(|x| x == id) {
        f.remove(i);
    } else {
        f.push(id.to_string());
    }
    let _ = app.run("prefs.set", json!({"values": {"interface.favorites": f}}));
}

pub fn is_favorite(app: &PhotocraftApp, id: &str) -> bool {
    favorites(app).iter().any(|x| x == id)
}

/// A menu command's label in the UI language (the catalog's English label translated).
pub fn command_label(id: &str) -> Option<String> {
    let (_, label, _, _) = crate::menu_catalog::CATALOG.iter().find(|c| c.3 == id)?;
    Some(crate::i18n::tr_id(crate::i18n::current(), id, label).trim_end_matches('…').to_string())
}

/// An icon for a command that has one (the rest show their name).
fn command_icon(id: &str) -> Option<&'static str> {
    Some(match id {
        "edit.undo" | "edit.stepBackward" => "undo-2",
        "edit.redo" | "edit.stepForward" => "redo-2",
        "file.new" => "file-plus",
        "file.open" => "folder-open",
        "file.save" | "file.export.quickExportAsPng" => "download",
        "edit.search" => "search",
        "edit.inpaintAi" => "sparkles",
        "layer.new.layer" => "layers",
        _ => return None,
    })
}

/// Run a favorite or recent entry: a tool (`tool:Name`) is selected, anything else is a command.
pub fn run_entry(app: &mut PhotocraftApp, ctx: &egui::Context, id: &str) {
    if let Some(name) = id.strip_prefix("tool:") {
        if let Some(t) = Tool::from_name(name) {
            crate::tool_memory::select(app, t);
        }
        return;
    }
    if let Err(e) = crate::menus::invoke(app, ctx, id, json!({})) {
        app.ui.status = e;
    }
}

/// The compact top bar: menus (☰ on a phone), the document's name, Undo, Redo, Search.
pub fn top_bar(app: &mut PhotocraftApp, ui: &mut Ui) {
    // The first of the simple interface's pieces each frame: a fresh list of where its buttons are.
    ui.ctx().data_mut(|d| d.remove::<Vec<(String, [f32; 4])>>(rects_id()));
    let t = Tokens::get(ui.ctx());
    // The notch and status bar (the safe area's top inset) above the bar, in its colour.
    let inset = crate::safe_area::get(ui.ctx()).top;
    if inset > 0.5 {
        egui::Panel::top("simple_safe_top").exact_size(inset).frame(egui::Frame::NONE.fill(t.chrome)).show(ui, |_| {});
    }
    egui::Panel::top("simple_top_bar")
        .exact_size(TOP_BAR)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left: 8, right: 6, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            let full = ui.max_rect();
            ui.horizontal_centered(|ui| {
                // File, Edit and Image by name; the other menus behind "⋯".
                let menus_end = crate::menus::menu_bar_with(app, ui, crate::menus::BarLayout::Split(&["File", "Edit", "Image"]));
                // Undo, Redo and Search at the far end when there is room for them (else they are
                // in the Capi sheet), then the name in the room between.
                let fit = full.right() - menus_end >= 3.0 * 40.0 + 12.0;
                ui.ctx().data_mut(|d| d.insert_temp(Id::new("simple-ui-bar-icons"), fit));
                if !fit {
                    return;
                }
                let right = Rect::from_min_max(pos2(menus_end, full.top()), full.right_bottom());
                let mut r = ui.new_child(egui::UiBuilder::new().max_rect(right).layout(egui::Layout::right_to_left(egui::Align::Center)));
                r.spacing_mut().item_spacing.x = 2.0;
                if crate::icons::button(&mut r, "search", 40.0, app.ui.palette_open, &crate::shortcuts::tip_label(app, "Search commands", "edit.search")).clicked() {
                    app.ui.palette_open = !app.ui.palette_open;
                }
                let has_doc = app.session.active().is_some();
                let (can_undo, can_redo) = app.session.active().map_or((false, false), |d| (d.history.can_undo(), d.history.can_redo()));
                r.add_enabled_ui(has_doc && can_redo, |ui| {
                    if crate::icons::button(ui, "redo-2", 40.0, false, tl!("Redo")).clicked() {
                        run_entry(app, ui.ctx(), "edit.redo");
                    }
                });
                r.add_enabled_ui(has_doc && can_undo, |ui| {
                    if crate::icons::button(ui, "undo-2", 40.0, false, tl!("Undo")).clicked() {
                        run_entry(app, ui.ctx(), "edit.undo");
                    }
                });
                let end = r.min_rect().left();
                let title = app.session.active().map(|d| format!("{}{}", d.doc.name, if d.is_dirty() { " •" } else { "" })).unwrap_or_else(|| "Capishot".into());
                let room = (end - menus_end - 12.0).max(0.0);
                if room > 24.0 {
                    let mut job = egui::text::LayoutJob::simple_singleline(title, crate::theme::medium(14.0), t.text_dim);
                    job.wrap = egui::text::TextWrapping::truncate_at_width(room);
                    let galley = ui.painter().layout_job(job);
                    let x = menus_end + 6.0 + (room - galley.size().x).max(0.0) / 2.0;
                    ui.painter().galley(pos2(x, full.center().y - galley.size().y / 2.0), galley, t.text_dim);
                }
            });
        });
}

/// Whether the top bar had room for Undo, Redo and Search (else the Capi sheet offers them).
fn bar_icons(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(Id::new("simple-ui-bar-icons"))).unwrap_or(true)
}

fn recent_id() -> Id {
    Id::new("simple-ui-recent-tools")
}

/// The tools used lately, newest first (session-scoped).
pub fn recent_tools(ctx: &egui::Context) -> Vec<Tool> {
    ctx.data(|d| d.get_temp(recent_id())).unwrap_or_default()
}

/// Note that `tool` is the active tool: it moves to the front of the recent tools.
pub fn note_tool(ctx: &egui::Context, tool: Tool) {
    ctx.data_mut(|d| {
        let list = d.get_temp_mut_or_default::<Vec<Tool>>(recent_id());
        if list.first() != Some(&tool) {
            list.retain(|t| *t != tool);
            list.insert(0, tool);
            list.truncate(24);
        }
    });
}

/// The recent-tools bar: the tools used lately that aren't favorites, filled up with
/// [`DEFAULT_RECENT_TOOLS`], six of them. A tool already in the bar keeps its slot: a tool
/// newly used takes the slot of the one used longest ago, so the bar never shuffles under a
/// finger that is about to tap it.
pub fn recent_bar(app: &PhotocraftApp, ctx: &egui::Context) -> Vec<Tool> {
    let favs = favorites(app);
    let fav = |t: &Tool| favs.iter().any(|f| f.strip_prefix("tool:") == Some(&format!("{t:?}")));
    let recent = recent_tools(ctx);
    let bar_id = Id::new("simple-ui-recent-bar");
    let mut out: Vec<Tool> = ctx.data(|d| d.get_temp::<Vec<Tool>>(bar_id)).unwrap_or_default();
    out.retain(|t| !fav(t));
    out.dedup();
    for t in recent.iter().copied().chain(DEFAULT_RECENT_TOOLS) {
        if out.len() >= SLOTS {
            break;
        }
        if !fav(&t) && !out.contains(&t) {
            out.push(t);
        }
    }
    // Tools used lately that the bar lacks, newest last: each replaces the stalest slot.
    let rank = |t: &Tool| recent.iter().position(|r| r == t).unwrap_or(usize::MAX);
    for t in recent.iter().take(SLOTS).rev().copied() {
        if fav(&t) || out.contains(&t) {
            continue;
        }
        if let Some((k, _)) = out.iter().enumerate().filter(|(_, o)| rank(o) > rank(&t)).max_by_key(|(_, o)| rank(o)) {
            out[k] = t;
        }
    }
    out.truncate(SLOTS);
    ctx.data_mut(|d| d.insert_temp(bar_id, out.clone()));
    out
}

fn options_open_id() -> Id {
    Id::new("simple-ui-options-open")
}

/// The tool's options on a phone: the tool, its brush and size where it paints, and "⋯" for the
/// rest (Mode, Opacity, Flow, …), which open below the row and wrap to the window's width.
pub fn options_row(app: &mut PhotocraftApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    let open = ctx.data(|d| d.get_temp::<bool>(options_open_id())).unwrap_or(false);
    let mut toggle = false;
    let row = egui::Panel::top("simple_options_row")
        .exact_size(TOP_BAR)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let tool = app.ui.tool;
                let (icon, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
                crate::icons::paint(ui, icon, crate::icons::tool_icon(tool), 20.0, t.text);
                if app.ui.transform.is_some() {
                    ui.label(RichText::new(crate::i18n::t("Free Transform")).color(t.text_dim).size(13.0));
                } else if selection_modes(tool) {
                    // New, Add, Subtract, Intersect as chips, in place of the name.
                    mode_chips(app, ui);
                } else {
                    ui.label(RichText::new(crate::i18n::t(tool.label())).color(t.text_dim).size(13.0));
                }
                // With the options open the brush is there (its chip is one widget per frame).
                if !open && (tool.is_brushlike() || matches!(tool, Tool::Brush | Tool::Pencil | Tool::Eraser | Tool::MixerBrush)) {
                    crate::paint_mouse::sync_tool_brush(app);
                    let before = app.session.tools.brush.clone();
                    let mut b = before.clone();
                    crate::panels::brush_preset_chip(ui, &b, &mut app.ui);
                    crate::widgets::value_field(ui, &mut b.size, 1.0..=photocraft_engine::paint::MAX_BRUSH_SIZE, "px", 64.0);
                    crate::brush_panel::commit_gesture(app, ui.ctx(), &before, &b);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let more = crate::icons::button(ui, "sliders-horizontal", 38.0, open, tl!("Tool options"));
                    note(ui.ctx(), "options", more.rect);
                    if more.clicked() {
                        toggle = true;
                    }
                });
            });
        });
    let now = open != toggle;
    ctx.data_mut(|d| d.insert_temp(options_open_id(), now));
    // Opened by this frame's tap: shown from the next (the row drew the brush chip already).
    if !(now && open) {
        if now {
            ctx.request_repaint();
        }
        return;
    }
    // A dialog the options opened (the text colour's Color Picker) takes over: the options close.
    if !app.ui.dialogs.is_empty() {
        ctx.data_mut(|d| d.insert_temp(options_open_id(), false));
        return;
    }
    // The options, a form under the row as wide as the window, scrolling when taller than it;
    // a tap outside, the × or Esc (the phone's Back) closes them.
    let screen = crate::safe_area::rect(&ctx);
    let top = row.response.rect.bottom() + 4.0;
    let max_h = (screen.bottom() - top - 16.0).max(120.0);
    let mut close = false;
    let area = egui::Area::new(Id::new("simple-options")).order(egui::Order::Foreground).fixed_pos(pos2(screen.left() + 8.0, top)).show(&ctx, |ui| {
        float_frame(&t).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            let width = screen.width() - 16.0 - 20.0;
            ui.set_width(width);
            ui.set_max_width(width);
            let tool = app.ui.tool;
            ui.horizontal(|ui| {
                ui.label(RichText::new(crate::i18n::t(tool.label())).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let x = crate::icons::button(ui, "x", 32.0, false, tl!("Close"));
                    note(ui.ctx(), "options-close", x.rect);
                    if x.clicked() {
                        close = true;
                    }
                });
            });
            ui.add_space(4.0);
            // Taller than the room scrolls; a control wider than the window scrolls sideways
            // instead of widening the form past the screen.
            egui::ScrollArea::both().id_salt("simple-options-scroll").max_height(max_h - 50.0).max_width(width).auto_shrink([false, true]).show(ui, |ui| {
                ui.set_width(width - 4.0);
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                if matches!(tool, Tool::Brush | Tool::Pencil | Tool::Eraser | Tool::MixerBrush) {
                    paint_options(app, ui);
                } else {
                    // Any other tool: its options as a form, rows filling the width (no
                    // separators, home button or tool icon: the row above has the tool).
                    crate::widgets::in_phone_form(ui, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                        if tool.is_type() {
                            crate::panels::tool_options_contents(app, ui);
                        } else {
                            ui.horizontal_wrapped(|ui| crate::panels::tool_options_contents(app, ui));
                        }
                    });
                }
            });
        });
    });
    note(&ctx, "options-popover", area.response.rect);
    let outside = ctx.input(|i| i.pointer.primary_clicked() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p) && !row.response.rect.contains(p)));
    let back = !egui::Popup::is_any_open(&ctx) && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    if close || back || (outside && !egui::Popup::is_any_open(&ctx)) {
        ctx.data_mut(|d| d.insert_temp(options_open_id(), false));
    }
}

/// Whether the phone's tool options popover is open.
pub fn options_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(options_open_id())).unwrap_or(false)
}

/// The selection tools whose options bar has New, Add, Subtract and Intersect.
pub fn selection_modes(tool: Tool) -> bool {
    matches!(tool, Tool::RectMarquee | Tool::EllipseMarquee | Tool::SingleRowMarquee | Tool::SingleColumnMarquee | Tool::Lasso | Tool::PolygonLasso | Tool::MagneticLasso | Tool::MagicWand)
}

/// The selection mode chips: touch screens have no Shift or Alt to add or subtract (the touch
/// modifier button does it too).
fn mode_chips(app: &mut PhotocraftApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    for (k, label) in [(0u8, "New selection"), (1, "Add to selection"), (2, "Subtract from selection"), (3, "Intersect with selection")] {
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(38.0), Sense::click());
        let on = app.ui.selection_mode == k;
        let p = ui.painter();
        let bg = if on { t.accent } else if resp.hovered() { t.hover } else { t.chrome };
        p.rect(rect.shrink(2.0), CornerRadius::same(10), bg, Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
        paint_mode_glyph(p, rect.shrink(9.0), k, if on { Color32::WHITE } else { t.text }, bg);
        let resp = resp.on_hover_text(crate::i18n::t(label));
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, crate::i18n::t(label)));
        note(ui.ctx(), &format!("mode:{k}"), rect);
        if resp.clicked() {
            app.ui.selection_mode = k;
        }
    }
}

/// Two squares drawn as the selection mode: one (New), joined (Add), the second cut out of the
/// first (Subtract), or only their overlap (Intersect).
fn paint_mode_glyph(p: &egui::Painter, r: Rect, mode: u8, c: Color32, bg: Color32) {
    let s = r.width() * 0.62;
    let a = Rect::from_min_size(r.min, Vec2::splat(s));
    let b = Rect::from_min_size(r.max - Vec2::splat(s), Vec2::splat(s));
    let stroke = Stroke::new(1.5, c);
    match mode {
        0 => {
            p.rect_stroke(Rect::from_center_size(r.center(), Vec2::splat(s * 1.1)), 1.0, stroke, egui::StrokeKind::Middle);
        }
        1 => {
            p.rect_filled(a, 1.0, c);
            p.rect_filled(b, 1.0, c);
        }
        2 => {
            p.rect_filled(a, 1.0, c);
            p.rect_filled(a.intersect(b), 0.0, bg);
            p.rect_stroke(b, 1.0, stroke, egui::StrokeKind::Middle);
        }
        _ => {
            p.rect_stroke(a, 1.0, stroke, egui::StrokeKind::Middle);
            p.rect_stroke(b, 1.0, stroke, egui::StrokeKind::Middle);
            p.rect_filled(a.intersect(b), 0.0, c);
        }
    }
}

/// The painting tools' options in a grid: Mode, Opacity (and pressure), Flow (and airbrush),
/// Smoothing, pressure for size.
fn paint_options(app: &mut PhotocraftApp, ui: &mut Ui) {
    let tool = app.ui.tool;
    crate::paint_mouse::sync_tool_brush(app);
    let before = app.session.tools.brush.clone();
    let mut b = before.clone();
    // A Block-mode Eraser has a fixed size, opacity and flow; a Pencil-mode one full flow.
    let block = crate::eraser_ui::block_mode(app, tool);
    let pencil = tool == Tool::Pencil || crate::eraser_ui::pencil_mode(app, tool);
    let held = app.held_erase;
    let percent = |ui: &mut Ui, v: &mut f32, min: f32| {
        let mut p = *v * 100.0;
        if crate::widgets::value_field(ui, &mut p, min..=100.0, "%", 76.0).changed() {
            *v = p / 100.0;
        }
    };
    egui::Grid::new("simple-paint-options").num_columns(3).spacing([12.0, 10.0]).show(ui, |ui| {
        ui.label(tl!("Mode"));
        if tool == Tool::Eraser {
            crate::eraser_ui::mode_dropdown(ui, &mut app.ui.tool_options.eraser_mode);
        } else {
            let clear = if tool == Tool::Pencil { &mut app.ui.tool_options.pencil_clear } else { &mut app.ui.tool_options.brush_clear };
            crate::panels::paint_mode_dropdown(ui, "simple-paint-mode", &mut b, clear, held);
        }
        ui.label("");
        ui.end_row();
        ui.label(tl!("Opacity"));
        ui.add_enabled_ui(!block, |ui| percent(ui, &mut b.opacity, 0.0));
        if crate::icons::button(ui, "circle-dot", 32.0, b.pressure_opacity, tl!("Always use pressure for opacity")).clicked() {
            b.pressure_opacity = !b.pressure_opacity;
        }
        ui.end_row();
        if !pencil {
            ui.label(tl!("Flow"));
            ui.add_enabled_ui(!block, |ui| percent(ui, &mut b.flow, 1.0));
            if crate::icons::button(ui, "sparkles", 32.0, b.build_up, tl!("Enable airbrush-style build-up effects")).clicked() {
                b.build_up = !b.build_up;
            }
            ui.end_row();
        }
        ui.label(tl!("Smoothing"));
        crate::panels::smoothing_field(ui, &mut b, 76.0);
        crate::panels::smoothing_options(ui, &mut b);
        ui.end_row();
        ui.label(tl!("Size"));
        ui.label("");
        if crate::icons::button(ui, "circle-dot", 32.0, b.pressure_size, tl!("Always use pressure for size")).clicked() {
            b.pressure_size = !b.pressure_size;
        }
        ui.end_row();
    });
    crate::brush_panel::commit_gesture(app, ui.ctx(), &before, &b);
}

/// The floating pieces over the canvas: the favorites bar, the recent-tools bar below it, the
/// Capi button between both, and whatever sheet it has open.
pub fn overlays(app: &mut PhotocraftApp, ctx: &egui::Context) {
    note_tool(ctx, app.ui.tool);
    // Typing: the keyboard and the type bar take the bottom (touch_ui::type_bar); the Capi
    // button stays, above the bar, so a tool can still be chosen (which ends the typing).
    if app.ui.text_edit.is_some() && sheet(ctx) == Sheet::Closed {
        let screen = crate::safe_area::rect(ctx);
        let bar = crate::touch_ui::type_bar_height(ctx);
        let capi = Rect::from_center_size(pos2(screen.center().x, screen.bottom() - 6.0 - bar - 10.0 - CAPI / 2.0), Vec2::splat(CAPI));
        capi_button(app, ctx, capi);
        return;
    }
    // Esc (the phone's Back) closes the open sheet.
    if sheet(ctx) != Sheet::Closed
        && help_open(ctx).is_none()
        && app.ui.dialogs.is_empty()
        && !egui::Popup::is_any_open(ctx)
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        set_sheet(ctx, Sheet::Closed);
    }
    let screen = crate::safe_area::rect(ctx);
    let has_doc = app.session.active().is_some();
    let open = sheet(ctx);
    let lower = Rect::from_min_size(pos2(screen.left() + MARGIN, screen.bottom() - MARGIN - BAR), vec2(screen.width() - 2.0 * MARGIN, BAR));
    let upper = lower.translate(vec2(0.0, -(BAR + GAP)));
    let capi = Rect::from_center_size(pos2(screen.center().x, lower.top() - GAP / 2.0), Vec2::splat(CAPI));
    if open == Sheet::Closed && has_doc && !app.ui.palette_open {
        let favs = favorites(app);
        tool_bar(app, ctx, "simple-favorites", upper, &favs);
        let recents: Vec<String> = recent_bar(app, ctx).iter().map(|t| format!("tool:{t:?}")).collect();
        tool_bar(app, ctx, "simple-recents", lower, &recents);
    }
    capi_button(app, ctx, capi);
    match open {
        Sheet::Closed => {}
        Sheet::Master => master_sheet(app, ctx, screen),
        Sheet::Panel(g, tab) => panel_sheet(app, ctx, screen, g, tab),
    }
    help_card(app, ctx, screen);
}

fn help_id() -> Id {
    Id::new("simple-help")
}

/// The long-press card for `entry` (a tool or command), pressed at `rect`.
fn open_help(ctx: &egui::Context, entry: &str, rect: Rect) {
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|d| d.insert_temp(help_id(), (entry.to_string(), [rect.min.x, rect.min.y, rect.max.x, rect.max.y], frame)));
}

/// A long press on `entry`'s button (a finger resting half a second; a right click with a mouse).
/// Frames keep coming while the finger rests, so it fires without waiting for the lift.
fn held(resp: &Response, entry: &str) -> bool {
    resp.long_touched() || resp.secondary_clicked() || crate::touch_nav::long_press(&resp.ctx, Id::new(("simple-hold", entry)), resp.rect).is_some()
}

/// The finger lifting after a long press on `entry` (whose card it opened) is no tap.
fn long_pressed(ctx: &egui::Context, entry: &str) -> bool {
    help_open(ctx).as_deref() == Some(entry)
}

/// The entry whose long-press card is open.
pub fn help_open(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp::<(String, [f32; 4], u64)>(help_id())).map(|h| h.0)
}

fn close_help(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<(String, [f32; 4], u64)>(help_id()));
}

/// The long-press card: what the tool or command does (the palette's description), the "Which
/// one should I use?" hint of its family, and the favorites bar's add or remove. A press
/// anywhere else, or Back, closes it.
fn help_card(app: &mut PhotocraftApp, ctx: &egui::Context, screen: Rect) {
    let Some((id, r, opened)) = ctx.data(|d| d.get_temp::<(String, [f32; 4], u64)>(help_id())) else { return };
    let anchor = Rect::from_min_max(pos2(r[0], r[1]), pos2(r[2], r[3]));
    let t = Tokens::get(ctx);
    let lang = crate::i18n::current();
    let title = match id.strip_prefix("tool:").and_then(Tool::from_name) {
        Some(tool) => crate::i18n::t(tool.label()).to_string(),
        None => command_label(&id).unwrap_or_else(|| id.clone()),
    };
    let about = crate::help_text::describe(&id, lang).map(str::to_string).or_else(|| {
        let (path, ..) = crate::menu_catalog::CATALOG.iter().find(|c| c.3 == id)?;
        Some(path.iter().map(|p| crate::i18n::t(p)).collect::<Vec<_>>().join(" › "))
    });
    let which = crate::help_text::WHICH.iter().find(|w| w.members.contains(&id.as_str()));
    let width = (screen.width() - 24.0).min(360.0);
    // Above what was pressed (the bars are at the bottom), kept on screen.
    let x = (anchor.center().x - width / 2.0).clamp(screen.left() + 12.0, screen.right() - 12.0 - width);
    let below = anchor.center().y < screen.center().y;
    let (pos, pivot) = if below { (pos2(x, anchor.bottom() + 8.0), Align2::LEFT_TOP) } else { (pos2(x, anchor.top() - 8.0), Align2::LEFT_BOTTOM) };
    let fav = is_favorite(app, &id);
    let mut toggle = false;
    let mut close = false;
    let out = egui::Area::new(Id::new("simple-help-card")).order(egui::Order::Tooltip).pivot(pivot).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::NONE
            .fill(t.card)
            .stroke(Stroke::new(1.0, t.card_border))
            .corner_radius(CornerRadius::same(14))
            .shadow(egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: t.shadow })
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(width - 24.0);
                ui.label(RichText::new(&title).font(crate::theme::semibold(14.0)).color(t.text));
                if let Some(a) = &about {
                    ui.add_space(2.0);
                    ui.label(RichText::new(a).size(12.5).color(t.text_dim));
                }
                if let Some(w) = which {
                    ui.add_space(6.0);
                    ui.label(RichText::new(tl!("Which one should I use?")).font(crate::theme::semibold(12.0)).color(t.text));
                    ui.label(RichText::new(w.text(lang)).size(11.5).color(t.text_faint));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let label = if fav { tl!("Remove from favorites") } else { tl!("Add to favorites") };
                    if chip(ui, label, 34.0).clicked() {
                        toggle = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let x = crate::icons::button(ui, "x", 34.0, false, tl!("Close"));
                        note(ui.ctx(), "help:close", x.rect);
                        if x.clicked() {
                            close = true;
                        }
                    });
                });
            });
    });
    let card = out.response.rect;
    // A press elsewhere (not the long press that opened it) closes the card.
    let elsewhere =
        ctx.cumulative_frame_nr() > opened + 1 && ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !card.contains(p)));
    let back = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    if toggle {
        toggle_favorite(app, &id);
        close = true;
    }
    if close || elsewhere || back {
        close_help(ctx);
    }
}

/// A rounded bar floating over the canvas.
fn float_frame(t: &Tokens) -> egui::Frame {
    egui::Frame::NONE
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.card_border))
        .corner_radius(CornerRadius::same(18))
        .shadow(egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: t.shadow })
}

/// One of the floating bars: `entries` in its slots, three left of the Capi button and three
/// right of it; more than six scroll sideways (past the button). A tap runs an entry, a long
/// press makes a tool a favorite or takes it out.
fn tool_bar(app: &mut PhotocraftApp, ctx: &egui::Context, id: &str, rect: Rect, entries: &[String]) {
    let t = Tokens::get(ctx);
    let mut run = None;
    let mut fav = None;
    // Where the slots go: evenly across each side of the Capi button.
    let half = (rect.width() - CAPI - 16.0) / 2.0;
    let step = half / 3.0;
    let slot_x = |i: usize| -> f32 {
        let side = if i < 3 { rect.left() } else { rect.left() + half + CAPI + 16.0 };
        side + step * (i % 3) as f32 + (step - SLOT) / 2.0
    };
    // A layer below the Capi button's (and the menus', which open over the bars).
    egui::Area::new(Id::new(id)).order(egui::Order::Middle).fixed_pos(rect.min).show(ctx, |ui| {
        float_frame(&t).show(ui, |ui| {
            let (frame_rect, _) = ui.allocate_exact_size(rect.size(), Sense::hover());
            let y = frame_rect.center().y - SLOT / 2.0;
            let mut place = |ui: &mut Ui, entry: &str, x: f32| {
                let slot = Rect::from_min_size(pos2(x, y), Vec2::splat(SLOT));
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(slot));
                let resp = entry_button(app, &mut child, entry, SLOT);
                if held(&resp, entry) {
                    fav = Some((entry.to_string(), resp.rect));
                } else if resp.clicked() && !long_pressed(ui.ctx(), entry) {
                    run = Some(entry.to_string());
                }
            };
            if entries.len() <= SLOTS {
                for (i, e) in entries.iter().enumerate() {
                    place(ui, e, slot_x(i));
                }
            } else {
                // More favorites than slots: the bar scrolls; the fourth starts past the button.
                let mut sub = ui.new_child(egui::UiBuilder::new().max_rect(frame_rect));
                egui::ScrollArea::horizontal()
                    .id_salt((id, "scroll"))
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(&mut sub, |ui| {
                        ui.horizontal_centered(|ui| {
                            ui.spacing_mut().item_spacing.x = step - SLOT;
                            ui.add_space((step - SLOT) / 2.0);
                            for (i, e) in entries.iter().enumerate() {
                                if i == 3 {
                                    ui.add_space(CAPI + 16.0 - (step - SLOT));
                                }
                                let resp = entry_button(app, ui, e, SLOT);
                                if held(&resp, e) {
                                    fav = Some((e.clone(), resp.rect));
                                } else if resp.clicked() && !long_pressed(ui.ctx(), e) {
                                    run = Some(e.clone());
                                }
                            }
                            ui.add_space((step - SLOT) / 2.0);
                        });
                    });
            }
        });
    });
    if let Some((id, rect)) = fav {
        open_help(ctx, &id, rect);
    } else if let Some(id) = run {
        run_entry(app, ctx, &id);
    }
}

/// One slot: a tool or command icon, or a command's initials.
fn entry_button(app: &PhotocraftApp, ui: &mut Ui, id: &str, side: f32) -> Response {
    let resp = if let Some(tool) = id.strip_prefix("tool:").and_then(Tool::from_name) {
        crate::icons::button(ui, crate::icons::tool_icon(tool), side, app.ui.tool == tool, crate::i18n::t(tool.label()))
    } else {
        let label = command_label(id).unwrap_or_else(|| id.to_string());
        match command_icon(id) {
            Some(icon) => crate::icons::button(ui, icon, side, false, &label),
            None => initials(ui, &label, side),
        }
    };
    note(ui.ctx(), &format!("slot:{id}"), resp.rect);
    resp
}

/// A command without an icon: its first two letters in a rounded square (its name is the label).
fn initials(ui: &mut Ui, label: &str, side: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    ui.painter().rect_filled(rect.shrink(4.0), CornerRadius::same(10), if resp.hovered() { t.hover } else { t.chrome });
    let short: String = label.chars().filter(|c| c.is_alphanumeric()).take(2).collect();
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, short, crate::theme::semibold(14.0), t.text);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    resp
}

/// A pill with a label (touch-sized).
fn chip(ui: &mut Ui, label: &str, height: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    let galley = ui.painter().layout_no_wrap(label.to_string(), crate::theme::medium(13.0), t.text);
    let size = vec2(galley.size().x + 22.0, height);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if resp.is_pointer_button_down_on() || resp.hovered() { t.hover } else { t.chrome };
    ui.painter().rect_filled(rect, CornerRadius::same((height / 2.0) as u8), fill);
    ui.painter().rect_stroke(rect, CornerRadius::same((height / 2.0) as u8), Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, t.text);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    note(ui.ctx(), label, rect);
    resp
}

/// The Capi button: opens (or closes) the sheet with everything else.
fn capi_button(app: &mut PhotocraftApp, ctx: &egui::Context, rect: Rect) {
    let t = Tokens::get(ctx);
    let busy = crate::ai_progress::busy(app);
    egui::Area::new(Id::new("simple-capi")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        let (r, resp) = ui.allocate_exact_size(rect.size(), Sense::click());
        let open = sheet(ctx) != Sheet::Closed;
        let p = ui.painter();
        p.add(egui::epaint::CircleShape {
            center: r.center() + vec2(0.0, 3.0),
            radius: r.width() / 2.0,
            fill: Color32::from_black_alpha(80),
            stroke: Stroke::NONE,
        });
        p.circle(
            r.center(),
            r.width() / 2.0,
            if resp.is_pointer_button_down_on() { t.accent_soft } else { t.card },
            Stroke::new(2.5, if open { t.accent } else { t.card_border }),
        );
        // An AI run (or its model's download) in progress: the button pulses.
        if busy {
            crate::ai_progress::capi_ring(ui, r.center(), r.width() / 2.0);
        }
        if open {
            crate::icons::paint(ui, r, "x", 24.0, t.text);
        } else {
            crate::brand::paint_mark(ui, Rect::from_center_size(r.center(), Vec2::splat(42.0)));
        }
        note(ctx, "capi", r);
        let label = if open { tl!("Close") } else { tl!("Capi: tools, panels and actions") };
        let resp = resp.on_hover_text(label);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        if resp.clicked() {
            set_sheet(ctx, if open { Sheet::Closed } else { Sheet::Master });
        }
    });
}

/// Dims the window behind a sheet; a tap there closes the sheet.
fn scrim(ctx: &egui::Context, screen: Rect) {
    egui::Area::new(Id::new("simple-scrim")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let r = ui.allocate_exact_size(screen.size(), Sense::click()).1;
        ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(90));
        if r.clicked() {
            set_sheet(ctx, Sheet::Closed);
        }
    });
}

/// A sheet across the bottom of the window, above the Capi button's row.
/// Room under a sheet: the bars' margin, the lower bar and the upper half of the Capi button.
const UNDER_SHEET: f32 = MARGIN + BAR + GAP / 2.0 + CAPI / 2.0 + 8.0;

fn bottom_sheet(ctx: &egui::Context, screen: Rect, id: &str, height: f32, add: impl FnOnce(&mut Ui)) {
    let t = Tokens::get(ctx);
    let top = screen.bottom() - UNDER_SHEET - height;
    egui::Area::new(Id::new(id)).order(egui::Order::Foreground).fixed_pos(pos2(screen.left() + 8.0, top.max(screen.top() + TOP_BAR))).show(ctx, |ui| {
        egui::Frame::NONE
            .fill(t.card)
            .stroke(Stroke::new(1.0, t.card_border))
            .corner_radius(CornerRadius::same(18))
            .shadow(egui::Shadow { offset: [0, 10], blur: 30, spread: 0, color: t.shadow })
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(screen.width() - 16.0 - 24.0);
                ui.set_max_height(height - 24.0);
                add(ui);
            });
    });
    // egui raises an area that was tapped: once the scrim had closed a sheet it stayed above the
    // next one and took its taps. The sheet and the Capi button always go over the scrim.
    ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, Id::new(id)));
    ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, Id::new("simple-capi")));
}

fn heading(ui: &mut Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(6.0);
    ui.label(RichText::new(text).font(crate::theme::semibold(13.0)).color(t.text_dim));
    ui.add_space(2.0);
}

/// The panels the sheet offers (the groups a phone needs most).
const SHEET_PANELS: [(Group, &str, &str); 6] = [
    (Group::Layers, "Layers", "layers"),
    (Group::Properties, "Properties", "sliders-horizontal"),
    (Group::Color, "Color", "palette"),
    (Group::Brushes, "Brushes", "brush"),
    (Group::History, "History", "clock"),
    (Group::Character, "Character", "panels-top-left"),
];

fn master_sheet(app: &mut PhotocraftApp, ctx: &egui::Context, screen: Rect) {
    scrim(ctx, screen);
    let height = (screen.height() * 0.78).min(screen.height() - TOP_BAR - UNDER_SHEET - 8.0);
    let mut action: Option<String> = None;
    let mut pick_tool: Option<Tool> = None;
    let mut fav: Option<(String, Rect)> = None;
    let mut panel: Option<Group> = None;
    bottom_sheet(ctx, screen, "simple-master", height, |ui| {
        let t = Tokens::get(ui.ctx());
        // Search first: the palette finds any command by its name in Spanish, Portuguese or English.
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::click());
        ui.painter().rect_filled(r, CornerRadius::same(12), t.chrome);
        crate::icons::paint(ui, Rect::from_min_size(r.min + vec2(8.0, 9.0), Vec2::splat(24.0)), "search", 17.0, t.text_dim);
        ui.painter().text(r.left_center() + vec2(40.0, 0.0), Align2::LEFT_CENTER, tl!("Search commands, tools and panels…"), crate::theme::medium(14.0), t.text_faint);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!("Search commands, tools and panels…")));
        note(ui.ctx(), "search", r);
        if resp.clicked() {
            action = Some("edit.search".into());
        }
        egui::ScrollArea::vertical().id_salt("simple-master-scroll").auto_shrink([false, true]).show(ui, |ui| {
            heading(ui, tl!("Panels"));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                for (g, label, _) in SHEET_PANELS {
                    if chip(ui, crate::i18n::t(label), 36.0).clicked() {
                        panel = Some(g);
                    }
                }
            });
            heading(ui, tl!("Actions"));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                let undo: &[&str] = if bar_icons(ui.ctx()) { &[] } else { &["edit.undo", "edit.redo"] };
                for &id in undo.iter().chain(&["file.new", "file.open", "file.save", "file.export.quickExportAsPng", "edit.inpaintAi", "edit.preferences.general"]) {
                    let label = command_label(id).unwrap_or_else(|| id.into());
                    if chip(ui, &label, 36.0).clicked() {
                        action = Some(id.into());
                    }
                }
                if chip(ui, tl!("Full interface"), 36.0).clicked() {
                    action = Some("view.simpleUi".into());
                }
            });
            heading(ui, tl!("Tools"));
            ui.label(RichText::new(tl!("Tap to use a tool; press and hold to see what it does and add it to the favorites bar.")).small().color(t.text_faint));
            ui.add_space(4.0);
            let side = 48.0;
            let per_row = ((ui.available_width() + 4.0) / (side + 4.0)).floor().max(1.0) as usize;
            for row in Tool::ALL.chunks(per_row) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for &tool in row {
                        let id = format!("tool:{tool:?}");
                        let resp = crate::icons::button(ui, crate::icons::tool_icon(tool), side, app.ui.tool == tool, crate::i18n::t(tool.label()));
                        note(ui.ctx(), &id, resp.rect);
                        if is_favorite(app, &id) {
                            let star = Rect::from_min_size(resp.rect.right_top() + vec2(-15.0, 2.0), Vec2::splat(13.0));
                            crate::icons::paint(ui, star, "star-filled", 11.0, t.accent);
                        }
                        if held(&resp, &id) {
                            fav = Some((id.clone(), resp.rect));
                        } else if resp.clicked() && !long_pressed(ui.ctx(), &id) {
                            pick_tool = Some(tool);
                        }
                    }
                });
            }
        });
    });
    if let Some((id, rect)) = fav {
        open_help(ctx, &id, rect);
    }
    if let Some(tool) = pick_tool {
        crate::tool_memory::select(app, tool);
        set_sheet(ctx, Sheet::Closed);
    }
    if let Some(g) = panel {
        set_sheet(ctx, Sheet::Panel(g, 0));
    }
    if let Some(id) = action {
        set_sheet(ctx, Sheet::Closed);
        if id == "view.simpleUi" {
            if let Err(e) = toggle(app, ctx) {
                app.ui.status = e;
            }
        } else if id == "edit.search" {
            app.ui.palette_open = true;
        } else {
            run_entry(app, ctx, &id);
        }
    }
}

fn panel_sheet(app: &mut PhotocraftApp, ctx: &egui::Context, screen: Rect, g: Group, tab: usize) {
    scrim(ctx, screen);
    let height = (screen.height() * 0.62).min(screen.height() - TOP_BAR - UNDER_SHEET - 8.0);
    let pro = Tokens::get(ctx).pro;
    let tabs = g.tabs(pro);
    let tab = tab.min(tabs.len().saturating_sub(1));
    let mut next = Sheet::Panel(g, tab);
    bottom_sheet(ctx, screen, "simple-panel", height, |ui| {
        ui.horizontal(|ui| {
            let back = crate::icons::button(ui, "chevron-left", 36.0, false, tl!("Back"));
            note(ui.ctx(), "back", back.rect);
            if back.clicked() {
                next = Sheet::Master;
            }
            for (i, name) in tabs.iter().enumerate() {
                if crate::widgets::pill_tab(ui, crate::i18n::t(name), i == tab).clicked() {
                    next = Sheet::Panel(g, i);
                }
            }
        });
        ui.add_space(6.0);
        egui::ScrollArea::vertical().id_salt(("simple-panel-scroll", g.key(), tab)).auto_shrink([false, false]).show(ui, |ui| {
            crate::panels::dock_body(app, ui, g, tab);
        });
    });
    if sheet(ctx) == Sheet::Panel(g, tab) {
        set_sheet(ctx, next);
    }
}

#[cfg(test)]
#[path = "simple_ui_tests.rs"]
mod tests;

/// Something the phone's Back button should close (a dialog, the options, a sheet): the web
/// shell keeps a history entry while there is one, and a Back sends Esc, which closes it.
pub fn back_target(app: &PhotocraftApp, ctx: &egui::Context) -> bool {
    !app.ui.dialogs.is_empty() || options_open(ctx) || sheet(ctx) != Sheet::Closed || crate::remove_ai_ui::is_open(app) || app.ui.text_edit.is_some() || help_open(ctx).is_some()
}

/// Where the button `label` was drawn (the last time this frame), as `ui.inspect` reports it.
pub fn noted_rect(ctx: &egui::Context, label: &str) -> Option<Rect> {
    let rects: Vec<(String, [f32; 4])> = ctx.data(|d| d.get_temp(rects_id())).unwrap_or_default();
    rects.iter().rev().find(|(l, _)| l == label).map(|(_, r)| Rect::from_min_max(pos2(r[0], r[1]), pos2(r[2], r[3])))
}
