//! TypeR placement commands (WP1 owns this file; see `docs/typer.md`).
//!
//! WP0 registers the command contract of the port plan (§3.4) so the UI (WP3), the CLI and MCP
//! can be built against it; every command reports "not implemented yet" until WP1 lands it.
//! Each finished command must be one `Session::edit` (one history step), `journal: true`, and
//! advance the cursor with `State::next_line(true)` on success when `advance` is on.

use serde_json::Value;

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

pub(crate) fn wp1(_: &Session) -> std::result::Result<(), String> {
    Err("not implemented yet (TypeR placement, work package WP1)".into())
}

/// Never reached while `enabled` refuses; kept so a direct call still errors cleanly.
pub(crate) fn not_yet(_: &mut Session, _: &Value) -> Result<Value> {
    Err(EngineError::Other("not implemented yet (TypeR work package WP1)".into()))
}

pub fn specs() -> Vec<CommandSpec> {
    let stub = |id: &'static str, label: &'static str, params: &'static str| CommandSpec {
        id,
        label,
        menu: &[],
        shortcut: None,
        params,
        enabled: wp1,
        run: not_yet,
        journal: true,
    };
    vec![
        // P1 + P7: style the current line into the selected bubble, centred on its opened shape.
        stub(
            "typer.paste",
            "TypeR Paste",
            r#"{"text":string?,"style":{"id":id}|style JSON?,"point":bool?,"padding":px?,"direction":"ltr"|"rtl"?,"keepSize":bool?,"bounds":[x,y,w,h]? (instead of the selection),"advance":bool=true} → {layer,bounds,nextLine} (not implemented yet: WP1)"#,
        ),
        // M2: fill every stored multi-bubble selection (one history step).
        stub(
            "typer.pasteMany",
            "TypeR Paste Many",
            r#"{"entries":[{text,style,bounds}]? (default: the stored selections),"point":bool?,"padding":px?} → {layers} (not implemented yet: WP1)"#,
        ),
        // P2/P3/P4: replace text and/or style of the selected text layers, keeping their centre.
        stub(
            "typer.apply",
            "TypeR Apply",
            r#"{"text":string?,"style":{"id":id}?,"keepSize":bool?,"direction":"ltr"|"rtl"?,"mode":"textAndStyle"|"textOnly"|"styleOnly"="textAndStyle","layers":[id]?,"advance":bool=true} → {layers,nextLine} (not implemented yet: WP1)"#,
        ),
        // P9: one line per selected text layer, in click order; rolls back on error.
        stub("typer.applyLines", "TypeR Apply Lines", r#"{"layers":[id]? (click order),"advance":bool=true} → {layers,nextLine} (not implemented yet: WP1)"#),
        // P5 + D1 + P7: centre the active text layer in the selected (or auto-detected) bubble.
        stub(
            "typer.align",
            "TypeR Align",
            r#"{"resizeBox":bool?,"padding":px?,"layers":[id]?,"tolerance":0..255=20} → {layers,bounds} (not implemented yet: WP1)"#,
        ),
        // P6: size −/+ keeping the centre; fixed leading follows.
        stub("typer.resize", "TypeR Resize Text", r#"{"delta":number (in the layer's type unit),"layers":[id]?} → {layers} (not implemented yet: WP1)"#),
        // D4 apply: set the text with line breaks, refit the box, keep the centre.
        stub("typer.applyShape", "TypeR Apply Shape", r#"{"layer":id,"lines":[string]} → {layer} (not implemented yet: WP1/WP4)"#),
        // P10: hide/restore the non-text pixel layers (per document).
        stub("typer.toggleCleaningLayers", "TypeR Toggle Cleaning Layers", "{} → {hidden} (not implemented yet: WP1)"),
        // M1/M3: multi-bubble capture.
        stub("typer.multiBubble.toggle", "TypeR Multi-Bubble Mode", "{} → {on,stored} (not implemented yet: WP1)"),
        stub(
            "typer.multiBubble.capture",
            "TypeR Capture Bubble",
            r#"{"bounds":[x,y,w,h]? (default: the selection's opened bounds)} → {stored,nextLine} (not implemented yet: WP1)"#,
        ),
        stub("typer.multiBubble.removeLast", "TypeR Remove Last Bubble", "{} → {stored,line} (not implemented yet: WP1)"),
        stub("typer.multiBubble.clear", "TypeR Clear Bubbles", "{} → {stored} (not implemented yet: WP1)"),
        // L4 "Copy layer style": a style from the active text layer (psjson::text_props_from).
        stub("typer.style.fromLayer", "TypeR Style From Layer", r#"{"layer":id?,"name":string?,"folder":id?} → {id} (not implemented yet: WP1/WP2)"#),
    ]
}
