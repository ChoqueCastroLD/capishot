//! `theme.css`: a theme as a small, declarative stylesheet of CSS custom properties.
//!
//! egui has no CSS, so this is not CSS: only `--name: value;` declarations, at the top level or
//! in a `:root { }` block, plus a few scoped blocks for widget states:
//!
//! ```css
//! :root {
//!   --base: studio;          /* the built-in theme it starts from (its layout family) */
//!   --accent: #8b7cf6;
//!   --radius: 8;
//!   --window-shadow: 0 10 32 0 #0000008c;
//! }
//! button:hover { --bg: #2c2c30; --border-width: 1; }
//! selection { --bg: #8b7cf62e; }
//! ```
//!
//! A scoped `button:hover { --bg: … }` is the same as `--button-hover-bg` at the top level.
//! Values are colours (`#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `transparent`, or `auto` for a
//! widget colour that follows the theme's colours), numbers (`12`, `12.5px`), pairs (`8 6`),
//! shadows (`x y blur spread [colour]`, or `none`), `true`/`false` and theme names. Every design
//! token is reachable ([`TOKENS`]). Nothing is fetched or run: anything else (other selectors,
//! `url()`, `@import`, unknown properties, bad values) is skipped and reported in the warnings,
//! and out-of-range numbers are clamped.

use egui::{Color32, Vec2};

use super::{ShadowTokens, ThemeKind, Tokens, WidgetTokens};

/// Where a token is shown in the Theme editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Base,
    Surfaces,
    Text,
    Accent,
    States,
    Status,
    Canvas,
    Shape,
    Typography,
    Spacing,
    Shadows,
    Selection,
    Label,
    Button,
    ButtonHover,
    ButtonActive,
    ButtonOpen,
}

impl Group {
    pub const ALL: [Group; 17] = [
        Group::Base,
        Group::Surfaces,
        Group::Text,
        Group::Accent,
        Group::States,
        Group::Status,
        Group::Canvas,
        Group::Shape,
        Group::Typography,
        Group::Spacing,
        Group::Shadows,
        Group::Selection,
        Group::Label,
        Group::Button,
        Group::ButtonHover,
        Group::ButtonActive,
        Group::ButtonOpen,
    ];

    /// Its English title (the UI translates it).
    pub fn label(self) -> &'static str {
        match self {
            Group::Base => "Base",
            Group::Surfaces => "Surfaces",
            Group::Text => "Text and icons",
            Group::Accent => "Accent",
            Group::States => "Hover and selection",
            Group::Status => "Status colours",
            Group::Canvas => "Canvas and scopes",
            Group::Shape => "Corners and layout",
            Group::Typography => "Text sizes",
            Group::Spacing => "Spacing",
            Group::Shadows => "Shadows",
            Group::Selection => "Selection",
            Group::Label => "Labels",
            Group::Button => "Controls",
            Group::ButtonHover => "Controls: hover",
            Group::ButtonActive => "Controls: pressed",
            Group::ButtonOpen => "Controls: open",
        }
    }

    /// The scoped block these tokens are written in (`None`: `:root`).
    fn selector(self) -> Option<&'static str> {
        match self {
            Group::Selection => Some("selection"),
            Group::Label => Some("label"),
            Group::Button => Some("button"),
            Group::ButtonHover => Some("button:hover"),
            Group::ButtonActive => Some("button:active"),
            Group::ButtonOpen => Some("button:open"),
            _ => None,
        }
    }
}

/// A token's value, borrowed from the [`Tokens`] it belongs to.
pub enum Slot<'a> {
    Color(&'a mut Color32),
    /// A colour that, unset, follows the theme's colours.
    Auto(&'a mut Option<Color32>),
    /// A number and its range.
    Num(&'a mut f32, f32, f32),
    /// An optional number (unset: follows the theme) and its range.
    AutoNum(&'a mut Option<f32>, f32, f32),
    Byte(&'a mut u8),
    Bool(&'a mut bool),
    Pair(&'a mut Vec2, f32, f32),
    Shadow(&'a mut ShadowTokens),
    Base(&'a mut ThemeKind),
}

/// One design token: its property name (without `--`; scoped ones as `button-hover-bg`), where
/// the editor shows it, its English label and its value.
pub struct Token {
    pub name: &'static str,
    pub group: Group,
    pub label: &'static str,
    pub slot: for<'a> fn(&'a mut Tokens) -> Slot<'a>,
}

macro_rules! token {
    ($group:ident, $name:expr, $label:literal, |$t:ident| $slot:expr) => {
        Token { name: $name, group: Group::$group, label: $label, slot: |$t| $slot }
    };
}

macro_rules! widget_tokens {
    ($group:ident, $prefix:literal, $state:ident) => {
        [
            token!($group, concat!($prefix, "-bg"), "Background", |t| Slot::Auto(&mut t.style.widgets.$state.bg)),
            token!($group, concat!($prefix, "-border"), "Border", |t| Slot::Auto(&mut t.style.widgets.$state.border)),
            token!($group, concat!($prefix, "-fg"), "Text", |t| Slot::Auto(&mut t.style.widgets.$state.fg)),
            token!($group, concat!($prefix, "-border-width"), "Border width", |t| Slot::Num(&mut t.style.widgets.$state.border_width, 0.0, 8.0)),
            token!($group, concat!($prefix, "-fg-width"), "Line width", |t| Slot::Num(&mut t.style.widgets.$state.fg_width, 0.0, 8.0)),
            token!($group, concat!($prefix, "-radius"), "Corner radius", |t| Slot::AutoNum(&mut t.style.widgets.$state.radius, 0.0, 64.0)),
            token!($group, concat!($prefix, "-expansion"), "Grow by", |t| Slot::Num(&mut t.style.widgets.$state.expansion, -4.0, 8.0)),
        ]
    };
}

/// Every design token, in the editor's order.
pub static TOKENS: std::sync::LazyLock<Vec<Token>> = std::sync::LazyLock::new(|| {
    let mut v = vec![
        token!(Base, "base", "Starts from", |t| Slot::Base(&mut t.kind)),
        token!(Base, "dark", "Dark theme", |t| Slot::Bool(&mut t.dark)),
        token!(Surfaces, "chrome", "Window and toolbars", |t| Slot::Color(&mut t.chrome)),
        token!(Surfaces, "dock", "Panel area", |t| Slot::Color(&mut t.dock)),
        token!(Surfaces, "card", "Panels", |t| Slot::Color(&mut t.card)),
        token!(Surfaces, "card-border", "Panel borders", |t| Slot::Color(&mut t.card_border)),
        token!(Surfaces, "field", "Fields", |t| Slot::Color(&mut t.field)),
        token!(Surfaces, "field-border", "Field borders", |t| Slot::Color(&mut t.field_border)),
        token!(Surfaces, "separator", "Separators", |t| Slot::Color(&mut t.separator)),
        token!(Surfaces, "tab-strip", "Tab strip", |t| Slot::Color(&mut t.tab_strip)),
        token!(Text, "text", "Text", |t| Slot::Color(&mut t.text)),
        token!(Text, "text-dim", "Secondary text", |t| Slot::Color(&mut t.text_dim)),
        token!(Text, "text-faint", "Faint text", |t| Slot::Color(&mut t.text_faint)),
        token!(Text, "icon", "Icons", |t| Slot::Color(&mut t.icon)),
        token!(Accent, "accent", "Accent", |t| Slot::Color(&mut t.accent)),
        token!(Accent, "accent-soft", "Soft accent", |t| Slot::Color(&mut t.accent_soft)),
        token!(Accent, "accent-border", "Accent border", |t| Slot::Color(&mut t.accent_border)),
        token!(Accent, "accent-text", "Accent text", |t| Slot::Color(&mut t.accent_text)),
        token!(Accent, "primary-bg", "Primary button", |t| Slot::Color(&mut t.primary_bg)),
        token!(Accent, "primary-text", "Primary button text", |t| Slot::Color(&mut t.primary_text)),
        token!(States, "hover", "Hover", |t| Slot::Color(&mut t.hover)),
        token!(States, "pressed", "Pressed", |t| Slot::Color(&mut t.pressed)),
        token!(States, "row-selected", "Selected row", |t| Slot::Color(&mut t.row_selected)),
        token!(Status, "danger", "Danger", |t| Slot::Color(&mut t.danger)),
        token!(Status, "warning", "Warning", |t| Slot::Color(&mut t.warning)),
        token!(Status, "caption-close", "Close button", |t| Slot::Color(&mut t.caption_close)),
        token!(Status, "caption-close-text", "Close button glyph", |t| Slot::Color(&mut t.caption_close_text)),
        token!(Canvas, "canvas", "Canvas surround", |t| Slot::Color(&mut t.canvas)),
        token!(Canvas, "canvas-dot", "Canvas dots", |t| Slot::Color(&mut t.canvas_dot)),
        token!(Canvas, "histogram-bg", "Scope background", |t| Slot::Color(&mut t.histogram_bg)),
        token!(Canvas, "histogram-level", "Scope brightness", |t| Slot::Byte(&mut t.histogram_level)),
        token!(Canvas, "scrim", "Dimming", |t| Slot::Color(&mut t.scrim)),
        token!(Shape, "radius-sm", "Small corners", |t| Slot::Num(&mut t.radius_sm, 0.0, 64.0)),
        token!(Shape, "radius", "Corners", |t| Slot::Num(&mut t.radius, 0.0, 64.0)),
        token!(Shape, "radius-lg", "Large corners", |t| Slot::Num(&mut t.radius_lg, 0.0, 64.0)),
        token!(Shape, "bevel", "3D bevels", |t| Slot::Bool(&mut t.bevel)),
        token!(Shape, "pro", "Pro controls", |t| Slot::Bool(&mut t.pro)),
        token!(Shape, "round-chips", "Round colour chips", |t| Slot::Bool(&mut t.round_chips)),
        token!(Typography, "font-small", "Small", |t| Slot::Num(&mut t.style.font_small, 6.0, 40.0)),
        token!(Typography, "font-body", "Body", |t| Slot::Num(&mut t.style.font_body, 6.0, 40.0)),
        token!(Typography, "font-button", "Buttons", |t| Slot::Num(&mut t.style.font_button, 6.0, 40.0)),
        token!(Typography, "font-heading", "Headings", |t| Slot::Num(&mut t.style.font_heading, 6.0, 60.0)),
        token!(Typography, "font-mono", "Numbers", |t| Slot::Num(&mut t.style.font_mono, 6.0, 40.0)),
        token!(Spacing, "item-spacing", "Between items", |t| Slot::Pair(&mut t.style.item_spacing, 0.0, 40.0)),
        token!(Spacing, "button-padding", "Button padding", |t| Slot::Pair(&mut t.style.button_padding, 0.0, 40.0)),
        token!(Spacing, "interact-size", "Control size", |t| Slot::Pair(&mut t.style.interact_size, 8.0, 80.0)),
        token!(Spacing, "slider-width", "Slider width", |t| Slot::Num(&mut t.style.slider_width, 40.0, 600.0)),
        token!(Spacing, "combo-width", "Menu button width", |t| Slot::Num(&mut t.style.combo_width, 40.0, 600.0)),
        token!(Spacing, "menu-margin", "Menu margin", |t| Slot::Num(&mut t.style.menu_margin, 0.0, 40.0)),
        token!(Spacing, "window-margin", "Window margin", |t| Slot::Num(&mut t.style.window_margin, 0.0, 60.0)),
        token!(Spacing, "icon-width", "Check box size", |t| Slot::Num(&mut t.style.icon_width, 6.0, 40.0)),
        token!(Spacing, "tooltip-width", "Tooltip width", |t| Slot::Num(&mut t.style.tooltip_width, 80.0, 800.0)),
        token!(Spacing, "scroll-width", "Scroll bar width", |t| Slot::Num(&mut t.style.scroll_width, 1.0, 20.0)),
        token!(Shadows, "shadow", "Shadow colour", |t| Slot::Color(&mut t.shadow)),
        token!(Shadows, "window-shadow", "Windows", |t| Slot::Shadow(&mut t.style.window_shadow)),
        token!(Shadows, "popup-shadow", "Menus and pop-ups", |t| Slot::Shadow(&mut t.style.popup_shadow)),
        token!(Selection, "selection-bg", "Background", |t| Slot::Auto(&mut t.style.selection_bg)),
        token!(Selection, "selection-stroke", "Outline", |t| Slot::Auto(&mut t.style.selection_stroke)),
    ];
    v.extend(widget_tokens!(Label, "label", label));
    v.extend(widget_tokens!(Button, "button", normal));
    v.extend(widget_tokens!(ButtonHover, "button-hover", hover));
    v.extend(widget_tokens!(ButtonActive, "button-active", active));
    v.extend(widget_tokens!(ButtonOpen, "button-open", open));
    v
});

/// The token named `name` (without `--`).
pub fn token(name: &str) -> Option<&'static Token> {
    TOKENS.iter().find(|t| t.name == name)
}

/// The most declarations read, and the longest name and value.
const MAX_DECLS: usize = 4000;
const MAX_NAME: usize = 64;
const MAX_VALUE: usize = 200;

/// A property read: its name (scoped ones flattened), value text and line.
struct Decl {
    name: String,
    value: String,
    line: usize,
}

/// Splits `css` into declarations, reporting what isn't one.
fn declarations(css: &str, warnings: &mut Vec<String>) -> Vec<Decl> {
    // Comments out, keeping line numbers.
    let mut text = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(i) = rest.find("/*") {
        text.push_str(&rest[..i]);
        match rest[i + 2..].find("*/") {
            Some(j) => {
                text.extend(rest[i..i + 2 + j + 2].chars().filter(|c| *c == '\n'));
                rest = &rest[i + 2 + j + 2..];
            }
            None => {
                warnings.push("an unclosed comment: the rest of the file is ignored".into());
                rest = "";
            }
        }
    }
    text.push_str(rest);
    let mut out = Vec::new();
    let mut scope: Option<Option<String>> = None; // None: top level; Some(None): a block skipped.
    let mut line = 1;
    let mut buf = String::new();
    let mut buf_line = 1;
    for c in text.chars() {
        match c {
            '{' => {
                let selector = buf.trim().to_string();
                if scope.is_some() {
                    warnings.push(format!("line {line}: nested blocks are not supported"));
                    scope = Some(None);
                } else {
                    scope = Some(scope_prefix(&selector));
                    if scope == Some(None) {
                        warnings.push(format!("line {buf_line}: `{}` ignored: not a theme selector", clip(&selector)));
                    }
                }
                buf.clear();
            }
            '}' => {
                if !buf.trim().is_empty() {
                    push_decl(&mut out, &scope, &buf, buf_line, warnings);
                }
                if scope.is_none() {
                    warnings.push(format!("line {line}: an unmatched `}}`"));
                }
                scope = None;
                buf.clear();
            }
            ';' => {
                push_decl(&mut out, &scope, &buf, buf_line, warnings);
                buf.clear();
            }
            _ => {
                if buf.trim().is_empty() {
                    buf_line = line;
                }
                buf.push(c);
            }
        }
        if c == '\n' {
            line += 1;
        }
        if out.len() > MAX_DECLS {
            warnings.push(format!("more than {MAX_DECLS} properties: the rest is ignored"));
            break;
        }
    }
    if !buf.trim().is_empty() {
        push_decl(&mut out, &scope, &buf, buf_line, warnings);
    }
    out
}

fn clip(s: &str) -> String {
    let mut c: String = s.chars().take(40).collect();
    if s.chars().count() > 40 {
        c.push('…');
    }
    c
}

/// The flattened-name prefix of a block's selector (`Some("")` for `:root`), `None` when it
/// isn't one a theme knows.
fn scope_prefix(selector: &str) -> Option<String> {
    let s: String = selector.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(
        match s.as_str() {
            ":root" | "theme" | "*" => "",
            "selection" | "::selection" => "selection-",
            "label" => "label-",
            "button" => "button-",
            "button:hover" => "button-hover-",
            "button:active" => "button-active-",
            "button:open" => "button-open-",
            _ => return None,
        }
        .to_string(),
    )
}

fn push_decl(out: &mut Vec<Decl>, scope: &Option<Option<String>>, text: &str, line: usize, warnings: &mut Vec<String>) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let prefix = match scope {
        Some(None) => return,
        Some(Some(p)) => p.as_str(),
        None => "",
    };
    let Some((name, value)) = text.split_once(':') else {
        warnings.push(format!("line {line}: `{}` ignored: not a `--name: value` property", clip(text)));
        return;
    };
    let (name, value) = (name.trim(), value.trim().trim_end_matches("!important").trim());
    let Some(bare) = name.strip_prefix("--").filter(|n| !n.is_empty() && n.len() <= MAX_NAME && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
    else {
        warnings.push(format!("line {line}: `{}` ignored: only custom properties (`--name`) are read", clip(name)));
        return;
    };
    if value.is_empty() || value.len() > MAX_VALUE {
        warnings.push(format!("line {line}: `--{bare}` ignored: its value is empty or too long"));
        return;
    }
    out.push(Decl { name: format!("{prefix}{bare}"), value: value.to_string(), line });
}

/// Reads `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa` and `transparent`.
pub fn parse_color(v: &str) -> Option<Color32> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("transparent") {
        return Some(Color32::TRANSPARENT);
    }
    let h = v.strip_prefix('#')?;
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let nib = |i: usize| u8::from_str_radix(h.get(i..i + 1)?, 16).ok().map(|x| x * 17);
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    match h.len() {
        3 => Some(Color32::from_rgb(nib(0)?, nib(1)?, nib(2)?)),
        4 => Some(Color32::from_rgba_unmultiplied(nib(0)?, nib(1)?, nib(2)?, nib(3)?)),
        6 => Some(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color32::from_rgba_unmultiplied(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        _ => None,
    }
}

/// `#rrggbb`, or `#rrggbbaa` when not opaque.
pub fn color_text(c: Color32) -> String {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    if a == 255 { format!("#{r:02x}{g:02x}{b:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") }
}

fn number(v: &str) -> Option<f32> {
    let v = v.trim();
    let v = v.strip_suffix("px").unwrap_or(v);
    v.parse::<f32>().ok().filter(|x| x.is_finite())
}

fn bounded(name: &str, x: f32, min: f32, max: f32, line: usize, warnings: &mut Vec<String>) -> f32 {
    if x < min || x > max {
        warnings.push(format!("line {line}: `--{name}` {x} is out of range ({min} to {max}); clamped"));
    }
    x.clamp(min, max)
}

fn num_text(x: f32) -> String {
    let r = (x * 1000.0).round() / 1000.0;
    if r == r.trunc() { format!("{}", r as i64) } else { format!("{r}") }
}

/// Sets one token from its value text; `Err` says why it can't.
fn set(slot: Slot<'_>, name: &str, v: &str, line: usize, warnings: &mut Vec<String>) -> Result<(), &'static str> {
    let auto = v.eq_ignore_ascii_case("auto");
    match slot {
        Slot::Color(c) => *c = parse_color(v).ok_or("not a colour (#rrggbb)")?,
        Slot::Auto(c) => *c = if auto { None } else { Some(parse_color(v).ok_or("not a colour (#rrggbb or auto)")?) },
        Slot::Num(x, min, max) => *x = bounded(name, number(v).ok_or("not a number")?, min, max, line, warnings),
        Slot::AutoNum(x, min, max) => *x = if auto { None } else { Some(bounded(name, number(v).ok_or("not a number or auto")?, min, max, line, warnings)) },
        Slot::Byte(x) => *x = bounded(name, number(v).ok_or("not a number")?, 0.0, 255.0, line, warnings).round() as u8,
        Slot::Bool(b) => {
            *b = match v.to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => true,
                "false" | "no" | "off" | "0" => false,
                _ => return Err("not true or false"),
            }
        }
        Slot::Pair(p, min, max) => {
            let parts: Vec<f32> = v.split_whitespace().map(number).collect::<Option<_>>().ok_or("not one or two numbers")?;
            let (x, y) = match parts[..] {
                [x] => (x, x),
                [x, y] => (x, y),
                _ => return Err("not one or two numbers"),
            };
            *p = Vec2::new(bounded(name, x, min, max, line, warnings), bounded(name, y, min, max, line, warnings));
        }
        Slot::Shadow(s) => {
            if v.eq_ignore_ascii_case("none") {
                *s = ShadowTokens { offset: [0, 0], blur: 0, spread: 0, color: Some(Color32::TRANSPARENT) };
                return Ok(());
            }
            let parts: Vec<&str> = v.split_whitespace().collect();
            let (nums, color) = match parts.split_last() {
                Some((last, rest)) if last.starts_with('#') || last.eq_ignore_ascii_case("transparent") => (rest, Some(parse_color(last).ok_or("its colour is not #rrggbb")?)),
                _ => (&parts[..], None),
            };
            let n: Vec<f32> = nums.iter().map(|x| number(x)).collect::<Option<_>>().ok_or("not `x y blur spread [colour]`")?;
            let [x, y, blur, spread] = match n[..] {
                [x, y, b, sp] => [x, y, b, sp],
                [x, y, b] => [x, y, b, 0.0],
                _ => return Err("not `x y blur spread [colour]`"),
            };
            let mut c8 = |x: f32| bounded(name, x, -127.0, 255.0, line, warnings).round();
            let [x, y, blur, spread] = [c8(x), c8(y), c8(blur), c8(spread)];
            *s = ShadowTokens { offset: [x.min(127.0) as i8, y.min(127.0) as i8], blur: blur.max(0.0) as u8, spread: spread.max(0.0) as u8, color };
        }
        Slot::Base(k) => *k = ThemeKind::from_name(v).ok_or("not a built-in theme name")?,
    }
    Ok(())
}

/// A token's value as `theme.css` writes it (`None`: unset, follows the theme).
pub fn value_text(slot: Slot<'_>) -> Option<String> {
    Some(match slot {
        Slot::Color(c) => color_text(*c),
        Slot::Auto(c) => color_text((*c)?),
        Slot::Num(x, ..) => num_text(*x),
        Slot::AutoNum(x, ..) => num_text((*x)?),
        Slot::Byte(x) => x.to_string(),
        Slot::Bool(b) => b.to_string(),
        Slot::Pair(p, ..) => format!("{} {}", num_text(p.x), num_text(p.y)),
        Slot::Shadow(s) => {
            let base = format!("{} {} {} {}", s.offset[0], s.offset[1], s.blur, s.spread);
            match s.color {
                Some(c) => format!("{base} {}", color_text(c)),
                None => base,
            }
        }
        Slot::Base(k) => k.id().to_string(),
    })
}

/// A stylesheet read into tokens.
pub struct Parsed {
    pub tokens: Tokens,
    /// What was skipped or clamped, for the person who wrote it.
    pub warnings: Vec<String>,
    /// The tokens it set (by name), so a built-in can be checked for completeness.
    pub set: Vec<&'static str>,
}

/// Reads `css` on top of `start` (its `--base` replaces `start` with that built-in theme first).
fn parse_onto(css: &str, start: impl FnOnce(Option<ThemeKind>) -> Tokens) -> Parsed {
    let mut warnings = Vec::new();
    let decls = declarations(css, &mut warnings);
    let base = decls.iter().rev().find(|d| d.name == "base").and_then(|d| ThemeKind::from_name(&d.value));
    let mut tokens = start(base);
    let mut set_names = Vec::new();
    for d in &decls {
        let Some(tok) = token(&d.name) else {
            warnings.push(format!("line {}: `--{}` is not a theme property", d.line, d.name));
            continue;
        };
        match set((tok.slot)(&mut tokens), tok.name, &d.value, d.line, &mut warnings) {
            Ok(()) => set_names.push(tok.name),
            Err(why) => warnings.push(format!("line {}: `--{}: {}` ignored: {why}", d.line, d.name, clip(&d.value))),
        }
    }
    Parsed { tokens, warnings, set: set_names }
}

/// Reads a theme's stylesheet: it starts from its `--base` built-in theme (Studio without one).
pub fn parse(css: &str) -> Parsed {
    parse_onto(css, |base| Tokens::for_kind(base.unwrap_or(ThemeKind::Studio)))
}

/// Reads a built-in theme's own stylesheet, which sets every token itself.
pub(super) fn parse_builtin(css: &str, kind: ThemeKind) -> Parsed {
    parse_onto(css, |_| Tokens::blank(kind))
}

/// `tokens` as a complete `theme.css`: every token, grouped, the widget states in their blocks.
pub fn write(tokens: &Tokens) -> String {
    let mut t = *tokens;
    let mut out = String::new();
    let mut open: Option<&'static str> = None;
    let mut root_open = false;
    for group in Group::ALL {
        let lines: Vec<String> = TOKENS
            .iter()
            .filter(|tok| tok.group == group)
            .filter_map(|tok| {
                let v = value_text((tok.slot)(&mut t))?;
                let name = match group.selector() {
                    Some(sel) => tok.name.strip_prefix(&format!("{}-", sel.replace(':', "-"))).unwrap_or(tok.name).to_string(),
                    None => tok.name.to_string(),
                };
                Some(format!("  --{name}: {v};"))
            })
            .collect();
        let sel = group.selector();
        if sel.is_some() && lines.is_empty() {
            continue;
        }
        match sel {
            None if !root_open => {
                out.push_str(":root {\n");
                root_open = true;
            }
            None => {}
            Some(s) => {
                if root_open || open.is_some() {
                    out.push_str("}\n\n");
                }
                root_open = false;
                out.push_str(&format!("{s} {{\n"));
                open = Some(s);
            }
        }
        out.push_str(&format!("  /* {} */\n", group.label()));
        for l in lines {
            out.push_str(&l);
            out.push('\n');
        }
    }
    out.push_str("}\n");
    out
}

/// Only what `tokens` changes from its `--base` theme, as `theme.css` (always with `--base`).
pub fn write_changes(tokens: &Tokens) -> String {
    let mut base = Tokens::for_kind(tokens.kind);
    let mut t = *tokens;
    let mut root = vec![format!("  --base: {};", tokens.kind.id())];
    let mut blocks: Vec<(&'static str, Vec<String>)> = Vec::new();
    for tok in TOKENS.iter().filter(|tok| tok.name != "base") {
        let (v, b) = (value_text((tok.slot)(&mut t)), value_text((tok.slot)(&mut base)));
        if v == b {
            continue;
        }
        let v = v.unwrap_or_else(|| "auto".into());
        match tok.group.selector() {
            None => root.push(format!("  --{}: {v};", tok.name)),
            Some(sel) => {
                let name = tok.name.strip_prefix(&format!("{}-", sel.replace(':', "-"))).unwrap_or(tok.name);
                let line = format!("  --{name}: {v};");
                match blocks.iter_mut().find(|(s, _)| *s == sel) {
                    Some((_, l)) => l.push(line),
                    None => blocks.push((sel, vec![line])),
                }
            }
        }
    }
    let mut out = format!(":root {{\n{}\n}}\n", root.join("\n"));
    for (sel, lines) in blocks {
        out.push_str(&format!("\n{sel} {{\n{}\n}}\n", lines.join("\n")));
    }
    out
}

impl WidgetTokens {
    /// This state's background, border and text, with unset ones from the given defaults.
    pub fn colors(&self, bg: Color32, border: Color32, fg: Color32) -> (Color32, Color32, Color32) {
        (self.bg.unwrap_or(bg), self.border.unwrap_or(border), self.fg.unwrap_or(fg))
    }
}

#[cfg(test)]
#[path = "css_tests.rs"]
mod tests;
