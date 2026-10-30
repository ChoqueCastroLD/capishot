//! TypeR settings (TypeR `settings.jsx`, §2.10 of the port plan) in five tabs: General, Text,
//! Behavior, Shortcuts and Data. The first three edit a draft of `typer.settings.get` that Save
//! applies at once (`typer.settings.set {values}`); Shortcuts and Data act immediately.

use egui::RichText;
use photocraft_typer::exchange::{self, ImportKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Confirm, dialog_frame, with_now};
use crate::PhotocraftApp;
use crate::theme::Tokens;

/// The open settings dialog.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SettingsDraft {
    /// 0 General, 1 Text, 2 Behavior, 3 Shortcuts, 4 Data.
    pub tab: usize,
    /// The settings being edited (TypeR keys).
    pub values: Value,
    /// Ignore prefixes and tags as typed (one per line).
    pub ignore_prefixes: String,
    pub ignore_tags: String,
    /// Extra page marker words as typed (`Página; Pág.`).
    pub page_words: String,
    /// Data: export the settings with the styles.
    pub export_settings: bool,
    /// Data: the folder to export (its id).
    pub export_folder: Option<Value>,
    pub message: String,
}

/// Work a tab asks for, run once the dialog is drawn (it needs the app).
type AfterFn = Box<dyn FnOnce(&mut PhotocraftApp)>;

pub const TABS: [&str; 5] = ["General", "Text", "Behavior", "Shortcuts", "Data"];

pub fn open(app: &mut PhotocraftApp) {
    if app.ui.typer.settings.is_some() {
        app.ui.typer.settings = None;
        return;
    }
    let s = &app.session.typer.state.settings;
    app.ui.typer.settings = Some(SettingsDraft {
        values: s.to_json(),
        ignore_prefixes: s.ignore_line_prefixes.join("\n"),
        ignore_tags: s.ignore_tags.join("\n"),
        page_words: s.page_marker_words.join("; "),
        ..Default::default()
    });
}

/// A name safe for a download (`Series A` → `Series A`, `a/b:c` → `a_b_c`).
pub(crate) fn file_stem(name: &str) -> String {
    let s: String = name.trim().chars().map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '_' }).take(80).collect();
    if s.trim().is_empty() { "TypeR".into() } else { s }
}

/// Import a TypeR export (full, library or folder). A full import over a non-empty library asks
/// whether to keep the local styles first, as TypeR does.
pub fn import(app: &mut PhotocraftApp) {
    let r = app.pick_file_bytes_filtered(&["json", "tpl"], |app, name, bytes| {
        // Photoshop tool presets: each Type tool preset becomes a style in a new folder.
        if name.to_ascii_lowercase().ends_with(".tpl") {
            let now = app.services.clock.map(|c| c()).filter(|n| n.is_finite() && *n > 0.0);
            let short = crate::file_open::display_name(&name);
            let r = photocraft_engine::typer_import_cmds::import_tpl_bytes(&mut app.session, &bytes, &short, now).map_err(|e| format!("{short}: {e}"));
            match &r {
                Ok(_) => app.ui.typer.message.clear(),
                Err(e) => app.ui.typer.message = e.clone(),
            }
            return r;
        }
        let text = String::from_utf8(bytes).map_err(|_| format!("{name}: {}", tl!("not a TypeR export")))?;
        let data = exchange::parse(&text).map_err(|e| {
            app.ui.typer.message = format!("{name}: {e}");
            e
        })?;
        if exchange::classify(&data) == ImportKind::Full && !app.session.typer.state.library.styles.is_empty() {
            app.ui.typer.confirm = Some(Confirm::ImportKeepLocal { json: text });
            return Ok(Value::Null);
        }
        let p = with_now(app, json!({"json": text}));
        super::run(app, "typer.library.import", p)
    });
    if let Err(e) = r {
        app.ui.typer.message = e;
    }
}

fn save_text(app: &mut PhotocraftApp, file: String, text: String) {
    let r = app.pick_save(&file, move |app, path| {
        let write = app.services.write.as_mut().ok_or("no writer configured")?;
        write(&path, text.as_bytes())?;
        Ok(Value::Null)
    });
    if let Err(e) = r {
        app.ui.typer.message = e;
    }
}

/// Export the whole library (TypeR's full export), optionally with the settings.
pub fn export_library(app: &mut PhotocraftApp, with_settings: bool) {
    let Ok(v) = super::run(app, "typer.library.export", json!({"withSettings": with_settings})) else { return };
    save_text(app, "TypeR-styles.json".into(), serde_json::to_string_pretty(&v).unwrap_or_default());
}

fn label(ui: &mut egui::Ui, t: &Tokens, s: &str) {
    ui.label(RichText::new(s).color(t.text_dim).size(11.5));
}

fn hint(ui: &mut egui::Ui, t: &Tokens, s: &str) {
    ui.add(egui::Label::new(RichText::new(s).color(t.text_faint).size(11.0)).wrap());
}

fn flag(ui: &mut egui::Ui, v: &mut Value, key: &str, text: &str) {
    let mut on = v.get(key).and_then(Value::as_bool).unwrap_or(false);
    if crate::widgets::checkbox(ui, &mut on, text).changed() {
        v[key] = json!(on);
    }
}

fn number(ui: &mut egui::Ui, v: &mut Value, key: &str, default: f64, range: std::ops::RangeInclusive<f32>, suffix: &str) {
    let mut n = v.get(key).and_then(Value::as_f64).unwrap_or(default) as f32;
    if crate::widgets::compact_value_field(ui, &mut n, range, suffix, 60.0, 20.0).accessible_name(key).changed() {
        v[key] = json!(n);
    }
}

pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.typer.settings.clone() else { return };
    let t = Tokens::get(ctx);
    let styles: Vec<(String, String, Value)> =
        app.session.typer.state.library.styles.iter().map(|s| (s.id.to_string(), s.name.clone(), s.id.to_value())).collect();
    let folders: Vec<(String, String, Value)> =
        app.session.typer.state.library.flatten_tree().into_iter().map(|f| (f.id.to_string(), f.label, f.id.to_value())).collect();
    let overrides = app.session.prefs().shortcuts.clone();
    let keymap_on = super::keymap::is_on(&overrides);
    let mut keymap_change: Option<bool> = None;
    let (mut close, mut save) = (false, false);
    let mut run_after: Vec<AfterFn> = Vec::new();
    egui::Window::new(tl!("TypeR Settings"))
        .id(egui::Id::unique("typer-settings"))
        .title_bar(false)
        .resizable(false)
        .default_width(430.0)
        .max_width(470.0)
        .frame(dialog_frame(&t))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -30.0))
        .show(ctx, |ui| {
            ui.set_width(430.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(tl!("TypeR Settings")).color(t.text).size(13.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::icons::button(ui, "x", 20.0, false, tl!("Close")).clicked() {
                        close = true;
                    }
                });
            });
            ui.horizontal(|ui| {
                for (i, name) in TABS.iter().enumerate() {
                    if crate::widgets::pill_tab(ui, tl!(name), d.tab == i).clicked() {
                        d.tab = i;
                    }
                }
            });
            crate::widgets::hairline(ui);
            ui.add_space(4.0);
            egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| match d.tab {
                0 => {
                    egui::Grid::new("typer-settings-general").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        label(ui, &t, tl!("Pasted text layer"));
                        let mut point = d.values.get("pastePointText").and_then(Value::as_bool).unwrap_or(false);
                        if crate::widgets::dropdown(ui, "typer-set-kind", "", &mut point, &[(false, tl!("Paragraph text")), (true, tl!("Point text"))], 160.0) {
                            d.values["pastePointText"] = json!(point);
                        }
                        ui.end_row();
                        label(ui, &t, tl!("Default style"));
                        let mut cur = d.values.get("defaultStyleId").and_then(photocraft_typer::Id::from_value).map_or(String::new(), |i| i.to_string());
                        let mut opts: Vec<(String, &str)> = vec![(String::new(), tl!("No style set"))];
                        opts.extend(styles.iter().map(|(k, n, _)| (k.clone(), n.as_str())));
                        if crate::widgets::dropdown(ui, "typer-set-default", "", &mut cur, &opts, 200.0) {
                            d.values["defaultStyleId"] = styles.iter().find(|(k, _, _)| *k == cur).map_or(Value::Null, |(_, _, v)| v.clone());
                        }
                        ui.end_row();
                        label(ui, &t, tl!("Text direction"));
                        let mut dir = d.values.get("direction").and_then(Value::as_str).unwrap_or("ltr").to_string();
                        let opts = [("ltr".to_string(), tl!("Left to right")), ("rtl".to_string(), tl!("Right to left"))];
                        if crate::widgets::dropdown(ui, "typer-set-dir", "", &mut dir, &opts, 160.0) {
                            d.values["direction"] = json!(dir);
                        }
                        ui.end_row();
                    });
                    ui.add_space(6.0);
                    label(ui, &t, tl!("Tags for ignoring lines"));
                    ui.add(egui::TextEdit::multiline(&mut d.ignore_prefixes).desired_rows(2).desired_width(f32::INFINITY));
                    hint(ui, &t, tl!("One per line (or separated by ;). A line starting with one of them is skipped, like an empty line."));
                    label(ui, &t, tl!("Tags to ignore"));
                    ui.add(egui::TextEdit::multiline(&mut d.ignore_tags).desired_rows(2).desired_width(f32::INFINITY));
                    hint(ui, &t, tl!("Removed from the line wherever they appear; the rest of the line is kept."));
                    flag(ui, &mut d.values, "showTips", tl!("Show tips"));
                }
                1 => {
                    flag(ui, &mut d.values, "interpretMarkdown", tl!("Interpret Markdown (bold and italic)"));
                    hint(ui, &t, tl!("**bold**, *italic* and ***both*** in the script become faux bold and italic."));
                    flag(ui, &mut d.values, "resetLineCounterOnPage", tl!("Number lines from 1 on each page"));
                    flag(ui, &mut d.values, "longestPrefixMatch", tl!("Longest tag wins (TypeR: the first one)"));
                    flag(ui, &mut d.values, "extraPageMarkers", tl!("Extra page marker words"));
                    if d.values.get("extraPageMarkers").and_then(Value::as_bool).unwrap_or(true) {
                        ui.add(egui::TextEdit::singleline(&mut d.page_words).desired_width(f32::INFINITY).hint_text("Página; Pág."));
                    }
                    hint(ui, &t, tl!("Page is always a marker word. Separate words with ; (case, accents and a final dot don't matter): Página 12, Pág. 12 and Pag 12 all mark page 12."));
                    flag(ui, &mut d.values, "middleEast", tl!("Middle Eastern text mode"));
                    egui::Grid::new("typer-settings-text").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        label(ui, &t, tl!("Internal padding"));
                        number(ui, &mut d.values, "internalPadding", 10.0, 0.0..=100.0, "px");
                        ui.end_row();
                        label(ui, &t, tl!("Text scale"));
                        let mut s = d.values.get("textScale").and_then(Value::as_f64).unwrap_or(100.0) as f32;
                        if crate::widgets::compact_value_field(ui, &mut s, 1.0..=999.0, "%", 60.0, 20.0).accessible_name(tl!("Text scale")).changed() {
                            d.values["textScale"] = json!(s);
                        }
                        ui.end_row();
                        label(ui, &t, tl!("Text size step"));
                        number(ui, &mut d.values, "textSizeIncrement", 1.0, 1.0..=99.0, "px");
                        ui.end_row();
                    });
                }
                2 => {
                    flag(ui, &mut d.values, "autoScrollStyle", tl!("Scroll to the selected style"));
                    flag(ui, &mut d.values, "currentFolderTagPriority", tl!("Tags of the current style's folder only"));
                    hint(ui, &t, tl!("Two series can use the same tags (B:, SFX:) in their own folders."));
                    flag(ui, &mut d.values, "resizeTextBoxOnCenter", tl!("Resize the text box when aligning"));
                    flag(ui, &mut d.values, "multiBubbleMode", tl!("Multi-bubble mode"));
                    flag(ui, &mut d.values, "openStoredSelections", tl!("Multi-bubble: centre on the bubble, as Paste does"));
                    flag(ui, &mut d.values, "showQuickStyleSize", tl!("Quick style size −/+"));
                    flag(ui, &mut d.values, "multiTabEnabled", tl!("Work tabs (one per chapter)"));
                    flag(ui, &mut d.values, "autoClosePSD", tl!("Close the previous page's file when the next page opens"));
                    hint(ui, &t, tl!("A page with unsaved changes asks first: Save, Don't Save or Cancel."));
                    ui.horizontal(|ui| {
                        label(ui, &t, tl!("Style size step"));
                        number(ui, &mut d.values, "styleSizeStep", 1.0, 0.1..=100.0, "");
                    });
                }
                3 => {
                    let mut on = keymap_on;
                    if crate::widgets::checkbox(ui, &mut on, tl!("TypeR keymap")).changed() {
                        keymap_change = Some(on);
                    }
                    hint(ui, &t, tl!("Photoshop's shortcuts always win. TypeR's Win+Ctrl, Win+Shift and Win+Alt become Ctrl+Alt keys; keys Photoshop uses (Ctrl+Enter, Ctrl+Tab, Ctrl+H) get free alternatives."));
                    ui.add_space(4.0);
                    egui::Grid::new("typer-settings-keys").num_columns(3).striped(true).spacing([12.0, 3.0]).show(ui, |ui| {
                        for s in [tl!("Action"), tl!("TypeR"), tl!("Capishot")] {
                            ui.label(RichText::new(s).color(t.text_faint).size(11.0));
                        }
                        ui.end_row();
                        for (id, name, typer, preset) in super::keymap::ENTRIES {
                            ui.label(RichText::new(crate::i18n::tr_id(crate::i18n::current(), id, name)).color(t.text_dim).size(11.5));
                            ui.label(RichText::new(*typer).color(t.text_faint).size(11.5));
                            // The key in effect, else the preset's, greyed.
                            match overrides.get(*id).filter(|k| !k.is_empty()) {
                                Some(k) => ui.label(RichText::new(crate::shortcuts::pretty(k)).color(t.text).size(11.5)),
                                None => ui.label(RichText::new(format!("— ({})", crate::shortcuts::pretty(preset))).color(t.text_faint).size(11.5)),
                            };
                            ui.end_row();
                        }
                    });
                    ui.add_space(4.0);
                    if ui.button(tl!("Keyboard Shortcuts…")).clicked() {
                        run_after.push(Box::new(|app| {
                            crate::prefs_ui::open_shortcuts(app, 0);
                        }));
                    }
                }
                _ => {
                    crate::widgets::section_label(ui, tl!("Import"));
                    if ui.button(tl!("Import styles or settings…")).clicked() {
                        run_after.push(Box::new(import));
                    }
                    hint(ui, &t, tl!("A TypeR export (.json): a folder, a style library or a full export with settings. Photoshop tool presets (.tpl): each Type tool preset becomes a style in a new folder."));
                    crate::widgets::section_label(ui, tl!("Export"));
                    ui.horizontal(|ui| {
                        crate::widgets::checkbox(ui, &mut d.export_settings, tl!("With my settings"));
                        if ui.button(tl!("Export all styles…")).clicked() {
                            let with = d.export_settings;
                            run_after.push(Box::new(move |app| export_library(app, with)));
                        }
                    });
                    ui.horizontal(|ui| {
                        let mut cur = d.export_folder.as_ref().and_then(photocraft_typer::Id::from_value).map_or(String::new(), |i| i.to_string());
                        let mut opts: Vec<(String, &str)> = vec![(String::new(), tl!("Choose a folder"))];
                        opts.extend(folders.iter().map(|(k, n, _)| (k.clone(), n.as_str())));
                        if crate::widgets::dropdown(ui, "typer-set-export-folder", "", &mut cur, &opts, 200.0) {
                            d.export_folder = folders.iter().find(|(k, _, _)| *k == cur).map(|(_, _, v)| v.clone());
                        }
                        if ui.add_enabled(d.export_folder.is_some(), egui::Button::new(tl!("Export folder…"))).clicked()
                            && let Some(id) = d.export_folder.clone()
                        {
                            let name = folders.iter().find(|(_, _, v)| *v == id).map_or_else(String::new, |(_, n, _)| n.clone());
                            run_after.push(Box::new(move |app| super::styles::export_folder(app, id, &name)));
                        }
                    });
                }
            });
            if !d.message.is_empty() {
                ui.label(RichText::new(&d.message).color(t.warning).size(11.5));
            }
            ui.add_space(6.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                use crate::widgets::{ButtonRole, DialogButton};
                match crate::widgets::dialog_buttons(ui, &[DialogButton::new(ButtonRole::Default, tl!("Save"), 80.0), DialogButton::new(ButtonRole::Cancel, tl!("Cancel"), 80.0)]) {
                    Some(ButtonRole::Default) => save = true,
                    Some(ButtonRole::Cancel) => close = true,
                    _ => {}
                }
            });
        });
    if let Some(on) = keymap_change
        && let Err(e) = super::keymap::apply(app, on)
    {
        d.message = e;
    }
    for f in run_after {
        f(app);
    }
    if close {
        app.ui.typer.settings = None;
        return;
    }
    if save {
        let mut values = d.values.clone();
        values["ignoreLinePrefixes"] = json!(d.ignore_prefixes);
        values["ignoreTags"] = json!(d.ignore_tags);
        values["pageMarkerWords"] = json!(d.page_words);
        // Only the keys TypeR's settings know; `set` is all-or-nothing.
        match app.run("typer.settings.set", json!({"values": values})) {
            Ok(_) => {
                app.ui.typer.settings = None;
                return;
            }
            Err(e) => d.message = e,
        }
    }
    if app.ui.typer.settings.is_some() {
        app.ui.typer.settings = Some(d);
    }
}
