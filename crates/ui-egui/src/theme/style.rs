//! The design tokens beyond colours: text sizes, spacing, shadows and each widget state's
//! colours and shape, as [`super::apply`] gives them to egui. A theme's `theme.css` sets them like
//! any other token ([`super::css`]); a colour left unset (`None`) follows the theme's colours.

use egui::{Color32, Vec2};

/// A drop shadow; its colour, unset, is the theme's `--shadow`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowTokens {
    pub offset: [i8; 2],
    pub blur: u8,
    pub spread: u8,
    pub color: Option<Color32>,
}

/// One widget state's look. Unset colours come from the theme's colours, as [`super::apply`]
/// describes for each state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WidgetTokens {
    pub bg: Option<Color32>,
    pub border: Option<Color32>,
    pub fg: Option<Color32>,
    pub border_width: f32,
    pub fg_width: f32,
    /// Corner radius (unset: the theme's `--radius-sm`).
    pub radius: Option<f32>,
    /// How far the widget grows in this state (pixels).
    pub expansion: f32,
}

impl WidgetTokens {
    const PLAIN: Self = Self { bg: None, border: None, fg: None, border_width: 1.0, fg_width: 1.0, radius: None, expansion: 0.0 };
}

/// Text labels (`label`) and the four states of buttons, fields and other controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WidgetStates {
    pub label: WidgetTokens,
    pub normal: WidgetTokens,
    pub hover: WidgetTokens,
    pub active: WidgetTokens,
    pub open: WidgetTokens,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StyleTokens {
    pub font_small: f32,
    pub font_body: f32,
    pub font_button: f32,
    pub font_heading: f32,
    pub font_mono: f32,
    pub item_spacing: Vec2,
    pub button_padding: Vec2,
    pub interact_size: Vec2,
    pub slider_width: f32,
    pub combo_width: f32,
    pub menu_margin: f32,
    pub window_margin: f32,
    pub icon_width: f32,
    pub tooltip_width: f32,
    /// Width of the thin floating scroll bars (themes with solid bars ignore it).
    pub scroll_width: f32,
    pub window_shadow: ShadowTokens,
    pub popup_shadow: ShadowTokens,
    /// Selected text and list rows (unset: the accent in Pro and Classic, the soft accent
    /// elsewhere) and their outline (unset: the accent text colour).
    pub selection_bg: Option<Color32>,
    pub selection_stroke: Option<Color32>,
    pub widgets: WidgetStates,
}

impl StyleTokens {
    /// Every value zero or unset: what a stylesheet fills in.
    pub const BLANK: Self = Self {
        font_small: 0.0,
        font_body: 0.0,
        font_button: 0.0,
        font_heading: 0.0,
        font_mono: 0.0,
        item_spacing: Vec2::ZERO,
        button_padding: Vec2::ZERO,
        interact_size: Vec2::ZERO,
        slider_width: 0.0,
        combo_width: 0.0,
        menu_margin: 0.0,
        window_margin: 0.0,
        icon_width: 0.0,
        tooltip_width: 0.0,
        scroll_width: 0.0,
        window_shadow: ShadowTokens { offset: [0, 0], blur: 0, spread: 0, color: None },
        popup_shadow: ShadowTokens { offset: [0, 0], blur: 0, spread: 0, color: None },
        selection_bg: None,
        selection_stroke: None,
        widgets: WidgetStates {
            label: WidgetTokens::PLAIN,
            normal: WidgetTokens::PLAIN,
            hover: WidgetTokens::PLAIN,
            active: WidgetTokens::PLAIN,
            open: WidgetTokens::PLAIN,
        },
    };
}
