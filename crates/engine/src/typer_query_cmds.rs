//! TypeR bubble and line-shaping queries (WP1 and WP4 own this file; see `docs/typer.md`).
//!
//! Pure queries (`journal: false`) that never touch the selection or history. The bubble scan is
//! the same one `typer.align` uses: the wand at tolerance 20 on the page without its type layers
//! (cached per document change), holes filled, then TypeR 3.0's adaptive opening.

use photocraft_doc::{LayerContent, LayerId};
use photocraft_typer::bubble::{self, Rect};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::typer_place_cmds::{bad, has_doc, page_image, rect_json, selection_mask, typer_error};
use crate::{EngineError, Result, Session};

fn rows_param(p: &Value, cmd: &str) -> Result<usize> {
    match p.get("rows") {
        None | Some(Value::Null) => Ok(bubble::DEFAULT_ROWS),
        Some(v) => {
            v.as_u64().filter(|n| (1..=31).contains(n)).map(|n| n as usize).ok_or_else(|| bad(cmd, "\"rows\" must be 1..=31 (fewer than 5 counts as 5)"))
        }
    }
}

fn hit_json(hit: &bubble::BubbleHit, seed: Option<(i32, i32)>, reason: Option<&str>) -> Value {
    json!({
        "found": hit.found,
        "bbox": rect_json(&hit.bbox),
        "opened": rect_json(&hit.opened),
        "center": hit.center,
        "rows": hit.rows,
        "area": hit.area,
        "seed": seed,
        "reason": reason,
    })
}

fn detect_bubble(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.detectBubble";
    let tolerance = match p.get("tolerance") {
        None | Some(Value::Null) => bubble::WAND_TOLERANCE,
        Some(v) => v.as_f64().filter(|t| (0.0..=255.0).contains(t)).ok_or_else(|| bad(CMD, "\"tolerance\" must be 0..=255"))? as f32,
    };
    let rows = rows_param(p, CMD)?;
    let doc = std::sync::Arc::clone(&s.active().ok_or(EngineError::NoDocument)?.doc);
    let (seed, text): ((i32, i32), Option<(Rect, f64)>) = match (p.get("layer").filter(|v| !v.is_null()), p.get("x"), p.get("y")) {
        (Some(l), _, _) => {
            let id = LayerId(l.as_u64().ok_or_else(|| bad(CMD, "\"layer\" must be a layer id"))?);
            let layer = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
            if !matches!(layer.content, LayerContent::Text(_)) {
                return Err(typer_error(CMD, "noTextLayer"));
            }
            let ink = crate::typer_place_cmds::layer_ink(&doc, id).ok_or_else(|| typer_error(CMD, "noText"))?;
            (bubble::seed_left_of(&ink), Some((ink, crate::typer_place_cmds::layer_size_px(&doc, id))))
        }
        (None, Some(x), Some(y)) => {
            let c = |v: &Value| v.as_f64().filter(|n| n.is_finite() && n.abs() < 1e9).map(|n| n.floor() as i32);
            (c(x).zip(c(y)).ok_or_else(|| bad(CMD, "\"x\" and \"y\" must be pixel coordinates"))?, None)
        }
        _ => return Err(bad(CMD, "pass {\"x\", \"y\"} or {\"layer\"}")),
    };
    let page = page_image(s)?;
    // A joined double bubble: the lobe holding the text (or the clicked point).
    let at = text.as_ref().map(|(r, _)| r.center());
    let Some((mask, outline)) = bubble::wand_bubble(&page.img, page.area, seed, tolerance, at) else {
        return Ok(hit_json(&bubble::BubbleHit::default(), Some(seed), Some("outside the page")));
    };
    let judge = bubble::Judge {
        text: text.as_ref().map(|(r, sz)| (r, *sz)),
        seed: bubble::pixel(&page.img, page.area, seed),
        page: Some(page.rect()),
        outline: Some(outline),
    };
    let (hit, reason) = bubble::hit(&mask, &judge, rows);
    Ok(hit_json(&hit, Some(seed), reason))
}

/// `typer.shapeText` also shapes plain text without a document.
fn has_doc_or_text(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

fn has_selection(s: &Session) -> std::result::Result<(), String> {
    s.active().filter(|d| d.doc.selection.is_some()).map(|_| ()).ok_or_else(|| "no selection".into())
}

fn selection_bounds(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.selectionBounds";
    let rows = rows_param(p, CMD)?;
    let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
    let mask = selection_mask(doc).ok_or_else(|| typer_error(CMD, "noSelection"))?;
    // The user's selection is taken as it is: only its size is judged.
    let (mut hit, _) = bubble::hit(&mask, &bubble::Judge::default(), rows);
    hit.found = hit.opened.area() >= bubble::MIN_AREA;
    Ok(hit_json(&hit, None, (!hit.found).then_some("smallSelection")))
}

/// Most bubbles `typer.detectBubbles {store: true}` stores.
const MAX_STORED: usize = 1000;

fn detect_bubbles(s: &mut Session, p: &Value) -> Result<Value> {
    use photocraft_typer::pagedetect::{self, DetectOptions};
    const CMD: &str = "typer.detectBubbles";
    let sensitivity = match p.get("sensitivity") {
        None | Some(Value::Null) => pagedetect::DEFAULT_SENSITIVITY,
        Some(v) => v.as_u64().filter(|n| (1..=9).contains(n)).ok_or_else(|| bad(CMD, "\"sensitivity\" must be 1..=9"))? as u8,
    };
    let flag = |key: &str, default: bool| -> Result<bool> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(v) => v.as_bool().ok_or_else(|| bad(CMD, format!("\"{key}\" must be true or false"))),
        }
    };
    let (rtl, splits, store) = (flag("rtl", true)?, flag("splits", true)?, flag("store", false)?);
    let rows = rows_param(p, CMD)?;
    let page = page_image(s)?;
    let (w, h) = (page.area.width() as usize, page.area.height() as usize);
    let opts = DetectOptions { sensitivity, rtl, splits, rows };
    let found = pagedetect::detect(&pagedetect::Page { px: &page.img, width: w, height: h }, (f64::from(page.area.x0), f64::from(page.area.y0)), &opts);
    let mut stored = Vec::new();
    if store {
        // TypeR's auto fill: bubbles take consecutive usable script lines from the current one,
        // as if each had been captured in multi-bubble mode.
        let st = &s.typer.state;
        let lines = st.lines();
        let mut cursor = st.current_line().map(|_| st.current_line_index);
        let style = st.current_style_id.clone();
        let mut last = None;
        for b in &found {
            if s.typer.stored_selections.len() + stored.len() >= MAX_STORED {
                break;
            }
            let key = b.bbox.key();
            if s.typer.stored_selections.iter().chain(stored.iter()).any(|x: &bubble::StoredSelection| x.bounds.key() == key) {
                continue;
            }
            let line_index = cursor.and_then(|c| photocraft_typer::script::next_usable(lines, c));
            if let Some(l) = line_index {
                last = Some(l);
                cursor = Some(l + 1);
            } else {
                cursor = None;
            }
            stored.push(bubble::StoredSelection { bounds: b.bbox, opened: Some(b.opened), line_index, style_id: style.clone() });
        }
        let n = stored.len();
        s.typer.stored_selections.append(&mut stored);
        if let Some(l) = last
            && n > 0
        {
            let next = photocraft_typer::script::next_usable(s.typer.state.lines(), l + 1).unwrap_or(l);
            s.typer.state.set_line(next);
        }
        s.typer.state.edit_settings(|st| st.multi_bubble_mode = true);
    }
    Ok(json!({
        "bubbles": found.iter().map(|b| json!({
            "bbox": rect_json(&b.bbox),
            "opened": rect_json(&b.opened),
            "center": b.center,
            "rows": b.rows,
            "confidence": b.confidence,
            "split": b.split,
            "area": b.area,
        })).collect::<Vec<_>>(),
        "count": found.len(),
        "order": if rtl { "rtl" } else { "ltr" },
        "stored": if store { Some(s.typer.stored_selections.len()) } else { None },
        "state": if store { Some(crate::typer_cmds::summary(&s.typer.state)) } else { None },
    }))
}

/// Most variants `typer.shapeText` returns.
const MAX_SHAPE_LIMIT: u64 = 64;

/// A bubble for TextShapeR: its body box, outline rows and where it came from; or why there is none.
type ShapeBubble = std::result::Result<(Rect, Vec<bubble::Row>, &'static str), &'static str>;

/// The bubble TextShapeR fits into: the user's selection, else the wand around the layer's text
/// (as Align finds it). `Err(reason)` when there is none.
fn shape_bubble(s: &mut Session, id: LayerId, rows: usize) -> Result<ShapeBubble> {
    let doc = std::sync::Arc::clone(&s.active().ok_or(EngineError::NoDocument)?.doc);
    if let Some(mask) = selection_mask(&doc) {
        return Ok(match bubble::body_rows(&mask, rows) {
            Some((o, r)) if o.opened.area() >= bubble::MIN_AREA => Ok((o.opened, r, "selection")),
            _ => Err("smallSelection"),
        });
    }
    let Some(ink) = crate::typer_place_cmds::layer_ink(&doc, id) else { return Ok(Err("noText")) };
    let size = crate::typer_place_cmds::layer_size_px(&doc, id);
    let page = page_image(s)?;
    let seed = bubble::seed_left_of(&ink);
    let Some((mask, outline)) = bubble::wand_bubble(&page.img, page.area, seed, bubble::WAND_TOLERANCE, Some(ink.center())) else {
        return Ok(Err("noSelection"));
    };
    let Some((o, r)) = bubble::body_rows(&mask, rows) else { return Ok(Err("noSelection")) };
    let judge = bubble::Judge { text: Some((&ink, size)), seed: bubble::pixel(&page.img, page.area, seed), page: Some(page.rect()), outline: Some(outline) };
    Ok(match judge.verdict(&o) {
        None => Ok((o.opened, r, "wand")),
        Some(why) => Err(why),
    })
}

fn shape_text(s: &mut Session, p: &Value) -> Result<Value> {
    use photocraft_typer::shaper::{self, Calibration, CharClass, Hyphenation, Options, Profile, TextMeasure};
    const CMD: &str = "typer.shapeText";
    let profile = match p.get("profile") {
        None | Some(Value::Null) => Profile::Balanced,
        Some(v) => v.as_str().and_then(Profile::parse).ok_or_else(|| bad(CMD, "\"profile\" must be balanced, round, tall or wide"))?,
    };
    let limit = match p.get("limit") {
        None | Some(Value::Null) => shaper::MAX_VARIANTS,
        Some(v) => v.as_u64().filter(|n| (1..=MAX_SHAPE_LIMIT).contains(n)).ok_or_else(|| bad(CMD, "\"limit\" must be 1..=64"))? as usize,
    };
    let max_lines = match p.get("maxLines") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().filter(|n| (1..=8).contains(n)).ok_or_else(|| bad(CMD, "\"maxLines\" must be 1..=8"))? as usize),
    };
    let flag = |key: &str, default: bool| -> Result<bool> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(v) => v.as_bool().ok_or_else(|| bad(CMD, format!("\"{key}\" must be true or false"))),
        }
    };
    let hyphenate = flag("hyphenate", true)?;
    let dehyphenate = flag("dehyphenate", s.typer.state.settings.dehyphenate_text_shape_r)?;
    let language = match p.get("language") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let tag = v.as_str().filter(|t| t.len() <= 35).ok_or_else(|| bad(CMD, "\"language\" must be a language tag (es, pt, en, fr)"))?;
            Some(Hyphenation::from_tag(tag).ok_or_else(|| bad(CMD, "\"language\" must be es, pt, en or fr"))?)
        }
    };
    let text_in = match p.get("text") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let t = v.as_str().ok_or_else(|| bad(CMD, "\"text\" must be a string"))?;
            if t.len() > shaper::MAX_TEXT {
                return Err(bad(CMD, format!("\"text\" is longer than {} bytes", shaper::MAX_TEXT)));
            }
            Some(t.to_string())
        }
    };
    // "rows" is an outline (a list) or how many rows to sample from the layer's bubble (a count).
    let rows_wanted = if p.get("rows").is_some_and(Value::is_array) { bubble::DEFAULT_ROWS } else { rows_param(p, CMD)? };
    let num = |key: &str| -> Result<Option<f64>> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_f64()
                .filter(|n| n.is_finite() && *n > 0.0 && *n < 1e6)
                .map(Some)
                .ok_or_else(|| bad(CMD, format!("\"{key}\" must be a positive number of pixels"))),
        }
    };
    let layer_param = match p.get("layer") {
        None | Some(Value::Null) => None,
        Some(v) => Some(LayerId(v.as_u64().ok_or_else(|| bad(CMD, "\"layer\" must be a layer id"))?)),
    };
    let (width, height) = (num("width")?, num("height")?);
    // Without text or a box, shape the active type layer.
    let layer = match layer_param {
        Some(id) => Some(id),
        None if text_in.is_none() || (width.is_none() && height.is_none() && !p.get("rows").is_some_and(Value::is_array)) => s
            .active()
            .and_then(|d| d.active_layer)
            .filter(|id| s.active().and_then(|d| d.doc.layer(*id)).is_some_and(|l| matches!(l.content, LayerContent::Text(_)))),
        None => None,
    };
    let mut opts = Options { profile, limit, max_lines, hyphenation: None, dehyphenate, ..Default::default() };
    let mut bubble_json = Value::Null;
    let (text, variants) = if let Some(id) = layer {
        let doc = std::sync::Arc::clone(&s.active().ok_or(EngineError::NoDocument)?.doc);
        let l = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
        let LayerContent::Text(t) = &l.content else { return Err(typer_error(CMD, "noTextLayer")) };
        let text = text_in.unwrap_or_else(|| t.text.replace(['\r', '\n', '\u{3}'], " "));
        if text.trim().is_empty() {
            return Err(typer_error(CMD, "noText"));
        }
        let style = t.char_runs().first().map(|r| r.style.clone()).unwrap_or_default();
        let para = t.paragraph_runs().first().map(|r| r.style.clone()).unwrap_or_default();
        let dpi = doc.resolution_dpi;
        let k = f64::from(if dpi > 0.0 { dpi / 72.0 } else { 1.0 });
        let size_px = f64::from(style.size_pt) * k;
        let line_px = style.leading_pt.map_or(f64::from(para.auto_leading) * size_px, |l| f64::from(l) * k);
        opts.hyphenation = if hyphenate { language.or_else(|| Hyphenation::for_text(style.language.as_deref(), &text)) } else { None };
        match (width, height) {
            (Some(w), Some(h)) => {
                opts.width = Some(w);
                opts.height = Some(h);
                opts.rows = rows_list(p, CMD)?;
            }
            _ => match shape_bubble(s, id, rows_wanted)? {
                Ok((rect, rows, source)) => {
                    opts.width = Some(rect.width);
                    opts.height = Some(rect.height);
                    opts.rows = rows;
                    bubble_json = json!({"found": true, "source": source, "box": rect_json(&rect)});
                }
                Err(why) => bubble_json = json!({"found": false, "reason": why}),
            },
        }
        if line_px.is_finite() && line_px > 0.0 {
            opts.calibration = Some(Calibration { unit_px: None, line_px });
        }
        let mut eng = photocraft_text::shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let v = shaper::shape(&text, &opts, &mut TextMeasure { engine: &mut eng, style, dpi });
        (text, v)
    } else {
        let text = text_in.ok_or_else(|| bad(CMD, "pass \"text\", or a type \"layer\" (default: the active one)"))?;
        opts.hyphenation = if hyphenate { language.or_else(|| Hyphenation::for_text(None, &text)) } else { None };
        opts.width = width;
        opts.height = height;
        opts.rows = rows_list(p, CMD)?;
        if let Some(c) = p.get("calibration").filter(|v| !v.is_null()) {
            let f = |k: &str| c.get(k).and_then(Value::as_f64).filter(|n| n.is_finite() && *n > 0.0 && *n < 1e6);
            let (Some(u), Some(lp)) = (f("unitPx"), f("linePx")) else { return Err(bad(CMD, "\"calibration\" must be {\"unitPx\": px, \"linePx\": px}")) };
            opts.calibration = Some(Calibration { unit_px: Some(u), line_px: lp });
        }
        let v = shaper::shape(&text, &opts, &mut CharClass);
        (text, v)
    };
    let language = match opts.hyphenation {
        Some(Hyphenation::French) => Some("fr"),
        Some(Hyphenation::Patterns(l)) => Some(l.code()),
        None => None,
    };
    Ok(json!({
        "text": text,
        "layer": layer.map(|l| l.0),
        "variants": variants.iter().map(|v| json!({
            "lines": v.lines,
            "text": v.text(),
            "hyphens": v.hyphens,
            "fits": v.fits,
            "score": v.score,
            "badness": v.badness,
            "widths": v.widths,
        })).collect::<Vec<_>>(),
        "bubble": bubble_json,
        "language": language,
        "profile": profile,
    }))
}

/// `"rows"` as a list of outline rows (`{y, left, right, width?}`, 0..1), at most 64.
fn rows_list(p: &Value, cmd: &str) -> Result<Vec<bubble::Row>> {
    let Some(v) = p.get("rows").filter(|v| !v.is_null() && !v.is_number()) else { return Ok(Vec::new()) };
    let a = v.as_array().filter(|a| a.len() <= 64).ok_or_else(|| bad(cmd, "\"rows\" must be a list of at most 64 {y, left, right} rows"))?;
    a.iter()
        .map(|r| {
            let f = |k: &str| r.get(k).and_then(Value::as_f64).filter(|n| n.is_finite());
            let (Some(y), Some(left), Some(right)) = (f("y"), f("left"), f("right")) else {
                return Err(bad(cmd, "each row needs numeric \"y\", \"left\" and \"right\" (0..1)"));
            };
            Ok(bubble::Row { y, left, right, width: f("width").unwrap_or(right - left) })
        })
        .collect()
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        // D1-D3: wand at tolerance 20 on the page, then the adaptive opening (P7).
        CommandSpec {
            id: "typer.detectBubble",
            label: "TypeR Detect Bubble",
            menu: &[],
            shortcut: None,
            params: r#"{"x":px,"y":px}|{"layer":id (seeds 5 px left of its text, like Align)},"tolerance":0..255=20,"rows":5..31=21 → {found,bbox,opened,center,rows:[{y,left,right,width}],area,seed,reason} (type layers are left out of the scan; found = a light filled region ≥ 16 px across and ≥ 200 px² opened, and with a layer it holds ≥ 60 % of the text and is ≤ 60× its area; reason = why not)"#,
            enabled: has_doc,
            run: detect_bubble,
            journal: false,
        },
        CommandSpec {
            id: "typer.selectionBounds",
            label: "TypeR Selection Bounds",
            menu: &[],
            shortcut: None,
            params: r#"{"rows":5..31=21} → {found,bbox,opened,center,rows,area} of the current selection (holes filled, tail removed)"#,
            enabled: has_selection,
            run: selection_bounds,
            journal: false,
        },
        // D7: every bubble on the page, in reading order (multi-bubble auto fill).
        CommandSpec {
            id: "typer.detectBubbles",
            label: "TypeR Detect Bubbles",
            menu: &[],
            shortcut: None,
            params: r#"{"sensitivity":1..9=5 (higher accepts greyer whites and weaker bubbles),"rtl":bool=true (manga: right to left inside a row),"splits":bool=true (split joined double bubbles),"rows":5..31=21,"store":bool=false (store them as multi-bubble selections with consecutive script lines from the current one, turn multi-bubble mode on and move the cursor past them; then typer.pasteMany fills them)} → {bubbles:[{bbox,opened,center,rows,confidence,split,area}],count,order,stored?,state?} (white regions sealed by a dark outline, scored with TypeR's prior model; type layers are left out of the scan)"#,
            enabled: has_doc,
            run: detect_bubbles,
            journal: false,
        },
        // D4: TextShapeR variants measured with the style's real font.
        CommandSpec {
            id: "typer.shapeText",
            label: "TypeR Shape Text",
            menu: &[],
            shortcut: None,
            params: r#"{"layer":id? (default: the active type layer; its text, style, leading and bubble),"text":string? (default: the layer's text),"width":px?,"height":px?,"rows":[{y,left,right}]?|5..31=21 (with width/height: the bubble outline instead of the layer's bubble; without a layer: character-class widths, "calibration":{"unitPx","linePx"}? for the fit),"limit":1..64=12,"maxLines":1..8?,"profile":"balanced"|"round"|"tall"|"wide"="balanced","hyphenate":bool=true,"language":"es"|"pt"|"en"|"fr"? (default: the style's language, else guessed from the text),"dehyphenate":bool=setting dehyphenateTextShapeR} → {text,layer,variants:[{lines,text,hyphens,fits,score,badness,widths}],bubble:{found,source:"selection"|"wand",box}|{found:false,reason},language,profile}. The bubble is the selection, else the wand left of the text (tail removed); lines are measured with the layer's font. Apply one with typer.applyShape."#,
            enabled: has_doc_or_text,
            run: shape_text,
            journal: false,
        },
    ]
}

#[cfg(test)]
#[path = "typer_query_cmds_tests.rs"]
mod tests;
