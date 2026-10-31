//! The font menu's Custom tab: the user's own fonts in their folders (one level of nesting), the
//! font projects as folders of their fonts (with each font's role), and the fonts in no folder
//! ("Unfiled"). Folders are the engine's (`type.fontFolder.*`, kept in the preferences); this is
//! their UI: New Folder, a folder's Rename / New Subfolder / Delete, a font row's star, and moving
//! a font: dragged onto a folder (mouse), or "Move to…" from its menu (right-click, or press and
//! hold on a phone), which works from every tab.

use std::collections::HashSet;

use egui::{Align2, Color32, Id, Rect, RichText, Sense, Stroke, pos2, vec2};
use photocraft_engine::font_projects::Projects;
use photocraft_engine::prefs::FontFolder;
use serde_json::json;

use crate::PhotocraftApp;
use crate::theme::Tokens;

/// Below this width (points) the menu is a phone's: rows a finger hits (48 pt).
pub const PHONE_WIDTH: f32 = 600.0;
/// A row's height on a phone.
pub const PHONE_ROW: f32 = 48.0;
/// A row's height with a mouse.
const ROW: f32 = 26.0;
/// The list's height at most (the font menu's).
const LIST_HEIGHT: f32 = 440.0;
/// How far a subfolder (and a folder's fonts) is indented.
const INDENT: f32 = 18.0;

/// Is the window a phone's (the font menu's rows grow to [`PHONE_ROW`])?
pub fn phone(ctx: &egui::Context) -> bool {
    ctx.content_rect().width() < PHONE_WIDTH
}

/// The Custom tab's own state, between frames.
#[derive(Clone, Debug, Default)]
struct TabState {
    /// Folders shown closed (all open by default).
    collapsed: Vec<String>,
    /// The New Folder field: its parent (`None`: top level) and text.
    creating: Option<(Option<String>, String)>,
    /// The folder being renamed and the field's text.
    renaming: Option<(String, String)>,
    /// The folder whose actions (Rename, New Subfolder, Delete) show under it.
    actions: Option<String>,
    /// A field just opened: it takes the keyboard focus once.
    focus: bool,
}

fn state_id() -> Id {
    Id::unique("font-folders-tab")
}

fn moving_id() -> Id {
    Id::unique("font-folder-moving")
}

/// A font row's right-click (or press and hold): "Move to…" for that font, shown at the top of
/// the menu by [`move_panel`].
pub fn row_menu(response: &egui::Response, family: &str) {
    if response.secondary_clicked() || response.long_touched() {
        response.ctx.data_mut(|d| d.insert_temp(moving_id(), family.to_string()));
    }
    // A press held still on the row (a finger: the menu takes a release over a row as its pick,
    // and the touch router keeps egui's own long touch from showing): "Move to…", and the release
    // that ends the hold is no pick.
    if response.is_pointer_button_down_on() && response.ctx.input(|i| i.pointer.any_pressed()) {
        // A new press: an earlier hold whose release picked nothing is over.
        response.ctx.data_mut(|d| d.remove::<String>(held_id()));
    }
    // Phones only: with a mouse, a press that rests before it moves is the start of a drag.
    let held = response.long_touched()
        || (phone(&response.ctx)
            && response.is_pointer_button_down_on()
            && response.ctx.input(|i| {
                let still = match (i.pointer.press_origin(), i.pointer.latest_pos()) {
                    (Some(a), Some(b)) => a.distance(b) < HOLD_SLOP,
                    _ => false,
                };
                still && i.pointer.press_start_time().is_some_and(|t| i.time - t >= HOLD_S)
            }));
    if held && response.ctx.data(|d| d.get_temp::<String>(held_id())).as_deref() != Some(family) {
        response.ctx.data_mut(|d| {
            d.insert_temp(moving_id(), family.to_string());
            d.insert_temp(held_id(), family.to_string());
        });
    } else if response.is_pointer_button_down_on() && phone(&response.ctx) {
        response.ctx.request_repaint_after(std::time::Duration::from_millis(80));
    }
}

/// How long a press rests on a font row before it is a press and hold (seconds).
const HOLD_S: f64 = 0.55;
/// How far (points) a held press may wander and still be one.
const HOLD_SLOP: f32 = 10.0;

fn held_id() -> Id {
    Id::unique("font-folder-held")
}

/// Is this pick of `family` the release of a press and hold on its row (which asked for "Move
/// to…", not for the font)? Answers once.
pub fn swallow_pick(ctx: &egui::Context, family: &str) -> bool {
    let held = ctx.data(|d| d.get_temp::<String>(held_id())).is_some_and(|f| f == family);
    if held {
        ctx.data_mut(|d| d.remove::<String>(held_id()));
    }
    held
}

/// What a click in the tab asks for, done after the list is drawn.
enum Action {
    Create(Option<String>, String),
    Rename(String, String),
    Delete(String),
    Move(String, Option<String>),
    Star(String),
}

fn run(app: &mut PhotocraftApp, action: Action) {
    let (id, p) = match action {
        Action::Create(parent, name) => ("type.fontFolder.create", json!({"name": name, "parent": parent})),
        Action::Rename(name, to) => ("type.fontFolder.rename", json!({"name": name, "to": to})),
        Action::Delete(name) => ("type.fontFolder.delete", json!({"name": name})),
        Action::Move(family, folder) => ("type.fontFolder.move", json!({"family": family, "folder": folder})),
        Action::Star(family) => ("type.font.favorite", json!({"family": family})),
    };
    if let Err(e) = app.run(id, p) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// The folders in display order: each top-level folder, then its subfolders, with their depth.
fn ordered(folders: &[FontFolder]) -> Vec<(&FontFolder, usize)> {
    let mut v = Vec::new();
    for top in folders.iter().filter(|f| f.parent.as_ref().is_none_or(|p| !folders.iter().any(|x| x.name == *p))) {
        v.push((top, 0));
        for sub in folders.iter().filter(|f| f.parent.as_deref() == Some(top.name.as_str())) {
            v.push((sub, 1));
        }
    }
    v
}

/// "Move {font} to…": the folders, Unfiled and Cancel, at the top of the menu while a row's
/// "Move to…" is pending ([`row_menu`]). Any tab but Find More.
pub fn move_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some(family) = ui.data(|d| d.get_temp::<String>(moving_id())) else { return };
    let t = Tokens::get(ui.ctx());
    let folders = app.session.prefs().type_.font_folders.clone();
    let current = folders.iter().find(|f| f.fonts.contains(&family)).map(|f| f.name.clone());
    let h = if phone(ui.ctx()) { 40.0 } else { 22.0 };
    let mut action = None;
    let mut done = false;
    egui::Frame::group(ui.style()).fill(t.accent_soft).show(ui, |ui| {
        ui.label(RichText::new(crate::i18n::fmt(tl!("Move {font} to…"), &[("font", &family)])).strong());
        ui.horizontal_wrapped(|ui| {
            for (f, depth) in ordered(&folders) {
                let label = if depth > 0 { format!("↳ {}", f.name) } else { f.name.clone() };
                if ui.add(egui::Button::selectable(current.as_deref() == Some(f.name.as_str()), label).min_size(vec2(0.0, h))).clicked() {
                    action = Some(Action::Move(family.clone(), Some(f.name.clone())));
                }
            }
            if ui.add(egui::Button::selectable(current.is_none(), tl!("Unfiled")).min_size(vec2(0.0, h))).clicked() {
                action = Some(Action::Move(family.clone(), None));
            }
            if ui.add(egui::Button::new(tl!("New Folder…")).min_size(vec2(0.0, h))).clicked() {
                ui.data_mut(|d| {
                    let st = d.get_temp_mut_or_default::<TabState>(state_id());
                    st.creating = Some((None, String::new()));
                    st.focus = true;
                });
                app.ui.fonts.tab = crate::fonts_ui::PickerTab::Custom;
            }
            if ui.add(egui::Button::new(tl!("Cancel")).min_size(vec2(0.0, h))).clicked() {
                done = true;
            }
        });
        if folders.is_empty() {
            ui.add(egui::Label::new(RichText::new(tl!("No folders yet: make one with New Folder…")).color(t.text_faint).size(11.0)).wrap());
        }
    });
    if let Some(a) = action {
        run(app, a);
        done = true;
    }
    if done {
        ui.data_mut(|d| d.remove::<String>(moving_id()));
    }
}

/// One font row: star, name, a role's label at the right. `drag`: a mouse drag carries the
/// family to a folder.
struct Row<'a> {
    family: &'a str,
    role: Option<String>,
    on: bool,
    favorite: bool,
    indent: f32,
}

/// Draws `row`; returns (picked, star clicked).
fn font_row(ui: &mut egui::Ui, row: &Row<'_>, width: f32, h: f32, drag: bool) -> (bool, bool) {
    let t = Tokens::get(ui.ctx());
    let sense = if drag { Sense::click_and_drag() } else { Sense::click() };
    let (rect, resp) = ui.allocate_exact_size(vec2(width, h), sense);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, row.on, row.family));
    if row.on || resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, if row.on { t.accent } else { t.hover });
    }
    let star = Rect::from_min_size(pos2(rect.left() + row.indent, rect.top()), vec2(h.min(32.0), h));
    let star_resp = ui.interact(star, resp.id.with("star"), Sense::click());
    let tip = if row.favorite { tl!("Remove from Favorites") } else { tl!("Add to Favorites") };
    let star_resp = star_resp.on_hover_text(tip);
    star_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
    let tint = if row.favorite { Color32::from_rgb(0xf5, 0xc5, 0x18) } else if row.on { Color32::WHITE } else { t.text_dim };
    crate::icons::paint(ui, star, if row.favorite { "star-filled" } else { "star" }, 16.0, tint);
    let text = if row.on { Color32::WHITE } else { t.text };
    let x = star.right() + 4.0;
    let mut right = rect.right() - 8.0;
    if let Some(role) = &row.role {
        let g = ui.painter().layout_no_wrap(role.clone(), egui::FontId::proportional(11.0), if row.on { Color32::WHITE } else { t.text_faint });
        right -= g.size().x;
        ui.painter().galley(pos2(right, rect.center().y - g.size().y / 2.0), g, Color32::WHITE);
        right -= 8.0;
    }
    let clip = Rect::from_min_max(pos2(x, rect.top()), pos2(right.max(x), rect.bottom()));
    ui.painter().with_clip_rect(clip).text(pos2(x, rect.center().y), Align2::LEFT_CENTER, row.family, egui::FontId::proportional(13.5), text);
    if drag && resp.drag_started() {
        resp.dnd_set_drag_payload(row.family.to_string());
    }
    row_menu(&resp, row.family);
    let picked = resp.clicked() && !star_resp.hovered() && !swallow_pick(ui.ctx(), row.family);
    (picked, star_resp.clicked())
}

/// A folder's header row: open/closed, its name (or the rename field), its count, and its
/// actions button. Returns the response a dragged font drops on.
#[allow(clippy::too_many_arguments)]
fn folder_header(
    ui: &mut egui::Ui,
    st: &mut TabState,
    name: &str,
    label: &str,
    count: usize,
    indent: f32,
    width: f32,
    h: f32,
    editable: bool,
    actions: &mut Vec<Action>,
) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(width, h), Sense::click());
    let open = !st.collapsed.iter().any(|c| c == name);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let hovering_drop = resp.dnd_hover_payload::<String>().is_some();
    if hovering_drop {
        ui.painter().rect(rect, 4.0, t.accent_soft, Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, t.hover);
    }
    let chevron = Rect::from_min_size(pos2(rect.left() + indent, rect.top()), vec2(16.0, h));
    crate::icons::paint(ui, chevron, if open { "chevron-down" } else { "chevron-right" }, 14.0, t.text_dim);
    let icon = Rect::from_min_size(pos2(chevron.right(), rect.top()), vec2(20.0, h));
    crate::icons::paint(ui, icon, if open { "folder-open" } else { "folder" }, 15.0, t.text_dim);
    let x = icon.right() + 4.0;
    let more = Rect::from_min_size(pos2(rect.right() - h.min(32.0), rect.top()), vec2(h.min(32.0), h));
    let renaming = st.renaming.as_ref().is_some_and(|(n, _)| n == name);
    if renaming {
        let field = Rect::from_min_max(pos2(x, rect.top() + 2.0), pos2(more.left() - 4.0, rect.bottom() - 2.0));
        // Enter renames, Esc cancels; clicking elsewhere leaves the field as it is.
        let focus = std::mem::take(&mut st.focus);
        let mut done = None;
        if let Some((_, text)) = st.renaming.as_mut() {
            let r = ui.put(field, egui::TextEdit::singleline(text).id(Id::unique(("font-folder-rename", name))).hint_text(tl!("Folder name")));
            if focus {
                r.request_focus();
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                done = Some(false);
            } else if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                done = Some(true);
            }
        }
        if let Some(apply) = done
            && let Some((n, text)) = st.renaming.take()
            && apply
            && !text.trim().is_empty()
            && text.trim() != n
        {
            actions.push(Action::Rename(n, text.trim().to_string()));
        }
    } else {
        let g = ui.painter().layout_no_wrap(format!("{label}  ({count})"), crate::theme::medium(13.0), t.text);
        ui.painter().with_clip_rect(Rect::from_min_max(pos2(x, rect.top()), pos2(more.left(), rect.bottom()))).galley(
            pos2(x, rect.center().y - g.size().y / 2.0),
            g,
            t.text,
        );
    }
    if editable {
        let r = ui.interact(more, resp.id.with("more"), Sense::click()).on_hover_text(tl!("Folder actions"));
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{} {label}", tl!("Folder actions"))));
        crate::icons::paint(ui, more, "ellipsis", 16.0, t.text_dim);
        if r.clicked() {
            st.actions = if st.actions.as_deref() == Some(name) { None } else { Some(name.to_string()) };
        } else if resp.clicked() && !renaming {
            toggle(st, name);
        }
        if resp.secondary_clicked() || resp.long_touched() {
            st.actions = Some(name.to_string());
        }
    } else if resp.clicked() {
        toggle(st, name);
    }
    resp
}

fn toggle(st: &mut TabState, name: &str) {
    if let Some(i) = st.collapsed.iter().position(|c| c == name) {
        st.collapsed.remove(i);
    } else {
        st.collapsed.push(name.to_string());
    }
}

/// A faint hint under a folder, `indent` in, wrapped to the list's width (a phone's is narrow).
fn hint_line(ui: &mut egui::Ui, indent: f32, text: &str, color: Color32) {
    egui::Frame::NONE.inner_margin(egui::Margin { left: indent.min(120.0) as i8, ..Default::default() }).show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(text).color(color).size(11.0)).wrap());
    });
}

/// The New Folder field (top level or in `parent`).
fn create_field(ui: &mut egui::Ui, st: &mut TabState, width: f32, h: f32, actions: &mut Vec<Action>) {
    let focus = std::mem::take(&mut st.focus);
    let Some((parent, text)) = st.creating.as_mut() else { return };
    let mut done = None;
    ui.horizontal(|ui| {
        let (icon, _) = ui.allocate_exact_size(vec2(20.0, h), Sense::hover());
        crate::icons::paint(ui, icon, "folder-plus", 15.0, Tokens::get(ui.ctx()).text_dim);
        // Its own id: the rows above it change (a folder's actions close), and an id counted from
        // them would drop the focus.
        let id = Id::unique(("font-folder-new", parent.clone()));
        let r = ui.add_sized([(width - 170.0).max(80.0), h - 4.0], egui::TextEdit::singleline(text).id(id).hint_text(tl!("Folder name")));
        if focus {
            r.request_focus();
        }
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.add(egui::Button::new(tl!("Create")).min_size(vec2(0.0, h - 4.0))).clicked() || enter {
            done = Some(true);
        }
        if ui.add(egui::Button::new(tl!("Cancel")).min_size(vec2(0.0, h - 4.0))).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            done = Some(false);
        }
    });
    if let Some(apply) = done {
        let (parent, text) = (parent.clone(), text.trim().to_string());
        st.creating = None;
        if apply && !text.is_empty() {
            actions.push(Action::Create(parent, text));
        }
    }
}

/// The Custom tab, while nothing is typed in the font field (a search lists every font). A
/// font clicked: its family, and its role when it was picked from the active project's folder.
pub fn custom_tab(app: &mut PhotocraftApp, ui: &mut egui::Ui, current: &str) -> Option<(String, Option<String>)> {
    let t = Tokens::get(ui.ctx());
    let phone = phone(ui.ctx());
    let h = if phone { PHONE_ROW } else { ROW };
    let width = crate::type_tool::phone_menu_width(ui.ctx()).map_or(420.0, |w| w - 14.0);
    let mut st: TabState = ui.data(|d| d.get_temp(state_id())).unwrap_or_default();
    let folders = app.session.prefs().type_.font_folders.clone();
    let favorites = photocraft_engine::font_library_cmds::favorite_fonts(&app.session);
    let families = crate::type_tool::families();
    let user = crate::type_tool::user_family_set();
    let filed: HashSet<&str> = folders.iter().flat_map(|f| f.fonts.iter().map(String::as_str)).collect();
    let unfiled: Vec<&String> = families.iter().filter(|f| user.contains(&f.to_lowercase()) && !filed.contains(f.as_str())).collect();
    let projects = Projects::of(&app.session);
    let mut actions = Vec::new();
    let mut picked = None;

    ui.horizontal(|ui| {
        if ui.add(egui::Button::new(tl!("New Folder…")).min_size(vec2(0.0, h.min(36.0)))).clicked() {
            st.creating = Some((None, String::new()));
            st.focus = true;
        }
    });
    if st.creating.as_ref().is_some_and(|(p, _)| p.is_none()) {
        create_field(ui, &mut st, width, h, &mut actions);
    }
    let room = ui.ctx().content_rect().bottom() - ui.cursor().top() - 16.0;
    let height = LIST_HEIGHT.min(room).max(h * 3.0);
    egui::ScrollArea::vertical().id_salt("font-folders-list").max_height(height).auto_shrink([false, true]).show(ui, |ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 1.0;
        let mut row = |ui: &mut egui::Ui, family: &str, role: Option<String>, indent: f32, role_id: Option<String>, actions: &mut Vec<Action>| {
            let r = Row { family, role, on: family == current, favorite: favorites.iter().any(|f| f == family), indent };
            let (clicked, star) = font_row(ui, &r, width, h, !phone);
            if star {
                actions.push(Action::Star(family.to_string()));
            } else if clicked {
                picked = Some((family.to_string(), role_id));
            }
        };
        for (f, depth) in ordered(&folders) {
            let parent_closed = depth > 0 && f.parent.as_ref().is_some_and(|p| st.collapsed.contains(p));
            if parent_closed {
                continue;
            }
            let indent = depth as f32 * INDENT;
            let header = folder_header(ui, &mut st, &f.name, &f.name, f.fonts.len(), indent, width, h, true, &mut actions);
            if let Some(family) = header.dnd_release_payload::<String>() {
                actions.push(Action::Move((*family).clone(), Some(f.name.clone())));
            }
            if st.actions.as_deref() == Some(f.name.as_str()) {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(indent + 34.0);
                    let bh = h.min(40.0) - 4.0;
                    if ui.add(egui::Button::new(tl!("Rename…")).min_size(vec2(0.0, bh))).clicked() {
                        st.renaming = Some((f.name.clone(), f.name.clone()));
                        st.actions = None;
                        st.focus = true;
                    }
                    if depth == 0 && ui.add(egui::Button::new(tl!("New Subfolder…")).min_size(vec2(0.0, bh))).clicked() {
                        st.creating = Some((Some(f.name.clone()), String::new()));
                        st.actions = None;
                        st.focus = true;
                        st.collapsed.retain(|c| *c != f.name);
                    }
                    let delete = ui.add(egui::Button::new(tl!("Delete Folder")).min_size(vec2(0.0, bh))).on_hover_text(tl!("Its fonts stay, unfiled"));
                    if delete.clicked() {
                        actions.push(Action::Delete(f.name.clone()));
                        st.actions = None;
                    }
                });
            }
            if st.creating.as_ref().is_some_and(|(p, _)| p.as_deref() == Some(f.name.as_str())) {
                ui.horizontal(|ui| {
                    ui.add_space(indent + INDENT);
                    create_field(ui, &mut st, width - indent - INDENT, h, &mut actions);
                });
            }
            if st.collapsed.contains(&f.name) {
                continue;
            }
            for fam in &f.fonts {
                row(ui, fam, None, indent + INDENT, None, &mut actions);
            }
            if f.fonts.is_empty() && !folders.iter().any(|x| x.parent.as_deref() == Some(f.name.as_str())) {
                let hint = if phone { tl!("Empty: press and hold a font, then Move to…") } else { tl!("Empty: drag a font here, or right-click it › Move to…") };
                hint_line(ui, indent + INDENT + 8.0, hint, t.text_faint);
            }
        }
        // The font projects, as folders of their fonts (with each font's role). A project
        // imported from a .capiproj file is one more.
        for p in &projects.projects {
            let key = format!("\u{1}project:{}", p.id);
            let active = projects.active.as_deref() == Some(p.id.as_str());
            let label = crate::i18n::fmt(tl!("{name} (project)"), &[("name", &p.name)]);
            folder_header(ui, &mut st, &key, &label, p.fonts.len(), 0.0, width, h, false, &mut actions);
            if st.collapsed.contains(&key) {
                continue;
            }
            for f in &p.fonts {
                let role = crate::font_projects_ui::role_name(p, &f.role);
                row(ui, &f.family, Some(role), INDENT, active.then(|| f.role.clone()), &mut actions);
            }
        }
        // The user's fonts in no folder.
        let key = "\u{1}unfiled";
        let header = folder_header(ui, &mut st, key, tl!("Unfiled"), unfiled.len(), 0.0, width, h, false, &mut actions);
        if let Some(family) = header.dnd_release_payload::<String>() {
            actions.push(Action::Move((*family).clone(), None));
        }
        if !st.collapsed.iter().any(|c| c == key) {
            for fam in &unfiled {
                row(ui, fam, None, INDENT, None, &mut actions);
            }
            if unfiled.is_empty() {
                hint_line(ui, INDENT + 8.0, tl!("Your own fonts (Load Fonts…) that are in no folder show here."), t.text_faint);
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(state_id(), st));
    for a in actions {
        run(app, a);
    }
    picked
}

#[cfg(test)]
#[path = "font_folders_ui_tests.rs"]
mod tests;
