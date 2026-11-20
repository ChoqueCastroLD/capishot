//! Keep editable stops separate from the dense stops used by the compositors.

use photocraft_doc::effects::GradientInterpolation;
use photocraft_doc::{Color, Gradient};
use serde_json::{Value, json};

use super::{color_of, f_of, overlay_gradient};

/// The most stops a `stops` or `opacityStops` param may carry (Photoshop's editor allows far fewer).
const MAX_STOPS: usize = 256;

/// Explicit colour stops, `"stops": [[location 0..1, colour], …]`, sorted by location. A colour is
/// `"#rrggbb"`, `[r, g, b]` (0..1) or a stored colour object (`{"mode", "c", "alpha"}`, exact, any
/// colour model). `None` when absent or malformed, or with fewer than two stops.
pub fn stops_param(params: &Value) -> Option<Vec<(f32, Color)>> {
    let list = params.get("stops")?.as_array()?;
    if list.len() < 2 || list.len() > MAX_STOPS {
        return None;
    }
    let mut out = Vec::with_capacity(list.len());
    for stop in list {
        let pair = stop.as_array()?;
        let location = pair.first()?.as_f64()? as f32;
        let raw = pair.get(1)?;
        let color = match serde_json::from_value::<Color>(raw.clone()) {
            Ok(c) if c.c.iter().all(|v| v.is_finite()) && c.alpha.is_finite() => Color { alpha: c.alpha.clamp(0.0, 1.0), ..c },
            _ => color_of(&json!({ "c": raw }), "c")?,
        };
        if !location.is_finite() {
            return None;
        }
        out.push((location.clamp(0.0, 1.0), color));
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    Some(out)
}

/// Explicit transparency stops, `"opacityStops": [[location 0..1, opacity 0..100], …]`, sorted;
/// an empty list means fully opaque. `None` when absent or malformed.
pub fn opacity_stops_param(params: &Value) -> Option<Vec<(f32, f32)>> {
    let list = params.get("opacityStops")?.as_array()?;
    if list.len() > MAX_STOPS {
        return None;
    }
    let mut out = Vec::with_capacity(list.len());
    for stop in list {
        let pair = stop.as_array()?;
        let (location, opacity) = (pair.first()?.as_f64()? as f32, pair.get(1)?.as_f64()? as f32);
        if !location.is_finite() || !opacity.is_finite() {
            return None;
        }
        out.push((location.clamp(0.0, 1.0), (opacity / 100.0).clamp(0.0, 1.0)));
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    Some(out)
}

fn edit_endpoint(stop: Option<&mut (f32, Color)>, params: &Value, key: &str) {
    if let (Some(stop), Some(color)) = (stop, color_of(params, key)) {
        // The dialog displays hex colours. Opening it must not quantize the saved source.
        let [r, g, b, _] = color.to_rgba8();
        let [sr, sg, sb, _] = stop.1.to_rgba8();
        if !params.get(key).is_some_and(Value::is_string) || [r, g, b] != [sr, sg, sb] {
            let mut edited = color.in_mode(stop.1.mode);
            edited.alpha = stop.1.alpha;
            stop.1 = edited;
        }
    }
}

/// `params` without a `from`/`to` hex string that only repeats the gradient's current end as the
/// dialog shows it (8-bit): confirming the dialog unchanged must not quantize a baked or
/// imported curve's exact ends (as `edit_endpoint` guards the recorded source).
fn without_unchanged_ends(gradient: &Gradient, params: &Value) -> Value {
    let mut params = params.clone();
    for (key, end) in [("from", gradient.stops.first()), ("to", gradient.stops.last())] {
        let shown = |c: &Color| {
            let [r, g, b, _] = c.to_rgba8();
            [r, g, b]
        };
        let same = params.get(key).is_some_and(Value::is_string) && end.zip(color_of(&params, key)).is_some_and(|(end, color)| shown(&end.1) == shown(&color));
        if same && let Some(object) = params.as_object_mut() {
            object.remove(key);
        }
    }
    params
}

/// Edits recorded source stops, initializing legacy curves from their end colours.
/// Explicit `stops` (a preset picked in the dialog, or its stop strip) replace the editable
/// colour stops before the `from`/`to` end edits apply; `opacityStops` replace the transparency.
pub(super) fn overlay(gradient: &mut Gradient, interpolation: &mut Option<GradientInterpolation>, params: &Value) {
    let explicit = stops_param(params);
    if let Some(opacity) = opacity_stops_param(params) {
        gradient.opacity_stops = opacity;
    }
    // Reset to Default restores the factory's unbaked interpolation too.
    if params.get("method").and_then(Value::as_str) == Some("custom") {
        if let Some(source) = interpolation.take() {
            gradient.stops = source.stops;
        }
        if let Some(stops) = explicit {
            gradient.stops = stops;
        }
        overlay_gradient(gradient, &without_unchanged_ends(gradient, params));
        return;
    }
    let requested = params.get("method").and_then(Value::as_str).filter(|name| photocraft_io::GradientMethod::from_name(name).is_some());
    if interpolation.is_none() && requested.is_none() {
        if let Some(stops) = explicit {
            gradient.stops = stops;
        }
        overlay_gradient(gradient, &without_unchanged_ends(gradient, params));
        return;
    }
    let previous = interpolation.clone();
    let source = interpolation.get_or_insert_with(|| GradientInterpolation {
        method: requested.unwrap_or("classic").to_ascii_lowercase(),
        smoothness: 100.0,
        // Without a source record, intermediate stops may be baked samples (PSD import or
        // older method commands). Keep the legacy rebuild-from-ends behaviour on first choice.
        stops: vec![(0.0, gradient.stops.first().map_or(Color::BLACK, |s| s.1)), (1.0, gradient.stops.last().map_or(Color::WHITE, |s| s.1))],
    });
    if let Some(stops) = explicit {
        source.stops = stops;
    }
    if let Some(method) = requested {
        source.method = method.to_ascii_lowercase();
    }
    if let Some(smoothness) = f_of(params, "smoothness").filter(|n| n.is_finite()) {
        source.smoothness = smoothness.clamp(0.0, 100.0);
    }
    edit_endpoint(source.stops.first_mut(), params, "from");
    edit_endpoint(source.stops.last_mut(), params, "to");
    if previous.as_ref() != Some(source)
        && let Some(method) = photocraft_io::GradientMethod::from_name(&source.method)
    {
        // Missing midpoint entries mean 50 %, including for multi-stop source gradients.
        gradient.stops = photocraft_io::bake_gradient(source.stops.clone(), &[], source.smoothness / 100.0, method);
    }
    // Geometry edits must not replace just the ends of the baked curve.
    let mut geometry = params.clone();
    if let Some(object) = geometry.as_object_mut() {
        for key in ["method", "smoothness", "from", "to", "stops", "opacityStops"] {
            object.remove(key);
        }
    }
    overlay_gradient(gradient, &geometry);
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::BlendMode;
    use photocraft_doc::{Effect, FxCommon};
    use photocraft_io::{GradientMethod, bake_gradient};
    use serde_json::json;

    #[test]
    fn choosing_a_method_on_a_baked_overlay_rebuilds_from_its_end_colours() {
        let ends = vec![(0.0, Color::BLACK), (1.0, Color::WHITE)];
        for imported_method in [GradientMethod::Perceptual, GradientMethod::Smooth] {
            let gradient = Gradient {
                stops: bake_gradient(ends.clone(), &[0.5], 1.0, imported_method),
                opacity_stops: vec![(0.0, 0.25), (0.6, 0.8), (1.0, 0.5)],
                align: false,
                offset: (0.2, -0.1),
                ..Gradient::default()
            };
            assert!(gradient.stops.len() > 2);
            let source_ends = vec![(0.0, gradient.stops.first().unwrap().1), (1.0, gradient.stops.last().unwrap().1)];
            let mut fx =
                Effect::GradientOverlay { common: FxCommon::new(BlendMode::Normal, 1.0), gradient: gradient.clone(), dither: true, interpolation: None };
            super::super::overlay_effect(&mut fx, &json!({"method": "linear"}));
            let mut expected = gradient;
            expected.stops = bake_gradient(source_ends.clone(), &[0.5], 1.0, GradientMethod::Linear);
            assert_eq!(
                fx,
                Effect::GradientOverlay {
                    common: FxCommon::new(BlendMode::Normal, 1.0),
                    gradient: expected,
                    dither: true,
                    interpolation: Some(GradientInterpolation { method: "linear".into(), smoothness: 100.0, stops: source_ends }),
                }
            );
        }
    }

    #[test]
    fn endpoint_edits_after_selecting_a_method_recolour_the_whole_baked_overlay() {
        for same_edit in [false, true] {
            let mut fx = Effect::GradientOverlay {
                common: FxCommon::new(BlendMode::Normal, 1.0),
                gradient: Gradient {
                    stops: bake_gradient(vec![(0.0, Color::BLACK), (1.0, Color::WHITE)], &[0.5], 1.0, GradientMethod::Smooth),
                    ..Gradient::default()
                },
                dither: false,
                interpolation: None,
            };
            if same_edit {
                super::super::overlay_effect(&mut fx, &json!({"method": "classic", "from": "#ff0000", "to": "#0000ff"}));
            } else {
                super::super::overlay_effect(&mut fx, &json!({"method": "classic"}));
                super::super::overlay_effect(&mut fx, &json!({"from": "#ff0000", "to": "#0000ff"}));
            }
            let ends = vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (1.0, Color::rgb(0.0, 0.0, 1.0))];
            let Effect::GradientOverlay { gradient, interpolation, .. } = fx else { panic!("expected overlay") };
            assert_eq!(gradient.stops, bake_gradient(ends.clone(), &[0.5], 1.0, GradientMethod::Classic));
            assert_eq!(interpolation.unwrap().stops, ends);
        }
    }

    #[test]
    fn changing_method_keeps_all_source_stops_and_transparency() {
        let mut gradient = Gradient {
            stops: vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (0.3, Color::rgb(0.0, 1.0, 0.0)), (1.0, Color::rgb(0.0, 0.0, 1.0))],
            opacity_stops: vec![(0.0, 0.25), (1.0, 0.8)],
            align: false,
            offset: (0.2, -0.1),
            ..Gradient::default()
        };
        let original = gradient.clone();
        let mut source = Some(GradientInterpolation { method: "classic".into(), smoothness: 100.0, stops: original.stops.clone() });
        gradient.stops = bake_gradient(original.stops.clone(), &[0.5, 0.5], 1.0, GradientMethod::Classic);
        for method in ["classic", "smooth", "perceptual", "linear", "classic"] {
            overlay(&mut gradient, &mut source, &json!({"method": method}));
            let source = source.as_ref().unwrap();
            assert_eq!(source.method, method);
            assert_eq!(source.stops, original.stops);
            let expected = photocraft_io::bake_gradient(original.stops.clone(), &[0.5, 0.5], 1.0, photocraft_io::GradientMethod::from_name(method).unwrap());
            assert_eq!(gradient.stops, expected);
            assert_eq!(gradient.opacity_stops, original.opacity_stops);
            assert_eq!(gradient.align, original.align);
            assert_eq!(gradient.offset, original.offset);
        }
        overlay(&mut gradient, &mut source, &json!({"from": "#112233", "to": "#445566"}));
        let source = source.unwrap();
        assert_eq!(source.stops.len(), 3);
        assert_eq!(source.stops[1], original.stops[1], "endpoint edits preserve the middle source stop");
        assert_eq!(gradient.stops, bake_gradient(source.stops, &[0.5, 0.5], 1.0, GradientMethod::Classic));
    }

    #[test]
    fn colour_edit_rebakes_the_curve_and_geometry_edits_leave_it_exact() {
        let mut gradient = Gradient::default();
        let mut source = None;
        overlay(&mut gradient, &mut source, &json!({"method": "smooth", "from": "#ff0000", "to": "#0000ff"}));
        let before = gradient.stops.clone();
        overlay(&mut gradient, &mut source, &json!({"from": "#00ff00", "angle": 45}));
        assert_ne!(gradient.stops, before);
        let expected = photocraft_io::bake_gradient(
            vec![(0.0, Color::rgb(0.0, 1.0, 0.0)), (1.0, Color::rgb(0.0, 0.0, 1.0))],
            &[0.5],
            1.0,
            photocraft_io::GradientMethod::Smooth,
        );
        assert_eq!(gradient.stops, expected);
        overlay(&mut gradient, &mut source, &json!({"angle": 90, "reverse": true, "scale": 75}));
        assert_eq!(gradient.stops, expected);
        assert_eq!(gradient.angle, 90.0);
        assert!(gradient.reverse);
        assert_eq!(gradient.scale, 0.75);
    }

    #[test]
    fn reopening_does_not_quantize_source_colours_or_rebake() {
        let mut gradient = Gradient::default();
        gradient.stops[0].1.alpha = 0.37;
        let mut source = None;
        overlay(&mut gradient, &mut source, &json!({"from": [0.123456, 0.234567, 0.345678], "method": "perceptual"}));
        let before = (gradient.clone(), source.clone());
        assert_eq!(source.as_ref().unwrap().stops[0].1.alpha, 0.37);
        let [r, g, b, _] = source.as_ref().unwrap().stops[0].1.to_rgba8();
        overlay(&mut gradient, &mut source, &json!({"from": format!("#{r:02x}{g:02x}{b:02x}"), "to": "#ffffff", "method": "perceptual", "smoothness": 100}));
        assert_eq!((gradient, source), before);
    }

    #[test]
    fn old_gradients_are_unchanged_until_a_method_is_chosen() {
        let mut gradient = Gradient::default();
        gradient.stops.insert(1, (0.4, Color::rgb(0.1, 0.9, 0.2)));
        let original = gradient.clone();
        let mut source = None;
        overlay(&mut gradient, &mut source, &json!({"method": "custom", "opacity": 50}));
        assert_eq!(gradient, original);
        assert!(source.is_none());
    }

    #[test]
    fn endpoint_edits_keep_the_source_colour_model() {
        for mode in [photocraft_doc::ColorMode::Grayscale, photocraft_doc::ColorMode::Cmyk, photocraft_doc::ColorMode::Lab] {
            let mut gradient = Gradient { stops: vec![(0.0, Color::BLACK.in_mode(mode)), (1.0, Color::WHITE.in_mode(mode))], ..Gradient::default() };
            let mut source = None;
            overlay(&mut gradient, &mut source, &json!({"method": "classic"}));
            overlay(&mut gradient, &mut source, &json!({"from": "#ff0000"}));
            assert!(source.as_ref().unwrap().stops.iter().all(|s| s.1.mode == mode));
        }
    }

    #[test]
    fn reset_to_factory_defaults_removes_the_recorded_method() {
        let mut gradient = Gradient::default();
        let mut source = None;
        overlay(&mut gradient, &mut source, &json!({"method": "smooth", "from": "#ff0000"}));
        overlay(&mut gradient, &mut source, &super::super::effect_defaults("gradientOverlay"));
        assert!(source.is_none());
        assert_eq!(gradient, Gradient::default());
    }

    #[test]
    fn explicit_stops_replace_the_colour_stops_and_keep_opacity() {
        let three = vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (0.4, Color::rgb(0.0, 1.0, 0.0)), (1.0, Color::rgb(0.0, 0.0, 1.0))];
        let stops = json!(three.iter().map(|(t, c)| json!([t, c])).collect::<Vec<_>>());
        // Recorded and unrecorded interpolation, and the dialog's default `custom`.
        for (method, recorded) in [("custom", false), ("custom", true), ("classic", true), ("smooth", false)] {
            let mut gradient = Gradient { opacity_stops: vec![(0.0, 0.3), (1.0, 0.9)], ..Gradient::default() };
            let mut source =
                recorded.then(|| GradientInterpolation { method: "classic".into(), smoothness: 100.0, stops: Gradient::default().stops });
            // The dialog sends the new ends as `from`/`to` hex too: they must not re-quantize them.
            overlay(&mut gradient, &mut source, &json!({"method": method, "stops": stops, "from": "#ff0000", "to": "#0000ff"}));
            let editable = source.as_ref().map_or(&gradient.stops, |s| &s.stops);
            assert_eq!(editable, &three, "{method} {recorded}");
            assert_eq!(gradient.opacity_stops, vec![(0.0, 0.3), (1.0, 0.9)]);
            if let Some(m) = source.as_ref().and_then(|s| GradientMethod::from_name(&s.method)) {
                assert_eq!(gradient.stops, bake_gradient(three.clone(), &[0.5, 0.5], 1.0, m));
            }
        }
    }

    #[test]
    fn stop_params_accept_hex_and_rgb_and_reject_garbage() {
        let parsed = stops_param(&json!({"stops": [[1.0, "#0000ff"], [0.0, [1.0, 0.0, 0.0]]]})).unwrap();
        assert_eq!(parsed, vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (1.0, Color::rgb(0.0, 0.0, 1.0))]);
        for bad in [json!({"stops": [[0.0, "#000000"]]}), json!({"stops": "x"}), json!({"stops": [[0.0, "zz"], [1.0, 3]]}), json!({"stops": [[null, "#000000"], [1, "#ffffff"]]})] {
            assert!(stops_param(&bad).is_none(), "{bad}");
        }
        assert_eq!(opacity_stops_param(&json!({"opacityStops": [[1.0, 50], [0.0, 100]]})), Some(vec![(0.0, 1.0), (1.0, 0.5)]));
        assert_eq!(opacity_stops_param(&json!({"opacityStops": []})), Some(vec![]));
        assert!(opacity_stops_param(&json!({"opacityStops": [[0.0]]})).is_none());
        let fx = super::super::effect_from_params("gradientOverlay", &json!({"stops": [[0, "#ff0000"], [0.5, "#00ff00"], [1, "#0000ff"]]})).unwrap();
        let Effect::GradientOverlay { gradient, .. } = fx else { panic!("overlay") };
        assert_eq!(gradient.stops.len(), 3);
    }
}
