//! The tab bar (TypeR 3.0's work tabs, `typer.tab.*`): one tab per chapter or series, each with
//! its own script, page files and cursor. A click switches, a double-click renames, `+` adds a
//! tab, and the right-click menu renames or closes one. Closing a tab that has a script or page
//! files asks first; the last tab can't be closed; a dot marks tabs with page files. Hidden when
//! Settings › Behavior › Work tabs is off (`multiTabEnabled`).

use egui::{RichText, Sense, Stroke, StrokeKind, vec2};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Confirm, act};
use crate::PhotocraftApp;
use crate::theme::Tokens;

/// A tab being renamed in place.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Rename {
    pub id: String,
    pub name: String,
}

/// A tab's name as shown: TypeR's default names ("Tab 3") in the UI language.
pub(crate) fn display_name(name: &str) -> String {
    match name.strip_prefix("Tab ").filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
        Some(n) => format!("{} {n}", tl!("Tab")),
        None => name.to_string(),
    }
}

/// Close tab `id`: at once when it is empty, else after asking.
pub(crate) fn close(app: &mut PhotocraftApp, id: &str) {
    let st = &app.session.typer.state;
    let Some(tab) = st.tab_views().into_iter().find(|t| t.id == id) else { return };
    if st.tabs.len() <= 1 {
        app.ui.typer.message = tl!("The last tab can't be closed.").into();
        return;
    }
    if tab.has_content() {
        app.ui.typer.confirm = Some(Confirm::CloseTab { id: id.to_string(), name: display_name(&tab.name) });
    } else {
        act(app, "typer.tab.close", json!({"id": id}));
    }
}

/// Add a tab named "Tab N" in the UI language.
fn add(app: &mut PhotocraftApp) {
    let n = app.session.typer.state.tabs.len().max(1) + 1;
    act(app, "typer.tab.new", json!({"name": format!("{} {n}", tl!("Tab"))}));
}

pub(super) fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let st = &app.session.typer.state;
    if !st.settings.multi_tab_enabled {
        return;
    }
    let t = Tokens::get(ui.ctx());
    // Before the first tab command the workspace has one implicit tab.
    let tabs: Vec<(String, String, bool, bool)> = if st.tabs.is_empty() {
        vec![(String::new(), "Tab 1".into(), true, !st.images.is_empty())]
    } else {
        st.tab_views().into_iter().map(|v| (v.id.clone(), v.name.clone(), v.id == st.current_tab_id, !v.images.is_empty())).collect()
    };
    let mut action: Option<Action> = None;
    let editing = app.ui.typer.renaming.clone();
    ui.horizontal(|ui| {
        egui::ScrollArea::horizontal().id_salt("typer-tabs").max_width((ui.available_width() - 26.0).max(40.0)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                for (id, name, current, synced) in &tabs {
                    if let Some(r) = editing.as_ref().filter(|r| &r.id == id) {
                        let mut draft = r.name.clone();
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut draft)
                                .id(egui::Id::unique("typer-tab-rename"))
                                .desired_width(90.0)
                                .font(egui::FontId::proportional(11.5)),
                        );
                        if !resp.has_focus() && !resp.lost_focus() {
                            resp.request_focus();
                        }
                        action = if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            Some(Action::StopRenaming)
                        } else if resp.lost_focus() {
                            Some(Action::Rename(id.clone(), draft))
                        } else {
                            Some(Action::Edit(draft))
                        };
                        continue;
                    }
                    let label = display_name(name);
                    let resp = pill(ui, &t, &label, *current, *synced);
                    if resp.double_clicked() {
                        action = Some(Action::StartRenaming(id.clone(), label.clone()));
                    } else if resp.clicked() && !current {
                        action = Some(Action::Switch(id.clone()));
                    }
                    resp.context_menu(|ui| {
                        if ui.button(tl!("Rename tab")).clicked() {
                            action = Some(Action::StartRenaming(id.clone(), label.clone()));
                            ui.close();
                        }
                        if ui.add_enabled(tabs.len() > 1, egui::Button::new(tl!("Close tab"))).clicked() {
                            action = Some(Action::Close(id.clone()));
                            ui.close();
                        }
                    });
                }
            });
        });
        if crate::icons::button_with_icon_size(ui, "plus", 20.0, 13.0, false, tl!("New tab")).clicked() {
            action = Some(Action::Add);
        }
    });
    match action {
        Some(Action::Switch(id)) => act(app, "typer.tab.switch", json!({"id": id})),
        Some(Action::StartRenaming(mut id, name)) => {
            // The implicit first tab gets its id first.
            if id.is_empty() {
                app.session.typer.state.ensure_tabs();
                id.clone_from(&app.session.typer.state.current_tab_id);
            }
            app.ui.typer.renaming = Some(Rename { id, name });
        }
        Some(Action::Edit(name)) => {
            if let Some(r) = app.ui.typer.renaming.as_mut() {
                r.name = name;
            }
        }
        Some(Action::Rename(id, name)) => {
            app.ui.typer.renaming = None;
            let old = tabs.iter().find(|(i, ..)| *i == id).map(|(_, n, ..)| display_name(n)).unwrap_or_default();
            if name.trim() != old {
                act(app, "typer.tab.rename", json!({"id": id, "name": name}));
            }
        }
        Some(Action::StopRenaming) => app.ui.typer.renaming = None,
        Some(Action::Close(id)) => close(app, &id),
        Some(Action::Add) => add(app),
        None => {}
    }
}

/// What the tab bar was asked to do this frame.
enum Action {
    Switch(String),
    StartRenaming(String, String),
    Edit(String),
    Rename(String, String),
    StopRenaming,
    Close(String),
    Add,
}

/// One tab: its name, bold and on the accent when current, with a dot when it has page files.
fn pill(ui: &mut egui::Ui, t: &Tokens, label: &str, current: bool, synced: bool) -> egui::Response {
    let text = RichText::new(label).size(11.5).color(if current { t.accent_text } else { t.text_dim });
    let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Truncate), 140.0, egui::TextStyle::Body);
    let dot = if synced { 9.0 } else { 0.0 };
    let size = vec2(galley.size().x + 14.0 + dot, 20.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if current {
        t.accent_soft
    } else if resp.hovered() {
        t.hover
    } else {
        t.field
    };
    ui.painter().rect_filled(rect, t.radius_sm, fill);
    ui.painter().rect_stroke(rect, t.radius_sm, Stroke::new(1.0, if current { t.accent_border } else { t.field_border }), StrokeKind::Inside);
    if synced {
        ui.painter().circle_filled(egui::pos2(rect.left() + 8.0, rect.center().y), 3.0, t.accent);
    }
    let fg = if current { t.accent_text } else { t.text_dim };
    ui.painter().galley(egui::pos2(rect.left() + 7.0 + dot, rect.center().y - galley.size().y / 2.0), galley, fg);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, current, label));
    resp.on_hover_text(tl!("Click: switch · double-click: rename · right-click: more"))
}
