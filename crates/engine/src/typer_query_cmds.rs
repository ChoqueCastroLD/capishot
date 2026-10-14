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

fn wp4(_: &crate::Session) -> std::result::Result<(), String> {
    Err("not implemented yet (TypeR line shaping, work package WP4)".into())
}

/// Never reached while `enabled` refuses; kept so a direct call still errors cleanly.
fn not_yet(_: &mut Session, _: &Value) -> Result<Value> {
    Err(EngineError::Other("not implemented yet (TypeR work package WP4)".into()))
}

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
    let Some(mask) = bubble::wand(&page.img, page.area, seed, tolerance) else {
        return Ok(hit_json(&bubble::BubbleHit::default(), Some(seed), Some("outside the page")));
    };
    let judge = bubble::Judge { text: text.as_ref().map(|(r, sz)| (r, *sz)), seed: bubble::pixel(&page.img, page.area, seed), page: Some(page.rect()) };
    let (hit, reason) = bubble::hit(&mask, &judge, rows);
    Ok(hit_json(&hit, Some(seed), reason))
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
        // D4: TextShapeR variants measured with the style's real font.
        CommandSpec {
            id: "typer.shapeText",
            label: "TypeR Shape Text",
            menu: &[],
            shortcut: None,
            params: r#"{"text":string,"layer":id?|"width":px,"height":px,"rows":[{y,left,right}]?,"limit":int=12,"profile":"balanced"|"round"|"tall"|"wide"?,"hyphenate":bool=true,"language":"es"|"pt"|"en"|"fr"?} → [{lines,hyphens,fits,score}] (not implemented yet: WP4)"#,
            enabled: wp4,
            run: not_yet,
            journal: false,
        },
    ]
}
