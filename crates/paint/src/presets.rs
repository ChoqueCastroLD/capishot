//! Built-in brush presets (our own designs; sampled tips and textures are generated procedurally).

use crate::brush::*;
use crate::procedural::{bristle_tip, chalk_tip, spatter_tip};

fn preset(name: &str, brush: BrushSettings) -> BrushPreset {
    BrushPreset { name: name.to_string(), brush, builtin: true }
}

/// The built-in preset set, in display order.
pub fn builtin() -> Vec<BrushPreset> {
    let base = BrushSettings { pressure_size: false, spacing: 0.25, ..Default::default() };
    vec![
        preset("Hard Round", BrushSettings { size: 30.0, hardness: 1.0, ..base.clone() }),
        preset("Soft Round", BrushSettings { size: 45.0, hardness: 0.0, ..base.clone() }),
        preset(
            "Hard Round Pressure Size",
            BrushSettings {
                size: 30.0,
                hardness: 1.0,
                shape_dynamics: ShapeDynamics { enabled: true, size: Dynamic::controlled(Control::PenPressure), ..Default::default() },
                ..base.clone()
            },
        ),
        preset(
            "Soft Round Pressure Opacity",
            BrushSettings {
                size: 45.0,
                hardness: 0.0,
                transfer: Transfer { enabled: true, opacity: Dynamic::controlled(Control::PenPressure), ..Default::default() },
                ..base.clone()
            },
        ),
        preset("Airbrush Soft", BrushSettings { size: 80.0, hardness: 0.0, flow: 0.1, spacing: 0.1, build_up: true, build_up_rate: 25.0, ..base.clone() }),
        preset("Hard Pencil", BrushSettings { size: 3.0, hardness: 1.0, aliased: true, spacing: 0.1, ..base.clone() }),
        preset("Calligraphy Flat", BrushSettings { size: 28.0, hardness: 1.0, roundness: 0.2, angle: 45.0, spacing: 0.05, ..base.clone() }),
        preset(
            "Chalk",
            BrushSettings {
                size: 40.0,
                tip: TipShape::Sampled(chalk_tip(64, 7)),
                spacing: 0.2,
                shape_dynamics: ShapeDynamics {
                    enabled: true,
                    angle: Dynamic::jitter(1.0),
                    size: Dynamic { jitter: 0.15, ..Default::default() },
                    ..Default::default()
                },
                texture: Texture {
                    enabled: true,
                    pattern: Pattern::Procedural { style: PatternStyle::Paper, size: 128, seed: 3 },
                    depth: 0.6,
                    ..Default::default()
                },
                ..base.clone()
            },
        ),
        preset(
            "Spatter",
            BrushSettings {
                size: 60.0,
                tip: TipShape::Sampled(spatter_tip(64, 11, 14)),
                spacing: 0.6,
                shape_dynamics: ShapeDynamics {
                    enabled: true,
                    size: Dynamic { jitter: 0.6, minimum: 0.2, ..Default::default() },
                    angle: Dynamic::jitter(1.0),
                    flip_x_jitter: true,
                    flip_y_jitter: true,
                    ..Default::default()
                },
                scattering: Scattering { enabled: true, scatter: Dynamic::jitter(1.5), both_axes: true, count: 2, count_jitter: Dynamic::jitter(0.5) },
                ..base.clone()
            },
        ),
        preset(
            "Dry Bristle",
            BrushSettings {
                size: 50.0,
                tip: TipShape::Sampled(bristle_tip(64, 5)),
                spacing: 0.04,
                shape_dynamics: ShapeDynamics { enabled: true, angle: Dynamic::controlled(Control::Direction), ..Default::default() },
                transfer: Transfer {
                    enabled: true,
                    flow: Dynamic { control: Control::Fade, fade_steps: 400, minimum: 0.1, ..Default::default() },
                    ..Default::default()
                },
                ..base.clone()
            },
        ),
        preset(
            "Watercolor Wet Edges",
            BrushSettings {
                size: 60.0,
                hardness: 0.3,
                wet_edges: true,
                texture: Texture {
                    enabled: true,
                    pattern: Pattern::Procedural { style: PatternStyle::Paper, size: 256, seed: 9 },
                    depth: 0.35,
                    scale: 1.5,
                    ..Default::default()
                },
                ..base.clone()
            },
        ),
        preset(
            "Canvas Texture",
            BrushSettings {
                size: 50.0,
                hardness: 0.8,
                texture: Texture {
                    enabled: true,
                    pattern: Pattern::Procedural { style: PatternStyle::Canvas, size: 64, seed: 1 },
                    mode: MaskMode::Subtract,
                    depth: 0.7,
                    ..Default::default()
                },
                ..base.clone()
            },
        ),
        preset(
            "Confetti",
            BrushSettings {
                size: 24.0,
                hardness: 1.0,
                roundness: 0.5,
                spacing: 1.2,
                shape_dynamics: ShapeDynamics { enabled: true, angle: Dynamic::jitter(1.0), size: Dynamic::jitter(0.5), ..Default::default() },
                scattering: Scattering { enabled: true, scatter: Dynamic::jitter(3.0), both_axes: true, count: 3, ..Default::default() },
                color_dynamics: ColorDynamics { enabled: true, per_tip: true, hue_jitter: 1.0, saturation_jitter: 0.2, purity: 0.5, ..Default::default() },
                ..base.clone()
            },
        ),
        preset(
            "Dual Grain",
            BrushSettings {
                size: 50.0,
                hardness: 0.6,
                noise: true,
                dual_brush: DualBrush {
                    enabled: true,
                    tip: TipShape::Sampled(spatter_tip(48, 23, 20)),
                    size: 30.0,
                    spacing: 0.3,
                    scatter: 1.0,
                    both_axes: true,
                    count: 2,
                    flip: true,
                    ..Default::default()
                },
                ..base
            },
        ),
    ]
}

/// Find a preset by name (case-insensitive).
pub fn find<'a>(presets: &'a [BrushPreset], name: &str) -> Option<&'a BrushPreset> {
    presets.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}
