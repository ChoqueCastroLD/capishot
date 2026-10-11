//! Live previews of the retouching strokes (`LiveRetouch`, `LiveDab`): they show what the commit
//! paints, and stay local on a full manga page.

use super::*;

fn session(w: u32, h: u32, depth: u64) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h, "depth": depth, "mode": "rgb"})).unwrap();
    s
}

fn paint_layer(s: &mut Session, f: impl Fn(i32, i32) -> [f32; 4]) {
    s.edit("setup", |doc, active| {
        let b = doc.bounds();
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        let fmt = surf.format();
        let mut data = Vec::new();
        for y in b.y0..b.y1 {
            for x in b.x0..b.x1 {
                data.extend(photocraft_raster::from_rgba(&fmt, f(x, y)));
            }
        }
        surf.write_region(b, &data);
        Ok(())
    })
    .unwrap();
}

fn pts(v: &[[f64; 2]]) -> Vec<StrokePoint> {
    v.iter().map(|q| StrokePoint::new(q[0], q[1], 1.0)).collect()
}

fn with_points(p: &Value, points: &[[f64; 2]]) -> Value {
    let mut p = p.clone();
    p["points"] = json!(points);
    p
}

/// Feed `all` to a live stroke one point at a time, as the canvas does while dragging.
fn live_retouch(s: &Session, cmd: &str, p: &Value, all: &[[f64; 2]]) -> std::sync::Arc<Document> {
    let mut live = LiveRetouch::begin(s, cmd, &with_points(p, &all[..1])).unwrap();
    for q in &all[1..] {
        live.push(&pts(&[*q])).unwrap();
    }
    live.doc.clone()
}

fn live_dab(s: &Session, cmd: &str, p: &Value, all: &[[f64; 2]]) -> std::sync::Arc<Document> {
    let mut live = LiveDab::begin(s, cmd, &with_points(p, &all[..1])).unwrap();
    for q in &all[1..] {
        live.push(&pts(&[*q])).unwrap();
    }
    live.doc.clone()
}

/// Worst and mean channel difference between the live document and the committed one over `r`.
fn diff(shown: &Document, s: &Session, r: Rect) -> (f32, f32) {
    let st = s.active().unwrap();
    let id = st.active_layer.unwrap();
    let (a, b) = (shown.layer(id).unwrap().surface().unwrap(), st.doc.layer(id).unwrap().surface().unwrap());
    let (mut worst, mut sum, mut n) = (0.0f32, 0.0f32, 0usize);
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            let (g, w) = (a.rgba(x, y), b.rgba(x, y));
            for c in 0..4 {
                let d = (g[c] - w[c]).abs();
                worst = worst.max(d);
                sum += d;
                n += 1;
            }
        }
    }
    (worst, sum / n.max(1) as f32)
}

/// Stripes on the left (the texture to clone), a flat light colour on the right.
fn stripes_and_flat(x: i32, _: i32) -> [f32; 4] {
    if x < 64 {
        let t = if (x / 3) % 2 == 0 { 0.08 } else { -0.08 };
        [0.2 + t, 0.25 + t, 0.5 + t, 1.0]
    } else {
        [0.9, 0.6, 0.3, 1.0]
    }
}

/// The Healing Brush heals while it is drawn (Photoshop CC's real-time healing): the shown pixels
/// take the destination's colour and the source's texture, not the raw source, and end up close
/// to what the commit (one solve over the whole stroke) paints.
#[test]
fn live_healing_blends_while_drawing_and_lands_near_the_commit() {
    for depth in [8u64, 32] {
        let mut s = session(160, 64, depth);
        paint_layer(&mut s, stripes_and_flat);
        let p = json!({"source": [24, 32], "size": 24, "hardness": 100});
        let all = [[84.0, 32.0], [92.0, 32.0], [100.0, 33.0], [108.0, 32.0], [116.0, 31.0], [124.0, 32.0]];
        let shown = live_retouch(&s, "paint.healingBrush", &p, &all);
        let id = s.active().unwrap().active_layer.unwrap();
        let surf = shown.layer(id).unwrap().surface().unwrap();
        let vals: Vec<[f32; 4]> = (-6..=6).flat_map(|dy| (-6..=6).map(move |dx| (104 + dx, 32 + dy))).map(|(x, y)| surf.rgba(x, y)).collect();
        let mean: Vec<f32> = (0..3).map(|c| vals.iter().map(|v| v[c]).sum::<f32>() / vals.len() as f32).collect();
        for (c, want) in [0.9f32, 0.6, 0.3].iter().enumerate() {
            assert!((mean[c] - want).abs() < 0.06, "depth {depth} channel {c}: live mean {} want {want} (blended, not the raw source)", mean[c]);
        }
        let var = vals.iter().map(|v| (v[1] - mean[1]).powi(2)).sum::<f32>() / vals.len() as f32;
        assert!(var > 0.002, "depth {depth}: the source texture shows live ({var})");
        s.execute("paint.healingBrush", with_points(&p, &all)).unwrap();
        let (worst, mean) = diff(&shown, &s, Rect::new(64, 12, 150, 52));
        assert!(mean < 0.01 && worst < 0.08, "depth {depth}: live vs commit worst {worst}, mean {mean}");
    }
}

/// Healing with Sample: All Layers onto an empty layer heals what is visible, live as on release.
#[test]
fn live_healing_samples_all_layers_onto_an_empty_layer() {
    let mut s = session(160, 64, 32);
    paint_layer(&mut s, stripes_and_flat);
    s.execute("layer.new.layer", json!({})).unwrap();
    let p = json!({"source": [24, 32], "size": 20, "hardness": 100, "sampleLayer": "all"});
    let all = [[96.0, 32.0], [110.0, 32.0]];
    let shown = live_retouch(&s, "paint.healingBrush", &p, &all);
    let id = s.active().unwrap().active_layer.unwrap();
    let px = shown.layer(id).unwrap().surface().unwrap().rgba(103, 32);
    assert!(px[3] > 0.99 && (px[0] - 0.9).abs() < 0.12, "healed onto the empty layer: {px:?}");
    s.execute("paint.healingBrush", with_points(&p, &all)).unwrap();
    let (worst, mean) = diff(&shown, &s, Rect::new(70, 12, 140, 52));
    assert!(mean < 0.01 && worst < 0.1, "live vs commit worst {worst}, mean {mean}");
}

/// The Pattern Stamp shows exactly what it commits, aligned or not, and Impressionist with the
/// stroke's seed.
#[test]
fn live_pattern_stamp_matches_the_commit() {
    let all = [[20.0, 20.0], [40.0, 26.0], [60.0, 22.0], [80.0, 30.0]];
    for (extra, impressionist) in [
        (json!({"aligned": true}), false),
        (json!({"aligned": false, "scale": 150, "angle": 20}), false),
        (json!({"impressionist": true, "seed": 7}), true),
    ] {
        let mut s = session(100, 50, 32);
        paint_layer(&mut s, |x, y| [0.2 + x as f32 / 400.0, 0.3, 0.4 + y as f32 / 200.0, 1.0]);
        let mut p = json!({"size": 16, "hardness": 60, "opacity": 80, "pattern": "Checkerboard"});
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let shown = if impressionist { live_dab(&s, "paint.patternStamp", &p, &all) } else { live_retouch(&s, "paint.patternStamp", &p, &all) };
        s.execute("paint.patternStamp", with_points(&p, &all)).unwrap();
        let (worst, _) = diff(&shown, &s, Rect::new(0, 0, 100, 50));
        // The commit also lays the stroke's last dab, which the live stroke shows on release.
        let (worst_head, _) = diff(&shown, &s, Rect::new(0, 0, 66, 50));
        assert!(worst_head <= 1e-4, "{extra}: live and commit differ by {worst_head} (whole canvas {worst})");
    }
    // A live Impressionist stroke needs the seed its commit will use.
    let s = session(40, 40, 8);
    assert!(LiveDab::begin(&s, "paint.patternStamp", &json!({"points": [[10, 10]], "impressionist": true, "pattern": "Checkerboard"})).is_err());
    assert!(LiveRetouch::begin(&s, "paint.patternStamp", &json!({"points": [[10, 10]], "impressionist": true, "seed": 1})).is_err());
}

/// Blur and Smudge with Sample All Layers preview live on an empty layer, as they commit.
#[test]
fn live_sample_all_layers_dabs_match_the_commit() {
    let checker = |x: i32, y: i32| if ((x / 2) + (y / 2)) % 2 == 0 { [0.3, 0.35, 0.4, 1.0] } else { [0.7, 0.65, 0.6, 1.0] };
    let all = [[10.0, 15.0], [30.0, 16.0], [50.0, 14.0], [70.0, 15.0]];
    for (cmd, p) in [
        ("paint.blur", json!({"size": 14, "hardness": 80, "strength": 100, "sampleAllLayers": true})),
        ("paint.smudge", json!({"size": 12, "hardness": 100, "strength": 70, "sampleAllLayers": true})),
    ] {
        let mut s = session(100, 30, 32);
        paint_layer(&mut s, checker);
        s.execute("layer.new.layer", json!({})).unwrap();
        let shown = live_dab(&s, cmd, &p, &all);
        let id = s.active().unwrap().active_layer.unwrap();
        assert!(shown.layer(id).unwrap().surface().unwrap().rgba(40, 15)[3] > 0.99, "{cmd}: the empty layer shows the effect live");
        s.execute(cmd, with_points(&p, &all)).unwrap();
        let (worst, _) = diff(&shown, &s, Rect::new(0, 0, 62, 30));
        assert!(worst <= 1e-4, "{cmd}: live and commit differ by {worst}");
    }
}

/// On a 4000 × 6000 page a live stroke renders only the tiles it touches: no full-surface
/// composite or scan per stroke or per step.
#[test]
fn live_strokes_on_a_full_page_stay_local() {
    let mut s = session(4000, 6000, 8);
    paint_layer(&mut s, |x, y| if (x / 3 + y / 3) % 2 == 0 { [0.95, 0.95, 0.95, 1.0] } else { [0.1, 0.1, 0.1, 1.0] });
    s.execute("layer.new.layer", json!({})).unwrap();
    let all: Vec<[f64; 2]> = (0..20).map(|i| [1000.0 + 20.0 * f64::from(i), 3000.0]).collect();
    let mut live = LiveDab::begin(&s, "paint.blur", &json!({"points": [all[0]], "size": 60, "sampleAllLayers": true})).unwrap();
    live.push(&pts(&all[1..])).unwrap();
    let tiles = live.visible.as_ref().unwrap().rendered_tiles();
    assert!((1..=6).contains(&tiles), "Sample All Layers rendered {tiles} tiles");
    let mut live = LiveRetouch::begin(&s, "paint.healingBrush", &json!({"points": [all[0]], "source": [600, 600], "size": 60, "sampleLayer": "all"})).unwrap();
    live.push(&pts(&all[1..])).unwrap();
    let LivePaint::Heal { healed, .. } = &live.paint else { panic!("a healing stroke") };
    assert!((1..=6).contains(&healed.rendered_tiles()), "healing rendered {} tiles", healed.rendered_tiles());
}

/// Release-build timings of each live retouching tool on a 4000 × 6000 page (see the commit
/// message): `cargo test -p photocraft-engine --release live_retouch_timings -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark"]
fn live_retouch_timings() {
    let mut s = session(4000, 6000, 8);
    paint_layer(&mut s, |x, y| {
        if (2900..3100).contains(&y) && (1200..1800).contains(&x) && (x / 7) % 3 == 0 {
            [0.0, 0.0, 0.0, 1.0]
        } else if (x / 4 + y / 4) % 2 == 0 {
            [0.85, 0.85, 0.85, 1.0]
        } else {
            [1.0, 1.0, 1.0, 1.0]
        }
    });
    s.execute("layer.new.layer", json!({})).unwrap();
    let id = s.active().unwrap().doc.walk().first().map(|(_, _, l)| l.id).unwrap();
    s.active_mut().unwrap().active_layer = Some(id);
    // A 600 px drag in 4 px pointer steps, as a mouse sends them.
    let path: Vec<[f64; 2]> = (0..150).map(|i| [1200.0 + 4.0 * f64::from(i), 3000.0 + (f64::from(i) / 9.0).sin() * 30.0]).collect();
    let size = 80;
    let cases: Vec<(&str, Value, bool)> = vec![
        ("paint.cloneStamp", json!({"source": [600, 600]}), false),
        ("paint.cloneStamp", json!({"source": [600, 600], "sampleLayer": "all"}), false),
        ("paint.healingBrush", json!({"source": [600, 600]}), false),
        ("paint.healingBrush", json!({"source": [600, 600], "sampleLayer": "all"}), false),
        ("paint.historyBrush", json!({}), false),
        ("paint.patternStamp", json!({"pattern": "Checkerboard"}), false),
        ("paint.patternStamp", json!({"pattern": "Checkerboard", "impressionist": true, "seed": 3}), true),
        ("paint.dodge", json!({}), true),
        ("paint.burn", json!({}), true),
        ("paint.sponge", json!({}), true),
        ("paint.blur", json!({}), true),
        ("paint.blur", json!({"sampleAllLayers": true}), true),
        ("paint.sharpen", json!({}), true),
        ("paint.smudge", json!({}), true),
        ("paint.smudge", json!({"sampleAllLayers": true}), true),
    ];
    for (cmd, extra, dab) in cases {
        let mut p = json!({"size": size, "hardness": 80, "points": [path[0]]});
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let t0 = std::time::Instant::now();
        let mut steps = Vec::new();
        if dab {
            let mut live = LiveDab::begin(&s, cmd, &p).unwrap();
            let begin = t0.elapsed();
            for q in &path[1..] {
                let t = std::time::Instant::now();
                live.push(&pts(&[*q])).unwrap();
                steps.push(t.elapsed());
            }
            report(cmd, &extra, begin, &steps);
        } else {
            let mut live = LiveRetouch::begin(&s, cmd, &p).unwrap();
            let begin = t0.elapsed();
            for q in &path[1..] {
                let t = std::time::Instant::now();
                live.push(&pts(&[*q])).unwrap();
                steps.push(t.elapsed());
            }
            report(cmd, &extra, begin, &steps);
        }
    }
    fn report(cmd: &str, extra: &Value, begin: std::time::Duration, steps: &[std::time::Duration]) {
        let mut ms: Vec<f64> = steps.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
        ms.sort_by(f64::total_cmp);
        let mean = ms.iter().sum::<f64>() / ms.len().max(1) as f64;
        let p95 = ms.get(ms.len() * 95 / 100).copied().unwrap_or(0.0);
        let max = ms.last().copied().unwrap_or(0.0);
        println!("{cmd:20} {extra:58} begin {:6.2} ms  step mean {mean:6.2} p95 {p95:6.2} max {max:6.2} ms", begin.as_secs_f64() * 1000.0);
    }
}

#[test]
#[ignore = "benchmark"]
fn heal_region_timing_probe() {
    let fmt = photocraft_color::PixelFormat::RGBA8;
    let r = Rect::new(0, 0, 92, 92);
    let mut src = Region::new(r, 4);
    let mut dst = Region::new(r, 4);
    for (i, v) in src.data.iter_mut().enumerate() {
        *v = ((i * 37) % 255) as f32 / 255.0;
    }
    for (i, v) in dst.data.iter_mut().enumerate() {
        *v = ((i * 11) % 255) as f32 / 255.0;
    }
    let mask: Vec<bool> = (0..92 * 92).map(|i| ((i % 92) as i32 - 46).pow(2) + ((i / 92) as i32 - 46).pow(2) < 40 * 40).collect();
    for _ in 0..3 {
        let t = std::time::Instant::now();
        let _ = heal_region(&fmt, &src, &dst, &mask);
        println!("heal_region 92x92: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
    }
}
