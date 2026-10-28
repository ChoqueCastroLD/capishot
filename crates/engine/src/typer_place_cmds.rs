// Ported from TypeR app_src/host.js (master 342be99: _createTextLayerInSelection,
// _setActiveLayerText, _alignTextLayerToSelection, _changeActiveLayerTextSize,
// _createTextLayersInStoredSelections; develop 71bc061: keep size, apply to many,
// toggleCleaningLayers, the measure-box fit) and app_src/context.jsx (multi-bubble capture), MIT,
// (c) 2017 Swirt. See crates/typer/LICENSE-TypeR.txt. Behaviour only: no jam/ActionDescriptor
// code is ported.

//! TypeR placement commands (`docs/typer.md`): paste the current line into a bubble, apply lines
//! and styles to type layers, centre layers in their bubbles, resize, multi-bubble capture and
//! paste, the cleaning-layer toggle, and "copy layer style".
//!
//! Every document change is one `Session::edit`, so one undo step per user action (named like
//! TypeR's: "TypeR Paste", "TypeR Change", "TypeR Align", "TypeR Resize", "TypeR Multiple
//! Paste"). A failure anywhere inside returns before the edit commits: nothing changes. The
//! cursor advances (TypeR's `nextLine {add}`) only after success, and only when the text came
//! from the script.

mod support;
#[cfg(test)]
mod tests;

use photocraft_doc::text::{ParagraphRun, TextRun, TextShape};
use photocraft_doc::{ColorMode, Effect, FxPaint, Layer, LayerContent, LayerId};
use photocraft_typer::bubble::{self, Opened, Rect, StoredSelection};
use photocraft_typer::payload::{self, Stored};
use photocraft_typer::psjson::{self, MapOptions};
use photocraft_typer::{Id, Style, TextType, place};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::typer_cmds::summary;
use crate::{EngineError, Result, Session};
use support::{DocInfo, Fit, Styled};
pub(crate) use support::{PlaceState, bad, layer_ink, layer_size_px, page_image, rect_json, selection_mask, set_stroke, typer_error};

/// Longest text a placement takes (bytes).
pub(crate) const MAX_TEXT_BYTES: usize = 64 << 10;
/// Most layers or bubbles one command fills.
pub(crate) const MAX_LAYERS: usize = 4096;

/// Forget a closed document's cached page and hidden layers.
pub(crate) fn forget_doc(s: &mut Session, id: photocraft_doc::DocId) {
    support::forget_doc(s, id);
}

pub(crate) fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

/// The style a paste or apply uses: the `style` param, else the current style, else TypeR's
/// default style.
fn chosen_style(s: &Session, p: &Value, cmd: &str) -> Result<Style> {
    Ok(support::style_param(s, p, cmd)?.or_else(|| s.typer.state.current_style().cloned()).unwrap_or_else(|| Style::new(Id::str("default"), "Default")))
}

/// Style mapping options for the active document (text scale applied unless `scaled`).
fn map_opts(s: &Session, info: &DocInfo, p: &Value, cmd: &str, scaled: bool) -> Result<MapOptions> {
    let set = &s.typer.state.settings;
    Ok(MapOptions {
        dpi: f64::from(info.dpi),
        text_scale: if scaled { None } else { set.text_scale },
        page_width: Some(info.width),
        direction: support::direction_param(p, cmd, set.direction)?,
    })
}

/// Size of the active type layer (keep size, P8); `sizeSource` when it isn't a type layer.
fn active_size_pt(s: &Session, cmd: &str) -> Result<f32> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    match st.active_layer.and_then(|id| st.doc.layer(id)).map(|l| &l.content) {
        Some(LayerContent::Text(t)) => Ok(t.runs.first().map_or(t.size_pt, |r| r.style.size_pt)),
        _ => Err(typer_error(cmd, "sizeSource")),
    }
}

/// The current script line's text; `noText` without one.
fn current_text(s: &Session, cmd: &str) -> Result<String> {
    s.typer.state.current_line().map(|l| l.text.clone()).filter(|t| !t.trim().is_empty()).ok_or_else(|| typer_error(cmd, "noText"))
}

fn set_damage(s: &mut Session, damage: Option<photocraft_geom::Rect>) {
    if let Some(st) = s.active_mut() {
        st.last_damage = damage;
    }
}

fn union(a: Option<photocraft_geom::Rect>, b: Option<photocraft_geom::Rect>) -> Option<photocraft_geom::Rect> {
    a.zip(b).map(|(a, b)| a.union(&b))
}

/// The bubble a paste fills: `bounds` (used as given, like a stored selection) or the opened
/// document selection.
fn paste_target(s: &Session, bounds: Option<Rect>, cmd: &str) -> Result<Opened> {
    if let Some(b) = bounds {
        if b.area() < bubble::MIN_AREA {
            return Err(typer_error(cmd, "smallSelection"));
        }
        return Ok(Opened { bbox: b, opened: b, radius: 0, area: b.area() as usize });
    }
    let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
    support::selection_target(doc, cmd)?.ok_or_else(|| typer_error(cmd, "noSelection"))
}

/// One new layer to place: text, resolved style, kind and bubble.
struct NewText {
    text: String,
    styled: Styled,
    point: bool,
    bubble: Rect,
}

/// Create the layers (one edit, labelled `label`) above the active layer, each centred in its
/// bubble. The last one becomes active. Returns `(id, ink)` per layer.
fn place_new(s: &mut Session, label: &str, items: Vec<NewText>, padding: f64) -> Result<Vec<(LayerId, Option<Rect>)>> {
    let md = s.typer.state.settings.interpret_markdown;
    let (out, damage) = s.edit(label, |doc, active| {
        let info = DocInfo::of(doc);
        let mut out = Vec::new();
        let mut damage = Some(photocraft_geom::Rect::EMPTY);
        for it in items {
            let (bw, bh) = place::box_size(&it.bubble, padding);
            let mut t = support::new_text(&it.text, &it.styled, it.point, bw, bh, md);
            if t.text.trim().is_empty() {
                continue;
            }
            let name = crate::type_cmds::layer_name(&t.text);
            let fit = if it.point { Fit::Keep } else { Fit::Height };
            let ink = support::fit_and_center(&mut t, fit, it.bubble.center(), info);
            let mut layer = Layer::new(name, LayerContent::Text(t));
            support::set_stroke(&mut layer, it.styled.stroke.as_ref(), doc.mode);
            damage = union(damage, support::damage_of(&layer));
            let id = doc.insert_above(*active, layer);
            *active = Some(id);
            out.push((id, ink));
        }
        if out.is_empty() {
            return Err(EngineError::Other("noText: nothing to paste (every text was empty)".into()));
        }
        Ok((out, damage))
    })?;
    set_damage(s, damage);
    Ok(out)
}

fn paste(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.paste";
    let text = support::text_param(p, "text", CMD)?;
    let bounds = support::rect_param(p, "bounds", CMD)?;
    let advance = support::bool_param(p, "advance", CMD)?.unwrap_or(true);
    let keep = support::bool_param(p, "keepSize", CMD)?.unwrap_or(false);
    let point = support::bool_param(p, "point", CMD)?;
    let padding = support::num_param(p, "padding", CMD, 0.0, 1000.0)?;
    let (has_selection, text_layers) = {
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let texts = support::click_order(st).into_iter().filter(|id| matches!(st.doc.layer(*id).map(|l| &l.content), Some(LayerContent::Text(_)))).count();
        (st.doc.selection.is_some(), texts)
    };
    // TypeR's "add" key: with stored bubbles in multi-bubble mode it fills them; with several
    // type layers selected and no selection it applies successive lines to them (3.0).
    if text.is_none() && bounds.is_none() && p.get("style").is_none() {
        if s.typer.state.settings.multi_bubble_mode && !s.typer.stored_selections.is_empty() {
            return paste_many(s, p);
        }
        if !has_selection && text_layers >= 2 {
            return apply_lines(s, p);
        }
    }
    let target = paste_target(s, bounds, CMD)?;
    let from_script = text.is_none();
    let text = match text {
        Some(t) if !t.trim().is_empty() => t,
        Some(_) => return Err(typer_error(CMD, "noText")),
        None => current_text(s, CMD)?,
    };
    let style = chosen_style(s, p, CMD)?;
    let size = if keep { Some(active_size_pt(s, CMD)?) } else { None };
    let info = DocInfo::of(&s.active().ok_or(EngineError::NoDocument)?.doc);
    let opts = map_opts(s, &info, p, CMD, false)?;
    let settings = &s.typer.state.settings;
    let point = point.unwrap_or_else(|| payload::resolve_point_text(Some(&style), settings.paste_point_text));
    let padding = padding.unwrap_or(settings.internal_padding);
    let fallback = place::px_to_pt(place::FALLBACK_SIZE_PX, f64::from(info.dpi)) as f32;
    let styled = support::styled(&style, &opts, size, fallback);
    let placed = place_new(s, "TypeR Paste", vec![NewText { text, styled, point, bubble: target.opened }], padding)?;
    let moved = from_script && advance && s.typer.state.next_line(true) == photocraft_typer::Moved::Yes;
    let (id, ink) = placed.first().copied().ok_or_else(|| EngineError::Other("nothing pasted".into()))?;
    Ok(json!({
        "mode": "paste",
        "layer": id.0,
        "bounds": ink.as_ref().map(rect_json),
        "bubble": rect_json(&target.opened),
        "center": target.opened.center(),
        "point": point,
        "advanced": moved,
        "state": summary(&s.typer.state),
    }))
}

/// One `pasteMany` entry: text, style (default: the current one) and bubble.
type Entry = (String, Option<Style>, Rect);

/// `entries` of `typer.pasteMany`.
fn entries_param(s: &Session, p: &Value, cmd: &str) -> Result<Option<Vec<Entry>>> {
    let Some(v) = p.get("entries").filter(|v| !v.is_null()) else { return Ok(None) };
    let list = v.as_array().ok_or_else(|| bad(cmd, "\"entries\" must be a list of {text, style?, bounds}"))?;
    if list.is_empty() || list.len() > MAX_LAYERS {
        return Err(bad(cmd, format!("\"entries\" must hold 1..={MAX_LAYERS} items")));
    }
    let mut out = Vec::new();
    for (i, e) in list.iter().enumerate() {
        if !e.is_object() {
            return Err(bad(cmd, format!("entries[{i}] must be {{text, style?, bounds}}")));
        }
        let text = support::text_param(e, "text", cmd)?.ok_or_else(|| bad(cmd, format!("entries[{i}].text is missing")))?;
        let bounds = support::rect_param(e, "bounds", cmd)?.ok_or_else(|| bad(cmd, format!("entries[{i}].bounds is missing")))?;
        out.push((text, support::style_param(s, e, cmd)?, bounds));
    }
    Ok(Some(out))
}

fn paste_many(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.pasteMany";
    let keep = support::bool_param(p, "keepSize", CMD)?.unwrap_or(false);
    let point = support::bool_param(p, "point", CMD)?;
    let padding = support::num_param(p, "padding", CMD, 0.0, 1000.0)?.unwrap_or(s.typer.state.settings.internal_padding);
    let size = if keep { Some(active_size_pt(s, CMD)?) } else { None };
    let info = DocInfo::of(&s.active().ok_or(EngineError::NoDocument)?.doc);
    let fallback = place::px_to_pt(place::FALLBACK_SIZE_PX, f64::from(info.dpi)) as f32;
    let settings = s.typer.state.settings.clone();
    let current = chosen_style(s, &Value::Null, CMD)?;
    let (jobs, stored): (Vec<(String, Style, Rect, bool)>, bool) = match entries_param(s, p, CMD)? {
        Some(list) => (list.into_iter().map(|(t, st, b)| (t, st.unwrap_or_else(|| current.clone()), b, false)).collect(), false),
        None => {
            let sel = &s.typer.stored_selections;
            if sel.is_empty() {
                return Err(typer_error(CMD, "noSelection"));
            }
            let st = &s.typer.state;
            let stored: Vec<Stored> = sel.iter().map(|x| Stored { line_index: x.line_index, style_id: x.style_id.clone() }).collect();
            let picked = payload::stored_selections(&stored, st.lines(), st.current_line_index, &st.library, st.current_style_id.as_ref(), settings.text_scale);
            if picked.is_empty() {
                return Err(typer_error(CMD, "noText"));
            }
            let jobs = picked
                .into_iter()
                .zip(sel.iter())
                .map(|((text, style), x)| {
                    let b = if settings.open_stored_selections { x.opened.unwrap_or(x.bounds) } else { x.bounds };
                    (text, style.unwrap_or_else(|| current.clone()), b, true)
                })
                .collect();
            (jobs, true)
        }
    };
    let mut items = Vec::new();
    for (text, style, b, scaled) in jobs {
        let opts = map_opts(s, &info, p, CMD, scaled)?;
        let pt = point.unwrap_or_else(|| payload::resolve_point_text(Some(&style), settings.paste_point_text));
        items.push(NewText { text, styled: support::styled(&style, &opts, size, fallback), point: pt, bubble: b });
    }
    let placed = place_new(s, "TypeR Multiple Paste", items, padding)?;
    if stored {
        s.typer.stored_selections.clear();
    }
    Ok(json!({
        "mode": "pasteMany",
        "layers": placed.iter().map(|(id, _)| id.0).collect::<Vec<_>>(),
        "bounds": placed.iter().map(|(_, i)| i.as_ref().map(rect_json)).collect::<Vec<_>>(),
        "state": summary(&s.typer.state),
    }))
}

/// One layer of a `retext_all` batch: layer, mode, text, style with its mapping options.
type Retext = (LayerId, Mode, Option<String>, Option<(Style, MapOptions)>);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    TextAndStyle,
    TextOnly,
    StyleOnly,
}

/// Replace a type layer's text and/or style (`_setActiveLayerText`), keeping its ink centre.
/// Point text stays point and box text keeps its width (unless the style's own layer kind says
/// otherwise); the box height is refit to the text. A style without a size, or keep size, keeps
/// the layer's size.
#[allow(clippy::too_many_arguments)]
fn retext(
    l: &mut Layer,
    mode: Mode,
    text: Option<&str>,
    style: Option<(&Style, &MapOptions)>,
    keep_size: bool,
    md: bool,
    info: DocInfo,
    colors: ColorMode,
) -> Result<Option<Rect>> {
    let id = l.id;
    let LayerContent::Text(t) = &mut l.content else {
        return Err(EngineError::Other(format!("noTextLayer: layer {} is not a type layer", id.0)));
    };
    let old_ink = support::ink(t).or_else(|| support::measure(t, info.dpi, info.format));
    let old_size = t.runs.first().map_or(t.size_pt, |r| r.style.size_pt);
    let center = old_ink.map_or((t.transform.m[4], t.transform.m[5]), |i| i.center());
    let was_point = matches!(t.shape, TextShape::Point);
    let mut stroke = None;
    let mut target_point = was_point;
    match (mode, style) {
        (Mode::TextOnly, _) | (_, None) => {
            let text = text.ok_or_else(|| EngineError::Other("noText: no text to apply".into()))?;
            let base = t.char_runs().first().map(|r| r.style.clone()).unwrap_or_default();
            let para = support::safe_paragraph(t);
            let (text, runs) = support::runs_for(text, &base, md);
            t.paragraphs = vec![ParagraphRun { len: text.len(), style: para }];
            t.runs = runs;
            t.text = text;
        }
        (_, Some((style, opts))) => {
            let st = support::styled(style, opts, keep_size.then_some(old_size), old_size);
            let text = match (mode, text) {
                (Mode::StyleOnly, _) | (_, None) => t.text.clone(),
                (_, Some(x)) => x.to_string(),
            };
            let (text, runs) = if mode == Mode::StyleOnly {
                (text.clone(), vec![TextRun { len: text.len(), style: st.char_style.clone() }])
            } else {
                support::runs_for(&text, &st.char_style, md)
            };
            t.paragraphs = vec![ParagraphRun { len: text.len(), style: st.paragraph.clone() }];
            t.runs = runs;
            t.text = text;
            if let Some(aa) = st.antialias {
                t.antialias = aa;
            }
            stroke = st.stroke;
            target_point = match style.text_type() {
                TextType::Point => true,
                TextType::Paragraph => false,
                TextType::Inherit => was_point,
            };
        }
    }
    t.sync_summary();
    let new_size = place::pt_to_px(f64::from(t.runs.first().map_or(t.size_pt, |r| r.style.size_pt)), f64::from(info.dpi));
    if target_point && !was_point {
        t.shape = TextShape::Point;
    } else if !target_point && was_point {
        // A point layer turned into box text: as wide as its text was, scaled with the size.
        let ratio = place::size_ratio(f64::from(old_size), place::px_to_pt(new_size, f64::from(info.dpi))).max(1.0);
        let w = old_ink.map_or(new_size * 10.0, |i| (i.width * ratio).max(i.width + 0.5 * new_size));
        t.shape = TextShape::Box { x: 0.0, y: 0.0, width: w.max(1.0) as f32, height: 1.0 };
    }
    let fit = if target_point { Fit::Keep } else { Fit::Height };
    let ink = support::fit_and_center(t, fit, center, info);
    support::set_stroke(l, stroke.as_ref(), colors);
    Ok(ink)
}

/// Run `retext` over `targets` in one edit; any failure leaves the document unchanged.
fn retext_all(s: &mut Session, label: &str, targets: Vec<Retext>, keep_size: bool) -> Result<Vec<(LayerId, Option<Rect>)>> {
    let md = s.typer.state.settings.interpret_markdown;
    let (out, damage) = s.edit(label, |doc, _| {
        let info = DocInfo::of(doc);
        let mode_colors = doc.mode;
        let mut out = Vec::new();
        let mut damage = Some(photocraft_geom::Rect::EMPTY);
        for (id, mode, text, style) in &targets {
            let l = doc.layer_mut(*id).ok_or(EngineError::NoLayer(*id))?;
            let before = support::damage_of(l);
            let ink = retext(l, *mode, text.as_deref(), style.as_ref().map(|(s, o)| (s, o)), keep_size, md, info, mode_colors)?;
            let after = support::damage_of(l);
            damage = union(union(damage, before), after);
            out.push((*id, ink));
        }
        Ok((out, damage))
    })?;
    set_damage(s, damage);
    Ok(out)
}

fn apply_with(s: &mut Session, p: &Value, cmd: &str, mode: Mode) -> Result<Value> {
    let text_given = support::text_param(p, "text", cmd)?;
    let advance = support::bool_param(p, "advance", cmd)?.unwrap_or(true);
    let keep = support::bool_param(p, "keepSize", cmd)?.unwrap_or(false);
    let ids = support::target_layers(s, p, cmd)?;
    let ids = support::text_targets(s, &ids, cmd, true)?;
    let from_script = text_given.is_none() && mode != Mode::StyleOnly;
    let text = match (mode, text_given) {
        (Mode::StyleOnly, _) => None,
        (_, Some(t)) if t.trim().is_empty() => return Err(typer_error(cmd, "noText")),
        (_, Some(t)) => Some(t),
        (_, None) => Some(current_text(s, cmd)?),
    };
    let info = DocInfo::of(&s.active().ok_or(EngineError::NoDocument)?.doc);
    let style = match mode {
        Mode::TextOnly => None,
        _ => Some((chosen_style(s, p, cmd)?, map_opts(s, &info, p, cmd, false)?)),
    };
    let targets = ids.iter().map(|id| (*id, mode, text.clone(), style.clone())).collect();
    let done = retext_all(s, "TypeR Change", targets, keep)?;
    let moved = from_script && advance && s.typer.state.next_line(true) == photocraft_typer::Moved::Yes;
    Ok(json!({
        "layers": done.iter().map(|(id, _)| id.0).collect::<Vec<_>>(),
        "bounds": done.iter().map(|(_, i)| i.as_ref().map(rect_json)).collect::<Vec<_>>(),
        "advanced": moved,
        "state": summary(&s.typer.state),
    }))
}

fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.apply";
    let mode = match p.get("mode").and_then(Value::as_str) {
        None => Mode::TextAndStyle,
        Some("textAndStyle") => Mode::TextAndStyle,
        Some("textOnly") => Mode::TextOnly,
        Some("styleOnly") => Mode::StyleOnly,
        Some(_) => return Err(bad(CMD, "\"mode\" must be \"textAndStyle\", \"textOnly\" or \"styleOnly\"")),
    };
    if p.get("mode").is_some_and(|m| !m.is_string()) {
        return Err(bad(CMD, "\"mode\" must be a string"));
    }
    apply_with(s, p, CMD, mode)
}

fn apply_style(s: &mut Session, p: &Value) -> Result<Value> {
    apply_with(s, p, "typer.applyStyle", Mode::StyleOnly)
}

fn apply_lines(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.applyLines";
    let advance = support::bool_param(p, "advance", CMD)?.unwrap_or(true);
    let keep = support::bool_param(p, "keepSize", CMD)?.unwrap_or(false);
    let ids = support::target_layers(s, p, CMD)?;
    // Validate every target before touching any (3.0): all must be unlocked type layers.
    let ids = support::text_targets(s, &ids, CMD, true)?;
    let info = DocInfo::of(&s.active().ok_or(EngineError::NoDocument)?.doc);
    let opts = map_opts(s, &info, p, CMD, true)?;
    let st = &s.typer.state;
    let batch = payload::selected_layers(&ids, st.lines(), st.current_line_index, &st.library, st.current_style_id.as_ref(), st.settings.text_scale);
    if batch.items.is_empty() {
        return Err(typer_error(CMD, "noText"));
    }
    let fallback = chosen_style(s, &Value::Null, CMD)?;
    let used: Vec<(usize, Option<Id>)> = batch.items.iter().map(|i| (i.line_index, i.style.as_ref().map(|s| s.id.clone()))).collect();
    let targets = batch
        .items
        .iter()
        .map(|i| (i.target, Mode::TextAndStyle, Some(i.text.clone()), Some((i.style.clone().unwrap_or_else(|| fallback.clone()), opts))))
        .collect();
    let done = retext_all(s, "TypeR Change", targets, keep)?;
    if advance {
        let state = &mut s.typer.state;
        for (line, style) in used {
            if let (Some(style), Some(raw)) = (style, state.lines().get(line).map(|l| l.raw_text.clone())) {
                state.used_line_styles.insert(line, photocraft_typer::UsedLineStyle { raw_text: raw, style_id: style });
            }
        }
        state.set_line(batch.next_line_index);
    }
    Ok(json!({
        "mode": "applyLines",
        "layers": done.iter().map(|(id, _)| id.0).collect::<Vec<_>>(),
        "lines": batch.items.iter().map(|i| i.line_index).collect::<Vec<_>>(),
        "state": summary(&s.typer.state),
    }))
}

fn align(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.align";
    let tolerance = support::num_param(p, "tolerance", CMD, 0.0, 255.0)?.map_or(bubble::WAND_TOLERANCE, |t| t as f32);
    let resize = support::bool_param(p, "resizeBox", CMD)?.unwrap_or(s.typer.state.settings.resize_text_box_on_center);
    let padding = support::num_param(p, "padding", CMD, 0.0, 1000.0)?.unwrap_or(s.typer.state.settings.internal_padding);
    let ids = support::target_layers(s, p, CMD)?;
    let ids = support::text_targets(s, &ids, CMD, false)?;
    let doc = std::sync::Arc::clone(&s.active().ok_or(EngineError::NoDocument)?.doc);
    // The first layer goes into the user's selection; the others (or all, without one) into the
    // bubble the wand finds just left of their text.
    let mut user: std::result::Result<Option<Opened>, String> = match selection_mask(&doc).map(|m| bubble::opened_bounds(&m)) {
        None => Ok(None),
        Some(None) => Err("noSelection".into()),
        Some(Some(o)) if o.opened.area() < bubble::MIN_AREA => Err("smallSelection".into()),
        Some(Some(o)) => Ok(Some(o)),
    };
    let mut page = None;
    let mut plan: Vec<(LayerId, std::result::Result<Opened, String>)> = Vec::new();
    for id in &ids {
        let Some(ink) = support::layer_ink(&doc, *id) else {
            plan.push((*id, Err("noText".into())));
            continue;
        };
        let target = match std::mem::replace(&mut user, Ok(None)) {
            Ok(Some(o)) => Ok(o),
            Err(e) => Err(e),
            Ok(None) => {
                if page.is_none() {
                    page = Some(page_image(s)?);
                }
                match &page {
                    Some(pg) => support::bubble_for_ink(pg, &ink, support::layer_size_px(&doc, *id), tolerance).map_err(str::to_string),
                    None => Err("noSelection".into()),
                }
            }
        };
        plan.push((*id, target));
    }
    if plan.iter().all(|(_, r)| r.is_err()) {
        let first = plan.into_iter().find_map(|(_, r)| r.err()).unwrap_or_else(|| "noSelection".into());
        return Err(typer_error(CMD, &first));
    }
    let (done, damage) = s.edit("TypeR Align", |doc, _| {
        let info = DocInfo::of(doc);
        let mut done = Vec::new();
        let mut damage = Some(photocraft_geom::Rect::EMPTY);
        for (id, target) in &plan {
            let Ok(o) = target else { continue };
            let l = doc.layer_mut(*id).ok_or(EngineError::NoLayer(*id))?;
            let LayerContent::Text(t) = &mut l.content else { continue };
            let before = t.cache.as_ref().map(|c| c.tile_bounds());
            let mut fit = Fit::Keep;
            if resize && let TextShape::Box { x, y, .. } = t.shape {
                let (bw, bh) = place::box_size(&o.opened, padding);
                t.shape = TextShape::Box { x, y, width: bw as f32, height: bh as f32 };
                fit = Fit::Height;
            }
            let ink = support::fit_and_center(t, fit, o.opened.center(), info);
            let after = support::damage_of(l);
            damage = union(union(damage, before.map(|b| b.inflate(64))), after);
            done.push((*id, o.opened, ink));
        }
        // TypeR deselects after aligning.
        doc.selection = None;
        Ok((done, damage))
    })?;
    set_damage(s, damage);
    Ok(json!({
        "layers": done.iter().map(|(id, _, _)| id.0).collect::<Vec<_>>(),
        "aligned": done.iter().map(|(id, o, i)| json!({"layer": id.0, "bubble": rect_json(o), "center": o.center(), "bounds": i.as_ref().map(rect_json)})).collect::<Vec<_>>(),
        "errors": plan.iter().filter_map(|(id, r)| r.as_ref().err().map(|e| json!({"layer": id.0, "error": e}))).collect::<Vec<_>>(),
    }))
}

fn resize(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.resize";
    let delta = support::num_param(p, "delta", CMD, -1000.0, 1000.0)?
        .filter(|d| *d != 0.0)
        .ok_or_else(|| bad(CMD, "\"delta\" must be a non-zero number of pixels (or points with \"unit\": \"pt\")"))?;
    let unit_pt = match p.get("unit") {
        None | Some(Value::Null) => false,
        Some(Value::String(u)) if u == "px" => false,
        Some(Value::String(u)) if u == "pt" => true,
        Some(_) => return Err(bad(CMD, "\"unit\" must be \"px\" or \"pt\"")),
    };
    let ids = support::target_layers(s, p, CMD)?;
    let ids = support::text_targets(s, &ids, CMD, false)?;
    let (done, damage) = s.edit("TypeR Resize", |doc, _| {
        let info = DocInfo::of(doc);
        let d_pt = if unit_pt { delta } else { place::px_to_pt(delta, f64::from(info.dpi)) } as f32;
        let (lo, hi) = (crate::type_cmds::TYPE_SIZE_MIN_PT as f32, crate::type_cmds::TYPE_SIZE_MAX_PT as f32);
        let mut done = Vec::new();
        let mut damage = Some(photocraft_geom::Rect::EMPTY);
        for id in &ids {
            let l = doc.layer_mut(*id).ok_or(EngineError::NoLayer(*id))?;
            let LayerContent::Text(t) = &mut l.content else { continue };
            let before = t.cache.as_ref().map(|c| c.tile_bounds());
            let old_ink = support::ink(t).or_else(|| support::measure(t, info.dpi, info.format));
            let center = old_ink.map_or((t.transform.m[4], t.transform.m[5]), |i| i.center());
            let mut runs = t.char_runs();
            let old = runs.first().map_or(t.size_pt, |r| r.style.size_pt);
            for r in &mut runs {
                r.style.size_pt = (r.style.size_pt + d_pt).clamp(lo, hi);
                // Fixed leading follows the size; auto leading stays auto.
                if let Some(l) = r.style.leading_pt {
                    r.style.leading_pt = Some((l + d_pt).clamp(0.01, 5000.0));
                }
            }
            let new = runs.first().map_or(old, |r| r.style.size_pt);
            t.runs = runs;
            t.sync_summary();
            if let TextShape::Box { x, y, width, height } = t.shape {
                let k = place::size_ratio(f64::from(old), f64::from(new)) as f32;
                t.shape = TextShape::Box { x, y, width: (width * k).max(1.0), height: (height * k).max(1.0) };
            }
            let ink = support::fit_and_center(t, Fit::Keep, center, info);
            damage = union(union(damage, before.map(|b| b.inflate(64))), support::damage_of(l));
            done.push((*id, new, ink));
        }
        Ok((done, damage))
    })?;
    set_damage(s, damage);
    Ok(json!({
        "layers": done.iter().map(|(id, _, _)| id.0).collect::<Vec<_>>(),
        "sizes": done.iter().map(|(_, sz, _)| sz).collect::<Vec<_>>(),
    }))
}

fn apply_shape(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.applyShape";
    let lines = p.get("lines").and_then(Value::as_array).ok_or_else(|| bad(CMD, "\"lines\" must be a list of strings"))?;
    if lines.is_empty() || lines.len() > 1000 {
        return Err(bad(CMD, "\"lines\" must hold 1..=1000 lines"));
    }
    let lines: Vec<&str> = lines.iter().map(|l| l.as_str().ok_or_else(|| bad(CMD, "\"lines\" must be a list of strings"))).collect::<Result<_>>()?;
    let text = lines.join("\n");
    if text.len() > MAX_TEXT_BYTES || text.trim().is_empty() {
        return Err(typer_error(CMD, "noText"));
    }
    let id = match p.get("layer") {
        None | Some(Value::Null) => s.active().and_then(|d| d.active_layer).ok_or_else(|| typer_error(CMD, "noTextLayer"))?,
        Some(v) => LayerId(v.as_u64().ok_or_else(|| bad(CMD, "\"layer\" must be a layer id"))?),
    };
    let id = *support::text_targets(s, &[id], CMD, true)?.first().ok_or_else(|| typer_error(CMD, "noTextLayer"))?;
    let (ink, damage) = s.edit("TypeR Change", |doc, _| {
        let info = DocInfo::of(doc);
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let LayerContent::Text(t) = &mut l.content else { return Err(typer_error(CMD, "noTextLayer")) };
        let before = t.cache.as_ref().map(|c| c.tile_bounds());
        let old_ink = support::ink(t).or_else(|| support::measure(t, info.dpi, info.format));
        let center = old_ink.map_or((t.transform.m[4], t.transform.m[5]), |i| i.center());
        let base = t.char_runs().first().map(|r| r.style.clone()).unwrap_or_default();
        let para = support::safe_paragraph(t);
        t.runs = vec![TextRun { len: text.len(), style: base }];
        t.paragraphs = vec![ParagraphRun { len: text.len(), style: para }];
        t.text = text;
        t.sync_summary();
        let fit = if matches!(t.shape, TextShape::Point) { Fit::Keep } else { Fit::Both };
        let ink = support::fit_and_center(t, fit, center, info);
        let damage = union(before.map(|b| b.inflate(64)), support::damage_of(l));
        Ok((ink, damage))
    })?;
    set_damage(s, damage);
    Ok(json!({"layer": id.0, "bounds": ink.as_ref().map(rect_json)}))
}

/// Visible layers TypeR counts as cleaning or redraw: not type, group or adjustment layers, with
/// every enclosing group visible, except the bottom layer (the page scan).
fn cleaning_layers(doc: &photocraft_doc::Document) -> Vec<LayerId> {
    fn walk(layers: &[Layer], out: &mut Vec<LayerId>, depth: usize) {
        if depth > 512 {
            return;
        }
        for l in layers {
            if !l.visible {
                continue;
            }
            match &l.content {
                LayerContent::Group(_) => {
                    if let Some(ch) = l.children() {
                        walk(ch, out, depth + 1);
                    }
                }
                LayerContent::Text(_) | LayerContent::Adjustment(_) => {}
                _ => out.push(l.id),
            }
        }
    }
    let mut out = Vec::new();
    let bottom = doc.layers.first().filter(|l| l.children().is_none()).map(|l| l.id);
    walk(&doc.layers, &mut out, 0);
    out.retain(|id| Some(*id) != bottom);
    out
}

fn toggle_cleaning_layers(s: &mut Session, _: &Value) -> Result<Value> {
    let doc_id = s.active().ok_or(EngineError::NoDocument)?.doc.id;
    if let Some(ids) = s.typer.place.hidden_cleaning.remove(&doc_id) {
        let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
        // Layers deleted while hidden are skipped.
        let ids: Vec<LayerId> = ids.into_iter().filter(|id| doc.layer(*id).is_some_and(|l| !l.visible)).collect();
        if !ids.is_empty() {
            s.edit("TypeR Show Cleaning Layers", |doc, _| {
                for id in &ids {
                    if let Some(l) = doc.layer_mut(*id) {
                        l.visible = true;
                    }
                }
                Ok(())
            })?;
        }
        return Ok(json!({"hidden": false, "count": ids.len(), "layers": ids.iter().map(|i| i.0).collect::<Vec<_>>()}));
    }
    let ids = cleaning_layers(&s.active().ok_or(EngineError::NoDocument)?.doc);
    if !ids.is_empty() {
        s.edit("TypeR Hide Cleaning Layers", |doc, _| {
            for id in &ids {
                if let Some(l) = doc.layer_mut(*id) {
                    l.visible = false;
                }
            }
            Ok(())
        })?;
    }
    s.typer.place.hidden_cleaning.insert(doc_id, ids.clone());
    Ok(json!({"hidden": true, "count": ids.len(), "layers": ids.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

fn stored_json(s: &Session) -> Value {
    json!(s.typer.stored_selections)
}

fn mb_toggle(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.multiBubble.toggle";
    let on = support::bool_param(p, "on", CMD)?.unwrap_or(!s.typer.state.settings.multi_bubble_mode);
    s.typer.state.edit_settings(|st| st.multi_bubble_mode = on);
    if !on {
        s.typer.stored_selections.clear();
    }
    Ok(json!({"on": on, "stored": s.typer.stored_selections.len()}))
}

fn mb_capture(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.multiBubble.capture";
    let advance = support::bool_param(p, "advance", CMD)?.unwrap_or(true);
    let (bounds, opened) = match support::rect_param(p, "bounds", CMD)? {
        Some(b) => (b, None),
        None => {
            let doc = &s.active().ok_or_else(|| typer_error(CMD, "noSelection"))?.doc;
            let mask = selection_mask(doc).ok_or_else(|| typer_error(CMD, "noSelection"))?;
            let o = bubble::opened_bounds(&mask).ok_or_else(|| typer_error(CMD, "noSelection"))?;
            (o.bbox, Some(o.opened))
        }
    };
    let key = bounds.key();
    if s.typer.stored_selections.iter().any(|x| x.bounds.key() == key) {
        return Ok(json!({"added": false, "stored": s.typer.stored_selections.len(), "state": summary(&s.typer.state)}));
    }
    if s.typer.stored_selections.len() >= MAX_LAYERS {
        return Err(bad(CMD, format!("at most {MAX_LAYERS} stored bubbles")));
    }
    let st = &s.typer.state;
    let line_index = st.current_line().map(|_| st.current_line_index);
    s.typer.stored_selections.push(StoredSelection { bounds, opened, line_index, style_id: st.current_style_id.clone() });
    if advance && line_index.is_some() {
        s.typer.state.next_line(true);
    }
    Ok(json!({"added": true, "stored": s.typer.stored_selections.len(), "selections": stored_json(s), "state": summary(&s.typer.state)}))
}

fn mb_remove_last(s: &mut Session, _: &Value) -> Result<Value> {
    let removed = s.typer.stored_selections.pop();
    if let Some(r) = &removed {
        let st = &mut s.typer.state;
        let line = payload::stored_selection_line(r.line_index, st.current_line_index, st.lines());
        if r.line_index.is_some() {
            st.set_line(line);
        }
    }
    Ok(json!({"removed": removed.is_some(), "stored": s.typer.stored_selections.len(), "state": summary(&s.typer.state)}))
}

fn mb_clear(s: &mut Session, _: &Value) -> Result<Value> {
    s.typer.stored_selections.clear();
    Ok(json!({"stored": 0}))
}

/// "Copy layer style" (L4): a TypeR style from a type layer's first run and paragraph, and its
/// stroke.
fn style_from_layer(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.style.fromLayer";
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let id = match p.get("layer") {
        None | Some(Value::Null) => st.active_layer.ok_or_else(|| typer_error(CMD, "noTextLayer"))?,
        Some(v) => LayerId(v.as_u64().ok_or_else(|| bad(CMD, "\"layer\" must be a layer id"))?),
    };
    let l = st.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    let LayerContent::Text(t) = &l.content else { return Err(typer_error(CMD, "noTextLayer")) };
    let c = t.char_runs().first().map(|r| r.style.clone()).unwrap_or_default();
    let para = t.paragraph_runs().first().map(|r| r.style.clone()).unwrap_or_default();
    let props = psjson::text_props_from(&c, &para, t.antialias, f64::from(st.doc.resolution_dpi));
    let stroke = l
        .effects
        .items
        .iter()
        .find_map(|e| match e {
            Effect::Stroke(sf) if sf.common.enabled => {
                let rgb = match &sf.paint {
                    FxPaint::Color(col) => col.to_rgb(),
                    _ => [0.0, 0.0, 0.0],
                };
                let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round();
                Some(json!({"enabled": true, "size": sf.size, "opacity": (sf.common.opacity * 100.0).round(), "position": "outer",
                    "color": {"r": ch(rgb[0]), "g": ch(rgb[1]), "b": ch(rgb[2])}}))
            }
            _ => None,
        })
        .unwrap_or_else(photocraft_typer::library::default_stroke);
    let layer_name = l.name.clone();
    let name = support::text_param(p, "name", CMD)?.filter(|n| !n.trim().is_empty()).unwrap_or(layer_name);
    let folder = match p.get("folder") {
        None | Some(Value::Null) => None,
        Some(v) => Some(Id::from_value(v).ok_or_else(|| bad(CMD, "\"folder\" must be a folder id"))?),
    };
    let lib = &s.typer.state.library;
    if let Some(f) = &folder
        && lib.folder(f).is_none()
    {
        return Err(bad(CMD, format!("no folder {f}")));
    }
    let existing = match p.get("id") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let sid = Id::from_value(v).ok_or_else(|| bad(CMD, "\"id\" must be a style id"))?;
            Some(lib.style(&sid).cloned().ok_or_else(|| bad(CMD, format!("no style {sid}")))?)
        }
    };
    let mut style = match existing {
        Some(mut e) => {
            if p.get("name").is_some() {
                e.name = name;
            }
            e
        }
        None => {
            let mut e = Style::new(lib.fresh_id("s"), &name);
            e.folder = folder.clone().or_else(|| s.typer.state.current_style().and_then(|c| c.folder.clone()));
            e
        }
    };
    if folder.is_some() {
        style.folder = folder;
    }
    style.props.insert("textProps".into(), props);
    style.props.insert("stroke".into(), stroke);
    if let Some(now) = p.get("now").and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0).and_then(serde_json::Number::from_f64) {
        style.edited = Some(now);
    }
    let out = style.to_json();
    let id = s.typer.state.edit_library(|l| l.save_style(style));
    Ok(json!({"id": id, "style": out}))
}

pub fn specs() -> Vec<CommandSpec> {
    let edit = |id: &'static str, label: &'static str, params: &'static str, run: fn(&mut Session, &Value) -> Result<Value>| CommandSpec {
        id,
        label,
        menu: &[],
        shortcut: None,
        params,
        enabled: has_doc,
        run,
        journal: true,
    };
    let session = |id: &'static str, label: &'static str, params: &'static str, run: fn(&mut Session, &Value) -> Result<Value>| CommandSpec {
        id,
        label,
        menu: &[],
        shortcut: None,
        params,
        enabled: always,
        run,
        journal: false,
    };
    vec![
        // P1 + P7: style the current line into the selected bubble, centred on its opened shape.
        edit(
            "typer.paste",
            "TypeR Paste",
            r#"{"text":string? (default: the current line; then the cursor advances),"style":{"id":id}|TypeR style JSON? (default: the current style),"point":bool? (default: the style's layer kind, else the setting),"padding":px? (default: the setting),"direction":"ltr"|"rtl"?,"keepSize":bool? (the active type layer's size),"bounds":[x,y,w,h]? (instead of the selection, used as is),"advance":bool=true} → {mode,layer,bounds,bubble,center,point,advanced,state}. Without text, bounds or style it acts as TypeR's Paste key: multi paste when bubbles are stored in multi-bubble mode, applyLines when 2+ type layers are selected and nothing is selected"#,
            paste,
        ),
        // M2: fill every stored multi-bubble selection (one history step).
        edit(
            "typer.pasteMany",
            "TypeR Paste Many",
            r#"{"entries":[{"text":string,"style":{"id":id}|style JSON?,"bounds":[x,y,w,h]}]? (default: the stored bubbles with their lines; cleared afterwards),"point":bool?,"padding":px?,"keepSize":bool?,"direction":"ltr"|"rtl"?} → {mode,layers,bounds,state}"#,
            paste_many,
        ),
        // P2/P3/P4: replace text and/or style of the selected type layers, keeping their centre.
        edit(
            "typer.apply",
            "TypeR Apply",
            r#"{"text":string? (default: the current line; then the cursor advances),"style":{"id":id}|style JSON?,"keepSize":bool?,"direction":"ltr"|"rtl"?,"mode":"textAndStyle"|"textOnly"|"styleOnly"="textAndStyle","layers":[id]? (default: the selected layers),"advance":bool=true} → {layers,bounds,advanced,state}"#,
            apply,
        ),
        // P4: restyle the selected type layers, text unchanged.
        edit(
            "typer.applyStyle",
            "TypeR Apply Style",
            r#"{"style":{"id":id}|style JSON? (default: the current style),"keepSize":bool?,"direction":"ltr"|"rtl"?,"layers":[id]?} → {layers,bounds}"#,
            apply_style,
        ),
        // P9: one line per selected type layer, in click order; nothing changes on error.
        edit(
            "typer.applyLines",
            "TypeR Apply Lines",
            r#"{"layers":[id]? (default: the selected layers in click order),"keepSize":bool?,"advance":bool=true} → {mode,layers,lines,state}"#,
            apply_lines,
        ),
        // P5 + D1 + P7: centre type layers in the selected (or detected) bubble.
        edit(
            "typer.align",
            "TypeR Align",
            r#"{"resizeBox":bool? (default: the setting),"padding":px?,"layers":[id]? (default: the selected layers),"tolerance":0..255=20} → {layers,aligned:[{layer,bubble,center,bounds}],errors}. The first layer goes into the selection; the others, or all without one, into the bubble the wand finds just left of their text. Deselects"#,
            align,
        ),
        // P6: size −/+ keeping the centre; fixed leading and the box follow.
        edit("typer.resize", "TypeR Resize Text", r#"{"delta":number (pixels; ±1000),"unit":"px"|"pt"="px","layers":[id]?} → {layers,sizes}"#, resize),
        // D4 apply: set the text with line breaks, shrink the box to it, keep the centre.
        edit("typer.applyShape", "TypeR Apply Shape", r#"{"layer":id? (default: the active layer),"lines":[string]} → {layer,bounds}"#, apply_shape),
        // P10: hide/restore the non-text pixel layers (per document).
        edit("typer.toggleCleaningLayers", "TypeR Toggle Cleaning Layers", "{} → {hidden,count,layers}", toggle_cleaning_layers),
        // M1/M3: multi-bubble capture.
        session(
            "typer.multiBubble.toggle",
            "TypeR Multi-Bubble Mode",
            r#"{"on":bool? (default: flip)} → {on,stored} (off clears the stored bubbles)"#,
            mb_toggle,
        ),
        session(
            "typer.multiBubble.capture",
            "TypeR Capture Bubble",
            r#"{"bounds":[x,y,w,h]? (default: the selection),"advance":bool=true} → {added,stored,selections,state} (stores the bubble with the current line and style, then advances; a bubble already stored is skipped)"#,
            mb_capture,
        ),
        session("typer.multiBubble.removeLast", "TypeR Remove Last Bubble", "{} → {removed,stored,state} (the cursor goes back to its line)", mb_remove_last),
        session("typer.multiBubble.clear", "TypeR Clear Bubbles", "{} → {stored}", mb_clear),
        // L4 "Copy layer style": a style from a type layer.
        CommandSpec {
            id: "typer.style.fromLayer",
            label: "TypeR Style From Layer",
            menu: &[],
            shortcut: None,
            params: r#"{"layer":id? (default: the active layer),"id":id? (update this style instead of adding one),"name":string? (default: the layer name),"folder":id?,"now":ms?} → {id,style}"#,
            enabled: has_doc,
            run: style_from_layer,
            journal: false,
        },
    ]
}
