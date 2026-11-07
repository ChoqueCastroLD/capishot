//! `edit.cleanQuick`: cleaning what's selected with no model. A blob at a time, so each takes from
//! what's around it: a flat colour, the one around the blob, for letters on an even ground; or a
//! content-aware fill, for tones and simple backgrounds. The result goes where a clean's goes.

use photocraft_algo::content_aware::{FillOptions, color_level, fill, rotation_level};
use serde_json::{Value, json};

use super::{Output, Plan, bad, head_of, write_result};
use crate::commands::CommandSpec;
use crate::jobs::JobCtx;
use crate::{Result, Session};

pub const ID: &str = "edit.cleanQuick";
const LABEL: &str = "Clean";
/// What's read around the selection, at least.
const MARGIN: usize = 96;
/// How far around a blob its ground is read.
const RING: usize = 4;
/// How far around a blob a fill samples, at least.
const WINDOW: usize = 64;

#[derive(Clone, Copy)]
enum Method {
    Fill,
    /// The given colour, or the one around each blob.
    Color(Option<[f32; 3]>),
}

/// A blob of the selection: its pixels, and the box they're in.
struct Blob {
    pixels: Vec<usize>,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

fn blobs(cover: &[f32], w: usize, h: usize) -> Vec<Blob> {
    let mut seen = vec![false; cover.len()];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..cover.len() {
        if seen[start] || cover[start] <= 0.0 {
            continue;
        }
        let mut b = Blob { pixels: Vec::new(), x0: w, y0: h, x1: 0, y1: 0 };
        seen[start] = true;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            (b.x0, b.y0, b.x1, b.y1) = (b.x0.min(x), b.y0.min(y), b.x1.max(x + 1), b.y1.max(y + 1));
            b.pixels.push(i);
            for ny in y.saturating_sub(1)..(y + 2).min(h) {
                for nx in x.saturating_sub(1)..(x + 2).min(w) {
                    let n = ny * w + nx;
                    if !seen[n] && cover[n] > 0.0 {
                        seen[n] = true;
                        stack.push(n);
                    }
                }
            }
        }
        out.push(b);
    }
    out
}

/// The median of what's just around a blob and in no blob: its ground.
fn ground(b: &Blob, rgb: &[f32], cover: &[f32], w: usize, h: usize) -> Option<[f32; 3]> {
    let (x0, y0) = (b.x0.saturating_sub(RING), b.y0.saturating_sub(RING));
    let (x1, y1) = ((b.x1 + RING).min(w), (b.y1 + RING).min(h));
    let (gw, gh) = (x1 - x0, y1 - y0);
    let mut near = vec![0u8; gw * gh];
    for &i in &b.pixels {
        near[(i / w - y0) * gw + i % w - x0] = 1;
    }
    for step in 1..=RING as u8 {
        for gy in 0..gh {
            for gx in 0..gw {
                if near[gy * gw + gx] == step {
                    for ny in gy.saturating_sub(1)..(gy + 2).min(gh) {
                        for nx in gx.saturating_sub(1)..(gx + 2).min(gw) {
                            if near[ny * gw + nx] == 0 {
                                near[ny * gw + nx] = step + 1;
                            }
                        }
                    }
                }
            }
        }
    }
    let mut samples: [Vec<f32>; 3] = Default::default();
    for gy in 0..gh {
        for gx in 0..gw {
            let i = (gy + y0) * w + gx + x0;
            if near[gy * gw + gx] > 1 && cover[i] <= 0.0 {
                for (k, v) in samples.iter_mut().enumerate() {
                    v.push(rgb[i * 3 + k]);
                }
            }
        }
    }
    if samples[0].is_empty() {
        return None;
    }
    Some(samples.map(|mut v| {
        let mid = v.len() / 2;
        *v.select_nth_unstable_by(mid, f32::total_cmp).1
    }))
}

/// What goes where the selection covers (interleaved RGB of the plan's region).
fn generate(plan: &Plan<()>, method: Method, ctx: &JobCtx) -> Result<Vec<f32>> {
    let (w, h) = (plan.region.width() as usize, plan.region.height() as usize);
    let (rgb, cover) = (&plan.rgb, &plan.cover);
    let mut out = rgb.clone();
    let found = blobs(cover, w, h);
    let total = found.len().max(1) as f32;
    for (n, b) in found.iter().enumerate() {
        ctx.check()?;
        ctx.progress(n as f32 / total, LABEL);
        match method {
            Method::Color(color) => {
                // With nothing around it (the whole page is selected): white, the paper.
                let c = color.or_else(|| ground(b, rgb, cover, w, h)).unwrap_or([1.0; 3]);
                for &i in &b.pixels {
                    out[i * 3..i * 3 + 3].copy_from_slice(&c);
                }
            }
            Method::Fill => {
                let reach = WINDOW.max((b.x1 - b.x0).max(b.y1 - b.y0));
                let (x0, y0) = (b.x0.saturating_sub(reach), b.y0.saturating_sub(reach));
                let (x1, y1) = ((b.x1 + reach).min(w), (b.y1 + reach).min(h));
                let (fw, fh) = (x1 - x0, y1 - y0);
                let mut img = Vec::with_capacity(fw * fh * 3);
                let mut source = Vec::with_capacity(fw * fh);
                for y in y0..y1 {
                    img.extend_from_slice(&rgb[(y * w + x0) * 3..(y * w + x1) * 3]);
                    source.extend(cover[y * w + x0..y * w + x1].iter().map(|c| *c <= 0.0));
                }
                if !source.iter().any(|s| *s) {
                    return Err(crate::EngineError::Other("nothing around the selection to fill it from".into()));
                }
                let mut hole = vec![false; fw * fh];
                for &i in &b.pixels {
                    hole[(i / w - y0) * fw + i % w - x0] = true;
                }
                let opts = FillOptions { color_adaptation: color_level("default"), rotations: rotation_level("none"), scale: false, mirror: false, seed: 1 };
                let filled = fill(fw, fh, 3, &img, &hole, &source, &opts);
                for &i in &b.pixels {
                    let at = (i / w - y0) * fw + i % w - x0;
                    out[i * 3..i * 3 + 3].copy_from_slice(&filled[at * 3..at * 3 + 3]);
                }
            }
        }
    }
    Ok(out)
}

fn hex(s: &str) -> Option<[u8; 3]> {
    let s = s.strip_prefix('#').filter(|s| s.len() == 6 && s.is_ascii())?;
    let at = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some([at(0)?, at(2)?, at(4)?])
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let method = match p.get("method").and_then(Value::as_str) {
        Some("fill") => Method::Fill,
        Some("color") => {
            let color = match p.get("color") {
                None | Some(Value::Null) => None,
                Some(v) => {
                    let c = v.as_str().and_then(hex).ok_or_else(|| bad("`color` must be #rrggbb"))?;
                    Some(c.map(|v| f32::from(v) / 255.0))
                }
            };
            Method::Color(color)
        }
        _ => return Err(bad("`method` must be \"fill\" or \"color\"")),
    };
    let head = head_of(s, p, Output::Current, MARGIN, ())?;
    crate::jobs::run_async_prepared(
        s,
        LABEL,
        true,
        move |_ctx| head.read(),
        move |ctx, plan: Plan<()>, done| {
            ctx.progress(0.0, LABEL);
            let r = generate(&plan, method, ctx);
            done.finish(r.map(|generated| (plan, generated)));
        },
        move |s, (plan, generated): (Plan<()>, Vec<f32>)| {
            let (layer, count, region) = write_result(s, LABEL, plan, &generated)?;
            if let Some(st) = s.active_mut() {
                st.last_damage = Some(region);
            }
            Ok(json!({"layer": layer.0, "pixels": count, "region": [region.x0, region.y0, region.width(), region.height()]}))
        },
    )
}

pub(super) fn spec() -> CommandSpec {
    CommandSpec {
        id: ID,
        label: LABEL,
        menu: &[],
        shortcut: None,
        params: r##"{"method": "fill" (content-aware, from around each blob of the selection) | "color" (a flat colour), "color"?: "#rrggbb" (default: the colour around each blob), "output"?: "current" (default) | "new" | "single", "layerName"?: string, "keepActive"?: bool, "layer"?: id}: cleans what's selected with no model"##,
        enabled: |s| {
            crate::edit_menu_cmds::pixel_layer(s)?;
            let d = s.active().ok_or("no document open")?;
            if d.doc.selection.as_ref().is_none_or(|m| m.content_bounds().is_empty()) {
                return Err("select the text or object to clean".into());
            }
            Ok(())
        },
        journal: true,
        run,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A red page with black letters on a white balloon.
    fn page() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 300, "height": 200, "background": "white"})).unwrap();
        s.execute("select.rect", json!({"x": 150, "y": 0, "width": 150, "height": 200})).unwrap();
        s.execute("edit.fill", json!({"color": "#ff0000"})).unwrap();
        for x in [40, 200] {
            s.execute("select.rect", json!({"x": x, "y": 80, "width": 50, "height": 30})).unwrap();
            s.execute("edit.fill", json!({"color": "#000000"})).unwrap();
        }
        s.execute("select.rect", json!({"x": 40, "y": 80, "width": 50, "height": 30})).unwrap();
        s.execute("select.rect", json!({"x": 200, "y": 80, "width": 50, "height": 30, "mode": "add"})).unwrap();
        s
    }

    fn at(s: &Session, x: i32, y: i32) -> [f32; 3] {
        let p = photocraft_compose::render(&s.active().unwrap().doc, photocraft_geom::Rect::from_xywh(x, y, 1, 1)).px[0];
        [p[0], p[1], p[2]]
    }

    #[test]
    fn a_flat_clean_paints_each_blob_in_the_colour_around_it() {
        let mut s = page();
        s.execute(ID, json!({"method": "color"})).unwrap();
        assert_eq!(at(&s, 60, 95), [1.0, 1.0, 1.0], "white around this one");
        assert_eq!(at(&s, 220, 95), [1.0, 0.0, 0.0], "red around that one");
        s.execute("edit.undo", json!({})).unwrap();
        s.execute(ID, json!({"method": "color", "color": "#00ff00"})).unwrap();
        assert_eq!(at(&s, 60, 95), [0.0, 1.0, 0.0]);
        assert!(s.execute(ID, json!({"method": "color", "color": "green"})).is_err());
        assert!(s.execute(ID, json!({"method": "nope"})).is_err());
    }

    #[test]
    fn a_fill_takes_from_around_each_blob_and_the_output_is_a_cleans() {
        let mut s = page();
        let layers = s.active().unwrap().doc.layers.len();
        s.execute(ID, json!({"method": "fill", "output": "new", "layerName": "Limpieza"})).unwrap();
        assert_eq!(s.active().unwrap().doc.layers.len(), layers + 1, "on a layer of its own");
        let (a, b) = (at(&s, 60, 95), at(&s, 220, 95));
        assert!(a[1] > 0.9, "white around: {a:?}");
        assert!(b[0] > 0.9 && b[1] < 0.1, "red around: {b:?}");
    }

    #[test]
    fn with_nothing_selected_it_says_so() {
        let mut s = page();
        s.execute("select.deselect", json!({})).unwrap();
        assert!(s.execute(ID, json!({"method": "fill"})).is_err());
    }
}
