use super::*;
use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::adjust::{CurvePoint, LevelsChannel};
use photocraft_doc::{Adjustment, Document, Fill, Layer, LayerContent, LayerMask};
use photocraft_geom::Size;

const E: f32 = 2.0 / 255.0;

fn close4(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= E)
}

fn doc_white(w: u32, h: u32) -> Document {
    Document::with_background("t", Size::new(w, h), ColorMode::Rgb, SampleType::U8, Color::WHITE)
}

fn solid_layer(name: &str, rect: Rect, rgba: [f32; 4]) -> Layer {
    let mut l = Layer::raster(name, PixelFormat::RGBA8);
    l.surface_mut().unwrap().fill_rect(rect, &rgba);
    l
}

fn px(doc: &Document, x: i32, y: i32) -> [f32; 4] {
    render(doc, Rect::from_xywh(x, y, 1, 1)).px[0]
}

#[test]
fn background_only() {
    let d = doc_white(8, 8);
    assert!(close4(px(&d, 3, 3), [1.0; 4]));
    // outside canvas the background layer has no pixels
    assert!(close4(px(&d, 20, 3), [0.0; 4]));
}

#[test]
fn normal_layer_over_background() {
    let mut d = doc_white(8, 8);
    d.layers.push(solid_layer("red", Rect::new(0, 0, 4, 8), [1.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 1, 1), [1.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 5, 1), [1.0; 4]));
}

#[test]
fn opacity_and_fill_multiply() {
    let mut d = doc_white(4, 4);
    let mut l = solid_layer("k", Rect::new(0, 0, 4, 4), [0.0, 0.0, 0.0, 1.0]);
    l.opacity = 0.5;
    l.fill_opacity = 0.5;
    d.layers.push(l);
    let p = px(&d, 0, 0);
    assert!((p[0] - 0.75).abs() <= E, "{p:?}");
}

#[test]
fn hidden_layers_skipped() {
    let mut d = doc_white(4, 4);
    let mut l = solid_layer("k", Rect::new(0, 0, 4, 4), [0.0, 0.0, 0.0, 1.0]);
    l.visible = false;
    d.layers.push(l);
    assert!(close4(px(&d, 0, 0), [1.0; 4]));
}

#[test]
fn every_blend_mode_matches_reference_on_opaque_pixels() {
    let backdrop = [0.6, 0.3, 0.2, 1.0];
    let source = [0.2, 0.7, 0.5, 1.0];
    for mode in BlendMode::LAYER_MODES {
        if mode == BlendMode::Dissolve {
            continue;
        }
        let mut d = Document::new("b", Size::new(2, 2), ColorMode::Rgb, SampleType::F32);
        let mut bottom = Layer::raster("b", PixelFormat::RGBA32F);
        bottom.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2, 2), &backdrop);
        let mut top = Layer::raster("t", PixelFormat::RGBA32F);
        top.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2, 2), &source);
        top.blend = mode;
        d.layers = vec![bottom, top];
        let got = px(&d, 0, 0);
        let want = blend::blend_rgb(mode, [0.6, 0.3, 0.2], [0.2, 0.7, 0.5]);
        for i in 0..3 {
            assert!((got[i] - want[i]).abs() < 1e-5, "{mode:?}: {got:?} vs {want:?}");
        }
        assert!((got[3] - 1.0).abs() < 1e-6);
    }
}

#[test]
fn layer_mask_hides_pixels() {
    let mut d = doc_white(4, 4);
    let mut l = solid_layer("k", Rect::new(0, 0, 4, 4), [0.0, 0.0, 0.0, 1.0]);
    let mut m = LayerMask::reveal_all();
    m.surface.fill_rect(Rect::new(0, 0, 2, 4), &[0.0]);
    l.mask = Some(m);
    d.layers.push(l);
    assert!(close4(px(&d, 0, 0), [1.0; 4]));
    assert!(close4(px(&d, 3, 0), [0.0, 0.0, 0.0, 1.0]));
}

#[test]
fn disabled_mask_is_ignored() {
    let mut d = doc_white(2, 2);
    let mut l = solid_layer("k", Rect::new(0, 0, 2, 2), [0.0, 0.0, 0.0, 1.0]);
    let mut m = LayerMask::hide_all();
    m.enabled = false;
    l.mask = Some(m);
    d.layers.push(l);
    // CMYK displays through the built-in CMYK profile: 100 % K alone is a dark neutral
    // (as in any real CMYK profile), not pure black.
    let p = px(&d, 0, 0);
    assert!(p[0] < 0.3 && (p[0] - p[1]).abs() < 0.05 && (p[1] - p[2]).abs() < 0.05 && p[3] == 1.0, "{p:?}");
}

#[test]
fn clipping_mask_restricts_to_base_alpha() {
    let mut d = doc_white(8, 8);
    d.layers.push(solid_layer("base", Rect::new(0, 0, 4, 8), [0.0, 0.0, 1.0, 1.0]));
    let mut clip = solid_layer("clip", Rect::new(0, 0, 8, 8), [1.0, 0.0, 0.0, 1.0]);
    clip.clipped = true;
    d.layers.push(clip);
    assert!(close4(px(&d, 1, 1), [1.0, 0.0, 0.0, 1.0]), "inside base: clipped layer visible");
    assert!(close4(px(&d, 6, 1), [1.0; 4]), "outside base: clipped layer hidden");
}

#[test]
fn hidden_base_hides_clipping_group() {
    let mut d = doc_white(4, 4);
    let mut base = solid_layer("base", Rect::new(0, 0, 4, 4), [0.0, 0.0, 1.0, 1.0]);
    base.visible = false;
    d.layers.push(base);
    let mut clip = solid_layer("clip", Rect::new(0, 0, 4, 4), [1.0, 0.0, 0.0, 1.0]);
    clip.clipped = true;
    d.layers.push(clip);
    assert!(close4(px(&d, 1, 1), [1.0; 4]));
}

#[test]
fn isolated_group_differs_from_pass_through() {
    // A Multiply layer inside a group: pass-through multiplies with the background,
    // an isolated (Normal) group multiplies only with its own (empty) contents.
    let make = |blend: BlendMode| {
        let mut d = doc_white(2, 2);
        d.layers[0].surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2, 2), &[0.5, 0.5, 0.5, 1.0]);
        let mut m = solid_layer("m", Rect::new(0, 0, 2, 2), [0.5, 0.5, 0.5, 1.0]);
        m.blend = BlendMode::Multiply;
        let mut g = Layer::group("g", vec![m]);
        g.blend = blend;
        d.layers.push(g);
        px(&d, 0, 0)
    };
    let pass = make(BlendMode::PassThrough);
    let iso = make(BlendMode::Normal);
    assert!((pass[0] - 0.25).abs() <= E, "{pass:?}");
    assert!((iso[0] - 0.5).abs() <= E, "{iso:?}");
}

#[test]
fn group_opacity_applies_once() {
    let mut d = doc_white(2, 2);
    let a = solid_layer("a", Rect::new(0, 0, 2, 2), [0.0, 0.0, 0.0, 1.0]);
    let b = solid_layer("b", Rect::new(0, 0, 2, 2), [0.0, 0.0, 0.0, 1.0]);
    let mut g = Layer::group("g", vec![a, b]);
    g.blend = BlendMode::Normal;
    g.opacity = 0.5;
    d.layers.push(g);
    assert!((px(&d, 0, 0)[0] - 0.5).abs() <= E);
}

#[test]
fn pass_through_group_opacity_mixes() {
    let mut d = doc_white(2, 2);
    let a = solid_layer("a", Rect::new(0, 0, 2, 2), [0.0, 0.0, 0.0, 1.0]);
    let mut g = Layer::group("g", vec![a]);
    g.opacity = 0.25;
    d.layers.push(g);
    assert!((px(&d, 0, 0)[0] - 0.75).abs() <= E);
}

#[test]
fn invert_adjustment_layer() {
    let mut d = doc_white(2, 2);
    d.layers[0].surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2, 2), &[0.2, 0.4, 0.6, 1.0]);
    d.layers.push(Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert)));
    assert!(close4(px(&d, 0, 0), [0.8, 0.6, 0.4, 1.0]));
}

#[test]
fn adjustment_opacity_and_mask() {
    let mut d = doc_white(4, 1);
    d.layers[0].surface_mut().unwrap().fill_rect(Rect::new(0, 0, 4, 1), &[0.0, 0.0, 0.0, 1.0]);
    let mut adj = Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert));
    adj.opacity = 0.5;
    let mut m = LayerMask::reveal_all();
    m.surface.fill_rect(Rect::new(2, 0, 4, 1), &[0.0]);
    adj.mask = Some(m);
    d.layers.push(adj);
    assert!((px(&d, 0, 0)[0] - 0.5).abs() <= E);
    assert!(px(&d, 3, 0)[0].abs() <= E);
}

#[test]
fn clipped_adjustment_only_affects_base() {
    let mut d = doc_white(4, 1);
    d.layers.push(solid_layer("base", Rect::new(0, 0, 2, 1), [0.0, 0.0, 0.0, 1.0]));
    let mut adj = Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert));
    adj.clipped = true;
    d.layers.push(adj);
    assert!(close4(px(&d, 0, 0), [1.0; 4]), "base inverted to white");
    assert!(close4(px(&d, 3, 0), [1.0; 4]), "background untouched (white)");
}

#[test]
fn threshold_posterize_levels_curves() {
    let mut buf = Buffer::filled(Rect::new(0, 0, 1, 1), [0.3, 0.3, 0.3, 1.0]);
    adjust::apply(&Adjustment::Threshold { level: 0.5 }, &mut buf);
    assert!(close4(buf.px[0], [0.0, 0.0, 0.0, 1.0]));

    let mut buf = Buffer::filled(Rect::new(0, 0, 1, 1), [0.3, 0.6, 0.9, 1.0]);
    adjust::apply(&Adjustment::Posterize { levels: 2 }, &mut buf);
    assert!(close4(buf.px[0], [0.0, 1.0, 1.0, 1.0]));

    let lv = LevelsChannel { in_black: 0.2, in_white: 0.8, ..Default::default() };
    assert!((adjust::levels(&lv, 0.5) - 0.5).abs() < 1e-6);
    assert!((adjust::levels(&lv, 0.2)).abs() < 1e-6);
    assert!((adjust::levels(&lv, 0.9) - 1.0).abs() < 1e-6);

    let identity = adjust::curve_lut(&[CurvePoint { input: 0.0, output: 0.0 }, CurvePoint { input: 1.0, output: 1.0 }]);
    for (i, v) in identity.iter().enumerate().step_by(97) {
        assert!((v - i as f32 / (identity.len() - 1) as f32).abs() < 1e-4);
    }
    let s = adjust::curve_lut(&[
        CurvePoint { input: 0.0, output: 0.0 },
        CurvePoint { input: 0.25, output: 0.15 },
        CurvePoint { input: 0.75, output: 0.85 },
        CurvePoint { input: 1.0, output: 1.0 },
    ]);
    assert!(s.windows(2).all(|w| w[1] >= w[0] - 1e-6), "monotone");
    assert!(s[s.len() / 4] < 0.25 && s[3 * s.len() / 4] > 0.75);
}

#[test]
fn hue_saturation_roundtrips_and_desaturates() {
    let c = [0.8, 0.3, 0.1];
    let (h, s, l) = adjust::rgb_to_hsl(c);
    let back = adjust::hsl_to_rgb(h, s, l);
    for i in 0..3 {
        assert!((back[i] - c[i]).abs() < 1e-5);
    }
    let mut buf = Buffer::filled(Rect::new(0, 0, 1, 1), [0.8, 0.3, 0.1, 1.0]);
    adjust::apply(&Adjustment::HueSaturation { hue: 0.0, saturation: -100.0, lightness: 0.0, colorize: false }, &mut buf);
    let p = buf.px[0];
    assert!((p[0] - p[1]).abs() < 1e-5 && (p[1] - p[2]).abs() < 1e-5);
    let mut buf = Buffer::filled(Rect::new(0, 0, 1, 1), [1.0, 0.0, 0.0, 1.0]);
    adjust::apply(&Adjustment::HueSaturation { hue: 120.0, saturation: 0.0, lightness: 0.0, colorize: false }, &mut buf);
    assert!(close4(buf.px[0], [0.0, 1.0, 0.0, 1.0]));
}

#[test]
fn solid_and_gradient_fill_layers() {
    let mut d = doc_white(10, 1);
    d.layers.push(Layer::new("fill", LayerContent::Fill(Fill::Solid(Color::rgb(0.0, 0.0, 1.0)))));
    assert!(close4(px(&d, 5, 0), [0.0, 0.0, 1.0, 1.0]));

    let g = Fill::Gradient { stops: vec![(0.0, Color::BLACK), (1.0, Color::WHITE)], angle: 0.0, scale: 1.0, style: photocraft_doc::GradientStyle::Linear, reverse: false };
    let buf = render_fill(&g, Rect::new(0, 0, 10, 1), Rect::new(0, 0, 10, 1), &[]);
    // tile independence: a 1px render of the right edge equals the full render
    let one = render_fill(&g, Rect::new(9, 0, 10, 1), Rect::new(0, 0, 10, 1), &[]);
    assert_eq!(one.px[0], buf.px[9]);
    assert!(buf.px[0][0] < buf.px[9][0], "left dark, right light");
}

#[test]
fn dissolve_coverage_matches_opacity() {
    let mut d = Document::new("d", Size::new(64, 64), ColorMode::Rgb, SampleType::U8);
    let mut l = solid_layer("k", Rect::new(0, 0, 64, 64), [0.0, 0.0, 0.0, 1.0]);
    l.blend = BlendMode::Dissolve;
    l.opacity = 0.3;
    d.layers.push(l);
    let b = flatten(&d);
    let covered = b.px.iter().filter(|p| p[3] > 0.5).count() as f32 / b.px.len() as f32;
    assert!((covered - 0.3).abs() < 0.05, "{covered}");
    // deterministic
    assert_eq!(flatten(&d), b);
}

#[test]
fn render_is_tile_independent() {
    // Rendering a sub-rect must equal the same region of a full render.
    let mut d = doc_white(300, 300);
    let mut l = solid_layer("a", Rect::new(20, 20, 280, 280), [0.1, 0.5, 0.9, 0.7]);
    l.blend = BlendMode::Overlay;
    d.layers.push(l);
    let full = flatten(&d);
    let sub = render(&d, Rect::new(250, 250, 270, 262));
    for y in 250..262 {
        for x in 250..270 {
            assert_eq!(full.get(x, y), sub.get(x, y));
        }
    }
}

#[test]
fn cmyk_document_renders_via_rgb() {
    let mut d = Document::new("c", Size::new(2, 2), ColorMode::Cmyk, SampleType::U8);
    let mut l = Layer::raster("k", d.pixel_format());
    l.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2, 2), &[0.0, 0.0, 0.0, 1.0, 1.0]);
    d.layers.push(l);
    // CMYK displays through the built-in CMYK profile: 100 % K alone is a dark neutral
    // (as in any real CMYK profile), not pure black.
    let p = px(&d, 0, 0);
    assert!(p[0] < 0.3 && (p[0] - p[1]).abs() < 0.05 && (p[1] - p[2]).abs() < 0.05 && p[3] == 1.0, "{p:?}");
}

#[test]
fn sixteen_bit_and_float_layers_composite() {
    for fmt in [PixelFormat::RGBA16, PixelFormat::RGBA32F] {
        let mut d = Document::new("x", Size::new(2, 2), ColorMode::Rgb, fmt.sample);
        let mut l = Layer::raster("a", fmt);
        l.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2, 2), &[0.25, 0.5, 0.75, 1.0]);
        d.layers.push(l);
        assert!(close4(px(&d, 1, 1), [0.25, 0.5, 0.75, 1.0]), "{fmt:?}");
    }
}

#[test]
fn thumbnail_dimensions() {
    let d = doc_white(400, 200);
    let t = thumbnail(&d, 100);
    assert_eq!((t.width, t.height), (100, 50));
    assert_eq!(&t.pixels[0..4], &[255, 255, 255, 255]);
    let small = doc_white(10, 5);
    let t = thumbnail(&small, 100);
    assert_eq!((t.width, t.height), (10, 5));
}

#[test]
fn buffer_over_background() {
    let b = Buffer::filled(Rect::new(0, 0, 1, 1), [0.0, 0.0, 0.0, 0.5]);
    let o = b.over_background([1.0, 1.0, 1.0]);
    assert!(close4(o.px[0], [0.5, 0.5, 0.5, 1.0]));
    assert_eq!(b.to_rgba8().pixels, vec![0, 0, 0, 128]);
}

// ---------- layer effects ----------

use photocraft_doc::{Effect, FxCommon, FxPaint, Glow, GlowSource, GlowTechnique, Gradient, GradientStyle, Satin, Shadow, StrokeFx, StrokePosition};

fn fx_doc(effects: Vec<Effect>) -> Document {
    let mut d = doc_white(40, 40);
    let mut l = solid_layer("sq", Rect::new(10, 10, 30, 30), [1.0, 0.0, 0.0, 1.0]);
    l.effects.items = effects;
    d.layers.push(l);
    d
}

fn stroke(size: f32, position: StrokePosition) -> Effect {
    Effect::Stroke(StrokeFx { common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0), size, position, paint: FxPaint::Color(Color::rgb(0.0, 0.0, 1.0)) })
}

#[test]
fn outside_stroke_width() {
    let d = fx_doc(vec![stroke(3.0, StrokePosition::Outside)]);
    assert!(close4(px(&d, 8, 20), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 7, 20), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 5, 20), [1.0; 4]), "{:?}", px(&d, 5, 20));
    assert!(close4(px(&d, 20, 20), [1.0, 0.0, 0.0, 1.0]), "interior untouched");
}

#[test]
fn inside_and_center_strokes() {
    let d = fx_doc(vec![stroke(2.0, StrokePosition::Inside)]);
    assert!(close4(px(&d, 10, 20), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 11, 20), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 13, 20), [1.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 9, 20), [1.0; 4]));
    let d = fx_doc(vec![stroke(4.0, StrokePosition::Center)]);
    assert!(close4(px(&d, 8, 20), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 11, 20), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 13, 20), [1.0, 0.0, 0.0, 1.0]));
}

#[test]
fn master_switch_and_per_effect_enable() {
    let mut d = fx_doc(vec![stroke(3.0, StrokePosition::Outside)]);
    d.layers[1].effects.enabled = false;
    assert!(close4(px(&d, 8, 20), [1.0; 4]));
    let mut d = fx_doc(vec![stroke(3.0, StrokePosition::Outside)]);
    if let Effect::Stroke(s) = &mut d.layers[1].effects.items[0] {
        s.common.enabled = false;
    }
    assert!(close4(px(&d, 8, 20), [1.0; 4]));
}

#[test]
fn color_overlay_ignores_fill_opacity_but_not_opacity() {
    let mut d = fx_doc(vec![Effect::ColorOverlay { common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0), color: Color::rgb(0.0, 1.0, 0.0) }]);
    d.layers[1].fill_opacity = 0.0;
    assert!(close4(px(&d, 20, 20), [0.0, 1.0, 0.0, 1.0]), "overlay shows at fill 0");
    d.layers[1].opacity = 0.5;
    assert!(close4(px(&d, 20, 20), [0.5, 1.0, 0.5, 1.0]), "{:?}", px(&d, 20, 20));
}

fn shadow(distance: f32, angle: f32) -> Shadow {
    Shadow {
        common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0),
        color: Color::BLACK,
        angle,
        use_global_light: false,
        distance,
        spread: 1.0,
        size: 0.0,
        contour: photocraft_doc::Contour::Linear,
        anti_alias: false,
        noise: 0.0,
        knocks_out: true,
    }
}

#[test]
fn drop_shadow_falls_away_from_light() {
    // Light from the top (90°): shadow below the square.
    let d = fx_doc(vec![Effect::DropShadow(shadow(5.0, 90.0))]);
    assert!(close4(px(&d, 20, 32), [0.0, 0.0, 0.0, 1.0]), "{:?}", px(&d, 20, 32));
    assert!(close4(px(&d, 20, 8), [1.0; 4]));
    // Light from the right (0°, 3 o'clock): shadow to the left.
    let d = fx_doc(vec![Effect::DropShadow(shadow(5.0, 0.0))]);
    assert!(close4(px(&d, 8, 20), [0.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 32, 20), [1.0; 4]));
}

#[test]
fn drop_shadow_uses_global_light_and_knockout() {
    let mut d = fx_doc(vec![Effect::DropShadow(Shadow { use_global_light: true, ..shadow(5.0, 0.0) })]);
    d.global_light.angle = 90.0;
    assert!(close4(px(&d, 20, 32), [0.0, 0.0, 0.0, 1.0]));
    // Knock-out: with fill 0 the shadow does not show through the layer.
    d.layers[1].fill_opacity = 0.0;
    assert!(close4(px(&d, 20, 28), [1.0; 4]), "{:?}", px(&d, 20, 28));
    if let Effect::DropShadow(s) = &mut d.layers[1].effects.items[0] {
        s.knocks_out = false;
    }
    assert!(close4(px(&d, 20, 28), [0.0, 0.0, 0.0, 1.0]));
}

#[test]
fn soft_shadow_is_blurred_and_bounded() {
    let d = fx_doc(vec![Effect::DropShadow(Shadow { spread: 0.0, size: 6.0, ..shadow(0.0, 90.0) })]);
    let edge = px(&d, 30, 20); // just outside the right edge
    let far = px(&d, 39, 20);
    assert!(edge[0] < 0.8 && edge[0] > 0.2, "{edge:?}");
    assert!(far[0] > 0.99, "{far:?}");
}

#[test]
fn inner_shadow_only_inside() {
    let d = fx_doc(vec![Effect::InnerShadow(Shadow { knocks_out: false, ..shadow(4.0, 90.0) })]);
    // Top rows of the square are shadowed (light from the top pushes the
    // outside's shadow down into the shape).
    assert!(close4(px(&d, 20, 11), [0.0, 0.0, 0.0, 1.0]), "{:?}", px(&d, 20, 11));
    assert!(close4(px(&d, 20, 25), [1.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 20, 5), [1.0; 4]));
}

fn glow(technique: GlowTechnique, source: GlowSource) -> Glow {
    Glow {
        common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0),
        paint: FxPaint::Color(Color::rgb(0.0, 1.0, 0.0)),
        technique,
        spread: 0.0,
        size: 4.0,
        contour: photocraft_doc::Contour::Linear,
        anti_alias: false,
        range: 0.5,
        jitter: 0.0,
        noise: 0.0,
        source,
    }
}

#[test]
fn outer_and_inner_glow_regions() {
    let d = fx_doc(vec![Effect::OuterGlow(glow(GlowTechnique::Precise, GlowSource::Edge))]);
    let near = px(&d, 9, 20);
    assert!(near[1] > 0.7 && near[0] < 0.3, "{near:?}");
    assert!(close4(px(&d, 3, 20), [1.0; 4]));
    assert!(close4(px(&d, 20, 20), [1.0, 0.0, 0.0, 1.0]));
    let d = fx_doc(vec![Effect::InnerGlow(glow(GlowTechnique::Softer, GlowSource::Edge))]);
    let edge = px(&d, 10, 20);
    assert!(edge[1] > 0.3, "{edge:?}");
    assert!(close4(px(&d, 20, 20), [1.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 5, 20), [1.0; 4]));
}

#[test]
fn gradient_overlay_follows_angle_and_reverse() {
    let g = Gradient { stops: vec![(0.0, Color::BLACK), (1.0, Color::WHITE)], angle: 0.0, ..Gradient::default() };
    let d = fx_doc(vec![Effect::GradientOverlay { common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0), gradient: g.clone(), dither: false }]);
    let (l, r) = (px(&d, 10, 20), px(&d, 29, 20));
    assert!(l[0] < 0.1 && r[0] > 0.9, "{l:?} {r:?}");
    let d = fx_doc(vec![Effect::GradientOverlay {
        common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0),
        gradient: Gradient { reverse: true, style: GradientStyle::Linear, ..g },
        dither: false,
    }]);
    assert!(px(&d, 10, 20)[0] > 0.9);
}

#[test]
fn satin_and_bevel_stay_inside_shape() {
    let satin = Effect::Satin(Satin {
        common: FxCommon::new(photocraft_color::BlendMode::Multiply, 1.0),
        color: Color::BLACK,
        angle: 19.0,
        distance: 4.0,
        size: 4.0,
        contour: photocraft_doc::Contour::Linear,
        anti_alias: true,
        invert: false,
    });
    let d = fx_doc(vec![satin, Effect::BevelEmboss(photocraft_doc::Bevel {
        enabled: true,
        style: photocraft_doc::BevelStyle::InnerBevel,
        technique: photocraft_doc::BevelTechnique::Smooth,
        depth: 1.0,
        up: true,
        size: 4.0,
        soften: 0.0,
        angle: 90.0,
        altitude: 30.0,
        use_global_light: false,
        gloss_contour: photocraft_doc::Contour::Linear,
        highlight: FxCommon::new(photocraft_color::BlendMode::Screen, 0.75),
        highlight_color: Color::WHITE,
        shadow: FxCommon::new(photocraft_color::BlendMode::Multiply, 0.75),
        shadow_color: Color::BLACK,
        contour: None,
        texture: None,
    })]);
    for (x, y) in [(5, 5), (35, 20), (20, 35)] {
        assert!(close4(px(&d, x, y), [1.0; 4]), "({x},{y}) {:?}", px(&d, x, y));
    }
    // Light from the top: the top bevel edge is brighter than the bottom one.
    let (top, bot) = (px(&d, 20, 11), px(&d, 20, 28));
    assert!(top[1] > bot[1], "{top:?} {bot:?}");
}

#[test]
fn clipped_layer_effects_are_clipped_to_base() {
    let mut d = doc_white(40, 40);
    d.layers.push(solid_layer("base", Rect::new(10, 10, 30, 30), [1.0, 0.0, 0.0, 1.0]));
    let mut c = solid_layer("clip", Rect::new(20, 10, 40, 30), [0.0, 1.0, 0.0, 1.0]);
    c.clipped = true;
    c.effects.items.push(stroke(3.0, StrokePosition::Outside));
    d.layers.push(c);
    assert!(close4(px(&d, 18, 20), [0.0, 0.0, 1.0, 1.0]), "stroke inside base");
    assert!(close4(px(&d, 20, 8), [1.0; 4]), "stroke clipped outside base");
}

#[test]
fn group_effects_apply_to_group_shape() {
    let mut d = doc_white(40, 40);
    let mut g = Layer::group("g", vec![solid_layer("a", Rect::new(10, 10, 20, 30), [1.0, 0.0, 0.0, 1.0]), solid_layer("b", Rect::new(20, 10, 30, 30), [1.0, 0.0, 0.0, 1.0])]);
    g.effects.items.push(stroke(2.0, StrokePosition::Outside));
    d.layers.push(g);
    assert!(close4(px(&d, 9, 20), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 20, 20), [1.0, 0.0, 0.0, 1.0]), "no stroke at the seam between children");
}

#[test]
fn effects_render_identically_in_tiles() {
    let d = fx_doc(vec![Effect::DropShadow(Shadow { spread: 0.0, size: 5.0, ..shadow(4.0, 120.0) }), stroke(2.0, StrokePosition::Outside)]);
    let full = flatten(&d);
    for (x, y) in [(33, 33), (8, 20), (31, 12)] {
        let t = render(&d, Rect::from_xywh(x, y, 1, 1)).px[0];
        assert!(close4(t, full.get(x, y)), "({x},{y})");
    }
}

#[test]
fn parallel_tiles_match_single_pass() {
    // A document exercising masks, clipping, groups, adjustments, fills and effects.
    let mut d = doc_white(97, 61);
    let mut a = solid_layer("a", Rect::new(5, 5, 70, 50), [1.0, 0.2, 0.1, 0.8]);
    let mut m = LayerMask::reveal_all();
    m.surface.fill_rect(Rect::new(0, 0, 40, 61), &[0.25]);
    m.density = 0.8;
    a.mask = Some(m);
    a.effects.items.push(stroke(3.0, StrokePosition::Outside));
    d.layers.push(a);
    let mut clip = solid_layer("clip", Rect::new(30, 0, 97, 61), [0.0, 1.0, 0.0, 1.0]);
    clip.clipped = true;
    clip.blend = photocraft_color::BlendMode::Multiply;
    d.layers.push(clip);
    let mut g = Layer::group("g", vec![solid_layer("in", Rect::new(50, 20, 90, 60), [0.2, 0.3, 0.9, 1.0])]);
    g.opacity = 0.6;
    d.layers.push(g);
    d.layers.push(Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert)));
    let mut f = Layer::new("fill", LayerContent::Fill(Fill::Solid(Color::rgb(0.5, 0.5, 0.0))));
    f.opacity = 0.3;
    d.layers.push(f);
    let r = d.bounds();
    let whole = render_tiled(&d, r, 10_000);
    for tile in [7, 16, 33] {
        assert_eq!(render_tiled(&d, r, tile), whole, "tile {tile}");
    }
    assert_eq!(render(&d, r), whole);
}

#[test]
fn vector_mask_combines_with_pixel_mask() {
    use photocraft_doc::{Path, Subpath, VectorMask};
    let mut d = doc_white(8, 8);
    let mut l = solid_layer("k", Rect::new(0, 0, 8, 8), [0.0, 0.0, 0.0, 1.0]);
    // Vector mask reveals x in 0..4 (and half of column 4); the pixel mask hides rows 0..2.
    l.vector_mask = Some(VectorMask::new(Path::new(vec![Subpath::polygon(&[(0.0, 0.0), (4.5, 0.0), (4.5, 8.0), (0.0, 8.0)])])));
    let mut m = LayerMask::reveal_all();
    m.surface.fill_rect(Rect::new(0, 0, 8, 2), &[0.0]);
    l.mask = Some(m);
    d.layers.push(l);
    assert!(close4(px(&d, 1, 4), [0.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 6, 4), [1.0; 4]));
    assert!(close4(px(&d, 4, 4), [0.5, 0.5, 0.5, 1.0]));
    assert!(close4(px(&d, 1, 1), [1.0; 4]));
    // Tiled rendering gives the same result.
    let whole = render_tiled(&d, d.bounds(), 3);
    assert_eq!(whole.px, render_tiled(&d, d.bounds(), 256).px);
    // Density and disabling.
    let top = d.layers.len() - 1;
    let vm = d.layers[top].vector_mask.as_mut().unwrap();
    vm.density = 0.5;
    assert!(close4(px(&d, 6, 4), [0.5, 0.5, 0.5, 1.0]));
    d.layers[top].vector_mask.as_mut().unwrap().enabled = false;
    assert!(close4(px(&d, 6, 4), [0.0, 0.0, 0.0, 1.0]));
    // Without a pixel mask the vector mask alone applies.
    d.layers[top].mask = None;
    d.layers[top].vector_mask.as_mut().unwrap().enabled = true;
    d.layers[top].vector_mask.as_mut().unwrap().density = 1.0;
    assert!(close4(px(&d, 1, 1), [0.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 6, 1), [1.0; 4]));
}

#[test]
fn effect_maps_are_cached_and_invalidated_by_pixel_changes() {
    let mut doc = doc_white(64, 64);
    let mut l = solid_layer("fx", Rect::new(16, 16, 48, 48), [1.0, 0.0, 0.0, 1.0]);
    l.effects.items.push(photocraft_doc::Effect::default_drop_shadow());
    doc.layers.push(l);
    let cx = Ctx { canvas: doc.bounds(), transfer: adjust::Transfer::Srgb, light: doc.global_light, patterns: &doc.patterns, mode: doc.mode, depth: doc.depth };
    let a = effect_maps(&doc.layers[1], &cx);
    let b = effect_maps(&doc.layers[1], &cx);
    assert!(std::sync::Arc::ptr_eq(&a, &b), "second request hits the cache");
    let first = flatten(&doc);
    // Editing the layer's pixels changes its tiles, so the maps are rebuilt.
    doc.layers[1].surface_mut().unwrap().fill_rect(Rect::new(8, 8, 20, 20), &[0.0, 0.0, 1.0, 1.0]);
    let c = effect_maps(&doc.layers[1], &cx);
    assert!(!std::sync::Arc::ptr_eq(&a, &c), "pixel edit invalidates");
    // Cached rendering equals a fresh build (tiled and full renders agree too).
    let again = flatten(&doc);
    let region = doc.bounds();
    let tiled = render_tiled(&doc, region, 16);
    for (p, q) in again.px.iter().zip(&tiled.px) {
        assert!(close4(*p, *q));
    }
    assert_ne!(first.px, again.px);
}

// ---------- PSD-fidelity effect semantics (fitted on Photoshop composites) ----------

#[test]
fn stroke_corners_follow_the_5x5_chamfer_metric() {
    // Square 10..30, outside stroke 3: pixel (9, 7) is offset (1, 3) from the corner pixel.
    // Photoshop measures √5 + 1 = 3.236 there (exact distance 3.162): coverage 3 + 1 − 3.236.
    let d = fx_doc(vec![stroke(3.0, StrokePosition::Outside)]);
    let a = 1.0 - px(&d, 9, 7)[0]; // blue stroke over white: red channel drops by coverage
    assert!((a - 0.764).abs() < 0.01, "{a}");
    // (2, 2) stays Euclidean (two diagonal steps).
    let a = 1.0 - px(&d, 8, 8)[0];
    assert!((a - 1.0).abs() < 0.01, "{a}");
}

#[test]
fn inside_stroke_respects_partial_edge_coverage() {
    // A 25 %-alpha edge column puts the edge 0.75 px further out: a 3 px inside stroke then
    // reaches 75 % into the fourth column, and the edge column takes the stroke colour at 25 %.
    let mut d = doc_white(40, 40);
    let mut l = solid_layer("sq", Rect::new(10, 10, 30, 30), [1.0, 0.0, 0.0, 1.0]);
    l.surface_mut().unwrap().fill_rect(Rect::new(10, 10, 11, 30), &[1.0, 0.0, 0.0, 0.25]);
    l.effects.items = vec![stroke(3.0, StrokePosition::Inside)];
    d.layers.push(l);
    let edge = px(&d, 10, 20);
    assert!(close4(edge, [0.75, 0.75, 1.0, 1.0]), "edge = blue stroke at 25 % over white: {edge:?}");
    let p = px(&d, 13, 20);
    assert!((p[2] - 0.75).abs() < 0.02 && (p[0] - 0.25).abs() < 0.02, "{p:?}");
}

#[test]
fn interior_effects_keep_the_layer_alpha() {
    // A colour overlay replaces a half-transparent pixel's colour without adding coverage.
    let mut d = Document::new("t", Size::new(40, 40), ColorMode::Rgb, SampleType::U8);
    let mut l = solid_layer("sq", Rect::new(10, 10, 30, 30), [1.0, 0.0, 0.0, 0.5]);
    l.effects.items = vec![Effect::ColorOverlay { common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0), color: Color::rgb(0.0, 1.0, 0.0) }];
    d.layers.push(l);
    assert!(close4(px(&d, 20, 20), [0.0, 1.0, 0.0, 0.5]), "{:?}", px(&d, 20, 20));
}

#[test]
fn upper_stroke_blends_with_the_backdrop_not_the_lower_stroke() {
    // Multiply yellow (3 px) listed above a Normal blue (8 px): the inner ring multiplies the
    // white backdrop (stays yellow), the outer ring is blue.
    let yellow = Effect::Stroke(StrokeFx {
        common: FxCommon::new(photocraft_color::BlendMode::Multiply, 1.0),
        size: 3.0,
        position: StrokePosition::Outside,
        paint: FxPaint::Color(Color::rgb(1.0, 1.0, 0.0)),
    });
    let d = fx_doc(vec![yellow, stroke(8.0, StrokePosition::Outside)]);
    assert!(close4(px(&d, 8, 20), [1.0, 1.0, 0.0, 1.0]), "{:?}", px(&d, 8, 20));
    assert!(close4(px(&d, 4, 20), [0.0, 0.0, 1.0, 1.0]), "{:?}", px(&d, 4, 20));
}

#[test]
fn shadow_knockout_needs_see_through_fill() {
    // At 100 % fill an anti-aliased edge is not attenuated twice: the result equals no knockout.
    let mut base = Document::new("t", Size::new(40, 40), ColorMode::Rgb, SampleType::U8);
    base.layers.push(solid_layer("bg", Rect::new(0, 0, 40, 40), [1.0; 4]));
    let mut l = solid_layer("sq", Rect::new(10, 10, 30, 30), [1.0, 0.0, 0.0, 1.0]);
    l.surface_mut().unwrap().fill_rect(Rect::new(10, 10, 11, 30), &[1.0, 0.0, 0.0, 0.5]);
    l.effects.items = vec![Effect::DropShadow(Shadow { distance: 0.0, spread: 1.0, size: 4.0, ..shadow(0.0, 0.0) })];
    base.layers.push(l);
    let on = px(&base, 10, 20);
    if let Effect::DropShadow(s) = &mut base.layers[1].effects.items[0] {
        s.knocks_out = false;
    }
    assert!(close4(on, px(&base, 10, 20)), "{on:?}");
}

#[test]
fn linked_pattern_overlay_anchors_at_the_effects_reference_point() {
    use photocraft_doc::pattern::Pattern;
    // 2 × 1 tile: red, blue. Anchored at x = 11 (reference point), x = 11 is red, 12 blue.
    let mut tile = photocraft_raster::Surface::new(PixelFormat::RGBA8);
    tile.fill_rect(Rect::new(0, 0, 1, 1), &[1.0, 0.0, 0.0, 1.0]);
    tile.fill_rect(Rect::new(1, 0, 2, 1), &[0.0, 0.0, 1.0, 1.0]);
    let pat = Pattern::new("rb", tile, 2, 1);
    let overlay = Effect::PatternOverlay {
        common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0),
        name: "rb".into(),
        id: pat.id.clone(),
        scale: 1.0,
        angle: 0.0,
        link: true,
        phase: (0.0, 0.0),
    };
    let mut d = fx_doc(vec![overlay]);
    d.patterns.push(pat);
    // Without a reference point the layer's top-left (10) anchors the tiling.
    assert!(close4(px(&d, 10, 20), [1.0, 0.0, 0.0, 1.0]), "{:?}", px(&d, 10, 20));
    d.layers[1].effects.reference = Some((11.0, 0.0));
    assert!(close4(px(&d, 11, 20), [1.0, 0.0, 0.0, 1.0]), "{:?}", px(&d, 11, 20));
    assert!(close4(px(&d, 12, 20), [0.0, 0.0, 1.0, 1.0]), "{:?}", px(&d, 12, 20));
}

#[test]
fn channel_restrictions_keep_the_backdrop() {
    // Blue left out (Photoshop's Advanced Blending › Channels: R, G only).
    let mut d = doc_white(8, 8);
    let mut l = solid_layer("dark", Rect::new(0, 0, 8, 8), [0.2, 0.3, 0.4, 1.0]);
    l.excluded_channels = 0b100;
    d.layers.push(l);
    assert!(close4(px(&d, 1, 1), [0.2, 0.3, 1.0, 1.0]));
    // Adjustment layers honour it too.
    let mut inv = Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert));
    inv.excluded_channels = 0b001;
    d.layers.push(inv);
    assert!(close4(px(&d, 1, 1), [0.2, 0.7, 0.0, 1.0]));
    // CMYK documents composite in display RGB: no exact equivalent, ignored.
    assert_eq!(channel_weights(&d.layers[1], ColorMode::Cmyk), None);
    assert_eq!(channel_weights(&d.layers[1], ColorMode::Rgb), Some([1.0, 1.0, 0.0]));
}

#[test]
fn effect_maps_built_inside_parallel_tiles_do_not_deadlock() {
    // A map large enough for the blur to go multi-threaded, built while rayon renders tiles that
    // all wait on the same map (a rayon-parallel blur deadlocked here).
    let mut d = doc_white(900, 700);
    let mut l = solid_layer("fx", Rect::new(100, 100, 800, 600), [0.2, 0.4, 0.9, 1.0]);
    l.effects.items = vec![photocraft_doc::Effect::default_drop_shadow()];
    if let photocraft_doc::Effect::DropShadow(s) = &mut l.effects.items[0] {
        s.size = 30.0;
    }
    d.layers.push(l);
    let out = render(&d, d.bounds());
    assert_eq!(out.px.len(), 900 * 700);
}

#[test]
fn adjustment_results_are_rounded_to_the_document_depth() {
    for (depth, q) in [(SampleType::U8, Some(255.0f32)), (SampleType::U16, Some(32768.0)), (SampleType::F32, None)] {
        let mut d = Document::with_background("q", Size::new(4, 4), ColorMode::Rgb, depth, Color::rgb(0.3, 0.6, 0.9));
        d.layers.push(Layer::new("lv", LayerContent::Adjustment(Adjustment::Exposure { exposure: 0.37, offset: 0.0, gamma: 1.0 })));
        let p = px(&d, 1, 1);
        match q {
            Some(q) => assert!(p.iter().all(|v| ((v * q).round() - v * q).abs() < 1e-3), "{depth:?} {p:?}"),
            None => assert!(p[..3].iter().any(|v| ((v * 255.0).round() - v * 255.0).abs() > 1e-3), "{p:?}"),
        }
    }
}

// Ground truth captured from Adobe Photoshop 2026 (27.10.0): a 0..255 ramp pushed through modern
// (non-legacy) Brightness/Contrast via `executeAction("BrgC", ... useLegacy=false)`. These pin our
// reverse-engineered curves (see crates/compose/src/adjust.rs modern_* and log/devlog.md).
#[test]
fn modern_contrast_matches_photoshop() {
    // (contrast, [out at x = 0,16,32,64,96,128,160,192,224,255])
    let cases: [(f32, [u8; 10]); 4] = [
        (50.0, [0, 11, 23, 52, 87, 128, 169, 204, 233, 255]),
        (-50.0, [0, 21, 41, 76, 105, 128, 151, 180, 215, 255]),
        (100.0, [0, 5, 14, 40, 78, 128, 178, 216, 242, 255]),
        (-25.0, [0, 19, 37, 70, 101, 128, 155, 186, 220, 255]),
    ];
    let xs = [0usize, 16, 32, 64, 96, 128, 160, 192, 224, 255];
    for (c, out) in cases {
        for (i, &x) in xs.iter().enumerate() {
            let got = adjust::modern_contrast(x as f32 / 255.0, c) * 255.0;
            let err = (got - out[i] as f32).abs();
            assert!(err <= 2.0, "contrast {c} x={x}: got {got:.1} want {} (err {err:.1})", out[i]);
        }
    }
}

#[test]
fn modern_brightness_matches_photoshop() {
    let cases: [(f32, [u8; 10]); 2] = [
        (50.0, [0, 22, 44, 88, 132, 171, 203, 228, 246, 255]),
        (-50.0, [0, 12, 23, 47, 70, 93, 118, 148, 186, 255]),
    ];
    let xs = [0usize, 16, 32, 64, 96, 128, 160, 192, 224, 255];
    for (b, out) in cases {
        for (i, &x) in xs.iter().enumerate() {
            let got = adjust::modern_brightness(x as f32 / 255.0, b) * 255.0;
            let err = (got - out[i] as f32).abs();
            // The brightness roll-off is a spline; our fit is close but approximate at the top.
            assert!(err <= 6.0, "brightness {b} x={x}: got {got:.1} want {} (err {err:.1})", out[i]);
        }
    }
    // Endpoints and the zero slider are exact.
    assert_eq!(adjust::modern_brightness(0.0, 120.0), 0.0);
    assert_eq!(adjust::modern_brightness(1.0, -120.0), 1.0);
    assert_eq!(adjust::modern_brightness(0.37, 0.0), 0.37);
    assert_eq!(adjust::modern_contrast(0.37, 0.0), 0.37);
}

// Levels ground truth from Adobe Photoshop 2026 (adjustLevels on a 0..255 ramp). Verifies our
// `levels()` matches the real app, including the gamma>1 soft shadow toe (initial slope 2^gamma).
#[test]
fn levels_matches_photoshop() {
    let ident = |g: f32| LevelsChannel { in_black: 0.0, in_white: 1.0, gamma: g, out_black: 0.0, out_white: 1.0 };
    // gamma 2.0 (lifts shadows): x -> Photoshop out. The deep shadow is the toe-bounded region.
    let g2: [(usize, u8); 8] = [(1, 4), (8, 30), (16, 55), (32, 90), (64, 128), (128, 181), (192, 221), (224, 239)];
    for (x, ps) in g2 {
        let got = (adjust::levels(&ident(2.0), x as f32 / 255.0) * 255.0).round();
        assert!((got - ps as f32).abs() <= 4.0, "levels gamma 2.0 x={x}: got {got} want {ps}");
    }
    // gamma 0.5 (darkens midtones) is exact: out = (x/255)^2.
    for x in [0usize, 32, 64, 128, 192, 255] {
        let got = (adjust::levels(&ident(0.5), x as f32 / 255.0) * 255.0).round();
        let want = ((x as f32 / 255.0).powi(2) * 255.0).round();
        assert_eq!(got, want, "levels gamma 0.5 x={x}");
    }
    // Endpoints pinned; identity is identity. (The gamma>1 toe's soft-min leaves white within a
    // few parts in 1e5 of 1.0 — invisible at any bit depth; exact at 8-bit.)
    assert_eq!(adjust::levels(&ident(2.5), 0.0), 0.0);
    assert_eq!((adjust::levels(&ident(2.5), 1.0) * 255.0).round(), 255.0);
    assert!((adjust::levels(&ident(2.5), 1.0) - 1.0).abs() < 5e-4);
    assert!((adjust::levels(&ident(1.0), 0.37) - 0.37).abs() < 1e-6);
}
