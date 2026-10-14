//! The styles block (TypeR `stylesBlock.jsx`): the style library as a folder tree. Each style row
//! shows its text colour, its tag chip in the tag colour, its name drawn in its font and its size
//! or size presets, with a quick −/+ on hover. A click selects a style, a double-click edits it,
//! a right-click copies, pastes, duplicates, applies or deletes it. Folder headers collapse and
//! have their own menu (edit, add a style or subfolder, duplicate, export, delete).

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
    let last_id = egui::Id::new("typer-styles-last-current");
    let scroll_to_current = auto_scroll && ctx.data(|d| d.get_temp::<Option<String>>(last_id)).flatten() != current;
    ctx.data_mut(|d| d.insert_temp(last_id, current.clone()));
    egui::ScrollArea::vertical().id_salt("typer-styles").min_scrolled_height(height).max_height(height).auto_shrink([false, false]).show(ui, |ui| {
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
            header(ui, &t, None, tl!("Unsorted"), 0, unsorted.len(), has_current, open, has_clip, &mut actions);
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
            header(ui, &t, Some((&f.id.to_value(), &key)), &f.name, f.depth, members.len(), has_current, open, has_clip, &mut actions);
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
                    &mut actions,
                );
            }
        }
    });
    for a in actions {
        apply(app, a);
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
    actions: &mut Vec<Action>,
) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_H), Sense::click());
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
    actions: &mut Vec<Action>,
) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &r.name));
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
