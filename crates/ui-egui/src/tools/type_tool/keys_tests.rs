//! Photoshop's Type-tool selection clicks and formatting shortcuts (#218: TYPE-218-6, -8):
//! multi-click selection through the real canvas, Alt+arrows kerning / tracking / leading /
//! baseline shift, the ⇧⌘ formatting keys, ⌥⌫/⌘⌫ colour fills and ⌘H.

use egui::{Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use photocraft_doc::text::{Caps, CharStyle, Kerning, TextAlign};
use photocraft_doc::{LayerContent, LayerId, TextLayer};
use photocraft_geom::Point;
use serde_json::json;

use super::layout;
use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::state::TextEdit;

fn new_app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 1600, "height": 900})).unwrap();
    app.sync_views();
    app.ui.extras.rulers = false;
    app.ui.extras.snap = false;
    app.ui.tool = crate::state::Tool::Type;
    app
}

/// A harness whose frames are 1/60 s apart, so successive clicks count as one multi-click.
fn harness(app: PhotocraftApp) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).with_step_dt(1.0 / 60.0).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            let ctx = ui.ctx().clone();
            if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            crate::shortcuts::handle(app, &ctx);
            egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.run_steps(4);
    h
}

fn text(app: &PhotocraftApp, id: LayerId) -> TextLayer {
    match &app.session.active().unwrap().doc.layer(id).unwrap().content {
        LayerContent::Text(t) => t.clone(),
        other => panic!("{}", other.kind_name()),
    }
}

/// Style of character `i`.
fn style(app: &PhotocraftApp, id: LayerId, i: usize) -> CharStyle {
    let t = text(app, id);
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

/// Screen point inside glyph `i`, `f` of the way along it.
fn glyph(h: &mut Harness<'static, PhotocraftApp>, id: LayerId, i: usize, f: f32) -> Pos2 {
    let app = h.state_mut();
    let (l, aff, s) = layout(app, id).unwrap();
    let b = s.char_indices().nth(i).unwrap().0;
    let c = l.clusters.iter().find(|c| c.range.start == b).unwrap().clone();
    let ln = &l.lines[c.line];
    let p = aff.apply(Point::new(f64::from(c.x + c.advance * f), f64::from(ln.baseline - ln.ascent * 0.35)));
    let v = &app.ui.views[0];
    let xf = ViewXform {
        rect: crate::rulers::content_rect(app, app.last_canvas_rect),
        zoom: v.zoom / app.canvas_ppp(),
        center: v.center,
        flip: app.ui.view.flip_horizontal,
        rotation: v.rotation,
        aspect: app.ui.view.display_aspect(),
    };
    xf.to_screen(p.x as f32, p.y as f32)
}

fn click(h: &mut Harness<'static, PhotocraftApp>, p: Pos2) {
    h.hover_at(p);
    h.run_steps(1);
    for down in [true, false] {
        h.event(egui::Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: down, modifiers: Modifiers::NONE });
        h.run_steps(1);
    }
}

fn selection(h: &Harness<'static, PhotocraftApp>) -> (usize, usize) {
    let e = h.state().ui.text_edit.clone().expect("editing");
    (e.anchor.min(e.caret), e.anchor.max(e.caret))
}

/// Double-click selects a word, triple-click a laid-out line, quadruple-click the paragraph and
/// quintuple-click all the text; a slow click places the caret again.
#[test]
fn multi_clicks_select_word_line_paragraph_and_all() {
    let mut app = new_app();
    // Paragraph text: the first paragraph wraps over several lines.
    let body = "alpha beta gamma delta epsilon zeta eta theta\nsecond paragraph";
    let id = LayerId(app.run("type.create", json!({"text": body, "size": 40, "box": [200, 150, 420, 500]})).unwrap()["layer"].as_u64().unwrap());
    let mut h = harness(app);
    let lines = layout(h.state_mut(), id).unwrap().0.lines.clone();
    assert!(lines.len() >= 3 && lines[0].paragraph == lines[1].paragraph, "the first paragraph wraps: {lines:?}");
    let s = text(h.state(), id).text;
    let chars = |b: usize| s[..b].chars().count();
    // Character 13 ("gamma") on the first or second line.
    let i = 13;
    let p = glyph(&mut h, id, i, 0.5);
    let b = s.char_indices().nth(i).unwrap().0;
    let line = lines.iter().find(|l| b >= l.range.start && b < l.range.end).unwrap().clone();
    let expect = [(11, 16), (chars(line.range.start), chars(line.range.end)), (0, 45), (0, s.chars().count())];
    click(&mut h, p);
    assert!(h.state().ui.text_edit.is_some(), "the first click edits");
    for (n, want) in expect.into_iter().enumerate() {
        click(&mut h, p);
        assert_eq!(selection(&h), want, "{} clicks", n + 2);
    }
    // A pause, then a single click: a caret.
    h.run_steps(60);
    click(&mut h, p);
    let (a, b) = selection(&h);
    assert_eq!(a, b, "slow click places the caret");
    assert!((13..=14).contains(&a));
    // Selecting changes nothing in the document.
    assert_eq!(text(h.state(), id).text, body);
}

/// Edit layer `id` with `range` selected (caret at its end).
fn editing(app: &mut PhotocraftApp, id: LayerId, (anchor, caret): (usize, usize)) {
    app.ui.text_edit = Some(TextEdit {
        layer: id.0,
        document: None,
        caret,
        anchor,
        session: "keys-test".into(),
        created: false,
        dragging: false,
        resize: None,
        preedit: None,
        mask: false,
    });
}

fn press(h: &mut Harness<'static, PhotocraftApp>, key: Key, modifiers: Modifiers) {
    h.event(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers });
    h.event(egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers });
    h.run_steps(1);
}

const ALT: Modifiers = Modifiers::ALT;
fn cmd_shift() -> Modifiers {
    Modifiers::COMMAND | Modifiers::SHIFT
}

/// Alt+←/→ kerns at a collapsed caret and tracks over a selection (±20, ⌘ ±100); Alt+↑/↓
/// changes the leading and Alt+Shift+↑/↓ the baseline shift by 2 pt (⌘ 10) of the selection.
#[test]
fn alt_arrows_kern_track_lead_and_shift() {
    let mut app = new_app();
    let id = LayerId(app.run("type.create", json!({"text": "AVATAR wave", "size": 30, "x": 300, "y": 400})).unwrap()["layer"].as_u64().unwrap());
    editing(&mut app, id, (1, 1));
    let mut h = harness(app);
    // Caret between A and V: kerning of the pair (manual from then on).
    press(&mut h, Key::ArrowRight, ALT);
    let k = style(h.state(), id, 0);
    assert_eq!(k.kerning, Kerning::Off);
    let first = k.kern;
    press(&mut h, Key::ArrowLeft, ALT | Modifiers::COMMAND);
    assert!((style(h.state(), id, 0).kern - (first - 100.0)).abs() < 1e-3);
    assert_eq!(style(h.state(), id, 1).tracking, 0.0, "kerning is not tracking");
    // Select "wave": tracking.
    editing(h.state_mut(), id, (7, 11));
    press(&mut h, Key::ArrowRight, ALT);
    press(&mut h, Key::ArrowRight, ALT | Modifiers::COMMAND);
    press(&mut h, Key::ArrowLeft, ALT);
    assert_eq!(style(h.state(), id, 8).tracking, 100.0);
    assert_eq!(style(h.state(), id, 3).tracking, 0.0, "outside the selection");
    // Leading, as Photoshop: ⌥↑ tightens it by 2 (auto is 36 pt for 30 pt type), ⌘⌥↓ loosens 10.
    press(&mut h, Key::ArrowUp, ALT);
    assert_eq!(style(h.state(), id, 8).leading_pt, Some(34.0));
    press(&mut h, Key::ArrowDown, ALT | Modifiers::COMMAND);
    assert_eq!(style(h.state(), id, 8).leading_pt, Some(44.0));
    assert_eq!(style(h.state(), id, 3).leading_pt, None);
    // Baseline shift.
    press(&mut h, Key::ArrowUp, ALT | Modifiers::SHIFT);
    press(&mut h, Key::ArrowUp, ALT | Modifiers::SHIFT);
    press(&mut h, Key::ArrowDown, ALT | Modifiers::SHIFT | Modifiers::COMMAND);
    assert_eq!(style(h.state(), id, 8).baseline_shift_pt, -6.0);
    // The selection survives (each press is its own history step, as in Photoshop).
    assert_eq!(selection(&h), (7, 11));
}

/// The ⇧⌘ formatting keys of Photoshop's default shortcut list.
#[test]
fn command_shift_keys_format_the_selection() {
    let mut app = new_app();
    let id = LayerId(app.run("type.create", json!({"text": "one two\nthree four", "size": 30, "x": 300, "y": 400})).unwrap()["layer"].as_u64().unwrap());
    app.run("type.setStyle", json!({"layer": id.0, "horizontalScale": 80, "verticalScale": 120, "tracking": 50, "leading": 50})).unwrap();
    editing(&mut app, id, (0, 3));
    let mut h = harness(app);
    type Toggle = (Key, Modifiers, fn(&CharStyle) -> bool);
    let toggles: [Toggle; 7] = [
        (Key::K, cmd_shift(), |s| s.caps == Caps::AllCaps),
        (Key::H, cmd_shift(), |s| s.caps == Caps::SmallCaps),
        (Key::U, cmd_shift(), |s| s.underline),
        (Key::Slash, cmd_shift(), |s| s.strikethrough),
        (Key::Questionmark, cmd_shift(), |s| s.strikethrough),
        (Key::B, cmd_shift(), |s| s.faux_bold),
        (Key::I, cmd_shift(), |s| s.faux_italic),
    ];
    for (key, m, on) in toggles {
        let was = on(&style(h.state(), id, 0));
        press(&mut h, key, m);
        assert_ne!(on(&style(h.state(), id, 0)), was, "{key:?} toggles");
        assert!(!on(&style(h.state(), id, 5)) || was, "{key:?} only the selection");
    }
    assert!(h.state().ui.text_edit.is_some(), "still editing (no Select Inverse, Desaturate…)");
    // Resets.
    press(&mut h, Key::X, cmd_shift());
    assert_eq!(style(h.state(), id, 0).horizontal_scale, 1.0);
    press(&mut h, Key::X, cmd_shift() | Modifiers::ALT);
    assert_eq!(style(h.state(), id, 0).vertical_scale, 1.0);
    press(&mut h, Key::A, cmd_shift() | Modifiers::ALT);
    assert_eq!(style(h.state(), id, 0).leading_pt, None);
    press(&mut h, Key::Q, cmd_shift());
    assert_eq!(style(h.state(), id, 0).tracking, 0.0);
    let rest = style(h.state(), id, 5);
    assert_eq!((rest.horizontal_scale, rest.tracking, rest.leading_pt), (0.8, 50.0, Some(50.0)), "outside the selection");
    // egui-winit sends ⇧⌘X / ⇧⌘C as Cut / Copy events: they reset the scale and center.
    h.state_mut().run("type.setStyle", json!({"layer": id.0, "horizontalScale": 70})).unwrap();
    h.event(egui::Event::ModifiersChanged(cmd_shift()));
    h.event(egui::Event::Cut);
    h.run_steps(1);
    assert_eq!(style(h.state(), id, 0).horizontal_scale, 1.0);
    assert_eq!(text(h.state(), id).text, "one two\nthree four", "not cut");
    h.event(egui::Event::ModifiersChanged(cmd_shift()));
    h.event(egui::Event::Copy);
    h.run_steps(1);
    h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    let align = |h: &Harness<'static, PhotocraftApp>| text(h.state(), id).paragraph_runs().iter().map(|r| r.style.align).collect::<Vec<_>>();
    assert_eq!(align(&h), [TextAlign::Center, TextAlign::Left], "the selection's paragraph");
    // A collapsed caret aligns its own paragraph.
    editing(h.state_mut(), id, (12, 12));
    for (key, a) in [(Key::R, TextAlign::Right), (Key::J, TextAlign::JustifyLeft), (Key::F, TextAlign::JustifyAll), (Key::L, TextAlign::Left)] {
        press(&mut h, key, cmd_shift());
        assert_eq!(align(&h), [TextAlign::Center, a], "{key:?}");
    }
    // ⌥⇧⌘H: hyphenation of the caret's paragraph.
    press(&mut h, Key::H, cmd_shift() | Modifiers::ALT);
    let hy: Vec<bool> = text(h.state(), id).paragraph_runs().iter().map(|r| r.style.hyphenate).collect();
    assert_eq!(hy, [false, true]);
    // ⌘A still selects all (no ⇧).
    press(&mut h, Key::A, Modifiers::COMMAND);
    assert_eq!(selection(&h), (0, 18));
}

/// ⌥⌫ fills the selected type with the foreground colour and ⌘⌫ with the background colour;
/// without a selection they still delete.
#[test]
fn alt_and_command_backspace_fill_the_selected_type() {
    let mut app = new_app();
    app.run("tools.setColors", json!({"foreground": "#ff0000", "background": "#0000ff"})).unwrap();
    let id =
        LayerId(app.run("type.create", json!({"text": "red blue", "size": 30, "x": 300, "y": 400, "color": "#000000"})).unwrap()["layer"].as_u64().unwrap());
    editing(&mut app, id, (0, 3));
    let mut h = harness(app);
    press(&mut h, Key::Backspace, ALT);
    let c = style(h.state(), id, 0).color;
    assert!((c.c[0] - 1.0).abs() < 1e-3 && c.c[2] < 1e-3, "{c:?}");
    editing(h.state_mut(), id, (4, 8));
    press(&mut h, Key::Backspace, Modifiers::COMMAND);
    let c = style(h.state(), id, 5).color;
    assert!((c.c[2] - 1.0).abs() < 1e-3 && c.c[0] < 1e-3, "{c:?}");
    assert_eq!(text(h.state(), id).text, "red blue");
    // Collapsed caret: Alt+Backspace deletes the word before it.
    editing(h.state_mut(), id, (8, 8));
    press(&mut h, Key::Backspace, ALT);
    assert_eq!(text(h.state(), id).text, "red ");
}

/// ⌘H while editing toggles View › Extras, which hides the selection highlight; typing goes on.
#[test]
fn command_h_hides_the_selection_highlight() {
    let mut app = new_app();
    let id = LayerId(app.run("type.create", json!({"text": "hidden", "size": 30, "x": 300, "y": 400})).unwrap()["layer"].as_u64().unwrap());
    editing(&mut app, id, (0, 6));
    let mut h = harness(app);
    let shown = h.state().ui.view.extras;
    press(&mut h, Key::H, Modifiers::COMMAND);
    assert_ne!(h.state().ui.view.extras, shown);
    assert_eq!(selection(&h), (0, 6), "still editing, selection kept");
    press(&mut h, Key::H, Modifiers::COMMAND);
    assert_eq!(h.state().ui.view.extras, shown);
}
