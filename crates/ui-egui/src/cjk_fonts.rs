//! Lazy CJK fallback fonts for the UI.
//!
//! The bundled Inter / JetBrains Mono have no Japanese, Chinese or Korean glyphs, and the OS
//! fonts that do are large (Hiragino ~10 MB, Apple SD Gothic Neo 28 MB, PingFang 78 MB, Noto
//! Sans CJK ~20 MB). Instead of reading them at startup, an egui plugin scans each frame's text
//! for CJK characters no registered font covers, and registers the next system font for that
//! character's script (`ctx.add_font`, active from the next frame, which it requests). Fonts are
//! appended at the lowest priority to every family, so Latin text keeps Inter.
//!
//! Script order follows the UI locale ([`photocraft_text::cjk::script_order`]): Kana prefers a
//! Japanese font, Hangul a Korean one, Bopomofo a Traditional Chinese one, and Han the
//! locale's script (Japanese forms only for a Japanese locale). At most one font is read per
//! frame, each file at most once, and once every script has been tried the scan stops.

use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use egui::{FontData, FontFamily, FontId, Shape};
use photocraft_text::cjk::{self, CjkChar, CjkScript, FontFile};
use std::path::PathBuf;

/// Skip absurdly large files (a corrupt or non-font path must not eat memory).
pub const MAX_FONT_BYTES: u64 = 128 << 20;
/// Name prefix of the registered fallback fonts.
pub const FONT_PREFIX: &str = "system-cjk";

/// Where fonts come from; swapped out in tests.
#[derive(Clone)]
pub struct Sources {
    pub locale: fn() -> Option<String>,
    pub files: fn(CjkScript) -> Vec<FontFile>,
    pub last_resort: fn() -> Vec<FontFile>,
}

impl Sources {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn system() -> Self {
        Self { locale: || cjk::ui_locale().map(str::to_string), files: cjk::font_files, last_resort: cjk::last_resort_files }
    }
    #[cfg(target_arch = "wasm32")]
    pub fn system() -> Self {
        Self { locale: || None, files: |_| Vec::new(), last_resort: Vec::new }
    }
}

/// The lazy loader's state: which scripts were tried and which files were read.
pub struct CjkFallback {
    sources: Sources,
    order: Option<[CjkScript; 4]>,
    tried: Vec<CjkScript>,
    last_resort_tried: bool,
    loaded: Vec<PathBuf>,
    /// Fonts registered so far: (name, path, face index).
    pub registered: Vec<(String, PathBuf, u32)>,
}

impl CjkFallback {
    pub fn new(sources: Sources) -> Self {
        Self { sources, order: None, tried: Vec::new(), last_resort_tried: false, loaded: Vec::new(), registered: Vec::new() }
    }

    /// Script order for the UI locale (resolved on first use, not at startup).
    pub fn order(&mut self) -> [CjkScript; 4] {
        *self.order.get_or_insert_with(|| cjk::script_order((self.sources.locale)().as_deref()))
    }

    /// Every script and the last-resort fonts have been tried: nothing more to load.
    pub fn exhausted(&self) -> bool {
        self.tried.len() >= 4 && self.last_resort_tried
    }

    /// The next font to register for a character of kind `kind` that no current font covers:
    /// the first readable file of the first untried script (the character's preferred script,
    /// then the locale order), then the last-resort fonts. `None` when nothing is left.
    pub fn next_font(&mut self, kind: CjkChar) -> Option<(String, FontData)> {
        let order = self.order();
        let scripts: Vec<CjkScript> = std::iter::once(kind.preferred(&order)).chain(order).collect();
        for s in scripts {
            if self.tried.contains(&s) {
                continue;
            }
            self.tried.push(s);
            let files = (self.sources.files)(s);
            if let Some(f) = self.load_first(&files) {
                return Some(f);
            }
        }
        if !self.last_resort_tried {
            self.last_resort_tried = true;
            let files = (self.sources.last_resort)();
            return self.load_first(&files);
        }
        None
    }

    fn load_first(&mut self, files: &[FontFile]) -> Option<(String, FontData)> {
        for f in files {
            if self.loaded.contains(&f.path) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&f.path) else { continue };
            if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_FONT_BYTES {
                continue;
            }
            let Ok(bytes) = std::fs::read(&f.path) else { continue };
            let index = cjk::face_index_for_family(&bytes, f.family);
            self.loaded.push(f.path.clone());
            let name = format!("{FONT_PREFIX}-{}", self.registered.len());
            log::info!("UI font fallback: registered {} (face {index}) as {name}", f.path.display());
            self.registered.push((name.clone(), f.path.clone(), index));
            let mut data = FontData::from_owned(bytes);
            data.index = index;
            return Some((name, data));
        }
        None
    }
}

/// First CJK character in the frame's text that the UI fonts can't draw.
fn missing_char(ctx: &egui::Context, shapes: &[egui::epaint::ClippedShape]) -> Option<char> {
    fn collect(shape: &Shape, out: &mut Vec<char>) {
        match shape {
            Shape::Text(t) => {
                let text = &t.galley.job.text;
                if !text.is_ascii() {
                    for c in text.chars().filter(|c| cjk::classify(*c).is_some()) {
                        if out.len() >= 256 {
                            return;
                        }
                        if !out.contains(&c) {
                            out.push(c);
                        }
                    }
                }
            }
            Shape::Vec(v) => v.iter().for_each(|s| collect(s, out)),
            _ => {}
        }
    }
    let mut chars = Vec::new();
    for s in shapes {
        collect(&s.shape, &mut chars);
    }
    if chars.is_empty() {
        return None;
    }
    let font = FontId::proportional(12.0);
    ctx.fonts_mut(|f| chars.into_iter().find(|c| !f.has_glyph(&font, *c)))
}

/// Registers `name` at the lowest priority in every font family.
fn add_to_all_families(ctx: &egui::Context, name: String, data: FontData) {
    let mut families: Vec<FontFamily> = ctx.fonts(|f| f.definitions().families.keys().cloned().collect());
    for f in [FontFamily::Proportional, FontFamily::Monospace] {
        if !families.contains(&f) {
            families.push(f);
        }
    }
    let families = families.into_iter().map(|family| InsertFontFamily { family, priority: FontPriority::Lowest }).collect();
    ctx.add_font(FontInsert { name, data, families });
}

/// The egui plugin that watches drawn text and loads fallback fonts on demand.
pub struct CjkFontPlugin(pub CjkFallback);

impl egui::Plugin for CjkFontPlugin {
    fn debug_name(&self) -> &'static str {
        "photocraft-cjk-fonts"
    }

    fn output_hook(&mut self, ctx: &egui::Context, output: &mut egui::FullOutput) {
        if self.0.exhausted() {
            return;
        }
        let Some(c) = missing_char(ctx, &output.shapes) else { return };
        let Some(kind) = cjk::classify(c) else { return };
        if let Some((name, data)) = self.0.next_font(kind) {
            add_to_all_families(ctx, name, data);
            ctx.request_repaint();
        } else if !self.0.exhausted() {
            // Nothing readable for this script; try the next one on the next frame.
            ctx.request_repaint();
        }
    }
}

/// Installs the lazy CJK fallback (system fonts; nothing on the web).
pub fn install(ctx: &egui::Context) {
    install_with(ctx, Sources::system());
}

pub fn install_with(ctx: &egui::Context, sources: Sources) {
    ctx.add_plugin(CjkFontPlugin(CjkFallback::new(sources)));
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

    fn fake_dir() -> PathBuf {
        let mut g = DIR.lock().unwrap();
        g.get_or_insert_with(|| {
            let dir = std::env::temp_dir().join(format!("photocraft-cjk-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("font.ttf"), include_bytes!("../../../assets/fonts/Inter-Regular.ttf")).unwrap();
            std::fs::write(dir.join("empty.ttf"), b"").unwrap();
            dir
        })
        .clone()
    }

    fn fake(name: &str) -> FontFile {
        FontFile { path: fake_dir().join(name), family: "" }
    }

    fn fake_sources(locale: fn() -> Option<String>) -> Sources {
        Sources {
            locale,
            // Korean has a readable "font", Japanese only broken files, Chinese none.
            files: |s| match s {
                CjkScript::Korean => vec![fake("missing.ttc"), fake("empty.ttf"), fake("font.ttf")],
                CjkScript::Japanese => vec![fake("empty.ttf")],
                _ => vec![],
            },
            last_resort: || vec![fake("font.ttf")],
        }
    }

    #[test]
    fn picks_preferred_script_skips_unreadable_and_exhausts() {
        let mut fb = CjkFallback::new(fake_sources(|| Some("en_US".into())));
        assert!(!fb.exhausted());
        // Hangul: Korean first; missing and empty files are skipped.
        let (name, data) = fb.next_font(CjkChar::Hangul).expect("korean font");
        assert!(name.starts_with(FONT_PREFIX));
        assert_eq!(data.index, 0);
        assert_eq!(fb.tried, vec![CjkScript::Korean]);
        // Han on an English locale: SC, TC, JA have nothing readable, and the last-resort file was
        // already loaded, so nothing new; everything is now tried.
        assert!(fb.next_font(CjkChar::Han).is_none());
        assert!(fb.exhausted());
        assert_eq!(fb.tried, vec![CjkScript::Korean, CjkScript::SimplifiedChinese, CjkScript::TraditionalChinese, CjkScript::Japanese]);
        assert_eq!(fb.registered.len(), 1, "a file is read at most once");
        assert!(fb.next_font(CjkChar::Kana).is_none());
    }

    #[test]
    fn han_follows_locale() {
        for (loc, first) in [
            ("ja_JP.UTF-8", CjkScript::Japanese),
            ("zh-Hans-CN", CjkScript::SimplifiedChinese),
            ("zh-Hant-TW", CjkScript::TraditionalChinese),
            ("ko_KR", CjkScript::Korean),
        ] {
            let mut fb = CjkFallback::new(Sources { locale: || None, files: |_| vec![], last_resort: Vec::new });
            fb.order = Some(cjk::script_order(Some(loc)));
            assert!(fb.next_font(CjkChar::Han).is_none());
            assert_eq!(fb.tried[0], first, "{loc}");
        }
    }

    /// Runs frames showing `text` until fonts settle; returns whether every glyph is covered.
    fn render(ctx: &egui::Context, text: &str) -> bool {
        for _ in 0..12 {
            let mut out = ctx.run_ui(Default::default(), |ui| {
                ui.label(text);
            });
            out.textures_delta.clear();
        }
        ctx.fonts_mut(|f| f.has_glyphs(&FontId::proportional(12.0), &text.replace(' ', "")))
    }

    #[test]
    fn lazy_registration_triggers_only_for_cjk() {
        let ctx = egui::Context::default();
        install_with(&ctx, fake_sources(|| Some("ko".into())));
        let _ = render(&ctx, "Layer 1 – café");
        let n = ctx.fonts(|f| f.definitions().font_data.keys().filter(|k| k.starts_with(FONT_PREFIX)).count());
        assert_eq!(n, 0, "Latin text loads nothing");
        // Hangul isn't in the stand-in font, but the Korean candidate must have been registered.
        let _ = render(&ctx, "카드 배경");
        let fonts = ctx.fonts(|f| f.definitions().clone());
        let name = format!("{FONT_PREFIX}-0");
        assert!(fonts.font_data.contains_key(&name));
        for (fam, stack) in &fonts.families {
            assert_eq!(stack.last(), Some(&name), "{fam:?}: appended last so Latin keeps Inter");
        }
    }

    /// With the real system fonts, every CJK sample renders (skipped per script when the
    /// machine has no font for it, e.g. CI Linux without Noto CJK).
    #[test]
    fn system_fonts_cover_cjk_samples() {
        let available = |s: CjkScript| cjk::font_files(s).iter().any(|f| f.path.is_file());
        let samples = [
            ("图层", CjkScript::SimplifiedChinese),
            ("圖層", CjkScript::TraditionalChinese),
            ("카드 배경", CjkScript::Korean),
            ("レイヤー", CjkScript::Japanese),
        ];
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        for (text, script) in samples {
            if !available(script) {
                eprintln!("skipping {text}: no {script:?} system font");
                continue;
            }
            assert!(render(&ctx, text), "{text} still has missing glyphs");
        }
        // All at once in a fresh context too (several scripts in one frame).
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let all: String = samples.iter().filter(|(_, s)| available(*s)).map(|(t, _)| *t).collect::<Vec<_>>().join(" ");
        assert!(render(&ctx, &all), "{all}");
    }
}
