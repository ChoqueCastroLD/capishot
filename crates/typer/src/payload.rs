// Ported from TypeR app_src/textLayerPayload.js (develop 71bc061), MIT, (c) 2017 Swirt.
// See crates/typer/LICENSE-TypeR.txt.

//! Which line and style each target gets when several are filled at once: stored multi-bubble
//! selections (M2) or selected text layers (P9), plus the text scale (S14) and the per-style
//! layer kind (L6). WP1's placement commands consume these.

use serde_json::Value;

use crate::id::{Id, num};
use crate::library::{Library, Style, TextType};
use crate::script::Line;

/// `style` with its size, numeric non-zero leading and size presets multiplied by
/// `text_scale` % (`None` = unchanged).
pub fn scaled_style(style: &Style, text_scale: Option<f64>) -> Style {
    let Some(scale) = text_scale.filter(|s| s.is_finite() && *s != 0.0) else { return style.clone() };
    let ratio = scale / 100.0;
    let mut s = style.clone();
    if let Some(ts) = s.text_style_mut() {
        if let Some(size) = ts.get("size").and_then(Value::as_f64) {
            ts.insert("size".into(), num(size * ratio));
        }
        if let Some(leading) = ts.get("leading").and_then(Value::as_f64).filter(|l| *l != 0.0) {
            ts.insert("leading".into(), num(leading * ratio));
        }
    }
    if let Some(Value::Array(presets)) = s.props.get_mut("sizePresets") {
        for p in presets.iter_mut() {
            if let Some(v) = p.as_f64() {
                *p = num(v * ratio);
            }
        }
    }
    s
}

/// Point text for this style? Its own `textType` wins over the global setting.
pub fn resolve_point_text(style: Option<&Style>, fallback_point_text: bool) -> bool {
    match style.map(Style::text_type) {
        Some(TextType::Point) => true,
        Some(TextType::Paragraph) => false,
        _ => fallback_point_text,
    }
}

/// One target of a batch: the line's text and the (scaled) style.
#[derive(Clone, Debug, PartialEq)]
pub struct Item<T> {
    pub target: T,
    pub line_index: usize,
    pub text: String,
    /// `None` when there is no style at all (empty library).
    pub style: Option<Style>,
}

/// Result of [`selected_layers`].
#[derive(Clone, Debug, PartialEq)]
pub struct LayerBatch<T> {
    pub items: Vec<Item<T>>,
    /// Where the cursor goes afterwards: the next usable line, else the last line used, else the
    /// current line.
    pub next_line_index: usize,
}

fn next_usable(lines: &[Line], from: usize) -> Option<usize> {
    crate::script::next_usable(lines, from)
}

/// Assign lines to selected text layers in click order (P9): each layer gets the next usable
/// line; its style is the line's used style, else its tag style, else the style the previous
/// layer got (starting from the current style).
pub fn selected_layers<T: Clone>(
    layers: &[T],
    lines: &[Line],
    current_line: usize,
    library: &Library,
    current_style: Option<&Id>,
    text_scale: Option<f64>,
) -> LayerBatch<T> {
    let mut items = Vec::new();
    let mut line_index = current_line;
    let mut active: Option<Id> = current_style.cloned();
    for layer in layers {
        let Some(usable) = next_usable(lines, line_index) else { break };
        let Some(line) = lines.get(usable) else { break };
        let target_style = line.used_style.clone().or_else(|| line.style.clone()).or_else(|| active.clone());
        if target_style.is_some() {
            active = target_style.clone();
        }
        let style = target_style.as_ref().and_then(|id| library.style(id)).map(|s| scaled_style(s, text_scale));
        items.push(Item { target: layer.clone(), line_index: usable, text: line.text.clone(), style });
        line_index = usable + 1;
    }
    let next = next_usable(lines, line_index).or_else(|| items.last().map(|i| i.line_index)).unwrap_or(current_line);
    LayerBatch { items, next_line_index: next }
}

/// A stored multi-bubble selection: the line and style captured with it (both optional).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stored {
    pub line_index: Option<usize>,
    pub style_id: Option<Id>,
}

/// Assign lines and styles to stored selections (M2). A selection keeps its captured line while
/// that line is usable, else takes the next unused usable line. Style precedence: the captured
/// style, then the line's used style, then its tag style, then the current style. Stops when the
/// lines run out. Returns `(text, style)` per selection, in order.
pub fn stored_selections(
    selections: &[Stored],
    lines: &[Line],
    current_line: usize,
    library: &Library,
    current_style: Option<&Id>,
    text_scale: Option<f64>,
) -> Vec<(String, Option<Style>)> {
    let mut out = Vec::new();
    let mut fallback = current_line;
    for sel in selections {
        let stored = sel.line_index.and_then(|i| lines.get(i).filter(|l| !l.ignore).map(|l| (i, l)));
        let line = match stored {
            Some((i, l)) => {
                fallback = fallback.max(i + 1);
                Some(l)
            }
            None => {
                let mut found = None;
                while fallback < lines.len() {
                    let candidate = lines.get(fallback);
                    fallback += 1;
                    if let Some(c) = candidate.filter(|c| !c.ignore) {
                        found = Some(c);
                        break;
                    }
                }
                found
            }
        };
        let Some(line) = line else { break };
        let style = sel
            .style_id
            .as_ref()
            .and_then(|id| library.style(id))
            .or_else(|| line.used_style.as_ref().and_then(|id| library.style(id)))
            .or_else(|| line.style.as_ref().and_then(|id| library.style(id)))
            .or_else(|| current_style.and_then(|id| library.style(id)));
        out.push((line.text.clone(), style.map(|s| scaled_style(s, text_scale))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make(id: &str, size: f64, leading: Value, text_type: &str) -> Style {
        Style::from_json(&json!({"id": id, "name": id, "textType": text_type,
            "textProps": {"layerText": {"textStyleRange": [{"textStyle": {"size": size, "leading": leading}}]}}}))
        .unwrap()
    }

    fn size(s: &Style) -> f64 {
        s.text_style().unwrap()["size"].as_f64().unwrap()
    }

    fn line(text: &str, ignore: bool, style: Option<&str>) -> Line {
        Line {
            raw_text: text.into(),
            raw_index: 0,
            ignore_prefix: String::new(),
            style_prefix: String::new(),
            style: style.map(Id::str),
            used_style: None,
            ignore,
            index: 0,
            text: text.into(),
            page: None,
            last: false,
        }
    }

    // scripts/testTextLayerPayload.js
    #[test]
    fn scaling_and_point_text_resolution() {
        let mut base = make("base", 20.0, json!(24), "inherit");
        base.props.insert("sizePresets".into(), json!([20, 30, 40]));
        assert_eq!(scaled_style(&base, None), base);
        let scaled = scaled_style(&base, Some(50.0));
        assert_eq!(size(&scaled), 10.0);
        assert_eq!(scaled.text_style().unwrap()["leading"], json!(12));
        assert_eq!(scaled.props["sizePresets"], json!([10, 15, 20]));
        assert_eq!(base.props["sizePresets"], json!([20, 30, 40]));
        assert_eq!(size(&base), 20.0);

        assert!(resolve_point_text(Some(&make("p", 20.0, json!(24), "point")), false));
        assert!(!resolve_point_text(Some(&make("q", 20.0, json!(24), "paragraph")), true));
        assert!(resolve_point_text(Some(&base), true));
        assert!(!resolve_point_text(None, false));

        let partial = Style::from_json(
            &json!({"id": "partial", "name": "p", "textProps": {"layerText": {"textStyleRange": [{"textStyle": {"size": 12, "leading": "auto"}}]}}}),
        )
        .unwrap();
        let partial = scaled_style(&partial, Some(150.0));
        assert_eq!(size(&partial), 18.0);
        assert_eq!(partial.text_style().unwrap()["leading"], "auto");
    }

    fn library() -> Library {
        Library {
            folders: vec![],
            styles: vec![
                make("current", 30.0, json!(36), "inherit"),
                make("tagged", 18.0, json!(22), "inherit"),
                make("stored", 16.0, json!(20), "inherit"),
                make("manual", 14.0, json!(18), "inherit"),
                make("prefixed", 22.0, json!(26), "inherit"),
            ],
        }
    }

    fn lines() -> Vec<Line> {
        vec![line("ignored", true, None), line("first", false, Some("tagged")), line("second", false, None), line("third", false, None)]
    }

    #[test]
    fn stored_selection_precedence() {
        let lib = library();
        let cur = Id::str("current");
        let sel = [Stored { line_index: Some(1), style_id: None }, Stored { line_index: None, style_id: Some(Id::str("stored")) }, Stored::default()];
        let out = stored_selections(&sel, &lines(), 2, &lib, Some(&cur), Some(200.0));
        let texts: Vec<&str> = out.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, ["first", "second", "third"]);
        let ids: Vec<String> = out.iter().map(|(_, s)| s.as_ref().unwrap().id.to_string()).collect();
        assert_eq!(ids, ["tagged", "stored", "current"]);
        assert_eq!(size(out[0].1.as_ref().unwrap()), 36.0);
        assert_eq!(size(lib.style(&Id::str("stored")).unwrap()), 16.0);

        let manual = stored_selections(
            &[Stored { line_index: Some(0), style_id: Some(Id::str("manual")) }],
            &[line("prefixed line", false, Some("prefixed"))],
            0,
            &lib,
            Some(&cur),
            None,
        );
        assert_eq!(manual[0].1.as_ref().unwrap().id, Id::str("manual"));

        let sparse = stored_selections(
            &[
                Stored { line_index: Some(0), style_id: None },
                Stored { line_index: Some(99), style_id: None },
                Stored { line_index: Some(3), style_id: None },
                Stored::default(),
            ],
            &lines(),
            0,
            &lib,
            Some(&cur),
            None,
        );
        let texts: Vec<&str> = sparse.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, ["first", "second", "third"]);
        let ids: Vec<String> = sparse.iter().map(|(_, s)| s.as_ref().unwrap().id.to_string()).collect();
        assert_eq!(ids, ["tagged", "current", "current"]);

        let exhausted =
            stored_selections(&[Stored::default(), Stored::default(), Stored::default()], &[line("comment", true, None)], 0, &lib, Some(&cur), None);
        assert!(exhausted.is_empty());
    }

    #[test]
    fn selected_layer_precedence() {
        let lib = library();
        let cur = Id::str("current");
        let batch = selected_layers(&[101, 102, 103], &lines(), 0, &lib, Some(&cur), Some(50.0));
        let got: Vec<(i32, &str, String)> = batch.items.iter().map(|i| (i.target, i.text.as_str(), i.style.as_ref().unwrap().id.to_string())).collect();
        assert_eq!(got, [(101, "first", "tagged".into()), (102, "second", "tagged".into()), (103, "third", "tagged".into())]);
        let idx: Vec<usize> = batch.items.iter().map(|i| i.line_index).collect();
        assert_eq!(idx, [1, 2, 3]);
        assert_eq!(batch.next_line_index, 3);
        assert_eq!(size(batch.items[0].style.as_ref().unwrap()), 9.0);

        let partial_lines = vec![line("one", false, Some("tagged")), line("note", true, None), line("two", false, None)];
        let partial = selected_layers(&[201, 202, 203], &partial_lines, 1, &lib, Some(&cur), None);
        assert_eq!(partial.items.iter().map(|i| i.target).collect::<Vec<_>>(), [201]);
        assert_eq!(partial.items[0].line_index, 2);
        assert_eq!(partial.items[0].style.as_ref().unwrap().id, Id::str("current"));
        assert_eq!(partial.next_line_index, 2);
    }
}
