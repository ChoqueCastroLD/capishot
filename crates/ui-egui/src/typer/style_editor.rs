//! The style editor (TypeR `editStyle.jsx`): name, folder, tags and tag colour, layer kind, the
//! character and paragraph settings of TypeR's `textProps`, stroke, size presets with auto size
//! by page width, and "Copy layer style" from the active text layer. It edits a draft of the
//! style's TypeR JSON and saves it with `typer.style.save`.

use egui::{Color32, RichText, vec2};
use photocraft_doc::LayerContent;
use photocraft_typer::{Id, library, presets, psjson};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{Confirm, dialog_frame, with_now};
use crate::PhotocraftApp;
use crate::theme::Tokens;

/// TypeR's tag colour presets.
pub const TAG_COLORS: [&str; 8] = ["#FFA6A4", "#FDD4BB", "#FFF3B0", "#CDFDD3", "#BEDADC", "#C2F2FF", "#C3C9FF", "#F6D5FF"];

/// The style being edited.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StyleDraft {
    /// The style's TypeR JSON, as it will be saved.
    pub style: Value,
    /// Tags as typed (split on new lines and `;` when saved).
    pub prefixes: String,
    /// Not saved yet.
    pub new: bool,
    /// The editor's last error.
    pub message: String,
}

impl Default for StyleDraft {
    fn default() -> Self {
        Self { style: json!({}), prefixes: String::new(), new: true, message: String::new() }
    }
}

/// Make sure the draft has the `textProps` shape the fields edit (TypeR's defaults fill gaps).
fn normalize(style: &mut Value) {
    let defaults = library::default_text_props();
    if !style.get("textProps").is_some_and(Value::is_object) {
        style["textProps"] = defaults.clone();
    }
    let ok = |s: &Value, p: &str| s.pointer(p).is_some_and(Value::is_object);
    if !ok(style, "/textProps/layerText/textStyleRange/0/textStyle") || !ok(style, "/textProps/layerText/paragraphStyleRange/0/paragraphStyle") {
        let mut props = defaults;
        if let (Some(dst), Some(src)) =
            (props.get_mut("layerText").and_then(Value::as_object_mut), style.pointer("/textProps/layerText").and_then(Value::as_object))
        {
            for (k, v) in src {
                if k != "textStyleRange" && k != "paragraphStyleRange" {
                    dst.insert(k.clone(), v.clone());
                }
            }
        }
        if let Some(ts) = style.pointer("/textProps/layerText/textStyleRange/0/textStyle").cloned()
            && let Some(dst) = props.pointer_mut("/layerText/textStyleRange/0/textStyle").and_then(Value::as_object_mut)
            && let Some(src) = ts.as_object()
        {
            dst.extend(src.clone());
        }
        if let Some(unit) = style.pointer("/textProps/typeUnit").cloned() {
            props["typeUnit"] = unit;
        }
        style["textProps"] = props;
    }
    if !style.get("stroke").is_some_and(Value::is_object) {
        style["stroke"] = library::default_stroke();
    }
}

/// Open the editor on an existing style.
pub fn open(app: &mut PhotocraftApp, id: &Value) {
    let Some(style) = Id::from_value(id).and_then(|id| app.session.typer.state.library.style(&id).map(|s| s.to_json())) else { return };
    let prefixes =
        style.get("prefixes").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")).unwrap_or_default();
    let mut style = style;
    normalize(&mut style);
    app.ui.typer.style_editor = Some(StyleDraft { style, prefixes, new: false, message: String::new() });
}

/// Open the editor on a new style (TypeR's defaults, in the current style's folder).
pub fn open_new(app: &mut PhotocraftApp) {
    let folder = app.session.typer.state.current_style().and_then(|s| s.folder.as_ref()).map_or(Value::Null, Id::to_value);
    let mut style = json!({"name": "", "folder": folder, "prefixes": [], "prefixColor": "#FFF3B0"});
    normalize(&mut style);
    app.ui.typer.style_editor = Some(StyleDraft { style, ..Default::default() });
}

fn obj<'a>(v: &'a mut Value, pointer: &str) -> Option<&'a mut Map<String, Value>> {
    v.pointer_mut(pointer).and_then(Value::as_object_mut)
}

fn get_f(o: &Map<String, Value>, k: &str, default: f64) -> f64 {
    photocraft_typer::id::parse_float(o.get(k)).unwrap_or(default)
}

fn get_b(o: &Map<String, Value>, k: &str, default: bool) -> bool {
    o.get(k).and_then(Value::as_bool).unwrap_or(default)
}

fn get_s<'a>(o: &'a Map<String, Value>, k: &str) -> &'a str {
    o.get(k).and_then(Value::as_str).unwrap_or("")
}

/// A labelled grid row.
fn row(ui: &mut egui::Ui, t: &Tokens, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.label(RichText::new(label).color(t.text_dim).size(11.5));
    ui.horizontal(add);
    ui.end_row();
}

fn num(ui: &mut egui::Ui, o: &mut Map<String, Value>, k: &str, default: f64, range: std::ops::RangeInclusive<f32>, suffix: &str) {
    let mut v = get_f(o, k, default) as f32;
    if crate::widgets::compact_value_field(ui, &mut v, range, suffix, 64.0, 20.0).changed() {
        o.insert(k.into(), photocraft_typer::id::num(f64::from(v)));
    }
}

fn check(ui: &mut egui::Ui, o: &mut Map<String, Value>, k: &str, default: bool, label: &str) {
    let mut v = get_b(o, k, default);
    if crate::widgets::checkbox(ui, &mut v, label).changed() {
        o.insert(k.into(), json!(v));
    }
}

/// A choice stored as one of several strings.
fn choice(ui: &mut egui::Ui, id: &str, o: &mut Map<String, Value>, k: &str, default: &str, options: &[(&str, &str)], width: f32) {
    let mut v = o.get(k).and_then(Value::as_str).unwrap_or(default).to_string();
    let opts: Vec<(String, &str)> = options.iter().map(|(v, l)| (v.to_string(), *l)).collect();
    if crate::widgets::dropdown(ui, id, &mut v, &opts, width) {
        o.insert(k.into(), json!(v));
    }
}

fn color(ui: &mut egui::Ui, o: &mut Map<String, Value>, k: &str, short: bool) {
    let (r, g, b) = if short { ("r", "g", "b") } else { ("red", "green", "blue") };
    let c = super::rgb_color(o.get(k)).unwrap_or(Color32::BLACK);
    let mut rgb = [c.r(), c.g(), c.b()];
    if crate::widgets::color_edit_button_srgb(ui, &mut rgb).changed() {
        o.insert(k.into(), json!({r: rgb[0], g: rgb[1], b: rgb[2]}));
    }
}

/// The font family: the Character panel's picker (search, previews, the user's own fonts and
/// Load Fonts…). A family the font database doesn't have is kept and flagged.
fn family_field(app: &mut PhotocraftApp, ui: &mut egui::Ui, t: &Tokens, ts: &mut Map<String, Value>) {
    let mut family = get_s(ts, "fontName").to_string();
    if crate::type_tool::fonts_ready(ui, 170.0) {
        let preview = app.session.prefs().type_.font_preview;
        if crate::type_tool::font_picker(app, ui, &mut family, 170.0, preview) {
            ts.insert("fontName".into(), json!(family));
            // The PostScript name no longer applies; the family and style name pick the face.
            ts.insert("fontPostScriptName".into(), json!(""));
        }
    }
    if !family.is_empty() && !crate::type_tool::families().iter().any(|f| f.eq_ignore_ascii_case(&family)) {
        ui.label(RichText::new(tl!("not installed")).color(t.warning).size(11.0)).on_hover_text(tl!("Load the font with Type › Load Fonts…"));
    }
}

/// Character and paragraph settings from the active text layer ("Copy layer style").
fn from_layer(app: &PhotocraftApp) -> Option<Value> {
    let st = app.session.active()?;
    let layer = st.doc.layer(st.active_layer?)?;
    let LayerContent::Text(text) = &layer.content else { return None };
    let ch = text.char_runs().first().map(|r| r.style.clone()).unwrap_or_default();
    let para = text.paragraph_runs().first().map(|r| r.style.clone()).unwrap_or_default();
    Some(psjson::text_props_from(&ch, &para, text.antialias, f64::from(st.doc.resolution_dpi)))
}

pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.typer.style_editor.clone() else { return };
    let t = Tokens::get(ctx);
    let lib = app.session.typer.state.library.clone();
    let middle_east = app.session.typer.state.settings.middle_east;
    let layer_props = from_layer(app);
    let mut close = false;
    let mut save = false;
    let mut delete = false;
    let title = if d.new { tl!("New style") } else { tl!("Edit style") };
    let canvas = app.last_canvas_rect;
    egui::Window::new(title)
        .id(egui::Id::new("typer-style-editor"))
        .title_bar(false)
        .resizable(false)
        .default_width(400.0)
        .max_width(440.0)
        .frame(dialog_frame(&t))
        .default_pos(egui::pos2((canvas.right().min(ctx.content_rect().right()) - 440.0).max(canvas.left() + 8.0).max(0.0), canvas.top().max(0.0) + 30.0))
        .constrain(true)
        .show(ctx, |ui| {
            ui.set_width(400.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).color(t.text).size(13.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::icons::button(ui, "x", 20.0, false, tl!("Close")).clicked() {
                        close = true;
                    }
                });
            });
            crate::widgets::hairline(ui);
            egui::ScrollArea::vertical().max_height(ctx.content_rect().height() * 0.7).show(ui, |ui| {
                fields(app, ui, &t, &mut d, &lib, middle_east, layer_props.as_ref());
            });
            if !d.message.is_empty() {
                ui.label(RichText::new(&d.message).color(t.warning).size(11.5));
            }
            ui.add_space(6.0);
            // Align::Min: a centred row would keep last frame's height (see `dialogs`).
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                use crate::widgets::{ButtonRole, DialogButton};
                let named = d.style.get("name").and_then(Value::as_str).is_some_and(|n| !n.trim().is_empty());
                match crate::widgets::dialog_buttons(
                    ui,
                    &[DialogButton::new(ButtonRole::Default, tl!("Save"), 80.0).enabled(named), DialogButton::new(ButtonRole::Cancel, tl!("Cancel"), 80.0)],
                ) {
                    Some(ButtonRole::Default) => save = true,
                    Some(ButtonRole::Cancel) => close = true,
                    _ => {}
                }
                if !d.new && crate::widgets::secondary_button(ui, tl!("Delete"), 70.0).clicked() {
                    delete = true;
                }
            });
        });
    if close {
        app.ui.typer.style_editor = None;
        return;
    }
    if delete {
        let name = d.style.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        app.ui.typer.confirm = Some(Confirm::DeleteStyle { id: d.style.get("id").cloned().unwrap_or(Value::Null), name });
    }
    if save {
        let mut style = d.style.clone();
        style["prefixes"] = json!(library::split_list(&d.prefixes));
        finish_presets(&mut style);
        if d.new
            && let Some(o) = style.as_object_mut()
        {
            o.remove("id");
        }
        let p = with_now(app, json!({"style": style}));
        match app.run("typer.style.save", p) {
            Ok(v) => {
                app.ui.typer.style_editor = None;
                // A new style becomes the current one, as in TypeR.
                if d.new
                    && let Some(id) = v.get("id").cloned()
                {
                    super::act(app, "typer.style.select", json!({"id": id}));
                }
                return;
            }
            Err(e) => d.message = e,
        }
    }
    if app.ui.typer.style_editor.is_some() {
        app.ui.typer.style_editor = Some(d);
    }
}

/// Size presets on save: the active size is one of them and comes first (TypeR 3.0), at most
/// three; minimum widths follow their presets.
fn finish_presets(style: &mut Value) {
    let size = style.pointer("/textProps/layerText/textStyleRange/0/textStyle/size").and_then(|v| presets::parse_size(Some(v)));
    let list: Vec<f64> =
        style.get("sizePresets").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| presets::parse_size(Some(v))).collect()).unwrap_or_default();
    if list.is_empty() {
        if let Some(o) = style.as_object_mut() {
            for k in ["sizePresets", "sizePresetMinWidths", "sizePresetDefaultIndex", "autoSizeByPageWidth"] {
                o.remove(k);
            }
        }
        return;
    }
    let widths: Vec<Value> = style.get("sizePresetMinWidths").and_then(Value::as_array).cloned().unwrap_or_default();
    let default = style.get("sizePresetDefaultIndex").and_then(Value::as_u64).unwrap_or(0) as usize;
    let mut entries: Vec<(f64, Value, bool)> =
        list.iter().enumerate().map(|(i, s)| (*s, widths.get(i).cloned().unwrap_or(Value::Null), i == default)).collect();
    entries.dedup_by(|a, b| a.0 == b.0);
    if let Some(size) = size {
        match entries.iter().position(|e| e.0 == size) {
            Some(i) => {
                let e = entries.remove(i);
                entries.insert(0, e);
            }
            None => entries.insert(0, (size, Value::Null, false)),
        }
    }
    entries.truncate(presets::MAX_PRESETS);
    let default = entries.iter().position(|e| e.2).unwrap_or(0);
    style["sizePresets"] = json!(entries.iter().map(|e| photocraft_typer::id::num(e.0)).collect::<Vec<_>>());
    style["sizePresetMinWidths"] = json!(entries.iter().map(|e| e.1.clone()).collect::<Vec<_>>());
    style["sizePresetDefaultIndex"] = json!(default);
}

fn fields(app: &mut PhotocraftApp, ui: &mut egui::Ui, t: &Tokens, d: &mut StyleDraft, lib: &photocraft_typer::Library, middle_east: bool, layer_props: Option<&Value>) {
    let grid = |ui: &mut egui::Ui, id: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
        egui::Grid::new(id).num_columns(2).spacing([10.0, 5.0]).show(ui, |ui| add(ui));
    };
    // Name, folder, tags.
    grid(ui, "typer-style-main", &mut |ui| {
        row(ui, t, tl!("Style name"), |ui| {
            let mut name = d.style.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            if ui.add(egui::TextEdit::singleline(&mut name).desired_width(240.0)).changed() {
                d.style["name"] = json!(name);
            }
        });
        row(ui, t, tl!("Folder"), |ui| {
            let mut opts: Vec<(String, String)> = vec![(String::new(), tl!("Unsorted").to_string())];
            opts.extend(lib.flatten_tree().into_iter().map(|f| (f.id.to_string(), f.label)));
            let ids: Vec<(String, Value)> = lib.folders.iter().map(|f| (f.id.to_string(), f.id.to_value())).collect();
            let mut cur = d.style.get("folder").and_then(Id::from_value).map_or(String::new(), |i| i.to_string());
            let o: Vec<(String, &str)> = opts.iter().map(|(k, l)| (k.clone(), l.as_str())).collect();
            if crate::widgets::dropdown(ui, "typer-style-folder", &mut cur, &o, 240.0) {
                d.style["folder"] = ids.iter().find(|(k, _)| *k == cur).map_or(Value::Null, |(_, v)| v.clone());
            }
        });
        row(ui, t, tl!("Tags"), |ui| {
            ui.add(egui::TextEdit::multiline(&mut d.prefixes).desired_rows(2).desired_width(240.0).hint_text(tl!("SFX: (one per line, or separated by ;)")))
                .on_hover_text(tl!("A line starting with one of these tags picks this style; the tag isn't pasted"));
        });
        row(ui, t, tl!("Tag colour"), |ui| {
            let cur = d.style.get("prefixColor").and_then(Value::as_str).unwrap_or("#FFF3B0").to_uppercase();
            for c in TAG_COLORS {
                let fill = super::hex_color(c).unwrap_or(Color32::WHITE);
                let (rect, r) = ui.allocate_exact_size(vec2(16.0, 16.0), egui::Sense::click());
                ui.painter().rect_filled(rect, 3.0, fill);
                if cur == c {
                    ui.painter().rect_stroke(rect.expand(1.5), 3.0, egui::Stroke::new(2.0, t.accent), egui::StrokeKind::Outside);
                }
                if r.clicked() {
                    d.style["prefixColor"] = json!(c);
                }
            }
            let c = super::hex_color(&cur).unwrap_or(Color32::WHITE);
            let mut rgb = [c.r(), c.g(), c.b()];
            if crate::widgets::color_edit_button_srgb(ui, &mut rgb).changed() {
                d.style["prefixColor"] = json!(format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]));
            }
        });
        row(ui, t, tl!("Layer kind"), |ui| {
            let mut v = d.style.get("textType").and_then(Value::as_str).unwrap_or("inherit").to_string();
            let opts =
                [("inherit".to_string(), tl!("As in Settings")), ("paragraph".to_string(), tl!("Paragraph text")), ("point".to_string(), tl!("Point text"))];
            if crate::widgets::dropdown(ui, "typer-style-texttype", &mut v, &opts, 160.0) {
                if v == "inherit" {
                    if let Some(o) = d.style.as_object_mut() {
                        o.remove("textType");
                    }
                } else {
                    d.style["textType"] = json!(v);
                }
            }
        });
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let r = ui.add_enabled(layer_props.is_some(), egui::Button::new(tl!("Copy layer style")));
        let r =
            r.on_hover_text(tl!("Copy the character and paragraph settings of the active text layer")).on_disabled_hover_text(tl!("Select a text layer first"));
        if r.clicked()
            && let Some(p) = layer_props
        {
            d.style["textProps"] = p.clone();
            normalize(&mut d.style);
        }
    });
    crate::widgets::section_label(ui, tl!("Character"));
    let unit = d.style.pointer("/textProps/typeUnit").and_then(Value::as_str).unwrap_or("pixelsUnit").to_string();
    let suffix = match unit.as_str() {
        "pointsUnit" => "pt",
        "millimetersUnit" => "mm",
        _ => "px",
    };
    if let Some(ts) = obj(&mut d.style, "/textProps/layerText/textStyleRange/0/textStyle") {
        grid(ui, "typer-style-char", &mut |ui| {
            row(ui, t, tl!("Font"), |ui| family_field(app, ui, t, ts));
            row(ui, t, tl!("Font style"), |ui| {
                let mut face = get_s(ts, "fontStyleName").to_string();
                if ui.add(egui::TextEdit::singleline(&mut face).desired_width(150.0).hint_text("Regular")).changed() {
                    ts.insert("fontStyleName".into(), json!(face));
                    ts.insert("fontPostScriptName".into(), json!(""));
                }
            });
            row(ui, t, tl!("Size"), |ui| {
                num(ui, ts, "size", 14.0, 0.1..=2000.0, suffix);
                let mut auto = get_b(ts, "autoLeading", true);
                if crate::widgets::checkbox(ui, &mut auto, tl!("Auto leading")).changed() {
                    ts.insert("autoLeading".into(), json!(auto));
                }
                if !auto {
                    num(ui, ts, "leading", get_f(ts, "size", 14.0) * 1.2, 0.0..=4000.0, suffix);
                }
            });
            row(ui, t, tl!("Tracking"), |ui| num(ui, ts, "tracking", 0.0, -1000.0..=10000.0, ""));
            row(ui, t, tl!("Scale"), |ui| {
                ui.label(RichText::new("H").color(t.text_faint));
                num(ui, ts, "horizontalScale", 100.0, 0.0..=1000.0, "%");
                ui.label(RichText::new("V").color(t.text_faint));
                num(ui, ts, "verticalScale", 100.0, 0.0..=1000.0, "%");
            });
            row(ui, t, tl!("Baseline shift"), |ui| num(ui, ts, "baselineShift", 0.0, -1000.0..=1000.0, suffix));
            row(ui, t, tl!("Kerning"), |ui| {
                choice(
                    ui,
                    "typer-style-kern",
                    ts,
                    "autoKern",
                    "metricsKern",
                    &[("metricsKern", tl!("Metrics")), ("opticalKern", tl!("Optical")), ("manual", tl!("Manual"))],
                    120.0,
                )
            });
            row(ui, t, tl!("Text colour"), |ui| color(ui, ts, "color", false));
            if middle_east {
                row(ui, t, tl!("Diacritics"), |ui| {
                    num(ui, ts, "diacXOffset", 0.0, -1000.0..=1000.0, "");
                    num(ui, ts, "markYDistFromBaseline", 100.0, -1000.0..=1000.0, "");
                });
            }
        });
        // Three columns: checkboxes are rows of their own, so a wrapping row wouldn't wrap them.
        egui::Grid::new("typer-style-flags").num_columns(3).spacing([12.0, 4.0]).show(ui, |ui| {
            check(ui, ts, "syntheticBold", false, tl!("Faux bold"));
            check(ui, ts, "syntheticItalic", false, tl!("Faux italic"));
            let mut caps = get_s(ts, "fontCaps").to_string();
            let mut all = caps == "allCaps";
            if crate::widgets::checkbox(ui, &mut all, tl!("All caps")).changed() {
                caps = if all { "allCaps".into() } else { "normal".into() };
                ts.insert("fontCaps".into(), json!(caps));
            }
            ui.end_row();
            let mut small = caps == "smallCaps";
            if crate::widgets::checkbox(ui, &mut small, tl!("Small caps")).changed() {
                ts.insert("fontCaps".into(), json!(if small { "smallCaps" } else { "normal" }));
            }
            let mut under = get_s(ts, "underline").starts_with("underlineOn");
            if crate::widgets::checkbox(ui, &mut under, tl!("Underline")).changed() {
                ts.insert("underline".into(), json!(if under { "underlineOnLeftInVertical" } else { "underlineOff" }));
            }
            let mut strike = get_s(ts, "strikethrough").ends_with("StrikethroughOn");
            if crate::widgets::checkbox(ui, &mut strike, tl!("Strikethrough")).changed() {
                ts.insert("strikethrough".into(), json!(if strike { "xHeightStrikethroughOn" } else { "strikethroughOff" }));
            }
            ui.end_row();
            check(ui, ts, "ligature", false, tl!("Ligatures"));
            check(ui, ts, "altligature", false, tl!("Discretionary ligatures"));
            ui.end_row();
            let base = get_s(ts, "baseline").to_string();
            let mut sup = base == "superScript";
            if crate::widgets::checkbox(ui, &mut sup, tl!("Superscript")).changed() {
                ts.insert("baseline".into(), json!(if sup { "superScript" } else { "normal" }));
            }
            let mut sub = base == "subScript";
            if crate::widgets::checkbox(ui, &mut sub, tl!("Subscript")).changed() {
                ts.insert("baseline".into(), json!(if sub { "subScript" } else { "normal" }));
            }
        });
    }
    if let Some(lt) = obj(&mut d.style, "/textProps") {
        ui.horizontal(|ui| {
            ui.label(RichText::new(tl!("Units")).color(t.text_dim).size(11.5));
            choice(ui, "typer-style-unit", lt, "typeUnit", "pixelsUnit", &[("pixelsUnit", "px"), ("pointsUnit", "pt"), ("millimetersUnit", "mm")], 70.0);
            if let Some(l) = lt.get_mut("layerText").and_then(Value::as_object_mut) {
                ui.label(RichText::new(tl!("Anti-aliasing")).color(t.text_dim).size(11.5));
                choice(
                    ui,
                    "typer-style-aa",
                    l,
                    "antiAlias",
                    "antiAliasSmooth",
                    &[
                        ("antiAliasNone", tl!("None")),
                        ("antiAliasSharp", tl!("Sharp")),
                        ("antiAliasCrisp", tl!("Crisp")),
                        ("antiAliasStrong", tl!("Strong")),
                        ("antiAliasSmooth", tl!("Smooth")),
                    ],
                    100.0,
                );
            }
        });
    }
    crate::widgets::section_label(ui, tl!("Paragraph"));
    if let Some(ps) = obj(&mut d.style, "/textProps/layerText/paragraphStyleRange/0/paragraphStyle") {
        ui.horizontal(|ui| {
            for (v, icon, tip) in [
                ("left", "align-left", tl!("Left align text")),
                ("center", "align-center", tl!("Centre text")),
                ("right", "align-right", tl!("Right align text")),
            ] {
                let on = get_s(ps, "alignment").eq_ignore_ascii_case(v) || (v == "left" && get_s(ps, "alignment").is_empty());
                if crate::icons::button(ui, icon, 22.0, on, tip).clicked() {
                    ps.insert("alignment".into(), json!(v));
                }
            }
            check(ui, ps, "hyphenate", true, tl!("Hyphenate"));
            check(ui, ps, "hangingRoman", true, tl!("Hanging punctuation"));
        });
    }
    crate::widgets::section_label(ui, tl!("Stroke"));
    if let Some(st) = obj(&mut d.style, "/stroke") {
        ui.horizontal(|ui| {
            let mut on = get_b(st, "enabled", false);
            if crate::widgets::checkbox(ui, &mut on, tl!("Stroke")).changed() {
                st.insert("enabled".into(), json!(on));
                if on && get_f(st, "size", 0.0) <= 0.0 {
                    st.insert("size".into(), json!(3));
                }
            }
            num(ui, st, "size", 0.0, 0.0..=250.0, "px");
            color(ui, st, "color", true);
        });
    }
    presets_section(ui, t, d);
}

/// Up to three size presets with a default and minimum page widths (TypeR 3.0).
fn presets_section(ui: &mut egui::Ui, t: &Tokens, d: &mut StyleDraft) {
    crate::widgets::section_label(ui, tl!("Size presets"));
    let mut list: Vec<f64> = d
        .style
        .get("sizePresets")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| photocraft_typer::id::parse_float(Some(v))).collect())
        .unwrap_or_default();
    let mut widths: Vec<Option<f64>> = d
        .style
        .get("sizePresetMinWidths")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| photocraft_typer::id::parse_float(Some(v))).collect())
        .unwrap_or_default();
    widths.resize(list.len(), None);
    let mut default = d.style.get("sizePresetDefaultIndex").and_then(Value::as_u64).unwrap_or(0) as usize;
    let mut auto = d.style.get("autoSizeByPageWidth").and_then(Value::as_bool).unwrap_or(false);
    let mut remove = None;
    let mut changed = false;
    egui::Grid::new("typer-style-presets").num_columns(4).spacing([8.0, 4.0]).show(ui, |ui| {
        if !list.is_empty() {
            ui.label(RichText::new(tl!("Size")).color(t.text_faint).size(11.0));
            ui.label(RichText::new(tl!("Default")).color(t.text_faint).size(11.0));
            ui.label(RichText::new(tl!("From page width")).color(t.text_faint).size(11.0));
            ui.label("");
            ui.end_row();
        }
        for i in 0..list.len() {
            let mut v = list[i] as f32;
            if crate::widgets::compact_value_field(ui, &mut v, 0.1..=2000.0, "", 60.0, 20.0).changed() {
                list[i] = f64::from(v);
                changed = true;
            }
            if ui.radio(default == i, "").clicked() {
                default = i;
                changed = true;
            }
            if default == i {
                ui.label(RichText::new("—").color(t.text_faint));
            } else {
                let mut w = widths[i].unwrap_or(0.0) as f32;
                if crate::widgets::compact_value_field(ui, &mut w, 0.0..=100000.0, "px", 80.0, 20.0).changed() {
                    widths[i] = (w > 0.0).then_some(f64::from(w));
                    changed = true;
                }
            }
            if crate::icons::button(ui, "x", 18.0, false, tl!("Remove")).clicked() {
                remove = Some(i);
            }
            ui.end_row();
        }
    });
    if let Some(i) = remove {
        list.remove(i);
        widths.remove(i);
        default = if default == i {
            0
        } else if default > i {
            default - 1
        } else {
            default
        };
        changed = true;
    }
    ui.horizontal(|ui| {
        if list.len() < presets::MAX_PRESETS && ui.button(tl!("Add preset")).clicked() {
            let size = d
                .style
                .pointer("/textProps/layerText/textStyleRange/0/textStyle/size")
                .and_then(|v| photocraft_typer::id::parse_float(Some(v)))
                .unwrap_or(14.0);
            list.push(if list.contains(&size) { size + 2.0 } else { size });
            widths.push(None);
            changed = true;
        }
        let r = ui.add_enabled_ui(list.len() >= 2, |ui| crate::widgets::checkbox(ui, &mut auto, tl!("Auto size by page width"))).inner;
        if r.changed() {
            changed = true;
        }
    });
    if changed {
        d.style["sizePresets"] = json!(list.iter().map(|v| photocraft_typer::id::num(*v)).collect::<Vec<_>>());
        d.style["sizePresetMinWidths"] = json!(widths.iter().map(|w| w.map_or(Value::Null, photocraft_typer::id::num)).collect::<Vec<_>>());
        d.style["sizePresetDefaultIndex"] = json!(default.min(list.len().saturating_sub(1)));
        d.style["autoSizeByPageWidth"] = json!(auto && list.len() >= 2);
    }
}
