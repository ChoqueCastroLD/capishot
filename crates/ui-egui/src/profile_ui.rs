//! Edit › Preferences › Profile & Themes: the colour themes (every built-in one and the user's
//! own, all `theme.css` stylesheets, [`crate::theme::css`]), the Theme editor with its live
//! preview, and the profile: exported as a `.proj.capishot` with the sections the person ticks,
//! and imported after showing what a file carries, replacing or merging, with the profile as it
//! was kept so the import can be undone (`profile.*` in the engine).

use photocraft_engine::font_projects as fp;
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::theme::css::{self, Group, Slot, TOKENS};
use crate::theme::{ThemeKind, Tokens};

/// This dialog's kind.
pub const KIND: &str = "profile";

/// Sections the shell carries itself, besides the engine's (`photocraft_engine::prefs::profile`).
const SHELL_SECTIONS: [(&str, &str); 3] = [("bubble", "Bubble Type styles"), ("fontFiles", "Font files"), ("fontProject", "Font project")];

/// Every section a profile can carry, with its English label.
fn all_sections() -> Vec<(&'static str, &'static str)> {
    photocraft_engine::prefs::profile::SECTIONS.iter().copied().chain(SHELL_SECTIONS).collect()
}

/// Window › Theme items of the user's themes: `window.theme.custom.<theme id>`.
pub const CUSTOM: &str = "window.theme.custom.";
/// Window › Theme: Manage Themes…, Create Theme… and Load Theme…, in menu order.
pub const ACTIONS: [&str; 3] = ["window.theme.manage", "window.theme.create", "window.theme.load"];

/// Runs a Window › Theme item of this dialog's; `None` when `id` isn't one.
pub fn menu(app: &mut PhotocraftApp, id: &str) -> Option<Result<Value, String>> {
    match id {
        "window.theme.manage" => Some(Ok(json!({"dialog": open(app)}))),
        "window.theme.create" => Some(Ok(json!({"dialog": create(app)}))),
        "window.theme.load" => Some(load_theme(app)),
        _ => id.strip_prefix(CUSTOM).map(str::to_string).map(|theme| app.run("theme.custom.use", json!({"id": theme}))),
    }
}

/// Opens Profile & Themes on its themes tab.
pub fn open(app: &mut PhotocraftApp) -> u64 {
    open_on(app, "themes")
}

/// Opens the Theme editor on a new theme starting from the one in use. A theme already being
/// edited stays, with its unsaved changes.
fn create(app: &mut PhotocraftApp) -> u64 {
    let id = open(app);
    let custom = app.session.prefs().interface.active_custom_theme().cloned();
    let kind = app.ui.theme;
    if let Some(d) = app.ui.dialog_mut(id)
        && !d.fields.get("edit").is_some_and(Value::is_object)
    {
        match custom {
            Some(c) => {
                let name = crate::i18n::fmt(tl!("My {theme}"), &[("theme", &c.name)]);
                edit(&mut d.fields, "", &name, &c.css, serde_json::to_value(&c.meta).unwrap_or_default(), &c.icon);
            }
            None => edit_from_builtin(&mut d.fields, kind),
        }
    }
    id
}

fn open_on(app: &mut PhotocraftApp, tab: &str) -> u64 {
    if let Some(d) = app.ui.dialogs.iter_mut().find(|d| d.fields.get("__prefsui").and_then(Value::as_str) == Some(KIND)) {
        d.fields.insert("tab".into(), json!(tab));
        return d.id;
    }
    let sections: Map<String, Value> = all_sections().iter().map(|(id, _)| (id.to_string(), json!(*id != "fontFiles"))).collect();
    let fields = json!({"tab": tab, "export": sections, "includeRestricted": false, "merge": false, "message": "", "name": tl!("My Capishot profile")});
    crate::prefs_ui::open_dialog(app, KIND, tl!("Profile & Themes"), fields)
}

/// Is the dialog open? (The Theme editor's live preview ends when it closes.)
pub fn is_open(app: &PhotocraftApp) -> bool {
    app.ui.dialogs.iter().any(|d| d.fields.get("__prefsui").and_then(Value::as_str) == Some(KIND))
}

/// The dialog's OK: leaves the theme being edited unsaved.
pub fn confirm(app: &mut PhotocraftApp) -> Result<Value, String> {
    app.prefs_rt.theme_preview = None;
    Ok(Value::Null)
}

pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let tab = f.get("tab").and_then(Value::as_str).unwrap_or("themes").to_string();
    ui.horizontal_wrapped(|ui| {
        for (id, label) in [("themes", "Themes"), ("export", "Export profile"), ("import", "Import profile")] {
            if crate::widgets::pill_tab(ui, label, tab == id).clicked() {
                f.insert("tab".into(), json!(id));
            }
        }
    });
    ui.add_space(8.0);
    let message = f.get("message").and_then(Value::as_str).unwrap_or_default().to_string();
    if !message.is_empty() {
        let t = Tokens::get(ui.ctx());
        ui.label(egui::RichText::new(message).color(t.text_dim));
        ui.add_space(4.0);
    }
    let height = (ui.ctx().content_rect().height() * 0.62).clamp(240.0, 620.0);
    egui::ScrollArea::vertical().id_salt("profile-scroll").max_height(height).auto_shrink([false, true]).show(ui, |ui| match tab.as_str() {
        "export" => export_tab(app, ui, f),
        "import" => import_tab(app, ui, f),
        _ if f.get("edit").is_some_and(Value::is_object) => editor(app, ui, f),
        _ => themes_tab(app, ui, f),
    });
    if !f.get("edit").is_some_and(Value::is_object) && app.prefs_rt.theme_preview.is_some() {
        app.prefs_rt.theme_preview = None;
    }
}

fn say(f: &mut Map<String, Value>, msg: impl Into<String>) {
    f.insert("message".into(), json!(msg.into()));
}

// ------------------------------------------------------------------ themes

/// A theme's swatch: panels, chrome strip and accent dot.
fn swatch(ui: &mut egui::Ui, p: &Tokens) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 22.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, p.dock);
    painter.rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 5.0)), 0.0, p.chrome);
    painter.circle_filled(rect.right_bottom() - egui::vec2(6.0, 6.0), 2.5, p.accent);
    painter.rect_stroke(rect, 3.0, egui::Stroke::new(1.0, Tokens::get(ui.ctx()).card_border), egui::StrokeKind::Inside);
}

/// Starts the editor on a new theme whose stylesheet is `css`.
fn edit(f: &mut Map<String, Value>, id: &str, name: &str, css: &str, meta: Value, icon: &str) {
    f.insert("edit".into(), json!({"id": id, "name": name, "css": css, "meta": meta, "icon": icon, "text": false}));
}

/// Starts the editor on a new theme based on the built-in `kind`.
fn edit_from_builtin(f: &mut Map<String, Value>, kind: ThemeKind) {
    let name = crate::i18n::fmt(tl!("My {theme}"), &[("theme", tl!(kind.label()))]);
    edit(f, "", &name, &format!(":root {{\n  --base: {};\n}}\n", kind.id()), json!({}), "");
}

fn themes_tab(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let interface = app.session.prefs().interface.clone();
    let active = interface.active_custom_theme().map(|c| c.id.clone());
    ui.label(egui::RichText::new(tl!("Your themes")).strong());
    if interface.custom_themes.is_empty() {
        ui.label(egui::RichText::new(tl!("None yet: start one from a built-in theme below, or import a .theme.capishot file.")).color(t.text_dim));
    }
    let mut action: Option<(&str, String)> = None;
    let renaming = f.get("rename").and_then(Value::as_str).map(str::to_string);
    for c in &interface.custom_themes {
        ui.horizontal_wrapped(|ui| {
            swatch(ui, &css::parse(&c.css).tokens);
            if renaming.as_deref() == Some(c.id.as_str()) {
                let mut name = f.get("renameText").and_then(Value::as_str).unwrap_or(&c.name).to_string();
                let r = crate::widgets::text_field(ui, egui::Id::unique(("theme-rename", &c.id)), &mut name, tl!("Name"), 160.0, false);
                f.insert("renameText".into(), json!(name));
                if r.lost_focus() || crate::widgets::secondary_button(ui, tl!("OK"), 0.0).clicked() {
                    action = Some(("rename", c.id.clone()));
                }
            } else {
                let on = active.as_deref() == Some(c.id.as_str());
                if ui.selectable_label(on, &c.name).clicked() {
                    action = Some(("use", c.id.clone()));
                }
            }
            for (id, label) in [("edit", "Edit…"), ("duplicate", "Duplicate"), ("startRename", "Rename"), ("export", "Export…"), ("delete", "Delete")] {
                if ui.small_button(tl!(label)).clicked() {
                    action = Some((id, c.id.clone()));
                }
            }
        });
    }
    if let Some((what, id)) = action {
        let r = match what {
            "use" => app.run("theme.custom.use", json!({"id": id})),
            "edit" => {
                if let Some(c) = interface.custom_themes.iter().find(|c| c.id == id) {
                    edit(f, &c.id, &c.name, &c.css, serde_json::to_value(&c.meta).unwrap_or_default(), &c.icon);
                }
                Ok(Value::Null)
            }
            "duplicate" => app.run("theme.custom.duplicate", json!({"id": id})),
            "startRename" => {
                f.insert("rename".into(), json!(id));
                f.remove("renameText");
                Ok(Value::Null)
            }
            "rename" => {
                let name = f.remove("renameText").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
                f.remove("rename");
                if name.trim().is_empty() { Ok(Value::Null) } else { app.run("theme.custom.rename", json!({"id": id, "name": name})) }
            }
            "export" => export_theme(app, &id),
            _ => app.run("theme.custom.delete", json!({"id": id})),
        };
        if let Err(e) = r {
            say(f, e);
        }
    }
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        if crate::widgets::secondary_button(ui, tl!("Import theme…"), 0.0).clicked()
            && let Err(e) = load_theme(app)
        {
            say(f, e);
        }
        let back = active.is_some();
        if back && crate::widgets::secondary_button(ui, tl!("Use the built-in theme"), 0.0).clicked() {
            let _ = app.run("theme.custom.use", json!({"id": ""}));
        }
    });
    ui.add_space(10.0);
    ui.label(egui::RichText::new(tl!("Built-in themes")).strong());
    ui.label(egui::RichText::new(tl!("Start a theme of your own from any of them.")).color(t.text_dim));
    for kind in ThemeKind::ALL {
        ui.horizontal_wrapped(|ui| {
            let p = Tokens::for_kind(kind);
            swatch(ui, &p);
            let on = active.is_none() && app.ui.theme == kind;
            if ui.selectable_label(on, tl!(kind.label())).clicked() {
                app.set_theme(ui.ctx(), kind);
            }
            if ui.small_button(tl!("New theme from this…")).clicked() {
                edit_from_builtin(f, kind);
            }
        });
    }
}

/// Saves the theme `id` as a `.theme.capishot`.
fn export_theme(app: &mut PhotocraftApp, id: &str) -> Result<Value, String> {
    let out = app.run("theme.package.export", json!({"id": id}))?;
    let bytes = out["data"].as_str().and_then(photocraft_engine::paint::tile::b64_decode).ok_or("the theme couldn't be packed")?;
    let file = out["file"].as_str().unwrap_or("theme.theme.capishot").to_string();
    crate::font_projects_ui::save_bytes(app, &file, bytes);
    Ok(Value::Null)
}

/// Asks for a `.theme.capishot` and imports it.
fn load_theme(app: &mut PhotocraftApp) -> Result<Value, String> {
    app.pick_file_bytes_filtered(&["capishot"], |app, name, bytes| import_theme(app, &name, &bytes).map(|()| Value::Null))
}

/// A `.theme.capishot` opened (File › Open, a drop, Import theme…): added and used, with what
/// its stylesheet had that Capishot doesn't read reported.
pub fn import_theme(app: &mut PhotocraftApp, name: &str, bytes: &[u8]) -> Result<(), String> {
    let r = app.run("theme.package.import", json!({"data": photocraft_engine::paint::tile::b64_encode(bytes)}))?;
    let id = r["id"].as_str().unwrap_or_default();
    let theme = app.session.prefs().interface.custom_themes.iter().find(|t| t.id == id).cloned();
    let mut lines: Vec<String> = r["warnings"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
    if let Some(t) = &theme {
        lines.extend(css::parse(&t.css).warnings);
    }
    lines.truncate(12);
    let title = crate::i18n::fmt(tl!("Theme {name} imported"), &[("name", theme.as_ref().map_or(name, |t| t.name.as_str()))]);
    crate::notices::post(app, title, lines, false, None);
    Ok(())
}

// ------------------------------------------------------------------ the Theme editor

/// The editor's stylesheet read into tokens, cached by its text.
fn parsed(ui: &egui::Ui, css_text: &str) -> (Tokens, Vec<String>) {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    css_text.hash(&mut h);
    let key = h.finish();
    let id = egui::Id::unique("theme-editor-parsed");
    if let Some((k, t, w)) = ui.data(|d| d.get_temp::<(u64, Tokens, Vec<String>)>(id))
        && k == key
    {
        return (t, w);
    }
    let p = css::parse(css_text);
    ui.data_mut(|d| d.insert_temp(id, (key, p.tokens, p.warnings.clone())));
    (p.tokens, p.warnings)
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn editor(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let Some(mut e) = f.get("edit").cloned() else { return };
    let css_text = str_of(&e, "css").to_string();
    let (mut tokens, warnings) = parsed(ui, &css_text);
    app.prefs_rt.theme_preview = Some(tokens);
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(tl!("Name"));
        let mut name = str_of(&e, "name").to_string();
        crate::widgets::text_field(ui, egui::Id::unique("theme-name"), &mut name, tl!("Name"), 200.0, false);
        e["name"] = json!(name);
        ui.label(tl!("Starts from"));
        let mut base = tokens.kind;
        let options: Vec<(ThemeKind, &str)> = ThemeKind::ALL.iter().map(|k| (*k, tl!(k.label()))).collect();
        if crate::widgets::dropdown(ui, "theme-base", tl!("Starts from"), &mut base, &options, 180.0) {
            e["css"] = json!(format!(":root {{\n  --base: {};\n}}\n", base.id()));
        }
    });
    let (mut done, mut discard) = (None, false);
    ui.horizontal_wrapped(|ui| {
        if crate::widgets::primary_button(ui, tl!("Save theme"), 0.0).clicked() {
            done = Some(false);
        }
        if crate::widgets::secondary_button(ui, tl!("Save and export…"), 0.0).clicked() {
            done = Some(true);
        }
        discard = crate::widgets::secondary_button(ui, tl!("Discard changes"), 0.0).clicked();
    });
    ui.add_space(4.0);
    let mut as_text = e.get("text").and_then(Value::as_bool).unwrap_or(false);
    ui.horizontal_wrapped(|ui| {
        crate::widgets::checkbox(ui, &mut as_text, tl!("Edit as text"));
        ui.label(egui::RichText::new(tl!("Changes show at once across the app.")).color(t.text_dim).small());
    });
    e["text"] = json!(as_text);
    if as_text {
        let mut text = str_of(&e, "css").to_string();
        let r = ui.add(egui::TextEdit::multiline(&mut text).code_editor().desired_rows(18).desired_width(f32::INFINITY).char_limit(photocraft_engine::theme_cmds::MAX_CSS));
        if r.changed() {
            e["css"] = json!(text);
        }
    } else {
        for group in Group::ALL.into_iter().filter(|g| *g != Group::Base) {
            let open = matches!(group, Group::Surfaces | Group::Text | Group::Accent);
            egui::CollapsingHeader::new(tl!(group.label())).id_salt(("theme-group", group.label())).default_open(open).show(ui, |ui| {
                egui::Grid::new(("theme-grid", group.label())).num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                    for tok in TOKENS.iter().filter(|tok| tok.group == group) {
                        ui.label(tl!(tok.label)).on_hover_text(format!("--{}", tok.name));
                        changed |= token_control(ui, (tok.slot)(&mut tokens), tok.name);
                        ui.end_row();
                    }
                });
            });
        }
        if changed {
            e["css"] = json!(css::write_changes(&tokens));
        }
    }
    if !warnings.is_empty() {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(crate::i18n::fmt(tl!("{n} lines ignored:"), &[("n", &warnings.len().to_string())])).color(t.warning));
        for w in warnings.iter().take(8) {
            ui.label(egui::RichText::new(w).color(t.text_dim).small());
        }
    }
    egui::CollapsingHeader::new(tl!("Description (for sharing)")).id_salt("theme-meta").show(ui, |ui| meta_form(app, ui, &mut e));
    if discard {
        f.remove("edit");
        app.prefs_rt.theme_preview = None;
        app.prefs_rt.custom_theme = None;
        return;
    }
    if let Some(export) = done {
        let id = str_of(&e, "id").to_string();
        let mut p = json!({"css": e["css"], "name": e["name"], "meta": e["meta"], "icon": e["icon"], "use": true});
        if !id.is_empty() {
            p["id"] = json!(id);
        }
        match app.run("theme.custom.save", p) {
            Ok(r) => {
                let id = r["id"].as_str().unwrap_or_default().to_string();
                f.remove("edit");
                app.prefs_rt.theme_preview = None;
                app.prefs_rt.custom_theme = None;
                say(f, tl!("Theme saved."));
                if export && let Err(err) = export_theme(app, &id) {
                    say(f, err);
                }
            }
            Err(err) => say(f, err),
        }
        return;
    }
    f.insert("edit".into(), e);
}

/// One token's control; true when it changed.
fn token_control(ui: &mut egui::Ui, slot: Slot<'_>, name: &str) -> bool {
    let drag = |ui: &mut egui::Ui, x: &mut f32, min: f32, max: f32| ui.add(egui::DragValue::new(x).range(min..=max).speed(0.25).max_decimals(1)).changed();
    match slot {
        Slot::Color(c) => crate::widgets::color_edit_button_srgba(ui, c).changed(),
        Slot::Auto(c) => {
            ui.horizontal(|ui| {
                let mut auto = c.is_none();
                let mut ch = crate::widgets::checkbox(ui, &mut auto, tl!("Auto")).changed();
                if ch {
                    *c = if auto { None } else { Some(Tokens::get(ui.ctx()).field) };
                }
                if let Some(col) = c {
                    ch |= crate::widgets::color_edit_button_srgba(ui, col).changed();
                }
                ch
            })
            .inner
        }
        Slot::Num(x, min, max) => drag(ui, x, min, max),
        Slot::AutoNum(x, min, max) => {
            ui.horizontal(|ui| {
                let mut auto = x.is_none();
                let mut ch = crate::widgets::checkbox(ui, &mut auto, tl!("Auto")).changed();
                if ch {
                    *x = if auto { None } else { Some(4.0) };
                }
                if let Some(v) = x {
                    ch |= drag(ui, v, min, max);
                }
                ch
            })
            .inner
        }
        Slot::Byte(b) => ui.add(egui::DragValue::new(b).range(0..=255)).changed(),
        Slot::Bool(b) => crate::widgets::checkbox(ui, b, "").changed(),
        Slot::Pair(p, min, max) => ui.horizontal(|ui| drag(ui, &mut p.x, min, max) | drag(ui, &mut p.y, min, max)).inner,
        Slot::Shadow(s) => {
            ui.horizontal_wrapped(|ui| {
                let mut ch = false;
                for (i, v) in s.offset.iter_mut().enumerate() {
                    ch |= ui.add(egui::DragValue::new(v).range(-127..=127).prefix(if i == 0 { "x " } else { "y " })).changed();
                }
                ch |= ui.add(egui::DragValue::new(&mut s.blur).prefix(format!("{} ", tl!("Blur")))).changed();
                ch |= ui.add(egui::DragValue::new(&mut s.spread).prefix(format!("{} ", tl!("Spread")))).changed();
                let mut auto = s.color.is_none();
                if crate::widgets::checkbox(ui, &mut auto, tl!("Auto")).changed() {
                    s.color = if auto { None } else { Some(Tokens::get(ui.ctx()).shadow) };
                    ch = true;
                }
                if let Some(c) = &mut s.color {
                    ch |= crate::widgets::color_edit_button_srgba(ui, c).changed();
                }
                ch
            })
            .inner
        }
        Slot::Base(_) => {
            let _ = name;
            false
        }
    }
}

/// The package description (title, author, tags…) and its icon.
fn meta_form(app: &mut PhotocraftApp, ui: &mut egui::Ui, e: &mut Value) {
    let mut meta = e.get("meta").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
    egui::Grid::new("theme-meta-grid").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
        for (key, label) in [("title", "Title"), ("author", "Author"), ("version", "Version"), ("license", "License"), ("homepage", "Web page")] {
            ui.label(tl!(label));
            let mut v = str_of(&meta, key).to_string();
            crate::widgets::text_field(ui, egui::Id::unique(("theme-meta", key)), &mut v, "", 220.0, false);
            meta[key] = json!(v);
            ui.end_row();
        }
        ui.label(tl!("Tags"));
        let mut tags = meta.get("tags").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        crate::widgets::text_field(ui, egui::Id::unique("theme-meta-tags"), &mut tags, tl!("dark, manga, high contrast"), 220.0, false);
        meta["tags"] = json!(tags.split(',').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>());
        ui.end_row();
    });
    ui.label(tl!("Description"));
    let mut desc = str_of(&meta, "description").to_string();
    ui.add(egui::TextEdit::multiline(&mut desc).desired_rows(3).desired_width(f32::INFINITY).char_limit(2000));
    meta["description"] = json!(desc);
    e["meta"] = meta;
    ui.horizontal_wrapped(|ui| {
        let has_icon = !str_of(e, "icon").is_empty();
        ui.label(if has_icon { tl!("Icon: 256 × 256") } else { tl!("No icon") });
        if crate::widgets::secondary_button(ui, tl!("Choose icon…"), 0.0).clicked() {
            let r = app.pick_file_bytes_filtered(&["png", "jpg", "jpeg", "webp"], move |app, _name, bytes| {
                let png = icon_png(&bytes)?;
                if let Some(d) = app.ui.dialogs.iter_mut().find(|d| d.fields.get("__prefsui").and_then(Value::as_str) == Some(KIND))
                    && let Some(edit) = d.fields.get_mut("edit")
                {
                    edit["icon"] = json!(photocraft_engine::paint::tile::b64_encode(&png));
                }
                Ok(Value::Null)
            });
            if let Err(err) = r {
                app.ui.status = err;
            }
        }
        if has_icon && ui.small_button(tl!("Remove")).clicked() {
            e["icon"] = json!("");
        }
    });
}

/// An image as a package icon: cropped to its centre square and scaled to 256×256, as a PNG.
pub fn icon_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    const SIDE: u32 = photocraft_engine::package::ICON_SIDE;
    let img = photocraft_codecs::decode(bytes).map_err(|e| e.to_string())?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > 100_000_000 {
        return Err("the image is empty or too large for an icon".into());
    }
    let rgba = img.to_rgba8();
    let side = w.min(h);
    let (x0, y0) = ((w - side) / 2, (h - side) / 2);
    let mut out = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    // Each output pixel averages the source pixels it covers (or picks the nearest one when
    // scaling up).
    let step = f64::from(side) / f64::from(SIDE);
    for oy in 0..SIDE {
        let (sy0, sy1) = ((f64::from(oy) * step) as u32, (((f64::from(oy) + 1.0) * step).ceil() as u32).max((f64::from(oy) * step) as u32 + 1).min(side));
        for ox in 0..SIDE {
            let (sx0, sx1) = ((f64::from(ox) * step) as u32, (((f64::from(ox) + 1.0) * step).ceil() as u32).max((f64::from(ox) * step) as u32 + 1).min(side));
            let mut acc = [0u64; 4];
            let mut n = 0u64;
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let i = (((y0 + sy) as usize) * w as usize + (x0 + sx) as usize) * 4;
                    if let Some(px) = rgba.get(i..i + 4) {
                        for (a, v) in acc.iter_mut().zip(px) {
                            *a += u64::from(*v);
                        }
                        n += 1;
                    }
                }
            }
            out.extend(acc.map(|a| (a / n.max(1)) as u8));
        }
    }
    let img = photocraft_codecs::Image::from_raw(SIDE, SIDE, photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8, out).map_err(|e| e.to_string())?;
    photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ export

fn ticked(f: &Map<String, Value>, group: &str) -> Vec<String> {
    f.get(group).and_then(Value::as_object).map(|m| m.iter().filter(|(_, v)| v.as_bool() == Some(true)).map(|(k, _)| k.clone()).collect()).unwrap_or_default()
}

fn section_checks(ui: &mut egui::Ui, f: &mut Map<String, Value>, group: &str, available: Option<&[String]>) {
    let mut m = f.get(group).and_then(Value::as_object).cloned().unwrap_or_default();
    for (id, label) in all_sections() {
        if available.is_some_and(|a| !a.iter().any(|x| x == id)) {
            continue;
        }
        let mut on = m.get(id).and_then(Value::as_bool).unwrap_or(false);
        crate::widgets::checkbox(ui, &mut on, tl!(label));
        m.insert(id.into(), json!(on));
    }
    f.insert(group.into(), Value::Object(m));
}

fn export_tab(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(tl!("A .proj.capishot carries your settings to another computer or to your team. Never pages or images.")).color(t.text_dim));
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(tl!("Name"));
        let mut name = f.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
        crate::widgets::text_field(ui, egui::Id::unique("profile-name"), &mut name, "", 220.0, false);
        f.insert("name".into(), json!(name));
    });
    section_checks(ui, f, "export", None);
    let fonts = ticked(f, "export").iter().any(|s| s == "fontFiles");
    if fonts {
        let mut restricted = f.get("includeRestricted").and_then(Value::as_bool).unwrap_or(false);
        crate::widgets::checkbox(ui, &mut restricted, tl!("Also fonts whose licence doesn't allow sharing them"));
        f.insert("includeRestricted".into(), json!(restricted));
        ui.label(egui::RichText::new(tl!("Font files can make the file large.")).color(t.text_dim).small());
    }
    ui.add_space(6.0);
    let waiting = f.get("exportWaiting").and_then(Value::as_bool) == Some(true);
    if crate::widgets::primary_button(ui, tl!("Export profile…"), 0.0).clicked() || waiting {
        match export_profile(app, f) {
            Ok(true) => {
                f.remove("exportWaiting");
            }
            Ok(false) => {
                f.insert("exportWaiting".into(), json!(true));
                say(f, tl!("Reading your fonts…"));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
            }
            Err(e) => {
                f.remove("exportWaiting");
                say(f, e);
            }
        }
    }
}

/// Packs and saves the ticked sections; `Ok(false)` while the user's fonts it packs are still
/// being read back from storage.
fn export_profile(app: &mut PhotocraftApp, f: &mut Map<String, Value>) -> Result<bool, String> {
    let chosen = ticked(f, "export");
    let engine: Vec<&str> = photocraft_engine::prefs::profile::SECTIONS.iter().map(|(id, _)| *id).filter(|id| chosen.iter().any(|c| c == id)).collect();
    let mut profile = match app.run("profile.capture", json!({"sections": engine}))? {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    if chosen.iter().any(|c| c == "bubble")
        && let Ok(b) = serde_json::to_value(&app.ui.tool_options.bubble_styles)
    {
        profile.insert("bubble".into(), b);
    }
    let mut files = Vec::new();
    let mut families = Vec::new();
    if chosen.iter().any(|c| c == "fontFiles") {
        families = photocraft_text::user_fonts::families();
        for fam in &families {
            match crate::font_projects_ui::files_for(fam) {
                Some(mut v) => files.append(&mut v),
                None => return Ok(false),
            }
        }
    }
    let project = if chosen.iter().any(|c| c == "fontProject") { fp::active(&app.session) } else { None };
    let name = f.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Capishot profile").to_string();
    let aliases = app.session.prefs().type_.font_aliases.clone();
    let include_restricted = f.get("includeRestricted").and_then(Value::as_bool).unwrap_or(false);
    let meta = photocraft_engine::package::Meta { title: name.clone(), ..Default::default() };
    let pack = fp::Pack { name: name.clone(), project: project.as_ref(), families, files: &files, include_restricted, aliases: Some(&aliases), profile, meta, ..Default::default() };
    let out = fp::export(&pack).map_err(|e| e.to_string())?;
    let lines = crate::font_projects_ui::export_lines(&out);
    crate::font_projects_ui::save_bytes(app, &crate::font_projects_ui::package_file(&name), out.bytes);
    if !lines.is_empty() {
        crate::notices::post(app, tl!("Profile exported").to_string(), lines, false, None);
    }
    say(f, "");
    Ok(true)
}

// ------------------------------------------------------------------ import

/// The sections an opened file carries.
pub fn sections_of(imp: &fp::Imported) -> Vec<String> {
    let mut v: Vec<String> = imp.profile.keys().cloned().collect();
    if !imp.fonts.is_empty() || !imp.aliases.is_empty() {
        v.push("fontFiles".into());
    }
    if imp.project.is_some() {
        v.push("fontProject".into());
    }
    v
}

/// A profile opened: shown on the Import tab to choose what to apply.
pub fn preview(app: &mut PhotocraftApp, name: &str, imp: fp::Imported) {
    let found = sections_of(&imp);
    app.font_projects.profile_import = Some((name.to_string(), imp));
    let d = open_on(app, "import");
    if let Some(d) = app.ui.dialog_mut(d) {
        d.fields.insert("apply".into(), Value::Object(found.into_iter().map(|s| (s, json!(true))).collect()));
    }
}

fn import_tab(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    if crate::widgets::secondary_button(ui, tl!("Choose a .proj.capishot file…"), 0.0).clicked()
        && let Err(e) = app.pick_file_bytes_filtered(&["capishot", "capiproj"], |app, name, bytes| {
            let imp = fp::import(&bytes).map_err(|e| e.to_string())?;
            preview(app, &name, imp);
            Ok(Value::Null)
        })
    {
        say(f, e);
    }
    let pending = app.font_projects.profile_import.as_ref().map(|(name, imp)| (name.clone(), sections_of(imp), imp.meta.clone(), imp.fonts.len()));
    if let Some((name, found, meta, nfonts)) = pending {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(crate::i18n::fmt(tl!("{file} carries:"), &[("file", &name)])).strong());
        for (label, v) in [(tl!("Title"), &meta.title), (tl!("Author"), &meta.author), (tl!("License"), &meta.license), (tl!("Web page"), &meta.homepage)] {
            if !v.is_empty() {
                ui.label(egui::RichText::new(format!("{label}: {v}")).color(t.text_dim));
            }
        }
        if nfonts > 0 {
            ui.label(egui::RichText::new(crate::i18n::fmt(tl!("{n} font files"), &[("n", &nfonts.to_string())])).color(t.text_dim));
        }
        section_checks(ui, f, "apply", Some(&found));
        let mut merge = f.get("merge").and_then(Value::as_bool).unwrap_or(false);
        ui.horizontal_wrapped(|ui| {
            if ui.radio(!merge, tl!("Replace mine")).clicked() {
                merge = false;
            }
            if ui.radio(merge, tl!("Merge with mine")).clicked() {
                merge = true;
            }
        });
        f.insert("merge".into(), json!(merge));
        ui.label(egui::RichText::new(tl!("Your current profile is kept first, so you can undo the import.")).color(t.text_dim).small());
        ui.horizontal_wrapped(|ui| {
            if crate::widgets::primary_button(ui, tl!("Apply"), 0.0).clicked()
                && let Some((_, imp)) = app.font_projects.profile_import.take()
            {
                match apply_import(app, imp, &ticked(f, "apply"), merge) {
                    Ok(lines) => {
                        crate::notices::post(app, tl!("Profile imported").to_string(), lines, false, None);
                        say(f, tl!("Profile imported."));
                    }
                    Err(e) => say(f, e),
                }
            }
            if crate::widgets::secondary_button(ui, tl!("Cancel"), 0.0).clicked() {
                app.font_projects.profile_import = None;
            }
        });
    }
    ui.add_space(10.0);
    let can_undo = app.session.is_enabled("profile.undoImport");
    if ui.add_enabled(can_undo, egui::Button::new(tl!("Undo last profile import"))).clicked() {
        match undo_import(app) {
            Ok(()) => say(f, tl!("Your profile is back as it was before the import.")),
            Err(e) => say(f, e),
        }
    }
}

/// Applies the `sections` of an opened file (replacing, or merging with `merge`), after keeping
/// the current profile for Undo. Returns the lines of its notice.
pub fn apply_import(app: &mut PhotocraftApp, imp: fp::Imported, sections: &[String], merge: bool) -> Result<Vec<String>, String> {
    let has = |s: &str| sections.iter().any(|x| x == s);
    let engine: Vec<&String> = sections.iter().filter(|s| photocraft_engine::prefs::profile::SECTIONS.iter().any(|(id, _)| id == s)).collect();
    let bubble_now = serde_json::to_value(&app.ui.tool_options.bubble_styles).unwrap_or(Value::Null);
    app.run("profile.apply", json!({"profile": imp.profile, "sections": engine, "merge": merge, "extra": {"bubble": bubble_now}}))?;
    if has("bubble")
        && let Some(b) = imp.profile.get("bubble").and_then(|b| serde_json::from_value::<crate::state::BubbleStyles>(b.clone()).ok())
    {
        app.ui.tool_options.bubble_styles = b;
    }
    let mut lines = Vec::new();
    if has("fontFiles") {
        let sink = app.font_sink(None);
        lines.push(crate::i18n::fmt(tl!("{n} font files installed"), &[("n", &imp.fonts.len().to_string())]));
        for (file, data) in imp.fonts {
            sink.file(file, data);
        }
        // The names its fonts were given, unless the user named them already.
        for (family, alias) in &imp.aliases {
            if photocraft_engine::font_library_cmds::alias_of(&app.session, family).is_none() {
                let _ = app.run("type.font.alias", json!({"family": family, "alias": alias}));
            }
        }
    }
    if has("fontProject")
        && let Some(project) = imp.project
    {
        let id = fp::add_imported(&mut app.session, project, true);
        app.font_projects.select(id);
    }
    if !imp.referenced.is_empty() {
        lines.push(crate::i18n::fmt(tl!("Install these separately: {fonts}"), &[("fonts", &imp.referenced.join(", "))]));
    }
    app.prefs_rt.custom_theme = None;
    Ok(lines)
}

/// Undo Last Profile Import: the profile as it was, the shell's Bubble Type styles too.
pub fn undo_import(app: &mut PhotocraftApp) -> Result<(), String> {
    let r = app.run("profile.undoImport", json!({}))?;
    if let Some(b) = r["extra"].get("bubble").and_then(|b| serde_json::from_value::<crate::state::BubbleStyles>(b.clone()).ok()) {
        app.ui.tool_options.bubble_styles = b;
    }
    app.prefs_rt.custom_theme = None;
    Ok(())
}

#[cfg(test)]
#[path = "profile_ui_tests.rs"]
mod tests;
