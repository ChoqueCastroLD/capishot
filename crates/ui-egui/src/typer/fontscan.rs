//! FontScanR (TypeR 3.0, `typer.fontScan`): scan the open documents or chosen PSD files for the
//! fonts their text layers use, then import the chosen ones as styles into a new folder, an
//! existing folder or Unsorted. Each proposal shows its most-used size, how many layers and
//! files use it, and whether its font is loaded (else a link to Type › Load Fonts…). PSD files
//! are read without decoding their pixels (`typer_import_cmds::scan_psd`).

use egui::RichText;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{dialog_frame, with_now};
use crate::PhotocraftApp;
use crate::theme::Tokens;

/// Where the imported styles go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Dest {
    #[default]
    NewFolder,
    Existing,
    Unsorted,
}

/// The open FontScanR dialog.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FontScanDraft {
    /// `typer.fontScan`'s groups, once scanned.
    pub groups: Vec<Value>,
    /// Chosen groups (by key) and their names as edited.
    pub selected: Vec<String>,
    pub names: Vec<String>,
    pub files: usize,
    pub layers: usize,
    pub scanned: bool,
    pub dest: Dest,
    pub new_folder: String,
    pub folder: Option<Value>,
    /// Files that couldn't be read.
    pub errors: Vec<String>,
    pub message: String,
}

pub fn open(app: &mut PhotocraftApp) {
    app.ui.typer.fontscan = if app.ui.typer.fontscan.is_some() { None } else { Some(FontScanDraft::default()) };
}

/// Take `typer.fontScan`'s result into the dialog.
fn take(app: &mut PhotocraftApp, r: Result<Value, String>, errors: Vec<String>) {
    let Some(d) = app.ui.typer.fontscan.as_mut() else { return };
    match r {
        Ok(v) => {
            d.groups = v["groups"].as_array().cloned().unwrap_or_default();
            d.selected = d.groups.iter().filter_map(|g| g["key"].as_str().map(str::to_string)).collect();
            d.names = d.groups.iter().map(|g| g["defaultName"].as_str().unwrap_or_default().to_string()).collect();
            d.files = v["files"].as_u64().unwrap_or(0) as usize;
            d.layers = v["layers"].as_u64().unwrap_or(0) as usize;
            d.scanned = true;
            d.errors = errors;
            d.message.clear();
        }
        Err(e) => d.message = e,
    }
}

/// Scan the open documents.
pub fn scan_documents(app: &mut PhotocraftApp) {
    let r = app.run("typer.fontScan", json!({"documents": "all"}));
    take(app, r, Vec::new());
}

/// Ask for PSD files and scan them.
pub fn scan_files(app: &mut PhotocraftApp) {
    let r = app.pick_files_bytes(&["psd", "psb"], |app, files| {
        let mut scans = Vec::new();
        let mut errors = Vec::new();
        for (name, bytes) in files {
            let short = crate::file_open::display_name(&name);
            match photocraft_engine::typer_import_cmds::scan_psd(&short, &bytes) {
                Ok(s) => scans.push(s),
                Err(e) => errors.push(e),
            }
        }
        scans.sort_by(|a, b| photocraft_typer::pages::natural_cmp(&a.file, &b.file));
        let r = app.run("typer.fontScan", json!({"scans": scans}));
        take(app, r, errors);
        Ok(Value::Null)
    });
    if let Err(e) = r
        && e != crate::file_dialog::CANCELLED
        && let Some(d) = app.ui.typer.fontscan.as_mut()
    {
        d.message = e;
    }
}

/// Import the chosen proposals. `Err` keeps the dialog open with the reason.
pub(crate) fn import(app: &mut PhotocraftApp, d: &FontScanDraft) -> Result<(), String> {
    let styles: Vec<Value> = d
        .groups
        .iter()
        .zip(&d.names)
        .filter(|(g, _)| g["key"].as_str().is_some_and(|k| d.selected.iter().any(|s| s == k)))
        .map(|(g, name)| {
            let name = if name.trim().is_empty() { g["defaultName"].as_str().unwrap_or("Style") } else { name.trim() };
            json!({"name": name, "textProps": g["textProps"], "stroke": g["stroke"]})
        })
        .collect();
    if styles.is_empty() {
        return Err(tl!("Choose at least one font to import.").into());
    }
    let mut p = json!({"styles": styles});
    match d.dest {
        Dest::NewFolder => {
            if d.new_folder.trim().is_empty() {
                return Err(tl!("Name the new folder.").into());
            }
            p["newFolder"] = json!(d.new_folder.trim());
        }
        Dest::Existing => p["folder"] = d.folder.clone().ok_or_else(|| tl!("Choose a folder.").to_string())?,
        Dest::Unsorted => {}
    }
    let p = with_now(app, p);
    super::run(app, "typer.fontScan.import", p).map(|_| ())
}

pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.typer.fontscan.clone() else { return };
    let t = Tokens::get(ctx);
    let folders: Vec<(String, String, Value)> =
        app.session.typer.state.library.flatten_tree().into_iter().map(|f| (f.id.to_string(), f.label, f.id.to_value())).collect();
    let families = crate::type_tool::families();
    let loaded = |g: &Value| {
        let name = g["fontName"].as_str().unwrap_or_default();
        g["installed"].as_bool() == Some(true) || families.iter().any(|f| f.eq_ignore_ascii_case(name))
    };
    let (mut close, mut do_import, mut scan_docs, mut scan_psds, mut load_fonts) = (false, false, false, false, false);
    egui::Window::new("FontScanR")
        .id(egui::Id::new("typer-fontscan"))
        .title_bar(false)
        .resizable(false)
        .default_width(460.0)
        .frame(dialog_frame(&t))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -20.0))
        .show(ctx, |ui| {
            ui.set_width(460.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("FontScanR").color(t.text).size(13.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::icons::button(ui, "x", 20.0, false, tl!("Close")).clicked() {
                        close = true;
                    }
                });
            });
            ui.add(
                egui::Label::new(
                    RichText::new(tl!("Find the fonts a chapter's text layers use and turn them into styles: the most used size wins."))
                        .color(t.text_dim)
                        .size(11.5),
                )
                .wrap(),
            );
            ui.horizontal(|ui| {
                if ui.add_enabled(!app.session.documents().is_empty(), egui::Button::new(tl!("Scan open documents"))).clicked() {
                    scan_docs = true;
                }
                if ui.button(tl!("Choose PSD files…")).clicked() {
                    scan_psds = true;
                }
            });
            if d.scanned {
                crate::widgets::hairline(ui);
                let summary = format!(
                    "{} · {} · {}",
                    crate::i18n::trn(crate::i18n::current(), d.files as u64, "{n} file", "{n} files"),
                    crate::i18n::trn(crate::i18n::current(), d.layers as u64, "{n} text layer", "{n} text layers"),
                    crate::i18n::trn(crate::i18n::current(), d.groups.len() as u64, "{n} font", "{n} fonts")
                );
                ui.label(RichText::new(summary).color(t.text_dim).size(11.5));
                for e in &d.errors {
                    ui.label(RichText::new(e).color(t.warning).size(11.0));
                }
                if d.groups.is_empty() {
                    ui.label(RichText::new(tl!("No text layers found.")).color(t.text_faint).size(11.5));
                }
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    for (i, g) in d.groups.iter().enumerate() {
                        let key = g["key"].as_str().unwrap_or_default().to_string();
                        ui.horizontal(|ui| {
                            let mut on = d.selected.contains(&key);
                            if crate::widgets::checkbox(ui, &mut on, "").changed() {
                                d.selected.retain(|k| *k != key);
                                if on {
                                    d.selected.push(key.clone());
                                }
                            }
                            if let Some(name) = d.names.get_mut(i) {
                                ui.add(egui::TextEdit::singleline(name).desired_width(180.0));
                            }
                            let size = g["topSize"].as_f64().unwrap_or(0.0);
                            let unit = if g["textProps"]["typeUnit"] == "pointsUnit" { "pt" } else { "px" };
                            let count = g["usageCount"].as_u64().unwrap_or(0);
                            let uses = crate::i18n::trn(crate::i18n::current(), count, "{n} layer", "{n} layers");
                            ui.label(RichText::new(format!("{} {unit} · {uses}", photocraft_typer::id::num(size))).color(t.text_dim).size(11.0));
                            if !loaded(g)
                                && ui
                                    .link(RichText::new(tl!("not loaded")).color(t.warning).size(11.0))
                                    .on_hover_text(tl!("Load the font with Type › Load Fonts…"))
                                    .clicked()
                            {
                                load_fonts = true;
                            }
                        });
                    }
                });
                if !d.groups.is_empty() {
                    crate::widgets::hairline(ui);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(tl!("Import into")).color(t.text_dim).size(11.5));
                        let opts = [(Dest::NewFolder, tl!("New folder")), (Dest::Existing, tl!("Existing folder")), (Dest::Unsorted, tl!("Unsorted"))];
                        crate::widgets::dropdown(ui, "typer-fontscan-dest", &mut d.dest, &opts, 140.0);
                        match d.dest {
                            Dest::NewFolder => {
                                ui.add(egui::TextEdit::singleline(&mut d.new_folder).desired_width(150.0).hint_text(tl!("Folder name")));
                            }
                            Dest::Existing => {
                                let mut cur = d.folder.as_ref().and_then(photocraft_typer::Id::from_value).map_or(String::new(), |i| i.to_string());
                                let mut opts: Vec<(String, &str)> = vec![(String::new(), tl!("Choose a folder"))];
                                opts.extend(folders.iter().map(|(k, n, _)| (k.clone(), n.as_str())));
                                if crate::widgets::dropdown(ui, "typer-fontscan-folder", &mut cur, &opts, 150.0) {
                                    d.folder = folders.iter().find(|(k, _, _)| *k == cur).map(|(_, _, v)| v.clone());
                                }
                            }
                            Dest::Unsorted => {}
                        }
                    });
                }
            }
            if !d.message.is_empty() {
                ui.label(RichText::new(&d.message).color(t.warning).size(11.5));
            }
            ui.add_space(6.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                use crate::widgets::{ButtonRole, DialogButton};
                let mut buttons = Vec::new();
                if d.scanned && !d.groups.is_empty() {
                    buttons.push(DialogButton::new(ButtonRole::Default, tl!("Import"), 80.0));
                }
                buttons.push(DialogButton::new(ButtonRole::Cancel, tl!("Cancel"), 80.0));
                match crate::widgets::dialog_buttons(ui, &buttons) {
                    Some(ButtonRole::Default) => do_import = true,
                    Some(ButtonRole::Cancel) => close = true,
                    _ => {}
                }
            });
        });
    if close {
        app.ui.typer.fontscan = None;
        return;
    }
    app.ui.typer.fontscan = Some(d.clone());
    if scan_docs {
        scan_documents(app);
    }
    if scan_psds {
        scan_files(app);
    }
    if load_fonts {
        let _ = crate::menus::invoke(app, ctx, "type.loadFonts", json!({}));
    }
    if do_import {
        match import(app, &d) {
            Ok(()) => app.ui.typer.fontscan = None,
            Err(e) => {
                if let Some(cur) = app.ui.typer.fontscan.as_mut() {
                    cur.message = e;
                }
            }
        }
    }
}
