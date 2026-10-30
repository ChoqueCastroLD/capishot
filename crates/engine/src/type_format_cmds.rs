//! Keyboard formatting of type, the commands behind Photoshop's Type-tool shortcuts.
//!
//! - `type.nudge` steps leading, baseline shift and tracking *relative to each character's own
//!   value* (Alt+↑/↓, Alt+Shift+↑/↓, and Alt+←/→ over a selection). Auto leading starts from the
//!   value it shows (size × the paragraph's auto-leading factor) and becomes a fixed leading, as in
//!   Photoshop.
//! - `type.toggleStyle` turns underline, strikethrough, faux bold, faux italic, all caps, small caps
//!   or paragraph hyphenation on or off (⇧⌘U, ⇧⌘/, ⇧⌘B, ⇧⌘I, ⇧⌘K, ⇧⌘H, ⌥⇧⌘H). Off only when the
//!   whole target already has it, so a mixed selection turns it on first.
//!
//! Both follow `type.setStyle`'s scope: `layer` + `range` (characters), else the selected Type
//! layers, one history step for the batch.

use photocraft_doc::text::{Caps, CharStyle};
use photocraft_doc::{LayerContent, TextLayer};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::type_cmds::{TRACKING_MAX, TRACKING_MIN, format_targets, range_param, style_paragraphs, style_range, with_text_targets};
use crate::{EngineError, Result, Session};

/// Photoshop's leading range (pt).
pub const LEADING_MIN_PT: f32 = 0.01;
pub const LEADING_MAX_PT: f32 = 5000.0;
/// Baseline shift range (pt), the Character panel's.
pub const BASELINE_SHIFT_MAX_PT: f32 = 1296.0;

/// Styles `type.toggleStyle` knows.
pub const TOGGLE_STYLES: [&str; 7] = ["underline", "strikethrough", "fauxBold", "fauxItalic", "allCaps", "smallCaps", "hyphenate"];

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

/// An optional finite delta no larger than `max` in magnitude.
fn delta(p: &Value, key: &str, max: f64) -> Result<Option<f32>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|d| d.is_finite() && d.abs() <= max)
            .map(|d| Some(d as f32))
            .ok_or_else(|| bad("type.nudge", format!("`{key}` must be a number between -{max} and {max}"))),
    }
}

/// Uniform scale of the layer transform (shown sizes are model sizes times this).
fn shown_scale(t: &TextLayer) -> f32 {
    let m = t.transform.m;
    let k = (m[0] * m[3] - m[1] * m[2]).abs().sqrt() as f32;
    if k.is_finite() && k > 1e-3 { k } else { 1.0 }
}

/// The character range a target layer is formatted in (as `with_text_targets` passes it).
fn target_range(t: &TextLayer, p: &Value) -> (usize, usize) {
    if p.get("layers").is_none() || p.get("layer").is_some() { range_param(&t.text, p) } else { (0, t.text.len()) }
}

/// Auto-leading factor of the paragraph holding byte `at`.
fn auto_leading_at(t: &TextLayer, at: usize) -> f32 {
    let mut end = 0;
    let paras = t.paragraph_runs();
    let f = paras
        .iter()
        .find(|r| {
            end += r.len;
            at < end
        })
        .or(paras.last())
        .map_or(1.2, |r| r.style.auto_leading);
    if f.is_finite() && f > 0.0 { f } else { 1.2 }
}

fn nudge(s: &mut Session, p: &Value) -> Result<Value> {
    let leading = delta(p, "leading", f64::from(LEADING_MAX_PT))?;
    let shift = delta(p, "baselineShift", f64::from(BASELINE_SHIFT_MAX_PT) * 2.0)?;
    let tracking = delta(p, "tracking", TRACKING_MAX - TRACKING_MIN)?;
    if leading.is_none() && shift.is_none() && tracking.is_none() {
        return Err(bad("type.nudge", "give `leading`, `baselineShift` or `tracking` (a step to add)"));
    }
    let shown = match p.get("metricsAsShown") {
        None => false,
        Some(v) => v.as_bool().ok_or_else(|| bad("type.nudge", "`metricsAsShown` must be a boolean"))?,
    };
    let label = match (leading, shift, tracking) {
        (Some(_), None, None) => "Leading",
        (None, Some(_), None) => "Baseline Shift",
        (None, None, Some(_)) => "Tracking",
        _ => "Set Type Style",
    };
    let ids = with_text_targets(s, p, label, |t, (a, b)| {
        let k = if shown { shown_scale(t) } else { 1.0 };
        let auto = auto_leading_at(t, a);
        style_range(t, a, b, &|st| {
            if let Some(d) = leading {
                let now = st.leading_pt.filter(|v| v.is_finite()).unwrap_or(st.size_pt * auto);
                st.leading_pt = Some((now + d / k).clamp(LEADING_MIN_PT, LEADING_MAX_PT));
            }
            if let Some(d) = shift {
                let now = if st.baseline_shift_pt.is_finite() { st.baseline_shift_pt } else { 0.0 };
                st.baseline_shift_pt = (now + d / k).clamp(-BASELINE_SHIFT_MAX_PT, BASELINE_SHIFT_MAX_PT);
            }
            if let Some(d) = tracking {
                let now = if st.tracking.is_finite() { st.tracking } else { 0.0 };
                st.tracking = (now + d).clamp(TRACKING_MIN as f32, TRACKING_MAX as f32);
            }
        });
        Ok(())
    })?;
    Ok(json!({ "layers": ids.iter().map(|id| id.0).collect::<Vec<_>>() }))
}

fn has_style(st: &CharStyle, style: &str) -> bool {
    match style {
        "underline" => st.underline,
        "strikethrough" => st.strikethrough,
        "fauxBold" => st.faux_bold,
        "fauxItalic" => st.faux_italic,
        "allCaps" => st.caps == Caps::AllCaps,
        "smallCaps" => st.caps == Caps::SmallCaps,
        _ => false,
    }
}

fn set_style(st: &mut CharStyle, style: &str, on: bool) {
    match style {
        "underline" => st.underline = on,
        "strikethrough" => st.strikethrough = on,
        "fauxBold" => st.faux_bold = on,
        "fauxItalic" => st.faux_italic = on,
        "allCaps" | "smallCaps" => {
            let caps = if style == "allCaps" { Caps::AllCaps } else { Caps::SmallCaps };
            if on {
                st.caps = caps;
            } else if st.caps == caps {
                st.caps = Caps::Normal;
            }
        }
        _ => {}
    }
}

/// Whether every character (or, for `hyphenate`, every paragraph) of the target has `style`.
fn all_have(t: &TextLayer, (a, b): (usize, usize), style: &str) -> bool {
    let len = t.text.len();
    let touches = |rs: usize, re: usize| (rs < b && re > a) || (a == b && a >= rs && (a < re || re == len));
    let mut at = 0;
    if style == "hyphenate" {
        return t.paragraph_runs().iter().all(|r| {
            let rs = at;
            at += r.len;
            !touches(rs, at) || r.style.hyphenate
        });
    }
    t.char_runs().iter().all(|r| {
        let rs = at;
        at += r.len;
        !(touches(rs, at) || len == 0) || has_style(&r.style, style)
    })
}

fn toggle(s: &mut Session, p: &Value) -> Result<Value> {
    let style = p
        .get("style")
        .and_then(Value::as_str)
        .filter(|v| TOGGLE_STYLES.contains(v))
        .ok_or_else(|| bad("type.toggleStyle", format!("`style` must be one of {}", TOGGLE_STYLES.join(", "))))?
        .to_string();
    let on = match p.get("on") {
        None | Some(Value::Null) => {
            let st = s.active().ok_or(EngineError::NoDocument)?;
            let mut all = true;
            for id in format_targets(s, p)? {
                if let Some(LayerContent::Text(t)) = st.doc.layer(id).map(|l| &l.content) {
                    all &= all_have(t, target_range(t, p), &style);
                }
            }
            !all
        }
        Some(v) => v.as_bool().ok_or_else(|| bad("type.toggleStyle", "`on` must be a boolean"))?,
    };
    let ids = with_text_targets(s, p, "Set Type Style", |t, (a, b)| {
        if style == "hyphenate" {
            style_paragraphs(t, a, b, &|ps| ps.hyphenate = on);
        } else {
            style_range(t, a, b, &|st| set_style(st, &style, on));
        }
        Ok(())
    })?;
    Ok(json!({ "style": style, "on": on, "layers": ids.iter().map(|id| id.0).collect::<Vec<_>>() }))
}

const SCOPE: &str = r#""layer":id? or "layers":[id,…]? (default the selected Type layers),"range":[startChar,endChar]? (single layer; default all)"#;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "type.nudge",
            label: "Nudge Type Formatting",
            menu: &[],
            shortcut: None,
            params: Box::leak(
                format!(
                    r#"{{{SCOPE},"leading":Δpt? (auto leading starts from the leading it shows; Type tool Alt+↑ +2, Alt+↓ -2, with Ctrl ±10),"baselineShift":Δpt? (Alt+Shift+↑/↓),"tracking":Δ(1/1000 em)? (Alt+←/→ over a selection: ±20, with Ctrl ±100),"metricsAsShown":bool=false (pt steps as the layer appears, after its transform)}}"#
                )
                .into_boxed_str(),
            ),
            enabled: crate::type_cmds::has_format_target,
            journal: true,
            run: nudge,
        },
        CommandSpec {
            id: "type.toggleStyle",
            label: "Toggle Type Style",
            menu: &[],
            shortcut: None,
            params: Box::leak(
                format!(
                    r#"{{"style":"underline|strikethrough|fauxBold|fauxItalic|allCaps|smallCaps|hyphenate","on":bool? (default: off only when the whole target has it),{SCOPE}}} → {{"style","on","layers"}}"#
                )
                .into_boxed_str(),
            ),
            enabled: crate::type_cmds::has_format_target,
            journal: true,
            run: toggle,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::LayerId;

    fn session() -> (Session, u64) {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 300, "height": 120, "background": "transparent"})).unwrap();
        let id = s.execute("type.create", json!({"x": 10, "y": 60, "text": "Hello world", "size": 20})).unwrap()["layer"].as_u64().unwrap();
        (s, id)
    }

    fn text(s: &Session, id: u64) -> TextLayer {
        match &s.active().unwrap().doc.layer(LayerId(id)).unwrap().content {
            LayerContent::Text(t) => t.clone(),
            other => panic!("{}", other.kind_name()),
        }
    }

    /// Style of the character at char index `i`.
    fn at(t: &TextLayer, i: usize) -> CharStyle {
        let b = t.text.char_indices().nth(i).unwrap().0;
        let mut end = 0;
        t.char_runs()
            .into_iter()
            .find(|r| {
                end += r.len;
                b < end
            })
            .unwrap()
            .style
    }

    #[test]
    fn nudge_leading_starts_from_auto_and_steps_each_run() {
        let (mut s, id) = session();
        s.execute("type.setStyle", json!({"layer": id, "range": [6, 11], "leading": 40})).unwrap();
        s.execute("type.nudge", json!({"layer": id, "leading": 2})).unwrap();
        let t = text(&s, id);
        // Auto leading of 20 pt type is 24 pt (120%): +2 makes it a fixed 26.
        assert!((at(&t, 0).leading_pt.unwrap() - 26.0).abs() < 1e-4);
        assert!((at(&t, 7).leading_pt.unwrap() - 42.0).abs() < 1e-4);
        s.execute("type.nudge", json!({"layer": id, "range": [0, 5], "leading": -10})).unwrap();
        let t = text(&s, id);
        assert!((at(&t, 0).leading_pt.unwrap() - 16.0).abs() < 1e-4);
        assert!((at(&t, 7).leading_pt.unwrap() - 42.0).abs() < 1e-4, "outside the range unchanged");
        // Clamped to Photoshop's range.
        s.execute("type.nudge", json!({"layer": id, "leading": -5000})).unwrap();
        assert_eq!(at(&text(&s, id), 0).leading_pt, Some(LEADING_MIN_PT));
    }

    #[test]
    fn nudge_baseline_and_tracking_are_relative_and_one_step() {
        let (mut s, id) = session();
        let before = s.active().unwrap().history.past_len();
        s.execute("type.nudge", json!({"layer": id, "range": [0, 5], "baselineShift": 2, "tracking": 20})).unwrap();
        s.execute("type.nudge", json!({"layer": id, "range": [0, 5], "baselineShift": 2, "tracking": -100})).unwrap();
        let t = text(&s, id);
        assert_eq!((at(&t, 0).baseline_shift_pt, at(&t, 0).tracking), (4.0, -80.0));
        assert_eq!((at(&t, 6).baseline_shift_pt, at(&t, 6).tracking), (0.0, 0.0));
        assert_eq!(s.active().unwrap().history.past_len(), before + 2);
        // A scaled layer steps as it appears: 2 pt shown on a 200% layer is 1 pt.
        s.execute("type.edit", json!({"layer": id, "transform": [2, 0, 0, 2, 10, 60]})).unwrap();
        s.execute("type.nudge", json!({"layer": id, "range": [0, 1], "baselineShift": 2, "metricsAsShown": true})).unwrap();
        assert_eq!(at(&text(&s, id), 0).baseline_shift_pt, 5.0);
    }

    #[test]
    fn toggle_turns_on_for_mixed_targets_then_off() {
        let (mut s, id) = session();
        s.execute("type.setStyle", json!({"layer": id, "range": [0, 5], "underline": true})).unwrap();
        let r = s.execute("type.toggleStyle", json!({"layer": id, "style": "underline"})).unwrap();
        assert_eq!(r["on"], json!(true), "mixed: on");
        assert!((0..11).all(|i| at(&text(&s, id), i).underline));
        let r = s.execute("type.toggleStyle", json!({"layer": id, "style": "underline"})).unwrap();
        assert_eq!(r["on"], json!(false));
        assert!((0..11).all(|i| !at(&text(&s, id), i).underline));
        // Caps: small caps replaces all caps; toggling off returns to normal.
        s.execute("type.toggleStyle", json!({"layer": id, "range": [0, 5], "style": "allCaps"})).unwrap();
        assert_eq!(at(&text(&s, id), 0).caps, Caps::AllCaps);
        s.execute("type.toggleStyle", json!({"layer": id, "range": [0, 5], "style": "smallCaps"})).unwrap();
        assert_eq!(at(&text(&s, id), 0).caps, Caps::SmallCaps);
        s.execute("type.toggleStyle", json!({"layer": id, "range": [0, 5], "style": "smallCaps"})).unwrap();
        assert_eq!(at(&text(&s, id), 0).caps, Caps::Normal);
        for style in ["strikethrough", "fauxBold", "fauxItalic"] {
            s.execute("type.toggleStyle", json!({"layer": id, "range": [6, 8], "style": style})).unwrap();
            let st = at(&text(&s, id), 6);
            assert!(has_style(&st, style), "{style}");
            assert!(!has_style(&at(&text(&s, id), 9), style), "{style} outside the range");
        }
    }

    #[test]
    fn toggle_hyphenation_follows_the_caret_paragraph() {
        let (mut s, id) = session();
        s.execute("type.edit", json!({"layer": id, "text": "one\ntwo"})).unwrap();
        s.execute("type.toggleStyle", json!({"layer": id, "range": [5, 5], "style": "hyphenate"})).unwrap();
        let p: Vec<bool> = text(&s, id).paragraph_runs().iter().map(|r| r.style.hyphenate).collect();
        assert_eq!(p, [false, true]);
    }

    #[test]
    fn bad_params_are_errors_not_panics() {
        let (mut s, id) = session();
        for p in [
            json!({"layer": id}),
            json!({"layer": id, "leading": "x"}),
            json!({"layer": id, "leading": 1e30}),
            json!({"layer": id, "tracking": f64::MAX}),
            json!({"layer": id, "baselineShift": 1, "metricsAsShown": 3}),
            json!({"layer": 9999, "leading": 2}),
        ] {
            assert!(s.execute("type.nudge", p.clone()).is_err(), "{p}");
        }
        for p in [
            json!({"layer": id}),
            json!({"layer": id, "style": "bold"}),
            json!({"layer": id, "style": "underline", "on": 1}),
            json!({"layer": 9999, "style": "underline"}),
        ] {
            assert!(s.execute("type.toggleStyle", p.clone()).is_err(), "{p}");
        }
        // Reversed or out-of-range character ranges clamp.
        s.execute("type.nudge", json!({"layer": id, "range": [50, 2], "tracking": 5})).unwrap();
        s.execute("type.toggleStyle", json!({"layer": id, "range": [99, 99], "style": "fauxBold"})).unwrap();
    }
}
