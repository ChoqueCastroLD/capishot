// Ported from TypeR app_src/fontScan.js (develop 71bc061), MIT, (c) 2017 Swirt. See
// crates/typer/LICENSE-TypeR.txt.

//! FontScanR (F3): the text layers of a chapter's PSDs grouped by font variant, each group with
//! its most-used size and a ready-to-save TypeR style.
//!
//! - **Group key:** PostScript name (else the font name) | faux bold | faux italic. Real bold or
//!   italic faces have their own PostScript name, so they are their own groups.
//! - **Counting:** identical runs inside one layer count once (Photoshop often splits one look
//!   into several ranges), so a count is a number of layers.
//! - **Size:** the most-used size wins; a tie goes to the smaller size.
//! - **Representative:** the most frequent variant (size, unit, anti-aliasing, colour, stroke) at
//!   that size gives the style's text and paragraph properties and its stroke.
//! - Groups are sorted by usage, then by first appearance.

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::id::{num, parse_float};

/// One scanned file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScanFile {
    pub file: String,
    pub layers: Vec<ScanLayer>,
}

/// One text layer: its runs' TypeR `textStyle` objects, and what the layer applies to all of
/// them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScanLayer {
    pub layer_name: String,
    /// `antiAliasSmooth`, … (empty = smooth).
    pub anti_alias: String,
    /// `pixelsUnit`, … (empty = pixels).
    pub type_unit: String,
    pub paragraph_style: Option<Value>,
    /// TypeR stroke `{enabled, size, opacity, position, color{r,g,b}}`.
    pub stroke: Option<Value>,
    pub runs: Vec<Value>,
}

/// A size and how many layers use it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SizeCount {
    pub size: f64,
    pub count: usize,
}

/// One font variant found in the scan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontGroup {
    pub key: String,
    pub order: usize,
    pub font_post_script_name: String,
    pub font_name: String,
    pub font_style_name: String,
    pub synthetic_bold: bool,
    pub synthetic_italic: bool,
    /// Layers using it.
    pub usage_count: usize,
    pub file_count: usize,
    /// Most used first.
    pub sizes: Vec<SizeCount>,
    pub top_size: f64,
    pub anti_alias: String,
    /// "CC Wild Words Bold (faux italic)".
    pub default_name: String,
    /// TypeR `textProps` for the style.
    pub text_props: Value,
    /// TypeR stroke for the style.
    pub stroke: Value,
}

impl FontGroup {
    /// The TypeR style to import, named `name` (else the default name).
    pub fn style(&self, name: Option<&str>, now: Option<f64>) -> Value {
        let name = name.map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&self.default_name);
        let mut s = json!({"name": name, "textProps": self.text_props, "stroke": self.stroke, "prefixes": []});
        if let (Some(o), Some(now)) = (s.as_object_mut(), now) {
            o.insert("edited".into(), num(now.floor()));
        }
        s
    }
}

/// TypeR's `roundSize`: two decimals, 0 when not a number.
pub fn round_size(v: Option<&Value>) -> f64 {
    parse_float(v).map_or(0.0, |s| (s * 100.0).round() / 100.0)
}

fn channel(c: &Value, long: &str, short: &str) -> f64 {
    c.get(long).filter(|v| !v.is_null()).or_else(|| c.get(short)).and_then(Value::as_f64).unwrap_or(0.0)
}

fn color_signature(color: Option<&Value>) -> String {
    let Some(c) = color.filter(|c| c.is_object()) else { return "none".into() };
    let r = channel(c, "red", "r").round();
    let g = channel(c, "green", "g").round();
    let b = channel(c, "blue", "b").round();
    format!("{r},{g},{b}")
}

fn stroke_on(stroke: Option<&Value>) -> Option<&Value> {
    stroke.filter(|s| s.get("enabled").is_some_and(truthy) && s.get("size").and_then(Value::as_f64).is_some_and(|z| z > 0.0))
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn stroke_signature(stroke: Option<&Value>) -> String {
    let Some(s) = stroke_on(stroke) else { return "none".into() };
    let size = (s.get("size").and_then(Value::as_f64).unwrap_or(0.0) * 10.0).round() / 10.0;
    let opacity = s.get("opacity").and_then(Value::as_f64).unwrap_or(100.0).round();
    format!("{size}|{opacity}|{}", color_signature(s.get("color")))
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// The group key of a run: `font|fauxBold|fauxItalic` (TypeR's `getFontGroupKey`).
pub fn group_key(text_style: &Value) -> String {
    let ps = str_of(text_style, "fontPostScriptName");
    let font = if ps.is_empty() { str_of(text_style, "fontName") } else { ps };
    let flag = |k: &str| if text_style.get(k).is_some_and(truthy) { 1 } else { 0 };
    format!("{font}|{}|{}", flag("syntheticBold"), flag("syntheticItalic"))
}

fn or_default<'a>(s: &'a str, d: &'a str) -> &'a str {
    if s.is_empty() { d } else { s }
}

fn variant_signature(text_style: &Value, layer: &ScanLayer) -> String {
    format!(
        "{}~{}~{}~{}~{}",
        round_size(text_style.get("size")),
        or_default(&layer.type_unit, "pixelsUnit"),
        or_default(&layer.anti_alias, "antiAliasSmooth"),
        color_signature(text_style.get("color")),
        stroke_signature(layer.stroke.as_ref())
    )
}

/// TypeR's paragraph defaults merged under the layer's paragraph style.
fn normalize_paragraph(p: Option<&Value>) -> Value {
    let mut out: Map<String, Value> = json!({
        "burasagari": "burasagariNone",
        "singleWordJustification": "justifyAll",
        "justificationMethodType": "justifMethodAutomatic",
        "textEveryLineComposer": false,
        "alignment": "center",
        "hangingRoman": true,
        "hyphenate": true,
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    if let Some(o) = p.and_then(Value::as_object) {
        out.extend(o.clone());
    }
    for (k, d) in [("burasagari", "burasagariNone"), ("singleWordJustification", "justifyAll"), ("justificationMethodType", "justifMethodAutomatic")] {
        if !out.get(k).is_some_and(truthy) {
            out.insert(k.into(), Value::String(d.into()));
        }
    }
    let composer = out.get("textEveryLineComposer").is_some_and(truthy);
    out.insert("textEveryLineComposer".into(), Value::Bool(composer));
    Value::Object(out)
}

/// TypeR's `normalizeStroke`: an off or empty stroke is the default disabled white stroke.
pub fn normalize_stroke(stroke: Option<&Value>) -> Value {
    let Some(s) = stroke_on(stroke) else {
        return json!({"enabled": false, "size": 0, "opacity": 100, "position": "outer", "color": {"r": 255, "g": 255, "b": 255}});
    };
    let size = (s.get("size").and_then(Value::as_f64).unwrap_or(0.0) * 100.0).round() / 100.0;
    let c = |k: &str| num(s.get("color").and_then(|c| c.get(k)).filter(|v| !v.is_null()).and_then(Value::as_f64).unwrap_or(255.0).round());
    json!({
        "enabled": true,
        "size": num(size),
        "opacity": num(s.get("opacity").filter(|v| !v.is_null()).and_then(Value::as_f64).unwrap_or(100.0).round()),
        "position": "outer",
        "color": {"r": c("r"), "g": c("g"), "b": c("b")},
    })
}

/// TypeR's `getGroupDisplayName`: the font name, its style unless Regular or already in the
/// name, and the faux flags.
pub fn display_name(font_name: &str, ps_name: &str, style_name: &str, faux_bold: bool, faux_italic: bool) -> String {
    let mut name = if !font_name.is_empty() {
        font_name.to_string()
    } else if !ps_name.is_empty() {
        ps_name.to_string()
    } else {
        "Unknown".to_string()
    };
    if !style_name.is_empty() && !style_name.eq_ignore_ascii_case("regular") && !name.to_lowercase().contains(&style_name.to_lowercase()) {
        name.push(' ');
        name.push_str(style_name);
    }
    if faux_bold {
        name.push_str(" (faux bold)");
    }
    if faux_italic {
        name.push_str(" (faux italic)");
    }
    name
}

struct Variant {
    sig: String,
    count: usize,
    size: f64,
    text_style: Value,
    type_unit: String,
    anti_alias: String,
    paragraph_style: Option<Value>,
    stroke: Option<Value>,
}

struct Group {
    key: String,
    order: usize,
    ps: String,
    font_name: String,
    style_name: String,
    faux_bold: bool,
    faux_italic: bool,
    usage: usize,
    files: BTreeSet<String>,
    /// Size in hundredths → count, in first-seen order.
    sizes: Vec<(i64, usize)>,
    variants: Vec<Variant>,
}

/// TypeR's `buildFontGroups`.
pub fn build_groups(files: &[ScanFile]) -> Vec<FontGroup> {
    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for file in files {
        for layer in &file.layers {
            let mut seen: BTreeSet<String> = BTreeSet::new();
            for ts in &layer.runs {
                let ps = str_of(ts, "fontPostScriptName");
                let font = str_of(ts, "fontName");
                if !ts.is_object() || (ps.is_empty() && font.is_empty()) {
                    continue;
                }
                let key = group_key(ts);
                let sig = variant_signature(ts, layer);
                if !seen.insert(format!("{key}::{sig}")) {
                    continue;
                }
                let gi = *index.entry(key.clone()).or_insert_with(|| {
                    groups.push(Group {
                        key: key.clone(),
                        order: groups.len(),
                        ps: ps.to_string(),
                        font_name: or_default(font, ps).to_string(),
                        style_name: str_of(ts, "fontStyleName").to_string(),
                        faux_bold: ts.get("syntheticBold").is_some_and(truthy),
                        faux_italic: ts.get("syntheticItalic").is_some_and(truthy),
                        usage: 0,
                        files: BTreeSet::new(),
                        sizes: Vec::new(),
                        variants: Vec::new(),
                    });
                    groups.len() - 1
                });
                let Some(g) = groups.get_mut(gi) else { continue };
                g.usage += 1;
                g.files.insert(file.file.clone());
                let size = round_size(ts.get("size"));
                let hundredths = (size * 100.0).round() as i64;
                match g.sizes.iter_mut().find(|(s, _)| *s == hundredths) {
                    Some((_, n)) => *n += 1,
                    None => g.sizes.push((hundredths, 1)),
                }
                match g.variants.iter_mut().find(|v| v.sig == sig) {
                    Some(v) => v.count += 1,
                    None => g.variants.push(Variant {
                        sig,
                        count: 1,
                        size,
                        text_style: ts.clone(),
                        type_unit: or_default(&layer.type_unit, "pixelsUnit").to_string(),
                        anti_alias: or_default(&layer.anti_alias, "antiAliasSmooth").to_string(),
                        paragraph_style: layer.paragraph_style.clone(),
                        stroke: layer.stroke.clone(),
                    }),
                }
            }
        }
    }
    let mut out: Vec<FontGroup> = groups.into_iter().filter_map(finish).collect();
    out.sort_by(|a, b| b.usage_count.cmp(&a.usage_count).then(a.order.cmp(&b.order)));
    out
}

fn finish(g: Group) -> Option<FontGroup> {
    let mut sizes: Vec<SizeCount> = g.sizes.iter().map(|(h, count)| SizeCount { size: *h as f64 / 100.0, count: *count }).collect();
    sizes.sort_by(|a, b| b.count.cmp(&a.count).then(a.size.total_cmp(&b.size)));
    let top = sizes.first().map_or(0.0, |s| s.size);
    let pick = |at_top: bool| {
        let mut best: Option<&Variant> = None;
        for v in &g.variants {
            if at_top && v.size != top {
                continue;
            }
            if best.is_none_or(|b| v.count > b.count) {
                best = Some(v);
            }
        }
        best
    };
    let rep = pick(true).or_else(|| pick(false))?;
    let mut ts = rep.text_style.as_object().cloned().unwrap_or_default();
    ts.remove("from");
    ts.remove("to");
    ts.insert("size".into(), num(top));
    ts.insert("impliedFontSize".into(), num(top));
    let text_props = json!({
        "layerText": {
            "textGridding": "none",
            "orientation": "horizontal",
            "antiAlias": rep.anti_alias,
            "textStyleRange": [{"from": 0, "to": 100, "textStyle": ts}],
            "paragraphStyleRange": [{"from": 0, "to": 100, "paragraphStyle": normalize_paragraph(rep.paragraph_style.as_ref())}],
        },
        "typeUnit": rep.type_unit,
    });
    Some(FontGroup {
        default_name: display_name(&g.font_name, &g.ps, &g.style_name, g.faux_bold, g.faux_italic),
        key: g.key,
        order: g.order,
        font_post_script_name: g.ps,
        font_name: g.font_name,
        font_style_name: g.style_name,
        synthetic_bold: g.faux_bold,
        synthetic_italic: g.faux_italic,
        usage_count: g.usage,
        file_count: g.files.len(),
        sizes,
        top_size: top,
        anti_alias: rep.anti_alias.clone(),
        text_props,
        stroke: normalize_stroke(rep.stroke.as_ref()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(over: Value) -> Value {
        let mut r = json!({
            "fontPostScriptName": "CCWildWords",
            "fontName": "CC Wild Words",
            "fontStyleName": "Regular",
            "syntheticBold": false,
            "syntheticItalic": false,
            "size": 16,
            "color": {"red": 0, "green": 0, "blue": 0},
        });
        if let (Some(o), Some(x)) = (r.as_object_mut(), over.as_object()) {
            o.extend(x.clone());
        }
        r
    }

    fn layer(runs: Vec<Value>) -> ScanLayer {
        ScanLayer {
            layer_name: "text layer".into(),
            anti_alias: "antiAliasSmooth".into(),
            type_unit: "pixelsUnit".into(),
            paragraph_style: Some(json!({"alignment": "center"})),
            stroke: None,
            runs,
        }
    }

    fn file(name: &str, layers: Vec<ScanLayer>) -> ScanFile {
        ScanFile { file: name.into(), layers }
    }

    // scripts/testFontScan.js
    #[test]
    fn variants_get_their_own_keys() {
        let base = group_key(&run(json!({})));
        assert_ne!(base, group_key(&run(json!({"fontPostScriptName": "CCWildWords-BoldItalic"}))));
        assert_ne!(base, group_key(&run(json!({"syntheticBold": true}))));
        assert_ne!(base, group_key(&run(json!({"syntheticItalic": true}))));
    }

    #[test]
    fn most_used_size_wins_and_variants_stay_apart() {
        let groups = build_groups(&[
            file(
                "a.psd",
                vec![
                    layer(vec![run(json!({"size": 16}))]),
                    layer(vec![run(json!({"size": 16}))]),
                    layer(vec![run(json!({"size": 20}))]),
                    layer(vec![run(json!({"fontPostScriptName": "CCWildWords-BoldItalic", "fontStyleName": "Bold Italic", "size": 18}))]),
                ],
            ),
            file("b.psd", vec![layer(vec![run(json!({"size": 16}))]), layer(vec![run(json!({"syntheticItalic": true, "size": 14}))])]),
        ]);
        assert_eq!(groups.len(), 3, "regular, real bold-italic and faux italic are separate");
        let regular = groups.iter().find(|g| g.key == group_key(&run(json!({})))).expect("regular group");
        assert_eq!(regular.top_size, 16.0, "most-used size wins");
        assert_eq!(regular.usage_count, 4);
        assert_eq!(regular.file_count, 2);
        assert_eq!(regular.text_props["layerText"]["textStyleRange"][0]["textStyle"]["size"], 16);
        assert_eq!(groups[0].key, regular.key, "sorted by usage");
        let faux = groups.iter().find(|g| g.synthetic_italic).expect("faux italic group");
        assert!(faux.default_name.to_lowercase().contains("faux italic"));
        let bold = groups.iter().find(|g| g.font_post_script_name == "CCWildWords-BoldItalic").expect("bold italic group");
        assert_eq!(bold.default_name, "CC Wild Words Bold Italic");

        // Stroke: off or missing is the default disabled stroke.
        assert_eq!(regular.stroke["enabled"], false);
        assert_eq!(regular.stroke["color"]["r"], 255);
        // Paragraph defaults mirror copyLayerStyle.
        let p = &regular.text_props["layerText"]["paragraphStyleRange"][0]["paragraphStyle"];
        assert_eq!(p["burasagari"], "burasagariNone");
        assert_eq!(p["singleWordJustification"], "justifyAll");
        assert_eq!(p["justificationMethodType"], "justifMethodAutomatic");
        assert_eq!(p["textEveryLineComposer"], false);
        assert_eq!(p["alignment"], "center");
    }

    #[test]
    fn identical_runs_in_a_layer_count_once() {
        let g = build_groups(&[file("c.psd", vec![layer(vec![run(json!({"size": 12})), run(json!({"size": 12})), run(json!({"size": 13}))])])]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].usage_count, 2, "identical runs in a layer deduplicated");
        // A tie goes to the smaller size.
        assert_eq!(g[0].top_size, 12.0);
    }

    #[test]
    fn the_representative_stroke_is_normalized() {
        let mut l = layer(vec![run(json!({"size": 15}))]);
        l.stroke = Some(json!({"enabled": true, "size": 3.13159, "opacity": 87.4, "position": "outer", "color": {"r": 254.6, "g": 0, "b": 0}}));
        let g = build_groups(&[file("d.psd", vec![l])]);
        assert_eq!(g[0].stroke["enabled"], true);
        assert_eq!(g[0].stroke["size"], 3.13);
        assert_eq!(g[0].stroke["opacity"], 87);
        assert_eq!(g[0].stroke["color"]["r"], 255);
        // The style to import.
        let s = g[0].style(Some("  "), Some(1234.7));
        assert_eq!((s["name"].as_str(), s["edited"].as_i64()), (Some("CC Wild Words"), Some(1234)));
        assert_eq!(s["prefixes"], json!([]));
    }

    #[test]
    fn empty_and_odd_input_stays_safe() {
        assert!(build_groups(&[]).is_empty());
        let odd = ScanLayer { runs: vec![json!(null), json!({"size": 3}), json!("x")], ..ScanLayer::default() };
        assert!(build_groups(&[file("e.psd", vec![odd])]).is_empty());
        assert_eq!(round_size(Some(&json!("12.345px"))), 12.35);
        assert_eq!(round_size(Some(&json!("abc"))), 0.0);
        assert_eq!(display_name("", "", "", true, false), "Unknown (faux bold)");
    }
}
