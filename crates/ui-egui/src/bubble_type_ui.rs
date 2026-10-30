//! The Bubble Type tool's styles: "Texto globo", "Texto narración" and "Texto flotante", picked
//! by what a click hits (`type.createInBubble`'s `"style": "auto"`) or forced from the options
//! bar's chip; edited in the "Estilos" popover with a live preview; the chosen one shown for a
//! moment by the cursor.

use egui::{Color32, Pos2, Stroke, vec2};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::state::{BubbleStyles, BubbleTextStyle};

/// The picks, in the picker's order: the command's `"style"` value and its label.
pub const PICKS: [(&str, &str); 4] = [("auto", "Auto"), ("bubble", "Bubble"), ("narration", "Narration"), ("floating", "Floating")];

/// A style's name as shown.
pub fn style_title(kind: &str) -> &'static str {
    match kind {
        "narration" => tl!("Narration text"),
        "floating" => tl!("Floating text"),
        _ => tl!("Bubble text"),
    }
}

fn pick_label(kind: &str) -> &'static str {
    match kind {
        "bubble" => tl!("Bubble"),
        "narration" => tl!("Narration"),
        "floating" => tl!("Floating"),
        _ => tl!("Auto"),
    }
}

/// `type.createInBubble`'s keys for one style. A face the family lacks is faked.
pub fn style_params(st: &BubbleTextStyle) -> Value {
    let faces = crate::type_tool::styles(&st.font);
    let (face, faux_italic, faux_bold) = if faces.iter().any(|f| f == &st.style) {
        (st.style.clone(), false, false)
    } else {
        ("Regular".to_string(), st.style.contains("Italic"), st.style.contains("Bold") || st.style.contains("Black"))
    };
    let mut v = json!({
        "font": st.font,
        "fontStyle": face,
        "size": st.size,
        "leading": st.leading.map_or(json!("auto"), |l| json!(l)),
        "tracking": st.tracking,
        "color": st.color,
        "align": st.align,
        "caps": if st.caps { "all" } else { "normal" },
        "inset": if st.auto_margin { json!("auto") } else { json!(st.margin) },
        "vertical": if st.top { "top" } else { "center" },
        "stroke": if st.stroke { json!({"width": st.stroke_width, "color": st.stroke_color}) } else { Value::Null },
    });
    if faux_italic {
        v["fauxItalic"] = json!(true);
    }
    if faux_bold {
        v["fauxBold"] = json!(true);
    }
    v
}

/// All three, as `"styles"`.
pub fn styles_params(s: &BubbleStyles) -> Value {
    json!({"bubble": style_params(&s.bubble), "narration": style_params(&s.narration), "floating": style_params(&s.floating)})
}

// ------------------------------------------------------------------ the chosen style, by the cursor

/// How long the chosen style shows by the cursor (s).
const FLASH_SECONDS: f64 = 1.4;

/// Show `kind` by the click at `at` (document px) from the next frame.
pub fn flash(app: &mut PhotocraftApp, kind: &str, at: [f64; 2]) {
    app.bubble_flash = Some((kind.to_string(), at, -1.0));
}

/// Draws the chosen style's name by the click for a moment.
pub fn draw_flash(app: &mut PhotocraftApp, painter: &egui::Painter, xf: &crate::canvas::ViewXform) {
    let ctx = painter.ctx();
    let now = ctx.input(|i| i.time);
    let f = &mut app.bubble_flash;
    let Some((kind, at, start)) = f.clone() else { return };
    // The first frame after the click starts the clock (the click's time is the app's).
    let start = if start < 0.0 {
        *f = Some((kind.clone(), at, now));
        now
    } else {
        start
    };
    let age = now - start;
    if !(0.0..FLASH_SECONDS).contains(&age) {
        *f = None;
        return;
    }
    let alpha = if age > FLASH_SECONDS - 0.3 { ((FLASH_SECONDS - age) / 0.3) as f32 } else { 1.0 };
    let t = crate::theme::Tokens::get(ctx);
    let p = xf.to_screen(at[0] as f32, at[1] as f32) + vec2(16.0, 18.0);
    let font = crate::theme::medium(13.0);
    // A kind shows its style's title; another source's style comes with its own name.
    let title = if PICKS.iter().any(|(k, _)| *k == kind) { style_title(&kind).to_string() } else { kind.clone() };
    let galley = painter.layout_no_wrap(title, font, t.text.gamma_multiply(alpha));
    let r = egui::Rect::from_min_size(p, galley.size() + vec2(16.0, 8.0));
    painter.rect(r, 6.0, t.chrome.gamma_multiply(0.95 * alpha), Stroke::new(1.0, t.accent.gamma_multiply(alpha)), egui::StrokeKind::Inside);
    painter.galley(r.min + vec2(8.0, 4.0), galley, Color32::WHITE);
    ctx.request_repaint();
}

// ------------------------------------------------------------------ the options bar

/// The chip (Auto / Globo / Narración / Flotante) and the Estilos button with its popover.
pub fn options(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    crate::widgets::vline(ui, 22.0);
    picker_button(app, ui, 150.0);
    let open_id = egui::Id::unique("bubble-styles-open");
    let open = ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false));
    let button = ui.add(egui::Button::new(tl!("Styles")).selected(open)).on_hover_text(tl!("The text styles for bubbles, narration boxes and floating text"));
    if button.clicked() {
        ui.data_mut(|d| d.insert_temp(open_id, !open));
    }
    if ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false)) {
        let area = egui::Area::new(egui::Id::unique("bubble-styles"))
            .order(egui::Order::Foreground)
            .fixed_pos(button.rect.left_bottom() + vec2(0.0, 6.0))
            .show(ui.ctx(), |ui| {
                let t = crate::theme::Tokens::get(ui.ctx());
                egui::Frame::popup(ui.style()).fill(t.chrome).show(ui, |ui| {
                    ui.set_width(380.0);
                    popover(app, ui);
                });
            });
        let clicked_outside = ui.input(|i| i.pointer.any_pressed())
            && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| !area.response.rect.contains(p) && !button.rect.contains(p))
            && !ui.ctx().any_popup_open();
        if clicked_outside || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            ui.data_mut(|d| d.insert_temp(open_id, false));
        }
    }
}

/// The popover: one tab per style, its fields and a live preview.
fn popover(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tab_id = egui::Id::unique("bubble-styles-tab");
    let mut tab: String = ui.data(|d| d.get_temp(tab_id)).unwrap_or_else(|| "bubble".to_string());
    ui.horizontal(|ui| {
        for k in ["bubble", "narration", "floating"] {
            if ui.selectable_label(tab == k, style_title(k)).clicked() {
                tab = k.to_string();
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(tab_id, tab.clone()));
    ui.separator();
    let mut st = match tab.as_str() {
        "narration" => app.ui.tool_options.bubble_styles.narration.clone(),
        "floating" => app.ui.tool_options.bubble_styles.floating.clone(),
        _ => app.ui.tool_options.bubble_styles.bubble.clone(),
    };
    let before = st.clone();
    style_fields(app, ui, &tab, &mut st);
    ui.separator();
    preview(ui, &tab, &st);
    ui.horizontal(|ui| {
        if ui.small_button(tl!("Reset")).on_hover_text(tl!("Back to this style's defaults")).clicked() {
            let d = BubbleStyles::default();
            st = match tab.as_str() {
                "narration" => d.narration,
                "floating" => d.floating,
                _ => d.bubble,
            };
        }
    });
    if st != before {
        let s = &mut app.ui.tool_options.bubble_styles;
        match tab.as_str() {
            "narration" => s.narration = st,
            "floating" => s.floating = st,
            _ => s.bubble = st,
        }
    }
}

fn hex_rgb(h: &str) -> [u8; 3] {
    let v = h.strip_prefix('#').and_then(|h| u32::from_str_radix(h, 16).ok()).unwrap_or(0);
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

fn rgb_hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(vec2(96.0, 20.0), egui::Label::new(label));
        add(ui);
    });
}

fn style_fields(app: &mut PhotocraftApp, ui: &mut egui::Ui, tab: &str, st: &mut BubbleTextStyle) {
    row(ui, tl!("Font"), |ui| {
        if crate::type_tool::fonts_ready(ui, 250.0) {
            let mut fam = st.font.clone();
            if crate::type_tool::font_picker_salted(app, ui, &format!("bubble-font-{tab}"), &mut fam, 160.0).is_some() {
                st.font = fam;
            }
            let mut faces = crate::type_tool::styles(&st.font);
            // Bold and Italic are offered even when faked.
            for f in ["Italic", "Bold"] {
                if !faces.iter().any(|x| x == f) {
                    faces.push(f.into());
                }
            }
            let labels: Vec<(String, String)> = faces.iter().map(|f| (f.clone(), crate::type_tool::style_label(f))).collect();
            let opts: Vec<(String, &str)> = labels.iter().map(|(a, b)| (a.clone(), b.as_str())).collect();
            crate::widgets::dropdown(ui, &format!("bubble-face-{tab}"), "", &mut st.style, &opts, 84.0);
        }
    });
    row(ui, tl!("Size"), |ui| {
        crate::widgets::value_field(ui, &mut st.size, 1.0..=400.0, "pt", 64.0);
    });
    row(ui, tl!("Leading"), |ui| {
        let mut auto = st.leading.is_none();
        if ui.checkbox(&mut auto, tl!("Auto")).changed() {
            st.leading = if auto { None } else { Some((st.size * 1.2).round()) };
        }
        if let Some(l) = st.leading.as_mut() {
            crate::widgets::value_field(ui, l, 1.0..=1000.0, "pt", 64.0);
        }
    });
    row(ui, tl!("Tracking"), |ui| {
        crate::widgets::value_field(ui, &mut st.tracking, -1000.0..=10000.0, "", 64.0);
    });
    row(ui, tl!("Color"), |ui| {
        let mut c = hex_rgb(&st.color);
        if crate::widgets::color_edit_button_srgb(ui, &mut c).changed() {
            st.color = rgb_hex(c);
        }
    });
    row(ui, tl!("Alignment"), |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (align, icon, tip) in
            [("left", "align-left", tl!("Left align text")), ("center", "align-center", tl!("Center text")), ("right", "align-right", tl!("Right align text"))]
        {
            if crate::icons::button(ui, icon, 22.0, st.align == align, tip).clicked() {
                st.align = align.into();
            }
        }
    });
    row(ui, tl!("Case"), |ui| {
        if ui.selectable_label(!st.caps, tl!("Normal")).clicked() {
            st.caps = false;
        }
        if ui.selectable_label(st.caps, tl!("ALL CAPS")).clicked() {
            st.caps = true;
        }
    });
    row(ui, tl!("Stroke"), |ui| {
        ui.checkbox(&mut st.stroke, "");
        ui.add_enabled_ui(st.stroke, |ui| {
            let mut c = hex_rgb(&st.stroke_color);
            if crate::widgets::color_edit_button_srgb(ui, &mut c).changed() {
                st.stroke_color = rgb_hex(c);
            }
            crate::widgets::value_field(ui, &mut st.stroke_width, 0.0..=100.0, "px", 56.0);
        });
    });
    if tab != "floating" {
        row(ui, tl!("Margin"), |ui| {
            ui.add_enabled_ui(!st.auto_margin, |ui| crate::widgets::value_field(ui, &mut st.margin, 0.0..=1000.0, "px", 56.0));
            ui.checkbox(&mut st.auto_margin, tl!("Auto")).on_hover_text(tl!("A margin of 5% of the bubble's smaller side"));
        });
        row(ui, tl!("Lines"), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if crate::icons::button(ui, "align-vertical-justify-start", 22.0, st.top, tl!("Lines from the top of the bubble")).clicked() {
                st.top = true;
            }
            if crate::icons::button(ui, "align-vertical-justify-center", 22.0, !st.top, tl!("Lines centered in the bubble")).clicked() {
                st.top = false;
            }
        });
    }
}

/// A sample in the style, drawn by the type engine (a small document, the floating style's
/// command), cached until the style changes.
fn preview(ui: &mut egui::Ui, tab: &str, st: &BubbleTextStyle) {
    let key = format!("{tab}{}", serde_json::to_string(st).unwrap_or_default());
    let id = egui::Id::unique("bubble-style-preview");
    let cached: Option<(String, egui::TextureHandle)> = ui.data(|d| d.get_temp(id));
    let tex = match cached {
        Some((k, tex)) if k == key => tex,
        _ => {
            let Some(img) = render_sample(st) else { return };
            let tex = ui.ctx().load_texture("bubble-style-preview", img, egui::TextureOptions::LINEAR);
            ui.data_mut(|d| d.insert_temp(id, (key, tex.clone())));
            tex
        }
    };
    let size = tex.size_vec2();
    let w = ui.available_width().min(size.x);
    ui.add(egui::Image::new((tex.id(), vec2(w, size.y * w / size.x))));
}

/// Preview size (px) and its background.
const PREVIEW: (u32, u32) = (380, 110);

fn render_sample(st: &BubbleTextStyle) -> Option<egui::ColorImage> {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": PREVIEW.0, "height": PREVIEW.1, "background": "#d9d9d9"})).ok()?;
    let mut floating = style_params(st);
    // The sample sits in the middle, whatever the style's alignment or margin.
    if let Value::Object(m) = &mut floating {
        m.remove("inset");
        m.remove("vertical");
        m.insert("size".into(), json!(st.size.min(48.0)));
    }
    let x = match st.align.as_str() {
        "left" => 16.0,
        "right" => f64::from(PREVIEW.0) - 16.0,
        _ => f64::from(PREVIEW.0) / 2.0,
    };
    s.execute(
        "type.createInBubble",
        json!({"x": x, "y": f64::from(PREVIEW.1) / 2.0, "style": "floating", "text": tl!("Sample text"), "styles": {"floating": floating}}),
    )
    .ok()?;
    let doc = &s.active()?.doc;
    let area = doc.bounds();
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let px: Vec<Color32> =
        photocraft_compose::render(doc, area).px.iter().map(|p| Color32::from_rgba_unmultiplied(q(p[0]), q(p[1]), q(p[2]), q(p[3]))).collect();
    Some(egui::ColorImage::new([area.width() as usize, area.height() as usize], px))
}

/// The style shown by the cursor and the point it was used at.
pub fn flashing(app: &PhotocraftApp) -> Option<(String, Pos2)> {
    app.bubble_flash.as_ref().map(|(k, at, _)| (k.clone(), Pos2::new(at[0] as f32, at[1] as f32)))
}

// ------------------------------------------------------------------ the style picker

/// How many recently used styles the picker lists.
const RECENT: usize = 5;

/// One section of the picker: its title and its styles (the `"style"` value and the label).
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub title: String,
    pub items: Vec<(String, String)>,
}

/// The name a style shows (`"auto"`, a kind, `project:<role>`, `typer:<id>`); `None` for one that
/// no longer exists.
pub fn label_of(app: &PhotocraftApp, key: &str) -> Option<String> {
    if let Some((_, _)) = PICKS.iter().find(|(k, _)| *k == key) {
        return Some(if key == "auto" { tl!("Automatic").into() } else { style_title(key).into() });
    }
    let r = photocraft_engine::type_bubble_cmds::StyleRef::parse(key)?;
    match &r {
        photocraft_engine::type_bubble_cmds::StyleRef::Project(role) => {
            let p = photocraft_engine::font_projects::active(&app.session)?;
            p.role(role)?;
            Some(crate::font_projects_ui::role_name(&p, role))
        }
        photocraft_engine::type_bubble_cmds::StyleRef::Typer(_) => photocraft_engine::type_bubble_cmds::ref_name(&app.session, &r),
    }
}

/// The picker's sections: Recent, Automatic, Bubble Type's own three, the active project's roles
/// and the TypeR library (folder by folder; `search` filters its styles by name or folder).
pub fn sections(app: &PhotocraftApp, search: &str) -> Vec<Section> {
    let mut out = Vec::new();
    let recent: Vec<(String, String)> =
        app.ui.tool_options.bubble_recent.iter().filter_map(|k| label_of(app, k).map(|l| (k.clone(), l))).take(RECENT).collect();
    if !recent.is_empty() && search.is_empty() {
        out.push(Section { title: tl!("Recent").into(), items: recent });
    }
    out.push(Section { title: tl!("Automatic").into(), items: vec![("auto".into(), tl!("Automatic (by shape)").into())] });
    out.push(Section {
        title: tl!("Bubble Type").into(),
        items: ["bubble", "narration", "floating"].iter().map(|k| ((*k).into(), style_title(k).into())).collect(),
    });
    if let Some(p) = photocraft_engine::font_projects::active(&app.session) {
        let items = p.roles.iter().map(|r| (format!("project:{}", r.id), crate::font_projects_ui::role_name(&p, &r.id))).collect();
        out.push(Section { title: crate::i18n::fmt(tl!("Project: {name}"), &[("name", &p.name)]), items });
    }
    let lib = &app.session.typer.state.library;
    if !lib.styles.is_empty() {
        let folders: std::collections::HashMap<String, String> = lib.flatten_tree().into_iter().map(|f| (f.id.to_string(), f.label)).collect();
        let needle = search.trim().to_lowercase();
        let mut items = Vec::new();
        for st in &lib.styles {
            let folder = st.folder.as_ref().and_then(|f| folders.get(&f.to_string())).cloned();
            let label = match &folder {
                Some(f) => format!("{f} / {}", st.name),
                None => st.name.clone(),
            };
            if needle.is_empty() || label.to_lowercase().contains(&needle) {
                items.push((format!("typer:{}", st.id), label));
            }
        }
        out.push(Section { title: "TypeR".into(), items });
    }
    out
}

/// Use `key` for the next bubbles; it goes first among the recent ones.
pub fn pick(app: &mut PhotocraftApp, key: &str) {
    let o = &mut app.ui.tool_options;
    o.bubble_pick = key.to_string();
    o.bubble_recent.retain(|k| k != key);
    o.bubble_recent.insert(0, key.to_string());
    o.bubble_recent.truncate(RECENT * 2);
}

/// Auto's style for each shape kind ("Remember by shape"): the active project's mapping, else
/// the tool's own.
pub fn shape_style(app: &PhotocraftApp, kind: &str) -> String {
    match photocraft_engine::font_projects::active(&app.session) {
        Some(p) => p
            .bubble
            .get(kind)
            .map(|v| if v.contains(':') || PICKS.iter().any(|(k, _)| k == v) { v.clone() } else { format!("project:{v}") })
            .unwrap_or_else(|| kind.to_string()),
        None => app.ui.tool_options.bubble_shape_styles.get(kind).cloned().unwrap_or_else(|| kind.to_string()),
    }
}

/// Map shape `kind` to `key` in Auto: in the active project when there is one.
pub fn set_shape_style(app: &mut PhotocraftApp, kind: &str, key: &str) {
    if photocraft_engine::font_projects::active(&app.session).is_some() {
        // A role keeps the project's own spelling (its id), so older builds read it too.
        let v = key.strip_prefix("project:").unwrap_or(key);
        let _ = app.run("fontProjects.setMapping", json!({"bubble": {kind: v}}));
    } else if key == kind {
        app.ui.tool_options.bubble_shape_styles.remove(kind);
    } else {
        app.ui.tool_options.bubble_shape_styles.insert(kind.to_string(), key.to_string());
    }
}

/// The `"shapeStyles"` a click sends (no project active; with one the engine reads its mapping).
pub fn shape_styles_param(app: &PhotocraftApp) -> Option<Value> {
    if photocraft_engine::font_projects::active(&app.session).is_some() || app.ui.tool_options.bubble_shape_styles.is_empty() {
        return None;
    }
    serde_json::to_value(&app.ui.tool_options.bubble_shape_styles).ok()
}

const OPEN: &str = "bubble-picker-open";
const ANCHOR: &str = "bubble-picker-anchor";
/// Set when the picker opens: its list starts at the top (Recent).
const FRESH: &str = "bubble-picker-fresh";

/// Open the picker (the phone's type bar and options call this).
pub fn open_picker(ctx: &egui::Context, anchor: egui::Rect) {
    ctx.data_mut(|d| {
        d.insert_temp(egui::Id::new(OPEN), true);
        d.insert_temp(egui::Id::new(ANCHOR), anchor);
        d.insert_temp(egui::Id::new(FRESH), true);
    });
}

pub fn picker_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::new(OPEN)).unwrap_or(false))
}

fn close_picker(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(OPEN), false));
}

/// The button showing the style in use; a click opens the picker.
pub fn picker_button(app: &mut PhotocraftApp, ui: &mut egui::Ui, width: f32) -> egui::Response {
    let key = app.ui.tool_options.bubble_pick.clone();
    let label = label_of(app, &key).unwrap_or_else(|| style_title("bubble").into());
    let resp = ui
        .add_sized(vec2(width, 24.0), egui::Button::new(egui::RichText::new(format!("{label}    "))).truncate().selected(picker_open(ui.ctx())))
        .on_hover_text(tl!("The style the next bubble uses: Bubble Type's, the font project's or a TypeR style"));
    // A small chevron at the right end: it opens a list.
    let c = egui::pos2(resp.rect.right() - 10.0, resp.rect.center().y);
    let col = crate::theme::Tokens::get(ui.ctx()).text_dim;
    ui.painter().add(egui::Shape::convex_polygon(vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 3.0)], col, Stroke::NONE));
    crate::touch_ui::note_rect(ui.ctx(), "bubbleStyle", resp.rect);
    if resp.clicked() {
        if picker_open(ui.ctx()) {
            close_picker(ui.ctx());
        } else {
            open_picker(ui.ctx(), resp.rect);
        }
    }
    resp
}

/// The picker, when open: a popup under its button, a sheet on a phone.
pub fn picker(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !picker_open(ctx) {
        return;
    }
    let screen = ctx.content_rect();
    let phone = screen.width() < crate::simple_ui::PHONE_BELOW;
    let anchor: egui::Rect = ctx.data(|d| d.get_temp(egui::Id::new(ANCHOR))).unwrap_or(egui::Rect::from_min_size(screen.center(), vec2(1.0, 1.0)));
    let t = crate::theme::Tokens::get(ctx);
    let (pos, width, height) = if phone {
        let h = (screen.height() * 0.7).min(560.0);
        (egui::pos2(screen.left() + 8.0, screen.bottom() - h - 8.0), screen.width() - 16.0, h)
    } else {
        (anchor.left_bottom() + vec2(0.0, 6.0), 320.0, (screen.bottom() - anchor.bottom() - 24.0).clamp(220.0, 520.0))
    };
    let area = egui::Area::new(egui::Id::new("bubble-picker")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        let frame = if phone {
            egui::Frame::NONE
                .fill(t.card)
                .stroke(Stroke::new(1.0, t.card_border))
                .corner_radius(egui::CornerRadius::same(18))
                .inner_margin(egui::Margin::same(12))
        } else {
            egui::Frame::popup(ui.style()).fill(t.chrome)
        };
        frame.show(ui, |ui| {
            ui.set_width(width - if phone { 24.0 } else { 12.0 });
            ui.set_max_height(height);
            picker_body(app, ui, phone);
        });
    });
    ctx.move_to_top(area.response.layer_id);
    let outside = ctx.input(|i| i.pointer.any_pressed())
        && ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| !area.response.rect.contains(p) && !anchor.contains(p))
        && !ctx.any_popup_open();
    if outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close_picker(ctx);
    }
}

fn picker_body(app: &mut PhotocraftApp, ui: &mut egui::Ui, phone: bool) {
    let search_id = egui::Id::new("bubble-picker-search");
    let mut search: String = ui.data(|d| d.get_temp(search_id)).unwrap_or_default();
    let row_h = if phone { 44.0 } else { 22.0 };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Bubble style")).font(crate::theme::semibold(14.0)));
        if phone {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_sized(vec2(64.0, 36.0), egui::Button::new(tl!("Close"))).clicked() {
                    close_picker(ui.ctx());
                }
            });
        }
    });
    let has_typer = !app.session.typer.state.library.styles.is_empty();
    if has_typer {
        ui.add(egui::TextEdit::singleline(&mut search).hint_text(tl!("Search TypeR styles")).desired_width(f32::INFINITY));
        ui.data_mut(|d| d.insert_temp(search_id, search.clone()));
    }
    let current = app.ui.tool_options.bubble_pick.clone();
    let mut chosen: Option<String> = None;
    let remember_id = egui::Id::new("bubble-picker-remember");
    let mut remember = ui.data(|d| d.get_temp::<bool>(remember_id).unwrap_or(false));
    let body_h = ui.available_height() - if remember { 150.0 } else { 40.0 };
    let fresh = ui.data_mut(|d| d.remove_temp::<bool>(egui::Id::new(FRESH)).unwrap_or(false));
    let mut list = egui::ScrollArea::vertical().max_height(body_h.max(120.0)).auto_shrink([false, true]);
    if fresh {
        list = list.vertical_scroll_offset(0.0);
    }
    list.show(ui, |ui| {
        for sec in sections(app, &search) {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(&sec.title).font(crate::theme::semibold(12.0)).color(crate::theme::Tokens::get(ui.ctx()).text_dim));
            if sec.items.is_empty() {
                ui.label(tl!("No styles match"));
            }
            for (key, label) in &sec.items {
                let r = ui.add(egui::Button::selectable(*key == current, label.as_str()).min_size(vec2(ui.available_width(), row_h)));
                crate::touch_ui::note_rect(ui.ctx(), &format!("bubbleStyle:{key}"), r.rect);
                if r.clicked() {
                    chosen = Some(key.clone());
                }
            }
        }
    });
    if let Some(k) = chosen {
        pick(app, &k);
        if phone {
            close_picker(ui.ctx());
        }
    }
    ui.separator();
    let check = ui.checkbox(&mut remember, tl!("Remember by shape")).on_hover_text(tl!("In Automatic, the style each kind of shape uses"));
    crate::touch_ui::note_rect(ui.ctx(), "bubbleRemember", check.rect);
    if check.changed() {
        ui.data_mut(|d| d.insert_temp(remember_id, remember));
    }
    if remember {
        let all: Vec<(String, String)> =
            sections(app, "").into_iter().filter(|s| s.title != tl!("Recent")).flat_map(|s| s.items).filter(|(k, _)| k != "auto").collect();
        let opts: Vec<(String, &str)> = all.iter().map(|(k, l)| (k.clone(), l.as_str())).collect();
        egui::Grid::new("bubble-remember").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            for kind in ["bubble", "narration", "floating"] {
                ui.label(pick_label(kind));
                let mut v = shape_style(app, kind);
                if crate::widgets::dropdown(ui, &format!("bubble-remember-{kind}"), pick_label(kind), &mut v, &opts, if phone { 200.0 } else { 190.0 }) {
                    set_shape_style(app, kind, &v);
                }
                ui.end_row();
            }
        });
    }
}

#[cfg(test)]
#[path = "bubble_picker_tests.rs"]
mod tests;
