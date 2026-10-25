//! Touch input routing (phones, tablets): what each finger on the glass is for, decided before the
//! frame runs (`raw_input_hook`), so every tool sees ordinary pointer input.
//!
//! Browsers and winit turn the first finger down into the pointer themselves. That finger may be
//! the one holding the touch modifier button, a palm next to a pen, or the first of a two-finger
//! tap, so [`TouchInput::route`] takes those emulated pointer events out and makes its own from
//! the finger it picked for the tool:
//!
//! - **Tool finger**: the first finger down (when fingers paint, see [`StylusMode`]) drives the
//!   pointer. A second finger stops its moves; the canvas drops the stroke (`touch_nav`) and the
//!   press ends when the last finger lifts, so nothing is committed.
//! - **Modifier finger**: a finger that lands on the touch modifier button (Photoshop on the
//!   iPad's touch shortcut) never reaches egui. Held, it is Shift; slid out of the button, Alt.
//!   Held still on its own for [`MOVE_HOLD_S`], it drags the button somewhere else.
//! - **Navigation fingers**: the second and later fingers, and every finger once a pen is in use
//!   (Auto) or always (Pen only), reach egui as touches only: two of them pinch (`touch_nav`), one
//!   alone pans ([`TouchInput::take_pan`]).
//! - **Taps**: two fingers tapped together undo, three redo (held still, each repeats), four hide
//!   the interface. They never fire once the tool finger has drawn.

use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, TouchId, TouchPhase, Vec2, pos2, vec2};

/// Which contacts paint (Preferences › Enhanced Controls › Stylus).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StylusMode {
    /// Fingers paint until a pen is seen; from then on only the pen paints and a finger navigates.
    #[default]
    Auto,
    /// Fingers always paint (a pen too).
    FingerPaints,
    /// Only a pen paints; fingers navigate.
    PenOnly,
}

impl StylusMode {
    pub fn from_pref(s: photocraft_engine::prefs::StylusInput) -> Self {
        use photocraft_engine::prefs::StylusInput as S;
        match s {
            S::Auto => Self::Auto,
            S::FingerPaints => Self::FingerPaints,
            S::PenOnly => Self::PenOnly,
        }
    }
}

/// How the tool finger drives the pointer for the precision tools (an offset cursor, so the
/// finger doesn't hide what it points at).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Precision {
    /// The pointer is under the finger.
    #[default]
    Direct,
    /// The pointer sits [`OFFSET`] above the finger and presses with it (Lasso, Clone Stamp, the
    /// healing brushes).
    Drag,
    /// The pointer sits above the finger and only hovers; lifting the finger clicks there
    /// (Eyedropper, Magic Wand): aim, then let go.
    Aim,
}

/// Where the offset cursor sits relative to the finger (points).
pub const OFFSET: Vec2 = vec2(0.0, -64.0);

/// A gesture the frame should act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchAction {
    Undo,
    Redo,
    /// Four fingers: hide the interface (canvas only), or bring it back.
    ToggleUi,
}

/// Radius of the modifier button (points): 56 pt across, past the 44 pt touch target.
pub const BUTTON_RADIUS: f32 = 28.0;
/// A modifier finger this far from the button's centre (points) is Alt, not Shift.
pub const ALT_RADIUS: f32 = 52.0;
/// How long the modifier finger rests alone before it moves the button.
pub const MOVE_HOLD_S: f64 = 0.5;
/// A multi-finger tap: every finger down and up within this time, none moving past [`TAP_SLOP`].
pub const TAP_S: f64 = 0.35;
pub const TAP_SLOP: f32 = 16.0;
/// The later fingers of a tap land within this time of the first.
const TAP_LAND_S: f64 = 0.25;
/// How long a finger's press waits for a second finger (a gesture) before the tool gets it.
pub const PRESS_WAIT_S: f64 = 0.1;
/// Two or three fingers held still: undo or redo repeats after this, then every [`REPEAT_S`].
pub const REPEAT_AFTER_S: f64 = 0.7;
/// Fingers held for the repeat stay within this (points): the start of a pinch or a twist moves
/// more.
pub const HOLD_SLOP: f32 = 6.0;
pub const REPEAT_S: f64 = 0.16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Tool,
    Modifier,
    Nav,
}

#[derive(Clone, Copy, Debug)]
struct Finger {
    id: TouchId,
    role: Role,
    start: Pos2,
    pos: Pos2,
    start_time: f64,
    pen: bool,
}

/// The fingers of one touch (from the first down to the last up), for the multi-finger taps.
#[derive(Clone, Copy, Debug, Default)]
struct Tap {
    active: bool,
    start: f64,
    most: usize,
    /// Every later finger landed within [`TAP_LAND_S`] and none moved past [`TAP_SLOP`].
    still: bool,
    /// No finger moved past [`HOLD_SLOP`]: held, the fingers repeat undo or redo (a pinch or a
    /// twist, however slow, never does).
    resting: bool,
    /// Repeats fired while held (the release then fires nothing).
    repeats: u32,
    next_repeat: f64,
}

/// The modifier button's state, for drawing it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ButtonState {
    /// Its centre (screen points); `None` until placed (the shell puts it bottom left).
    pub center: Option<Pos2>,
    pub held: bool,
    pub alt: bool,
    /// Being dragged to a new place.
    pub moving: bool,
}

/// See the module docs. One per app window.
#[derive(Clone, Debug, Default)]
pub struct TouchInput {
    fingers: Vec<Finger>,
    /// The tool finger is down as the pointer (a press was sent and no release yet).
    pointer_down: bool,
    /// The pointer stops following the tool finger (a second finger landed).
    frozen: bool,
    /// The tool finger moved past [`TAP_SLOP`]: it drew, so no tap gesture fires.
    drew: bool,
    tap: Tap,
    pub button: ButtonState,
    /// The button is shown and works (touch UI on, Preferences › Enhanced Controls).
    pub button_enabled: bool,
    pub mode: StylusMode,
    /// The active tool's offset cursor (set each frame by the touch interface).
    pub precision: Precision,
    /// The offset cursor's point while a precision finger is down (screen points), for the loupe.
    pub cursor: Option<Pos2>,
    /// Multi-finger taps (Enhanced Controls › Enable Gestures).
    pub gestures: bool,
    /// A pen has been seen this session (Auto mode then leaves fingers to navigation).
    pub pen_seen: bool,
    /// Touch contacts known to be a pen (the web shell's Apple Pencil `touchType`).
    pub pen_ids: Vec<TouchId>,
    /// A finger touched the screen this session: the touch interface applies (tablets too).
    pub touch_seen: bool,
    /// Last time (`RawInput::time`) a touch arrived.
    pub last_touch: f64,
    pan: Vec2,
    actions: Vec<TouchAction>,
    modifiers: Modifiers,
    /// The modifiers last told to egui (`Event::ModifiersChanged`).
    sent_modifiers: Modifiers,
    /// The modifier finger's start, for its drag.
    mod_start: Option<(Pos2, f64)>,
    other_since_mod: bool,
    /// The last finger's role and why (`ui.inspect` › `touch`, for diagnosing devices).
    pub last_start: String,
    /// The finger starting now landed on the canvas (not on the interface over it).
    landed_on_canvas: bool,
    /// The tool finger's precision, decided where it landed.
    finger_precision: Precision,
    /// The tool finger's press, waiting [`PRESS_WAIT_S`] (where, since when).
    pending: Option<(Pos2, f64)>,
    /// The tool finger aims (Precision::Aim): no press until it lifts.
    aiming: bool,
}

impl TouchInput {
    /// How many contacts the router holds (diagnostics).
    pub fn finger_count(&self) -> usize {
        self.fingers.len()
    }

    /// Fingers paint right now (not navigation only).
    pub fn fingers_paint(&self) -> bool {
        match self.mode {
            StylusMode::FingerPaints => true,
            StylusMode::PenOnly => false,
            StylusMode::Auto => !self.pen_seen,
        }
    }

    /// The modifiers the button holds (Shift, or Alt slid out), added to every event this frame.
    pub fn held_modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// One finger's pan since the last call (screen points), when fingers navigate.
    pub fn take_pan(&mut self) -> Vec2 {
        std::mem::take(&mut self.pan)
    }

    pub fn take_actions(&mut self) -> Vec<TouchAction> {
        std::mem::take(&mut self.actions)
    }

    /// Fingers down that egui sees (not the modifier's).
    fn nav_count(&self) -> usize {
        self.fingers.iter().filter(|f| f.role != Role::Modifier && !f.pen).count()
    }

    fn on_button(&self, p: Pos2) -> bool {
        self.button_enabled && self.button.center.is_some_and(|c| (p - c).length() <= BUTTON_RADIUS + 6.0)
    }

    /// Route this frame's touches (see the module docs). `pen_down`: a pen is on the glass now
    /// (the stylus feed).
    pub fn route(&mut self, raw: &mut RawInput, pen_down: bool) {
        self.route_with(raw, pen_down, &|_| true);
    }

    /// [`Self::route`], where `on_canvas` says whether a point is on the canvas (not on a bar,
    /// sheet or button over it): only a finger that lands there gets the offset cursor.
    pub fn route_with(&mut self, raw: &mut RawInput, pen_down: bool, on_canvas: &dyn Fn(Pos2) -> bool) {
        let now = raw.time.unwrap_or(self.last_touch);
        if pen_down {
            self.pen_seen = true;
        }
        let has_touch = raw.events.iter().any(|e| matches!(e, Event::Touch { .. }));
        if !has_touch && self.fingers.is_empty() {
            self.modifiers = Modifiers::NONE;
            if self.sent_modifiers.any() {
                raw.events.insert(0, Event::ModifiersChanged(Modifiers::NONE));
                self.sent_modifiers = Modifiers::NONE;
            }
            return;
        }
        self.touch_seen = true;
        self.last_touch = now;
        let known: Vec<Pos2> = self.fingers.iter().map(|f| f.pos).collect();
        let derived = touch_derived(&raw.events, &known);
        let events = std::mem::take(&mut raw.events);
        let mut out = Vec::with_capacity(events.len() + 4);
        for (i, e) in events.into_iter().enumerate() {
            if derived.get(i).copied().unwrap_or(false) {
                continue;
            }
            match e {
                Event::Touch { device_id, id, phase, pos, force } => {
                    if phase == TouchPhase::Start {
                        let canvas = on_canvas(pos);
                        self.landed_on_canvas = canvas;
                        if !self.fingers.iter().any(|f| f.role == Role::Tool) {
                            self.finger_precision = if canvas { self.precision } else { Precision::Direct };
                        }
                    }
                    let keep = self.touch(&mut out, id, phase, pos, force, now);
                    if keep {
                        out.push(Event::Touch { device_id, id, phase, pos, force });
                    }
                }
                other => out.push(other),
            }
        }
        self.tick(&mut out, now);
        // The held modifiers on everything this frame (tools read `i.modifiers` and the press's).
        let m = self.modifiers;
        if m.any() {
            for e in &mut out {
                if let Event::PointerButton { modifiers, .. } = e {
                    *modifiers |= m;
                }
            }
        }
        if m != self.sent_modifiers {
            out.insert(0, Event::ModifiersChanged(m));
            self.sent_modifiers = m;
        }
        raw.events = out;
    }

    /// One touch event; returns whether egui gets it.
    fn touch(&mut self, out: &mut Vec<Event>, id: TouchId, phase: TouchPhase, pos: Pos2, force: Option<f32>, now: f64) -> bool {
        match phase {
            TouchPhase::Start => {
                if self.fingers.iter().any(|f| f.id == id) {
                    return false;
                }
                // Only contacts the platform names as a pen (an Apple Pencil's touches): fingers
                // report a force too (Android, and Chrome's touch emulation, give 1.0).
                let pen = self.pen_ids.contains(&id);
                if pen {
                    self.pen_seen = true;
                }
                let others = self.nav_count();
                if !pen && self.button.center.is_some() && self.on_button(pos) && !self.fingers.iter().any(|f| f.role == Role::Modifier) {
                    self.fingers.push(Finger { id, role: Role::Modifier, start: pos, pos, start_time: now, pen });
                    self.mod_start = Some((pos, now));
                    self.other_since_mod = others > 0;
                    self.button.held = true;
                    self.update_modifiers(pos);
                    return false;
                }
                if self.fingers.iter().any(|f| f.role == Role::Modifier) {
                    self.other_since_mod = true;
                }
                // A finger on the interface (a bar, a button, a sheet) always taps it, even
                // once fingers only navigate the canvas.
                let paints = pen || self.fingers_paint() || !self.landed_on_canvas;
                let tool_free = !self.fingers.iter().any(|f| f.role == Role::Tool);
                let role = if paints && tool_free && (others == 0 || pen) { Role::Tool } else { Role::Nav };
                if !pen {
                    // Taps: the first finger opens a touch, later ones join it if they land soon.
                    if others == 0 {
                        self.tap = Tap { active: true, start: now, most: 1, still: true, resting: true, repeats: 0, next_repeat: now + REPEAT_AFTER_S };
                        self.drew = false;
                    } else if self.tap.active {
                        if now - self.tap.start > TAP_LAND_S || self.drew {
                            self.tap.still = false;
                        }
                        self.tap.most = self.tap.most.max(others + 1);
                        self.tap.next_repeat = now + REPEAT_AFTER_S;
                    }
                }
                self.fingers.push(Finger { id, role, start: pos, pos, start_time: now, pen });
                self.last_start = format!("{role:?} pen={pen} force={force:?} canvas={}", self.landed_on_canvas);
                match role {
                    Role::Tool => {
                        let p = self.aim(pos, pen);
                        out.push(Event::PointerMoved(p));
                        if pen {
                            out.push(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: self.modifiers });
                            self.pointer_down = true;
                        } else if self.finger_precision != Precision::Aim {
                            // A finger's press waits a moment: a second finger landing now makes
                            // this a gesture, and the tool never hears of it.
                            self.pending = Some((p, now));
                        }
                        self.aiming = !pen && self.finger_precision == Precision::Aim;
                        self.frozen = false;
                    }
                    _ => {
                        // A second finger: the tool finger stops (the canvas drops its stroke),
                        // or, before its press went out, never presses.
                        if !pen && (self.pointer_down || self.pending.is_some() || self.aiming) {
                            self.frozen = true;
                            self.pending = None;
                            self.aiming = false;
                        }
                    }
                }
                // A pen's contact is the pointer only; fingers reach egui for its gestures.
                !pen && role != Role::Modifier
            }
            TouchPhase::Move => {
                let Some(k) = self.fingers.iter().position(|f| f.id == id) else { return false };
                let prev = self.fingers[k].pos;
                self.fingers[k].pos = pos;
                let f = self.fingers[k];
                if !f.pen && ((pos - f.start).length() > HOLD_SLOP || self.pair_turned()) {
                    self.tap.resting = false;
                }
                if !f.pen && (pos - f.start).length() > TAP_SLOP {
                    self.tap.still = false;
                    if f.role == Role::Tool && !self.frozen {
                        self.drew = true;
                    }
                }
                match f.role {
                    Role::Modifier => {
                        if self.button.moving {
                            if let Some(c) = &mut self.button.center {
                                *c += pos - prev;
                            }
                        } else {
                            self.update_modifiers(pos);
                        }
                        false
                    }
                    Role::Tool => {
                        if self.pending.is_some() && (pos - f.start).length() > TAP_SLOP {
                            self.flush(out);
                        }
                        if !self.frozen && self.pending.is_none() {
                            out.push(Event::PointerMoved(self.aim(pos, f.pen)));
                        }
                        !f.pen
                    }
                    Role::Nav => {
                        if !self.fingers_paint() && self.nav_count() == 1 {
                            self.pan += pos - prev;
                        }
                        true
                    }
                }
            }
            TouchPhase::End | TouchPhase::Cancel => {
                let Some(k) = self.fingers.iter().position(|f| f.id == id) else { return false };
                let f = self.fingers.remove(k);
                match f.role {
                    Role::Modifier => {
                        self.button.held = false;
                        self.button.moving = false;
                        self.button.alt = false;
                        self.mod_start = None;
                        self.modifiers = Modifiers::NONE;
                        return false;
                    }
                    Role::Tool => {
                        // A quick still tap clicks under the finger; an aimed press, at the cursor.
                        let quick = now - f.start_time < 0.3 && (pos - f.start).length() <= TAP_SLOP;
                        let p = if self.aiming && quick { pos } else { self.aim(pos, f.pen) };
                        if self.aiming && !self.frozen && self.nav_count() == 0 && phase == TouchPhase::End {
                            // Aim, then let go: the click lands at the cursor.
                            out.push(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: self.modifiers });
                            self.pointer_down = true;
                        }
                        if self.pending.is_some() && !self.frozen {
                            // A tap shorter than the press's wait: the click now.
                            self.flush(out);
                        }
                        self.pending = None;
                        self.aiming = false;
                        self.cursor = None;
                        // The press ends with the last finger, so a stroke a second finger
                        // cancelled stays cancelled (the canvas ignores fingers until all lift).
                        if self.nav_count() == 0 || f.pen {
                            self.release(out, p);
                        }
                    }
                    Role::Nav => {
                        if self.nav_count() == 0 && self.pointer_down {
                            let at = pos;
                            self.release(out, at);
                        }
                    }
                }
                if self.fingers.iter().all(|f| f.role == Role::Modifier) {
                    // Every finger is up: the next touch starts clean.
                    self.frozen = false;
                    self.pending = None;
                    self.aiming = false;
                }
                if !f.pen && self.nav_count() == 0 {
                    if phase == TouchPhase::End {
                        self.end_tap(now);
                    } else {
                        // A cancelled touch (the system took it, or its end was lost) is no tap.
                        self.tap = Tap::default();
                    }
                }
                !f.pen
            }
        }
    }

    /// The platform's list of contacts down now (the browser's `TouchEvent.touches`): a finger the
    /// router still holds but the platform doesn't (its end never arrived) is let go, so a lost
    /// `touchend` can't leave two "held" fingers repeating undo, or the tool finger frozen.
    pub fn sync_active(&mut self, raw: &mut RawInput, active: &[TouchId]) {
        let stale: Vec<TouchId> = self.fingers.iter().map(|f| f.id).filter(|id| !active.contains(id)).collect();
        if stale.is_empty() {
            return;
        }
        let now = raw.time.unwrap_or(self.last_touch);
        for id in stale {
            let pos = self.fingers.iter().find(|f| f.id == id).map_or(Pos2::ZERO, |f| f.pos);
            raw.events.push(Event::Touch { device_id: egui::TouchDeviceId(0), id, phase: TouchPhase::Cancel, pos, force: None });
        }
        let _ = now;
    }

    /// The first two fingers on the glass have pinched or twisted since they landed (their
    /// distance by more than [`HOLD_SLOP`], their angle by more than 3°): no held undo.
    fn pair_turned(&self) -> bool {
        let mut two = self.fingers.iter().filter(|f| f.role != Role::Modifier && !f.pen);
        let (Some(a), Some(b)) = (two.next(), two.next()) else { return false };
        let (d0, d1) = (b.start - a.start, b.pos - a.pos);
        let turn = (d1.angle() - d0.angle()).abs().to_degrees();
        let turn = turn.min(360.0 - turn);
        (d1.length() - d0.length()).abs() > HOLD_SLOP || turn > 3.0
    }

    /// Send the tool finger's waiting press.
    fn flush(&mut self, out: &mut Vec<Event>) {
        if let Some((p, _)) = self.pending.take() {
            out.push(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: self.modifiers });
            self.pointer_down = true;
        }
    }

    /// The pointer's point for the tool finger at `pos`: offset for the precision tools.
    fn aim(&mut self, pos: Pos2, pen: bool) -> Pos2 {
        if pen || self.finger_precision == Precision::Direct {
            self.cursor = None;
            return pos;
        }
        let p = pos + OFFSET;
        self.cursor = Some(p);
        p
    }

    /// Stop the tool finger driving the pointer (a long press took it over): its release then
    /// clicks nothing.
    pub fn take_over(&mut self) {
        if self.fingers.iter().any(|f| f.role == Role::Tool) {
            self.frozen = true;
            self.drew = true;
        }
    }

    /// Where the tool finger is now (screen points), while one is down.
    pub fn tool_finger(&self) -> Option<Pos2> {
        self.fingers.iter().find(|f| f.role == Role::Tool && !f.pen).map(|f| f.pos)
    }

    fn release(&mut self, out: &mut Vec<Event>, pos: Pos2) {
        if self.pointer_down {
            // After a multi-finger touch the release must be no click (a Paint Bucket fill after
            // the two-finger undo): it comes from far off, past egui's click distance.
            let pos = if self.frozen {
                let far = pos + vec2(100_000.0, 100_000.0);
                out.push(Event::PointerMoved(far));
                far
            } else {
                pos
            };
            out.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: self.modifiers });
            out.push(Event::PointerGone);
        }
        self.pointer_down = false;
        self.frozen = false;
    }

    fn update_modifiers(&mut self, pos: Pos2) {
        let Some(c) = self.button.center else { return };
        let alt = (pos - c).length() > ALT_RADIUS;
        self.button.alt = alt;
        self.modifiers = if alt { Modifiers::ALT } else { Modifiers::SHIFT };
    }

    fn end_tap(&mut self, now: f64) {
        let t = std::mem::take(&mut self.tap);
        if !(self.gestures && t.active && t.still && t.repeats == 0 && !self.drew) || now - t.start > TAP_S {
            return;
        }
        if let Some(a) = tap_action(t.most) {
            self.actions.push(a);
        }
    }

    /// Time-driven parts: the finger's waiting press, the modifier button's move hold, and held
    /// undo/redo repeating.
    fn tick(&mut self, out: &mut Vec<Event>, now: f64) {
        if self.pending.is_some_and(|(_, t)| now - t >= PRESS_WAIT_S) {
            self.flush(out);
        }
        if let Some((start, t0)) = self.mod_start
            && !self.button.moving
            && !self.other_since_mod
            && now - t0 >= MOVE_HOLD_S
            && self.fingers.iter().find(|f| f.role == Role::Modifier).is_some_and(|f| (f.pos - start).length() <= TAP_SLOP)
        {
            self.button.moving = true;
            self.modifiers = Modifiers::NONE;
        }
        if self.gestures && self.tap.active && self.tap.still && self.tap.resting && !self.drew && matches!(self.tap.most, 2 | 3) && self.nav_count() == self.tap.most && now >= self.tap.next_repeat {
            if let Some(a) = tap_action(self.tap.most) {
                self.actions.push(a);
            }
            self.tap.repeats += 1;
            self.tap.next_repeat = now + REPEAT_S;
        }
    }

    /// Something the frame must keep drawing for: a held repeat or the move hold is due.
    pub fn wants_repaint(&self) -> bool {
        self.pending.is_some() || self.mod_start.is_some() || (self.tap.active && self.tap.still && self.tap.most >= 2)
    }

    /// Put the button at its default place when it has none, or back on the screen.
    pub fn place_button(&mut self, screen: egui::Rect, default: Pos2) {
        let c = self.button.center.unwrap_or(default);
        let r = BUTTON_RADIUS + 4.0;
        let c = pos2(c.x.clamp(screen.left() + r, screen.right() - r), c.y.clamp(screen.top() + r, screen.bottom() - r));
        self.button.center = Some(c);
    }
}

fn tap_action(fingers: usize) -> Option<TouchAction> {
    match fingers {
        2 => Some(TouchAction::Undo),
        3 => Some(TouchAction::Redo),
        4 => Some(TouchAction::ToggleUi),
        _ => None,
    }
}

/// Which events are a browser's or winit's pointer emulation of a touch: pointer events next to a
/// touch event (only other pointer events between) at the place of a finger: the touch beside it,
/// or another finger down (eframe emulates the first finger down, which may be the modifier's
/// while another finger lands). A mouse's or a pen's own pointer events have no touch beside them.
fn touch_derived(events: &[Event], fingers: &[Pos2]) -> Vec<bool> {
    let pointer_pos = |e: &Event| match e {
        Event::PointerMoved(p) => Some(Some(*p)),
        Event::PointerButton { pos, button: PointerButton::Primary, .. } => Some(Some(*pos)),
        Event::PointerGone => Some(None),
        _ => None,
    };
    let touches: Vec<Pos2> = events.iter().filter_map(|e| if let Event::Touch { pos, .. } = e { Some(*pos) } else { None }).collect();
    let at_finger = |p: Pos2| touches.iter().chain(fingers).any(|f| (p - *f).length() <= 2.0);
    let near = |touch: &Event, pos: Option<Pos2>| match touch {
        Event::Touch { phase, .. } => match pos {
            Some(p) => at_finger(p),
            None => matches!(phase, TouchPhase::End | TouchPhase::Cancel),
        },
        _ => false,
    };
    (0..events.len())
        .map(|i| {
            let Some(pos) = pointer_pos(&events[i]) else { return false };
            let before = events[..i].iter().rev().find(|e| pointer_pos(e).is_none());
            let after = events[i + 1..].iter().find(|e| pointer_pos(e).is_none());
            before.is_some_and(|e| near(e, pos)) || after.is_some_and(|e| near(e, pos))
        })
        .collect()
}

/// The button's default centre: bottom left, above the floating bars of the simple interface.
pub fn default_button_center(screen: egui::Rect, bottom_reserve: f32) -> Pos2 {
    pos2(screen.left() + 16.0 + BUTTON_RADIUS, screen.bottom() - bottom_reserve - 20.0 - BUTTON_RADIUS) + vec2(0.0, 0.0)
}

#[cfg(test)]
#[path = "touch_input_tests.rs"]
mod tests;
