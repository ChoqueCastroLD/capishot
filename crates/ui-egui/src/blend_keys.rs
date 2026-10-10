//! Blending-mode keys, as in Photoshop: ⇧⌥ + a letter picks a mode (⇧⌥M Multiply, ⇧⌥S Screen,
//! ⇧⌥N Normal…) and ⇧+ / ⇧- step to the next or previous mode in menu order. With the Brush or
//! Pencil they set the brush's Mode, with the Gradient its Mode, and with tools that have no mode
//! of their own (Move, the selection tools, Type, shapes…) the active layer's blending mode. With
//! tools whose mode Capishot doesn't keep (the stamps, healing and toning tools) they do nothing.
//!
//! Like the opacity number keys (`opacity_keys`) these are key handling, not commands: each press
//! runs the command its target already has (`tools.setBrush`, `layer.setProps`).

use egui::{Event, Key};
use photocraft_color::BlendMode;
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;

/// Photoshop's ⇧⌥ letters for the modes Capishot has (Behind and Clear are painting-only modes it
/// lacks; Darker Color, Lighter Color, Subtract and Divide have no letter).
const LETTERS: [(Key, BlendMode); 23] = [
    (Key::N, BlendMode::Normal),
    (Key::I, BlendMode::Dissolve),
    (Key::K, BlendMode::Darken),
    (Key::M, BlendMode::Multiply),
    (Key::B, BlendMode::ColorBurn),
    (Key::A, BlendMode::LinearBurn),
    (Key::G, BlendMode::Lighten),
    (Key::S, BlendMode::Screen),
    (Key::D, BlendMode::ColorDodge),
    (Key::W, BlendMode::LinearDodge),
    (Key::O, BlendMode::Overlay),
    (Key::F, BlendMode::SoftLight),
    (Key::H, BlendMode::HardLight),
    (Key::V, BlendMode::VividLight),
    (Key::J, BlendMode::LinearLight),
    (Key::Z, BlendMode::PinLight),
    (Key::L, BlendMode::HardMix),
    (Key::E, BlendMode::Difference),
    (Key::X, BlendMode::Exclusion),
    (Key::U, BlendMode::Hue),
    (Key::T, BlendMode::Saturation),
    (Key::C, BlendMode::Color),
    (Key::Y, BlendMode::Luminosity),
];

/// What a press asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Change {
    Set(BlendMode),
    Step(i32),
}

/// Whose mode the keys change.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Brush,
    Gradient,
    Layer,
    None,
}

fn target(tool: Tool) -> Target {
    match tool {
        Tool::Brush | Tool::Pencil => Target::Brush,
        Tool::Gradient => Target::Gradient,
        t if t.is_brushlike() || matches!(t, Tool::PaintBucket | Tool::Dodge | Tool::Burn | Tool::Sponge | Tool::MagicEraser) => Target::None,
        _ => Target::Layer,
    }
}

/// The change a key press stands for. The physical key decides the letter: ⌥ + a letter on a Mac
/// types a symbol (⌥M is µ), so the logical key is not the letter.
fn change(key: Key, physical: Option<Key>, m: egui::Modifiers) -> Option<Change> {
    if m.command || m.ctrl || m.mac_cmd || !m.shift {
        return None;
    }
    if m.alt {
        let k = physical.unwrap_or(key);
        return LETTERS.iter().find(|(l, _)| *l == k).map(|(_, mode)| Change::Set(*mode));
    }
    match (physical.unwrap_or(key), key) {
        (Key::Equals | Key::Plus, _) | (_, Key::Plus) => Some(Change::Step(1)),
        (Key::Minus, _) | (_, Key::Minus) => Some(Change::Step(-1)),
        _ => None,
    }
}

/// `mode` after `change`; stepping stops at either end of the menu, as in Photoshop.
fn apply(mode: BlendMode, c: Change) -> BlendMode {
    match c {
        Change::Set(m) => m,
        Change::Step(d) => {
            let modes = BlendMode::LAYER_MODES;
            let at = modes.iter().position(|m| *m == mode).unwrap_or(0);
            let to = (at as i64 + i64::from(d)).clamp(0, modes.len() as i64 - 1);
            usize::try_from(to).ok().and_then(|i| modes.get(i).copied()).unwrap_or(mode)
        }
    }
}

/// Consumes this frame's blending-mode key presses and applies them. True when one was used.
pub fn handle(app: &mut PhotocraftApp, ctx: &egui::Context) -> bool {
    let to = target(app.ui.tool);
    // Consumed even when the tool has no mode to set, so ⇧⌥M never reaches the tool keys (M).
    let presses: Vec<Change> = ctx.input_mut(|i| {
        let mut out = Vec::new();
        i.events.retain(|e| match e {
            Event::Key { key, physical_key, pressed: true, modifiers, .. } => match change(*key, *physical_key, *modifiers) {
                Some(c) => {
                    out.push(c);
                    false
                }
                None => true,
            },
            _ => true,
        });
        out
    });
    for c in &presses {
        set(app, to, *c);
    }
    !presses.is_empty()
}

fn set(app: &mut PhotocraftApp, to: Target, c: Change) {
    let r = match to {
        Target::Brush => {
            let mode = apply(app.session.tools.brush.mode, c);
            app.run("tools.setBrush", json!({ "brush": { "mode": mode } })).map(|_| ())
        }
        Target::Gradient => {
            app.ui.tool_options.gradient_blend_mode = apply(app.ui.tool_options.gradient_blend_mode, c);
            Ok(())
        }
        Target::Layer => {
            let Some(st) = app.session.active() else { return };
            let Some(layer) = st.active_layer.and_then(|id| st.doc.layer(id)) else { return };
            // The locked Background has no blending mode; a group's Pass Through steps to Normal.
            if crate::doc_props_ui::is_background(&st.doc, layer) {
                return;
            }
            let (id, mode) = (layer.id.0, apply(layer.blend, c));
            if mode == layer.blend {
                return;
            }
            app.run("layer.setProps", json!({ "layer": id, "blend": mode })).map(|_| ())
        }
        Target::None => Ok(()),
    };
    if let Err(e) = r {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Modifiers;

    fn app(tool: Tool) -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 32, "height": 32})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        app.ui.tool = tool;
        app
    }

    fn press(app: &mut PhotocraftApp, key: Key, physical: Key, modifiers: Modifiers) -> bool {
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput {
            events: vec![Event::Key { key, physical_key: Some(physical), pressed: true, repeat: false, modifiers }],
            ..Default::default()
        });
        let used = handle(app, &ctx);
        ctx.end_pass().textures_delta.clear();
        used
    }

    fn layer_blend(app: &PhotocraftApp) -> BlendMode {
        let st = app.session.active().unwrap();
        st.doc.layer(st.active_layer.unwrap()).unwrap().blend
    }

    #[test]
    fn shift_alt_letters_set_the_layers_mode_with_the_move_tool() {
        let mut app = app(Tool::Move);
        let sa = Modifiers::SHIFT | Modifiers::ALT;
        assert!(press(&mut app, Key::M, Key::M, sa));
        assert_eq!(layer_blend(&app), BlendMode::Multiply);
        // A Mac's ⌥M types µ: the physical key still says M.
        assert!(press(&mut app, Key::Questionmark, Key::S, sa));
        assert_eq!(layer_blend(&app), BlendMode::Screen);
        // Not with ⌘, and not a letter without a mode.
        assert!(!press(&mut app, Key::M, Key::M, sa | Modifiers::COMMAND));
        assert!(!press(&mut app, Key::P, Key::P, sa));
        assert_eq!(layer_blend(&app), BlendMode::Screen);
        // Every mode in the table is a layer mode; one letter per mode.
        for (k, m) in LETTERS {
            assert!(BlendMode::LAYER_MODES.contains(&m), "{m:?}");
            assert_eq!(LETTERS.iter().filter(|(l, _)| *l == k).count(), 1, "{k:?}");
        }
    }

    #[test]
    fn shift_plus_and_minus_step_through_the_modes_and_stop_at_the_ends() {
        let mut app = app(Tool::RectMarquee);
        // ⇧= arrives as `+` on a US layout.
        assert!(press(&mut app, Key::Plus, Key::Equals, Modifiers::SHIFT));
        assert_eq!(layer_blend(&app), BlendMode::Dissolve);
        assert!(press(&mut app, Key::Minus, Key::Minus, Modifiers::SHIFT));
        press(&mut app, Key::Minus, Key::Minus, Modifiers::SHIFT);
        assert_eq!(layer_blend(&app), BlendMode::Normal, "stops at Normal");
        assert!(!press(&mut app, Key::Minus, Key::Minus, Modifiers::NONE), "plain - is not a mode key");
        assert_eq!(apply(BlendMode::Luminosity, Change::Step(1)), BlendMode::Luminosity);
    }

    #[test]
    fn painting_tools_change_their_own_mode() {
        let mut app = app(Tool::Brush);
        assert!(press(&mut app, Key::O, Key::O, Modifiers::SHIFT | Modifiers::ALT));
        assert_eq!(app.session.tools.brush.mode, BlendMode::Overlay);
        assert_eq!(layer_blend(&app), BlendMode::Normal, "the layer is untouched");
        app.ui.tool = Tool::Gradient;
        assert!(press(&mut app, Key::Plus, Key::Equals, Modifiers::SHIFT));
        assert_eq!(app.ui.tool_options.gradient_blend_mode, BlendMode::Dissolve);
        // Tools whose mode isn't kept take the key without touching the layer (nor switching tool).
        app.ui.tool = Tool::CloneStamp;
        assert!(press(&mut app, Key::M, Key::M, Modifiers::SHIFT | Modifiers::ALT));
        assert_eq!(layer_blend(&app), BlendMode::Normal);
    }

    #[test]
    fn the_background_and_no_document_are_left_alone() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.ui.tool = Tool::Move;
        assert!(press(&mut app, Key::M, Key::M, Modifiers::SHIFT | Modifiers::ALT), "consumed, nothing to change");
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        press(&mut app, Key::M, Key::M, Modifiers::SHIFT | Modifiers::ALT);
        assert_eq!(layer_blend(&app), BlendMode::Normal, "the Background has no mode");
    }
}
