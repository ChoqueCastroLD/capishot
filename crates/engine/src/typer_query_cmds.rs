//! TypeR bubble and line-shaping queries (WP1 and WP4 own this file; see `docs/typer.md`).
//!
//! Pure queries (`journal: false`) that never touch the selection or history. WP0 registers the
//! contract; each reports "not implemented yet" until its work package lands it.

use crate::commands::CommandSpec;
use crate::typer_place_cmds::{not_yet, wp1};

fn wp4(_: &crate::Session) -> std::result::Result<(), String> {
    Err("not implemented yet (TypeR line shaping, work package WP4)".into())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        // D1-D3: wand at tolerance 20 on the composite, then the adaptive opening (P7).
        CommandSpec {
            id: "typer.detectBubble",
            label: "TypeR Detect Bubble",
            menu: &[],
            shortcut: None,
            params: r#"{"x":px,"y":px}|{"layer":id},"tolerance":0..255=20,"rows":5..31=21 → {found,bbox,opened,center,rows:[{y,left,right,width}],area} (not implemented yet: WP1)"#,
            enabled: wp1,
            run: not_yet,
            journal: false,
        },
        CommandSpec {
            id: "typer.selectionBounds",
            label: "TypeR Selection Bounds",
            menu: &[],
            shortcut: None,
            params: "{} → {bbox,opened,center,area} of the current selection (not implemented yet: WP1)",
            enabled: wp1,
            run: not_yet,
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
