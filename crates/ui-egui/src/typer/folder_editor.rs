//! The folder editor (TypeR `editFolder.jsx`): name, parent folder and member styles, saved
//! with `typer.folder.save`; Delete asks first (`typer.folder.delete`).

use egui::RichText;
use photocraft_typer::Id;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Confirm, dialog_frame};
use crate::PhotocraftApp;
use crate::theme::Tokens;

/// The folder being edited.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FolderDraft {
    /// `None` for a new folder.
    pub id: Option<Value>,
    pub name: String,
    pub parent: Option<Value>,
    /// Ids of the styles in the folder.
    pub styles: Vec<Value>,
    pub message: String,
}

pub fn open(app: &mut PhotocraftApp, id: &Value) {
    let lib = &app.session.typer.state.library;
    let Some(f) = Id::from_value(id).and_then(|id| lib.folder(&id)) else { return };
    let styles = lib.styles.iter().filter(|s| s.folder.as_ref() == Some(&f.id)).map(|s| s.id.to_value()).collect();
    app.ui.typer.folder_editor =
        Some(FolderDraft { id: Some(f.id.to_value()), name: f.name.clone(), parent: f.parent_id.as_ref().map(Id::to_value), styles, message: String::new() });
}

pub fn open_new(app: &mut PhotocraftApp, parent: Option<Value>) {
    app.ui.typer.folder_editor = Some(FolderDraft { parent, ..Default::default() });
}

pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.typer.folder_editor.clone() else { return };
    let t = Tokens::get(ctx);
    let lib = &app.session.typer.state.library;
    let own = d.id.as_ref().and_then(Id::from_value);
    // A folder can't move under itself or its subfolders.
    let excluded: Vec<Id> = own.iter().flat_map(|id| std::iter::once(id.clone()).chain(lib.descendants(id))).collect();
    let mut parents: Vec<(String, String)> = vec![(String::new(), tl!("No parent (top level)").to_string())];
    parents.extend(lib.flatten_tree().into_iter().filter(|f| !excluded.contains(&f.id)).map(|f| (f.id.to_string(), f.label)));
    let folder_ids: Vec<(String, Value)> = lib.folders.iter().map(|f| (f.id.to_string(), f.id.to_value())).collect();
    let styles: Vec<(Value, String)> = lib.styles.iter().map(|s| (s.id.to_value(), s.name.clone())).collect();
    let (mut close, mut save, mut delete) = (false, false, false);
    let title = if d.id.is_none() { tl!("New folder") } else { tl!("Edit folder") };
    egui::Window::new(title)
        .id(egui::Id::unique("typer-folder-editor"))
        .title_bar(false)
        .resizable(false)
        .default_width(320.0)
        .max_width(360.0)
        .frame(dialog_frame(&t))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -40.0))
        .show(ctx, |ui| {
            ui.set_width(320.0);
            ui.label(RichText::new(title).color(t.text).size(13.0).strong());
            crate::widgets::hairline(ui);
            egui::Grid::new("typer-folder-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label(RichText::new(tl!("Folder name")).color(t.text_dim).size(11.5));
                ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(200.0));
                ui.end_row();
                ui.label(RichText::new(tl!("Parent folder")).color(t.text_dim).size(11.5));
                let mut cur = d.parent.as_ref().and_then(Id::from_value).map_or(String::new(), |i| i.to_string());
                let o: Vec<(String, &str)> = parents.iter().map(|(k, l)| (k.clone(), l.as_str())).collect();
                if crate::widgets::dropdown(ui, "typer-folder-parent", "", &mut cur, &o, 200.0) {
                    d.parent = folder_ids.iter().find(|(k, _)| *k == cur).map(|(_, v)| v.clone());
                }
                ui.end_row();
            });
            ui.add_space(4.0);
            ui.label(RichText::new(tl!("Styles in folder")).color(t.text_dim).size(11.5));
            egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                if styles.is_empty() {
                    ui.label(RichText::new(tl!("No styles yet")).color(t.text_faint));
                }
                for (id, name) in &styles {
                    let mut on = d.styles.contains(id);
                    if crate::widgets::checkbox(ui, &mut on, name).changed() {
                        if on {
                            d.styles.push(id.clone());
                        } else {
                            d.styles.retain(|s| s != id);
                        }
                    }
                }
            });
            if !d.message.is_empty() {
                ui.label(RichText::new(&d.message).color(t.warning).size(11.5));
            }
            ui.add_space(6.0);
            // Align::Min: a centred row would keep last frame's height (see `dialogs`).
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                use crate::widgets::{ButtonRole, DialogButton};
                let named = !d.name.trim().is_empty();
                match crate::widgets::dialog_buttons(
                    ui,
                    &[DialogButton::new(ButtonRole::Default, tl!("Save"), 80.0).enabled(named), DialogButton::new(ButtonRole::Cancel, tl!("Cancel"), 80.0)],
                ) {
                    Some(ButtonRole::Default) => save = true,
                    Some(ButtonRole::Cancel) => close = true,
                    _ => {}
                }
                if d.id.is_some() && crate::widgets::secondary_button(ui, tl!("Delete"), 70.0).clicked() {
                    delete = true;
                }
            });
        });
    if close {
        app.ui.typer.folder_editor = None;
        return;
    }
    if delete && let Some(id) = d.id.clone() {
        app.ui.typer.confirm = Some(Confirm::DeleteFolder { id, name: d.name.clone(), permanent: false });
    }
    if save {
        let mut folder = json!({"name": d.name.trim(), "parentId": d.parent.clone().unwrap_or(Value::Null)});
        if let Some(id) = &d.id {
            folder["id"] = id.clone();
        }
        match app.run("typer.folder.save", json!({"folder": folder, "styleIds": d.styles})) {
            Ok(_) => {
                app.ui.typer.folder_editor = None;
                return;
            }
            Err(e) => d.message = e,
        }
    }
    if app.ui.typer.folder_editor.is_some() {
        app.ui.typer.folder_editor = Some(d);
    }
}
