// Ported from TypeR app_src/styleSizePresets.js (develop 71bc061), MIT, (c) 2017 Swirt.
// See crates/typer/LICENSE-TypeR.txt.

//! Size presets per style and auto size by page width (L5).
//!
//! A style keeps up to [`MAX_PRESETS`] sizes; the active one is the style's text size and always
//! comes first. With `autoSizeByPageWidth`, the preset with the highest minimum width that the
//! page reaches wins, else the default preset.

use serde_json::Value;

use crate::id::{num, parse_float};
use crate::library::Style;

pub const MAX_PRESETS: usize = 3;

/// A positive, finite size (`parseFloat`), else `None`.
pub fn parse_size(v: Option<&Value>) -> Option<f64> {
    parse_float(v).filter(|s| *s > 0.0)
}

/// A positive page width in whole pixels, else `None`.
pub fn parse_page_width(v: f64) -> Option<f64> {
    (v.is_finite() && v > 0.0).then(|| v.round())
}

/// The style's presets: stored ones (positive, unique, at most three) with the active size
/// moved in front; just the active size when none are stored.
pub fn normalize_presets(style: &Style) -> Vec<f64> {
    let active = style.size();
    let mut presets: Vec<f64> = Vec::new();
    if let Some(Value::Array(a)) = style.props.get("sizePresets") {
        for v in a {
            let Some(size) = parse_size(Some(v)) else { continue };
            if presets.contains(&size) || presets.len() >= MAX_PRESETS {
                continue;
            }
            presets.push(size);
        }
    }
    if let Some(a) = active
        && !presets.contains(&a)
    {
        if presets.len() >= MAX_PRESETS {
            presets.pop();
        }
        presets.insert(0, a);
    }
    presets
}

/// Auto-size settings of a style, normalized.
#[derive(Clone, Debug, PartialEq)]
pub struct WidthConfig {
    /// On only with two or more presets.
    pub auto_size_by_page_width: bool,
    pub default_index: usize,
    /// Minimum page width per preset; `None` for the default preset and unset ones.
    pub min_widths: Vec<Option<f64>>,
}

pub fn width_config(style: &Style) -> WidthConfig {
    let presets = normalize_presets(style);
    let default_index =
        parse_float(style.props.get("sizePresetDefaultIndex")).map(f64::trunc).filter(|i| *i >= 0.0 && (*i as usize) < presets.len()).map_or(0, |i| i as usize);
    let source = match style.props.get("sizePresetMinWidths") {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    let min_widths = (0..presets.len()).map(|i| if i == default_index { None } else { parse_float(source.get(i)).and_then(parse_page_width) }).collect();
    WidthConfig {
        auto_size_by_page_width: style.props.get("autoSizeByPageWidth").and_then(Value::as_bool) == Some(true) && presets.len() > 1,
        default_index,
        min_widths,
    }
}

/// The size to use on a page `page_width` pixels wide: the auto-size preset when enabled, else
/// the style's size (`None` = keep the layer's size).
pub fn size_for_page_width(style: &Style, page_width: Option<f64>) -> Option<f64> {
    let presets = normalize_presets(style);
    let width = page_width.and_then(parse_page_width);
    let enabled = style.props.get("autoSizeByPageWidth").and_then(Value::as_bool) == Some(true);
    let (Some(width), true, true) = (width, enabled, presets.len() >= 2) else { return style.size() };
    let config = width_config(style);
    let mut selected = presets.get(config.default_index).or(presets.first()).copied();
    let mut threshold = -1.0;
    for (i, t) in config.min_widths.iter().enumerate() {
        let Some(t) = *t else { continue };
        if i == config.default_index {
            continue;
        }
        if width >= t && t > threshold {
            threshold = t;
            selected = presets.get(i).copied();
        }
    }
    selected
}

/// Write `size` as the style's text size (and implied size) with `presets`; `edited` becomes
/// `now` when given. No-op without a text style.
fn write_size(style: &mut Style, size: f64, presets: &[f64], now: Option<u64>) -> bool {
    let Some(ts) = style.text_style_mut() else { return false };
    ts.insert("size".into(), num(size));
    if ts.get("impliedFontSize").is_some_and(|v| !v.is_null()) {
        ts.insert("impliedFontSize".into(), num(size));
    }
    style.props.insert("sizePresets".into(), Value::Array(presets.iter().map(|p| num(*p)).collect()));
    if let Some(now) = now {
        style.edited = Some(now.into());
    }
    true
}

/// Make the preset `size` the active size. Returns whether anything changed (sizes that are not
/// presets change nothing).
pub fn set_preset(style: &mut Style, size: f64, now: Option<u64>) -> bool {
    let Some(size) = parse_size(Some(&num(size))) else { return false };
    let presets = normalize_presets(style);
    if !presets.contains(&size) {
        return false;
    }
    let stored: Vec<Option<f64>> = match style.props.get("sizePresets") {
        Some(Value::Array(a)) => a.iter().map(|v| parse_size(Some(v))).collect(),
        _ => Vec::new(),
    };
    if style.size() == Some(size) && stored.len() == presets.len() && stored.iter().zip(&presets).all(|(s, p)| *s == Some(*p)) {
        return false;
    }
    write_size(style, size, &presets, now)
}

/// Change the active preset's size (quick size edit). Refuses a size another preset already has.
pub fn update_active_preset(style: &mut Style, size: f64, now: Option<u64>) -> bool {
    let Some(size) = parse_size(Some(&num(size))) else { return false };
    let mut presets = normalize_presets(style);
    if presets.is_empty() {
        return false;
    }
    let active = style.size();
    let active_index = presets.iter().position(|p| Some(*p) == active).unwrap_or(0);
    if presets.iter().enumerate().any(|(i, p)| i != active_index && *p == size) || presets.get(active_index) == Some(&size) {
        return false;
    }
    if let Some(slot) = presets.get_mut(active_index) {
        *slot = size;
    }
    write_size(style, size, &presets, now)
}

/// Switch to the next preset, wrapping around. Needs two or more presets.
pub fn cycle_preset(style: &mut Style, now: Option<u64>) -> bool {
    let presets = normalize_presets(style);
    if presets.len() < 2 {
        return false;
    }
    let next = match presets.iter().position(|p| Some(*p) == style.size()) {
        None => 0,
        Some(i) => (i + 1) % presets.len(),
    };
    presets.get(next).is_some_and(|s| set_preset(style, *s, now))
}

/// Store the normalized presets and auto-size settings on the style (what TypeR's `saveStyle`
/// and `setStyles` do).
pub fn normalize_style_presets(style: &mut Style) {
    let presets = normalize_presets(style);
    style.props.insert("sizePresets".into(), Value::Array(presets.iter().map(|p| num(*p)).collect()));
    let c = width_config(style);
    style.props.insert("autoSizeByPageWidth".into(), Value::Bool(c.auto_size_by_page_width));
    style.props.insert("sizePresetDefaultIndex".into(), num(c.default_index as f64));
    style.props.insert("sizePresetMinWidths".into(), Value::Array(c.min_widths.iter().map(|w| w.map_or(Value::Null, num)).collect()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::Id;
    use serde_json::json;

    fn make(size: f64, presets: Option<Value>) -> Style {
        let mut v =
            json!({"id": "title", "name": "Title", "textProps": {"layerText": {"textStyleRange": [{"textStyle": {"size": size, "impliedFontSize": size}}]}}});
        if let Some(p) = presets {
            v["sizePresets"] = p;
        }
        Style::from_json(&v).unwrap()
    }

    fn implied(s: &Style) -> f64 {
        s.text_style().unwrap()["impliedFontSize"].as_f64().unwrap()
    }

    // scripts/testStyleSizePresets.js
    #[test]
    fn presets_normalize_like_typer() {
        assert_eq!(MAX_PRESETS, 3);
        assert_eq!(normalize_presets(&make(23.0, None)), [23.0]);
        assert_eq!(normalize_presets(&make(35.0, Some(json!([23, 23, 0, "bad"])))), [35.0, 23.0]);
        assert_eq!(normalize_presets(&make(23.0, Some(json!([23, 35, 48, 60])))), [23.0, 35.0, 48.0]);
    }

    #[test]
    fn set_cycle_and_update_presets() {
        let original = make(23.0, Some(json!([23, 35, 48])));
        let mut selected = original.clone();
        assert!(set_preset(&mut selected, 35.0, None));
        assert_eq!(original.size(), Some(23.0));
        assert_eq!(selected.size(), Some(35.0));
        assert_eq!(implied(&selected), 35.0);
        assert_eq!(selected.props["sizePresets"], json!([23, 35, 48]));

        let mut cycled = selected.clone();
        assert!(cycle_preset(&mut cycled, None));
        assert_eq!(cycled.size(), Some(48.0));
        assert!(cycle_preset(&mut cycled, None));
        assert_eq!(cycled.size(), Some(23.0));

        let mut unchanged = original.clone();
        assert!(!set_preset(&mut unchanged, 99.0, None));
        assert_eq!(unchanged, original);

        let mut resized = original.clone();
        assert!(update_active_preset(&mut resized, 26.0, Some(5)));
        assert_eq!(resized.props["sizePresets"], json!([26, 35, 48]));
        assert_eq!(resized.size(), Some(26.0));
        assert_eq!(implied(&resized), 26.0);
        assert_eq!(resized.edited, Some(5.into()));
        let mut dup = original.clone();
        assert!(!update_active_preset(&mut dup, 35.0, None), "quick editing must not create duplicate presets");
        assert_eq!(dup, original);
    }

    #[test]
    fn auto_size_by_page_width() {
        let mut automatic = make(23.0, Some(json!([23, 35, 48])));
        automatic.props.insert("autoSizeByPageWidth".into(), json!(true));
        automatic.props.insert("sizePresetDefaultIndex".into(), json!(0));
        automatic.props.insert("sizePresetMinWidths".into(), json!([null, 1000, 1500]));
        assert_eq!(
            width_config(&automatic),
            WidthConfig { auto_size_by_page_width: true, default_index: 0, min_widths: vec![None, Some(1000.0), Some(1500.0)] }
        );
        assert_eq!(size_for_page_width(&automatic, Some(999.0)), Some(23.0));
        assert_eq!(size_for_page_width(&automatic, Some(1000.0)), Some(35.0));
        assert_eq!(size_for_page_width(&automatic, Some(1499.0)), Some(35.0));
        assert_eq!(size_for_page_width(&automatic, Some(1500.0)), Some(48.0));
        assert_eq!(size_for_page_width(&automatic, None), Some(23.0));
        let mut off = automatic.clone();
        off.props.insert("autoSizeByPageWidth".into(), json!(false));
        assert_eq!(size_for_page_width(&off, Some(2000.0)), Some(23.0));
    }

    #[test]
    fn styles_without_text_props_are_left_alone() {
        let mut s = Style::from_json(&json!({"id": Id::str("x").to_value(), "name": "x", "sizePresets": [10, 20]})).unwrap();
        assert_eq!(normalize_presets(&s), [10.0, 20.0]);
        assert!(!set_preset(&mut s, 20.0, None), "no text style to write");
        assert_eq!(s.size(), None);
        normalize_style_presets(&mut s);
        assert_eq!(s.props["autoSizeByPageWidth"], json!(false));
        assert_eq!(s.props["sizePresetMinWidths"], json!([null, null]));
    }
}
