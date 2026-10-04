//! Adjustment parameters (data only). Evaluation lives in `photocraft-compose`.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub input: f32,
    pub output: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LevelsChannel {
    pub in_black: f32,
    pub in_white: f32,
    pub gamma: f32,
    pub out_black: f32,
    pub out_white: f32,
}

impl Default for LevelsChannel {
    fn default() -> Self {
        Self { in_black: 0.0, in_white: 1.0, gamma: 1.0, out_black: 0.0, out_white: 1.0 }
    }
}

/// Photoshop adjustment layers (Layer → New Adjustment Layer).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Adjustment {
    BrightnessContrast {
        brightness: f32,
        contrast: f32,
        legacy: bool,
    },
    /// Composite channel first, then R, G, B.
    Levels {
        master: LevelsChannel,
        per_channel: [LevelsChannel; 3],
    },
    /// Master curve then R, G, B curves.
    Curves {
        master: Vec<CurvePoint>,
        per_channel: [Vec<CurvePoint>; 3],
    },
    Exposure {
        exposure: f32,
        offset: f32,
        gamma: f32,
    },
    Vibrance {
        vibrance: f32,
        saturation: f32,
    },
    HueSaturation {
        hue: f32,
        saturation: f32,
        lightness: f32,
        colorize: bool,
    },
    ColorBalance {
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
        preserve_luminosity: bool,
    },
    BlackWhite {
        weights: [f32; 6],
        tint: Option<[f32; 3]>,
    },
    PhotoFilter {
        color: [f32; 3],
        density: f32,
        preserve_luminosity: bool,
    },
    ChannelMixer {
        matrix: [[f32; 4]; 3],
        monochrome: bool,
    },
    /// A 3D LUT: `size`³ RGB triplets, red varying fastest (`((b·size + g)·size + r)·3`), in 0..=1.
    /// `lut` is None for a lookup Photoshop stores as an ICC profile (identity here).
    ColorLookup {
        name: String,
        lut: Option<Arc<Vec<f32>>>,
        size: u32,
        /// Tetrahedral instead of trilinear interpolation.
        #[serde(default)]
        tetrahedral: bool,
        /// Ordered dither of ±½ an 8-bit step to hide banding (Photoshop's "Dither").
        #[serde(default)]
        dither: bool,
    },
    Invert,
    Posterize {
        levels: u32,
    },
    Threshold {
        level: f32,
    },
    GradientMap {
        stops: Vec<(f32, [f32; 3])>,
        reverse: bool,
    },
    /// Per range (reds, yellows, greens, cyans, blues, magentas, whites, neutrals, blacks) the
    /// cyan, magenta, yellow and black change in percent (-100..=100), as in Photoshop.
    SelectiveColor {
        relative: bool,
        adjustments: [[f32; 4]; 9],
    },
    /// A PSD adjustment we can't evaluate yet; preserved raw for round-trip.
    Unsupported {
        psd_key: String,
        raw: Vec<u8>,
    },
}

impl Adjustment {
    pub fn label(&self) -> &str {
        match self {
            Adjustment::BrightnessContrast { .. } => "Brightness/Contrast",
            Adjustment::Levels { .. } => "Levels",
            Adjustment::Curves { .. } => "Curves",
            Adjustment::Exposure { .. } => "Exposure",
            Adjustment::Vibrance { .. } => "Vibrance",
            Adjustment::HueSaturation { .. } => "Hue/Saturation",
            Adjustment::ColorBalance { .. } => "Color Balance",
            Adjustment::BlackWhite { .. } => "Black & White",
            Adjustment::PhotoFilter { .. } => "Photo Filter",
            Adjustment::ChannelMixer { .. } => "Channel Mixer",
            Adjustment::ColorLookup { .. } => "Color Lookup",
            Adjustment::Invert => "Invert",
            Adjustment::Posterize { .. } => "Posterize",
            Adjustment::Threshold { .. } => "Threshold",
            Adjustment::GradientMap { .. } => "Gradient Map",
            Adjustment::SelectiveColor { .. } => "Selective Color",
            Adjustment::Unsupported { .. } => "Adjustment",
        }
    }

    pub fn default_hue_saturation() -> Self {
        Adjustment::HueSaturation { hue: 0.0, saturation: 0.0, lightness: 0.0, colorize: false }
    }

    pub fn identity_curves() -> Self {
        let line = || vec![CurvePoint { input: 0.0, output: 0.0 }, CurvePoint { input: 1.0, output: 1.0 }];
        Adjustment::Curves { master: line(), per_channel: [line(), line(), line()] }
    }

    pub fn identity_levels() -> Self {
        Adjustment::Levels { master: LevelsChannel::default(), per_channel: Default::default() }
    }
}
