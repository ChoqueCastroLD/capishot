//! Photoshop shortcut parity: every default shortcut in Adobe's published list
//! (`shortcut_parity/photoshop_defaults.tsv`, which names its source) is pressed through the real
//! keyboard handler ([`crate::shortcuts::handle`]) and must do what it does in Photoshop.
//!
//! Rows Capishot can't match yet are listed in [`KNOWN_GAPS`] with the reason; the test fails if a
//! row outside that list mismatches, and also when a listed gap starts to match (so the list only
//! ever shrinks). `cargo test -p photocraft-ui-egui shortcut_parity -- --nocapture` prints the
//! coverage.

use egui::{Event, Key, KeyboardShortcut, Modifiers, RawInput};
use photocraft_color::BlendMode;
use serde_json::json;

use crate::PhotocraftApp;
use crate::shortcut_dispatch::{Outcome, set_dry_run, take_log};
use crate::shortcuts::parse;
use crate::state::Tool;

const TABLE: &str = include_str!("shortcut_parity/photoshop_defaults.tsv");

/// Rows (by action) that don't match yet, and why. Keep it short: fix rows rather than list them.
const KNOWN_GAPS: &[(&str, &str)] = &[
    ("Frame", "no Frame tool"),
    ("Behind (painting tools)", "the brush has no Behind painting mode"),
    ("Clear (painting tools)", "the brush has no Clear painting mode"),
    ("Next document", "Ctrl+Tab lands with upstream PhotoCraft #2353"),
    ("Previous document", "Ctrl+Shift+Tab lands with upstream PhotoCraft #2353"),
    ("Increase type size", "lands with upstream PhotoCraft #2402"),
    ("Decrease type size", "lands with upstream PhotoCraft #2402"),
];

#[derive(Debug)]
struct Row {
    area: String,
    action: String,
    keys: String,
    target: String,
}

/// Adobe's notation (`Ctrl+Alt+Shift+E`, `Command+Option+Delete`, `Page Up`) as Capishot's
/// (`Cmd+Alt+Shift+E`, `Cmd+Alt+Backspace`, `PageUp`): ⌘ on the Mac and Ctrl elsewhere are `Cmd`.
fn canonical(adobe: &str, mac: bool) -> String {
    let (mods, key) = if let Some(m) = adobe.strip_suffix("++") {
        (m, "+")
    } else {
        match adobe.rsplit_once('+') {
            Some((m, k)) => (m, k),
            None => ("", adobe),
        }
    };
    let mut out: Vec<String> = mods
        .split('+')
        .filter(|m| !m.is_empty())
        .map(|m| match m {
            "Ctrl" if !mac => "Cmd",
            "Command" => "Cmd",
            "Control" | "Ctrl" => "Ctrl",
            "Option" | "Alt" => "Alt",
            "Shift" => "Shift",
            other => panic!("unknown modifier {other} in {adobe}"),
        })
        .map(str::to_string)
        .collect();
    let key = match key {
        "+" => "",
        "Page Up" => "PageUp",
        "Page Down" => "PageDown",
        "Right Arrow" => "ArrowRight",
        "Left Arrow" => "ArrowLeft",
        "Up Arrow" => "ArrowUp",
        "Down Arrow" => "ArrowDown",
        "Esc" => "Escape",
        "Return" => "Enter",
        // The Mac's delete key is Backspace.
        "Delete" if mac => "Backspace",
        k => k,
    };
    out.push(key.to_string());
    out.join("+")
}

fn rows() -> Vec<Row> {
    let mac = cfg!(target_os = "macos");
    TABLE
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 5, "five tab-separated columns: {l}");
            let keys = canonical(if mac { f[3] } else { f[2] }, mac);
            Row { area: f[0].into(), action: f[1].into(), keys, target: f[4].into() }
        })
        .collect()
}

/// The modifiers egui-winit reports for a shortcut's: `Cmd` is ⌘ on the Mac and Ctrl elsewhere.
fn platform(m: Modifiers) -> Modifiers {
    let mut out = Modifiers { alt: m.alt, shift: m.shift, ctrl: m.ctrl, ..Default::default() };
    if m.command {
        out.command = true;
        if cfg!(target_os = "macos") {
            out.mac_cmd = true;
        } else {
            out.ctrl = true;
        }
    }
    out
}

/// The character a US layout reports with ⇧ held, where egui has a key for it (⇧] is `}`).
fn shifted(k: Key) -> Key {
    match k {
        Key::Equals => Key::Plus,
        Key::Semicolon => Key::Colon,
        Key::OpenBracket => Key::OpenCurlyBracket,
        Key::CloseBracket => Key::CloseCurlyBracket,
        Key::Backslash => Key::Pipe,
        Key::Num1 => Key::Exclamationmark,
        k => k,
    }
}

/// One frame with `sc` pressed, through the app's keyboard handler.
fn press(app: &mut PhotocraftApp, ctx: &egui::Context, sc: &KeyboardShortcut, time: f64) {
    let m = platform(sc.modifiers);
    let physical = sc.logical_key;
    let key = if m.shift { shifted(physical) } else { physical };
    let events = vec![Event::ModifiersChanged(m), Event::Key { key, physical_key: Some(physical), pressed: true, repeat: false, modifiers: m }];
    ctx.begin_pass(RawInput { events, time: Some(time), ..Default::default() });
    crate::shortcuts::handle(app, ctx);
    ctx.end_pass().textures_delta.clear();
}

/// A 200 × 160 document with a filled pixel layer "paint" (active) above the Background.
fn app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.run("file.new", json!({"width": 200, "height": 160})).unwrap();
    app.run("layer.new.layer", json!({"name": "paint"})).unwrap();
    app.run("select.rect", json!({"x": 20, "y": 20, "width": 60, "height": 40})).unwrap();
    app.run("edit.fill", json!({"color": "#3070c0"})).unwrap();
    app.run("select.deselect", json!({})).unwrap();
    app.ui.tool = Tool::Move;
    app
}

fn active_layer(app: &PhotocraftApp) -> &photocraft_doc::Layer {
    let st = app.session.active().unwrap();
    st.doc.layer(st.active_layer.unwrap()).unwrap()
}

fn left(app: &PhotocraftApp) -> i32 {
    photocraft_engine::layer_multi_cmds::layer_bounds(active_layer(app)).map_or(i32::MIN, |r| r.x0)
}

fn blend_mode(name: &str) -> Option<BlendMode> {
    serde_json::from_value(json!(name[..1].to_uppercase() + &name[1..])).ok()
}

/// What pressing `row`'s keys did, when it isn't what the table says (`Ok` when it matches).
fn check(row: &Row) -> Result<(), String> {
    let sc = parse(&row.keys).ok_or_else(|| format!("unparsable keys {}", row.keys))?;
    let (kind, arg) = row.target.split_once(':').ok_or("target without a kind")?;
    let mut app = app();
    let ctx = egui::Context::default();
    let mut time = 10.0;
    let mut go = |app: &mut PhotocraftApp| {
        time += 5.0;
        press(app, &ctx, &sc, time);
    };
    match kind {
        "cmd" => {
            set_dry_run(&ctx, true);
            go(&mut app);
            let log = take_log(&ctx);
            match log.last() {
                Some((id, _)) if id == arg || crate::menus::panel_alias(arg) == Some(id.as_str()) => Ok(()),
                Some((id, Outcome::Disabled(why))) => Err(format!("ran {id} (disabled: {why})")),
                Some((id, _)) => Err(format!("ran {id}")),
                None => Err(format!("dispatched nothing (tool {:?})", app.ui.tool)),
            }
        }
        "tool" => {
            let want = Tool::from_name(arg).ok_or_else(|| format!("no tool {arg}"))?;
            app.ui.tool = if want == Tool::Move { Tool::Zoom } else { Tool::Move };
            go(&mut app);
            if app.ui.tool.key() == want.key() { Ok(()) } else { Err(format!("selected {:?}", app.ui.tool)) }
        }
        "cycle" => {
            let from = Tool::from_name(arg).ok_or_else(|| format!("no tool {arg}"))?;
            app.ui.tool = from;
            go(&mut app);
            let to = app.ui.tool;
            if to != from && to.key() == from.key() { Ok(()) } else { Err(format!("{from:?} became {to:?}")) }
        }
        "hold" => {
            use crate::hold_keys::Temporary;
            let t = [Temporary::Hand, Temporary::ZoomIn, Temporary::ZoomOut].into_iter().find(|t| t.id() == arg).ok_or("unknown temporary tool")?;
            match crate::hold_keys::binding(&app, t) {
                Some(b) if b.logical_key == sc.logical_key && b.modifiers == sc.modifiers => Ok(()),
                b => Err(format!("bound to {b:?}")),
            }
        }
        "opacity" | "flow" => {
            app.ui.tool = Tool::Brush;
            app.run("tools.setBrush", json!({"brush": {"opacity": 0.33, "flow": 0.33}})).unwrap();
            go(&mut app);
            let b = &app.session.tools.brush;
            let got = if kind == "flow" { b.flow } else { b.opacity };
            let want: f32 = arg.parse::<f32>().map_err(|e| e.to_string())? / 100.0;
            if (got - want).abs() < 1e-4 { Ok(()) } else { Err(format!("{kind} is {got}")) }
        }
        "blend" => {
            // Start from a mode the press must change (Multiply, or Normal for ⇧⌥M itself).
            let start = if arg == "multiply" { BlendMode::Normal } else { BlendMode::Multiply };
            app.run("layer.setProps", json!({"blend": start})).map_err(|e| e.to_string())?;
            go(&mut app);
            let got = active_layer(&app).blend;
            let modes = BlendMode::LAYER_MODES;
            let at = modes.iter().position(|m| *m == start).ok_or("Multiply not a layer mode")?;
            let want = match arg {
                "next" => modes.get(at + 1).copied(),
                "previous" => at.checked_sub(1).and_then(|i| modes.get(i)).copied(),
                name => blend_mode(name),
            }
            .ok_or_else(|| format!("no blend mode {arg}"))?;
            if got == want { Ok(()) } else { Err(format!("blend is {got:?}")) }
        }
        "brushblend" => {
            app.ui.tool = Tool::Brush;
            let before = serde_json::to_value(&app.session.tools.brush).unwrap_or_default();
            go(&mut app);
            let after = serde_json::to_value(&app.session.tools.brush).unwrap_or_default();
            let mode = after.get("mode").map(ToString::to_string).unwrap_or_default();
            if before != after && mode.to_ascii_lowercase().contains(arg) { Ok(()) } else { Err(format!("brush mode is {mode}")) }
        }
        "transform" => {
            crate::transform_tool::begin(&mut app, &ctx).map_err(|e| format!("no transform: {e}"))?;
            let x0 = left(&app);
            if let Some(t) = app.ui.transform.as_mut() {
                t.quad = t.quad.map(|q| [q[0] + 5.0, q[1]]);
                t.pivot[0] += 5.0;
            }
            go(&mut app);
            let moved = left(&app) - x0;
            match (arg, app.ui.transform.is_none(), moved) {
                ("commit", true, 5) | ("cancel", true, 0) => Ok(()),
                (_, open, moved) => Err(format!("transform open: {}, layer moved {moved} px", !open)),
            }
        }
        "nudge" => {
            let x0 = left(&app);
            go(&mut app);
            let want: i32 = arg.parse().map_err(|_| "bad nudge")?;
            let moved = left(&app) - x0;
            if moved == want { Ok(()) } else { Err(format!("moved {moved} px")) }
        }
        other => Err(format!("unknown target kind {other}")),
    }
}

#[test]
fn table_parses_and_is_consistent_between_platforms() {
    let rows = rows();
    assert!(rows.len() > 150, "{} rows", rows.len());
    for line in TABLE.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
        let f: Vec<&str> = line.split('\t').collect();
        let (win, mac) = (canonical(f[2], false), canonical(f[3], true));
        assert!(parse(&win).is_some() && parse(&mac).is_some(), "{line}");
        // The same key on both platforms, ⌘ for Ctrl, except Photoshop's Control+Tab on the Mac.
        if !f[3].contains("Control") {
            assert_eq!(win, mac, "{line}");
        }
    }
    for (action, _) in KNOWN_GAPS {
        assert!(rows.iter().any(|r| r.action == *action), "known gap {action} is not a row");
    }
}

#[test]
fn photoshop_default_shortcuts_match() {
    let rows = rows();
    let mut matched = 0;
    let mut failures = Vec::new();
    let mut fixed = Vec::new();
    for row in &rows {
        let result = check(row);
        let gap = KNOWN_GAPS.iter().find(|(a, _)| *a == row.action);
        match (result, gap) {
            (Ok(()), None) => matched += 1,
            (Ok(()), Some(_)) => fixed.push(format!("{} › {}", row.area, row.action)),
            (Err(_), Some(_)) => {}
            (Err(e), None) => failures.push(format!("{} › {} ({}, want {}): {e}", row.area, row.action, row.keys, row.target)),
        }
    }
    println!("Photoshop default shortcuts: {matched}/{} match, {} known gaps", rows.len(), KNOWN_GAPS.len());
    assert!(failures.is_empty(), "{} shortcuts don't match Photoshop:\n{}", failures.len(), failures.join("\n"));
    assert!(fixed.is_empty(), "these known gaps match now; remove them from KNOWN_GAPS:\n{}", fixed.join("\n"));
}
