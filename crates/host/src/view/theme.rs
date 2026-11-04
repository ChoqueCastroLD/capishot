//! The host's look for the canvas chrome.

use egui::{Color32, Visuals};
use photocraft_ui_egui::embed;
use photocraft_ui_egui::theme::Tokens;

/// The colours the canvas chrome takes from the host: the pasteboard around the image, rulers,
/// transform handles, tool feedback, the editor's own cards. Text uses the host's own fonts (the
/// widget installs none). Built from the host's egui visuals by default
/// ([`HostTheme::from_visuals`]); set fields to override.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostTheme {
    /// A dark theme (dark editor cards, light text on the rulers).
    pub dark: bool,
    /// Selection, active handles, focus rings.
    pub accent: Color32,
    /// Text on an accent fill.
    pub accent_text: Color32,
    /// The area around the image.
    pub pasteboard: Color32,
    /// The pasteboard's dot pattern; `None` keeps the built-in theme's.
    pub pasteboard_dots: Option<Color32>,
    /// Ruler and bar backgrounds.
    pub chrome: Color32,
    /// The editor's cards (progress, prompts, menus on the canvas).
    pub card: Color32,
    /// Card and field borders.
    pub border: Color32,
    /// Text.
    pub text: Color32,
    /// Secondary text (ruler numbers, hints).
    pub text_dim: Color32,
    /// Errors.
    pub danger: Color32,
    /// Warnings.
    pub warning: Color32,
}

impl HostTheme {
    /// The host's look as egui knows it.
    pub fn from_visuals(v: &Visuals) -> Self {
        HostTheme {
            dark: v.dark_mode,
            accent: v.selection.bg_fill,
            accent_text: v.selection.stroke.color,
            pasteboard: v.extreme_bg_color,
            pasteboard_dots: None,
            chrome: v.panel_fill,
            card: v.window_fill,
            border: v.window_stroke.color,
            text: v.text_color(),
            text_dim: v.weak_text_color(),
            danger: v.error_fg_color,
            warning: v.warn_fg_color,
        }
    }

    /// Capishot's colour tokens for this theme.
    pub fn tokens(&self, v: &Visuals) -> Tokens {
        let mut t = embed::tokens_for_visuals(v);
        t.dark = self.dark;
        t.accent = self.accent;
        t.accent_text = self.accent_text;
        t.canvas = self.pasteboard;
        if let Some(dots) = self.pasteboard_dots {
            t.canvas_dot = dots;
        }
        t.chrome = self.chrome;
        t.card = self.card;
        t.card_border = self.border;
        t.field_border = self.border;
        t.text = self.text;
        t.text_dim = self.text_dim;
        t.danger = self.danger;
        t.warning = self.warning;
        t
    }
}
