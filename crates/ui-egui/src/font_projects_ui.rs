//! Font projects in the shell (`photocraft_engine::font_projects` keeps them): Window › Font
//! Projects… (create, rename, duplicate, delete; fonts by role, dragged between roles; each
//! role's settings with a live preview; Bubble Type's and TypeR's mappings; the pages that open
//! with the project; the fonts it needs that aren't here; import and export), the font menus'
//! sections (the project's fonts with their roles, then the recent ones; a font's context menu
//! adds it to the project), Bubble Type's role fonts, and `.capiproj` files opened or exported.

use egui::{Color32, RichText};
use photocraft_engine::font_projects::{self as fp, Project, Projects, RoleSettings, RoleStroke};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;

/// Window › Font Projects….
pub const COMMAND: &str = "window.fontProjects";

/// The shell's state of the projects window.
#[derive(Default)]
pub struct State {
    pub open: bool,
    /// The project shown in the window.
    selected: Option<String>,
    /// The window's "new role" field.
    new_role: String,
    /// The window's "new tag" field.
    new_tag: String,
    /// The export form, while it shows.
    export: Option<ExportForm>,
    /// An export waiting for the user's fonts it packs to be read back from storage.
    waiting: Option<(ExportForm, f64)>,
    /// The font menus apply a project font's role settings too (on by default).
    pub(crate) keep_settings: bool,
    /// The projects window's last import report (shown on top until closed).
    report: Option<String>,
}

impl State {
    pub fn new() -> Self {
        State { keep_settings: true, ..Default::default() }
    }
}

#[derive(Clone)]
struct ExportForm {
    id: String,
    /// Families not to pack.
    skip: Vec<String>,
    include_restricted: bool,
    with_styles: bool,
}

/// A role's name in the UI language (built-in roles are translated; the user's are their own).
pub fn role_name(project: &Project, role: &str) -> String {
    match role {
        "dialogue" => tl!("Dialogue").into(),
        "shout" => tl!("Shout").into(),
        "thought" => tl!("Thought").into(),
        "narration" => tl!("Narration").into(),
        "sfx" => tl!("SFX").into(),
        "notes" => tl!("Notes").into(),
        _ => project.role(role).map_or_else(|| role.to_string(), |r| r.name.clone()),
    }
}

fn projects(app: &PhotocraftApp) -> Projects {
    Projects::of(&app.session)
}

/// The menus' and the window's command (Window › Font Projects…).
pub fn invoke(app: &mut PhotocraftApp, id: &str) -> Option<Result<Value, String>> {
    (id == COMMAND).then(|| {
        app.font_projects.open = true;
        app.font_projects.keep_settings = true;
        Ok(json!({"open": true}))
    })
}

fn run(app: &mut PhotocraftApp, id: &str, p: Value) -> Option<Value> {
    match app.run(id, p) {
        Ok(v) => Some(v),
        Err(e) => {
            app.ui.status = e;
            app.ui.status_error = true;
            None
        }
    }
}

// ------------------------------------------------------------------ opening a page

/// A page just opened: its project (the one its PSD carries, else one its file name matches)
/// becomes the active one.
pub fn after_open(app: &mut PhotocraftApp) {
    let before = projects(app).active;
    if let Some(id) = fp::activate_for_open(&mut app.session)
        && before.as_deref() != Some(&id)
        && let Some(p) = projects(app).get(&id)
    {
        app.ui.status = crate::i18n::fmt(tl!("Font project: {name}"), &[("name", &p.name)]);
        app.ui.status_error = false;
    }
}

// ------------------------------------------------------------------ Bubble Type

/// Bubble Type's `"styles"` with the active project's roles over them: each style takes the
/// font, settings and stroke of the role the project maps it to.
pub fn bubble_styles(app: &PhotocraftApp, styles: &crate::state::BubbleStyles) -> Value {
    let mut v = crate::bubble_type_ui::styles_params(styles);
    let Some(project) = projects(app).active().cloned() else { return v };
    for kind in ["bubble", "narration", "floating"] {
        let Some(role) = project.bubble.get(kind) else { continue };
        let Some(obj) = v.get_mut(kind).and_then(Value::as_object_mut) else { continue };
        let style = project.role_style(role);
        if style.contains_key("font") {
            // The role's font in its regular face: the style's face belongs to its own font.
            obj.insert("fontStyle".into(), json!("Regular"));
            obj.remove("fauxBold");
            obj.remove("fauxItalic");
        }
        for (k, val) in style {
            obj.insert(k, val);
        }
        if let Some(st) = project.role(role).and_then(|r| r.settings.stroke.as_ref()) {
            obj.insert("stroke".into(), if st.width > 0.0 { json!({"width": st.width, "color": st.color}) } else { Value::Null });
        }
    }
    v
}

// ------------------------------------------------------------------ the font menus

/// Sets what a font row's context menu offers ([`take_add_request`] gets the choice): the active
/// project's roles. Nothing without an active project.
pub fn offer_roles(app: &PhotocraftApp, ctx: &egui::Context) {
    let roles: Vec<(String, String)> =
        projects(app).active().map(|p| p.roles.iter().map(|r| (r.id.clone(), role_name(p, &r.id))).collect()).unwrap_or_default();
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("font-row-roles"), roles));
}

/// A font row pressed and held, or right-clicked: the menu's top then offers it to the current
/// project as one of its roles ([`menu_sections`]). Within the same menu: a menu opened over
/// the font menu closed both.
pub fn row_menu(response: &egui::Response, family: &str) {
    let roles: Vec<(String, String)> = response.ctx.data(|d| d.get_temp(egui::Id::unique("font-row-roles"))).unwrap_or_default();
    if roles.is_empty() {
        return;
    }
    if response.secondary_clicked() || response.long_touched() {
        response.ctx.data_mut(|d| d.insert_temp(egui::Id::unique("font-row-adding"), family.to_string()));
    }
}

/// The font a row's context menu added to the project: done now.
pub fn take_add_request(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some((family, role)) = ctx.data_mut(|d| d.remove_temp::<(String, String)>(egui::Id::unique("font-row-add"))) else { return };
    if run(app, "fontProjects.addFont", json!({"family": family, "role": role})).is_some()
        && let Some(p) = projects(app).active()
    {
        app.ui.status =
            crate::i18n::fmt(tl!("{font} added to {project} as {role}"), &[("font", &family), ("project", &p.name), ("role", &role_name(p, &role))]);
        app.ui.status_error = false;
    }
}

/// The font menu's top (with projects only; without, the menu is as before): the projects,
/// the active project's fonts with their roles, then the recent fonts. A pick sets `current` and
/// returns the role it was picked for (`Some(None)`: a recent font).
pub fn menu_sections(app: &mut PhotocraftApp, ui: &mut egui::Ui, current: &mut String) -> Option<Option<String>> {
    let all = projects(app);
    if all.projects.is_empty() {
        return None;
    }
    let t = Tokens::get(ui.ctx());
    let mut picked = None;
    let mut activate: Option<Option<String>> = None;
    // A font held or right-clicked below: which role it joins the project as.
    let adding: Option<String> = ui.data(|d| d.get_temp(egui::Id::unique("font-row-adding")));
    if let (Some(family), Some(p)) = (adding, all.active()) {
        egui::Frame::group(ui.style()).fill(t.accent_soft).show(ui, |ui| {
            ui.label(RichText::new(crate::i18n::fmt(tl!("Add {font} to the current project as…"), &[("font", &family)])).strong());
            ui.horizontal_wrapped(|ui| {
                for r in &p.roles {
                    if ui.button(role_name(p, &r.id)).clicked() {
                        ui.ctx().data_mut(|d| {
                            d.insert_temp(egui::Id::unique("font-row-add"), (family.clone(), r.id.clone()));
                            d.remove::<String>(egui::Id::unique("font-row-adding"));
                        });
                    }
                }
                if ui.button(tl!("Cancel")).clicked() {
                    ui.ctx().data_mut(|d| d.remove::<String>(egui::Id::unique("font-row-adding")));
                }
            });
        });
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(tl!("Project:")).color(t.text_faint).size(11.5));
        for p in &all.projects {
            if ui.selectable_label(all.active.as_deref() == Some(&p.id), &p.name).clicked() && all.active.as_deref() != Some(&p.id) {
                activate = Some(Some(p.id.clone()));
            }
        }
        if ui.selectable_label(all.active.is_none(), tl!("None")).clicked() && all.active.is_some() {
            activate = Some(None);
        }
    });
    if let Some(p) = all.active() {
        ui.label(RichText::new(tl!("Current project")).color(t.text_faint).size(11.0));
        let families = crate::type_tool::families();
        for f in &p.fonts {
            let here = families.iter().any(|x| x == &f.family);
            let label = if here { f.family.clone() } else { format!("{}  ({})", f.family, tl!("missing")) };
            let on = *current == f.family;
            let r = ui.add_sized(
                [ui.available_width().min(330.0), 26.0],
                egui::Button::selectable(on, label)
                    .right_text(RichText::new(role_name(p, &f.role)).color(if on { Color32::WHITE } else { t.text_faint }).size(11.0)),
            );
            if r.clicked() && here {
                current.clone_from(&f.family);
                picked = Some(Some(f.role.clone()));
                ui.close();
            }
        }
        if p.fonts.is_empty() {
            ui.label(RichText::new(tl!("No fonts yet: right-click a font below (or press and hold) to add it.")).color(t.text_faint).size(11.0));
        }
        crate::widgets::checkbox(ui, &mut app.font_projects.keep_settings, tl!("Apply the role's settings too"));
    }
    if !all.recent.is_empty() {
        ui.label(RichText::new(tl!("Recent fonts")).color(t.text_faint).size(11.0));
        for f in all.recent.iter().take(6) {
            if ui.add_sized([ui.available_width().min(330.0), 24.0], egui::Button::selectable(*current == *f, f.as_str())).clicked() {
                current.clone_from(f);
                picked = Some(None);
                ui.close();
            }
        }
    }
    ui.label(RichText::new(tl!("All fonts")).color(t.text_faint).size(11.0));
    if let Some(id) = activate {
        set_active(app, id);
    }
    picked
}

/// Activates project `id` (or none) for this page: the page carries it, and a page name with a
/// page number is remembered for the project.
pub fn set_active(app: &mut PhotocraftApp, id: Option<String>) {
    run(app, "fontProjects.activate", json!({"id": id}));
    if let Some(id) = id
        && let Some(name) = app.session.active().map(|st| st.path.clone().unwrap_or_else(|| st.doc.name.clone()))
        && fp::page_prefix(&name).is_some()
    {
        let _ = app.run("fontProjects.rememberPattern", json!({"id": id, "file": name}));
    }
}

/// A font was picked in a menu: remembered as recent, and with `role` (picked from the project's
/// section) the role's settings go on the type target too.
pub fn picked(app: &mut PhotocraftApp, family: &str, role: Option<&str>) {
    fp::note_recent(&mut app.session, family);
    let Some(role) = role else { return };
    if !app.font_projects.keep_settings {
        return;
    }
    if let Some(p) = crate::type_tool::with_target(app, json!({"role": role, "font": false})) {
        let _ = app.run("fontProjects.applyRole", p);
    }
}

// ------------------------------------------------------------------ .capiproj

/// A `.capiproj` opened (File › Open, a drop, a link): the project is added and made active, its
/// fonts installed among the user's fonts (each file once), and Bubble Type's styles it carries
/// taken. A notice says what came and what to install separately.
pub fn import_bytes(app: &mut PhotocraftApp, name: &str, bytes: &[u8]) -> Result<(), String> {
    let imp = fp::import(bytes).map_err(|e| e.to_string())?;
    let sink = app.font_sink(None);
    let installed = imp.fonts.len();
    for (file, data) in imp.fonts {
        sink.file(file, data);
    }
    if let Some(b) = imp.bubble.as_ref().and_then(|b| serde_json::from_value::<crate::state::BubbleStyles>(b.clone()).ok()) {
        app.ui.tool_options.bubble_styles = b;
    }
    if let Some(t) = imp.typer.clone() {
        let _ = app.run("typer.library.import", json!({"json": t, "kind": "library"}));
    }
    let project_name = imp.project.name.clone();
    let id = fp::add_imported(&mut app.session, imp.project, true);
    app.font_projects.selected = Some(id);
    let mut lines = vec![crate::i18n::fmt(tl!("{n} font files installed"), &[("n", &installed.to_string())])];
    if !imp.referenced.is_empty() {
        lines.push(crate::i18n::fmt(tl!("Install these separately: {fonts}"), &[("fonts", &imp.referenced.join(", "))]));
    }
    let title = crate::i18n::fmt(tl!("Font project {name} imported"), &[("name", &project_name)]);
    crate::notices::post(app, title.clone(), lines.clone(), false, None);
    app.font_projects.report = Some(format!("{title}. {}", lines.join(". ")));
    let _ = name;
    Ok(())
}

/// The font files of `family` that a project can carry (the user's fonts and the served or
/// downloaded ones), named as the user's font store has them. `None`: one of the user's fonts
/// that isn't read back from storage yet (asked for now; try again).
fn files_for(family: &str) -> Option<Vec<fp::FontFile>> {
    let stored: Vec<(String, String)> = {
        let store = photocraft_text::user_fonts::store();
        store.entries().iter().filter(|e| e.families().iter().any(|f| f.eq_ignore_ascii_case(family))).map(|e| (e.id.clone(), e.name.clone())).collect()
    };
    let unloaded = {
        let store = photocraft_text::user_fonts::store();
        stored.iter().any(|(id, _)| !store.is_loaded(id))
    };
    if unloaded {
        photocraft_text::user_fonts::request(family);
        return None;
    }
    let files = photocraft_text::shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner).fonts.files_for_family(family);
    Some(
        files
            .into_iter()
            .map(|(name, bytes)| {
                let sha = photocraft_text::sha256::sha256_hex(&bytes);
                let name = stored.iter().find(|(id, _)| *id == sha).map_or(name, |(_, n)| n.clone());
                fp::FontFile { name, families: vec![family.to_string()], bytes }
            })
            .collect(),
    )
}

/// Packs and saves the form's project; `false` while fonts are still being read back.
fn export_now(app: &mut PhotocraftApp, form: &ExportForm) -> bool {
    let Some(project) = projects(app).get(&form.id).cloned() else { return true };
    let mut files = Vec::new();
    for f in &project.fonts {
        if form.skip.contains(&f.family) {
            continue;
        }
        match files_for(&f.family) {
            Some(mut v) => files.append(&mut v),
            None => return false,
        }
    }
    let bubble = form.with_styles.then(|| serde_json::to_value(&app.ui.tool_options.bubble_styles).ok()).flatten();
    let typer = if form.with_styles { app.run("typer.library.export", json!({})).ok() } else { None };
    let out = match fp::export(&project, &files, &form.skip, form.include_restricted, typer.as_ref(), bubble.as_ref()) {
        Ok(o) => o,
        Err(e) => {
            crate::notices::error(app, e.to_string());
            return true;
        }
    };
    let mut lines = Vec::new();
    if !out.restricted.is_empty() {
        lines.push(crate::i18n::fmt(
            tl!("Not included, its licence doesn't allow sharing it: {fonts}. Team members will need to install it separately."),
            &[("fonts", &out.restricted.join(", "))],
        ));
    }
    if !out.referenced.is_empty() {
        lines.push(crate::i18n::fmt(tl!("Named only (system or missing fonts): {fonts}"), &[("fonts", &out.referenced.join(", "))]));
    }
    if out.large {
        lines.push(crate::i18n::fmt(
            tl!("The project is large ({mb} MB): untick fonts to make it smaller."),
            &[("mb", &format!("{:.0}", out.bytes.len() as f64 / 1e6))],
        ));
    }
    let file =
        format!("{}.capiproj", project.name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect::<String>().trim());
    let bytes = out.bytes;
    if let Err(e) = app.pick_save(&file, move |app, path| {
        let write = app.services.write.as_mut().ok_or("no writer configured")?;
        write(&path, &bytes)?;
        app.ui.status = crate::i18n::fmt(tl!("Saved {file}"), &[("file", &crate::file_open::display_name(&path))]);
        app.ui.status_error = false;
        Ok(json!({"path": path}))
    }) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
    if !lines.is_empty() {
        crate::notices::post(app, tl!("Font project exported").to_string(), lines, false, None);
    }
    true
}

// ------------------------------------------------------------------ the window

/// A role's live preview: a sample line in its font and settings.
fn preview(ui: &mut egui::Ui, project: &Project, role: &str) {
    let style = project.role_style(role);
    let stroke = project.role(role).and_then(|r| r.settings.stroke_params());
    let effects = project.role(role).map(|r| r.settings.effect_params()).unwrap_or_default();
    let key = format!("{style:?}{stroke:?}{effects:?}");
    let id = egui::Id::unique(("font-role-preview", project.id.as_str(), role));
    let cached: Option<(String, Option<egui::TextureHandle>)> = ui.data(|d| d.get_temp(id));
    let tex = match cached {
        Some((k, tex)) if k == key => tex,
        _ => {
            let img = render_role(&style, stroke.as_ref(), &effects);
            let tex = img.map(|img| ui.ctx().load_texture(format!("font-role-{role}"), img, egui::TextureOptions::LINEAR));
            ui.data_mut(|d| d.insert_temp(id, (key, tex.clone())));
            tex
        }
    };
    if let Some(tex) = tex {
        let size = tex.size_vec2();
        let w = ui.available_width().min(size.x);
        ui.add(egui::Image::new((tex.id(), egui::vec2(w, size.y * w / size.x))));
    }
}

fn render_role(style: &serde_json::Map<String, Value>, stroke: Option<&Value>, effects: &[(&str, Value)]) -> Option<egui::ColorImage> {
    let (w, h) = (420u32, 90u32);
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": w, "height": h, "background": "#d9d9d9"})).ok()?;
    let mut p = Value::Object(style.clone());
    p["text"] = json!(tl!("Sample text"));
    p["size"] = json!(style.get("size").and_then(Value::as_f64).unwrap_or(24.0).min(40.0));
    p.as_object_mut()?.remove("align");
    p["x"] = json!(16);
    p["y"] = json!(f64::from(h) * 0.62);
    let r = s.execute("type.create", p).ok()?;
    if let Some(st) = stroke.filter(|st| st["size"].as_f64().unwrap_or(0.0) > 0.0) {
        let mut st = st.clone();
        st["layer"] = r["layer"].clone();
        let _ = s.execute("layer.layerStyle.stroke", st);
    }
    for (kind, fx) in effects {
        let mut fx = fx.clone();
        fx["layer"] = r["layer"].clone();
        let _ = s.execute(&format!("layer.layerStyle.{kind}"), fx);
    }
    let doc = &s.active()?.doc;
    let area = doc.bounds();
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let px: Vec<Color32> =
        photocraft_compose::render(doc, area).px.iter().map(|p| Color32::from_rgba_unmultiplied(q(p[0]), q(p[1]), q(p[2]), q(p[3]))).collect();
    Some(egui::ColorImage::new([area.width() as usize, area.height() as usize], px))
}

fn hex3(c: &str) -> [u8; 3] {
    let v = u32::from_str_radix(c.trim_start_matches('#'), 16).unwrap_or(0);
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

fn hex_of(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// A role's settings editor; returns the edited settings when they changed.
fn settings_editor(ui: &mut egui::Ui, salt: &str, s: &RoleSettings) -> Option<RoleSettings> {
    let mut n = s.clone();
    egui::Grid::new(("role-settings", salt)).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
        let opt = |ui: &mut egui::Ui, label: &str, v: &mut Option<f32>, range: std::ops::RangeInclusive<f32>, def: f32, suffix: &str| {
            let mut on = v.is_some();
            crate::widgets::checkbox(ui, &mut on, label);
            let mut x = v.unwrap_or(def);
            ui.add_enabled(on, egui::DragValue::new(&mut x).range(range).speed(0.5).suffix(suffix)).accessible_name(label);
            *v = on.then_some(x);
            ui.end_row();
        };
        opt(ui, tl!("Size"), &mut n.size_pt, 1.0..=1296.0, 24.0, " pt");
        opt(ui, tl!("Leading (0: Auto)"), &mut n.leading_pt, 0.0..=5000.0, 0.0, " pt");
        opt(ui, tl!("Tracking"), &mut n.tracking, -1000.0..=10000.0, 0.0, "");
        let mut caps = n.caps.is_some();
        crate::widgets::checkbox(ui, &mut caps, tl!("Letter case"));
        let mut c = n.caps.clone().unwrap_or_else(|| "all".into());
        ui.add_enabled_ui(caps, |ui| {
            craft_ui::ComboBox::from_id_salt(("role-caps", salt))
                .selected_text(match c.as_str() {
                    "small" => tl!("Small Caps"),
                    "normal" => tl!("Normal"),
                    _ => tl!("All Caps"),
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut c, "all".to_string(), tl!("All Caps"));
                    ui.selectable_value(&mut c, "small".to_string(), tl!("Small Caps"));
                    ui.selectable_value(&mut c, "normal".to_string(), tl!("Normal"));
                });
        });
        n.caps = caps.then_some(c);
        ui.end_row();
        let mut col = n.color.is_some();
        crate::widgets::checkbox(ui, &mut col, tl!("Color"));
        let mut rgb = hex3(n.color.as_deref().unwrap_or("#000000"));
        ui.add_enabled_ui(col, |ui| ui.color_edit_button_srgb(&mut rgb));
        n.color = col.then(|| hex_of(rgb));
        ui.end_row();
        let mut al = n.align.is_some();
        crate::widgets::checkbox(ui, &mut al, tl!("Alignment"));
        let mut a = n.align.clone().unwrap_or_else(|| "center".into());
        ui.add_enabled_ui(al, |ui| {
            ui.horizontal(|ui| {
                for (v, icon, tip) in [("left", "align-left", tl!("Left")), ("center", "align-center", tl!("Center")), ("right", "align-right", tl!("Right"))] {
                    if crate::icons::button(ui, icon, 22.0, a == v, tip).clicked() {
                        a = v.into();
                    }
                }
            });
        });
        n.align = al.then_some(a);
        ui.end_row();
        let mut st = n.stroke.is_some();
        crate::widgets::checkbox(ui, &mut st, tl!("Outline"));
        let mut k = n.stroke.clone().unwrap_or(RoleStroke { width: 3.0, color: "#ffffff".into(), position: "outside".into() });
        ui.add_enabled_ui(st, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::DragValue::new(&mut k.width).range(0.0..=250.0).speed(0.25).suffix(" px")).accessible_name(tl!("Outline"));
                let mut rgb = hex3(&k.color);
                if ui.color_edit_button_srgb(&mut rgb).changed() {
                    k.color = hex_of(rgb);
                }
                for (v, label) in [("outside", tl!("Outer")), ("center", tl!("Centered")), ("inside", tl!("Inner"))] {
                    if ui.selectable_label(k.position == v, label).clicked() {
                        k.position = v.into();
                    }
                }
            });
        });
        n.stroke = st.then_some(k);
        ui.end_row();
        // The type bars' Efectos, kept with the role.
        let color = |ui: &mut egui::Ui, c: &mut String| {
            let mut rgb = hex3(c);
            if ui.color_edit_button_srgb(&mut rgb).changed() {
                *c = hex_of(rgb);
            }
        };
        let mut on = n.gradient.is_some();
        crate::widgets::checkbox(ui, &mut on, tl!("Gradient"));
        let mut g = n.gradient.clone().unwrap_or_default();
        ui.add_enabled_ui(on, |ui| {
            ui.horizontal_wrapped(|ui| {
                color(ui, &mut g.from);
                color(ui, &mut g.to);
                ui.add(egui::DragValue::new(&mut g.angle).range(-180.0..=180.0).speed(1.0).suffix("°")).accessible_name(tl!("Angle"));
                for (v, label) in [("linear", tl!("Linear")), ("radial", tl!("Radial"))] {
                    if ui.selectable_label(g.style == v, label).clicked() {
                        g.style = v.into();
                    }
                }
            });
        });
        n.gradient = on.then_some(g);
        ui.end_row();
        let mut on = n.shadow.is_some();
        crate::widgets::checkbox(ui, &mut on, tl!("Shadow"));
        let mut d = n.shadow.clone().unwrap_or_default();
        ui.add_enabled_ui(on, |ui| {
            ui.horizontal_wrapped(|ui| {
                color(ui, &mut d.color);
                ui.add(egui::DragValue::new(&mut d.distance).range(0.0..=300.0).speed(0.25).suffix(" px")).accessible_name(tl!("Distance"));
                ui.add(egui::DragValue::new(&mut d.size).range(0.0..=250.0).speed(0.25).suffix(" px")).accessible_name(tl!("Size"));
                ui.add(egui::DragValue::new(&mut d.angle).range(-180.0..=180.0).speed(1.0).suffix("°")).accessible_name(tl!("Angle"));
                ui.add(egui::DragValue::new(&mut d.opacity).range(0.0..=100.0).speed(0.5).suffix(" %")).accessible_name(tl!("Opacity"));
            });
        });
        n.shadow = on.then_some(d);
        ui.end_row();
        let mut on = n.glow.is_some();
        crate::widgets::checkbox(ui, &mut on, tl!("Glow"));
        let mut g = n.glow.clone().unwrap_or_default();
        ui.add_enabled_ui(on, |ui| {
            ui.horizontal_wrapped(|ui| {
                color(ui, &mut g.color);
                ui.add(egui::DragValue::new(&mut g.size).range(0.0..=250.0).speed(0.25).suffix(" px")).accessible_name(tl!("Size"));
                ui.add(egui::DragValue::new(&mut g.opacity).range(0.0..=100.0).speed(0.5).suffix(" %")).accessible_name(tl!("Opacity"));
            });
        });
        n.glow = on.then_some(g);
        ui.end_row();
    });
    (n != *s).then_some(n)
}

/// Every frame: the projects window (a full-screen sheet on a phone), and an export waiting for
/// fonts.
pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if let Some((form, since)) = app.font_projects.waiting.clone() {
        let now = ctx.input(|i| i.time);
        if export_now(app, &form) || now - since > 20.0 {
            app.font_projects.waiting = None;
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    take_add_request(app, ctx);
    if !app.font_projects.open {
        return;
    }
    let all = projects(app);
    let selected = app
        .font_projects
        .selected
        .clone()
        .filter(|id| all.get(id).is_some())
        .or_else(|| all.active.clone())
        .or_else(|| all.projects.first().map(|p| p.id.clone()));
    app.font_projects.selected.clone_from(&selected);
    let screen = ctx.content_rect();
    let phone = screen.width() < 600.0;
    let mut open = true;
    let mut window = egui::Window::new(tl!("Font Projects")).id(egui::Id::unique("font-projects-window")).open(&mut open).collapsible(false);
    window = if phone { window.fixed_rect(screen).resizable(false) } else { window.default_size([760.0, 600.0]).resizable(true) };
    let mut actions: Vec<(String, Value)> = Vec::new();
    let mut select: Option<String> = None;
    let mut activate: Option<Option<String>> = None;
    let mut want_import = false;
    let mut export_form: Option<ExportForm> = None;
    window.show(ctx, |ui| {
        let t = Tokens::get(ui.ctx());
        if let Some(r) = app.font_projects.report.clone() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(r).color(t.text_dim));
                if crate::icons::button(ui, "x", 20.0, false, tl!("Close")).clicked() {
                    app.font_projects.report = None;
                }
            });
        }
        // The project list.
        ui.horizontal_wrapped(|ui| {
            craft_ui::ComboBox::from_id_salt("font-projects-pick")
                .selected_text(selected.as_deref().and_then(|id| all.get(id)).map_or_else(|| tl!("No projects yet").to_string(), |p| p.name.clone()))
                .width(200.0)
                .show_ui(ui, |ui| {
                    let mut folders: Vec<Option<String>> = all.projects.iter().map(|p| p.folder.clone()).collect();
                    folders.sort();
                    folders.dedup();
                    for folder in folders {
                        if let Some(f) = &folder {
                            ui.label(RichText::new(f).color(t.text_faint).size(11.0));
                        }
                        for p in all.projects.iter().filter(|p| p.folder == folder) {
                            if ui.selectable_label(selected.as_deref() == Some(&p.id), &p.name).clicked() {
                                select = Some(p.id.clone());
                            }
                        }
                    }
                });
            if ui.button(tl!("New")).clicked() {
                actions.push(("fontProjects.create".into(), json!({"name": tl!("New project")})));
            }
            if let Some(id) = &selected {
                if ui.button(tl!("Duplicate")).clicked() {
                    actions.push(("fontProjects.duplicate".into(), json!({"id": id})));
                }
                if ui.button(tl!("Delete")).clicked() {
                    actions.push(("fontProjects.delete".into(), json!({"id": id})));
                }
                if ui.button(tl!("Export…")).clicked() {
                    export_form = Some(ExportForm { id: id.clone(), skip: Vec::new(), include_restricted: false, with_styles: true });
                }
            }
            if ui.button(tl!("Import…")).clicked() {
                want_import = true;
            }
        });
        crate::widgets::hairline(ui);
        let Some(project) = selected.as_deref().and_then(|id| all.get(id)).cloned() else {
            ui.label(
                RichText::new(tl!("A font project keeps a series' fonts by role (dialogue, shout, narration…) and their settings. Create one to start."))
                    .color(t.text_dim),
            );
            return;
        };
        egui::ScrollArea::vertical().id_salt("font-projects-scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let mut name = project.name.clone();
                ui.label(tl!("Name"));
                if ui.add(egui::TextEdit::singleline(&mut name).desired_width(180.0)).changed() && !name.trim().is_empty() {
                    actions.push(("fontProjects.set".into(), json!({"id": project.id, "name": name})));
                }
                ui.label(tl!("Folder"));
                let mut folder = project.folder.clone().unwrap_or_default();
                if ui.add(egui::TextEdit::singleline(&mut folder).desired_width(140.0).hint_text(tl!("e.g. the scan"))).changed() {
                    actions.push(("fontProjects.set".into(), json!({"id": project.id, "folder": folder})));
                }
                let active = all.active.as_deref() == Some(&project.id);
                if ui.selectable_label(active, if active { tl!("Active") } else { tl!("Use for this page") }).clicked() {
                    activate = Some((!active).then(|| project.id.clone()));
                }
            });
            // Fonts by role: a font dragged onto another role moves there.
            ui.add_space(6.0);
            ui.label(RichText::new(tl!("Fonts by role")).font(crate::theme::semibold(13.0)));
            let families = crate::type_tool::families();
            for role in &project.roles {
                let frame = egui::Frame::group(ui.style()).inner_margin(egui::Margin::same(6));
                let (_, dropped) = ui.dnd_drop_zone::<String, _>(frame, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(role_name(&project, &role.id)).strong());
                        for f in project.fonts.iter().filter(|f| f.role == role.id) {
                            let here = families.iter().any(|x| x == &f.family);
                            let id = egui::Id::unique(("font-chip", project.id.as_str(), f.family.as_str()));
                            ui.dnd_drag_source(id, f.family.clone(), |ui| {
                                let label =
                                    if here { RichText::new(&f.family) } else { RichText::new(format!("{} ({})", f.family, tl!("missing"))).color(t.warning) };
                                ui.label(label);
                            });
                            if crate::icons::button(ui, "x", 16.0, false, tl!("Remove from the project")).clicked() {
                                actions.push(("fontProjects.removeFont".into(), json!({"id": project.id, "family": f.family})));
                            }
                        }
                        let mut add = String::new();
                        if crate::type_tool::family_picker_in(
                            ui,
                            &format!("font-role-add-{}", role.id),
                            &mut add,
                            140.0,
                            &families,
                            photocraft_engine::prefs::FontPreview::Off,
                            &mut crate::type_tool::FontLibrary::default(),
                        ) && !add.is_empty()
                        {
                            actions.push(("fontProjects.addFont".into(), json!({"id": project.id, "family": add, "role": role.id})));
                        }
                        if !role.builtin && crate::icons::button(ui, "trash", 18.0, false, tl!("Remove this role")).clicked() {
                            actions.push(("fontProjects.removeRole".into(), json!({"id": project.id, "role": role.id})));
                        }
                    });
                    egui::CollapsingHeader::new(tl!("Settings")).id_salt(("role-settings-open", role.id.as_str())).default_open(role.id == "dialogue").show(
                        ui,
                        |ui| {
                            if let Some(n) = settings_editor(ui, &role.id, &role.settings) {
                                actions.push(("fontProjects.setRole".into(), json!({"id": project.id, "role": role.id, "settings": n})));
                            }
                            preview(ui, &project, &role.id);
                        },
                    );
                });
                if let Some(family) = dropped {
                    actions.push(("fontProjects.addFont".into(), json!({"id": project.id, "family": *family, "role": role.id})));
                }
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut app.font_projects.new_role).hint_text(tl!("New role")).desired_width(160.0));
                if ui.button(tl!("Add role")).clicked() && !app.font_projects.new_role.trim().is_empty() {
                    let name = app.font_projects.new_role.trim().to_string();
                    let id: String = name.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
                    actions
                        .push(("fontProjects.setRole".into(), json!({"id": project.id, "role": if id.is_empty() { "role".into() } else { id }, "name": name})));
                    app.font_projects.new_role.clear();
                }
            });
            let role_combo = |ui: &mut egui::Ui, salt: &str, current: Option<&String>| -> Option<Option<String>> {
                let mut out = None;
                craft_ui::ComboBox::from_id_salt(salt).selected_text(current.map_or_else(|| tl!("None").to_string(), |r| role_name(&project, r))).show_ui(
                    ui,
                    |ui| {
                        if ui.selectable_label(current.is_none(), tl!("None")).clicked() {
                            out = Some(None);
                        }
                        for r in &project.roles {
                            if ui.selectable_label(current == Some(&r.id), role_name(&project, &r.id)).clicked() {
                                out = Some(Some(r.id.clone()));
                            }
                        }
                    },
                );
                out
            };
            // Bubble Type.
            ui.add_space(6.0);
            ui.label(RichText::new(tl!("Bubble Type styles")).font(crate::theme::semibold(13.0)));
            egui::Grid::new("font-projects-bubble").num_columns(2).show(ui, |ui| {
                for (kind, label) in [("bubble", tl!("Bubble")), ("narration", tl!("Narration")), ("floating", tl!("Floating"))] {
                    ui.label(label);
                    if let Some(r) = role_combo(ui, &format!("fp-bubble-{kind}"), project.bubble.get(kind)) {
                        actions.push(("fontProjects.setMapping".into(), json!({"id": project.id, "bubble": {kind: r}})));
                    }
                    ui.end_row();
                }
            });
            // TypeR tags.
            ui.add_space(6.0);
            ui.label(RichText::new(tl!("TypeR tags")).font(crate::theme::semibold(13.0)));
            egui::Grid::new("font-projects-tags").num_columns(3).show(ui, |ui| {
                for (tag, role) in &project.tags {
                    ui.label(format!("{tag}:"));
                    if let Some(r) = role_combo(ui, &format!("fp-tag-{tag}"), Some(role)) {
                        actions.push(("fontProjects.setMapping".into(), json!({"id": project.id, "tags": {tag: r}})));
                    }
                    if crate::icons::button(ui, "x", 16.0, false, tl!("Remove")).clicked() {
                        actions.push(("fontProjects.setMapping".into(), json!({"id": project.id, "tags": {tag: Value::Null}})));
                    }
                    ui.end_row();
                }
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut app.font_projects.new_tag).hint_text(tl!("TAG")).desired_width(100.0));
                if ui.button(tl!("Add tag")).clicked() && !app.font_projects.new_tag.trim().is_empty() {
                    let tag = app.font_projects.new_tag.trim().trim_end_matches(':').to_uppercase();
                    actions.push(("fontProjects.setMapping".into(), json!({"id": project.id, "tags": {tag: "dialogue"}})));
                    app.font_projects.new_tag.clear();
                }
            });
            // Pages.
            ui.add_space(6.0);
            ui.label(RichText::new(tl!("Pages that open with this project")).font(crate::theme::semibold(13.0)));
            for p in &project.patterns {
                ui.horizontal(|ui| {
                    ui.label(p.strip_prefix("folder:").map_or_else(|| format!("{p}…"), |f| crate::i18n::fmt(tl!("folder {name}"), &[("name", f)])));
                });
            }
            let page = app.session.active().map(|st| st.path.clone().unwrap_or_else(|| st.doc.name.clone()));
            if let Some(prefix) = page.as_deref().and_then(fp::page_prefix)
                && !project.patterns.contains(&prefix)
                && ui.button(crate::i18n::fmt(tl!("Remember pages like {name}"), &[("name", &format!("{prefix}…"))])).clicked()
            {
                actions.push(("fontProjects.rememberPattern".into(), json!({"id": project.id})));
            }
            // What's missing.
            let missing: Vec<&str> = project.fonts.iter().map(|f| f.family.as_str()).filter(|f| !families.iter().any(|x| x == f)).collect();
            if !missing.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new(crate::i18n::fmt(tl!("Missing fonts: {fonts}"), &[("fonts", &missing.join(", "))])).color(t.warning));
                if ui.button(tl!("Load Fonts…")).clicked() {
                    actions.push(("ui:type.loadFonts".into(), json!({})));
                }
            }
        });
    });
    if !open {
        app.font_projects.open = false;
    }
    if let Some(id) = select {
        app.font_projects.selected = Some(id);
    }
    if let Some(a) = activate {
        set_active(app, a);
    }
    for (id, p) in actions {
        if let Some(ui_id) = id.strip_prefix("ui:") {
            if let Err(e) = crate::menus::invoke(app, ctx, ui_id, p)
                && e != crate::file_dialog::CANCELLED
            {
                app.ui.status = e;
            }
            continue;
        }
        if let Some(v) = run(app, &id, p)
            && let Some(new) = v.get("id").and_then(Value::as_str)
            && matches!(id.as_str(), "fontProjects.create" | "fontProjects.duplicate")
        {
            app.font_projects.selected = Some(new.to_string());
        }
    }
    if want_import {
        let _ = crate::menus::invoke(app, ctx, "file.open", json!({}));
    }
    if let Some(f) = export_form {
        app.font_projects.export = Some(f);
    }
    export_dialog(app, ctx);
}

/// The export form: which fonts go in (restricted ones unticked, with why), the styles too.
fn export_dialog(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(mut form) = app.font_projects.export.clone() else { return };
    let Some(project) = projects(app).get(&form.id).cloned() else {
        app.font_projects.export = None;
        return;
    };
    let mut go = false;
    let mut cancel = false;
    egui::Modal::new(egui::Id::unique("font-project-export")).show(ctx, |ui| {
        let t = Tokens::get(ui.ctx());
        ui.set_width(ctx.content_rect().width().min(460.0) - 40.0);
        ui.label(RichText::new(crate::i18n::fmt(tl!("Export {name}"), &[("name", &project.name)])).font(crate::theme::semibold(15.0)));
        ui.label(RichText::new(tl!("A .capiproj carries the project's settings and its font files, never pages or images.")).color(t.text_dim).size(12.0));
        ui.add_space(6.0);
        for f in &project.fonts {
            let files = photocraft_text::shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner).fonts.files_for_family(&f.family);
            let stored = photocraft_text::user_fonts::store().is_user_family(&f.family);
            let restricted = files.iter().any(|(_, b)| fp::restricted(b));
            let mut on = !form.skip.contains(&f.family);
            ui.horizontal(|ui| {
                if crate::widgets::checkbox(ui, &mut on, &f.family).changed() {
                    if on {
                        form.skip.retain(|x| x != &f.family);
                    } else {
                        form.skip.push(f.family.clone());
                    }
                }
                let size: u64 = files.iter().map(|(_, b)| b.len() as u64).sum();
                if files.is_empty() && !stored {
                    ui.label(RichText::new(tl!("system font: named only")).color(t.text_faint).size(11.5));
                } else if size > 0 {
                    ui.label(RichText::new(format!("{:.1} MB", size as f64 / 1e6)).color(t.text_faint).size(11.5));
                }
            });
            if restricted && !form.include_restricted {
                ui.label(
                    RichText::new(tl!("This font doesn't allow redistribution; team members will need to install it separately.")).color(t.warning).size(11.5),
                );
            }
        }
        crate::widgets::checkbox(ui, &mut form.include_restricted, tl!("Include fonts whose licence restricts sharing"));
        crate::widgets::checkbox(ui, &mut form.with_styles, tl!("Include TypeR's styles and Bubble Type's styles"));
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            go = crate::widgets::primary_button(ui, tl!("Export"), 100.0).clicked();
            cancel = crate::widgets::secondary_button(ui, tl!("Cancel"), 84.0).clicked();
        });
    });
    if cancel {
        app.font_projects.export = None;
        return;
    }
    if go {
        app.font_projects.export = None;
        if !export_now(app, &form) {
            app.font_projects.waiting = Some((form, ctx.input(|i| i.time)));
        }
        return;
    }
    app.font_projects.export = Some(form);
}

/// `ui.inspect`'s `fontProjects`.
pub fn inspect(app: &PhotocraftApp) -> Value {
    let all = projects(app);
    json!({"open": app.font_projects.open, "selected": app.font_projects.selected, "active": all.active, "count": all.projects.len(), "exporting": app.font_projects.export.is_some()})
}

#[cfg(test)]
#[path = "font_projects_ui_tests.rs"]
mod tests;
