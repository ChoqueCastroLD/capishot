//! TypeR style JSON (`textProps`, Photoshop ActionDescriptor stringIDs) ↔ Capishot's type model
//! (§2.7 of the port plan).
//!
//! Written from the TypeR style JSON as data and Capishot's own PSD knowledge; nothing here
//! comes from TypeR's GPL JSON Action Manager library. Sizes stay in the style's `typeUnit` in the
//! library and are converted to points with the document resolution at apply time
//! (`pt = px * 72 / dpi`), so one style gives the same pixel size on 72 and 300 dpi pages.

use photocraft_color::Color;
use photocraft_doc::text::{AntiAlias, Caps, CharStyle, Kerning, ParagraphStyle, TextAlign, TextDirection};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::id::num;
use crate::library::Style;
use crate::presets;
use crate::settings::Direction;

/// A TypeR `typeUnit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Unit {
    Pixels,
    Points,
    Millimeters,
}

impl Unit {
    /// `pixelsUnit`, `pointsUnit`, `millimetersUnit`; anything else (or a missing unit) is pixels,
    /// TypeR's default.
    pub fn parse(s: Option<&str>) -> Unit {
        match s {
            Some("pointsUnit") => Unit::Points,
            Some("millimetersUnit") => Unit::Millimeters,
            _ => Unit::Pixels,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Unit::Pixels => "pixelsUnit",
            Unit::Points => "pointsUnit",
            Unit::Millimeters => "millimetersUnit",
        }
    }
}

/// A usable resolution: non-finite or non-positive values become 72.
pub fn sane_dpi(dpi: f64) -> f64 {
    if dpi.is_finite() && dpi > 0.0 { dpi } else { 72.0 }
}

/// `value` in `unit` → points at `dpi`.
pub fn to_pt(value: f64, unit: Unit, dpi: f64) -> f64 {
    match unit {
        Unit::Pixels => value * 72.0 / sane_dpi(dpi),
        Unit::Points => value,
        Unit::Millimeters => value * 72.0 / 25.4,
    }
}

/// Points → `unit` at `dpi`.
pub fn from_pt(pt: f64, unit: Unit, dpi: f64) -> f64 {
    match unit {
        Unit::Pixels => pt * sane_dpi(dpi) / 72.0,
        Unit::Points => pt,
        Unit::Millimeters => pt * 25.4 / 72.0,
    }
}

/// The style's outline (TypeR's stroke, applied as a layer-style Stroke).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stroke {
    /// Pixels.
    pub size: f64,
    /// 0..100.
    pub opacity: f64,
    pub color: [u8; 3],
}

/// What applying a style sets on a text layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Mapped {
    /// The character style. When [`Mapped::size_pt`] is `None` its size is a placeholder.
    pub char_style: CharStyle,
    pub paragraph: ParagraphStyle,
    /// `None` = the style says nothing (keep the layer's).
    pub antialias: Option<AntiAlias>,
    /// `None` = keep the layer's size (TypeR's `null` size).
    pub size_pt: Option<f32>,
    /// `None` = no stroke (disabled or zero size).
    pub stroke: Option<Stroke>,
    /// Properties Capishot can't render yet (stored in the style, applied as far as possible).
    pub warnings: Vec<String>,
}

/// Options of [`map_style`].
#[derive(Clone, Copy, Debug, Default)]
pub struct MapOptions {
    /// Document resolution (pixels per inch).
    pub dpi: f64,
    /// Text scale in % (`None` = 100).
    pub text_scale: Option<f64>,
    /// Page width in pixels, for auto size by page width (L5).
    pub page_width: Option<f64>,
    /// The RTL setting.
    pub direction: Direction,
}

fn f(m: &Map<String, Value>, k: &str) -> Option<f64> {
    m.get(k).and_then(Value::as_f64).filter(|v| v.is_finite())
}

fn b(m: &Map<String, Value>, k: &str) -> Option<bool> {
    m.get(k).and_then(Value::as_bool)
}

fn s<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    m.get(k).and_then(Value::as_str)
}

fn channel(v: Option<&Value>) -> f32 {
    v.and_then(Value::as_f64).filter(|x| x.is_finite()).map_or(0.0, |x| (x / 255.0).clamp(0.0, 1.0) as f32)
}

/// `(weight, italic)` from a face name such as "Bold Italic", "Black", "SemiBold".
pub fn weight_from_style_name(name: &str) -> (u16, bool) {
    let n: String = name.chars().filter(|c| !c.is_whitespace() && *c != '-' && *c != '_').flat_map(char::to_lowercase).collect();
    let italic = n.contains("italic") || n.contains("oblique");
    let weight = [
        ("thin", 100),
        ("hairline", 100),
        ("extralight", 200),
        ("ultralight", 200),
        ("semibold", 600),
        ("demibold", 600),
        ("extrabold", 800),
        ("ultrabold", 800),
        ("light", 300),
        ("medium", 500),
        ("bold", 700),
        ("heavy", 800),
        ("black", 900),
    ]
    .iter()
    .find(|(k, _)| n.contains(k))
    .map_or(400, |(_, w)| *w);
    (weight, italic)
}

/// Map a style to Capishot's type model for a document at `opts.dpi`. The size comes from the
/// auto-size preset for `opts.page_width` when enabled, then the text scale applies (also to a
/// fixed leading).
pub fn map_style(style: &Style, opts: &MapOptions) -> Mapped {
    let empty = Map::new();
    let ts = style.text_style().unwrap_or(&empty);
    let ps = style.paragraph_style().unwrap_or(&empty);
    let unit = Unit::parse(style.type_unit());
    let ratio = opts.text_scale.filter(|s| s.is_finite() && *s > 0.0).map_or(1.0, |s| s / 100.0);
    let pt = |v: f64| to_pt(v, unit, opts.dpi) as f32;
    let mut warnings = Vec::new();

    let mut c = CharStyle::default();
    if let Some(name) = s(ts, "fontName") {
        c.font_family = name.to_string();
    }
    if let Some(face) = s(ts, "fontStyleName") {
        c.font_style = face.to_string();
        (c.weight, c.italic) = weight_from_style_name(face);
    }
    c.postscript_name = s(ts, "fontPostScriptName").filter(|p| !p.is_empty()).map(str::to_string);
    let size = presets::size_for_page_width(style, opts.page_width).map(|v| v * ratio);
    let size_pt = size.map(pt).filter(|v| v.is_finite() && *v > 0.0);
    if let Some(sz) = size_pt {
        c.size_pt = sz;
    }
    c.leading_pt = match (b(ts, "autoLeading"), f(ts, "leading")) {
        (Some(true), _) | (_, None) => None,
        (_, Some(0.0)) => None,
        (_, Some(l)) => Some(pt(l * ratio)),
    };
    if let Some(t) = f(ts, "tracking") {
        c.tracking = t as f32;
    }
    if let Some(h) = f(ts, "horizontalScale") {
        c.horizontal_scale = (h / 100.0) as f32;
    }
    if let Some(v) = f(ts, "verticalScale") {
        c.vertical_scale = (v / 100.0) as f32;
    }
    if let Some(bs) = f(ts, "baselineShift") {
        c.baseline_shift_pt = pt(bs);
    }
    c.kerning = match s(ts, "autoKern") {
        Some("opticalKern") => Kerning::Optical,
        Some("manual") => Kerning::Off,
        _ => Kerning::Metrics,
    };
    c.caps = match s(ts, "fontCaps") {
        Some("smallCaps") => Caps::SmallCaps,
        Some("allCaps") => Caps::AllCaps,
        _ => Caps::Normal,
    };
    c.faux_bold = b(ts, "syntheticBold").unwrap_or(false);
    c.faux_italic = b(ts, "syntheticItalic").unwrap_or(false);
    c.underline = s(ts, "underline").is_some_and(|u| u != "underlineOff");
    c.strikethrough = s(ts, "strikethrough").is_some_and(|u| u != "strikethroughOff");
    if let Some(col) = ts.get("color").and_then(Value::as_object) {
        c.color = Color::rgb(channel(col.get("red")), channel(col.get("green")), channel(col.get("blue")));
    }
    // TypeR's default style turns standard ligatures off; a style without the key keeps them on.
    c.ligatures = b(ts, "ligature").unwrap_or(true);
    c.discretionary_ligatures = b(ts, "altligature").unwrap_or(false);
    if let Some(bl) = s(ts, "baseline").filter(|v| *v != "normal") {
        warnings.push(format!("baseline \"{bl}\" (superscript/subscript) is not rendered yet"));
    }
    if f(ts, "diacXOffset").is_some_and(|v| v != 0.0) || f(ts, "markYDistFromBaseline").is_some_and(|v| v != 100.0) {
        warnings.push("Middle-East diacritic offsets are kept but not rendered".into());
    }

    let mut p = ParagraphStyle { align: TextAlign::Left, ..ParagraphStyle::default() };
    p.align = match s(ps, "alignment") {
        Some("center") => TextAlign::Center,
        Some("right") => TextAlign::Right,
        Some("justifyLeft") => TextAlign::JustifyLeft,
        Some("justifyCenter") => TextAlign::JustifyCenter,
        Some("justifyRight") => TextAlign::JustifyRight,
        Some("justifyAll") => TextAlign::JustifyAll,
        _ => TextAlign::Left,
    };
    if let Some(al) = f(ps, "autoLeadingPercentage").filter(|v| *v > 0.0) {
        p.auto_leading = al as f32;
    }
    p.hyphenate = b(ps, "hyphenate").unwrap_or(false);
    p.direction = match (s(ps, "directionType"), opts.direction) {
        (Some("dirRightToLeft"), _) | (_, Direction::Rtl) => TextDirection::Rtl,
        (Some("dirLeftToRight"), _) => TextDirection::Ltr,
        _ => TextDirection::Auto,
    };
    // `hangingRoman` (hanging punctuation) is Phase C; TypeR's default style turns it on, so it
    // is kept in the style without a warning.

    let antialias = style.text_props().and_then(|t| t.get("layerText")).and_then(|l| l.get("antiAlias")).and_then(Value::as_str).and_then(|a| match a {
        "antiAliasNone" => Some(AntiAlias::None),
        "antiAliasSharp" => Some(AntiAlias::Sharp),
        "antiAliasCrisp" => Some(AntiAlias::Crisp),
        "antiAliasStrong" => Some(AntiAlias::Strong),
        "antiAliasSmooth" => Some(AntiAlias::Smooth),
        "antiAliasPlatformGray" => Some(AntiAlias::Windows),
        "antiAliasPlatformLCD" => Some(AntiAlias::WindowsLcd),
        _ => None,
    });

    Mapped { char_style: c, paragraph: p, antialias, size_pt, stroke: stroke(style), warnings }
}

/// The style's stroke, when enabled with a size above 0.
pub fn stroke(style: &Style) -> Option<Stroke> {
    let st = style.stroke()?;
    let size = f(st, "size").filter(|v| *v > 0.0)?;
    if b(st, "enabled") == Some(false) {
        return None;
    }
    let col = st.get("color").and_then(Value::as_object);
    let ch = |k: &str| col.and_then(|c| c.get(k)).and_then(Value::as_f64).filter(|v| v.is_finite()).map_or(255, |v| v.round().clamp(0.0, 255.0) as u8);
    Some(Stroke { size, opacity: f(st, "opacity").map_or(100.0, |o| o.clamp(0.0, 100.0)), color: [ch("r"), ch("g"), ch("b")] })
}

/// TypeR `textProps` for a Capishot character and paragraph style, in pixels at `dpi`
/// ("Copy layer style", L4). Fields Capishot doesn't model get TypeR's defaults.
pub fn text_props_from(c: &CharStyle, p: &ParagraphStyle, antialias: AntiAlias, dpi: f64) -> Value {
    let px = |pt: f32| num(from_pt(f64::from(pt), Unit::Pixels, dpi));
    let rgb = c.color.to_rgb();
    let ch = |v: f32| num(f64::from((v.clamp(0.0, 1.0) * 255.0).round()));
    let mut ts = json!({
        "fontName": c.font_family,
        "fontStyleName": if c.font_style.is_empty() { "Regular".to_string() } else { c.font_style.clone() },
        "size": px(c.size_pt),
        "impliedFontSize": px(c.size_pt),
        "horizontalScale": num(f64::from(c.horizontal_scale) * 100.0),
        "verticalScale": num(f64::from(c.vertical_scale) * 100.0),
        "autoLeading": c.leading_pt.is_none(),
        "tracking": num(f64::from(c.tracking)),
        "baselineShift": px(c.baseline_shift_pt),
        "autoKern": match c.kerning { Kerning::Metrics => "metricsKern", Kerning::Optical => "opticalKern", Kerning::Off => "manual" },
        "fontCaps": match c.caps { Caps::Normal => "normal", Caps::SmallCaps => "smallCaps", Caps::AllCaps => "allCaps" },
        "syntheticBold": c.faux_bold,
        "syntheticItalic": c.faux_italic,
        "underline": if c.underline { "underlineOnLeftInVertical" } else { "underlineOff" },
        "strikethrough": if c.strikethrough { "xHeightStrikethroughOn" } else { "strikethroughOff" },
        "ligature": c.ligatures,
        "altligature": c.discretionary_ligatures,
        "color": {"red": ch(rgb[0]), "green": ch(rgb[1]), "blue": ch(rgb[2])},
    });
    if let Some(o) = ts.as_object_mut() {
        if let Some(ps) = &c.postscript_name {
            o.insert("fontPostScriptName".into(), Value::String(ps.clone()));
        }
        if let Some(l) = c.leading_pt {
            o.insert("leading".into(), px(l));
        }
    }
    let alignment = match p.align {
        TextAlign::Left => "left",
        TextAlign::Center => "center",
        TextAlign::Right => "right",
        TextAlign::JustifyLeft => "justifyLeft",
        TextAlign::JustifyCenter => "justifyCenter",
        TextAlign::JustifyRight => "justifyRight",
        TextAlign::JustifyAll => "justifyAll",
    };
    let mut paragraph = json!({"alignment": alignment, "hyphenate": p.hyphenate, "autoLeadingPercentage": num(f64::from(p.auto_leading))});
    if let (Some(o), Some(dir)) = (
        paragraph.as_object_mut(),
        match p.direction {
            TextDirection::Rtl => Some("dirRightToLeft"),
            TextDirection::Ltr => Some("dirLeftToRight"),
            TextDirection::Auto => None,
        },
    ) {
        o.insert("directionType".into(), Value::String(dir.into()));
    }
    let aa = match antialias {
        AntiAlias::None => "antiAliasNone",
        AntiAlias::Sharp => "antiAliasSharp",
        AntiAlias::Crisp => "antiAliasCrisp",
        AntiAlias::Strong => "antiAliasStrong",
        AntiAlias::Smooth => "antiAliasSmooth",
        AntiAlias::Windows => "antiAliasPlatformGray",
        AntiAlias::WindowsLcd => "antiAliasPlatformLCD",
    };
    json!({
        "layerText": {
            "antiAlias": aa,
            "textStyleRange": [{"from": 0, "to": 100, "textStyle": ts}],
            "paragraphStyleRange": [{"from": 0, "to": 100, "paragraphStyle": paragraph}],
        },
        "typeUnit": "pixelsUnit",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::Id;
    use serde_json::json;

    fn opts(dpi: f64) -> MapOptions {
        MapOptions { dpi, ..MapOptions::default() }
    }

    #[test]
    fn default_style_maps_to_capishot() {
        let s = Style::new(Id::str("d"), "Default");
        let m = map_style(&s, &opts(72.0));
        assert_eq!(m.char_style.font_family, "Tahoma");
        assert_eq!(m.char_style.postscript_name.as_deref(), Some("Tahoma"));
        assert_eq!((m.char_style.weight, m.char_style.italic), (400, false));
        assert_eq!(m.size_pt, Some(14.0));
        assert_eq!(m.char_style.leading_pt, None);
        assert_eq!(m.char_style.kerning, Kerning::Metrics);
        assert!(!m.char_style.ligatures);
        assert_eq!(m.char_style.color, Color::BLACK);
        assert_eq!(m.paragraph.align, TextAlign::Center);
        assert!(m.paragraph.hyphenate);
        assert_eq!(m.antialias, Some(AntiAlias::Smooth));
        assert_eq!(m.stroke, None, "default stroke is off");
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    }

    #[test]
    fn pixel_sizes_convert_with_the_document_dpi() {
        let s = Style::new(Id::str("d"), "Default");
        let m = map_style(&s, &opts(300.0));
        assert!((m.size_pt.unwrap() - 14.0 * 72.0 / 300.0).abs() < 1e-4);
        assert_eq!(map_style(&s, &opts(f64::NAN)).size_pt, Some(14.0), "a bad dpi falls back to 72");
        let mut pts = s.clone();
        pts.props.get_mut("textProps").unwrap()["typeUnit"] = json!("pointsUnit");
        assert_eq!(map_style(&pts, &opts(300.0)).size_pt, Some(14.0));
        assert!((to_pt(25.4, Unit::Millimeters, 300.0) - 72.0).abs() < 1e-9);
        assert!((from_pt(to_pt(37.0, Unit::Pixels, 144.0), Unit::Pixels, 144.0) - 37.0).abs() < 1e-9);
    }

    #[test]
    fn every_mapped_property() {
        let s = Style::from_json(&json!({"id": "x", "name": "x",
            "textProps": {"typeUnit": "pixelsUnit", "layerText": {"antiAlias": "antiAliasCrisp",
                "textStyleRange": [{"textStyle": {"fontName": "Anime Ace", "fontStyleName": "Bold Italic", "fontPostScriptName": "AnimeAce-BoldItalic",
                    "size": 20, "autoLeading": false, "leading": 24, "tracking": -10, "horizontalScale": 90, "verticalScale": 110,
                    "baselineShift": 2, "autoKern": "opticalKern", "fontCaps": "allCaps", "syntheticBold": true, "syntheticItalic": true,
                    "underline": "underlineOnLeftInVertical", "strikethrough": "xHeightStrikethroughOn", "color": {"red": 255, "green": 0, "blue": 51},
                    "ligature": true, "altligature": true, "baseline": "superScript"}}],
                "paragraphStyleRange": [{"paragraphStyle": {"alignment": "right", "autoLeadingPercentage": 1.5, "hyphenate": false, "directionType": "dirRightToLeft"}}]}},
            "stroke": {"enabled": true, "size": 3, "opacity": 80, "position": "outer", "color": {"r": 255, "g": 255, "b": 255}}}))
        .unwrap();
        let m = map_style(&s, &MapOptions { dpi: 144.0, text_scale: Some(50.0), ..MapOptions::default() });
        let c = &m.char_style;
        assert_eq!((c.weight, c.italic), (700, true));
        assert_eq!(m.size_pt, Some(5.0), "20 px at 50 % on 144 dpi = 5 pt");
        assert_eq!(c.leading_pt, Some(6.0), "fixed leading scales too");
        assert_eq!((c.tracking, c.horizontal_scale, c.vertical_scale), (-10.0, 0.9, 1.1));
        assert_eq!(c.baseline_shift_pt, 1.0);
        assert_eq!((c.kerning, c.caps), (Kerning::Optical, Caps::AllCaps));
        assert!(c.faux_bold && c.faux_italic && c.underline && c.strikethrough && c.ligatures && c.discretionary_ligatures);
        assert_eq!(c.color, Color::rgb(1.0, 0.0, 0.2));
        assert_eq!(m.paragraph.align, TextAlign::Right);
        assert_eq!(m.paragraph.auto_leading, 1.5);
        assert_eq!(m.paragraph.direction, TextDirection::Rtl);
        assert!(!m.paragraph.hyphenate);
        assert_eq!(m.antialias, Some(AntiAlias::Crisp));
        assert_eq!(m.stroke, Some(Stroke { size: 3.0, opacity: 80.0, color: [255, 255, 255] }));
        assert_eq!(m.warnings.len(), 1);
    }

    #[test]
    fn null_size_keeps_the_layer_size() {
        let s = Style::from_json(&json!({"id": "x", "name": "x", "textProps": {"layerText": {"textStyleRange": [{"textStyle": {"size": null}}]}}})).unwrap();
        assert_eq!(map_style(&s, &opts(72.0)).size_pt, None);
        let bare = Style::from_json(&json!({"id": "y", "name": "y"})).unwrap();
        let m = map_style(&bare, &opts(72.0));
        assert_eq!(m.size_pt, None);
        assert_eq!(m.antialias, None);
    }

    #[test]
    fn reverse_mapping_round_trips_at_the_same_dpi() {
        let s = Style::from_json(&json!({"id": "x", "name": "x",
            "textProps": {"typeUnit": "pixelsUnit", "layerText": {"antiAlias": "antiAliasStrong",
                "textStyleRange": [{"textStyle": {"fontName": "Wild Words", "fontStyleName": "Italic", "fontPostScriptName": "WildWords-Italic",
                    "size": 33, "autoLeading": false, "leading": 40, "tracking": 25, "autoKern": "manual", "color": {"red": 10, "green": 20, "blue": 30}}}],
                "paragraphStyleRange": [{"paragraphStyle": {"alignment": "justifyCenter", "hyphenate": true}}]}}}))
        .unwrap();
        for dpi in [72.0, 300.0, 350.0] {
            let m = map_style(&s, &opts(dpi));
            let mut c = m.char_style.clone();
            c.size_pt = m.size_pt.unwrap();
            let props = text_props_from(&c, &m.paragraph, m.antialias.unwrap(), dpi);
            let back = Style::from_json(&json!({"id": "b", "name": "b", "textProps": props})).unwrap();
            let m2 = map_style(&back, &opts(dpi));
            assert!((m2.size_pt.unwrap() - m.size_pt.unwrap()).abs() < 0.01, "{dpi}");
            assert!((m2.char_style.leading_pt.unwrap() - m.char_style.leading_pt.unwrap()).abs() < 0.01);
            let mut a = m2.char_style.clone();
            let mut e = m.char_style.clone();
            (a.size_pt, e.size_pt, a.leading_pt, e.leading_pt) = (0.0, 0.0, None, None);
            assert_eq!(a, e);
            assert_eq!(m2.paragraph, m.paragraph);
            assert_eq!(m2.antialias, m.antialias);
        }
    }

    #[test]
    fn face_names_give_weights() {
        assert_eq!(weight_from_style_name("Regular"), (400, false));
        assert_eq!(weight_from_style_name("SemiBold Italic"), (600, true));
        assert_eq!(weight_from_style_name("ExtraBold"), (800, false));
        assert_eq!(weight_from_style_name("Black Oblique"), (900, true));
        assert_eq!(weight_from_style_name("Light"), (300, false));
    }
}
