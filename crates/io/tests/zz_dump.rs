mod common;
use common::*;
use photocraft_color::{ColorMode, SampleType};
use photocraft_io::*;
#[test]
fn zz_layers() {
    let f = Features { groups: true, masks: true, adjustments: false, fills: true, all_blends: true, extras: true };
    let full = gen_doc(ColorMode::Rgb, SampleType::U8, f);
    for k in 0..full.layers.len() {
        let mut d = full.clone();
        d.layers = d.layers.iter().enumerate().filter(|(i, l)| *i == k || l.name == "Background").map(|(_, l)| l.clone()).collect();
        let r = export(&d, "x.psd", &ExportOptions::default()).unwrap();
        let merged = merged_composite(&photocraft_psd::PsdFile::from_bytes(&r.bytes).unwrap()).unwrap();
        let ours = photocraft_compose::flatten(&import("x.psd", &r.bytes).unwrap().document).px;
        let mut worst = (0f32, 0usize);
        for (i, (a, b)) in ours.iter().zip(&merged).enumerate() { let m = (0..4).map(|c| (a[c]-b[c]).abs()).fold(0.0, f32::max); if m > worst.0 { worst = (m, i); } }
        if worst.0 > 3.0/255.0 { println!("{} {:?} fill {} op {} worst {:.3} ours {:?} psdc {:?}", full.layers[k].name, full.layers[k].blend, full.layers[k].fill_opacity, full.layers[k].opacity, worst.0, ours[worst.1], merged[worst.1]); }
    }
}
