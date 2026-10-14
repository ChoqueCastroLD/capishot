//! The system clipboard in the browser: images in and out, as the desktop app's OS clipboard
//! service does (`PhotocraftApp::import_os_clipboard`, `export_os_clipboard`).
//!
//! A page can read the clipboard two ways, both asynchronous:
//! - the `paste` event the browser sends for Ctrl/⌘+V, whose data the page may read without asking
//!   (the keyboard paste is the user's consent). Its image is read and decoded while the ⌘V press
//!   that caused it waits ([`after_app`] holds it back), then the press goes on and pastes the
//!   image through the app's own Paste, so placement, undo and history are the desktop's;
//! - `navigator.clipboard.read()`, for Edit › Paste (and Paste Special, New from Clipboard) chosen
//!   from a menu: the app parks the command ([`refresh`], `ClipboardRefreshFn`) and the shell
//!   resumes it when the read finishes. The browser may ask for permission or refuse; a refusal
//!   tells the user to press Ctrl/⌘+V instead.
//!
//! Copies go out as PNG with `navigator.clipboard.write` (other apps, Discord, chat), and are
//! recognised when they come back re-encoded ([`crate::clipboard_data::same_image`]), so
//! Capishot's own copy still pastes in place, with its layers.
//!
//! Text fields keep the browser's own text paste: while one has focus the `paste` event goes to
//! eframe untouched, and a text field's Edit › Paste (egui's `RequestPaste`, which eframe's web
//! runner ignores) reads the text with `navigator.clipboard.readText()`.

use std::cell::RefCell;

use js_sys::{Function, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;

use crate::clipboard_data::{self, Rgba};

/// A paste event's (or a read's) image is used by the paste that follows within this long; later
/// pastes read the clipboard again.
const FRESH_MS: f64 = 2000.0;

#[derive(Default)]
struct State {
    /// The clipboard's image as last seen (a `paste` event, a read, or Capishot's own copy); `None`
    /// when it holds no image.
    image: Option<Rgba>,
    /// What Capishot copied last, to recognise it coming back re-encoded.
    ours: Option<Rgba>,
    /// The browser accepted that copy: the clipboard holds it until something else is copied.
    /// Until then a paste that brings no image keeps it (the copy may have been refused).
    ours_written: bool,
    /// When `image` was last brought up to date by a paste event or read (ms).
    fresh_at: Option<f64>,
    /// An image is being read and decoded.
    reading: bool,
    /// ⌘V presses waiting for that read.
    held: Vec<egui::Event>,
    /// A read finished since the last frame: the waiting paste resumes.
    finished: bool,
    /// A text field had keyboard focus in the last frame.
    typing: bool,
    /// When Ctrl/⌘+V was last pressed (ms), and whether with Shift: a paste event without it (the
    /// browser's own Edit › Paste or context menu, Shift+Insert) pastes by itself.
    paste_key: Option<(f64, bool)>,
    /// Text read for a text field's Edit › Paste, delivered as `Event::Paste`.
    text: Vec<String>,
    /// Messages for the user: (title, lines, is an error).
    notices: Vec<(String, Vec<String>, bool)>,
    /// A copy out has failed this session (reported once).
    write_failed: bool,
    ctx: Option<egui::Context>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

fn now() -> f64 {
    js_sys::Date::now()
}

fn repaint() {
    if let Some(ctx) = with(|s| s.ctx.clone()) {
        ctx.request_repaint();
    }
}

fn notice(title: &str, lines: Vec<String>, error: bool) {
    with(|s| s.notices.push((title.to_string(), lines, error)));
    repaint();
}

/// The browser told the page it may not read or write the clipboard (`NotAllowedError`), or it
/// failed otherwise: `(denied, message)`.
fn describe(e: &JsValue) -> (bool, String) {
    let name = Reflect::get(e, &"name".into()).ok().and_then(|v| v.as_string()).unwrap_or_default();
    let message = Reflect::get(e, &"message".into()).ok().and_then(|v| v.as_string()).unwrap_or_else(|| format!("{e:?}"));
    (name == "NotAllowedError" || name == "SecurityError", if message.is_empty() { name } else { message })
}

/// Listen for the browser's `paste` events and handle egui's text paste requests. Call once,
/// before eframe's runner starts (its own `paste` listener is on the document; this one is on
/// the window, in the capture phase, so it sees the event first).
pub fn install(ctx: &egui::Context) {
    with(|s| s.ctx = Some(ctx.clone()));
    ctx.add_plugin(TextPasteRequests);
    let Some(window) = web_sys::window() else { return };
    let opts = web_sys::AddEventListenerOptions::new();
    opts.set_capture(true);
    let on_paste = Closure::<dyn FnMut(web_sys::ClipboardEvent)>::new(on_paste);
    if window.add_event_listener_with_callback_and_add_event_listener_options("paste", on_paste.as_ref().unchecked_ref(), &opts).is_ok() {
        on_paste.forget();
    }
    let on_key = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(|e: web_sys::KeyboardEvent| {
        if (e.ctrl_key() || e.meta_key()) && e.key().eq_ignore_ascii_case("v") {
            with(|s| s.paste_key = Some((now(), e.shift_key())));
        }
    });
    if window.add_event_listener_with_callback_and_add_event_listener_options("keydown", on_key.as_ref().unchecked_ref(), &opts).is_ok() {
        on_key.forget();
    }
}

/// Ctrl/⌘+V (or the browser's Edit › Paste) outside a text field: take the image, if the
/// clipboard has one, and keep eframe from also pasting its text.
fn on_paste(e: web_sys::ClipboardEvent) {
    if with(|s| s.typing) {
        return;
    }
    let Some(data) = e.clipboard_data() else { return };
    // Not from Ctrl/⌘+V, whose own press pastes: an image found here pastes with a press of its own.
    let key = with(|s| s.paste_key.take()).filter(|(t, _)| now() - t < 1000.0);
    let keyless = key.is_none();
    let press = || {
        if keyless {
            with(|s| {
                s.held.push(egui::Event::Key { key: egui::Key::V, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND })
            });
        }
    };
    let items = data.items();
    let files: Vec<(String, web_sys::File)> = (0..items.length())
        .filter_map(|i| items.get(i))
        .filter(|item| item.kind() == "file")
        .filter_map(|item| Some((item.type_(), item.get_as_file().ok().flatten()?)))
        .collect();
    if let Some((i, _)) = clipboard_data::best_image_type(files.iter().map(|(t, _)| t.as_str()))
        && let Some((mime, file)) = files.into_iter().nth(i)
    {
        e.prevent_default();
        e.stop_propagation();
        with(|s| s.reading = true);
        press();
        wasm_bindgen_futures::spawn_local(async move {
            let bytes = JsFuture::from(file.array_buffer()).await.map(|b| js_sys::Uint8Array::new(&b).to_vec()).map_err(|e| describe(&e).1);
            finish(bytes.and_then(|b| clipboard_data::decode(&mime, &b)).map(Some));
        });
        return;
    }
    // An SVG copied as text, or copied HTML with an inline image.
    let inline = data
        .get_data("image/svg+xml")
        .ok()
        .filter(|t| !t.trim().is_empty())
        .map(|t| ("image/svg+xml".to_string(), t.into_bytes()))
        .or_else(|| data.get_data("text/html").ok().and_then(|h| clipboard_data::html_inline_image(&h)));
    if let Some((mime, bytes)) = inline {
        e.prevent_default();
        e.stop_propagation();
        press();
        finish(clipboard_data::decode(&mime, &bytes).map(Some));
        return;
    }
    // ⇧⌘V (Paste in Place, Paste Into) is the browser's "paste as plain text": its event carries
    // the text only, so it says nothing about an image. The paste reads the clipboard instead.
    if key.is_some_and(|(_, shift)| shift) {
        return;
    }
    // No image (text, or nothing): the paste is Capishot's own copy only while the clipboard may
    // still hold it. eframe gets the event, whose text the ⌘V press pairs with.
    finish(Ok(None));
}

/// A paste event or read has found `result`: the clipboard's image (`Ok(None)`: none).
fn finish(result: Result<Option<Rgba>, String>) {
    let failed = with(|s| {
        s.reading = false;
        s.finished = true;
        s.fresh_at = Some(now());
        match result {
            Ok(Some(image)) => {
                // Capishot's own copy, re-encoded by the browser: the exact pixels it copied, so
                // the app knows it and pastes it in place, with its layers.
                s.image = Some(match &s.ours {
                    Some(ours) if clipboard_data::same_image(ours, &image) => ours.clone(),
                    _ => image,
                });
                None
            }
            Ok(None) => {
                if s.ours_written || s.image != s.ours {
                    s.image = None;
                }
                None
            }
            Err(e) => {
                // The notice says why; the paste that waited doesn't paste something else.
                s.image = None;
                s.held.clear();
                Some(e)
            }
        }
    });
    if let Some(e) = failed {
        log::warn!("photocraft-web: clipboard image: {e}");
        notice("Couldn't paste the clipboard image", vec![e], true);
    }
    repaint();
}

/// `Services::clipboard_get_image`: the clipboard's image as last seen.
pub fn get() -> Option<(u32, u32, Vec<u8>)> {
    with(|s| s.image.clone()).map(|i| (i.width, i.height, i.pixels))
}

/// `Services::clipboard_refresh`: before a paste, make sure [`get`] is current. False when it is (a
/// paste event just brought the clipboard's contents) or the browser can't read the clipboard;
/// true when a read started, the paste then waiting for [`take_finished`].
pub fn refresh() -> bool {
    let (fresh, reading) = with(|s| {
        let fresh = s.fresh_at.is_some_and(|t| now() - t < FRESH_MS);
        if fresh {
            s.fresh_at = None;
        }
        (fresh, s.reading)
    });
    if fresh {
        return false;
    }
    if reading {
        // A paste event's image is on its way: the paste waits for it.
        return true;
    }
    let Some(read) = clipboard_method("read") else { return false };
    with(|s| s.reading = true);
    wasm_bindgen_futures::spawn_local(async move {
        match read_image(read).await {
            Ok(found) => finish(Ok(found)),
            Err(e) => {
                let (denied, message) = describe(&e);
                log::warn!("photocraft-web: navigator.clipboard.read: {message}");
                // Nothing learned about the clipboard: paste what Capishot has, after saying why
                // an image copied elsewhere didn't come.
                with(|s| {
                    s.reading = false;
                    s.finished = true;
                    s.fresh_at = Some(now());
                });
                let hint = paste_key_hint();
                if denied {
                    notice("The browser didn't let Capishot read the clipboard", vec![hint], false);
                } else {
                    notice("Couldn't read the clipboard", vec![message, hint], true);
                }
            }
        }
    });
    true
}

/// "Press Ctrl+V" (⌘V on a Mac): a keyboard paste needs no permission.
fn paste_key_hint() -> String {
    let mac = web_sys::window().and_then(|w| w.navigator().platform().ok()).is_some_and(|p| p.starts_with("Mac") || p.starts_with("iP"));
    let key = if mac { "⌘V" } else { "Ctrl+V" };
    format!("Press {key} to paste instead: the browser always lets a page read what is pasted with the keyboard.")
}

/// `navigator.clipboard` and its method `name`, when the browser has them.
fn clipboard_method(name: &str) -> Option<(JsValue, Function)> {
    let navigator = web_sys::window()?.navigator();
    let clipboard = Reflect::get(&navigator, &"clipboard".into()).ok().filter(|c| !c.is_undefined() && !c.is_null())?;
    let f = Reflect::get(&clipboard, &name.into()).ok()?.dyn_into::<Function>().ok()?;
    Some((clipboard, f))
}

/// `navigator.clipboard.read()`: the first item with an image (or HTML with an inline one).
async fn read_image((clipboard, read): (JsValue, Function)) -> Result<Option<Rgba>, JsValue> {
    let promise: Promise = read.call0(&clipboard)?.dyn_into()?;
    let items: js_sys::Array = JsFuture::from(promise).await?.dyn_into()?;
    let blob_of = |item: &JsValue, ty: &str| -> Result<Promise, JsValue> {
        let get_type: Function = Reflect::get(item, &"getType".into())?.dyn_into()?;
        get_type.call1(item, &ty.into())?.dyn_into()
    };
    for item in items.iter() {
        let types: Vec<String> = Reflect::get(&item, &"types".into())
            .ok()
            .and_then(|t| t.dyn_into::<js_sys::Array>().ok())
            .map(|a| a.iter().filter_map(|t| t.as_string()).collect())
            .unwrap_or_default();
        let found = match clipboard_data::best_image_type(types.iter().map(String::as_str)) {
            Some((_, ty)) => {
                let blob: web_sys::Blob = JsFuture::from(blob_of(&item, ty)?).await?.dyn_into()?;
                let bytes = js_sys::Uint8Array::new(&JsFuture::from(blob.array_buffer()).await?).to_vec();
                Some((ty.to_string(), bytes))
            }
            None if types.iter().any(|t| t == "text/html") => {
                let blob: web_sys::Blob = JsFuture::from(blob_of(&item, "text/html")?).await?.dyn_into()?;
                let html = JsFuture::from(blob.text()).await?.as_string().unwrap_or_default();
                clipboard_data::html_inline_image(&html)
            }
            None => None,
        };
        if let Some((mime, bytes)) = found {
            return clipboard_data::decode(&mime, &bytes).map(Some).map_err(|e| JsValue::from(js_sys::Error::new(&e)));
        }
    }
    Ok(None)
}

/// `Services::clipboard_set_image`: Capishot copied (or cut) pixels; put them on the system
/// clipboard as PNG, for other apps. Never an error: where the browser can't take them, the copy
/// still pastes inside Capishot.
pub fn set(width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let ours = Rgba::new(width, height, rgba.to_vec()).ok_or("the copied pixels don't match their size")?;
    with(|s| {
        s.ours = Some(ours.clone());
        s.image = Some(ours);
        s.ours_written = false;
        s.fresh_at = None;
    });
    if let Err(e) = write_png(width, height, rgba) {
        let (denied, message) = describe(&e);
        log::warn!("photocraft-web: copying to the system clipboard: {message}");
        let first = with(|s| !std::mem::replace(&mut s.write_failed, true));
        if first && !denied {
            notice("The copy stays inside Capishot", vec![format!("The browser couldn't put it on the system clipboard: {message}")], false);
        }
    }
    Ok(())
}

/// `navigator.clipboard.write([new ClipboardItem({"image/png": blob})])`; the browser's answer
/// arrives later (`ours_written`).
fn write_png(width: u32, height: u32, rgba: &[u8]) -> Result<(), JsValue> {
    let (clipboard, write) = clipboard_method("write").ok_or_else(|| JsValue::from_str("this browser can't write images to the clipboard"))?;
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let item_class: Function =
        Reflect::get(&window, &"ClipboardItem".into())?.dyn_into().map_err(|_| JsValue::from_str("this browser can't write images to the clipboard"))?;
    let img = photocraft_codecs::Image::from_u8(width, height, photocraft_codecs::ChannelLayout::Rgba, rgba.to_vec())
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let png = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &photocraft_codecs::EncodeOptions::default())
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("image/png");
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&js_sys::Array::of1(&js_sys::Uint8Array::from(png.as_slice())), &opts)?;
    let record = js_sys::Object::new();
    Reflect::set(&record, &"image/png".into(), &blob)?;
    let item = Reflect::construct(&item_class, &js_sys::Array::of1(&record))?;
    let promise: Promise = write.call1(&clipboard, &js_sys::Array::of1(&item))?.dyn_into()?;
    wasm_bindgen_futures::spawn_local(async move {
        match JsFuture::from(promise).await {
            Ok(_) => with(|s| s.ours_written = s.ours.as_ref().is_some_and(|o| o.width == width && o.height == height)),
            Err(e) => {
                let (denied, message) = describe(&e);
                log::warn!("photocraft-web: copying to the system clipboard: {message}");
                let first = with(|s| !std::mem::replace(&mut s.write_failed, true));
                if first {
                    let why = if denied { "the browser didn't allow it".to_string() } else { message };
                    notice("The copy stays inside Capishot", vec![format!("It couldn't be put on the system clipboard for other apps: {why}.")], false);
                }
            }
        }
    });
    Ok(())
}

/// Before the app's raw input hook. `typing`: a text field has keyboard focus. The browser's
/// copy, cut and paste events give a text field its `Copy` / `Cut` / `Paste` events (the ⌘C / ⌘X
/// / ⌘V press itself does nothing there), so the presses are dropped here rather than made into
/// a second set by the app's key handling (which asks for the clipboard text again).
pub fn before_app(typing: bool, raw: &mut egui::RawInput) {
    let text = with(|s| {
        s.typing = typing;
        std::mem::take(&mut s.text)
    });
    raw.events.extend(text.into_iter().map(egui::Event::Paste));
    if typing {
        raw.events.retain(|e| {
            !matches!(e, egui::Event::Key { key: egui::Key::C | egui::Key::X | egui::Key::V, pressed: true, modifiers: m, .. }
                if m.command && !m.alt && !m.shift)
        });
    }
}

/// After the app's raw input hook: while a pasted image is read, its ⌘V press waits; when it
/// has arrived, the press (and a release) goes on to the app.
pub fn after_app(typing: bool, raw: &mut egui::RawInput) {
    with(|s| {
        if s.reading && !typing {
            let mut held = Vec::new();
            raw.events.retain(|e| {
                let paste = matches!(e, egui::Event::Key { key: egui::Key::V, pressed: true, modifiers: m, .. } if m.command);
                if paste {
                    held.push(e.clone());
                }
                !paste
            });
            s.held.extend(held);
        } else if !s.reading && !s.held.is_empty() {
            for e in std::mem::take(&mut s.held) {
                if let egui::Event::Key { key, physical_key, modifiers, .. } = e {
                    raw.events.push(e);
                    raw.events.push(egui::Event::Key { key, physical_key, pressed: false, repeat: false, modifiers });
                }
            }
        }
    });
}

/// A clipboard read finished since the last call: the shell resumes the paste that waited
/// (`PhotocraftApp::resume_clipboard_paste`).
pub fn take_finished() -> bool {
    with(|s| std::mem::take(&mut s.finished) && !s.reading)
}

/// Messages for the user since the last call.
pub fn take_notices() -> Vec<(String, Vec<String>, bool)> {
    with(|s| std::mem::take(&mut s.notices))
}

/// egui asks the window for the clipboard text when a text field's Edit › Paste (or its
/// right-click Paste) is chosen; eframe's web runner doesn't, so read it here.
struct TextPasteRequests;

impl egui::Plugin for TextPasteRequests {
    fn debug_name(&self) -> &'static str {
        "photocraft-web text paste"
    }

    fn output_hook(&mut self, _ctx: &egui::Context, output: &mut egui::FullOutput) {
        let mut asked = false;
        for v in output.viewport_output.values_mut() {
            v.commands.retain(|c| {
                let paste = matches!(c, egui::ViewportCommand::RequestPaste);
                asked |= paste;
                !paste
            });
        }
        if asked && let Some((clipboard, read_text)) = clipboard_method("readText") {
            let promise = read_text.call0(&clipboard).ok().and_then(|p| p.dyn_into::<Promise>().ok());
            let Some(promise) = promise else { return };
            wasm_bindgen_futures::spawn_local(async move {
                match JsFuture::from(promise).await {
                    Ok(text) => {
                        let text = text.as_string().unwrap_or_default().replace("\r\n", "\n");
                        if !text.is_empty() {
                            with(|s| s.text.push(text));
                            repaint();
                        }
                    }
                    Err(e) => {
                        let (denied, message) = describe(&e);
                        log::warn!("photocraft-web: navigator.clipboard.readText: {message}");
                        let title = if denied { "The browser didn't let Capishot read the clipboard" } else { "Couldn't read the clipboard" };
                        notice(title, vec![paste_key_hint()], !denied);
                    }
                }
            });
        }
    }
}
