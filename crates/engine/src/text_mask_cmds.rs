//! Finding text with the mask model: `select.detectText` runs it over the page and keeps the
//! pieces it found; `select.textPieces` makes a normal selection of the ones a person chose. The
//! model also marks art, so detecting selects nothing.

use std::sync::Arc;

use photocraft_ai_textmask::{Detection, Hooks, TextMasker, default_grow, selection_mask};
use photocraft_algo::selection as sel;
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

pub const DETECT: &str = "select.detectText";
pub const PIECES: &str = "select.textPieces";
pub const STROKE: &str = "select.brushStroke";
pub const ZONE: &str = "select.textZone";
pub const CLEAN: &str = "select.cleanPieces";
pub const STEP: &str = "select.textStep";
const LABEL: &str = "Detect Text";
const UNAVAILABLE: &str = "text detection isn't available in this build";
/// What choosing pieces holds at its most for each pixel of the page: the mask in floats and the
/// flags it's grown from (measured: 7).
const MASK_BYTES_PER_PIXEL: u64 = 8;

impl Session {
    /// Injects (or removes) the service that runs the mask model.
    pub fn set_text_masker(&mut self, masker: Option<Arc<dyn TextMasker>>) {
        if let (Some(m), Some(_)) = (&masker, self.ai_gpu_guard()) {
            m.set_cpu_only(true);
        }
        self.text_masker = masker;
    }

    pub fn text_masker(&self) -> Option<&Arc<dyn TextMasker>> {
        self.text_masker.as_ref()
    }

    /// Replaces what the active document's detection found (what's left of it after a clean).
    pub fn set_text_detection(&mut self, detection: Arc<Detection>) {
        if let Some(id) = self.active().map(|st| st.doc.id) {
            self.text_detections.retain(|(d, _)| *d != id);
            self.text_detections.push((id, detection));
        }
    }

    /// Drops what the last detection on the active document found.
    pub fn forget_text_detection(&mut self) {
        if let Some(id) = self.active().map(|st| st.doc.id) {
            self.text_detections.retain(|(d, _)| *d != id);
        }
    }

    /// What the last detection on the active document found.
    pub fn text_detection(&self) -> Option<Arc<Detection>> {
        let id = self.active()?.doc.id;
        self.text_detections.iter().find(|(d, _)| *d == id).map(|(_, t)| t.clone())
    }
}

fn pieces_json(d: &Detection) -> Vec<Value> {
    d.regions
        .iter()
        .map(|r| json!({"id": r.id, "group": r.group, "box": r.bbox, "area": r.area, "score": r.score, "suggested": r.suggested(), "bubble": r.bubble, "plain": r.plain, "method": auto_method(r)}))
        .collect()
}

/// How a piece is cleaned unless someone says otherwise: in a speech bubble with a plain
/// background, by the Bubble Text Remover; anything else (effects, text over art or screentone,
/// no bubble) with AI.
pub fn auto_method(r: &photocraft_ai_textmask::Region) -> &'static str {
    if r.bubble > 0 && r.plain { "bubble" } else { "ai" }
}

/// The most time finding the bubbles of a page may take after the text is found: past it the
/// pieces not yet looked at count as in no bubble.
const BUBBLES_BUDGET_MS: u128 = 1500;

/// The speech bubble of each word or effect of `d` (its pieces' [`Region::bubble`] and
/// [`Region::plain`]), as the Bubble Text Remover finds a bubble from a click at the word's
/// centre. A bubble found once serves every word inside it. A region counts as a speech bubble
/// only when it's closed, at most [`BUBBLE_MAX_SHARE`] of the page (a panel's white is larger),
/// about as full as an ellipse ([`BUBBLE_MIN_SHAPE`]: paper in the art is ragged), and the words
/// inside it sit about its middle, as lettering does (an outlined letter's own white holds no
/// word). How many bubbles were found.
///
/// [`Region::bubble`]: photocraft_ai_textmask::Region::bubble
/// [`Region::plain`]: photocraft_ai_textmask::Region::plain
pub(crate) fn mark_bubbles(d: &mut Detection, page: &crate::typer_place_cmds::support::PageImage, doc: &Arc<photocraft_doc::Document>) -> usize {
    let started = web_time::Instant::now();
    let mut groups: Vec<(u32, [u32; 4])> = Vec::new();
    for r in &d.regions {
        match groups.iter_mut().find(|(g, _)| *g == r.group) {
            Some((_, b)) => *b = [b[0].min(r.bbox[0]), b[1].min(r.bbox[1]), b[2].max(r.bbox[2]), b[3].max(r.bbox[3])],
            None => groups.push((r.group, r.bbox)),
        }
    }
    let inside = |m: &photocraft_typer::bubble::Mask, b: [u32; 4]| {
        let (cx, cy) = (((b[0] + b[2]) / 2) as i32, ((b[1] + b[3]) / 2) as i32);
        m.get(cx, cy) && m.bounds().is_some_and(|i| i.x0 <= b[0] as i32 && i.y0 <= b[1] as i32 && i.x1 >= b[2] as i32 && i.y1 >= b[3] as i32)
    };
    let page_area = (d.width * d.height).max(1) as f64;
    // Regions looked at: the interior, and the bubble it is (`None`: not a speech bubble).
    let mut seen: Vec<(photocraft_typer::bubble::Mask, Option<(u32, bool)>)> = Vec::new();
    let mut bubbles = 0u32;
    let mut of_group: Vec<(u32, u32, bool)> = Vec::new();
    for &(g, b) in &groups {
        let known = seen.iter().position(|(m, _)| inside(m, b));
        let at = match known {
            Some(k) => Some(k),
            None if started.elapsed().as_millis() < BUBBLES_BUDGET_MS => {
                let (cx, cy) = (((b[0] + b[2]) / 2) as i32, ((b[1] + b[3]) / 2) as i32);
                match crate::bubble_text_cmds::bubble_at(page, doc, cx, cy) {
                    Some(crate::bubble_text_cmds::BubbleAt { interior: m, body, shape, plain }) if inside(&m, b) => {
                        // The words it holds, together: about the middle of its body?
                        let words = groups.iter().filter(|(_, o)| inside(&m, *o)).fold(b, |u, (_, o)| [u[0].min(o[0]), u[1].min(o[1]), u[2].max(o[2]), u[3].max(o[3])]);
                        let (wx, wy) = (f64::from(words[0] + words[2]) / 2.0, f64::from(words[1] + words[3]) / 2.0);
                        let (bx, by) = (f64::from(body.x0 + body.x1) / 2.0, f64::from(body.y0 + body.y1) / 2.0);
                        let centred = (wx - bx).abs() <= BUBBLE_OFF_CENTRE * f64::from(body.width()) && (wy - by).abs() <= BUBBLE_OFF_CENTRE * f64::from(body.height());
                        let small = m.count() as f64 <= BUBBLE_MAX_SHARE * page_area;
                        let round = shape >= BUBBLE_MIN_SHAPE;
                        let bubble = (centred && small && round).then(|| {
                            bubbles += 1;
                            (bubbles, plain)
                        });
                        seen.push((m, bubble));
                        Some(seen.len() - 1)
                    }
                    _ => None,
                }
            }
            None => None,
        };
        if let Some((bubble, plain)) = at.and_then(|k| seen[k].1) {
            of_group.push((g, bubble, plain));
        }
    }
    for r in &mut d.regions {
        if let Some(&(_, bubble, plain)) = of_group.iter().find(|(g, _, _)| *g == r.group) {
            r.bubble = bubble;
            r.plain = plain;
        }
    }
    bubbles as usize
}

/// The most of the page a speech bubble covers: a panel is more.
const BUBBLE_MAX_SHARE: f64 = 0.06;
/// The least a bubble fills of its smallest enclosing rectangle, tail included (an ellipse 0.79, a
/// box 1, bubbles with tails seen at 0.71): white paper in the art, between lines and drawings,
/// is ragged (seen at 0.63).
const BUBBLE_MIN_SHAPE: f64 = 0.68;
/// How far from the middle of a bubble's body its words' middle may be, as a part of its side.
const BUBBLE_OFF_CENTRE: f64 = 0.25;

/// Fails when `bytes` more can't be had now. A buffer that can't be allocated ends the whole
/// app (Rust aborts), so a job's large ones are asked for first, where saying no is still an
/// error to show. Not in the browser: its memory only grows, and one block of this size can be
/// refused where the buffers themselves fit.
pub(crate) fn room_for(bytes: u64) -> Result<()> {
    if cfg!(target_arch = "wasm32") {
        return Ok(());
    }
    let short = || EngineError::Other(format!("not enough memory for this ({} MB more are needed): close other documents or apps and try again", bytes >> 20));
    let mut probe: Vec<u8> = Vec::new();
    probe.try_reserve_exact(usize::try_from(bytes).map_err(|_| short())?).map_err(|_| short())
}

/// The page as the model reads it: what shows, over white.
fn page_rgb(doc: &photocraft_doc::Document) -> Result<(Vec<u8>, usize, usize)> {
    let bounds = doc.bounds();
    // The flattened page in floats, and its bytes.
    room_for(u64::from(bounds.width()) * u64::from(bounds.height()) * 19)?;
    let b = photocraft_compose::flatten(doc);
    let (w, h) = (b.rect.width() as usize, b.rect.height() as usize);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for p in &b.px {
        for c in &p[..3] {
            // `flatten` gives colour over the alpha that shows.
            let v = c * p[3] + (1.0 - p[3]);
            rgb.push((v.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    Ok((rgb, w, h))
}

/// The page found for the bubbles, how many bubbles, in how long (ms).
type Bubbles = (Option<crate::typer_place_cmds::support::PageImage>, usize, u64);

fn detect(s: &mut Session, p: &Value) -> Result<Value> {
    let in_selection = p.get("inSelection").and_then(Value::as_bool).unwrap_or(false);
    let masker = s.text_masker.clone().ok_or_else(|| EngineError::Other(UNAVAILABLE.into()))?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (doc, id) = (st.doc.clone(), st.doc.id);
    let page = doc.clone();
    let guard = s.ai_gpu_guard();
    // The page as the Bubble Text Remover sees it, for the bubbles: composited on the job's
    // thread. Not in the browser, where the answer comes back on the page's thread.
    let bubble_page = if cfg!(target_arch = "wasm32") { None } else { Some(crate::typer_place_cmds::page_work(s)?) };
    // The page's pixels are read off the page's thread: seconds for a big page on a phone.
    crate::jobs::run_async_prepared(
        s,
        LABEL,
        true,
        move |_| {
            let started = web_time::Instant::now();
            let (rgb, width, height) = page_rgb(&page)?;
            Ok(crate::jobs::Prep::Done((rgb, width, height, started)))
        },
        move |ctx, (rgb, width, height, started), done| {
            ctx.progress(0.0, LABEL);
            let read_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let (cancel, step) = (ctx.clone(), ctx.clone());
            let hooks = Hooks { cancelled: Arc::new(move || cancel.cancelled()), progress: Arc::new(move |f| step.progress(f, LABEL)) };
            let service = masker.clone();
            // The model always reads the whole page: a crop would reach it larger than it learned.
            masker.detect_then(
                rgb,
                width,
                height,
                hooks,
                Box::new(move |r| {
                    let mut bubbles = (None, 0, 0);
                    let r = r.map_err(|e| EngineError::Other(e.to_string())).map(|mut d| {
                        if let Some(work) = bubble_page {
                            let t = web_time::Instant::now();
                            let page = work.finish();
                            let n = mark_bubbles(&mut d, &page, &doc);
                            bubbles = (Some(page), n, u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX));
                        }
                        match doc.selection.as_ref().filter(|_| in_selection) {
                            Some(zone) => d.clipped(|x, y| zone.sample_channel(x as i32, y as i32, 0) >= 0.5),
                            None => d,
                        }
                    });
                    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    done.finish(r.map(|d| (Arc::new(d), service.device(), ms, read_ms, service.timing(), bubbles)));
                }),
            );
        },
        move |s, (d, device, ms, read_ms, timing, bubbles): (Arc<Detection>, String, u64, u64, Option<photocraft_ai_textmask::Timing>, Bubbles)| {
            let (page, n_bubbles, bubbles_ms) = bubbles;
            if let Some(page) = page {
                // The Bubble Text Remover's clicks that follow find the page ready.
                crate::typer_place_cmds::install_page(s, page);
            }
            s.text_detections.retain(|(doc, _)| *doc != id);
            let mut out = json!({
                "pieces": pieces_json(&d),
                "model": photocraft_ai::catalog::TEXTMASK_ID,
                "cpu": device == "CPU",
                "device": device,
                "region": [0, 0, d.width, d.height],
                "ms": ms,
                "bubbles": n_bubbles,
                "stages": {"readMs": read_ms, "bubblesMs": bubbles_ms},
            });
            if let Some(g) = guard {
                out["gpuGuard"] = json!(g.id());
            }
            if let Some(t) = timing {
                out["fellBack"] = json!(t.fell_back);
                out["stages"]["loadMs"] = json!(t.load_ms);
                out["stages"]["inferMs"] = json!(t.infer_ms);
                out["stages"]["regionsMs"] = json!(t.regions_ms);
            }
            s.text_detections.push((id, d));
            Ok(out)
        },
    )
}

/// The pieces `p` names in `pieces`.
fn piece_ids(d: &Detection, p: &Value) -> Option<Vec<u32>> {
    match p.get("pieces") {
        Some(Value::String(a)) if a == "all" => Some(d.regions.iter().map(|r| r.id).collect()),
        Some(Value::String(a)) if a == "suggested" => Some(d.regions.iter().filter(|r| r.suggested()).map(|r| r.id).collect()),
        Some(Value::Array(a)) => Some(a.iter().filter_map(Value::as_u64).filter_map(|v| u32::try_from(v).ok()).collect()),
        _ => None,
    }
}

/// The Bubble Text Remover's clicks for the pieces `ids`: one per bubble (pieces in no bubble, one
/// per word), at the centre of what it holds of them, with those pieces.
pub fn bubble_clicks(d: &Detection, ids: &[u32]) -> Vec<([f64; 2], Vec<u32>)> {
    // By bubble (or word): the box of what it holds, and those pieces.
    type Click = ((bool, u32), [u32; 4], Vec<u32>);
    let mut out: Vec<Click> = Vec::new();
    for id in ids {
        let Some(r) = d.regions.get(*id as usize - 1) else { continue };
        let key = if r.bubble > 0 { (true, r.bubble) } else { (false, r.group) };
        match out.iter_mut().find(|(k, _, _)| *k == key) {
            Some((_, b, v)) => {
                *b = [b[0].min(r.bbox[0]), b[1].min(r.bbox[1]), b[2].max(r.bbox[2]), b[3].max(r.bbox[3])];
                v.push(*id);
            }
            None => out.push((key, r.bbox, vec![*id])),
        }
    }
    out.into_iter().map(|(_, b, v)| ([f64::from(b[0] + b[2]) / 2.0, f64::from(b[1] + b[3]) / 2.0], v)).collect()
}

/// A command a clean still has to run ([`Clean`]).
enum Todo {
    /// The history step the rest joins.
    Step,
    /// The Bubble Text Remover's click at `seed` for the pieces `group` of one bubble.
    Bubble { seed: [f64; 2], group: Vec<u32>, grow: usize, new_layer: bool, sample_all: bool },
    /// The pieces a step cleans with `command`, and the bubbles the remover refused before it:
    /// selected, then cleaned.
    Rest { ids: Vec<u32>, grow: usize, command: &'static str, params: Value },
}

/// What the command a clean is waiting for cleans.
enum Ran {
    Bubble { group: Vec<u32>, grow: usize },
    Rest { reach: Vec<bool>, refused: Vec<u32> },
    /// The step the rest joins, or the deselect at the end.
    Other,
}

/// A clean under way (`select.cleanPieces`): the commands it still has to run, one after another
/// ([`crate::jobs::Sequence`]), and what the ones that ran cleaned.
struct Clean {
    doc: photocraft_doc::DocId,
    d: Arc<Detection>,
    /// The history step every command of the clean joins.
    key: String,
    area: Rect,
    plan: std::collections::VecDeque<Todo>,
    /// How many commands the plan had.
    planned: usize,
    running: Option<Ran>,
    /// Bubbles the remover refused, for the step's AI clean.
    refused: Vec<u32>,
    /// The pixels cleaned so far.
    gone: Vec<bool>,
    done: Vec<Value>,
    deselected: bool,
}

impl Clean {
    fn clean(&mut self, reach: &[bool]) {
        for (g, r) in self.gone.iter_mut().zip(reach) {
            *g |= *r;
        }
    }
}

impl crate::jobs::Sequence for Clean {
    fn next(&mut self, s: &mut Session, last: Option<Result<Value>>) -> Result<crate::jobs::Next> {
        use crate::jobs::Next;
        match (self.running.take(), last) {
            (Some(Ran::Bubble { group, grow }), Some(Ok(v))) => {
                self.done.push(v);
                let reach = selection_mask(&self.d, &group, grow);
                self.clean(&reach);
            }
            // Not a plain bubble, or not one: its pieces go to the step's AI clean.
            (Some(Ran::Bubble { group, .. }), _) => self.refused.extend(group),
            (Some(Ran::Rest { reach, refused }), Some(Ok(mut r))) => {
                if !refused.is_empty() {
                    r["bubblesToAi"] = json!(refused);
                }
                self.done.push(r);
                self.clean(&reach);
            }
            (Some(_), Some(Err(e))) => return Err(e),
            _ => {}
        }
        if s.active().map(|st| st.doc.id) != Some(self.doc) {
            return Err(EngineError::Other("the clean stopped: its document isn't the one in front any more".into()));
        }
        while let Some(todo) = self.plan.pop_front() {
            match todo {
                Todo::Step => {
                    self.running = Some(Ran::Other);
                    return Ok(Next::Run(STEP, json!({"label": "Clean Text", "coalesce": self.key})));
                }
                Todo::Bubble { seed, group, grow, new_layer, sample_all } => {
                    self.running = Some(Ran::Bubble { group, grow });
                    return Ok(Next::Run(crate::bubble_text_cmds::ID, json!({"x": seed[0], "y": seed[1], "newLayer": new_layer, "sampleAll": sample_all, "coalesce": self.key})));
                }
                Todo::Rest { mut ids, grow, command, params } => {
                    let refused = std::mem::take(&mut self.refused);
                    ids.extend(refused.iter().copied());
                    if ids.is_empty() {
                        continue;
                    }
                    let reach = selection_mask(&self.d, &ids, grow);
                    let mask: Vec<f32> = reach.iter().map(|v| if *v { 1.0 } else { 0.0 }).collect();
                    s.coalesce_request = Some(self.key.clone());
                    let selected = crate::selection_cmds::set_selection(s, "Text Selection", self.area, mask, sel::SelectionMode::Replace);
                    s.coalesce_request = None;
                    selected?;
                    self.running = Some(Ran::Rest { reach, refused });
                    return Ok(Next::Run(command, params));
                }
            }
        }
        if !std::mem::replace(&mut self.deselected, true) && s.active().is_some_and(|st| st.doc.selection.is_some()) {
            self.running = Some(Ran::Other);
            return Ok(Next::Run("select.deselect", json!({"coalesce": self.key})));
        }
        let (d, gone) = (&self.d, &self.gone);
        let left = d.clipped(|x, y| !gone[y * d.width + x]);
        let pieces = pieces_json(&left);
        if left.regions.is_empty() {
            s.forget_text_detection();
        } else {
            s.set_text_detection(Arc::new(left));
        }
        Ok(Next::Done(json!({"steps": std::mem::take(&mut self.done), "pieces": pieces})))
    }

    fn progress(&self) -> f32 {
        1.0 - self.plan.len() as f32 / self.planned.max(1) as f32
    }
}

/// `select.cleanPieces`: each step cleans its pieces in its own way, as the tool's Clean does. What
/// was cleaned leaves the pieces; the answer has those left, with new ids. The whole clean is one
/// history step, named Clean Text: every command it runs joins it (`coalesce`), so one undo
/// brings the page back (each bubble, selection and fill used to be a step of its own, and an
/// undo after a clean on a new layer took back the last of them and left the layer).
///
/// Started as a job ([`Session::start`]: the app, hosts, the control channel) it is one, as the
/// tool's Clean: this checks the steps and returns, and the bubble clicks and fills run as jobs
/// of their own one after another ([`crate::jobs::run_sequence`]), so the thread that started it
/// is never held (a clean of a page took 1 to 42 s in one frame). Run inline it returns when
/// the clean is done.
fn clean_pieces(s: &mut Session, p: &Value) -> Result<Value> {
    let bad = |m: &str| EngineError::BadParams { cmd: CLEAN.into(), msg: m.into() };
    // Inline, the browser can't wait for the model's answer; as a job it needn't.
    if cfg!(target_arch = "wasm32") && !s.started_as_job() {
        return Err(bad("in the browser, run select.textPieces and the clean of each step and wait for it"));
    }
    let d = s.text_detection().ok_or_else(|| bad("detect the text first (select.detectText)"))?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (doc, revision) = (st.doc.id, st.revision);
    let steps = p.get("steps").and_then(Value::as_array).filter(|a| !a.is_empty()).ok_or_else(|| bad("\"steps\" must be a list of what to clean and how"))?;
    let area = Rect::new(0, 0, i32::try_from(d.width).unwrap_or(i32::MAX), i32::try_from(d.height).unwrap_or(i32::MAX));
    let px = |v: &Value, key: &str| v.get(key).and_then(Value::as_f64).map(|v| v.round().max(0.0) as usize);
    let default = px(p, "grow").unwrap_or_else(|| default_grow(d.width, d.height));
    room_for(MASK_BYTES_PER_PIXEL * (d.width * d.height) as u64)?;
    let key = p.get("coalesce").and_then(Value::as_str).map_or_else(|| format!("{CLEAN}.{revision}"), str::to_string);
    let mut plan = std::collections::VecDeque::new();
    let mut taken = std::collections::HashSet::new();
    for step in steps {
        let ids = piece_ids(&d, step).ok_or_else(|| bad("a step's \"pieces\" must be a list of ids, \"all\" or \"suggested\""))?;
        // "all" and "suggested" mean those no earlier step took; ids are taken as they're given.
        let ids: Vec<u32> = if step["pieces"].is_string() { ids.into_iter().filter(|id| !taken.contains(id)).collect() } else { ids };
        taken.extend(ids.iter().copied());
        if ids.is_empty() {
            continue;
        }
        let grow = px(step, "grow").unwrap_or(default);
        let mut params = json!({"output": step.get("output").or(p.get("output")).cloned().unwrap_or_else(|| json!("current")), "coalesce": key});
        for key in ["layerName", "keepActive", "color", "byBlocks", "sampleAllLayers", "allowDownload"] {
            if let Some(v) = step.get(key).or(p.get(key)) {
                params[key] = v.clone();
            }
        }
        let method = step.get("method").and_then(Value::as_str);
        // "auto": each piece as `auto_method` says; "bubble": the Bubble Text Remover, a click
        // per bubble, and AI for those it refuses.
        let (bubbles, rest): (Vec<u32>, Vec<u32>) = match method {
            Some("bubble") => (ids, Vec::new()),
            Some("auto") => ids.into_iter().partition(|id| d.regions.get(*id as usize - 1).is_some_and(|r| auto_method(r) == "bubble")),
            Some("ai" | "fill" | "color") => (Vec::new(), ids),
            _ => return Err(bad("a step's \"method\" must be \"ai\", \"fill\", \"color\", \"bubble\" or \"auto\"")),
        };
        let new_layer = params["output"].as_str().is_some_and(|o| o != "current");
        let sample_all = params.get("sampleAllLayers").and_then(Value::as_bool).unwrap_or(false);
        for (seed, group) in bubble_clicks(&d, &bubbles) {
            plan.push_back(Todo::Bubble { seed, group, grow, new_layer, sample_all });
        }
        let command = match method {
            Some(m @ ("fill" | "color")) => {
                params["method"] = json!(m);
                crate::ai_inpaint_cmds::quick::ID
            }
            _ => crate::remove_ai_cmds::ID,
        };
        plan.push_back(Todo::Rest { ids: rest, grow, command, params });
    }
    // The step the rest joins, when there is something to clean.
    if !plan.is_empty() {
        plan.push_front(Todo::Step);
    }
    let gone = vec![false; d.width * d.height];
    let clean = Clean { doc, d, key, area, planned: plan.len(), plan, running: None, refused: Vec::new(), gone, done: Vec::new(), deselected: false };
    crate::jobs::run_sequence(s, "Clean Text", clean)
}

/// `select.textStep`: a history step that changes nothing in the document, for what the Text
/// Selection tool keeps of its own (a piece's method, a block split): undo and redo take the
/// tool's state back with the document's (the tool remembers it by the history state's id).
fn step(s: &mut Session, p: &Value) -> Result<Value> {
    let label = p.get("label").and_then(Value::as_str).unwrap_or("Text Selection").chars().take(64).collect::<String>();
    s.edit(&label, |_, _| Ok(()))?;
    Ok(json!({"ok": true}))
}

/// `select.textPieces`: a selection of the chosen pieces.
fn pieces(s: &mut Session, p: &Value) -> Result<Value> {
    let bad = |m: &str| EngineError::BadParams { cmd: PIECES.into(), msg: m.into() };
    let d = s.text_detection().ok_or_else(|| bad("detect the text first (select.detectText)"))?;
    let ids = piece_ids(&d, p).ok_or_else(|| bad("\"pieces\" must be a list of ids, \"all\" or \"suggested\""))?;
    let px = |key: &str| p.get(key).and_then(Value::as_f64).map(|v| v.round().max(0.0) as usize);
    let grow = px("grow").unwrap_or_else(|| default_grow(d.width, d.height));
    let area = Rect::new(0, 0, i32::try_from(d.width).unwrap_or(i32::MAX), i32::try_from(d.height).unwrap_or(i32::MAX));
    let mut mode = sel::SelectionMode::parse(p.get("mode").and_then(Value::as_str).unwrap_or("replace"));
    room_for(MASK_BYTES_PER_PIXEL * (d.width * d.height) as u64)?;
    let mut mask: Vec<f32> = selection_mask(&d, &ids, grow).into_iter().map(|v| if v { 1.0 } else { 0.0 }).collect();
    // Taking a piece out leaves its neighbours' reach whole.
    if let Some(keep) = p.get("keep").and_then(Value::as_array) {
        let keep: Vec<u32> = keep.iter().filter_map(Value::as_u64).filter_map(|v| u32::try_from(v).ok()).collect();
        for (m, k) in mask.iter_mut().zip(selection_mask(&d, &keep, grow)) {
            if k {
                *m = 0.0;
            }
        }
    }
    // A new contour for pieces that have `from`: only their old reach comes out, so what a brush
    // added elsewhere stays.
    if let Some(from) = px("from") {
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let now = sel::mask_from_surface(st.doc.selection.as_ref(), area);
        let old = selection_mask(&d, &ids, from);
        for ((m, n), o) in mask.iter_mut().zip(now).zip(old) {
            *m = m.max(if o { 0.0 } else { n });
        }
        mode = sel::SelectionMode::Replace;
    }
    crate::selection_cmds::set_selection(s, "Text Selection", area, mask, mode)
}

/// `select.brushStroke`: a brush stroke adds to the selection or takes from it.
fn stroke(s: &mut Session, p: &Value) -> Result<Value> {
    let bad = |m: &str| EngineError::BadParams { cmd: STROKE.into(), msg: m.into() };
    let points: Vec<[f64; 2]> = p.get("points").cloned().and_then(|v| serde_json::from_value(v).ok()).ok_or_else(|| bad("\"points\" must be a list of [x, y]"))?;
    let radius = (p.get("size").and_then(Value::as_f64).ok_or_else(|| bad("\"size\" is required"))? / 2.0).max(0.5);
    let area = s.active().ok_or(EngineError::NoDocument)?.doc.bounds();
    let mode = sel::SelectionMode::parse(p.get("mode").and_then(Value::as_str).unwrap_or("add"));
    let mask = stroke_mask(&points, radius, area);
    crate::selection_cmds::set_selection(s, "Text Selection", area, mask, mode)
}

/// The inside of a closed shape.
fn polygon_mask(points: &[[f64; 2]], area: Rect) -> Vec<f32> {
    let (w, h) = (area.width() as usize, area.height() as usize);
    let mut mask = vec![0f32; w * h];
    let mut crossings = Vec::new();
    for y in 0..h {
        let line = y as f64 + 0.5 + f64::from(area.y0);
        crossings.clear();
        for (i, a) in points.iter().enumerate() {
            let b = points[(i + 1) % points.len()];
            if (a[1] <= line) != (b[1] <= line) {
                crossings.push(a[0] + (line - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) - f64::from(area.x0));
            }
        }
        crossings.sort_by(f64::total_cmp);
        for pair in crossings.chunks_exact(2) {
            let (x0, x1) = (pair[0].round().clamp(0.0, w as f64) as usize, pair[1].round().clamp(0.0, w as f64) as usize);
            mask[y * w + x0..y * w + x1].fill(1.0);
        }
    }
    mask
}

/// `select.textZone`: what a person drew joins the pieces as a piece of its own, or leaves them.
fn zone(s: &mut Session, p: &Value) -> Result<Value> {
    let bad = |m: &str| EngineError::BadParams { cmd: ZONE.into(), msg: m.into() };
    let d = s.text_detection().ok_or_else(|| bad("detect the text first (select.detectText)"))?;
    let area = Rect::new(0, 0, i32::try_from(d.width).unwrap_or(i32::MAX), i32::try_from(d.height).unwrap_or(i32::MAX));
    let points: Option<Vec<[f64; 2]>> = p.get("points").cloned().and_then(|v| serde_json::from_value(v).ok());
    let mask = if let Some(r) = p.get("rect").cloned().and_then(|v| serde_json::from_value::<[f64; 4]>(v).ok()) {
        polygon_mask(&[[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]], area)
    } else if let Some(points) = points.filter(|v| v.len() >= 2) {
        match p.get("size").and_then(Value::as_f64) {
            Some(size) => stroke_mask(&points, (size / 2.0).max(0.5), area),
            None => polygon_mask(&points, area),
        }
    } else {
        return Err(bad("give \"rect\" or \"points\""));
    };
    let inside = |x: usize, y: usize| mask[y * d.width + x] > 0.0;
    let mode = p.get("mode").and_then(Value::as_str).unwrap_or("add");
    let subtract = mode == "subtract";
    let changed = if subtract { Some(d.clipped(|x, y| !inside(x, y))) } else { d.with_zone(inside) };
    match changed {
        Some(left) if left.regions.is_empty() => s.forget_text_detection(),
        Some(left) => s.set_text_detection(Arc::new(left)),
        None => {}
    }
    crate::selection_cmds::set_selection(s, "Text Selection", area, mask, sel::SelectionMode::parse(mode))
}

/// The discs of a stroke along `points`, close enough to read as a line.
fn stroke_mask(points: &[[f64; 2]], radius: f64, area: Rect) -> Vec<f32> {
    let (w, h) = (area.width() as usize, area.height() as usize);
    let mut mask = vec![0f32; w * h];
    let mut disc = |cx: f64, cy: f64| {
        let (cx, cy) = (cx - f64::from(area.x0), cy - f64::from(area.y0));
        let (y0, y1) = ((cy - radius).floor().max(0.0) as usize, ((cy + radius).ceil().max(0.0) as usize).min(h));
        for y in y0..y1 {
            let dy = y as f64 + 0.5 - cy;
            let dx = (radius * radius - dy * dy).max(0.0).sqrt();
            let (x0, x1) = ((cx - dx).round().max(0.0) as usize, ((cx + dx).round().max(0.0) as usize).min(w));
            if x0 < x1 {
                mask[y * w + x0..y * w + x1].fill(1.0);
            }
        }
    };
    let step = (radius / 2.0).max(0.5);
    for (i, b) in points.iter().enumerate() {
        let a = points[i.saturating_sub(1)];
        let n = ((b[0] - a[0]).hypot(b[1] - a[1]) / step).ceil().max(1.0);
        for k in 1..=n as usize {
            let t = k as f64 / n;
            disc(a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t);
        }
    }
    mask
}

fn has_masker(s: &Session) -> std::result::Result<(), String> {
    s.active().ok_or("no document")?;
    if s.text_masker.is_none() {
        return Err(UNAVAILABLE.into());
    }
    Ok(())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: DETECT,
            label: LABEL,
            menu: &[],
            shortcut: None,
            params: r#"{"inSelection"?: bool = false (keep only what is inside the selection, cut at its edge)}: finds the text and sound effects on the page with the mask model, as a job; answers {pieces: [{id, group, box, area, score, suggested, bubble (the speech bubble it lies in, 1.., 0 none; not in the browser), plain (that bubble's background is plain), method ("bubble" in a plain bubble, else "ai": how the tool cleans it unless told otherwise)}], bubbles (how many found), model, device, cpu, fellBack (the GPU failed and the CPU ran it), region, ms (all of it), stages: {readMs (the page's pixels), bubblesMs (finding the bubbles), loadMs (the model, 0 when it was loaded), inferMs (the network), regionsMs (from its answer to the pieces); the last three where the runtime tells them}}. Selects nothing: the model also marks art, so a person chooses"#,
            enabled: has_masker,
            journal: false,
            run: detect,
        },
        CommandSpec {
            id: PIECES,
            label: "Text Selection",
            menu: &[],
            shortcut: None,
            params: r#"{"pieces": [id, …] | "all" | "suggested", "grow"?: px past the letters (by default in proportion to the page), "mode"?: "replace"|"add"|"subtract"|"intersect", "keep"?: [id, …] (pieces whose reach a subtraction leaves), "from"?: px (the contour these pieces have now: they change to "grow" and the rest of the selection stays)}: a normal selection of pieces from the last select.detectText"#,
            enabled: has_masker,
            journal: true,
            run: pieces,
        },
        CommandSpec {
            id: STROKE,
            label: "Text Selection",
            menu: &[],
            shortcut: None,
            params: r#"{"points": [[x, y], …], "size": px, "mode"?: "add"|"subtract"}: a brush stroke adds to the selection or takes from it"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document".to_string()),
            journal: true,
            run: stroke,
        },
        CommandSpec {
            id: ZONE,
            label: "Text Selection",
            menu: &[],
            shortcut: None,
            params: r#"{"rect": [x0, y0, x1, y1] | "points": [[x, y], …] (a closed shape; with "size": a brush stroke of that width), "mode"?: "add"|"subtract"}: adds a zone to the pieces as a piece of its own and to the selection, or cuts it from both"#,
            enabled: has_masker,
            journal: true,
            run: zone,
        },
        CommandSpec {
            id: STEP,
            label: "Text Selection",
            menu: &[],
            shortcut: None,
            params: r#"{"label"?: str (the history step's name, default "Text Selection")}: a history step that changes nothing in the document: the Text Selection tool's own changes (a piece's method, a block split) undo and redo with it"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document".to_string()),
            journal: false,
            run: step,
        },
        CommandSpec {
            id: CLEAN,
            label: "Clean Text",
            menu: &[],
            shortcut: None,
            params: r##"{"steps": [{"pieces": [id, …] | "all" | "suggested", "method": "ai" (Manga LaMa, edit.removeAi) | "fill" (content-aware) | "color" (flat) | "bubble" (the Bubble Text Remover, edit.removeBubbleText, one click per bubble; a bubble it refuses goes to AI, listed in the AI step's "bubblesToAi") | "auto" (each piece's "method" from select.detectText: "bubble" in a plain speech bubble, else "ai"), "grow"?: px, "output"?: "current" (default) | "new" | "single", "layerName"?, "color"?: "#rrggbb" (default: the colour around each blob)}, …], "grow"?: px, "output"?, "layerName"?}: cleans the pieces found by select.detectText, each step in its own way and in order ("all" and "suggested": those no earlier step took); what's cleaned leaves the pieces and the answer lists those left, with new ids. One history step (Clean Text): one undo takes the whole clean back. Started in the background (the app, a host, the control channel) it is a job whose bubble clicks and fills run as jobs of their own, one after another: the answer comes when the job ends, and cancelling it stops what is left. Run inline in the browser it is refused (start it as a job there, or run select.textPieces and then edit.removeAi or edit.cleanQuick, waiting for each)"##,
            enabled: has_masker,
            journal: false,
            run: clean_pieces,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_ai_textmask::Page;
    use photocraft_ai_textmask::regions::find_regions;

    /// Finds whatever is dark.
    struct Dark;
    impl TextMasker for Dark {
        fn model_state(&self) -> photocraft_ai_textmask::ModelState {
            photocraft_ai_textmask::ModelState::Ready
        }
        fn device(&self) -> String {
            "test".into()
        }
        fn timing(&self) -> Option<photocraft_ai_textmask::Timing> {
            Some(photocraft_ai_textmask::Timing { load_ms: 7, infer_ms: 40, regions_ms: 9, fell_back: false })
        }
        fn detect(&self, page: Page<'_>, _: &dyn Fn() -> bool) -> std::result::Result<Detection, photocraft_ai_textmask::Error> {
            let prob: Vec<f32> = page.rgb.chunks_exact(3).map(|c| if c[0] < 128 { 0.995 } else { 0.0 }).collect();
            Ok(find_regions(&prob, page.width, page.height))
        }
    }

    fn page() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 300, "height": 200, "background": "white"})).unwrap();
        s.set_text_masker(Some(Arc::new(Dark)));
        for (x, y) in [(20, 20), (200, 120)] {
            s.execute("select.rect", json!({"x": x, "y": y, "width": 40, "height": 30})).unwrap();
            s.execute("edit.fill", json!({"color": "#000000"})).unwrap();
        }
        s.execute("select.deselect", json!({})).unwrap();
        s
    }

    #[test]
    fn detecting_selects_nothing_and_a_person_picks_the_pieces() {
        let mut s = page();
        assert!(s.execute(PIECES, json!({"pieces": "all"})).is_err(), "nothing detected yet");
        let r = s.execute(DETECT, json!({})).unwrap();
        let pieces = r["pieces"].as_array().unwrap();
        assert_eq!(pieces.len(), 2);
        assert!(s.active().unwrap().doc.selection.is_none(), "detecting selects nothing");
        let top = pieces.iter().find(|p| p["box"][1] == 20).unwrap()["id"].clone();
        s.execute(PIECES, json!({"pieces": [top], "grow": 0})).unwrap();
        let sel = s.active().unwrap().doc.selection.clone().unwrap();
        assert_eq!(sel.content_bounds(), Rect::new(20, 20, 60, 50), "only that piece");
        s.execute(PIECES, json!({"pieces": "all", "grow": 3})).unwrap();
        let b = s.active().unwrap().doc.selection.clone().unwrap().content_bounds();
        assert_eq!(b, Rect::new(17, 17, 243, 153), "both, grown");
    }

    /// The answer says what ran, on what, and where the time went: the usage statistics' `ai_run`.
    #[test]
    fn a_detection_says_where_its_time_went() {
        let mut s = page();
        let r = s.execute(DETECT, json!({})).unwrap();
        assert_eq!((r["model"].as_str(), r["device"].as_str(), r["cpu"].as_bool()), (Some("manga-text-mask"), Some("test"), Some(false)), "{r}");
        assert_eq!(r["region"], json!([0, 0, 300, 200]));
        let t = &r["stages"];
        assert_eq!((&t["loadMs"], &t["inferMs"], &t["regionsMs"]), (&json!(7), &json!(40), &json!(9)), "{r}");
        assert!(t["readMs"].is_u64() && r["ms"].is_u64(), "{r}");
    }

    /// A job's large buffers are asked for before the work: one that can't be had is an error,
    /// where the allocation itself would have ended the app.
    #[test]
    fn what_cannot_be_had_is_an_error_not_an_abort() {
        assert!(room_for(1 << 20).is_ok());
        let e = room_for(u64::MAX / 4).unwrap_err().to_string();
        assert!(e.contains("not enough memory"), "{e}");
    }

    fn covered(s: &Session, x: i32, y: i32) -> bool {
        s.active().unwrap().doc.selection.as_ref().is_some_and(|m| m.sample_channel(x, y, 0) >= 0.5)
    }

    #[test]
    fn taking_a_piece_out_leaves_its_neighbours_reach() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 200, "height": 100, "background": "white"})).unwrap();
        s.set_text_masker(Some(Arc::new(Dark)));
        // Two pieces 10 px apart: grown by 10 they overlap.
        for x in [20, 70] {
            s.execute("select.rect", json!({"x": x, "y": 40, "width": 40, "height": 20})).unwrap();
            s.execute("edit.fill", json!({"color": "#000000"})).unwrap();
        }
        s.execute("select.deselect", json!({})).unwrap();
        let r = s.execute(DETECT, json!({})).unwrap();
        let id = |x: i64| r["pieces"].as_array().unwrap().iter().find(|p| p["box"][0] == x).unwrap()["id"].clone();
        s.execute(PIECES, json!({"pieces": "all", "grow": 10})).unwrap();
        s.execute(PIECES, json!({"pieces": [id(20)], "grow": 10, "mode": "subtract", "keep": [id(70)]})).unwrap();
        assert!(!covered(&s, 30, 50), "the piece went");
        assert!(covered(&s, 62, 50), "the neighbour's reach stayed, overlap and all");
    }

    #[test]
    fn a_new_contour_keeps_what_a_brush_added() {
        let mut s = page();
        s.execute(DETECT, json!({})).unwrap();
        s.execute(PIECES, json!({"pieces": "all", "grow": 10})).unwrap();
        s.execute(STROKE, json!({"points": [[120, 20], [150, 20]], "size": 10})).unwrap();
        assert!(covered(&s, 135, 20), "the brush added");
        assert!(covered(&s, 14, 30));
        s.execute(PIECES, json!({"pieces": "all", "grow": 2.0, "from": 10.0})).unwrap();
        assert!(!covered(&s, 14, 30), "the contour came in");
        assert!(covered(&s, 19, 30), "to its new reach");
        assert!(covered(&s, 135, 20), "the brush's stayed");
        s.execute(STROKE, json!({"points": [[120, 20], [150, 20]], "size": 10, "mode": "subtract"})).unwrap();
        assert!(!covered(&s, 135, 20), "and comes out with the brush");
    }

    #[test]
    fn a_zone_joins_the_pieces_and_another_cuts_them() {
        let mut s = page();
        s.execute(DETECT, json!({})).unwrap();
        s.execute(ZONE, json!({"rect": [100, 20, 150, 60]})).unwrap();
        let d = s.text_detection().unwrap();
        assert_eq!(d.regions.len(), 3, "a piece of its own");
        assert_eq!(d.regions[2].bbox, [100, 20, 150, 60]);
        assert!(covered(&s, 120, 40), "and selected");
        s.execute(ZONE, json!({"points": [[90, 10], [160, 10], [160, 30], [90, 30]], "mode": "subtract"})).unwrap();
        let d = s.text_detection().unwrap();
        assert_eq!(d.regions[2].bbox, [100, 30, 150, 60], "cut");
        assert!(!covered(&s, 120, 25) && covered(&s, 120, 40));
        s.execute(ZONE, json!({"points": [[0, 100], [90, 100]], "size": 20})).unwrap();
        assert!(covered(&s, 45, 100), "a stroke");
        assert!(!covered(&s, 45, 80));
        s.execute(ZONE, json!({"points": [[100, 150], [150, 150], [125, 190]]})).unwrap();
        assert!(covered(&s, 125, 160) && !covered(&s, 102, 185), "a lasso");
        assert!(s.execute(ZONE, json!({})).is_err());
    }

    #[test]
    fn each_step_cleans_its_pieces_in_its_own_way() {
        let mut s = page();
        let r = s.execute(DETECT, json!({})).unwrap();
        let id = |x: i64| r["pieces"].as_array().unwrap().iter().find(|p| p["box"][0] == x).unwrap()["id"].clone();
        let px = |s: &Session, x, y| photocraft_compose::render(&s.active().unwrap().doc, Rect::from_xywh(x, y, 1, 1)).px[0];
        let layers = s.active().unwrap().doc.layers.len();
        let out = s.execute(CLEAN, json!({"steps": [{"pieces": [id(20)], "method": "color", "color": "#00ff00", "grow": 2}]})).unwrap();
        assert_eq!(px(&s, 30, 30)[1], 1.0, "painted");
        assert_eq!(px(&s, 30, 30)[0], 0.0);
        assert_eq!(out["pieces"].as_array().unwrap().len(), 1, "the other is left, renumbered");
        assert_eq!(out["pieces"][0]["id"], 1);
        assert!(s.active().unwrap().doc.selection.is_none());
        let out = s.execute(CLEAN, json!({"steps": [{"pieces": "all", "method": "fill", "output": "new", "layerName": "Limpieza"}]})).unwrap();
        assert!(px(&s, 210, 130)[0] > 0.9, "filled");
        assert_eq!(s.active().unwrap().doc.layers.len(), layers + 1, "on a layer of its own");
        assert_eq!(out["pieces"], json!([]));
        // "all" after a first step: what's left, once.
        let mut s = page();
        s.execute(DETECT, json!({})).unwrap();
        let out = s.execute(CLEAN, json!({"steps": [{"pieces": "suggested", "method": "color"}, {"pieces": "all", "method": "fill"}]})).unwrap();
        assert_eq!(out["steps"].as_array().unwrap().len(), 1, "nothing was left for the second");
        assert!(s.text_detection().is_none());
        assert!(s.execute(CLEAN, json!({"steps": [{"pieces": "all", "method": "fill"}]})).is_err(), "nothing found");
    }

    #[test]
    fn suggested_pieces_are_a_list_of_their_own() {
        let mut s = page();
        s.execute(DETECT, json!({})).unwrap();
        s.execute(PIECES, json!({"pieces": "suggested", "grow": 0})).unwrap();
        assert!(covered(&s, 30, 30));
    }

    #[test]
    fn a_detection_can_stay_inside_the_selection() {
        let mut s = page();
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 100, "height": 100})).unwrap();
        let r = s.execute(DETECT, json!({"inSelection": true})).unwrap();
        assert_eq!(r["pieces"].as_array().unwrap().len(), 1, "only the piece in the zone");
        let r = s.execute(DETECT, json!({})).unwrap();
        assert_eq!(r["pieces"].as_array().unwrap().len(), 2, "by default the whole page");
    }

    #[test]
    fn a_detection_belongs_to_its_document() {
        let mut s = page();
        s.execute(DETECT, json!({})).unwrap();
        s.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
        assert!(s.text_detection().is_none());
        assert!(s.execute(PIECES, json!({"pieces": "all"})).is_err());
    }

    /// A step of the tool's own: a history state that leaves the document as it was, and clean.
    #[test]
    fn a_tool_step_is_a_history_state_that_changes_nothing() {
        let mut s = page();
        let st = s.active().unwrap();
        let (doc, id, past) = (st.doc.clone(), st.history.current_id(), st.history.past_len());
        s.execute(STEP, json!({"label": "Text Selection"})).unwrap();
        let st = s.active().unwrap();
        assert!(st.history.current_id() != id && st.history.past_len() == past + 1);
        assert!(photocraft_engine_same(&doc, &st.doc));
        s.undo();
        assert_eq!(s.active().unwrap().history.current_id(), id, "undo brings back the id");
    }

    /// A clean of several steps, one of them on a new layer, is one history step: one undo brings
    /// the page back (no layer of the clean left on it), one redo the clean.
    #[test]
    fn a_clean_of_several_steps_is_one_history_step() {
        let mut s = page();
        let r = s.execute(DETECT, json!({})).unwrap();
        let id = |x: i64| r["pieces"].as_array().unwrap().iter().find(|p| p["box"][0] == x).unwrap()["id"].clone();
        let st = s.active().unwrap();
        let (before, past, at) = (st.doc.clone(), st.history.past_len(), st.history.current_id());
        let steps = json!([{"pieces": [id(20)], "method": "color", "color": "#00ff00"}, {"pieces": "all", "method": "fill", "output": "new", "layerName": "AI Clean"}]);
        let out = s.execute(CLEAN, json!({"steps": steps})).unwrap();
        assert_eq!(out["steps"].as_array().unwrap().len(), 2);
        let st = s.active().unwrap();
        let cleaned = st.doc.clone();
        assert_eq!(cleaned.layers.len(), before.layers.len() + 1, "the second step's layer");
        assert!(!crate::layer_multi_cmds::same_pixels(&before, &cleaned));
        assert_eq!(st.history.past_len(), past + 1, "one step: {:?}", st.history.entries());
        assert_eq!(st.history.undo_label(), Some("Clean Text"));
        s.execute("edit.undo", json!({})).unwrap();
        let st = s.active().unwrap();
        assert_eq!(st.history.current_id(), at);
        assert_eq!(st.doc.layers.len(), before.layers.len(), "the clean's layer went with it");
        assert!(photocraft_engine_same(&before, &st.doc));
        s.execute("edit.redo", json!({})).unwrap();
        assert!(photocraft_engine_same(&cleaned, &s.active().unwrap().doc));
        // A clean with nothing to do leaves no step.
        let past = s.active().unwrap().history.past_len();
        assert!(s.execute(CLEAN, json!({"steps": [{"pieces": "all", "method": "fill"}]})).is_err(), "nothing left to clean");
        assert_eq!(s.active().unwrap().history.past_len(), past);
    }

    /// A page with the two blobs found, an inpainter, and a clean of both: one flat, one with AI
    /// on a new layer.
    fn ai_clean() -> (Session, Value) {
        let mut s = page();
        s.set_inpainter(Some(Arc::new(photocraft_ai_lama::service::FakeInpainter::default())));
        let r = s.execute(DETECT, json!({})).unwrap();
        let first = r["pieces"].as_array().unwrap().iter().find(|p| p["box"][0] == 20).unwrap()["id"].clone();
        let steps = json!([{"pieces": [first], "method": "color", "color": "#00ff00"}, {"pieces": "all", "method": "ai", "output": "new", "layerName": "AI Clean"}]);
        (s, json!({"steps": steps}))
    }

    /// Polls until job `id` has ended: every job that ended on the way.
    fn poll_to_end(s: &mut Session, id: crate::jobs::JobId) -> Vec<crate::jobs::JobEvent> {
        let mut events = Vec::new();
        for _ in 0..20_000 {
            events.extend(s.poll_jobs());
            if events.iter().any(|e| e.id == id) {
                return events;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the clean never ended: {events:?}");
    }

    /// Started as a job (the app, a host, a script through the control channel) the clean returns
    /// at once with nothing run yet; its fills then run as jobs of their own, one after another,
    /// as the polls find them done; it ends with the same answer, the same pixels and the same
    /// one history step as the clean run inline.
    #[test]
    fn started_as_a_job_a_clean_holds_no_one_and_ends_as_the_inline_one() {
        use crate::jobs::{JobOutcome, Started};
        let (mut inline, params) = ai_clean();
        let answer = inline.execute(CLEAN, params.clone()).unwrap();
        let (mut s, params) = ai_clean();
        let st = s.active().unwrap();
        let (before, past) = (st.doc.clone(), st.history.past_len());
        let Started::Job(job) = s.start(CLEAN, params).unwrap() else { panic!("a clean started in the background is a job") };
        assert!(Arc::ptr_eq(&s.active().unwrap().doc, &before), "nothing ran in the call that started it");
        assert_eq!(s.job(job).map(|j| j.label), Some("Clean Text".to_string()));
        let events = poll_to_end(&mut s, job);
        assert!(events.iter().any(|e| e.command == crate::remove_ai_cmds::ID && matches!(e.outcome, JobOutcome::Done(_))), "the AI fill ran as a job of its own: {events:?}");
        let ended = events.iter().find(|e| e.id == job).unwrap();
        assert_eq!(ended.command, CLEAN);
        let JobOutcome::Done(out) = &ended.outcome else { panic!("the clean ended {:?}", ended.outcome) };
        assert_eq!(out["pieces"], answer["pieces"]);
        assert_eq!(out["steps"].as_array().map(Vec::len), Some(2));
        assert!(!s.has_jobs());
        let st = s.active().unwrap();
        let all = Rect::new(0, 0, 300, 200);
        assert_eq!(photocraft_compose::render(&st.doc, all), photocraft_compose::render(&inline.active().unwrap().doc, all), "the same page as the clean run inline");
        assert!(st.doc.selection.is_none());
        assert_eq!(st.doc.layers.len(), before.layers.len() + 1);
        assert_eq!(st.history.past_len(), past + 1, "one step: {:?}", st.history.entries());
        s.execute("edit.undo", json!({})).unwrap();
        assert!(photocraft_engine_same(&s.active().unwrap().doc, &before));
    }

    /// Waiting for the clean's job (a script's default) runs its commands to the end.
    #[test]
    fn waiting_for_a_clean_job_runs_it_to_the_end() {
        let (mut s, params) = ai_clean();
        let crate::jobs::Started::Job(job) = s.start(CLEAN, params).unwrap() else { panic!("a job") };
        let out = s.wait_job(job).unwrap();
        assert_eq!(out["steps"].as_array().map(Vec::len), Some(2));
        assert_eq!(out["pieces"], json!([]));
        assert!(!s.has_jobs() && s.text_detection().is_none());
    }

    /// A clean cancelled before its commands ran runs none of them.
    #[test]
    fn a_cancelled_clean_job_runs_nothing_more() {
        let (mut s, params) = ai_clean();
        let before = s.active().unwrap().doc.clone();
        let crate::jobs::Started::Job(job) = s.start(CLEAN, params).unwrap() else { panic!("a job") };
        assert!(s.cancel_job(job));
        let events: Vec<_> = (0..50).flat_map(|_| s.poll_jobs()).collect();
        assert!(events.iter().all(|e| e.id == job && e.outcome == crate::jobs::JobOutcome::Cancelled), "{events:?}");
        assert!(!s.has_jobs());
        assert!(Arc::ptr_eq(&s.active().unwrap().doc, &before), "the page is as it was");
        assert!(s.text_detection().is_some(), "and its pieces");
    }

    fn photocraft_engine_same(a: &photocraft_doc::Document, b: &photocraft_doc::Document) -> bool {
        crate::layer_multi_cmds::same_pixels(a, b) && a.selection.is_none() == b.selection.is_none()
    }

    #[test]
    fn with_no_masker_the_commands_are_off() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        assert!(s.execute(DETECT, json!({})).is_err());
    }

    /// Finds what is pure black: the lettering of the test pages (the outlines and screentone
    /// are grey).
    struct Black;
    impl TextMasker for Black {
        fn model_state(&self) -> photocraft_ai_textmask::ModelState {
            photocraft_ai_textmask::ModelState::Ready
        }
        fn device(&self) -> String {
            "test".into()
        }
        fn detect(&self, page: Page<'_>, _: &dyn Fn() -> bool) -> std::result::Result<Detection, photocraft_ai_textmask::Error> {
            let prob: Vec<f32> = page.rgb.as_chunks::<3>().0.iter().map(|c| if c[0] < 10 { 0.995 } else { 0.0 }).collect();
            Ok(find_regions(&prob, page.width, page.height))
        }
    }

    /// A page as on Death Note p177: two speech bubbles with words in them over a screentone, one
    /// of them with screentone inside too, and an effect ("NOK-NOK") straight on the screentone.
    pub(crate) fn bubbles_page() -> Session {
        let mut s = Session::new();
        // A bubble is a few hundredths of a page: the page around the art is large.
        s.execute("file.new", json!({"width": 1400, "height": 1800, "background": "white"})).unwrap();
        s.set_text_masker(Some(Arc::new(Black)));
        s.edit("art", |doc, active| {
            let surf = doc.layer_mut(active.ok_or(EngineError::NoDocument)?).ok_or(EngineError::NoDocument)?.surface_mut().ok_or(EngineError::NoDocument)?;
            let grey = |v: f32| [v, v, v, 1.0];
            // Screentone everywhere: grey dots every 6 px.
            for y in (0..900).step_by(6) {
                for x in (0..700).step_by(6) {
                    surf.fill_rect(Rect::new(x, y, x + 3, y + 3), &grey(0.45));
                }
            }
            // Ellipses: an outline, then the white (or the screentone kept) inside.
            let ellipse = |surf: &mut photocraft_raster::Surface, cx: i32, cy: i32, rx: i32, ry: i32, v: Option<f32>| {
                for y in -ry..=ry {
                    let dx = (f64::from(rx) * (1.0 - f64::from(y * y) / f64::from(ry * ry)).max(0.0).sqrt()).round() as i32;
                    match v {
                        Some(v) => surf.fill_rect(Rect::new(cx - dx, cy + y, cx + dx + 1, cy + y + 1), &grey(v)),
                        None => {
                            // Clear the dots along the row: white, the tone redrawn on it.
                            surf.fill_rect(Rect::new(cx - dx, cy + y, cx + dx + 1, cy + y + 1), &grey(1.0));
                            if (cy + y) % 6 < 3 {
                                let mut x = (cx - dx).div_euclid(6) * 6;
                                while x <= cx + dx {
                                    let (a, b) = (x.max(cx - dx), (x + 3).min(cx + dx + 1));
                                    if a < b {
                                        surf.fill_rect(Rect::new(a, cy + y, b, cy + y + 1), &grey(0.45));
                                    }
                                    x += 6;
                                }
                            }
                        }
                    }
                }
            };
            ellipse(surf, 200, 180, 150, 100, Some(0.2));
            ellipse(surf, 200, 180, 145, 95, Some(1.0));
            ellipse(surf, 500, 520, 150, 100, Some(0.2));
            ellipse(surf, 500, 520, 145, 95, None);
            // Words: black letters in both bubbles, and the effect on the tone.
            for (x0, y0) in [(130, 150), (130, 195), (430, 490), (430, 535)] {
                for k in 0..6 {
                    surf.fill_rect(Rect::new(x0 + k * 24, y0, x0 + k * 24 + 14, y0 + 24), &grey(0.0));
                }
            }
            for k in 0..4 {
                surf.fill_rect(Rect::new(120 + k * 60, 720, 160 + k * 60, 820), &grey(0.0));
            }
            Ok(())
        })
        .unwrap();
        s
    }

    /// Pieces in a speech bubble with a plain background go to the Bubble Text Remover, the rest
    /// (an effect on screentone, a bubble with screentone inside) to AI.
    #[test]
    fn pieces_in_a_plain_bubble_are_the_bubble_removers_and_effects_ai() {
        let mut s = bubbles_page();
        let r = s.execute(DETECT, json!({})).unwrap();
        let pieces = r["pieces"].as_array().unwrap();
        let method = |x: u32, y: u32| {
            let p = pieces
                .iter()
                .find(|p| p["box"][0].as_u64().unwrap() as u32 <= x && x < p["box"][2].as_u64().unwrap() as u32 && p["box"][1].as_u64().unwrap() as u32 <= y && y < p["box"][3].as_u64().unwrap() as u32)
                .unwrap_or_else(|| panic!("a piece at {x},{y}: {r}"));
            (p["method"].as_str().unwrap().to_string(), p["bubble"].as_u64().unwrap(), p["plain"].as_bool().unwrap())
        };
        assert_eq!(method(135, 160), ("bubble".into(), 1, true), "the plain bubble's first line");
        assert_eq!(method(135, 205).0, "bubble", "and its second line");
        assert_eq!(method(135, 205).1, method(135, 160).1, "one bubble");
        assert_eq!(method(435, 500).0, "ai", "the bubble with screentone in it");
        assert_eq!(method(130, 760), ("ai".into(), 0, false), "the effect on the tone");
        assert!(r["bubbles"].as_u64().unwrap() >= 1, "{r}");
        assert!(r["stages"]["bubblesMs"].as_u64().unwrap() < 1500, "{r}");
        // And they are what's suggested.
        assert!(pieces.iter().all(|p| p["suggested"] == true), "{r}");
    }

    /// cleanPieces' "auto": the plain bubble's text goes without AI, as the Bubble Text Remover
    /// does it; nothing else changes.
    #[test]
    fn auto_cleans_a_plain_bubble_without_ai() {
        let mut s = bubbles_page();
        s.execute(DETECT, json!({})).unwrap();
        let d = s.text_detection().unwrap();
        let ids: Vec<u32> = d.regions.iter().filter(|r| auto_method(r) == "bubble").map(|r| r.id).collect();
        assert!(!ids.is_empty());
        let r = s.execute(CLEAN, json!({"steps": [{"pieces": ids, "method": "bubble"}]})).unwrap();
        let v = |s: &Session, x, y| photocraft_compose::render(&s.active().unwrap().doc, Rect::from_xywh(x, y, 1, 1)).px[0][0];
        assert!(v(&s, 135, 160) > 0.9 && v(&s, 135, 205) > 0.9, "the letters are gone: {r}");
        assert!(v(&s, 130, 760) < 0.1, "the effect stays");
        assert_eq!(r["steps"].as_array().unwrap().len(), 1, "one click for the bubble: {r}");
        assert!(r["pieces"].as_array().unwrap().iter().all(|p| p["method"] == "ai"), "what's left is AI's: {r}");
    }
}
