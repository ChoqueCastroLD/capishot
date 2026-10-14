// Ported from TypeR app_src/tplConverter.js (PR #209, branch sakushi/fix-pr-209 6acf8b2), MIT,
// (c) 2017 Swirt. See crates/typer/LICENSE-TypeR.txt.

//! Photoshop tool presets (`.tpl`) → TypeR styles (K6): every Type tool preset in the file
//! becomes a style in one folder named after the file.
//!
//! A `.tpl` starts with `8BTP`, has an `8BIM` resource, and its last `8BIMtptp` section lists
//! the presets: a Unicode name, 10 bytes, the tool's id and its properties as ActionDescriptor
//! values. Type tools (`typeCreateOrEditTool`…) keep their character options (`TxtS`) and
//! paragraph options; they map onto TypeR's `textProps` with Photoshop's defaults for what the
//! preset leaves out. Errors carry TypeR's codes (`TPL_INVALID_FILE`, `TPL_TRUNCATED_DATA`,
//! `TPL_UNSUPPORTED_DATA`, `TPL_MISSING_TOOL_DATA`, `TPL_NO_TEXT_PRESETS`).

use serde_json::{Map, Value, json};

use crate::id::num;

/// Largest `.tpl` read (Photoshop's own preset files are a few MB at most).
pub const MAX_TPL_BYTES: usize = 64 << 20;
/// Deepest descriptor nesting followed.
const MAX_DEPTH: usize = 64;

/// A `.tpl` that can't be converted, with TypeR's error code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TplError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for TplError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

fn err(code: &'static str, message: impl Into<String>) -> TplError {
    TplError { code, message: message.into() }
}

type R<T> = Result<T, TplError>;

/// One tool preset.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolPreset {
    pub name: String,
    /// The tool's id (`typeCreateOrEditTool`, `paintbrushTool`, …).
    pub tool: String,
    pub properties: Map<String, Value>,
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn remaining(&self) -> usize {
        self.b.len().saturating_sub(self.at)
    }

    fn take(&mut self, n: usize) -> R<&[u8]> {
        let end = self
            .at
            .checked_add(n)
            .filter(|e| *e <= self.b.len())
            .ok_or_else(|| err("TPL_TRUNCATED_DATA", format!("unexpected end of the data at byte {}", self.at)))?;
        let s = self.b.get(self.at..end).ok_or_else(|| err("TPL_TRUNCATED_DATA", "unexpected end of the data"))?;
        self.at = end;
        Ok(s)
    }

    fn u8(&mut self) -> R<u8> {
        Ok(self.take(1)?.first().copied().unwrap_or(0))
    }

    fn u32(&mut self) -> R<u32> {
        let s = self.take(4)?;
        Ok(s.iter().fold(0u32, |a, b| (a << 8) | u32::from(*b)))
    }

    fn i32(&mut self) -> R<i32> {
        Ok(self.u32()? as i32)
    }

    fn i64(&mut self) -> R<i64> {
        let hi = i64::from(self.i32()?);
        let lo = i64::from(self.u32()?);
        Ok(hi.saturating_mul(1 << 32).saturating_add(lo))
    }

    fn f64(&mut self) -> R<f64> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(f64::from_be_bytes(a))
    }

    fn ascii(&mut self, n: usize) -> R<String> {
        Ok(self.take(n)?.iter().map(|b| char::from(*b)).collect())
    }

    fn unicode(&mut self) -> R<String> {
        let len = self.u32()? as usize;
        if len > self.remaining() / 2 {
            return Err(err("TPL_TRUNCATED_DATA", format!("invalid Unicode string length at byte {}", self.at.saturating_sub(4))));
        }
        let units: Vec<u16> = self.take(len * 2)?.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).filter(|u| *u != 0).collect();
        Ok(String::from_utf16_lossy(&units))
    }

    fn id(&mut self) -> R<String> {
        let mut len = self.u32()? as usize;
        if len == 0 {
            len = 4;
        }
        if len > 1024 || len > self.remaining() {
            return Err(err("TPL_TRUNCATED_DATA", format!("invalid descriptor identifier length at byte {}", self.at.saturating_sub(4))));
        }
        Ok(clean_id(&self.ascii(len)?))
    }

    fn class(&mut self) -> R<Value> {
        let name = self.unicode()?;
        let id = self.id()?;
        Ok(json!({"name": name, "id": id}))
    }

    fn reference(&mut self) -> R<Value> {
        let count = self.u32()?;
        let mut items = Vec::new();
        for _ in 0..count {
            let form = self.ascii(4)?;
            let class = self.class()?;
            let value = match form.as_str() {
                "prop" => Value::String(self.id()?),
                "Enmr" => json!({"type": self.id()?, "value": self.id()?}),
                "rele" | "Idnt" | "indx" => json!(self.i32()?),
                "name" => Value::String(self.unicode()?),
                "Clss" => Value::Null,
                _ => return Err(err("TPL_UNSUPPORTED_DATA", format!("unsupported reference form {form}"))),
            };
            items.push(json!({"form": form, "desiredClass": class, "value": value}));
        }
        Ok(Value::Array(items))
    }

    fn descriptor(&mut self, depth: usize) -> R<Value> {
        let _class = self.class()?;
        let count = self.u32()?;
        if count > 100_000 {
            return Err(err("TPL_UNSUPPORTED_DATA", "a descriptor has too many properties"));
        }
        let mut m = Map::new();
        for _ in 0..count {
            let key = self.id()?;
            let v = self.value(depth + 1)?;
            m.insert(key, v);
        }
        Ok(Value::Object(m))
    }

    fn object_array(&mut self) -> R<Value> {
        let object_count = self.u32()?;
        let class = self.class()?;
        let n = self.u32()?;
        let mut items = Map::new();
        for _ in 0..n {
            let key = self.id()?;
            let ty = self.ascii(4)?;
            let unit = self.ascii(4)?;
            let count = self.u32()? as usize;
            if count > self.remaining() / 8 {
                return Err(err("TPL_TRUNCATED_DATA", "an object array is longer than the data"));
            }
            let values: Vec<Value> = (0..count).map(|_| self.f64().map(num)).collect::<R<_>>()?;
            items.insert(key, json!({"type": ty, "unit": unit, "values": values}));
        }
        Ok(json!({"objectCount": object_count, "objectClass": class, "items": items}))
    }

    fn value(&mut self, depth: usize) -> R<Value> {
        if depth > MAX_DEPTH {
            return Err(err("TPL_UNSUPPORTED_DATA", "descriptors nested too deep"));
        }
        let ty = self.ascii(4)?;
        Ok(match ty.as_str() {
            "Objc" | "GlbO" => self.descriptor(depth)?,
            "VlLs" => {
                let count = self.u32()?;
                if count > 100_000 {
                    return Err(err("TPL_UNSUPPORTED_DATA", "a descriptor list is too large"));
                }
                Value::Array((0..count).map(|_| self.value(depth + 1)).collect::<R<_>>()?)
            }
            "doub" => num(self.f64()?),
            "UntF" => {
                let unit = self.ascii(4)?;
                json!({"unit": unit, "value": num(self.f64()?)})
            }
            "TEXT" => Value::String(self.unicode()?),
            "enum" => json!({"enumType": self.id()?, "value": self.id()?}),
            "long" => json!(self.i32()?),
            "comp" => json!(self.i64()?),
            "bool" => Value::Bool(self.u8()? != 0),
            "type" | "GlbC" => self.class()?,
            "obj " => self.reference()?,
            "alis" | "Pth " | "tdta" => {
                let len = self.u32()? as usize;
                let data: String = self.take(len)?.iter().map(|b| format!("{b:02x}")).collect();
                json!({"type": ty, "data": data})
            }
            "ObAr" => self.object_array()?,
            _ => return Err(err("TPL_UNSUPPORTED_DATA", format!("unsupported descriptor value type {ty:?} at byte {}", self.at.saturating_sub(4)))),
        })
    }
}

fn clean_id(s: &str) -> String {
    s.trim_end_matches('\0').trim().to_string()
}

/// Every tool preset in a `.tpl` file.
pub fn parse(bytes: &[u8]) -> Result<Vec<ToolPreset>, TplError> {
    if bytes.len() > MAX_TPL_BYTES {
        return Err(err("TPL_INVALID_FILE", format!("the file is larger than {} MB", MAX_TPL_BYTES >> 20)));
    }
    let mut r = Reader { b: bytes, at: 0 };
    if r.remaining() < 16 || r.ascii(4)? != "8BTP" {
        return Err(err("TPL_INVALID_FILE", "not a Photoshop tool preset file (no 8BTP header)"));
    }
    r.take(8)?;
    if r.ascii(4)? != "8BIM" {
        return Err(err("TPL_INVALID_FILE", "missing Photoshop resource signature"));
    }
    let section = bytes.windows(8).rposition(|w| w == b"8BIMtptp").ok_or_else(|| err("TPL_MISSING_TOOL_DATA", "no tool preset data section"))?;
    let mut r = Reader { b: bytes, at: section + 16 };
    let mut tools = Vec::new();
    while r.remaining() >= 4 {
        if bytes.get(r.at..).is_none_or(|rest| rest.iter().all(|b| *b == 0)) {
            break;
        }
        let name = r.unicode()?;
        r.take(10)?;
        let tool = r.id()?;
        let count = r.u32()?;
        if count > 100_000 {
            return Err(err("TPL_UNSUPPORTED_DATA", "a tool preset has too many properties"));
        }
        let mut properties = Map::new();
        for _ in 0..count {
            let key = r.id()?;
            let v = r.value(0)?;
            properties.insert(key, v);
        }
        tools.push(ToolPreset { name, tool, properties });
    }
    Ok(tools)
}

/// TypeR's `unwrapValue`: a unit value's number, an enum's value (Photoshop's short ids
/// spelled out), anything else as it is.
fn unwrap(v: &Value) -> Value {
    if let Some(o) = v.as_object() {
        if o.contains_key("unit") {
            return o.get("value").cloned().unwrap_or(Value::Null);
        }
        if o.contains_key("enumType") {
            let s = o.get("value").and_then(Value::as_str).unwrap_or_default();
            let mapped = match s {
                "Nrml" => "normal",
                "Left" => "left",
                "Rght" => "right",
                "Cntr" => "center",
                "JstA" => "justifyAll",
                "JstF" => "justifyFull",
                "Hrzn" => "horizontal",
                "Vrtc" => "vertical",
                "None" => "none",
                other => other,
            };
            return Value::String(mapped.to_string());
        }
    }
    v.clone()
}

fn color(v: &Value, fallback: [f64; 3]) -> Value {
    let Some(o) = v.as_object() else { return json!({"red": num(fallback[0]), "green": num(fallback[1]), "blue": num(fallback[2])}) };
    let ch = |a: &str, b: &str, d: f64| {
        let x = o.get(a).filter(|x| !x.is_null()).or_else(|| o.get(b)).map(unwrap);
        let n = match x {
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
            Some(Value::Bool(b)) => Some(if b { 1.0 } else { 0.0 }),
            _ => None,
        };
        num(n.filter(|f| f.is_finite()).unwrap_or(d))
    };
    json!({"red": ch("Rd", "red", fallback[0]), "green": ch("Grn", "green", fallback[1]), "blue": ch("Bl", "blue", fallback[2])})
}

fn default_text_style() -> Map<String, Value> {
    // A JSON literal (too many keys for `json!`); it always parses.
    serde_json::from_str(
        r#"{
        "styleSheetHasParent": true, "fontPostScriptName": "MyriadPro-Regular", "fontName": "Myriad Pro", "fontStyleName": "Regular",
        "fontScript": 0, "fontTechnology": 0, "fontAvailable": true, "size": 12, "impliedFontSize": 12, "horizontalScale": 100,
        "verticalScale": 100, "characterRotation": 0, "syntheticBold": false, "syntheticItalic": false, "autoLeading": true,
        "tracking": 0, "baselineShift": 0, "impliedBaselineShift": 0, "autoKern": "metricsKern", "fontCaps": "normal",
        "digitSet": "defaultDigits", "kashidas": "kashidaDefault", "diacXOffset": 0, "diacYOffset": 0, "markYDistFromBaseline": 100,
        "baseline": "normal", "otbaseline": "normal", "strikethrough": "strikethroughOff", "underline": "underlineOff",
        "underlineOffset": 0, "ligature": true, "altligature": true, "contextualLigatures": false, "alternateLigatures": false,
        "oldStyle": false, "fractions": false, "ordinals": false, "swash": false, "titling": false, "connectionForms": false,
        "stylisticAlternates": false, "ornaments": false, "justificationAlternates": false, "figureStyle": "normal",
        "proportionalMetrics": false, "kana": false, "italics": false, "ruby": false, "baselineDirection": "withStream",
        "textLanguage": "englishLanguage", "japaneseAlternate": "defaultForm", "mojiZume": 0, "gridAlignment": "roman",
        "enableWariChu": false, "wariChuCount": 2, "wariChuLineGap": 0, "wariChuScale": 0.5, "wariChuWidow": 2, "wariChuOrphan": 2,
        "wariChuJustification": "wariChuAutoJustify", "tcyUpDown": 0, "tcyLeftRight": 0, "leftAki": -1, "rightAki": -1, "jiDori": 0,
        "noBreak": false, "color": {"red": 0, "green": 0, "blue": 0}, "strokeColor": {"red": 0, "green": 0, "blue": 0},
        "fill": true, "stroke": false, "fillFirst": true, "fillOverPrint": false, "strokeOverPrint": false, "lineCap": "buttCap",
        "lineJoin": "miterJoin", "lineWidth": 1, "miterLimit": 4, "lineDashoffset": 0
    }"#,
    )
    .unwrap_or_default()
}

fn default_paragraph_style() -> Map<String, Value> {
    // A JSON literal (too many keys for `json!`); it always parses.
    serde_json::from_str(
        r#"{
        "styleSheetHasParent": true, "alignment": "left", "firstLineIndent": 0, "impliedFirstLineIndent": 0, "startIndent": 0,
        "impliedStartIndent": 0, "endIndent": 0, "impliedEndIndent": 0, "spaceBefore": 0, "impliedSpaceBefore": 0, "spaceAfter": 0,
        "impliedSpaceAfter": 0, "autoLeadingPercentage": 1.2, "leadingType": "leadingBelow", "directionType": "dirLeftToRight",
        "kashidaWidthType": "kashidaWidthMedium", "hyphenate": false, "dropCapMultiplier": 1, "hyphenateWordSize": 6,
        "hyphenatePreLength": 2, "hyphenatePostLength": 2, "hyphenateLimit": 0, "hyphenationZone": 36, "hyphenateCapitalized": true,
        "hyphenationPreference": 0.5, "justificationWordMinimum": 0.8, "justificationWordDesired": 1, "justificationWordMaximum": 1.33,
        "justificationLetterMinimum": 0, "justificationLetterDesired": 0, "justificationLetterMaximum": 0,
        "justificationGlyphMinimum": 1, "justificationGlyphDesired": 1, "justificationGlyphMaximum": 1, "hangingRoman": false,
        "autoTCY": 0, "keepTogether": true, "burasagari": "burasagariNone", "preferredKinsokuOrder": "pushIn",
        "kurikaeshiMojiShori": false, "textEveryLineComposer": false, "defaultTabWidth": 36,
        "textComposerEngine": "textLatinCJKComposer", "singleWordJustification": "justifyAll",
        "justificationMethodType": "justifMethodAutomatic"
    }"#,
    )
    .unwrap_or_default()
}

/// TypeR's `normalizeTextStyle`: Photoshop's defaults, overridden by the preset's character
/// options (short ids such as `FntN`, `Sz`, `Clr` spelled out).
fn text_style(raw: &Map<String, Value>) -> Map<String, Value> {
    let mut ts = default_text_style();
    for (raw_key, v) in raw {
        let key = match raw_key.as_str() {
            "FntN" => "fontName",
            "FntS" => "fontStyleName",
            "Scrp" => "fontScript",
            "FntT" => "fontTechnology",
            "Sz" => "size",
            "HrzS" => "horizontalScale",
            "VrtS" => "verticalScale",
            "Trck" => "tracking",
            "Bsln" => "baselineShift",
            "AtKr" => "autoKern",
            "Undl" => "underline",
            "Clr" => "color",
            "Ldng" => "leading",
            "Fl" => "fill",
            "Strk" => "stroke",
            k => k,
        };
        if key == "color" || key == "strokeColor" {
            let fallback = ts.get(key).map_or([0.0; 3], |c| ["red", "green", "blue"].map(|k| c.get(k).and_then(Value::as_f64).unwrap_or(0.0)));
            ts.insert(key.to_string(), color(v, fallback));
        } else if ts.contains_key(key) || key == "leading" {
            ts.insert(key.to_string(), unwrap(v));
        }
    }
    let ps = ts.get("fontPostScriptName").and_then(Value::as_str).unwrap_or_default().to_string();
    if ps.is_empty() {
        let f = ts.get("fontName").and_then(Value::as_str).unwrap_or_default().to_string();
        let st = ts.get("fontStyleName").and_then(Value::as_str).unwrap_or_default().to_string();
        ts.insert("fontPostScriptName".into(), Value::String(format!("{f}-{st}")));
    }
    let size = ts.get("size").cloned().unwrap_or(Value::Null);
    ts.insert("impliedFontSize".into(), size);
    let shift = ts.get("baselineShift").cloned().unwrap_or(Value::Null);
    ts.insert("impliedBaselineShift".into(), shift);
    ts
}

fn paragraph_style(raw: &Map<String, Value>) -> Map<String, Value> {
    let mut ps = default_paragraph_style();
    for (raw_key, v) in raw {
        let key = if raw_key == "Algn" { "alignment" } else { raw_key.as_str() };
        if ps.contains_key(key) {
            ps.insert(key.to_string(), unwrap(v));
        }
    }
    for (implied, from) in [
        ("impliedFirstLineIndent", "firstLineIndent"),
        ("impliedStartIndent", "startIndent"),
        ("impliedEndIndent", "endIndent"),
        ("impliedSpaceBefore", "spaceBefore"),
        ("impliedSpaceAfter", "spaceAfter"),
    ] {
        let v = ps.get(from).cloned().unwrap_or(Value::Null);
        ps.insert(implied.into(), v);
    }
    ps
}

/// A Type tool preset as a TypeR style (no id or folder yet), else `None` for other tools or a
/// preset without character options.
pub fn style_from_preset(tool: &ToolPreset) -> Option<Value> {
    let empty = Map::new();
    let obj = |v: Option<&Value>| v.and_then(Value::as_object).unwrap_or(&empty).clone();
    let para_opts = obj(tool.properties.get("textToolParagraphOptions"));
    let char_opts = obj(tool.properties.get("textToolCharacterOptions"));
    let raw_text = char_opts.get("TxtS").or_else(|| para_opts.get("paragraphStyle").and_then(|p| p.get("defaultStyle"))).and_then(Value::as_object)?;
    let raw_para = obj(para_opts.get("paragraphStyle"));
    let ts = text_style(raw_text);
    let ps = paragraph_style(&raw_para);
    let orientation = para_opts.get("Ornt").map(unwrap).filter(|v| v.as_str().is_some_and(|s| !s.is_empty())).unwrap_or_else(|| {
        let vertical = tool.properties.get("textToolOptions").and_then(|o| o.get("textNewTextOrientation")).and_then(Value::as_i64) == Some(1);
        Value::String(if vertical { "vertical" } else { "horizontal" }.into())
    });
    let gridding = char_opts.get("textGridding").map(unwrap).filter(|v| v.as_str().is_some_and(|s| !s.is_empty())).unwrap_or_else(|| json!("none"));
    let aa = ["antiAliasNone", "antiAliasSharp", "antiAliasCrisp", "antiAliasStrong", "antiAliasSmooth"];
    let anti_alias =
        raw_text.get("AntA").and_then(Value::as_i64).and_then(|i| usize::try_from(i).ok()).and_then(|i| aa.get(i)).copied().unwrap_or("antiAliasSmooth");
    let unit = match raw_text.get("Sz").and_then(|s| s.get("unit")).and_then(Value::as_str) {
        Some("#Pxl") => "pixelsUnit",
        _ => "pointsUnit",
    };
    let preset_name = tool.name.rsplit('=').next().unwrap_or_default().trim().to_string();
    let name = if preset_name.is_empty() {
        format!("{} {}", ts.get("fontName").and_then(Value::as_str).unwrap_or_default(), ts.get("fontStyleName").and_then(Value::as_str).unwrap_or_default())
    } else {
        preset_name
    };
    Some(json!({
        "name": name,
        "textType": "inherit",
        "textProps": {
            "layerText": {
                "textGridding": gridding,
                "orientation": orientation,
                "antiAlias": anti_alias,
                "textStyleRange": [{"from": 0, "to": 100, "textStyle": ts}],
                "paragraphStyleRange": [{"from": 0, "to": 100, "paragraphStyle": ps}],
                "kerningRange": [],
            },
            "typeUnit": unit,
        },
        "prefixes": [],
        "prefixColor": "#FFF3B0",
        "stroke": {"enabled": false, "size": 0, "opacity": 100, "position": "outer", "color": {"r": 255, "g": 255, "b": 255}},
    }))
}

/// TypeR's `convertTplHexToTypeRFormat`: a TypeR library (`{folders, styles}`) with one folder
/// named after the file and a style per Type tool preset. Ids are `tpl-1`, `tpl-1-s1`, …; a
/// library import gives them fresh ones. `now` stamps `edited`.
pub fn convert(bytes: &[u8], file_name: &str, now: Option<f64>) -> Result<Value, TplError> {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    let folder_name = match base.len().checked_sub(4).filter(|i| base.get(*i..).is_some_and(|e| e.eq_ignore_ascii_case(".tpl"))) {
        Some(i) => base.get(..i).unwrap_or(base),
        None => base,
    }
    .trim();
    let folder_name = if folder_name.is_empty() { "TPL" } else { folder_name };
    let tools = parse(bytes)?;
    let mut styles = Vec::new();
    for t in tools.iter().filter(|t| t.tool.starts_with("typeCreateOrEdit")) {
        if let Some(mut s) = style_from_preset(t) {
            s["id"] = json!(format!("tpl-1-s{}", styles.len() + 1));
            s["folder"] = json!("tpl-1");
            if let Some(now) = now.filter(|n| n.is_finite()) {
                s["edited"] = num(now.floor());
            }
            styles.push(s);
        }
    }
    if styles.is_empty() {
        return Err(err("TPL_NO_TEXT_PRESETS", "no Photoshop Type tool presets in the file"));
    }
    Ok(json!({"folders": [{"id": "tpl-1", "name": folder_name, "parentId": null, "order": 0}], "styles": styles}))
}

#[cfg(test)]
mod tests {
    use super::*;

    // scripts/testTplConverter.js, its fixture writer ported.
    fn u32b(v: u32) -> Vec<u8> {
        v.to_be_bytes().to_vec()
    }
    fn unicode(s: &str) -> Vec<u8> {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut out = u32b(units.len() as u32 + 1);
        for u in units {
            out.extend(u.to_be_bytes());
        }
        out.extend([0, 0]);
        out
    }
    fn id(s: &str) -> Vec<u8> {
        let mut out = if s.len() == 4 { u32b(0) } else { u32b(s.len() as u32) };
        out.extend(s.as_bytes());
        out
    }
    fn typed(t: &str, v: Vec<u8>) -> Vec<u8> {
        let mut out = t.as_bytes().to_vec();
        out.extend(v);
        out
    }
    fn text(s: &str) -> Vec<u8> {
        typed("TEXT", unicode(s))
    }
    fn unit(u: &str, v: f64) -> Vec<u8> {
        let mut b = u.as_bytes().to_vec();
        b.extend(v.to_be_bytes());
        typed("UntF", b)
    }
    fn enumerated(t: &str, v: &str) -> Vec<u8> {
        let mut b = id(t);
        b.extend(id(v));
        typed("enum", b)
    }
    fn integer(v: i32) -> Vec<u8> {
        typed("long", v.to_be_bytes().to_vec())
    }
    fn boolean(v: bool) -> Vec<u8> {
        typed("bool", vec![u8::from(v)])
    }
    fn number(v: f64) -> Vec<u8> {
        typed("doub", v.to_be_bytes().to_vec())
    }
    fn props(p: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = u32b(p.len() as u32);
        for (k, v) in p {
            out.extend(id(k));
            out.extend(v);
        }
        out
    }
    fn object(class: &str, p: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut b = unicode("");
        b.extend(id(class));
        b.extend(props(p));
        typed("Objc", b)
    }
    fn tool(name: &str, ty: &str, p: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = unicode(name);
        out.extend([0u8; 10]);
        out.extend(id(ty));
        out.extend(props(p));
        out
    }
    fn tpl(tools: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"8BTP".to_vec();
        out.extend([0u8; 8]);
        out.extend(b"8BIM");
        out.extend([0u8; 4]);
        out.extend(b"8BIMtptp");
        out.extend([0u8; 8]);
        for t in tools {
            out.extend(t);
        }
        out
    }

    fn text_preset() -> Vec<u8> {
        let ts = [
            ("fontPostScriptName", text("CafeNihon-Bold")),
            ("FntN", text("Écriture 日本")),
            ("FntS", text("Gras")),
            ("Sz  ", unit("#Pnt", 37.5)),
            ("HrzS", number(92.0)),
            ("VrtS", number(108.0)),
            ("Trck", integer(125)),
            ("Bsln", unit("#Pnt", 2.25)),
            ("AtKr", enumerated("AtKr", "opticalKern")),
            ("textLanguage", enumerated("textLanguage", "frenchLanguage")),
            ("Clr ", object("RGBC", &[("Rd  ", number(12.0)), ("Grn ", number(34.0)), ("Bl  ", number(56.0))])),
            ("strokeColor", object("RGBC", &[("Rd  ", number(78.0)), ("Grn ", number(90.0)), ("Bl  ", number(123.0))])),
            ("Strk", boolean(true)),
            ("lineWidth", unit("#Pnt", 3.0)),
            ("AntA", integer(4)),
        ];
        tool(
            "$$$/Presets/Text=Café 日本 😀",
            "typeCreateOrEditTool",
            &[
                (
                    "textToolParagraphOptions",
                    object(
                        "textToolParagraphOptions",
                        &[
                            (
                                "paragraphStyle",
                                object(
                                    "paragraphStyle",
                                    &[("Algn", enumerated("Alg ", "Rght")), ("firstLineIndent", unit("#Pnt", 4.0)), ("autoLeadingPercentage", number(1.35))],
                                ),
                            ),
                            ("Ornt", enumerated("Ornt", "Vrtc")),
                        ],
                    ),
                ),
                (
                    "textToolCharacterOptions",
                    object("textToolCharacterOptions", &[("TxtS", object("TxtS", &ts)), ("textGridding", enumerated("textGridding", "None"))]),
                ),
            ],
        )
    }

    #[test]
    fn type_presets_convert_to_styles() {
        let fixture = tpl(&[text_preset()]);
        let parsed = parse(&fixture).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "$$$/Presets/Text=Café 日本 😀");
        assert_eq!(parsed[0].properties["textToolCharacterOptions"]["TxtS"]["FntN"], "Écriture 日本");

        let lib = convert(&fixture, "Styles internationaux.tpl", Some(5.0)).unwrap();
        assert_eq!(lib["folders"][0]["name"], "Styles internationaux");
        assert_eq!(lib["styles"].as_array().map(Vec::len), Some(1));
        let style = &lib["styles"][0];
        assert_eq!(style["name"], "Café 日本 😀");
        let c = &style["textProps"]["layerText"]["textStyleRange"][0]["textStyle"];
        let p = &style["textProps"]["layerText"]["paragraphStyleRange"][0]["paragraphStyle"];
        assert_eq!(c["fontPostScriptName"], "CafeNihon-Bold");
        assert_eq!(c["fontName"], "Écriture 日本");
        assert_eq!(c["fontStyleName"], "Gras");
        assert_eq!(c["size"], 37.5);
        assert_eq!(c["impliedFontSize"], 37.5);
        assert_eq!(c["horizontalScale"], 92);
        assert_eq!(c["verticalScale"], 108);
        assert_eq!(c["tracking"], 125);
        assert_eq!(c["baselineShift"], 2.25);
        assert_eq!(c["autoKern"], "opticalKern");
        assert_eq!(c["textLanguage"], "frenchLanguage");
        assert_eq!(c["color"], json!({"red": 12, "green": 34, "blue": 56}));
        assert_eq!(c["strokeColor"], json!({"red": 78, "green": 90, "blue": 123}));
        assert_eq!(c["stroke"], true);
        assert_eq!(c["lineWidth"], 3);
        assert_eq!(p["alignment"], "right");
        assert_eq!(p["firstLineIndent"], 4);
        assert_eq!(p["autoLeadingPercentage"], 1.35);
        assert_eq!(style["textProps"]["layerText"]["orientation"], "vertical");
        assert_eq!(style["textProps"]["layerText"]["antiAlias"], "antiAliasSmooth");
        assert_eq!(style["textProps"]["typeUnit"], "pointsUnit");
        assert_eq!(style["stroke"], json!({"enabled": false, "size": 0, "opacity": 100, "position": "outer", "color": {"r": 255, "g": 255, "b": 255}}));
        assert_eq!(style["edited"], 5);
        // It imports as a TypeR library.
        crate::exchange::validate(&lib).unwrap();
    }

    #[test]
    fn photoshop_defaults_parse() {
        assert_eq!(default_text_style().len(), 78);
        assert_eq!(default_paragraph_style().len(), 45);
    }

    #[test]
    fn bad_files_report_typer_codes() {
        assert_eq!(parse(b"not a tpl").unwrap_err().code, "TPL_INVALID_FILE");
        assert_eq!(parse(b"8BTP\0\0\0\0\0\0\0\0XXXX\0\0").unwrap_err().code, "TPL_INVALID_FILE");
        assert_eq!(parse(b"8BTP\0\0\0\0\0\0\0\08BIM\0\0\0\0").unwrap_err().code, "TPL_MISSING_TOOL_DATA");
        let brush = tool("Brush", "paintbrushTool", &[]);
        assert_eq!(convert(&tpl(&[brush]), "Brushes.tpl", None).unwrap_err().code, "TPL_NO_TEXT_PRESETS");
        // Cut anywhere: an error, never a panic.
        let full = tpl(&[text_preset()]);
        for cut in (24..full.len()).step_by(7) {
            if let Err(e) = parse(&full[..cut]) {
                assert!(["TPL_TRUNCATED_DATA", "TPL_UNSUPPORTED_DATA", "TPL_MISSING_TOOL_DATA"].contains(&e.code), "{cut}: {e}");
            }
        }
        let mut odd = tpl(&[tool("x", "typeCreateOrEditTool", &[("a", typed("ZZZZ", vec![]))])]);
        assert_eq!(parse(&odd).unwrap_err().code, "TPL_UNSUPPORTED_DATA");
        odd.truncate(30);
        assert_eq!(convert(&odd, "x.tpl", None).unwrap_err().code, "TPL_NO_TEXT_PRESETS", "a cut section header lists no presets");
        // Deep nesting is refused.
        let mut deep = typed("bool", vec![1]);
        for _ in 0..80 {
            let mut v = typed("VlLs", u32b(1));
            v.extend(deep);
            deep = v;
        }
        assert_eq!(parse(&tpl(&[tool("d", "typeCreateOrEditTool", &[("a", deep)])])).unwrap_err().code, "TPL_UNSUPPORTED_DATA");
        assert_eq!(convert(&tpl(&[text_preset()]), "", None).unwrap()["folders"][0]["name"], "TPL");
    }
}
