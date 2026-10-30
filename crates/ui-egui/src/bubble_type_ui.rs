//! The Bubble Type tool's styles: "Texto globo", "Texto narración" and "Texto flotante", picked
//! by what a click hits (`type.createInBubble`'s `"style": "auto"`) or forced from the options
//! bar's chip; edited in the "Estilos" popover with a live preview; the chosen one shown for a
//! moment by the cursor.

use egui::{Color32, Pos2, Stroke, vec2};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::state::{BubbleStyles, BubbleTextStyle};

/// The picks, in the chip's order: the command's `"style"` value and its label.
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
    let galley = painter.layout_no_wrap(style_title(&kind).to_string(), font, t.text.gamma_multiply(alpha));
    let r = egui::Rect::from_min_size(p, galley.size() + vec2(16.0, 8.0));
    painter.rect(r, 6.0, t.chrome.gamma_multiply(0.95 * alpha), Stroke::new(1.0, t.accent.gamma_multiply(alpha)), egui::StrokeKind::Inside);
    painter.galley(r.min + vec2(8.0, 4.0), galley, Color32::WHITE);
    ctx.request_repaint();
}

// ------------------------------------------------------------------ the options bar

/// The chip (Auto / Globo / Narración / Flotante) and the Estilos button with its popover.
pub fn options(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    crate::widgets::vline(ui, 22.0);
    let mut pick = app.ui.tool_options.bubble_pick.clone();
    let opts: Vec<(String, &str)> = PICKS.iter().map(|(k, _)| (k.to_string(), pick_label(k))).collect();
    let resp_changed = crate::widgets::dropdown(ui, "bubble-pick", "", &mut pick, &opts, 104.0);
    if resp_changed {
        app.ui.tool_options.bubble_pick = pick;
    }
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
