//! The Font Finder tool (T group): a click takes a square of the page around it, a drag takes that
//! rectangle, and `font.find` answers five fonts that look like the lettering there, with its fill
//! and outline, with no model and no OCR. Each shows drawn with its look; choosing one applies it to
//! the type layer in use and to the next text.
//!
//! It searches every font loaded in the app. In the browser those include the stored ones the
//! session hasn't read yet: they're read first, one by one and let go, however many there are.
//!
//! Google Fonts is optional and starts off. Turned on, the app fetches an index of the catalog's
//! shapes and, for each answer from it, that font's file, to draw it; choosing one keeps it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, PoisonError};

use photocraft_engine::font_find_cmds::{Face, ID, pack, read_font_file};
use photocraft_text::user_fonts;
use wasm_sync::Mutex;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::state::{BubbleTextStyle, Tool};
use crate::theme::Tokens;

/// The square a click takes, in document pixels.
const CLICK_SIDE: f64 = 256.0;
/// What the options draw: every kind of letter, as no words are known.
const SAMPLE: &str = "ABCDEF abcdef 123";
/// A drawing's letter size, whatever the zone's, so they can be told apart.
const SAMPLE_PT: f32 = 28.0;
/// The tallest the zone shows.
const CROP_HEIGHT: f32 = 110.0;
/// A drag smaller than this is a click.
const CLICK_DRAG: f64 = 4.0;
/// Fewer faces than this and the answer can't be much: say so.
const FEW_FACES: u64 = 10;

/// The tool's window (`PhotocraftApp::font_finder`).
#[derive(Default)]
pub(crate) struct State {
    /// The answer on screen, and each option's picture.
    shown: Option<Arc<Value>>,
    cards: Vec<Option<egui::TextureHandle>>,
    /// The zone as it is on the page, to compare with.
    crop: Option<egui::TextureHandle>,
    closed: bool,
    /// An option's family is on its way: draw them again when it comes.
    awaiting: bool,
    drawn_generation: u64,
    /// The zone to search once the stored fonts are read, and how many were to read.
    waiting: Option<[f64; 4]>,
    to_read: usize,
    /// Stored files read off the UI's thread.
    read: Arc<Mutex<Vec<(String, Vec<Face>)>>>,
    /// The Google Fonts index on its way, what came of it, and why it failed.
    pack_fetching: bool,
    pack_arrived: Arc<Mutex<Option<Result<pack::Pack, String>>>>,
    pack_error: Option<String>,
    /// An answer's fonts from the catalog: on their way, arrived and not yet handled, and here. The
    /// one chosen before it came.
    font_fetching: HashSet<String>,
    font_arrived: Arc<Mutex<Vec<(String, Result<Vec<u8>, String>)>>>,
    catalog_fonts: HashMap<String, Arc<Vec<u8>>>,
    keep: HashSet<String>,
}

/// Google Fonts on or off.
fn set_google(app: &mut PhotocraftApp, on: bool) {
    app.ui.tool_options.font_find_google = on;
    app.font_finder.pack_error = None;
    if !on {
        app.session.set_font_pack(None);
    }
}

/// The index is fetched when it's wanted, and joins the search when it's read.
fn pump_pack(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let arrived = app.font_finder.pack_arrived.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(r) = arrived {
        app.font_finder.pack_fetching = false;
        match r {
            Ok(p) if app.ui.tool_options.font_find_google => app.session.set_font_pack(Some(p)),
            Ok(_) => {}
            Err(why) => {
                app.font_finder.pack_error = Some(why);
                app.ui.tool_options.font_find_google = false;
            }
        }
    }
    // Asked for, not woken: a thread of the browser's pool can't touch the window.
    if app.font_finder.pack_fetching {
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
    }
    if !app.ui.tool_options.font_find_google || app.session.has_font_pack() || app.font_finder.pack_fetching {
        return;
    }
    let Some(fetch) = &app.services.fetch else {
        app.font_finder.pack_error = Some(tl!("Not available in this build").to_string());
        app.ui.tool_options.font_find_google = false;
        return;
    };
    app.font_finder.pack_fetching = true;
    let slot = app.font_finder.pack_arrived.clone();
    fetch(pack::URL.to_string(), pack::BYTES.saturating_mul(2).max(1 << 20), Box::new(move |r| {
        let work = move || {
            let read = r.and_then(|bytes| {
                if photocraft_text::sha256::sha256_hex(&bytes) != pack::SHA256 {
                    return Err("the file isn't the expected one".to_string());
                }
                pack::read(&bytes).ok_or_else(|| "the file can't be read".to_string())
            });
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(read);
        };
        if crate::jobs_ui::OFF_THREAD { crate::jobs_ui::off_thread(work) } else { work() }
    }));
}

/// The name a catalog font's file goes by.
fn file_of(url: &str) -> String {
    url.rsplit('/').next().unwrap_or("font.ttf").to_string()
}

/// Keeps a catalog font among the person's own.
fn keep_font(app: &mut PhotocraftApp, url: &str, bytes: &[u8]) {
    app.font_sink(None).send(crate::user_fonts_ui::Delivery::File(file_of(url), bytes.to_vec()));
}

/// An answer's fonts from the catalog come, to be drawn.
fn pump_fonts(app: &mut PhotocraftApp, ctx: &egui::Context, options: &[Value]) {
    let arrived = std::mem::take(&mut *app.font_finder.font_arrived.lock().unwrap_or_else(PoisonError::into_inner));
    for (url, r) in arrived {
        app.font_finder.font_fetching.remove(&url);
        match r {
            Ok(bytes) => {
                photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner).fonts.register_font_data(bytes.clone());
                if app.font_finder.keep.remove(&url) {
                    keep_font(app, &url, &bytes);
                }
                app.font_finder.catalog_fonts.insert(url, Arc::new(bytes));
                app.font_finder.cards.clear();
            }
            Err(why) => {
                app.ui.status = why;
                app.ui.status_error = true;
            }
        }
    }
    for url in options.iter().filter_map(|o| o["url"].as_str()) {
        if app.font_finder.catalog_fonts.contains_key(url) || app.font_finder.font_fetching.contains(url) {
            continue;
        }
        let Some(fetch) = &app.services.fetch else { return };
        app.font_finder.font_fetching.insert(url.to_string());
        let (slot, url) = (app.font_finder.font_arrived.clone(), url.to_string());
        fetch(url.clone(), 64 << 20, Box::new(move |r| {
            slot.lock().unwrap_or_else(PoisonError::into_inner).push((url, r));
        }));
    }
    if !app.font_finder.font_fetching.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
    }
}

fn search(app: &mut PhotocraftApp, rect: [f64; 4]) {
    if let Err(e) = app.run(ID, json!({"rect": rect})) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// A stored file arrived: its faces are read off the UI's thread and it's let go.
pub(crate) fn scanned(app: &mut PhotocraftApp, id: String, bytes: Result<Vec<u8>, String>) {
    let out = app.font_finder.read.clone();
    let work = move || {
        let faces = bytes.map(|b| read_font_file(&b)).unwrap_or_default();
        out.lock().unwrap_or_else(PoisonError::into_inner).push((id, faces));
    };
    if crate::jobs_ui::OFF_THREAD { crate::jobs_ui::off_thread(work) } else { work() }
}

/// What arrived joins the search, and with all of it there the search starts.
fn collect(app: &mut PhotocraftApp) {
    let arrived = std::mem::take(&mut *app.font_finder.read.lock().unwrap_or_else(PoisonError::into_inner));
    for (id, faces) in arrived {
        user_fonts::store().scan_done(&id);
        app.session.add_font_file(id, faces);
    }
    if app.font_finder.waiting.is_some() && user_fonts::store().scans_left() == 0 {
        let rect = app.font_finder.waiting.take();
        if let Some(rect) = rect.filter(|_| app.ui.tool == Tool::FontFinder) {
            search(app, rect);
        }
    }
}

/// A press and release of the tool (document px).
pub fn finish(app: &mut PhotocraftApp, start: [f64; 2], end: [f64; 2]) {
    let (w, h) = ((end[0] - start[0]).abs(), (end[1] - start[1]).abs());
    let rect = if w.max(h) < CLICK_DRAG {
        [end[0] - CLICK_SIDE / 2.0, end[1] - CLICK_SIDE / 2.0, CLICK_SIDE, CLICK_SIDE]
    } else {
        [start[0].min(end[0]), start[1].min(end[1]), w, h]
    };
    app.font_finder.closed = false;
    let session = &app.session;
    let left = user_fonts::store().ask_scan(|id| session.font_file_known(id));
    if left > 0 {
        if app.font_finder.waiting.is_none() {
            app.font_finder.to_read = left;
        }
        app.font_finder.waiting = Some(rect);
    } else {
        search(app, rect);
    }
}

pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    ui.label(tl!("Click on lettering or drag a rectangle around it to find its font"));
    crate::widgets::vline(ui, 22.0);
    let mut on = app.ui.tool_options.font_find_google;
    let tip = crate::i18n::fmt(tl!("Also looks through Google Fonts, not only your fonts: downloads an index of {mb} MB, and from jsDelivr the fonts it shows you"), &[("mb", &(pack::BYTES >> 20).to_string())]);
    if crate::widgets::checkbox(ui, &mut on, tl!("Also Google Fonts")).on_hover_text(tip).clicked() {
        set_google(app, on);
    }
    if app.font_finder.pack_fetching {
        ui.label(tl!("Downloading its index…"));
    } else if let Some(why) = &app.font_finder.pack_error {
        let line = crate::i18n::fmt(tl!("Couldn't get Google Fonts: {why}"), &[("why", why)]);
        ui.label(egui::RichText::new(line).color(Tokens::get(ui.ctx()).warning));
    }
}

fn style_of(o: &Value) -> BubbleTextStyle {
    let p = &o["params"];
    let text = |k: &str| p[k].as_str().unwrap_or_default().to_string();
    let stroke = &p["stroke"];
    BubbleTextStyle {
        font: text("font"),
        style: text("fontStyle"),
        size: p["size"].as_f64().unwrap_or(30.0) as f32,
        color: text("color"),
        stroke: stroke.is_object(),
        stroke_color: stroke["color"].as_str().unwrap_or("#ffffff").to_string(),
        stroke_width: stroke["width"].as_f64().unwrap_or(4.0) as f32,
        ..BubbleTextStyle::default()
    }
}

/// The answer's zone as the page shows it.
fn crop_of(app: &PhotocraftApp, answer: &Value, ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let side = |i: usize| answer["rect"].get(i).and_then(Value::as_i64);
    let (x, y, w, h) = (side(0)? as i32, side(1)? as i32, side(2)? as u32, side(3)? as u32);
    let doc = &app.session.active()?.doc;
    let buffer = photocraft_compose::render(doc, photocraft_geom::Rect::from_xywh(x, y, w, h));
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let px: Vec<egui::Color32> = buffer.px.iter().map(|p| egui::Color32::from_rgba_unmultiplied(q(p[0]), q(p[1]), q(p[2]), q(p[3]))).collect();
    Some(ctx.load_texture("font-finder-zone", egui::ColorImage::new([w as usize, h as usize], px), egui::TextureOptions::LINEAR))
}

/// `o` on the type layer in use, and on what the type tools write next.
fn choose(app: &mut PhotocraftApp, o: &Value) {
    let st = style_of(o);
    let params = o["params"].clone();
    let tools = &mut app.ui.tool_options;
    tools.type_font.clone_from(&st.font);
    tools.type_style.clone_from(&st.style);
    tools.type_size = st.size;
    let bubble = &mut tools.bubble_styles.bubble;
    (bubble.font, bubble.style, bubble.size, bubble.color) = (st.font.clone(), st.style.clone(), st.size, st.color.clone());
    (bubble.stroke, bubble.stroke_color, bubble.stroke_width) = (st.stroke, st.stroke_color.clone(), st.stroke_width);
    // From the catalog: it stays, among the person's fonts.
    if let Some(url) = o["url"].as_str() {
        match app.font_finder.catalog_fonts.get(url).cloned() {
            Some(bytes) => keep_font(app, url, &bytes),
            None => {
                app.font_finder.keep.insert(url.to_string());
            }
        }
    }
    let name = o["family"].as_str().unwrap_or_default().to_string();
    let layer = app.session.active().and_then(|d| d.active_layer).filter(|id| {
        app.session.active().is_some_and(|d| matches!(d.doc.layer(*id).map(|l| &l.content), Some(photocraft_doc::LayerContent::Text(_))))
    });
    if let Some(id) = layer {
        let styles = json!({"bubble": params.clone(), "narration": params.clone(), "floating": params});
        if let Err(e) = app.run("type.setBubbleStyle", json!({"layer": id.0, "styles": styles})) {
            app.ui.status = e;
            app.ui.status_error = true;
            return;
        }
    }
    app.ui.status = crate::i18n::fmt(tl!("Using {font}"), &[("font", &name)]);
    app.ui.status_error = false;
}

/// The window with the answer, each option drawn with its look.
pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    collect(app);
    if app.ui.tool == Tool::FontFinder {
        pump_pack(app, ctx);
    }
    let found = app.session.font_found();
    if !app.font_finder.shown.as_ref().zip(found.as_ref()).is_some_and(|(a, b)| Arc::ptr_eq(a, b)) {
        app.font_finder.shown.clone_from(&found);
        app.font_finder.cards.clear();
        app.font_finder.crop = found.as_deref().and_then(|a| crop_of(app, a, ctx));
        // An option may be a stored font that was only read to search it.
        for o in found.iter().flat_map(|a| a["options"].as_array().cloned().unwrap_or_default()) {
            user_fonts::request(o["family"].as_str().unwrap_or_default());
        }
        app.font_finder.awaiting = user_fonts::store().loading();
        app.font_finder.drawn_generation = photocraft_text::fonts::generation();
    }
    let generation = photocraft_text::fonts::generation();
    if app.font_finder.awaiting && app.font_finder.drawn_generation != generation {
        app.font_finder.drawn_generation = generation;
        app.font_finder.awaiting = user_fonts::store().loading();
        app.font_finder.cards.clear();
    }
    if app.ui.tool != Tool::FontFinder || app.font_finder.closed {
        return;
    }
    if app.font_finder.waiting.is_some() {
        let left = user_fonts::store().scans_left();
        let total = app.font_finder.to_read.max(left).max(1);
        let screen = crate::safe_area::rect(ctx);
        let at = egui::pos2((screen.right() - 700.0).max(screen.left() + 8.0), screen.top() + 90.0);
        egui::Window::new(tl!("Find Font")).id(egui::Id::unique("font-finder-reading")).collapsible(false).resizable(false).default_pos(at).show(ctx, |ui| {
            ui.label(crate::i18n::fmt(tl!("Reading your fonts: {done} of {total}"), &[("done", &(total - left).to_string()), ("total", &total.to_string())]));
            ui.add(egui::ProgressBar::new((total - left) as f32 / total as f32));
        });
        ctx.request_repaint();
        return;
    }
    let Some(answer) = app.font_finder.shown.clone() else { return };
    let options = answer["options"].as_array().cloned().unwrap_or_default();
    pump_fonts(app, ctx, &options);
    // A drawing a frame: five at once would hold the page.
    if app.font_finder.cards.len() < options.len() {
        let option = &options[app.font_finder.cards.len()];
        let st = BubbleTextStyle { size: SAMPLE_PT, ..style_of(option) };
        let img = crate::bubble_type_ui::render_text(&st, SAMPLE, option["params"].get("__roleEffects"));
        let n = app.font_finder.cards.len();
        app.font_finder.cards.push(img.map(|i| ctx.load_texture(format!("font-finder-{n}"), i, egui::TextureOptions::LINEAR)));
        ctx.request_repaint();
    }
    let t = Tokens::get(ctx);
    let screen = crate::safe_area::rect(ctx);
    let phone = screen.width() < 600.0;
    let mut open = true;
    let mut picked = None;
    let mut w = egui::Window::new(tl!("Find Font")).id(egui::Id::unique("font-finder-window")).open(&mut open).collapsible(false);
    // Beside the page, where the zone can still be seen.
    w = if phone { w.fixed_rect(screen.shrink(4.0)).resizable(false) } else { w.default_width(420.0).default_height(screen.height() * 0.8).default_pos(egui::pos2(screen.right() - 700.0, screen.top() + 90.0)) };
    let faces = answer["faces"].as_u64().unwrap_or(0);
    let mut load = false;
    w.show(ctx, |ui| {
        if faces < FEW_FACES {
            ui.label(egui::RichText::new(tl!("Very few fonts are loaded: load yours and the search looks through them")).color(t.warning));
            load = ui.button(tl!("Load Fonts…")).clicked();
        }
        if options.is_empty() {
            ui.label(tl!("No lettering found here. Try a tighter rectangle around the letters."));
            return;
        }
        if let Some(tex) = &app.font_finder.crop {
            let s = tex.size_vec2();
            let k = (ui.available_width() / s.x).min(CROP_HEIGHT / s.y);
            ui.add(egui::Image::new((tex.id(), s * k)));
        }
        if options[0]["mixed"].as_bool() == Some(true) {
            ui.label(egui::RichText::new(tl!("This zone has several sizes of lettering: these match the largest")).color(t.warning));
        }
        let among = crate::i18n::fmt(if app.session.has_font_pack() { tl!("Among {n} fonts, yours and Google Fonts. Click one to use it") } else { tl!("Among {n} loaded fonts. Click one to use it") }, &[("n", &faces.to_string())]);
        ui.label(egui::RichText::new(among).color(t.text_dim));
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (i, o) in options.iter().enumerate() {
                let mut name = format!("{}  {}", o["family"].as_str().unwrap_or_default(), o["style"].as_str().unwrap_or_default());
                if o["url"].is_string() {
                    name.push_str("  ·  Google Fonts");
                }
                let size = o["params"]["size"].as_f64().unwrap_or(0.0);
                let r = ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
                    ui.label(egui::RichText::new(format!("{name}  ·  {size:.0} pt")).strong());
                    if let Some(Some(tex)) = app.font_finder.cards.get(i) {
                        let s = tex.size_vec2();
                        let width = ui.available_width().min(s.x);
                        ui.add(egui::Image::new((tex.id(), egui::vec2(width, s.y * width / s.x))));
                    }
                });
                crate::touch_ui::note_rect(ctx, &format!("fontFinder:{i}"), r.response.rect);
                if r.response.clicked() {
                    picked = Some(o.clone());
                }
                ui.add_space(6.0);
            }
        });
    });
    if !open {
        app.font_finder.closed = true;
    }
    if load {
        let _ = app.run("type.loadFonts", json!({}));
    }
    if let Some(o) = picked {
        choose(app, &o);
        app.font_finder.closed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_takes_a_square_and_a_drag_its_rectangle() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.session.execute("file.new", json!({"width": 600, "height": 200, "background": "white"})).unwrap();
        app.session.execute("type.create", json!({"x": 40, "y": 110, "text": "NO PUEDE SER", "font": "Inter", "size": 60, "color": "#000000"})).unwrap();
        app.ui.tool = Tool::FontFinder;
        finish(&mut app, [10.0, 40.0], [590.0, 160.0]);
        let answer = app.session.font_found().expect("an answer");
        assert_eq!(answer["rect"], json!([10, 40, 580, 120]));
        assert!(!answer["options"].as_array().unwrap().is_empty());
        finish(&mut app, [300.0, 100.0], [300.0, 100.0]);
        let answer = app.session.font_found().expect("an answer");
        assert_eq!(answer["rect"], json!([172, 0, 256, 200]), "256 px around the click, inside the page");
    }

    #[test]
    fn google_fonts_starts_off_and_fetches_nothing_until_turned_on() {
        let asked = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut services = crate::Services::default();
        let seen = asked.clone();
        services.fetch = Some(Box::new(move |url, _, done| {
            seen.lock().unwrap().push(url);
            done(Err("no network here".to_string()));
        }));
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.ui.tool = Tool::FontFinder;
        let ctx = egui::Context::default();
        assert!(!app.ui.tool_options.font_find_google, "off by default");
        pump_pack(&mut app, &ctx);
        assert!(asked.lock().unwrap().is_empty(), "nothing goes out");
        set_google(&mut app, true);
        pump_pack(&mut app, &ctx);
        assert_eq!(asked.lock().unwrap().as_slice(), [pack::URL.to_string()]);
        pump_pack(&mut app, &ctx);
        assert!(!app.ui.tool_options.font_find_google, "a failure turns it off");
        assert!(app.font_finder.pack_error.is_some(), "and says so");
        assert_eq!(asked.lock().unwrap().len(), 1, "and doesn't ask again");
    }

    #[test]
    fn choosing_an_option_sets_the_next_text_and_the_layer_in_use() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.session.execute("file.new", json!({"width": 600, "height": 200, "background": "white"})).unwrap();
        app.session.execute("type.create", json!({"x": 40, "y": 110, "text": "HOLA", "font": "JetBrains Mono", "size": 40})).unwrap();
        let o = json!({"family": "Inter", "style": "Regular", "params": {"font": "Inter", "fontStyle": "Regular", "size": 33.0, "color": "#112233", "stroke": null}});
        choose(&mut app, &o);
        assert_eq!(app.ui.tool_options.type_font, "Inter");
        assert_eq!(app.ui.tool_options.bubble_styles.bubble.color, "#112233");
        let st = app.session.active().unwrap();
        let photocraft_doc::LayerContent::Text(t) = &st.doc.layer(st.active_layer.unwrap()).unwrap().content else { panic!("type") };
        assert_eq!(t.runs[0].style.font_family, "Inter");
    }
}
