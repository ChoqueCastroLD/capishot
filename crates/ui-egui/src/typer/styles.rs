//! The styles block (TypeR `stylesBlock.jsx`): the style library as a folder tree. Each style row
//! shows its text colour, its tag chip in the tag colour, its name drawn in its font and its size
//! or size presets, with a quick −/+ on hover. A click selects a style, a double-click edits it,
//! a right-click copies, pastes, duplicates, applies, moves to a folder or deletes it. Folder
//! headers collapse and have their own menu (edit, add a style or subfolder, move up or down,
//! duplicate, export, delete). A style dragged onto a folder (a mouse drag, or a finger's press
//! and hold, then drag) moves there; a folder dragged onto a sibling goes before it.

use egui::{Color32, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use photocraft_typer::{Id, presets};
use serde_json::{Value, json};

use super::{Confirm, act, font_label, with_now};
use crate::PhotocraftApp;
use crate::theme::Tokens;

const ROW_H: f32 = 24.0;
const HEADER_H: f32 = 22.0;
const INDENT: f32 = 12.0;

/// One style as drawn.
struct Row {
    id: Value,
    key: String,
    name: String,
    folder: Option<Id>,
    tag: Option<String>,
    chip: Option<Color32>,
    tags_off: bool,
    color: Option<Color32>,
    stroke: Option<Color32>,
    size: Option<f64>,
    unit: &'static str,
    presets: Vec<f64>,
    family: Option<String>,
}

/// What a click asked for, applied once the list is drawn.
enum Action {
    Select(Value),
    Edit(Value),
    Run(&'static str, Value),
    Copy(Value),
    Paste(Option<Value>),
    Ask(Confirm),
    NewStyle(Option<Value>),
    EditFolder(Value),
    NewFolder(Option<Value>),
    ExportFolder(Value, String),
    Toggle(String),
    /// A style into a folder (`None`: Unsorted).
    MoveStyle(Value, Option<Value>),
    /// A parent's folders in a new order.
    OrderFolders(Option<Value>, Vec<Value>),
}

/// A style dragged with the mouse.
#[derive(Clone)]
struct StyleDrag(Value);

/// A folder dragged with the mouse: its id and its parent's.
#[derive(Clone)]
struct FolderDrag(Value, Option<Value>);

/// The list, for a finger's press-and-hold drag ([`crate::touch_drag`]).
fn drag_list() -> egui::Id {
    egui::Id::unique("typer-styles-touch-drag")
}

/// Where a drag can be dropped: a folder header (`None`: Unsorted), its parent.
struct Drop {
    rect: Rect,
    folder: Option<Value>,
    parent: Option<Value>,
}

/// `order` (sibling folder ids) with `moved` put just before `before`.
fn reordered(order: &[Value], moved: &Value, before: &Value) -> Vec<Value> {
    let mut v: Vec<Value> = order.iter().filter(|o| *o != moved).cloned().collect();
    let at = v.iter().position(|o| o == before).unwrap_or(v.len());
    v.insert(at, moved.clone());
    v
}

fn unit_label(unit: Option<&str>) -> &'static str {
    match unit {
        Some("pointsUnit") => "pt",
        Some("millimetersUnit") => "mm",
        _ => "px",
    }
}

fn rows(app: &PhotocraftApp, ctx: &egui::Context) -> Vec<Row> {
    let lib = &app.session.typer.state.library;
    lib.styles
        .iter()
        .map(|s| {
            let ts = s.text_style();
            Row {
                id: s.id.to_value(),
                key: s.id.to_string(),
                name: s.name.clone(),
                folder: s.folder.clone(),
                tag: s.prefixes.first().cloned(),
                chip: super::hex_color(s.prefix_color()),
                tags_off: s.prefixes_disabled(),
                color: super::rgb_color(ts.and_then(|t| t.get("color"))),
                stroke: s.stroke().filter(|st| st.get("enabled").and_then(Value::as_bool).unwrap_or(false)).and_then(|st| super::rgb_color(st.get("color"))),
                size: s.size(),
                unit: unit_label(s.type_unit()),
                presets: presets::normalize_presets(s),
                family: font_label::family_of(ctx, s),
            }
        })
        .collect()
}

fn fmt_size(v: f64) -> String {
    crate::widgets::fmt_num(v)
}

pub(super) fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui, height: f32) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    let rows = rows(app, &ctx);
    let state = &app.session.typer.state;
    let current = state.current_style_id.as_ref().map(ToString::to_string);
    let default = state.settings.default_style_id.as_ref().map(ToString::to_string);
    let quick = state.settings.show_quick_style_size;
    let step = state.settings.style_size_step;
    let auto_scroll = state.settings.auto_scroll_style;
    let tree = state.library.flatten_tree();
    let collapsed = app.ui.typer.collapsed.clone();
    let has_clip = app.ui.typer.clipboard.is_some();
    let mut actions: Vec<Action> = Vec::new();
    let last_id = egui::Id::unique("typer-styles-last-current");
    let scroll_to_current = auto_scroll && ctx.data(|d| d.get_temp::<Option<String>>(last_id)).flatten() != current;
    ctx.data_mut(|d| d.insert_temp(last_id, current.clone()));
    // Each parent's folders in order (for reordering by drag or Move Up / Down).
    let siblings = |parent: Option<&Id>| -> Vec<Value> { tree.iter().filter(|f| f.parent_id.as_ref() == parent).map(|f| f.id.to_value()).collect() };
    let folder_list: Vec<(Value, String)> = tree.iter().map(|f| (f.id.to_value(), f.label.clone())).collect();
    let mut drops: Vec<Drop> = Vec::new();
    let finger = crate::touch_drag::moving(&ctx, drag_list()).map(|(_, at)| at);
    let scroll = !crate::touch_drag::dragging(&ctx, drag_list());
    egui::ScrollArea::vertical()
        .id_salt("typer-styles")
        .min_scrolled_height(height)
        .max_height(height)
        .auto_shrink([false, false])
        .scroll_source(if scroll { egui::scroll_area::ScrollSource::default() } else { egui::scroll_area::ScrollSource::NONE })
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            if rows.is_empty() && tree.is_empty() {
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(tl!("Add styles to apply to the pasted text: + below, or import a TypeR library.")).color(t.text_faint).size(11.5),
                    )
                    .wrap(),
                );
                return;
            }
            // Unsorted first, as TypeR matches its tags first.
            let unsorted: Vec<&Row> = rows.iter().filter(|r| r.folder.is_none()).collect();
            if !unsorted.is_empty() || tree.is_empty() {
                let open = !collapsed.iter().any(|c| c.is_empty());
                let has_current = unsorted.iter().any(|r| Some(&r.key) == current.as_ref());
                let h = header(ui, &t, None, tl!("Unsorted"), 0, unsorted.len(), has_current, open, has_clip, &[], None, &mut actions);
                drop_on(&h, None, None, finger, &mut actions);
                drops.push(Drop { rect: h.rect, folder: None, parent: None });
                if open {
                    for r in &unsorted {
                        style_row(
                            ui,
                            &t,
                            r,
                            1,
                            current.as_ref() == Some(&r.key),
                            default.as_ref() == Some(&r.key),
                            quick,
                            step,
                            has_clip,
                            scroll_to_current,
                            &folder_list,
                            &mut actions,
                        );
                    }
                }
            }
            let mut hidden_below: Option<usize> = None;
            for f in &tree {
                if hidden_below.is_some_and(|d| f.depth > d) {
                    continue;
                }
                hidden_below = None;
                let key = f.id.to_string();
                let open = !collapsed.contains(&key);
                let members: Vec<&Row> = rows.iter().filter(|r| r.folder.as_ref() == Some(&f.id)).collect();
                let has_current = members.iter().any(|r| Some(&r.key) == current.as_ref());
                let order = siblings(f.parent_id.as_ref());
                let parent = f.parent_id.as_ref().map(Id::to_value);
                let h = header(
                    ui,
                    &t,
                    Some((&f.id.to_value(), &key)),
                    &f.name,
                    f.depth,
                    members.len(),
                    has_current,
                    open,
                    has_clip,
                    &order,
                    parent.clone(),
                    &mut actions,
                );
                if h.drag_started() && !crate::touch_ui::flagged(&ctx) {
                    h.dnd_set_drag_payload(FolderDrag(f.id.to_value(), parent.clone()));
                }
                if crate::touch_ui::flagged(&ctx) && crate::touch_drag::held_still(&h) {
                    crate::touch_drag::start(&ctx, drag_list(), &format!("f:{key}"), &f.name);
                }
                drop_on(&h, Some(&f.id.to_value()), Some((&parent, order.as_slice())), finger, &mut actions);
                drops.push(Drop { rect: h.rect, folder: Some(f.id.to_value()), parent });
                if !open {
                    hidden_below = Some(f.depth);
                    continue;
                }
                for r in members {
                    style_row(
                        ui,
                        &t,
                        r,
                        f.depth + 1,
                        current.as_ref() == Some(&r.key),
                        default.as_ref() == Some(&r.key),
                        quick,
                        step,
                        has_clip,
                        scroll_to_current,
                        &folder_list,
                        &mut actions,
                    );
                }
            }
        });
    // A finger's drag let go over a header.
    if let Some((payload, at)) = crate::touch_drag::frame(&ctx, drag_list())
        && let Some(d) = drops.iter().find(|d| d.rect.contains(at))
    {
        if let Some(key) = payload.strip_prefix("s:")
            && let Some(r) = rows.iter().find(|r| r.key == key)
        {
            actions.push(Action::MoveStyle(r.id.clone(), d.folder.clone()));
        } else if let Some(key) = payload.strip_prefix("f:")
            && let Some(f) = tree.iter().find(|f| f.id.to_string() == key)
            && let Some(before) = &d.folder
            && d.parent == f.parent_id.as_ref().map(Id::to_value)
        {
            let order = siblings(f.parent_id.as_ref());
            actions.push(Action::OrderFolders(d.parent.clone(), reordered(&order, &f.id.to_value(), before)));
        }
    }
    for a in actions {
        apply(app, a);
    }
}

/// A header as a drop target: a style dropped on it moves into its folder (`folder`; `None`:
/// Unsorted), a sibling folder (`siblings`: its parent and their order) goes before it. Lit while
/// something that can drop there is over it.
fn drop_on(h: &egui::Response, folder: Option<&Value>, siblings: Option<(&Option<Value>, &[Value])>, finger: Option<egui::Pos2>, actions: &mut Vec<Action>) {
    // `dnd_release_payload` takes whatever is carried: ask for the type that is.
    let carried_style = egui::DragAndDrop::has_payload_of_type::<StyleDrag>(&h.ctx);
    if carried_style && let Some(s) = h.dnd_release_payload::<StyleDrag>() {
        actions.push(Action::MoveStyle(s.0.clone(), folder.cloned()));
    }
    let folder_drop = if carried_style { None } else { h.dnd_release_payload::<FolderDrag>() };
    if let (Some(f), Some(this), Some((parent, order))) = (folder_drop, folder, siblings)
        && f.1 == *parent
        && f.0 != *this
    {
        actions.push(Action::OrderFolders(parent.clone(), reordered(order, &f.0, this)));
    }
    let over = h.dnd_hover_payload::<StyleDrag>().is_some() || h.dnd_hover_payload::<FolderDrag>().is_some() || finger.is_some_and(|p| h.rect.contains(p));
    if over {
        let t = Tokens::get(&h.ctx);
        h.ctx.layer_painter(h.layer_id).rect_stroke(h.rect.shrink(1.0), 3.0, Stroke::new(2.0, t.accent), StrokeKind::Inside);
    }
}

#[allow(clippy::too_many_arguments)]
fn header(
    ui: &mut egui::Ui,
    t: &Tokens,
    folder: Option<(&Value, &str)>,
    name: &str,
    depth: usize,
    count: usize,
    has_current: bool,
    open: bool,
    has_clip: bool,
    siblings: &[Value],
    parent: Option<Value>,
    actions: &mut Vec<Action>,
) -> egui::Response {
    let sense = if folder.is_some() && !crate::touch_ui::flagged(ui.ctx()) { Sense::click_and_drag() } else { Sense::click() };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_H), sense);
    crate::touch_ui::note_rect(ui.ctx(), &format!("typerFolder:{}", folder.map_or("", |(_, k)| k)), rect);
    if resp.hovered() {
        ui.painter().rect_filled(rect, 2.0, t.hover);
    }
    let x = rect.left() + depth as f32 * INDENT;
    let chevron = Rect::from_center_size(pos2(x + 7.0, rect.center().y), vec2(12.0, 12.0));
    crate::icons::paint(ui, chevron, if open { "chevron-down" } else { "chevron-right" }, 12.0, t.text_dim);
    let icon = Rect::from_center_size(pos2(x + 21.0, rect.center().y), vec2(13.0, 13.0));
    crate::icons::paint(
        ui,
        icon,
        if folder.is_none() {
            "lock"
        } else if open {
            "folder-open"
        } else {
            "folder"
        },
        13.0,
        t.text_dim,
    );
    let font = if has_current { crate::theme::semibold(12.0) } else { egui::FontId::proportional(12.0) };
    let label = format!("{name}  ({count})");
    ui.painter().with_clip_rect(rect).text(
        pos2(x + 32.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        font,
        if has_current { t.text } else { t.text_dim },
    );
    let key = folder.map_or(String::new(), |(_, k)| k.to_string());
    if resp.clicked() {
        actions.push(Action::Toggle(key));
    }
    let folder_id = folder.map(|(v, _)| v.clone());
    resp.context_menu(|ui| {
        crate::widgets::style_spectrum_popup_menu(ui);
        if ui.button(tl!("Add style here…")).clicked() {
            actions.push(Action::NewStyle(folder_id.clone()));
            ui.close();
        }
        if ui.add_enabled(has_clip, egui::Button::new(tl!("Paste style"))).clicked() {
            actions.push(Action::Paste(folder_id.clone()));
            ui.close();
        }
        let Some((id, _)) = folder else { return };
        ui.separator();
        if ui.button(tl!("Edit folder…")).clicked() {
            actions.push(Action::EditFolder(id.clone()));
            ui.close();
        }
        if ui.button(tl!("Add subfolder…")).clicked() {
            actions.push(Action::NewFolder(Some(id.clone())));
            ui.close();
        }
        if let Some(i) = siblings.iter().position(|s| s == id) {
            if ui.add_enabled(i > 0, egui::Button::new(tl!("Move folder up"))).clicked() {
                let mut order = siblings.to_vec();
                order.swap(i, i - 1);
                actions.push(Action::OrderFolders(parent.clone(), order));
                ui.close();
            }
            if ui.add_enabled(i + 1 < siblings.len(), egui::Button::new(tl!("Move folder down"))).clicked() {
                let mut order = siblings.to_vec();
                order.swap(i, i + 1);
                actions.push(Action::OrderFolders(parent.clone(), order));
                ui.close();
            }
        }
        if ui.button(tl!("Duplicate folder")).clicked() {
            actions.push(Action::Run("typer.folder.duplicate", json!({"id": id})));
            ui.close();
        }
        if ui.button(tl!("Export folder…")).clicked() {
            actions.push(Action::ExportFolder(id.clone(), name.to_string()));
            ui.close();
        }
        ui.separator();
        if ui.button(tl!("Delete folder…")).clicked() {
            actions.push(Action::Ask(Confirm::DeleteFolder { id: id.clone(), name: name.to_string(), permanent: false }));
            ui.close();
        }
        if ui.button(tl!("Delete folder and its styles…")).clicked() {
            actions.push(Action::Ask(Confirm::DeleteFolder { id: id.clone(), name: name.to_string(), permanent: true }));
            ui.close();
        }
    });
    resp
}

#[allow(clippy::too_many_arguments)]
fn style_row(
    ui: &mut egui::Ui,
    t: &Tokens,
    r: &Row,
    depth: usize,
    selected: bool,
    is_default: bool,
    quick: bool,
    step: f64,
    has_clip: bool,
    scroll: bool,
    folders: &[(Value, String)],
    actions: &mut Vec<Action>,
) {
    let touch = crate::touch_ui::flagged(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), if touch { Sense::click() } else { Sense::click_and_drag() });
    crate::touch_ui::note_rect(ui.ctx(), &format!("typerStyle:{}", r.key), rect);
    // Dragged onto a folder header: moved there (a finger presses and holds first).
    if !touch && resp.drag_started() {
        resp.dnd_set_drag_payload(StyleDrag(r.id.clone()));
    }
    if touch && crate::touch_drag::held_still(&resp) {
        crate::touch_drag::start(ui.ctx(), drag_list(), &format!("s:{}", r.key), &r.name);
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, &r.name));
    if selected {
        ui.painter().rect_filled(rect, 2.0, t.row_selected);
        if scroll {
            ui.scroll_to_rect(rect, Some(egui::Align::Center));
        }
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 2.0, t.hover);
    }
    let mut x = rect.left() + depth as f32 * INDENT + 6.0;
    // Text colour, with the stroke as its outline.
    let sw = Rect::from_center_size(pos2(x + 6.0, rect.center().y), vec2(11.0, 11.0));
    ui.painter().rect_filled(sw, 2.0, r.color.unwrap_or(Color32::BLACK));
    ui.painter().rect_stroke(sw, 2.0, Stroke::new(if r.stroke.is_some() { 2.0 } else { 1.0 }, r.stroke.unwrap_or(t.field_border)), StrokeKind::Outside);
    x += 18.0;
    // Tag chip in the tag colour (struck through while the tags are off).
    if let Some(tag) = &r.tag {
        let short: String = tag.chars().take(8).collect();
        let galley = ui.painter().layout_no_wrap(short, egui::FontId::monospace(10.0), Color32::from_gray(28));
        let chip = Rect::from_min_size(pos2(x, rect.center().y - 7.5), vec2(galley.size().x + 8.0, 15.0));
        let fill = r.chip.unwrap_or(Color32::from_rgb(0xFF, 0xF3, 0xB0));
        ui.painter().rect_filled(chip, 3.0, if r.tags_off { fill.gamma_multiply(0.35) } else { fill });
        ui.painter().galley(pos2(chip.left() + 4.0, chip.center().y - galley.size().y / 2.0), galley, Color32::from_gray(28));
        if r.tags_off {
            ui.painter().line_segment([chip.left_center(), chip.right_center()], Stroke::new(1.0, Color32::from_gray(28)));
        }
        x = chip.right() + 6.0;
    }
    // Right side: size presets, the size, or the quick −/+ on hover.
    let hovered = resp.hovered() || ui.rect_contains_pointer(rect);
    let mut right = rect.right() - 4.0;
    let mut clicked_inner = false;
    if quick && hovered && r.size.is_some() {
        for (icon, delta) in [("plus", step), ("minus", -step)] {
            let b = Rect::from_center_size(pos2(right - 9.0, rect.center().y), vec2(18.0, 18.0));
            let br = ui.interact(b, resp.id.with(icon), Sense::click());
            crate::icons::paint(ui, b, icon, 12.0, if br.hovered() { t.text } else { t.text_dim });
            if br.clicked()
                && let Some(size) = r.size
            {
                clicked_inner = true;
                let next = (size + delta).max(0.1);
                actions.push(Action::Run("typer.style.setSize", json!({"id": r.id, "size": (next * 1000.0).round() / 1000.0})));
            }
            right -= 20.0;
        }
    }
    if r.presets.len() >= 2 {
        for p in r.presets.iter().rev() {
            let active = r.size == Some(*p);
            let galley = ui.painter().layout_no_wrap(fmt_size(*p), egui::FontId::proportional(10.5), if active { t.accent_text } else { t.text_dim });
            let chip = Rect::from_min_size(pos2(right - galley.size().x - 8.0, rect.center().y - 8.0), vec2(galley.size().x + 8.0, 16.0));
            let cr = ui.interact(chip, resp.id.with(("preset", p.to_bits())), Sense::click()).on_hover_text(tl!("Use this size preset"));
            ui.painter().rect_filled(
                chip,
                8.0,
                if active {
                    t.accent_soft
                } else if cr.hovered() {
                    t.hover
                } else {
                    t.field
                },
            );
            ui.painter().galley(pos2(chip.left() + 4.0, chip.center().y - galley.size().y / 2.0), galley, Color32::PLACEHOLDER);
            if cr.clicked() {
                clicked_inner = true;
                actions.push(Action::Run("typer.style.setSizePreset", json!({"id": r.id, "size": p})));
            }
            right = chip.left() - 3.0;
        }
    } else if let Some(size) = r.size {
        let text = format!("{} {}", fmt_size(size), r.unit);
        let g = ui.painter().layout_no_wrap(text, egui::FontId::proportional(10.5), t.text_faint);
        ui.painter().galley(pos2(right - g.size().x, rect.center().y - g.size().y / 2.0), g.clone(), Color32::PLACEHOLDER);
        right -= g.size().x + 6.0;
    }
    if is_default {
        let star = Rect::from_center_size(pos2(right - 7.0, rect.center().y), vec2(12.0, 12.0));
        crate::icons::paint(ui, star, "star-filled", 11.0, t.text_dim);
        right -= 16.0;
    }
    // The name, in its font when that font is available.
    let name_rect = Rect::from_min_max(pos2(x, rect.top() + 2.0), pos2(right.max(x + 10.0), rect.bottom() - 2.0));
    let color = if selected { t.text } else { t.text_dim };
    let drawn = r.family.as_ref().is_some_and(|f| font_label::paint(ui, f, &r.name, 13.0, name_rect, color));
    if !drawn {
        ui.painter().with_clip_rect(name_rect).text(name_rect.left_center(), egui::Align2::LEFT_CENTER, &r.name, egui::FontId::proportional(12.5), color);
    }
    if resp.clicked() && !clicked_inner {
        actions.push(Action::Select(r.id.clone()));
    }
    if resp.double_clicked() && !clicked_inner {
        actions.push(Action::Edit(r.id.clone()));
    }
    resp.context_menu(|ui| {
        crate::widgets::style_spectrum_popup_menu(ui);
        if ui.button(tl!("Edit style…")).clicked() {
            actions.push(Action::Edit(r.id.clone()));
            ui.close();
        }
        if ui.button(tl!("Apply style to the current layer")).clicked() {
            actions.push(Action::Run("typer.apply", json!({"mode": "styleOnly", "style": {"id": r.id}, "advance": false})));
            ui.close();
        }
        ui.separator();
        if ui.button(tl!("Copy")).clicked() {
            actions.push(Action::Copy(r.id.clone()));
            ui.close();
        }
        if ui.add_enabled(has_clip, egui::Button::new(tl!("Paste style"))).clicked() {
            actions.push(Action::Paste(r.folder.as_ref().map(Id::to_value)));
            ui.close();
        }
        if ui.button(tl!("Duplicate")).clicked() {
            actions.push(Action::Run("typer.style.duplicate", json!({"id": r.id})));
            ui.close();
        }
        ui.menu_button(tl!("Move to folder"), |ui| {
            crate::widgets::style_spectrum_popup_menu(ui);
            if ui.add_enabled(r.folder.is_some(), egui::Button::new(tl!("Unsorted"))).clicked() {
                actions.push(Action::MoveStyle(r.id.clone(), None));
                ui.close();
            }
            for (id, label) in folders {
                let here = r.folder.as_ref().map(Id::to_value).as_ref() == Some(id);
                if ui.add_enabled(!here, egui::Button::new(label)).clicked() {
                    actions.push(Action::MoveStyle(r.id.clone(), Some(id.clone())));
                    ui.close();
                }
            }
        });
        ui.separator();
        if r.tag.is_some() && ui.button(if r.tags_off { tl!("Turn tags on") } else { tl!("Turn tags off") }).clicked() {
            actions.push(Action::Run("typer.style.togglePrefixes", json!({"id": r.id})));
            ui.close();
        }
        let default_label = if is_default { tl!("Remove as default style") } else { tl!("Make default style") };
        if ui.button(default_label).clicked() {
            actions.push(Action::Run("typer.settings.set", json!({"key": "defaultStyleId", "value": if is_default { Value::Null } else { r.id.clone() }})));
            ui.close();
        }
        ui.separator();
        if ui.button(tl!("Delete…")).clicked() {
            actions.push(Action::Ask(Confirm::DeleteStyle { id: r.id.clone(), name: r.name.clone() }));
            ui.close();
        }
    });
    if resp.hovered() {
        resp.on_hover_text(tl!("Click: make current · double-click: edit · right-click: more"));
    }
}

fn apply(app: &mut PhotocraftApp, a: Action) {
    match a {
        Action::Select(id) => act(app, "typer.style.select", json!({"id": id})),
        Action::Edit(id) => super::style_editor::open(app, &id),
        Action::Run(cmd, p) => {
            let p = if matches!(cmd, "typer.style.setSize" | "typer.style.setSizePreset") { with_now(app, p) } else { p };
            act(app, cmd, p);
        }
        Action::Copy(id) => {
            let style = Id::from_value(&id).and_then(|id| app.session.typer.state.library.style(&id).map(|s| s.to_json()));
            app.ui.typer.clipboard = style;
        }
        Action::Paste(folder) => {
            let Some(mut style) = app.ui.typer.clipboard.clone() else { return };
            if let Some(o) = style.as_object_mut() {
                o.remove("id");
                o.insert("folder".into(), folder.unwrap_or(Value::Null));
            }
            let p = with_now(app, json!({"style": style}));
            act(app, "typer.style.save", p);
        }
        Action::Ask(c) => app.ui.typer.confirm = Some(c),
        Action::NewStyle(folder) => {
            super::style_editor::open_new(app);
            if let Some(d) = app.ui.typer.style_editor.as_mut() {
                d.style["folder"] = folder.unwrap_or(Value::Null);
            }
        }
        Action::EditFolder(id) => super::folder_editor::open(app, &id),
        Action::NewFolder(parent) => super::folder_editor::open_new(app, parent),
        Action::ExportFolder(id, name) => export_folder(app, id, &name),
        Action::MoveStyle(id, folder) => act(app, "typer.style.reorder", json!({"id": id, "folder": folder})),
        Action::OrderFolders(parent, order) => act(app, "typer.folder.reorder", json!({"parentId": parent, "order": order})),
        Action::Toggle(key) => {
            let c = &mut app.ui.typer.collapsed;
            if let Some(i) = c.iter().position(|k| *k == key) {
                c.remove(i);
            } else {
                c.push(key);
            }
        }
    }
}

/// Folder export (TypeR's `{name, exportedStyles}`) saved as `<folder>.json`.
pub(super) fn export_folder(app: &mut PhotocraftApp, id: Value, name: &str) {
    let Ok(v) = super::run(app, "typer.library.exportFolder", json!({"id": id})) else { return };
    let text = serde_json::to_string_pretty(&v).unwrap_or_default();
    let file = format!("{}.json", super::settings::file_stem(name));
    let r = app.pick_save(&file, move |app, path| {
        let write = app.services.write.as_mut().ok_or("no writer configured")?;
        write(&path, text.as_bytes())?;
        Ok(Value::Null)
    });
    if let Err(e) = r {
        app.ui.typer.message = e;
    }
}
