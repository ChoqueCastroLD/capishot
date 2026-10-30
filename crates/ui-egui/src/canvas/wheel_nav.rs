//! Mouse wheel and trackpad navigation over the canvas (#293, #635):
//!
//! - The wheel scrolls up and down; ⇧ + wheel scrolls sideways.
//! - ⌘/Ctrl + wheel and ⌥/Alt + wheel zoom around the pointer.
//! - Preferences › General › Zoom with Scroll Wheel makes the plain wheel zoom too, and
//!   ⌥/Alt + wheel scroll instead.
//! - A trackpad pinch zooms around the pointer (a touch pinch around its centre), unless
//!   Preferences › Enhanced Controls › Zoom with Trackpad Pinch is off.
//!
//! Wheel zoom is continuous: the factor is `exp(K × d)` for a wheel delta of `d` pixels, with
//! `K` set so one mouse-wheel notch (100 px, or one line natively, three lines in a browser) is
//! ×[`NOTCH`]. A high-resolution wheel or a trackpad sends many small deltas that add up to the
//! same factor, so the zoom follows the fingers instead of jumping between presets. One frame's
//! change is capped at ×[`MAX_STEP`] either way. Zoom In / Zoom Out (⌘+ / ⌘−) and the Zoom tool
//! keep Photoshop's preset ladder (`zoom_levels`).
//!
//! [`configure`] makes egui fold ⌘/Ctrl + wheel into a sideways scroll for scroll areas (panels,
//! lists), and keeps egui from turning ⌘/Ctrl + wheel into its own zoom: the canvas reads the
//! wheel events itself. egui smooths a notch over several frames, so zooming reads the wheel
//! events and ignores that smoothed tail; panning keeps the smoothed delta. The modifier state of
//! the gesture is taken from the wheel events and kept until the next wheel event: releasing the
//! key while the tail is still settling must not turn it into a pan.
//!
//! Windows hands a precision touchpad pinch to apps as Ctrl + wheel in fractions of a notch:
//! [`fold_legacy_pinch`] turns those back into a pinch before egui sees them. In a browser,
//! eframe turns a Ctrl + wheel it believes is a pinch into `Event::Zoom` at a speed meant for
//! trackpads (×2.7 for one Chrome mouse notch); the web shell records the real Ctrl + wheel events
//! and [`restore_ctrl_wheel`] puts them back.

use egui::{Context, Event, Id, Modifiers, MouseWheelUnit, RawInput, Vec2};

/// Zoom factor of one mouse-wheel notch.
pub const NOTCH: f32 = 1.12;

/// Wheel pixels in one notch: Chrome and Edge report 100 px a notch.
pub const NOTCH_PX: f32 = 100.0;

/// Wheel lines in one notch: browsers in line mode (Firefox) report 3, native wheels 1.
pub const LINES_PER_NOTCH: f32 = if cfg!(target_arch = "wasm32") { 3.0 } else { 1.0 };

/// The largest zoom change one frame may make (and its inverse), however fast the wheel spins.
pub const MAX_STEP: f32 = 2.0;

/// A Ctrl + wheel event this soon (seconds) after a pinch step still belongs to the pinch, even
/// when it happens to be a whole notch.
const PINCH_GAP: f64 = 0.25;

/// What the wheel does to the view this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wheel {
    /// Multiply the zoom by this factor around the pointer.
    Zoom(f32),
    /// Step N rungs on the zoom ladder (positive = in, negative = out). The continuous wheel
    /// zoom never asks for it; callers that share the ladder handle it.
    SnapZoom(i32),
    /// Move the image by this many screen points (egui's content direction).
    Pan(Vec2),
}

/// Which modifier the wheel gesture in progress is held with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Gesture {
    /// No modifier, or ⇧ (sideways scroll).
    #[default]
    Plain,
    /// ⌥/Alt alone.
    Alt,
    /// ⌘/Ctrl (with or without others).
    Ctrl,
}

impl Gesture {
    /// The gesture a wheel event with `m` held belongs to.
    pub fn of(m: Modifiers) -> Self {
        if m.ctrl || m.command || m.mac_cmd {
            Self::Ctrl
        } else if m.alt {
            Self::Alt
        } else {
            Self::Plain
        }
    }
}

/// Inputs of one frame's wheel gesture.
#[derive(Clone, Copy, Debug)]
pub struct Input {
    /// Smoothed scroll delta (points, content direction); what panning uses.
    pub scroll: Vec2,
    /// This frame's wheel events for the gesture in progress, unsmoothed, in wheel pixels
    /// (content direction; see [`wheel_px`]); what zooming uses.
    pub raw: Vec2,
    /// egui's pinch zoom factor (1 = none).
    pub zoom_delta: f32,
    /// The modifier the wheel gesture in progress is held with.
    pub gesture: Gesture,
    /// Preferences › General › Zoom with Scroll Wheel.
    pub zoom_with_wheel: bool,
    /// Preferences › Enhanced Controls › Zoom with Trackpad Pinch.
    pub pinch_zooms: bool,
}

/// `f` capped to one frame's largest change; `None` for no change or a hostile number.
fn capped(f: f32) -> Option<f32> {
    (f.is_finite() && f > 0.0 && f != 1.0).then(|| f.clamp(1.0 / MAX_STEP, MAX_STEP))
}

/// The zoom factor of a wheel delta of `px` wheel pixels (positive zooms in): `NOTCH` per
/// `NOTCH_PX`, continuous in between, capped at [`MAX_STEP`] a frame.
pub fn wheel_factor(px: f32) -> Option<f32> {
    capped((NOTCH.ln() / NOTCH_PX * px).exp())
}

/// A wheel delta in wheel pixels: points as they are, lines at `NOTCH_PX / LINES_PER_NOTCH`, pages
/// at `page` points.
pub fn wheel_px(unit: MouseWheelUnit, delta: Vec2, page: f32) -> Vec2 {
    delta
        * match unit {
            MouseWheelUnit::Point => 1.0,
            MouseWheelUnit::Line => NOTCH_PX / LINES_PER_NOTCH,
            MouseWheelUnit::Page => {
                if page.is_finite() && page > 0.0 {
                    page
                } else {
                    NOTCH_PX
                }
            }
        }
}

/// The view change for one frame of wheel input, if any.
pub fn classify(i: Input) -> Option<Wheel> {
    if i.pinch_zooms && i.zoom_delta.is_finite() && i.zoom_delta > 0.0 && i.zoom_delta != 1.0 {
        return capped(i.zoom_delta).map(Wheel::Zoom);
    }
    let zoom = |d: f32| wheel_factor(d).map(Wheel::Zoom);
    match (i.gesture, i.zoom_with_wheel) {
        // Whichever axis the platform reports a modified wheel on; the smoothed tail is ignored.
        (Gesture::Ctrl, _) | (Gesture::Alt, false) => zoom(i.raw.y + i.raw.x),
        // A sideways scroll (⇧, or a trackpad's x) still scrolls.
        (Gesture::Plain, true) if i.raw.y != 0.0 || i.scroll.y != 0.0 => zoom(i.raw.y),
        _ if !(i.scroll.x.is_finite() && i.scroll.y.is_finite()) || i.scroll == Vec2::ZERO => None,
        _ => Some(Wheel::Pan(i.scroll)),
    }
}

/// Set egui's wheel modifiers (once, at setup): in scroll areas ⌘/Ctrl + wheel scrolls sideways
/// like ⇧ + wheel, and egui never zooms on its own; the canvas zooms with ⌘/Ctrl + wheel ([`read`]).
pub fn configure(ctx: &Context) {
    ctx.options_mut(|o| {
        o.input_options.zoom_modifier = Modifiers::NONE;
        o.input_options.horizontal_scroll_modifier = Modifiers::SHIFT | Modifiers::COMMAND;
    });
}

/// Turn a Windows touchpad pinch, which arrives as Ctrl + wheel in fractions of a notch, into
/// pinch events (`Event::Zoom`) at egui's Ctrl-scroll zoom speed (`notch` points per line,
/// `speed` per point). Ctrl + wheel in whole notches (a mouse wheel) is left to scroll sideways,
/// unless it comes within [`PINCH_GAP`] of a pinch step. `last` is the time of the previous pinch
/// step; returns the time of the latest one.
pub fn legacy_pinch_to_zoom(events: &mut [Event], now: f64, last: Option<f64>, notch: f32, speed: f32) -> Option<f64> {
    let ctrl_wheel = |e: &Event| match e {
        Event::MouseWheel { unit: MouseWheelUnit::Line, delta, modifiers, .. } if modifiers.ctrl => Some(*delta),
        _ => None,
    };
    let fraction = |d: Vec2| d.x.fract() != 0.0 || d.y.fract() != 0.0;
    let pinching = last.is_some_and(|t| (0.0..PINCH_GAP).contains(&(now - t))) || events.iter().filter_map(ctrl_wheel).any(fraction);
    if !pinching {
        return last;
    }
    let mut latest = last;
    for e in events.iter_mut() {
        if let Some(d) = ctrl_wheel(e) {
            let f = (speed * notch * (d.x + d.y)).exp();
            *e = Event::Zoom(if f.is_finite() && f > 0.0 { f } else { 1.0 });
            latest = Some(now);
        }
    }
    latest
}

fn pinch_id() -> Id {
    Id::unique("pc-wheel-legacy-pinch")
}

/// [`legacy_pinch_to_zoom`] for one frame's raw input (`raw_input_hook`, on Windows).
pub fn fold_legacy_pinch(ctx: &Context, raw: &mut RawInput) {
    let (notch, speed) = ctx.options(|o| (o.input_options.line_scroll_speed, o.input_options.scroll_zoom_speed));
    let now = raw.time.unwrap_or_else(|| ctx.input(|i| i.time));
    let last = ctx.data(|d| d.get_temp::<f64>(pinch_id()));
    let latest = legacy_pinch_to_zoom(&mut raw.events, now, last, notch, speed);
    if latest != last
        && let Some(t) = latest
    {
        ctx.data_mut(|d| d.insert_temp(pinch_id(), t));
    }
}

fn gesture_id() -> Id {
    Id::unique("pc-wheel-gesture")
}

/// Real Ctrl + wheel events that eframe's web runner took for a pinch and turned into
/// `Event::Zoom` (or passed through as Ctrl + wheel), put back as the wheel events they were.
/// `real` are those events, recorded by the web shell before eframe saw them, in order: each one
/// replaces the next `Event::Zoom` or Ctrl + wheel event eframe made of it; any eframe never
/// delivered are appended.
pub fn restore_ctrl_wheel(events: &mut Vec<Event>, real: Vec<Event>) {
    let mut real = real.into_iter();
    for e in events.iter_mut() {
        let made_of_ctrl_wheel = match e {
            Event::Zoom(_) => true,
            Event::MouseWheel { modifiers, .. } => modifiers.ctrl,
            _ => false,
        };
        if made_of_ctrl_wheel {
            let Some(r) = real.next() else { return };
            *e = r;
        }
    }
    events.extend(real);
}

/// Read this frame's wheel input. Call it every frame (hovered or not) so the modifier state of
/// the gesture follows the latest wheel event.
pub fn read(ctx: &Context, zoom_with_wheel: bool, pinch_zooms: bool) -> Option<Wheel> {
    let (scroll, zoom_delta, latest, wheel, page) = ctx.input(|i| {
        let wheel: Vec<(MouseWheelUnit, Vec2, Modifiers)> = i
            .events
            .iter()
            .filter_map(|e| match e {
                Event::MouseWheel { unit, delta, modifiers, .. } => Some((*unit, *delta, *modifiers)),
                _ => None,
            })
            .collect();
        let latest = wheel.last().map(|(_, _, m)| Gesture::of(*m));
        (i.smooth_scroll_delta, i.zoom_delta(), latest, wheel, i.content_rect().height())
    });
    let gesture = match latest {
        Some(g) => {
            ctx.data_mut(|d| d.insert_temp(gesture_id(), g));
            g
        }
        None => ctx.data(|d| d.get_temp::<Gesture>(gesture_id())).unwrap_or_default(),
    };
    // The wheel events that belong to the gesture in progress (a plain one: unmodified, as
    // ⇧ + wheel scrolls sideways and never zooms).
    let raw = wheel
        .iter()
        .filter(|(_, _, m)| Gesture::of(*m) == gesture && !(gesture == Gesture::Plain && m.shift))
        .fold(Vec2::ZERO, |sum, (unit, delta, _)| sum + wheel_px(*unit, *delta, page));
    classify(Input { scroll, raw, zoom_delta, gesture, zoom_with_wheel, pinch_zooms })
}

/// [`read`] for a view that is `hovered` (the wheel does nothing elsewhere, but the gesture state
/// still follows it). With Enable Gestures off (`touch_gestures`), two fingers on a touch screen
/// don't zoom. The zoom is continuous, so the animation preferences and the `owner` of a snap
/// residual have nothing to change here.
#[allow(clippy::too_many_arguments)]
pub fn read_with(
    ctx: &Context,
    zoom_with_wheel: bool,
    pinch_zooms: bool,
    _animated_zoom: bool,
    _owner: Id,
    hovered: bool,
    _animations: bool,
    touch_gestures: bool,
) -> Option<Wheel> {
    let w = read(ctx, zoom_with_wheel, pinch_zooms);
    if !hovered {
        return None;
    }
    let touch = ctx.input(|i| i.multi_touch().is_some());
    if touch && !touch_gestures && matches!(w, Some(Wheel::Zoom(_))) {
        return None;
    }
    w
}

/// Drops what is left of a zoom gesture's easing. The continuous zoom has none.
pub fn settle(_ctx: &Context, _owner: Id) {}

#[cfg(test)]
mod tests {
    use egui::TouchPhase;

    use super::*;

    fn input(scroll: Vec2, gesture: Gesture) -> Input {
        Input { scroll, raw: scroll, zoom_delta: 1.0, gesture, zoom_with_wheel: false, pinch_zooms: true }
    }

    fn wheel(delta: Vec2, modifiers: Modifiers) -> Event {
        Event::MouseWheel { source: egui::MouseWheelSource::Unknown, unit: MouseWheelUnit::Line, delta, phase: TouchPhase::Move, modifiers }
    }

    fn wheel_in(unit: MouseWheelUnit, delta: Vec2, modifiers: Modifiers) -> Event {
        Event::MouseWheel { source: egui::MouseWheelSource::Unknown, unit, delta, phase: TouchPhase::Move, modifiers }
    }

    const CTRL: Modifiers = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
    const CMD: Modifiers = Modifiers { mac_cmd: true, command: true, ..Modifiers::NONE };
    /// One notch in wheel lines, on this target.
    const LINE_NOTCH: f32 = LINES_PER_NOTCH;

    /// Feed `events` through an egui context set up like the app's (`legacy`: the Windows pinch
    /// fold on) and add up what the wheel did to the view over the following second.
    fn gesture(frames: Vec<Vec<Event>>, zoom_with_wheel: bool, legacy: bool) -> (Vec2, f32) {
        let ctx = Context::default();
        configure(&ctx);
        let (mut pan, mut zoom) = (Vec2::ZERO, 1.0);
        let mut frames = frames.into_iter();
        for n in 0..60 {
            let mut raw =
                RawInput { time: Some(f64::from(n) / 60.0), predicted_dt: 1.0 / 60.0, events: frames.next().unwrap_or_default(), ..Default::default() };
            if legacy {
                fold_legacy_pinch(&ctx, &mut raw);
            }
            let mut out = ctx.run_ui(raw, |ui| match read(ui.ctx(), zoom_with_wheel, true) {
                Some(Wheel::Pan(d)) => pan += d,
                Some(Wheel::Zoom(f)) => zoom *= f,
                Some(Wheel::SnapZoom(_)) | None => {}
            });
            out.textures_delta.clear();
        }
        (pan, zoom)
    }

    fn near(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 0.01
    }

    fn zoom_of(i: Input) -> f32 {
        match classify(i) {
            Some(Wheel::Zoom(f)) => f,
            other => panic!("expected a zoom from {i:?}, got {other:?}"),
        }
    }

    #[test]
    fn plain_scroll_pans() {
        assert_eq!(classify(input(Vec2::new(3.0, -40.0), Gesture::Plain)), Some(Wheel::Pan(Vec2::new(3.0, -40.0))));
        assert_eq!(classify(input(Vec2::ZERO, Gesture::Plain)), None);
    }

    /// One mouse notch (100 wheel px) is ×1.12 in and ÷1.12 out, for ⌘/Ctrl and ⌥ alike.
    #[test]
    fn one_notch_zooms_twelve_percent() {
        for g in [Gesture::Ctrl, Gesture::Alt] {
            let f = zoom_of(input(Vec2::new(0.0, NOTCH_PX), g));
            assert!((f - 1.12).abs() < 1e-5, "{g:?}: {f}");
            let f = zoom_of(input(Vec2::new(0.0, -NOTCH_PX), g));
            assert!((f - 1.0 / 1.12).abs() < 1e-5, "{g:?}: {f}");
            // Two notches in one frame: ×1.12².
            let f = zoom_of(input(Vec2::new(0.0, 2.0 * NOTCH_PX), g));
            assert!((f - 1.12f32.powi(2)).abs() < 1e-4, "{g:?}: {f}");
        }
    }

    /// A trackpad or a high-resolution wheel sends a notch as many small deltas: they add up to
    /// the same ×1.12, with every step a small one.
    #[test]
    fn small_deltas_add_up_to_one_notch() {
        let mut total = 1.0f32;
        for _ in 0..50 {
            let f = zoom_of(input(Vec2::new(0.0, NOTCH_PX / 50.0), Gesture::Ctrl));
            assert!(f > 1.0 && f < 1.003, "a 2 px step is a tiny zoom: {f}");
            total *= f;
        }
        assert!((total - 1.12).abs() < 1e-3, "{total}");
        // Proportional: half a notch is √1.12.
        let f = zoom_of(input(Vec2::new(0.0, NOTCH_PX / 2.0), Gesture::Alt));
        assert!((f - 1.12f32.sqrt()).abs() < 1e-5, "{f}");
    }

    /// However much the wheel spins in one frame, the zoom changes at most ×2 (or ÷2).
    #[test]
    fn one_frame_is_capped() {
        assert_eq!(zoom_of(input(Vec2::new(0.0, 50.0 * NOTCH_PX), Gesture::Ctrl)), MAX_STEP);
        assert_eq!(zoom_of(input(Vec2::new(0.0, -50.0 * NOTCH_PX), Gesture::Ctrl)), 1.0 / MAX_STEP);
        let pinch = Input { zoom_delta: 40.0, ..input(Vec2::ZERO, Gesture::Plain) };
        assert_eq!(zoom_of(pinch), MAX_STEP);
    }

    #[test]
    fn wheel_units_convert_to_wheel_pixels() {
        assert_eq!(wheel_px(MouseWheelUnit::Point, Vec2::new(0.0, 7.0), 900.0), Vec2::new(0.0, 7.0));
        let notch = wheel_px(MouseWheelUnit::Line, Vec2::new(0.0, LINE_NOTCH), 900.0);
        assert!((notch.y - NOTCH_PX).abs() < 1e-3, "one notch of lines is 100 px: {notch:?}");
        assert_eq!(wheel_px(MouseWheelUnit::Page, Vec2::new(0.0, 1.0), 900.0), Vec2::new(0.0, 900.0));
        assert_eq!(wheel_px(MouseWheelUnit::Page, Vec2::new(0.0, 1.0), f32::NAN), Vec2::new(0.0, NOTCH_PX));
    }

    #[test]
    fn a_notch_zooms_the_frame_it_arrives_and_its_smoothed_tail_does_nothing() {
        // egui keeps feeding a smoothed delta for several frames after the wheel event. Zooming
        // must not follow it (that would zoom twice), nor turn it into a pan.
        for g in [Gesture::Alt, Gesture::Ctrl] {
            let tail = Input { raw: Vec2::ZERO, ..input(Vec2::new(0.0, 12.0), g) };
            assert_eq!(classify(tail), None, "{g:?}");
        }
        let tail = Input { raw: Vec2::ZERO, zoom_with_wheel: true, ..input(Vec2::new(0.0, 12.0), Gesture::Plain) };
        assert_eq!(classify(tail), None);
        // Plain scrolling without the preference still pans from the smoothed delta.
        let pan = Input { raw: Vec2::ZERO, ..input(Vec2::new(0.0, 12.0), Gesture::Plain) };
        assert_eq!(classify(pan), Some(Wheel::Pan(Vec2::new(0.0, 12.0))));
    }

    #[test]
    fn pinch_zooms() {
        let i = Input { zoom_delta: 1.2, ..input(Vec2::ZERO, Gesture::Plain) };
        assert_eq!(classify(i), Some(Wheel::Zoom(1.2)));
        // The pinch wins over a wheel gesture.
        let i = Input { zoom_delta: 0.8, ..input(Vec2::new(0.0, 40.0), Gesture::Alt) };
        assert_eq!(classify(i), Some(Wheel::Zoom(0.8)));
    }

    /// Preferences › Enhanced Controls › Zoom with Trackpad Pinch off: the pinch is ignored, and
    /// a scroll in the same frame pans as it would without a pinch.
    #[test]
    fn the_pinch_preference_switches_pinch_zooming() {
        let i = Input { zoom_delta: 1.2, pinch_zooms: false, ..input(Vec2::ZERO, Gesture::Plain) };
        assert_eq!(classify(i), None);
        let i = Input { zoom_delta: 1.2, pinch_zooms: false, ..input(Vec2::new(0.0, 12.0), Gesture::Plain) };
        assert_eq!(classify(i), Some(Wheel::Pan(Vec2::new(0.0, 12.0))));
        // On (the default) the pinch still zooms.
        let i = Input { zoom_delta: 1.2, pinch_zooms: true, ..input(Vec2::ZERO, Gesture::Plain) };
        assert_eq!(classify(i), Some(Wheel::Zoom(1.2)));
    }

    /// The Windows pinch fold follows the preference too: off, the Ctrl + wheel fractions stay
    /// wheel events.
    #[test]
    fn the_windows_pinch_fold_follows_the_preference() {
        use eframe::App as _;
        if !cfg!(target_os = "windows") {
            return;
        }
        let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let ctx = Context::default();
        let event = || Event::MouseWheel {
            source: egui::MouseWheelSource::Unknown,
            unit: MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 0.25),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers { ctrl: true, command: true, ..Modifiers::NONE },
        };
        let mut raw = RawInput { time: Some(1.0), events: vec![event()], ..Default::default() };
        app.raw_input_hook(&ctx, &mut raw);
        assert!(matches!(raw.events[0], Event::Zoom(_)), "the fraction is a pinch: {:?}", raw.events[0]);
        app.run("prefs.set", serde_json::json!({"path": "enhancedControls.zoomWithTrackpadPinch", "value": false})).unwrap();
        let mut raw = RawInput { time: Some(2.0), events: vec![event()], ..Default::default() };
        app.raw_input_hook(&ctx, &mut raw);
        assert!(matches!(raw.events[0], Event::MouseWheel { .. }), "off: it stays a wheel event: {:?}", raw.events[0]);
    }

    #[test]
    fn zoom_with_scroll_wheel_preference_swaps_wheel_and_alt_wheel() {
        let pref = |scroll, g| Input { zoom_with_wheel: true, ..input(scroll, g) };
        let f = zoom_of(pref(Vec2::new(0.0, NOTCH_PX), Gesture::Plain));
        assert!((f - 1.12).abs() < 1e-5, "{f}");
        assert_eq!(classify(pref(Vec2::new(0.0, 40.0), Gesture::Alt)), Some(Wheel::Pan(Vec2::new(0.0, 40.0))), "⌥ + wheel scrolls");
        assert_eq!(classify(pref(Vec2::new(40.0, 0.0), Gesture::Plain)), Some(Wheel::Pan(Vec2::new(40.0, 0.0))), "sideways still scrolls");
        let f = zoom_of(pref(Vec2::new(0.0, NOTCH_PX), Gesture::Ctrl));
        assert!((f - 1.12).abs() < 1e-5, "⌘/Ctrl + wheel zooms either way: {f}");
    }

    #[test]
    fn each_modifier_maps() {
        let notch = Vec2::new(0.0, LINE_NOTCH);
        let one = |m: Modifiers| gesture(vec![vec![wheel(notch, m)]], false, false);
        let (pan, zoom) = one(Modifiers::NONE);
        assert!(near(pan, Vec2::new(0.0, 40.0 * LINE_NOTCH)) && zoom == 1.0, "wheel scrolls vertically: {pan:?} {zoom}");
        let (pan, zoom) = one(Modifiers::SHIFT);
        assert!(near(pan, Vec2::new(40.0 * LINE_NOTCH, 0.0)) && zoom == 1.0, "⇧ + wheel scrolls sideways: {pan:?} {zoom}");
        for m in [CTRL, CMD, Modifiers::CTRL, Modifiers::ALT, CTRL | Modifiers::SHIFT] {
            let (pan, zoom) = one(m);
            assert!(pan == Vec2::ZERO && (zoom - 1.12).abs() < 1e-4, "{m:?} + wheel zooms 12%, at once, and never pans: {pan:?} {zoom}");
        }
        // Zoom with Scroll Wheel on: the plain wheel zooms, ⌥ scrolls, ⌘/Ctrl still zooms.
        let (pan, zoom) = gesture(vec![vec![wheel(notch, Modifiers::ALT)]], true, false);
        assert!(near(pan, Vec2::new(0.0, 40.0 * LINE_NOTCH)) && zoom == 1.0, "{pan:?} {zoom}");
        let (pan, zoom) = gesture(vec![vec![wheel(notch, Modifiers::NONE)]], true, false);
        assert!(pan == Vec2::ZERO && (zoom - 1.12).abs() < 1e-4, "{pan:?} {zoom}");
        let (pan, zoom) = gesture(vec![vec![wheel(notch, CTRL)]], true, false);
        assert!(pan == Vec2::ZERO && (zoom - 1.12).abs() < 1e-4, "{pan:?} {zoom}");
    }

    /// A browser's pixel wheel: Chrome's 100 px notch is ×1.12; a trackpad's run of 4 px deltas
    /// over many frames zooms by the same amount for the same distance, a little each frame.
    #[test]
    fn pixel_wheel_deltas_zoom_smoothly() {
        let (pan, zoom) = gesture(vec![vec![wheel_in(MouseWheelUnit::Point, Vec2::new(0.0, 100.0), CTRL)]], false, false);
        assert!(pan == Vec2::ZERO && (zoom - 1.12).abs() < 1e-4, "{pan:?} {zoom}");
        let frames: Vec<Vec<Event>> = (0..25).map(|_| vec![wheel_in(MouseWheelUnit::Point, Vec2::new(0.0, -4.0), CTRL)]).collect();
        let (pan, zoom) = gesture(frames, false, false);
        assert!(pan == Vec2::ZERO && (zoom - 1.0 / 1.12).abs() < 1e-4, "{pan:?} {zoom}");
    }

    #[test]
    fn pinch_zooms_and_ctrl_wheel_zooms() {
        // macOS trackpad and browsers: the pinch is its own event.
        let (pan, zoom) = gesture(vec![vec![Event::Zoom(1.1)], vec![Event::Zoom(1.1)]], false, false);
        assert!(pan == Vec2::ZERO && (zoom - 1.21).abs() < 1e-4, "{pan:?} {zoom}");
        // Windows: a touchpad pinch is Ctrl + wheel in fractions of a notch.
        let (pan, zoom) = gesture(vec![vec![wheel(Vec2::new(0.0, 0.25), CTRL)], vec![wheel(Vec2::new(0.0, 0.5), CTRL)]], false, true);
        let speed = egui::InputOptions::default().scroll_zoom_speed;
        assert!(pan == Vec2::ZERO && (zoom - (speed * 40.0 * 0.75).exp()).abs() < 1e-4, "the pinch zooms: {pan:?} {zoom}");
        // A whole mouse notch with Ctrl zooms ×1.12 (the fold leaves it a wheel event).
        let (pan, zoom) = gesture(vec![vec![wheel(Vec2::new(0.0, 1.0), CTRL)]], false, true);
        assert!(pan == Vec2::ZERO && (zoom - NOTCH.powf(1.0 / LINE_NOTCH)).abs() < 1e-4, "Ctrl + mouse wheel zooms: {pan:?} {zoom}");
    }

    /// eframe's web runner turns a Ctrl + wheel it takes for a pinch into `Event::Zoom` (×2.7 for
    /// one Chrome notch): the real events, recorded by the web shell, replace what it made.
    #[test]
    fn restored_ctrl_wheel_replaces_eframes_pinch_guess() {
        let notch = wheel_in(MouseWheelUnit::Point, Vec2::new(0.0, 100.0), CTRL);
        let key = Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
        let mut events = vec![key.clone(), Event::Zoom(1f32.exp()), wheel(Vec2::new(0.0, 1.0), Modifiers::NONE), Event::Zoom(1f32.exp())];
        restore_ctrl_wheel(&mut events, vec![notch.clone(), notch.clone()]);
        assert_eq!(events, vec![key.clone(), notch.clone(), wheel(Vec2::new(0.0, 1.0), Modifiers::NONE), notch.clone()]);
        // eframe passed it through as Ctrl + wheel: replaced one for one.
        let mut events = vec![notch.clone()];
        restore_ctrl_wheel(&mut events, vec![notch.clone()]);
        assert_eq!(events, vec![notch.clone()]);
        // No recorded events: a real pinch stays a pinch.
        let mut events = vec![Event::Zoom(1.05)];
        restore_ctrl_wheel(&mut events, Vec::new());
        assert_eq!(events, vec![Event::Zoom(1.05)]);
        // A recorded event eframe never delivered is appended.
        let mut events = vec![key.clone()];
        restore_ctrl_wheel(&mut events, vec![notch.clone()]);
        assert_eq!(events, vec![key, notch.clone()]);
        // End to end: the restored notch zooms ×1.12.
        let mut frame = vec![Event::Zoom(1f32.exp())];
        restore_ctrl_wheel(&mut frame, vec![notch]);
        let (_, zoom) = gesture(vec![frame], false, false);
        assert!((zoom - 1.12).abs() < 1e-4, "{zoom}");
    }

    #[test]
    fn a_whole_notch_inside_a_pinch_stays_a_pinch() {
        let mut ev = [wheel(Vec2::new(0.0, 0.5), CTRL)];
        let t = legacy_pinch_to_zoom(&mut ev, 1.0, None, 40.0, 0.005);
        assert_eq!(t, Some(1.0));
        assert!(matches!(ev[0], Event::Zoom(f) if (f - 0.1f32.exp()).abs() < 1e-5), "{ev:?}");
        // The next step lands on a whole notch: still the pinch.
        let mut ev = [wheel(Vec2::new(0.0, -1.0), CTRL)];
        assert_eq!(legacy_pinch_to_zoom(&mut ev, 1.1, t, 40.0, 0.005), Some(1.1));
        assert!(matches!(ev[0], Event::Zoom(f) if f < 1.0), "{ev:?}");
        // Long after: a mouse wheel notch, untouched.
        let mut ev = [wheel(Vec2::new(0.0, -1.0), CTRL)];
        assert_eq!(legacy_pinch_to_zoom(&mut ev, 2.0, Some(1.1), 40.0, 0.005), Some(1.1));
        assert!(matches!(ev[0], Event::MouseWheel { .. }));
        // Without Ctrl nothing is a pinch.
        let mut ev = [wheel(Vec2::new(0.0, 0.5), Modifiers::NONE)];
        assert_eq!(legacy_pinch_to_zoom(&mut ev, 2.0, None, 40.0, 0.005), None);
        assert!(matches!(ev[0], Event::MouseWheel { .. }));
    }

    #[test]
    fn hostile_numbers_never_produce_a_bad_zoom() {
        for s in [f32::NAN, f32::INFINITY, -f32::INFINITY, 1e30, -1e30] {
            for (g, zoom_with_wheel) in [(Gesture::Plain, true), (Gesture::Alt, false), (Gesture::Ctrl, false)] {
                for zd in [f32::NAN, 0.0, -1.0, f32::INFINITY, 1.0, 1e30] {
                    let i = Input { scroll: Vec2::new(0.0, s), raw: Vec2::new(0.0, s), zoom_delta: zd, gesture: g, zoom_with_wheel, pinch_zooms: true };
                    if let Some(Wheel::Zoom(f)) = classify(i) {
                        assert!(f.is_finite() && (1.0 / MAX_STEP..=MAX_STEP).contains(&f), "{f} from {i:?}");
                    }
                }
            }
            for notch in [0.0, -1.0, f32::NAN, 40.0] {
                let mut ev = [wheel(Vec2::new(s, 0.5), CTRL)];
                let _ = legacy_pinch_to_zoom(&mut ev, f64::NAN, Some(f64::NAN), notch, 0.005);
                let _ = legacy_pinch_to_zoom(&mut ev, 0.0, None, notch, f32::NAN);
                if let Event::Zoom(f) = ev[0] {
                    assert!(f.is_finite() && f > 0.0, "{f}");
                }
            }
            assert!(wheel_factor(s).is_none_or(|f| f.is_finite() && f > 0.0));
        }
    }
}
