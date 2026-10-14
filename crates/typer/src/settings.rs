// Setting names and defaults follow TypeR app_src/context.jsx (develop 71bc061), MIT, (c) 2017
// Swirt. See crates/typer/LICENSE-TypeR.txt.

//! TypeR settings (§2.10 of the port plan), with TypeR 3.0's defaults. Keys are TypeR's
//! (camelCase), so settings exported by TypeR import directly.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::id::Id;
use crate::library::split_list;

/// Text direction for paste and apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Ltr,
    Rtl,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Paste point text instead of paragraph (box) text (P1).
    pub paste_point_text: bool,
    /// Lines starting with one of these are skipped (S2).
    pub ignore_line_prefixes: Vec<String>,
    /// Removed anywhere in a line, which is kept (S7).
    pub ignore_tags: Vec<String>,
    /// Style of lines without a tag (S8).
    pub default_style_id: Option<Id>,
    pub direction: Direction,
    /// Show the diacritic offset fields in the style editor.
    pub middle_east: bool,
    /// Close the previously opened page file when the next one opens (N5; Capishot asks first).
    #[serde(rename = "autoClosePSD")]
    pub auto_close_psd: bool,
    /// Scroll the style list to the selected style.
    pub auto_scroll_style: bool,
    /// Match tags only among the current style's folder (S5).
    pub current_folder_tag_priority: bool,
    /// Align also resizes box text to the bubble (P5).
    pub resize_text_box_on_center: bool,
    /// Multi-bubble capture mode (M1).
    pub multi_bubble_mode: bool,
    /// Horizontal padding inside the bubble, px, 0..100 (P1).
    pub internal_padding: f64,
    /// `**bold**` / `*italic*` in lines become faux bold/italic runs (S9).
    pub interpret_markdown: bool,
    pub show_tips: bool,
    pub show_quick_style_size: bool,
    /// Step of the quick style size −/+ buttons.
    pub style_size_step: f64,
    /// Step of Size −/+ on layers, 1..99 (P6).
    pub text_size_increment: f64,
    /// Paste at this % of the style size, 1..999; `None` = 100 (S14).
    pub text_scale: Option<f64>,
    /// Line numbers restart after each page marker (S3).
    pub reset_line_counter_on_page: bool,
    pub multi_tab_enabled: bool,
    pub inline_text_shape_r: bool,
    pub dehyphenate_text_shape_r: bool,
    /// Capishot: match the longest tag instead of the first one in style order (§6.6).
    pub longest_prefix_match: bool,
    /// Capishot: multi paste centres on the opened bubble like Paste instead of the raw
    /// selection bounds (TypeR's behaviour, the default) (§6.6).
    pub open_stored_selections: bool,
    /// Capishot: besides TypeR's `Page N`, page markers may start with
    /// [`Settings::page_marker_words`] (`Página 12`, `Pág. 12`).
    pub extra_page_markers: bool,
    /// Capishot: the extra marker words (case and accents don't matter; a trailing `.` is
    /// optional). Spanish and Portuguese by default.
    pub page_marker_words: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            paste_point_text: false,
            ignore_line_prefixes: vec!["##".into()],
            ignore_tags: Vec::new(),
            default_style_id: None,
            direction: Direction::Ltr,
            middle_east: false,
            auto_close_psd: false,
            auto_scroll_style: true,
            current_folder_tag_priority: true,
            resize_text_box_on_center: false,
            multi_bubble_mode: false,
            internal_padding: 10.0,
            interpret_markdown: true,
            show_tips: true,
            show_quick_style_size: true,
            style_size_step: 1.0,
            text_size_increment: 1.0,
            text_scale: None,
            reset_line_counter_on_page: true,
            multi_tab_enabled: true,
            inline_text_shape_r: true,
            dehyphenate_text_shape_r: false,
            longest_prefix_match: false,
            open_stored_selections: false,
            extra_page_markers: true,
            page_marker_words: crate::pages::DEFAULT_EXTRA_PAGE_WORDS.iter().map(|w| (*w).to_string()).collect(),
        }
    }
}

/// Settings keys a TypeR full export carries besides the library.
pub const EXPORTED_KEYS: &[&str] =
    &["ignoreLinePrefixes", "ignoreTags", "defaultStyleId", "autoClosePSD", "autoScrollStyle", "currentFolderTagPriority", "pastePointText"];

impl Settings {
    /// All settings as JSON (TypeR key names).
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// Set one setting from JSON, with TypeR's coercions: prefix and tag lists also accept one
    /// string split on newlines and `;`; `textScale` is clamped to 1..999 (0, `null` or "" =
    /// 100 %); `textSizeIncrement` to 1..99; `internalPadding` to 0..100.
    pub fn set(&mut self, key: &str, value: &Value) -> Result<(), String> {
        let list = |v: &Value| -> Result<Vec<String>, String> {
            match v {
                Value::Null => Ok(Vec::new()),
                Value::String(s) => Ok(split_list(s)),
                Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_string).ok_or_else(|| format!("\"{key}\" entries must be strings"))).collect(),
                _ => Err(format!("\"{key}\" must be a list of strings or one string")),
            }
        };
        let boolean = |v: &Value| v.as_bool().ok_or_else(|| format!("\"{key}\" must be true or false"));
        let number = |v: &Value| v.as_f64().filter(|f| f.is_finite()).ok_or_else(|| format!("\"{key}\" must be a number"));
        match key {
            "pastePointText" => self.paste_point_text = boolean(value)?,
            "ignoreLinePrefixes" => self.ignore_line_prefixes = list(value)?,
            "ignoreTags" => self.ignore_tags = list(value)?,
            "defaultStyleId" => {
                self.default_style_id = match value {
                    Value::Null => None,
                    Value::String(s) if s.is_empty() => None,
                    v => Some(Id::from_value(v).ok_or("\"defaultStyleId\" must be a style id or null")?),
                }
            }
            "direction" => {
                self.direction = match value.as_str() {
                    Some("ltr") => Direction::Ltr,
                    Some("rtl") => Direction::Rtl,
                    _ => return Err("\"direction\" must be \"ltr\" or \"rtl\"".into()),
                }
            }
            "middleEast" => self.middle_east = boolean(value)?,
            "autoClosePSD" => self.auto_close_psd = boolean(value)?,
            "autoScrollStyle" => self.auto_scroll_style = boolean(value)?,
            "currentFolderTagPriority" => self.current_folder_tag_priority = boolean(value)?,
            "resizeTextBoxOnCenter" => self.resize_text_box_on_center = boolean(value)?,
            "multiBubbleMode" => self.multi_bubble_mode = boolean(value)?,
            "internalPadding" => self.internal_padding = number(value)?.clamp(0.0, 100.0),
            "interpretMarkdown" => self.interpret_markdown = boolean(value)?,
            "showTips" => self.show_tips = boolean(value)?,
            "showQuickStyleSize" => self.show_quick_style_size = boolean(value)?,
            "styleSizeStep" => {
                let step = number(value)?;
                if step <= 0.0 {
                    return Err("\"styleSizeStep\" must be above 0".into());
                }
                self.style_size_step = step.min(100.0);
            }
            "textSizeIncrement" => self.text_size_increment = number(value).map(|n| n.trunc().clamp(1.0, 99.0)).unwrap_or(1.0),
            "textScale" => {
                let scale = match value {
                    Value::Null => None,
                    Value::String(s) if s.trim().is_empty() => None,
                    v => crate::id::parse_float(Some(v)).map(f64::trunc).filter(|n| *n != 0.0),
                };
                self.text_scale = scale.map(|s| s.clamp(1.0, 999.0)).filter(|s| *s != 100.0);
            }
            "resetLineCounterOnPage" => self.reset_line_counter_on_page = boolean(value)?,
            "multiTabEnabled" => self.multi_tab_enabled = boolean(value)?,
            "inlineTextShapeR" => self.inline_text_shape_r = boolean(value)?,
            "dehyphenateTextShapeR" => self.dehyphenate_text_shape_r = boolean(value)?,
            "longestPrefixMatch" => self.longest_prefix_match = boolean(value)?,
            "openStoredSelections" => self.open_stored_selections = boolean(value)?,
            "extraPageMarkers" => self.extra_page_markers = boolean(value)?,
            "pageMarkerWords" => {
                let words = list(value)?;
                if words.len() > crate::pages::MAX_PAGE_WORDS {
                    return Err(format!("\"pageMarkerWords\" takes at most {} words", crate::pages::MAX_PAGE_WORDS));
                }
                if let Some(w) = words.iter().find(|w| w.chars().count() > crate::pages::MAX_PAGE_WORD_CHARS || w.chars().any(|c| c.is_ascii_digit())) {
                    return Err(format!("\"{w}\" can't be a page marker word: at most {} characters, no digits", crate::pages::MAX_PAGE_WORD_CHARS));
                }
                self.page_marker_words = words.iter().map(|w| crate::pages::js_trim(w).to_string()).filter(|w| !w.is_empty()).collect();
            }
            _ => return Err(format!("unknown TypeR setting \"{key}\"")),
        }
        Ok(())
    }

    /// Apply every known key of `values`; unknown keys are skipped (TypeR exports carry other
    /// app state). Returns the keys applied. Stops at the first invalid value.
    pub fn merge(&mut self, values: &serde_json::Map<String, Value>) -> Result<Vec<String>, String> {
        let known = self.to_json();
        let mut applied = Vec::new();
        for (k, v) in values {
            if known.get(k).is_some() {
                self.set(k, v)?;
                applied.push(k.clone());
            }
        }
        Ok(applied)
    }

    /// The words page markers start with: `Page`, plus the extra words when they are on.
    pub fn marker_words(&self) -> crate::pages::MarkerWords {
        if self.extra_page_markers {
            crate::pages::MarkerWords::new(self.page_marker_words.iter().map(String::as_str))
        } else {
            crate::pages::MarkerWords::typer()
        }
    }

    /// The text scale as a ratio (1.0 = 100 %).
    pub fn scale_ratio(&self) -> f64 {
        self.text_scale.map_or(1.0, |s| s / 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_are_typer_3() {
        let s = Settings::default();
        assert_eq!(s.ignore_line_prefixes, ["##"]);
        assert!(s.interpret_markdown && s.reset_line_counter_on_page && s.current_folder_tag_priority);
        assert_eq!(s.internal_padding, 10.0);
        assert_eq!(s.text_scale, None);
        let v = s.to_json();
        assert_eq!(v["autoClosePSD"], false);
        assert_eq!(v["ignoreLinePrefixes"], json!(["##"]));
        assert_eq!(serde_json::from_value::<Settings>(json!({})).unwrap(), s, "missing keys take defaults");
        assert!(s.extra_page_markers);
        assert_eq!(v["pageMarkerWords"], json!(["Página", "Pág."]));
        assert_eq!(crate::pages::parse_page_marker_with("Pág. 4", &s.marker_words()), Some(4));
        let off = Settings { extra_page_markers: false, ..Settings::default() };
        assert_eq!(crate::pages::parse_page_marker_with("Pág. 4", &off.marker_words()), None);
    }

    #[test]
    fn coercions_and_errors() {
        let mut s = Settings::default();
        s.set("ignoreLinePrefixes", &json!("##; //\nNOTE:")).unwrap();
        assert_eq!(s.ignore_line_prefixes, ["##", "//", "NOTE:"]);
        s.set("textScale", &json!(5000)).unwrap();
        assert_eq!(s.text_scale, Some(999.0));
        s.set("textScale", &json!("50")).unwrap();
        assert_eq!(s.scale_ratio(), 0.5);
        s.set("textScale", &json!(100)).unwrap();
        assert_eq!(s.text_scale, None);
        s.set("textScale", &json!(0)).unwrap();
        assert_eq!(s.text_scale, None);
        s.set("textSizeIncrement", &json!(500)).unwrap();
        assert_eq!(s.text_size_increment, 99.0);
        s.set("internalPadding", &json!(-4)).unwrap();
        assert_eq!(s.internal_padding, 0.0);
        assert!(s.set("pastePointText", &json!("yes")).is_err());
        assert!(s.set("nope", &json!(1)).is_err());
        assert!(s.set("ignoreTags", &json!([1])).is_err());
        assert!(s.set("styleSizeStep", &json!(0)).is_err());
        assert!(s.set("defaultStyleId", &json!({})).is_err());
        s.set("defaultStyleId", &json!("abc")).unwrap();
        assert_eq!(s.default_style_id, Some(Id::str("abc")));
        s.set("pageMarkerWords", &json!("Página; Folha ;")).unwrap();
        assert_eq!(s.page_marker_words, ["Página", "Folha"]);
        assert!(s.set("pageMarkerWords", &json!(["P1"])).is_err());
        assert!(s.set("pageMarkerWords", &json!(vec!["x"; 30])).is_err());
        assert!(s.set("extraPageMarkers", &json!(1)).is_err());
        let applied = s.merge(json!({"ignoreTags": ["**"], "version": "3.0", "language": "es"}).as_object().unwrap()).unwrap();
        assert_eq!(applied, ["ignoreTags"]);
    }
}
