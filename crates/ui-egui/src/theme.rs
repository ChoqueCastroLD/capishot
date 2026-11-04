//! Design system: themes, colour tokens, radii, typography.
//!
//! - **Studio**: near-black surfaces, rounded cards, Inter + JetBrains Mono, soft violet
//!   accent. Modelled on the look of modern pro editors (such as Photoshop 2025).
//! - **Studio Light**: the same system on light surfaces.
//! - **Classic**: a deliberately Windows-2000-era look (grey bevels, square corners, navy selection)
//!   for people who prefer it.
//! - **Adwaita** (Light and Dark): the Studio layout in GNOME's libadwaita palette, so the app sits
//!   with the rest of a GNOME desktop.
//! - **Solarized Dark**: Ethan Schoonover's Solarized palette (base03/base02 surfaces, base0 text,
//!   Solarized blue accent) with the Studio layout.
//! - **Breeze** (Light and Dark): the Studio layout adapted to the look of KDE Plasma's Breeze
//!   (blue-grey surfaces, Breeze blue accent, 5 px corners), so the app sits with the rest of a
//!   Plasma desktop.
//!
//! - **Pro (Light Gray)** and **Pro (Light)**: Photoshop's third and fourth interface brightness
//!   levels (#B8B8B8 and #F0F0F0 panels) with the Pro layout (#2661).
//!
//! Each theme is a `theme.css` stylesheet ([`css`]): the built-in ones are compiled in from
//! `theme/builtin/<id>.theme.css` ([`builtin_css`]), the user's own live in the preferences
//! (Preferences › Profile & Themes). Widgets read [`Tokens::get`] instead of hard-coding colours,
//! so every theme applies everywhere.

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Visuals};
use photocraft_engine::prefs::UiFontSize;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub mod css;
mod style;
pub use style::{ShadowTokens, StyleTokens, WidgetStates, WidgetTokens};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeKind {
    /// Photoshop-style Spectrum dark: flat charcoal panels, tab strips, blue accents (darkest brightness).
    Pro,
    /// Photoshop's default (second) interface brightness: #535353 panels, #282828 canvas (default).
    #[default]
    ProMedium,
    /// Photoshop's third interface brightness: #B8B8B8 panels (#2661).
    ProLightGray,
    /// Photoshop's fourth (lightest) interface brightness: #F0F0F0 panels (#2661).
    ProLight,
    Studio,
    StudioLight,
    Classic,
    /// GNOME's libadwaita light style.
    Adwaita,
    /// GNOME's libadwaita dark style.
    AdwaitaDark,
    /// Solarized Dark (Ethan Schoonover's palette): the base03 background, base02 panels and
    /// base0 body text, with the Solarized blue accent.
    SolarizedDark,
    /// An adaptation of KDE Plasma's Breeze Light look.
    BreezeLight,
    /// An adaptation of KDE Plasma's Breeze Dark look.
    BreezeDark,
    /// A low-contrast dark theme: muted violet-grey surfaces, softened text and a periwinkle accent.
    SoftDark,
    /// A low-contrast light theme: pale violet-grey surfaces, softened dark text and a periwinkle accent.
    SoftLight,
}

impl ThemeKind {
    pub const ALL: [ThemeKind; 14] = [
        ThemeKind::Pro,
        ThemeKind::ProMedium,
        ThemeKind::ProLightGray,
        ThemeKind::ProLight,
        ThemeKind::Studio,
        ThemeKind::StudioLight,
        ThemeKind::Classic,
        ThemeKind::SolarizedDark,
        ThemeKind::Adwaita,
        ThemeKind::AdwaitaDark,
        ThemeKind::BreezeLight,
        ThemeKind::BreezeDark,
        ThemeKind::SoftDark,
        ThemeKind::SoftLight,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ThemeKind::Pro => "Pro (Dark)",
            ThemeKind::ProMedium => "Pro (Medium Gray)",
            ThemeKind::ProLightGray => "Pro (Light Gray)",
            ThemeKind::ProLight => "Pro (Light)",
            ThemeKind::Studio => "Studio (Dark)",
            ThemeKind::StudioLight => "Studio (Light)",
            ThemeKind::Classic => "Classic",
            ThemeKind::Adwaita => "Adwaita (Light)",
            ThemeKind::AdwaitaDark => "Adwaita (Dark)",
            ThemeKind::SolarizedDark => "Solarized Dark",
            ThemeKind::BreezeLight => "Breeze Light",
            ThemeKind::BreezeDark => "Breeze Dark",
            ThemeKind::SoftDark => "Soft Dark",
            ThemeKind::SoftLight => "Soft Light",
        }
    }
    /// The canonical name: `ui.set {theme}` accepts it and the Window › Theme commands are
    /// `window.theme.<id>`.
    pub fn id(self) -> &'static str {
        match self {
            ThemeKind::Pro => "pro",
            ThemeKind::ProMedium => "proMedium",
            ThemeKind::ProLightGray => "proLightGray",
            ThemeKind::ProLight => "proLight",
            ThemeKind::Studio => "studio",
            ThemeKind::StudioLight => "studioLight",
            ThemeKind::Classic => "classic",
            ThemeKind::Adwaita => "adwaita",
            ThemeKind::AdwaitaDark => "adwaitaDark",
            ThemeKind::SolarizedDark => "solarizedDark",
            ThemeKind::BreezeLight => "breezeLight",
            ThemeKind::BreezeDark => "breezeDark",
            ThemeKind::SoftDark => "softDark",
            ThemeKind::SoftLight => "softLight",
        }
    }
    /// Photoshop-grammar layout (tab strips, flat panels): the four Pro brightness levels.
    pub fn is_pro(self) -> bool {
        matches!(self, ThemeKind::Pro | ThemeKind::ProMedium | ThemeKind::ProLightGray | ThemeKind::ProLight)
    }
    /// Whether this theme belongs to the dark appearance family.
    pub fn is_dark(self) -> bool {
        matches!(
            self,
            ThemeKind::Studio
                | ThemeKind::Pro
                | ThemeKind::ProMedium
                | ThemeKind::SolarizedDark
                | ThemeKind::AdwaitaDark
                | ThemeKind::BreezeDark
                | ThemeKind::SoftDark
        )
    }
    /// Whether this theme belongs to the light appearance family.
    pub fn is_light(self) -> bool {
        !self.is_dark()
    }
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|k| *k == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace([' ', '_', '-', '(', ')'], "").as_str() {
            "pro" | "prodark" | "photoshop" | "dark" => Some(ThemeKind::Pro),
            "promedium" | "promediumgray" | "medium" | "mediumgray" => Some(ThemeKind::ProMedium),
            "prolightgray" | "lightgray" | "mediumlight" => Some(ThemeKind::ProLightGray),
            "prolight" | "prolightest" | "lightest" | "light" => Some(ThemeKind::ProLight),
            "studio" | "studiodark" => Some(ThemeKind::Studio),
            "studiolight" => Some(ThemeKind::StudioLight),
            "classic" | "win2000" | "retro" => Some(ThemeKind::Classic),
            "adwaita" | "adwaitalight" | "gnome" | "gnomelight" => Some(ThemeKind::Adwaita),
            "adwaitadark" | "gnomedark" => Some(ThemeKind::AdwaitaDark),
            "solarizeddark" | "solarized" => Some(ThemeKind::SolarizedDark),
            "breezelight" | "breeze" | "kde" | "kdelight" | "plasma" | "plasmalight" => Some(ThemeKind::BreezeLight),
            "breezedark" | "kdedark" | "plasmadark" => Some(ThemeKind::BreezeDark),
            "softdark" => Some(ThemeKind::SoftDark),
            "softlight" => Some(ThemeKind::SoftLight),
            _ => None,
        }
    }
}

/// A built-in theme's own `theme.css` (`theme/builtin/<id>.theme.css`).
pub fn builtin_css(kind: ThemeKind) -> &'static str {
    match kind {
        ThemeKind::Pro => include_str!("theme/builtin/pro.theme.css"),
        ThemeKind::ProMedium => include_str!("theme/builtin/proMedium.theme.css"),
        ThemeKind::ProLightGray => include_str!("theme/builtin/proLightGray.theme.css"),
        ThemeKind::ProLight => include_str!("theme/builtin/proLight.theme.css"),
        ThemeKind::Studio => include_str!("theme/builtin/studio.theme.css"),
        ThemeKind::StudioLight => include_str!("theme/builtin/studioLight.theme.css"),
        ThemeKind::Classic => include_str!("theme/builtin/classic.theme.css"),
        ThemeKind::Adwaita => include_str!("theme/builtin/adwaita.theme.css"),
        ThemeKind::AdwaitaDark => include_str!("theme/builtin/adwaitaDark.theme.css"),
        ThemeKind::SolarizedDark => include_str!("theme/builtin/solarizedDark.theme.css"),
        ThemeKind::BreezeLight => include_str!("theme/builtin/breezeLight.theme.css"),
        ThemeKind::BreezeDark => include_str!("theme/builtin/breezeDark.theme.css"),
        ThemeKind::SoftDark => include_str!("theme/builtin/softDark.theme.css"),
        ThemeKind::SoftLight => include_str!("theme/builtin/softLight.theme.css"),
    }
}

/// Colour and shape tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub kind: ThemeKind,
    /// Window chrome (title bar, toolbars).
    pub chrome: Color32,
    /// Canvas surround.
    pub canvas: Color32,
    /// Dot colour of the canvas grid pattern.
    pub canvas_dot: Color32,
    /// Dock background (behind cards).
    pub dock: Color32,
    /// Cards / panels.
    pub card: Color32,
    pub card_border: Color32,
    /// Inputs, fields, dropdowns.
    pub field: Color32,
    pub field_border: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub icon: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub accent_border: Color32,
    pub accent_text: Color32,
    pub separator: Color32,
    pub shadow: Color32,
    pub primary_bg: Color32,
    pub primary_text: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub radius_sm: f32,
    pub radius: f32,
    pub radius_lg: f32,
    /// Classic theme draws 3D bevels instead of flat fills.
    pub bevel: bool,
    /// Pro (Photoshop-grammar) layout: tab strips, flat panels, checkboxes, pill buttons.
    pub pro: bool,
    /// Toolbar colour chips are large and round, stacked, with the colour buttons below (Studio).
    pub round_chips: bool,
    /// Panel tab-strip background (Pro).
    pub tab_strip: Color32,
    /// Selected list row (layers, history).
    pub row_selected: Color32,
    /// Scope plot background (histogram, vectorscope, clipping diagnostics).
    pub histogram_bg: Color32,
    /// Channel intensity of scope outlines and hue markers.
    pub histogram_level: u8,
    /// The custom title bar's Close button while hovered (Windows' red), and its glyph.
    pub caption_close: Color32,
    pub caption_close_text: Color32,
    /// Dims the main window behind a modal that takes all input (Camera Raw).
    pub scrim: Color32,
    /// Whether this is a dark theme (egui's dark or light base style, and the dark-only tweaks).
    pub dark: bool,
    /// Text sizes, spacing, shadows and widget states.
    pub style: StyleTokens,
}

impl Tokens {
    /// Tool outlines need contrast on arbitrary document pixels, independently of UI theme.
    pub(crate) fn cursor_outline() -> [Color32; 2] {
        [Color32::from_black_alpha(160), Color32::from_white_alpha(235)]
    }

    /// Every token zero or transparent: what a built-in theme's stylesheet fills in.
    pub(crate) fn blank(kind: ThemeKind) -> Self {
        let c = Color32::TRANSPARENT;
        Tokens {
            kind,
            chrome: c,
            canvas: c,
            canvas_dot: c,
            dock: c,
            card: c,
            card_border: c,
            field: c,
            field_border: c,
            hover: c,
            pressed: c,
            text: c,
            text_dim: c,
            text_faint: c,
            icon: c,
            accent: c,
            accent_soft: c,
            accent_border: c,
            accent_text: c,
            separator: c,
            shadow: c,
            primary_bg: c,
            primary_text: c,
            danger: c,
            warning: c,
            radius_sm: 0.0,
            radius: 0.0,
            radius_lg: 0.0,
            bevel: false,
            pro: false,
            round_chips: false,
            tab_strip: c,
            row_selected: c,
            histogram_bg: c,
            histogram_level: 0,
            caption_close: c,
            caption_close_text: c,
            scrim: c,
            dark: false,
            style: StyleTokens::BLANK,
        }
    }

    /// A built-in theme's tokens, read from its embedded `theme.css` (once).
    pub fn for_kind(kind: ThemeKind) -> Self {
        static BUILT_IN: std::sync::LazyLock<Vec<Tokens>> =
            std::sync::LazyLock::new(|| ThemeKind::ALL.iter().map(|k| css::parse_builtin(builtin_css(*k), *k).tokens).collect());
        let i = ThemeKind::ALL.iter().position(|k| *k == kind).unwrap_or(0);
        BUILT_IN.get(i).copied().unwrap_or_else(|| Tokens::blank(kind))
    }

    /// Tokens for the active theme (stored in egui's context data by [`apply`]).
    pub fn get(ctx: &egui::Context) -> Tokens {
        ctx.data(|d| d.get_temp::<Tokens>(egui::Id::unique("photocraft-theme"))).unwrap_or_else(|| Tokens::for_kind(ThemeKind::Studio))
    }

    /// Thin RGB outlines; the filled bands use the same hues with subdued coverage.
    pub fn histogram_color(&self, mask: u8) -> Color32 {
        let v = self.histogram_level;
        Color32::from_rgb(if mask & 1 != 0 { v } else { 0 }, if mask & 2 != 0 { v } else { 0 }, if mask & 4 != 0 { v } else { 0 })
    }

    pub fn histogram_fill(&self, mask: u8) -> Color32 {
        self.histogram_color(mask).gamma_multiply(0.3)
    }

    /// Semantic label hues stay recognisable across themes; None keeps the normal row surface.
    pub fn layer_label_colors(&self, label: photocraft_doc::LabelColor) -> Option<(Color32, Color32)> {
        use photocraft_doc::LabelColor;
        let (swatch, eye) = match label {
            LabelColor::None => return None,
            LabelColor::Red => ([0xFC, 0x5D, 0x5B], [0x9F, 0x2F, 0x30]),
            LabelColor::Orange => ([0xF7, 0x97, 0x44], [0x93, 0x4F, 0x0C]),
            LabelColor::Yellow => ([0xDC, 0xD6, 0x4B], [0x99, 0x78, 0x0C]),
            LabelColor::Green => ([0x85, 0xDC, 0x6A], [0x4E, 0x71, 0x2E]),
            LabelColor::Seafoam => ([0x1C, 0x84, 0x88], [0x0B, 0x54, 0x4F]),
            LabelColor::Blue => ([0x78, 0xAD, 0xF6], [0x41, 0x5B, 0x87]),
            LabelColor::Indigo => ([0x54, 0x4B, 0xE7], [0x36, 0x34, 0x8E]),
            LabelColor::Magenta => ([0xCC, 0x1A, 0x7E], [0x98, 0x1B, 0x51]),
            LabelColor::Fuchsia => ([0xB0, 0x14, 0xC0], [0x71, 0x0F, 0x74]),
            LabelColor::Violet => ([0x91, 0x76, 0xD5], [0x5D, 0x3F, 0x8E]),
            LabelColor::Gray => ([0x9C, 0x9C, 0x9C], [0x57, 0x57, 0x57]),
        };
        let rgb = |[r, g, b]: [u8; 3]| Color32::from_rgb(r, g, b);
        Some((rgb(swatch), rgb(eye)))
    }

    pub fn layer_label_icon(&self, label: photocraft_doc::LabelColor) -> Color32 {
        if label == photocraft_doc::LabelColor::None { self.icon } else { Color32::from_gray(226) }
    }

    /// Analysis colours are semantic hues, independent of the application accent palette.
    pub fn scope_hue(&self, h: f32, s: f32) -> Color32 {
        let v = f32::from(self.histogram_level) / 255.0;
        let f = |offset: f32| {
            let k = (offset + h * 6.0).rem_euclid(6.0);
            (v * (1.0 - s * k.min(4.0 - k).clamp(0.0, 1.0)) * 255.0) as u8
        };
        Color32::from_rgb(f(5.0), f(3.0), f(1.0))
    }

    pub fn histogram_background(&self) -> Color32 {
        self.histogram_bg
    }

    /// The background behind selected text in a text field (#3789): a subtle shade rather than
    /// egui's default blue. The monochrome Pro themes use a grey so a selection doesn't read as a
    /// second accent; coloured themes keep their own accent. Scoped to text fields by
    /// [`crate::widgets::text_field`], because `Visuals::selection` also fills selected list rows
    /// and slider trails, which must keep the theme's.
    pub fn field_selection(&self) -> Color32 {
        if self.pro { Color32::from_gray(if self.dark() { 96 } else { 201 }) } else { self.accent_soft }
    }

    /// The 1 px ring round a slider knob, if the theme draws one: the Pro layouts' dark ring, and
    /// in the light themes the dim text colour, so their white knob stands out from a pale panel
    /// (#7927). The dark Studio-layout themes need none; Classic draws a bevelled knob instead.
    pub fn knob_outline(&self) -> Option<Color32> {
        if self.pro {
            Some(Color32::from_gray(40))
        } else if self.dark() {
            None
        } else {
            Some(self.text_dim)
        }
    }

    pub fn dark(&self) -> bool {
        self.dark
    }
}

/// Register Inter (UI) and JetBrains Mono (numbers) plus named weights.
pub fn install_fonts(ctx: &egui::Context) {
    install_fonts_with(ctx, crate::cjk_fonts::Sources::system());
}

/// [`install_fonts`] with the CJK and other-script fallback fonts taken from `cjk` (tests swap the sources).
pub fn install_fonts_with(ctx: &egui::Context, cjk: crate::cjk_fonts::Sources) {
    // egui's default fonts stay last in the stack, for symbols and emoji Inter lacks.
    // egui's own default fonts only where egui is built with them (the desktop): the web build
    // leaves out Ubuntu Light and Hack, and `add_symbol_fonts` adds the two emoji/icon fonts.
    let mut fonts = FontDefinitions::default();
    // The bundled fonts are inflated on first use (photocraft_text::fonts). Should one ever fail
    // to inflate (empty bytes), it is left out and egui's default fonts draw that text.
    let add = |fonts: &mut FontDefinitions, name: &str, bytes: &'static [u8]| -> Option<String> {
        if bytes.is_empty() {
            log::error!("bundled font {name} is unavailable; using the default UI font");
            return None;
        }
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
        Some(name.to_owned())
    };
    let inter = add(&mut fonts, "Inter", photocraft_text::fonts::INTER_REGULAR.as_slice());
    let medium = add(&mut fonts, "Inter-Medium", photocraft_text::fonts::INTER_MEDIUM.as_slice());
    let semibold = add(&mut fonts, "Inter-SemiBold", photocraft_text::fonts::INTER_SEMIBOLD.as_slice());
    let mono = add(&mut fonts, "JetBrainsMono", photocraft_text::fonts::JETBRAINS_MONO_REGULAR.as_slice());
    if let Some(inter) = inter {
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, inter);
    }
    if let Some(mono) = mono {
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, mono);
    }
    add_symbol_fonts(&mut fonts);
    // The optional craft-fonts input carries Japanese, Simplified Chinese, Korean, Arabic, Thai and
    // other faces. They are not registered into this static stack: `cjk_fonts` installs a
    // per-character font provider for them, so a broad Simplified Chinese face is not put in the
    // family stack ahead of the Japanese or Korean face, and a locale without a Traditional Chinese
    // craft face leaves Han to the system font. (A single linear per-family chain still decides a
    // glyph once a broad face is discovered; see `cjk_fonts`.)
    // Served script fallbacks the web build has received (Arabic, Japanese…), after everything else.
    for (name, data) in crate::served_fonts::ui_faces() {
        fonts.font_data.insert(name.clone(), data);
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.clone());
        }
    }
    // Named weights fall back to the default stack for missing glyphs.
    let fallback: Vec<String> = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (fam, primary) in [("medium", medium), ("semibold", semibold)] {
        let mut stack: Vec<String> = primary.into_iter().collect();
        stack.extend(fallback.iter().cloned());
        fonts.families.insert(FontFamily::Name(fam.into()), stack);
    }
    let size = ui_font_size(ctx);
    for (name, data) in &mut fonts.font_data {
        size_ui_font(ctx, name, Arc::make_mut(data));
    }
    ctx.set_fonts(fonts);
    NAMED_WEIGHTS.store(true, std::sync::atomic::Ordering::Relaxed);
    // egui keeps the first plugin of a type, so a re-install (language change) resets the
    // existing one. The new definitions land next pass: the plugin must not rescale before then.
    if ctx
        .with_plugin::<UiFontSizePlugin, _>(|plugin| {
            plugin.applied = size;
            plugin.defer_after_install = true;
        })
        .is_none()
    {
        ctx.add_plugin(UiFontSizePlugin { applied: size, defer_after_install: true });
    }
    // CJK, Thai, Arabic, Indic and other script fallback fonts (craft-fonts' Japanese ones if built in, then
    // the system's) are registered on demand (cjk_fonts.rs).
    crate::cjk_fonts::install_with(ctx, cjk);
}

/// Without egui's `default_fonts` feature (the web build: Ubuntu-Light and Hack are never drawn
/// with, Inter and JetBrains Mono come first) the defaults are empty, so the two fonts egui's
/// widgets and Capishot's labels take symbols from are added here, with egui's own tweaks: Noto
/// Emoji, then the emoji icon font. Inter backs JetBrains Mono for the few arrows it lacks (`↵`).
/// With the defaults (the desktop app) nothing changes.
fn add_symbol_fonts(fonts: &mut FontDefinitions) {
    use egui::FontTweak;
    let symbols = [("NotoEmoji-Regular", epaint_default_fonts::NOTO_EMOJI_REGULAR, 0.81), ("emoji-icon-font", epaint_default_fonts::EMOJI_ICON, 0.90)];
    let had_mono_fallback = fonts.font_data.contains_key("Hack");
    for (name, bytes, scale) in symbols {
        if fonts.font_data.contains_key(name) {
            continue;
        }
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes).tweak(FontTweak { scale, ..Default::default() })));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.to_owned());
        }
    }
    if !had_mono_fallback && fonts.font_data.contains_key("Inter") {
        let mono = fonts.families.entry(FontFamily::Monospace).or_default();
        if !mono.iter().any(|f| f == "Inter") {
            let at = usize::from(mono.first().is_some_and(|f| f == "JetBrainsMono"));
            mono.insert(at, "Inter".to_owned());
        }
    }
}

/// Has [`install_fonts`]'s stack reached the active fonts? When a host defers registration to
/// its first pass, the active fonts are still egui's defaults. Rescaling a copy of those would
/// overwrite the queued stack.
fn ui_fonts_active(ctx: &egui::Context) -> bool {
    ctx.fonts(|f| f.definitions().families.contains_key(&FontFamily::Name("medium".into())))
}

fn ui_fonts_id() -> egui::Id {
    egui::Id::unique("photocraft-ui-fonts")
}

fn ui_font_size(ctx: &egui::Context) -> UiFontSize {
    ctx.data(|d| d.get_temp::<UiFontSize>(ui_fonts_id())).unwrap_or_default()
}

/// The Interface > UI Font Size multiplier for the current context (including the default Small).
pub(crate) fn ui_font_scale(ctx: &egui::Context) -> f32 {
    font_scale(ui_font_size(ctx))
}

fn font_scale(size: UiFontSize) -> f32 {
    match size {
        UiFontSize::Tiny => 10.0 / 12.0,
        UiFontSize::Small => 1.0,
        UiFontSize::Medium => 14.0 / 12.0,
        UiFontSize::Large => 16.0 / 12.0,
    }
}

/// Register the original multiplier before sizing a face, including lazy CJK fallbacks.
pub(crate) fn size_ui_font(ctx: &egui::Context, name: &str, data: &mut FontData) {
    ctx.data_mut(|d| d.insert_temp(ui_fonts_id().with(name), data.tweak.scale));
    data.tweak.scale *= font_scale(ui_font_size(ctx));
}

/// Apply Interface › UI Font Size independently of display/canvas zoom. Scaling the registered
/// faces covers explicit RichText and painter font sizes as well as egui's text styles. egui
/// 0.36 uses these tweaks in shaping and row metrics; the layout tests below guard that contract.
pub(crate) fn set_ui_font_size(ctx: &egui::Context, size: UiFontSize) {
    if ui_font_size(ctx) == size {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(ui_fonts_id(), size));
    ctx.request_repaint();
}

/// Font access is valid after the first pass, including when preferences load before it.
/// Keep only the original multipliers, not a second copy of large system font files.
struct UiFontSizePlugin {
    applied: UiFontSize,
    /// `set_fonts` takes effect next pass: skip one hook so the rescale starts from that stack
    /// and not from the one it replaces.
    defer_after_install: bool,
}

impl egui::Plugin for UiFontSizePlugin {
    fn debug_name(&self) -> &'static str {
        "photocraft-ui-font-size"
    }

    /// Rescale only active font definitions, after any pending installation has landed, and
    /// keep on-demand fallback faces at the same scale as the next pass's registered fonts.
    fn output_hook(&mut self, ctx: &egui::Context, _output: &mut egui::FullOutput) {
        let size = ui_font_size(ctx);
        let deferred = std::mem::take(&mut self.defer_after_install);
        if size == self.applied {
            return;
        }
        if deferred || !ui_fonts_active(ctx) {
            // A host may register fonts and load the saved size in the same pass. Cloning the
            // active definitions now would discard that queued stack.
            ctx.request_repaint();
            return;
        }
        // Start from the current stack so lazily registered fallback faces are retained.
        let mut fonts = ctx.fonts(|f| f.definitions().clone());
        for (name, data) in &mut fonts.font_data {
            let id = ui_fonts_id().with(name);
            let original = ctx.data(|d| d.get_temp::<f32>(id)).unwrap_or(data.tweak.scale / font_scale(self.applied));
            ctx.data_mut(|d| d.insert_temp(id, original));
            Arc::make_mut(data).tweak.scale = original * font_scale(size);
        }
        ctx.set_fonts(fonts);
        // The provider must use the scale of the definitions that were active while this pass
        // laid out text. A face it discovers during the pass is inserted at that old scale; after
        // `set_fonts` the rescaled definitions take effect and the provider follows them.
        crate::cjk_fonts::set_craft_font_scale(ctx, font_scale(size));
        self.applied = size;
        ctx.request_repaint();
    }
}

/// Whether [`install_fonts`] registered the named weights (`medium`, `semibold`) in this
/// process. The embedded canvas (`embed.rs`) leaves the host's fonts alone, so they don't exist
/// there and naming them would make egui panic: [`medium`] and [`semibold`] fall back to the
/// host's proportional font.
static NAMED_WEIGHTS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn named_weight(size: f32, name: &str) -> FontId {
    if NAMED_WEIGHTS.load(std::sync::atomic::Ordering::Relaxed) {
        FontId::new(size, FontFamily::Name(name.into()))
    } else {
        FontId::proportional(size)
    }
}

pub fn medium(size: f32) -> FontId {
    named_weight(size, "medium")
}
pub fn semibold(size: f32) -> FontId {
    named_weight(size, "semibold")
}
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// Publish `t` for Capishot's own drawing ([`Tokens::get`]) without touching egui's style,
/// visuals, fonts or theme preference: the embedded canvas (`embed.rs`) draws with a host's
/// colours inside the host's own look.
pub fn publish_tokens(ctx: &egui::Context, t: Tokens) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("photocraft-theme"), t));
}

/// Apply a built-in theme to egui's global style and publish its tokens.
pub fn apply(ctx: &egui::Context, kind: ThemeKind) {
    apply_tokens(ctx, Tokens::for_kind(kind));
}

/// Apply tokens (a built-in theme's or one read from a `theme.css`) to egui's global style and
/// publish them. Unset widget colours follow the theme's: fields' colours for controls at rest,
/// `--hover` and `--pressed` (with the accent border) when hovered and pressed, the panels' for
/// labels.
pub fn apply_tokens(ctx: &egui::Context, t: Tokens) {
    // Keep native windows at SystemDefault so macOS continues reporting OS appearance changes,
    // including when an integration or restored egui state pinned its own theme preference.
    ctx.set_theme(egui::ThemePreference::System);
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("photocraft-theme"), t));
    let st = t.style;
    let shadow = |s: ShadowTokens| egui::Shadow { offset: s.offset, blur: s.blur, spread: s.spread, color: s.color.unwrap_or(t.shadow) };
    let radius = |r: f32| CornerRadius::same(r.clamp(0.0, 255.0) as u8);
    let mut v = if t.dark() { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = t.chrome;
    v.window_fill = t.card;
    v.window_stroke = Stroke::new(1.0, t.card_border);
    v.extreme_bg_color = t.field;
    v.faint_bg_color = t.card;
    v.code_bg_color = t.field;
    v.override_text_color = Some(t.text);
    v.hyperlink_color = t.accent;
    v.warn_fg_color = t.warning;
    v.error_fg_color = t.danger;
    v.window_corner_radius = radius(t.radius_lg);
    v.menu_corner_radius = radius(t.radius);
    v.window_shadow = shadow(st.window_shadow);
    v.popup_shadow = shadow(st.popup_shadow);
    v.selection.bg_fill = st.selection_bg.unwrap_or(if t.bevel || t.pro { t.accent } else { t.accent_soft });
    v.selection.stroke = Stroke::new(1.0, st.selection_stroke.unwrap_or(t.accent_text));
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.striped = false;
    let w = &mut v.widgets;
    for (wv, tok, bg, border, fg) in [
        (&mut w.noninteractive, st.widgets.label, t.card, t.separator, t.text_dim),
        (&mut w.inactive, st.widgets.normal, t.field, t.field_border, t.text),
        (&mut w.hovered, st.widgets.hover, t.hover, t.field_border, t.text),
        (&mut w.active, st.widgets.active, t.pressed, t.accent_border, t.text),
        (&mut w.open, st.widgets.open, t.hover, t.field_border, t.text),
    ] {
        let (bg, border, fg) = tok.colors(bg, border, fg);
        wv.bg_fill = bg;
        wv.weak_bg_fill = bg;
        wv.bg_stroke = Stroke::new(tok.border_width, border);
        wv.fg_stroke = Stroke::new(tok.fg_width, fg);
        wv.corner_radius = radius(tok.radius.unwrap_or(t.radius_sm));
        wv.expansion = tok.expansion;
    }
    ctx.set_visuals(v);
    let margin = |m: f32| egui::Margin::same(m.clamp(0.0, 127.0) as i8);
    ctx.global_style_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(st.font_small)),
            (TextStyle::Body, FontId::proportional(st.font_body)),
            (TextStyle::Button, FontId::proportional(st.font_button)),
            (TextStyle::Heading, semibold(st.font_heading)),
            (TextStyle::Monospace, FontId::monospace(st.font_mono)),
        ]
        .into();
        s.spacing.item_spacing = st.item_spacing;
        s.spacing.button_padding = st.button_padding;
        s.spacing.interact_size = st.interact_size;
        s.spacing.slider_width = st.slider_width;
        s.spacing.combo_width = st.combo_width;
        s.spacing.menu_margin = margin(st.menu_margin);
        s.spacing.window_margin = margin(st.window_margin);
        s.spacing.icon_width = st.icon_width;
        s.visuals.indent_has_left_vline = false;
        s.interaction.tooltip_delay = TOOLTIP_DELAY;
        // Thin overlay scrollbars that appear on hover (Photoshop/macOS style).
        // Thin bars stay visible, including while the pointer is on them. The stock thin
        // style fades the handle to the same opacity as the track, so the thumb disappears (#4611).
        let mut scroll = if t.bevel { egui::style::ScrollStyle::solid() } else { egui::style::ScrollStyle::thin() };
        if scroll.floating {
            scroll.interact_handle_opacity = 1.0;
            scroll.interact_background_opacity = 0.4;
            scroll.floating_width = st.scroll_width;
        }
        s.spacing.scroll = scroll;
        s.spacing.tooltip_width = st.tooltip_width;
    });
    // egui selects its style branch using OS appearance, independently of Capishot's fixed
    // Dark/Light mode or platform appearance service. Both branches need the selected palette.
    let style = ctx.global_style();
    ctx.set_style_of(egui::Theme::Light, Arc::clone(&style));
    ctx.set_style_of(egui::Theme::Dark, style);
}

/// Seconds the pointer rests on a control before its tooltip shows.
pub const TOOLTIP_DELAY: f32 = 0.35;

/// Vertical gap between stacked control rows in panels (Properties fields, the Layers panel's
/// Opacity and Fill rows); docks zero egui's item spacing, so rows add this themselves.
pub const ROW_GAP: f32 = 4.0;

pub fn canvas_bg(t: &Tokens) -> Color32 {
    t.canvas
}

#[cfg(test)]
mod tests;

/// Development feature: a `theme.css` reloaded live.
///
/// Set `PHOTOCRAFT_THEME_FILE=/path/theme.css` in a debug build; the file is polled and applied
/// on change ([`css::parse`]), so a theme can be tuned without recompiling. Compiled out of
/// release builds; Preferences › Profile & Themes edits themes in any build.
#[cfg(all(debug_assertions, not(target_arch = "wasm32")))]
pub mod live {
    use std::time::SystemTime;

    #[derive(Default)]
    pub struct LiveTokens {
        path: Option<std::path::PathBuf>,
        stamp: Option<SystemTime>,
        last_check: f64,
    }

    impl LiveTokens {
        pub fn from_env() -> Self {
            Self { path: std::env::var_os("PHOTOCRAFT_THEME_FILE").map(Into::into), ..Default::default() }
        }

        /// Re-apply the stylesheet if the file changed. Returns true when tokens were updated.
        pub fn poll(&mut self, ctx: &egui::Context) -> bool {
            let Some(path) = &self.path else { return false };
            let now = ctx.input(|i| i.time);
            ctx.request_repaint_after(std::time::Duration::from_millis(400));
            if now - self.last_check < 0.4 {
                return false;
            }
            self.last_check = now;
            let stamp = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
            if stamp.is_none() || stamp == self.stamp {
                return false;
            }
            self.stamp = stamp;
            let Ok(text) = std::fs::read_to_string(path) else { return false };
            let parsed = super::css::parse(&text);
            for w in &parsed.warnings {
                log::warn!("theme file: {w}");
            }
            super::apply_tokens(ctx, parsed.tokens);
            true
        }
    }
}
