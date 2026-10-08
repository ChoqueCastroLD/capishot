//! Write a small PSD for a live-text edit check in another editor.
//! `cargo run -p photocraft-io --example psd_text_fill -- /tmp/text-fill.psd`

use photocraft_color::{Color, ColorMode, SampleType};
use photocraft_doc::text::{CharStyle, TextRun};
use photocraft_doc::{Document, Layer, LayerContent, Size, TextLayer};
use photocraft_geom::Affine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os().nth(1).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "usage: psd_text_fill <output.psd>"))?;
    let mut doc = Document::new("Text fill", Size::new(640, 240), ColorMode::Rgb, SampleType::U8);
    let text = "Editable text";
    let mut layer = TextLayer {
        text: text.into(),
        runs: vec![TextRun {
            len: text.len(),
            style: CharStyle { font_family: "Inter".into(), size_pt: 48.0, color: Color::rgb(0.1, 0.2, 0.5), faux_bold: true, ..Default::default() },
        }],
        transform: Affine { m: [1.2, 0.0, 0.0, 1.4, 30.0, 150.0] },
        ..Default::default()
    };
    layer.sync_summary();
    photocraft_text::TextEngine::new().render_layer(&mut layer, doc.resolution_dpi, doc.pixel_format());
    doc.layers.push(Layer::new("Editable text", LayerContent::Text(layer)));
    let output = photocraft_io::export(&doc, "text-fill.psd", &Default::default())?;
    std::fs::write(path, output.bytes)?;
    Ok(())
}
