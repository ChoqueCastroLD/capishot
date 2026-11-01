//! The Bubble Text Remover tool: in the J group right after Remove with AI (⇧J reaches it), in the
//! Capi sheet, with help in English, Spanish and Portuguese; a tap inside a bubble on a phone
//! removes its text in one undo step, and a textured bubble is refused with a notice.

use egui::{Event, Key, Modifiers, Pos2, TouchDeviceId, TouchId, TouchPhase, vec2};
use egui_kittest::Harness;
use photocraft_geom::Rect;
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;

type H = Harness<'static, PhotocraftApp>;

const W: i32 = 800;
const HGT: i32 = 1200;

fn inside(x: i32, y: i32) -> f32 {
    ((x as f32 + 0.5 - 400.0) / 250.0).powi(2) + ((y as f32 + 0.5 - 500.0) / 160.0).powi(2)
}

/// A page with an oval bubble (its inside `fill`) and black text in it, merged into the pixels.
fn page(app: &mut PhotocraftApp, fill: fn(i32, i32) -> f32) {
    app.run("file.new", json!({"width": W, "height": HGT, "background": "white"})).unwrap();
    let id = app.session.active().unwrap().doc.layers[0].id;
    let fmt = app.session.active().unwrap().doc.layer(id).unwrap().surface().unwrap().format();
    let n = fmt.channels();
    let mut px = vec![0f32; (W * HGT) as usize * n];
    let mut enc = [0f32; 8];
    for y in 0..HGT {
        for x in 0..W {
            let e = inside(x, y);
            let v = if e < 0.96 {
                fill(x, y)
            } else if e < 1.04 {
                0.0
            } else {
                1.0
            };
            photocraft_raster::from_rgba_into(&fmt, [v, v, v, 1.0], &mut enc);
            let k = (y * W + x) as usize * n;
            px[k..k + n].copy_from_slice(&enc[..n]);
        }
    }
    app.session
        .edit("Page", |doc, _| {
            doc.layer_mut(id).unwrap().surface_mut().unwrap().write_region(Rect::new(0, 0, W, HGT), &px);
            Ok(())
        })
        .unwrap();
    app.run("type.create", json!({"text": "HELLO THERE", "size": 56, "x": 230, "y": 520, "color": "#000000"})).unwrap();
    app.run("layer.mergeDown", json!({})).unwrap();
}

/// Dark pixels inside the bubble (the text).
fn dark(app: &PhotocraftApp) -> usize {
    let surf = app.session.active().unwrap().doc.layers[0].surface().unwrap();
    let fmt = surf.format();
    surf.read_region(Rect::new(0, 0, W, HGT))
        .chunks_exact(fmt.channels())
        .enumerate()
        .filter(|(k, c)| inside(*k as i32 % W, *k as i32 / W) < 0.9 && photocraft_raster::to_rgba(&fmt, c)[0] < 0.5)
        .count()
}

fn phone(app: PhotocraftApp) -> H {
    let mut h = Harness::builder().with_size(vec2(360.0, 780.0)).with_max_steps(64).with_step_dt(1.0 / 30.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    for _ in 0..4 {
        frame(&mut h, vec![]);
    }
    h
}

fn frame(h: &mut H, events: Vec<Event>) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    raw.events.extend(events);
    raw.time = Some(ctx.input(|i| i.time) + 1.0 / 30.0);
    crate::touch_ui::route(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    h.step();
}

fn tap(h: &mut H, p: Pos2) {
    let t = |phase| Event::Touch { device_id: TouchDeviceId(1), id: TouchId(5), phase, pos: p, force: None };
    frame(h, vec![t(TouchPhase::Start)]);
    frame(h, vec![t(TouchPhase::End)]);
    for _ in 0..4 {
        frame(h, vec![]);
    }
}

fn screen(h: &H, x: f32, y: f32) -> Pos2 {
    crate::canvas::ViewXform::active(h.state()).unwrap().to_screen(x, y)
}

fn phone_app(lang: &str, fill: fn(i32, i32) -> f32) -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": lang, "interface.simpleUi": "on"}})).unwrap();
    page(&mut app, fill);
    app.ui.tool = Tool::BubbleTextRemover;
    app.sync_views();
    app
}

#[test]
fn in_the_j_group_after_remove_with_ai_and_shift_j_reaches_it() {
    assert_eq!(Tool::BubbleTextRemover.key(), 'J');
    assert!(!Tool::BubbleTextRemover.is_brushlike(), "a click tool");
    assert_eq!(Tool::from_name("Bubble Text Remover Tool"), Some(Tool::BubbleTextRemover));
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 20, "height": 10})).unwrap();
    app.ui.tool = Tool::RemoveAi;
    let ctx = egui::Context::default();
    let key = |pressed, modifiers| Event::Key { key: Key::J, physical_key: None, pressed, repeat: false, modifiers };
    let press = |app: &mut PhotocraftApp, m: Modifiers| {
        ctx.begin_pass(egui::RawInput { events: vec![Event::ModifiersChanged(m), key(true, m), key(false, m)], ..Default::default() });
        crate::shortcuts::handle(app, &ctx);
        ctx.end_pass().textures_delta.clear();
    };
    press(&mut app, Modifiers::NONE);
    assert_eq!(app.ui.tool, Tool::BubbleTextRemover, "J after Remove with AI");
    press(&mut app, Modifiers::SHIFT);
    assert_eq!(app.ui.tool, Tool::RemoveAi, "⇧J back");
    // Photoshop's classic keys: ⇧J steps forward to it.
    app.session.edit_prefs(|p| p.tools.repeat_letter_cycles = false);
    press(&mut app, Modifiers::SHIFT);
    assert_eq!(app.ui.tool, Tool::BubbleTextRemover, "⇧J after Remove with AI (classic)");
}

#[test]
fn its_help_is_in_three_languages() {
    for code in ["en", "es", "pt-br"] {
        let lang = crate::i18n::Lang::from_code(code).unwrap();
        let d = crate::help_text::describe("tool:BubbleTextRemover", lang).unwrap();
        assert!(!d.is_empty(), "{code}");
    }
    let es = crate::i18n::Lang::from_code("es").unwrap();
    assert!(crate::help_text::describe("tool:BubbleTextRemover", es).unwrap().contains("globo"));
    assert_eq!(crate::i18n::tr(es, "Bubble Text Remover Tool"), "Herramienta Quitar texto en globo");
    assert_eq!(crate::i18n::tr(es, photocraft_engine::bubble_text_cmds::NOT_PLAIN), "Este globo no tiene fondo liso; usa Quitar con IA");
    assert_eq!(crate::i18n::tr(es, "Clean Selected Area with AI"), "Limpiar área seleccionada con IA");
    let pt = crate::i18n::Lang::from_code("pt-br").unwrap();
    assert_eq!(crate::i18n::tr(pt, "Bubble Text Remover Tool"), "Ferramenta Remover texto do balão");
    assert_eq!(crate::i18n::tr(pt, "Clean Selected Area with AI"), "Limpar área selecionada com IA");
}

#[test]
fn a_tap_in_a_bubble_on_a_phone_removes_its_text_in_one_step() {
    let mut h = phone(phone_app("en", |_, _| 1.0));
    assert!(dark(h.state()) > 300, "text to remove");
    let past = h.state().session.active().unwrap().history.past_len();
    let p = screen(&h, 400.0, 430.0);
    tap(&mut h, p);
    assert_eq!(dark(h.state()), 0, "the text is gone");
    let st = h.state().session.active().unwrap();
    assert_eq!(st.history.past_len(), past + 1, "one step");
    // The Capi sheet lists the tool.
    crate::simple_ui::set_sheet(&h.ctx, crate::simple_ui::Sheet::Master);
    for _ in 0..3 {
        frame(&mut h, vec![]);
    }
    let s = crate::simple_ui::inspect(h.state(), &h.ctx);
    assert!(s["buttons"].as_array().unwrap().iter().any(|b| b["label"] == "tool:BubbleTextRemover"), "in the Capi sheet");
}

#[test]
fn a_textured_bubble_is_refused_with_a_notice() {
    let dots = |x: i32, y: i32| if (x % 5 - 2).pow(2) + (y % 5 - 2).pow(2) <= 2 { 0.8 } else { 1.0 };
    let mut h = phone(phone_app("es", dots));
    let before = dark(h.state());
    let p = screen(&h, 400.0, 430.0);
    tap(&mut h, p);
    assert_eq!(dark(h.state()), before, "nothing changed");
    let n = h.state().ui.notices.iter().find(|n| n.tag.as_deref() == Some(crate::bubble_text_ui::NOTICE_TAG)).expect("a notice");
    assert_eq!(n.text(&n.title), "Este globo no tiene fondo liso; usa Quitar con IA");
}
