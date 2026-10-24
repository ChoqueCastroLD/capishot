//! What the command palette (Edit › Search) matches beyond a command's name in the UI language:
//! the same command in Spanish, Portuguese and English (people write in the language they
//! think in, whatever the interface shows), accent-free spelling ("tamano" finds "Tamaño"),
//! everyday words for common jobs ("borrar" finds the Eraser, "letras" the Type tool), and
//! quick actions typed as a sentence ("zoom 200", "nuevo 1200x1700", "#ff3366", "pincel 30",
//! "rotar 90").

use photocraft_engine::prefs::Preferences;
use serde_json::{Value, json};

/// The languages every query is matched in, besides the UI language.
pub const SEARCH_LANGS: [&str; 3] = ["en", "es", "pt-br"];

/// Lowercase without accents (á → a, ç → c, ñ → n), for matching.
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            c => c,
        })
        .collect()
}

/// Everyday words (Spanish, Portuguese, English, accent-free) for the commands and tools people
/// look for most, and what they mean. A word matches when the query is a prefix of it (three
/// letters or more) or the query contains it.
pub const SYNONYMS: &[(&[&str], &str)] = &[
    (&["borrar", "borrador", "goma", "apagar", "borracha", "erase", "eraser", "rubber"], "tool:Eraser"),
    (&["pincel", "brocha", "pintar", "dibujar", "desenhar", "paint", "draw", "brush"], "tool:Brush"),
    (&["lapiz", "lapis", "pencil"], "tool:Pencil"),
    (&["texto", "letra", "letras", "escribir", "escrever", "tipografia", "text", "type", "font", "fuente", "fonte"], "tool:Type"),
    (&["recortar", "cortar", "encuadrar", "crop", "trim"], "tool:Crop"),
    (&["lazo", "laco", "lasso", "seleccion libre", "selecao livre"], "tool:Lasso"),
    (&["mover", "move", "arrastrar", "arrastar"], "tool:Move"),
    (&["cuentagotas", "conta-gotas", "contagotas", "gotero", "eyedropper", "picker", "pick color"], "tool:Eyedropper"),
    (&["mano", "mao", "hand", "pan", "desplazar"], "tool:Hand"),
    (&["lupa", "zoom", "magnify"], "tool:Zoom"),
    (&["tampon", "clonar", "clone", "carimbo"], "tool:CloneStamp"),
    (&["parche", "curar", "corrector", "healing", "spot", "manchas"], "tool:SpotHealing"),
    (&["degradado", "degrade", "gradiente", "gradient"], "tool:Gradient"),
    (&["bote", "balde", "rellenar", "preencher", "bucket", "fill"], "tool:PaintBucket"),
    (&["varita", "varinha", "magic wand", "wand"], "tool:MagicWand"),
    (&["seleccion rapida", "selecao rapida", "quick selection"], "tool:QuickSelection"),
    (&["deshacer", "desfazer", "undo", "atras", "volver"], "edit.undo"),
    (&["rehacer", "refazer", "redo"], "edit.redo"),
    (&["guardar", "salvar", "save", "grabar"], "file.save"),
    (&["abrir", "open", "importar", "import"], "file.open"),
    (&["nuevo", "novo", "nueva", "nova", "new", "crear", "criar"], "file.new"),
    (&["exportar", "export", "png", "descargar", "baixar", "download", "compartir", "compartilhar", "share"], "file.export.quickExportAsPng"),
    (&["capa", "camada", "layer"], "layer.new.layer"),
    (&["desenfocar", "desenfoque", "desfocar", "difuminar", "blur", "borroso"], "filter.blur.gaussianBlur"),
    (&["tamano", "tamanho", "redimensionar", "escalar", "resize", "size", "resolucion", "resolucao"], "image.imageSize"),
    (&["rotar", "girar", "rotate", "turn"], "image.imageRotation.90cw"),
    (&["voltear", "espejo", "espelhar", "inverter", "flip", "mirror"], "image.imageRotation.flipCanvasHorizontal"),
    (&["seleccionar todo", "selecionar tudo", "select all", "todo", "tudo"], "select.all"),
    (&["deseleccionar", "desmarcar", "deselect", "quitar seleccion"], "select.deselect"),
    (&["invertir seleccion", "inverter selecao", "invert selection"], "select.inverse"),
    (&["limpiar", "limpar", "clean", "quitar texto", "remover texto", "inpaint", "ia", "ai"], "edit.inpaintAi"),
    (&["tono", "saturacion", "saturacao", "hue", "saturation", "colores", "cores"], "image.adjustments.hueSaturation"),
    (&["niveles", "niveis", "levels"], "image.adjustments.levels"),
    (&["curvas", "curves"], "image.adjustments.curves"),
    (&["brillo", "brilho", "contraste", "brightness", "contrast"], "image.adjustments.brightnessContrast"),
    (&["ajustes", "configuracion", "configuracoes", "preferencias", "settings", "preferences", "opciones", "opcoes"], "edit.preferences.general"),
];

/// The synonym targets `query` names, best first (a whole-word match before a prefix).
pub fn synonym_targets(query: &str) -> Vec<(i32, &'static str)> {
    let q = fold(query.trim());
    if q.chars().count() < 3 {
        return Vec::new();
    }
    let mut out: Vec<(i32, &'static str)> = Vec::new();
    for (words, target) in SYNONYMS {
        let best = words
            .iter()
            .filter_map(|w| {
                // Above a command's own exact name (1000): Spanish "borrar" is also Edit ›
                // Clear's name, but the everyday word means the Eraser; Clear is listed next.
                if *w == q {
                    Some(1100)
                } else if w.starts_with(&q) {
                    Some(700)
                } else if q.split(' ').any(|part| part == *w) || (w.contains(' ') && q.contains(w)) {
                    Some(600)
                } else {
                    None
                }
            })
            .max();
        if let Some(s) = best
            && !out.iter().any(|(_, t)| t == target)
        {
            out.push((s, target));
        }
    }
    out.sort_by_key(|t| std::cmp::Reverse(t.0));
    out
}

/// A quick action the query spells out.
#[derive(Clone, Debug, PartialEq)]
pub enum Quick {
    /// Zoom the view to a percentage.
    Zoom(f32),
    /// A new document of this size (pixels).
    New(u32, u32),
    /// The foreground colour (`#rrggbb`).
    Color([u8; 3]),
    /// The brush size (pixels).
    BrushSize(u32),
    /// Rotate the canvas (degrees clockwise: 90, 180 or 270).
    Rotate(u32),
}

/// The words before a number that name each quick action (Spanish, Portuguese, English).
const NEW_WORDS: [&str; 7] = ["new", "nuevo", "nueva", "novo", "nova", "crear", "criar"];
const BRUSH_WORDS: [&str; 7] = ["brush", "pincel", "brocha", "size", "tamano", "tamanho", "grosor"];
const ROTATE_WORDS: [&str; 3] = ["rotate", "rotar", "girar"];

/// The quick action `query` spells, if any.
pub fn quick(query: &str) -> Option<Quick> {
    let q = fold(query.trim());
    let words: Vec<&str> = q.split_whitespace().collect();
    let num = |s: &str| s.trim_end_matches(['%', 'p', 'x']).parse::<u32>().ok();
    // "#ff3366", "ff3366"
    let hex = q.trim_start_matches('#');
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) && (q.starts_with('#') || hex.chars().any(|c| c.is_ascii_digit())) {
        let v = u32::from_str_radix(hex, 16).ok()?;
        return Some(Quick::Color([(v >> 16) as u8, (v >> 8) as u8, v as u8]));
    }
    // "200%", "zoom 200"
    if words.len() == 1 && q.ends_with('%') {
        return num(&q).filter(|n| (1..=3200).contains(n)).map(|n| Quick::Zoom(n as f32 / 100.0));
    }
    if words.len() == 2 && words[0] == "zoom" {
        return num(words[1]).filter(|n| (1..=3200).contains(n)).map(|n| Quick::Zoom(n as f32 / 100.0));
    }
    // "nuevo 1200x1700", "new 1200 x 1700"
    if let Some(first) = words.first()
        && NEW_WORDS.contains(first)
    {
        let rest: String = words[1..].concat();
        let (w, h) = rest.split_once(['x', '*'])?;
        let (w, h) = (w.parse::<u32>().ok()?, h.trim_end_matches("px").parse::<u32>().ok()?);
        return ((1..=30000).contains(&w) && (1..=30000).contains(&h)).then_some(Quick::New(w, h));
    }
    if words.len() == 2 && BRUSH_WORDS.contains(&words[0]) {
        return num(words[1]).filter(|n| (1..=5000).contains(n)).map(Quick::BrushSize);
    }
    if words.len() == 2 && ROTATE_WORDS.contains(&words[0]) {
        return match num(words[1].trim_end_matches('°'))? {
            90 => Some(Quick::Rotate(90)),
            180 => Some(Quick::Rotate(180)),
            270 => Some(Quick::Rotate(270)),
            _ => None,
        };
    }
    None
}

impl Quick {
    /// What the palette row says (UI language).
    pub fn label(&self) -> String {
        match self {
            Quick::Zoom(z) => crate::i18n::fmt(tl!("Zoom to {percent}%"), &[("percent", &format!("{:.0}", z * 100.0))]),
            Quick::New(w, h) => crate::i18n::fmt(tl!("New document {w} × {h} px"), &[("w", &w.to_string()), ("h", &h.to_string())]),
            Quick::Color([r, g, b]) => crate::i18n::fmt(tl!("Set the foreground color to {hex}"), &[("hex", &format!("#{r:02x}{g:02x}{b:02x}"))]),
            Quick::BrushSize(n) => crate::i18n::fmt(tl!("Brush size {px} px"), &[("px", &n.to_string())]),
            Quick::Rotate(d) => crate::i18n::fmt(tl!("Rotate the canvas {deg}°"), &[("deg", &d.to_string())]),
        }
    }

    /// The command (and its parameters) that does it, or `None` for the view's zoom.
    pub fn command(&self, prefs: &Preferences) -> Option<(&'static str, Value)> {
        let _ = prefs;
        match self {
            Quick::Zoom(_) => None,
            Quick::New(w, h) => Some(("file.new", json!({"width": w, "height": h, "background": "white"}))),
            Quick::Color([r, g, b]) => Some(("tools.setColors", json!({"foreground": [*r as f32 / 255.0, *g as f32 / 255.0, *b as f32 / 255.0, 1.0]}))),
            Quick::BrushSize(n) => Some(("tools.setBrush", json!({"brush": {"size": n}}))),
            Quick::Rotate(90) => Some(("image.imageRotation.90cw", json!({}))),
            Quick::Rotate(180) => Some(("image.imageRotation.180", json!({}))),
            Quick::Rotate(_) => Some(("image.imageRotation.90ccw", json!({}))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_drops_case_and_accents() {
        assert_eq!(fold("Tamaño de Imagen"), "tamano de imagen");
        assert_eq!(fold("Configurações"), "configuracoes");
    }

    #[test]
    fn everyday_words_name_tools_and_commands() {
        assert_eq!(synonym_targets("borrar").first().map(|t| t.1), Some("tool:Eraser"));
        assert_eq!(synonym_targets("apagar").first().map(|t| t.1), Some("tool:Eraser"));
        assert_eq!(synonym_targets("letras").first().map(|t| t.1), Some("tool:Type"));
        assert_eq!(synonym_targets("desenf").first().map(|t| t.1), Some("filter.blur.gaussianBlur"));
        assert_eq!(synonym_targets("Tamaño").first().map(|t| t.1), Some("image.imageSize"));
        assert!(synonym_targets("ab").is_empty(), "two letters are too few");
    }

    #[test]
    fn quick_actions_are_spelled_in_three_languages() {
        assert_eq!(quick("zoom 200"), Some(Quick::Zoom(2.0)));
        assert_eq!(quick("50%"), Some(Quick::Zoom(0.5)));
        assert_eq!(quick("nuevo 1200x1700"), Some(Quick::New(1200, 1700)));
        assert_eq!(quick("novo 800 x 600"), Some(Quick::New(800, 600)));
        assert_eq!(quick("new 1024*768px"), Some(Quick::New(1024, 768)));
        assert_eq!(quick("#FF3366"), Some(Quick::Color([255, 51, 102])));
        assert_eq!(quick("pincel 30"), Some(Quick::BrushSize(30)));
        assert_eq!(quick("tamaño 12"), Some(Quick::BrushSize(12)));
        assert_eq!(quick("rotar 90"), Some(Quick::Rotate(90)));
        assert_eq!(quick("girar 180°"), Some(Quick::Rotate(180)));
        assert_eq!(quick("rotate 45"), None);
        assert_eq!(quick("blur"), None);
        assert_eq!(quick("facade"), None, "a word that happens to be hex is no colour");
    }
}
