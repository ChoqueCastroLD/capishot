//! Type › Load Fonts… and Load Font Folder… in the browser: the page's pickers, reading the
//! chosen font files one at a time into the app (`photocraft_ui_egui::user_fonts_ui::FontSink`).
//!
//! A folder comes from `showDirectoryPicker()` (Chromium), walked recursively; elsewhere (and with
//! `?fontpicker=input`) from `<input type=file webkitdirectory>`, which lists a folder's files
//! with their relative paths. Files that aren't fonts (by extension) are passed over silently;
//! fonts in formats that can't be read are listed with why.
use js_sys::{Function, Promise, Reflect, Uint8Array};
use photocraft_ui_egui::user_fonts_ui::FontSink;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

/// The picker input's element id (browser tests set files on it).
const INPUT_ID: &str = "capishot_font_input";
/// Files read from one pick at most.
const MAX_FILES: usize = 10_000;
/// Folders deep at most.
const MAX_DEPTH: usize = 16;
/// Files waiting for the app at most before the next one is read (memory stays bounded).
const BACKLOG: usize = 4;

fn js(e: JsValue) -> String {
    crate::indexed_presets::js(e)
}

/// Shows the picker; the files go to `sink`, then [`FontSink::done`].
pub fn pick(folder: bool, sink: FontSink) {
    let window = web_sys::window();
    let directory_picker = window.as_ref().and_then(|w| Reflect::get(w, &JsValue::from_str("showDirectoryPicker")).ok()).filter(|f| f.is_function());
    let forced_input = window.as_ref().and_then(|w| w.location().search().ok()).is_some_and(|q| q.contains("fontpicker=input"));
    match directory_picker {
        Some(f) if folder && !forced_input => {
            // Called now, inside the click's user activation; read on.
            let started = f.dyn_into::<Function>().map_err(|_| "no picker".to_string()).and_then(|f| f.call0(&JsValue::NULL).map_err(js));
            match started.and_then(|p| p.dyn_into::<Promise>().map_err(|_| "no promise".to_string())) {
                Ok(promise) => wasm_bindgen_futures::spawn_local(async move {
                    match JsFuture::from(promise).await {
                        Ok(handle) => {
                            if let Err(e) = walk_directory(handle, &sink).await {
                                log::error!("reading the font folder: {e}");
                            }
                            sink.done(false);
                        }
                        // AbortError: the user closed the picker.
                        Err(_) => sink.done(true),
                    }
                }),
                Err(e) => {
                    log::warn!("showDirectoryPicker: {e}; using the folder input");
                    input_picker(true, sink);
                }
            }
        }
        _ => input_picker(folder, sink),
    }
}

/// `<input type=file multiple>` (with `webkitdirectory` for a folder), added to the page while
/// it is open.
fn input_picker(folder: bool, sink: FontSink) {
    let started = (|| -> Result<(), String> {
        let document = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
        if let Some(old) = document.get_element_by_id(INPUT_ID) {
            old.remove();
        }
        let input: web_sys::HtmlInputElement = document.create_element("input").map_err(js)?.dyn_into().map_err(|_| "not an input")?;
        input.set_type("file");
        input.set_id(INPUT_ID);
        input.set_multiple(true);
        if folder {
            input.set_attribute("webkitdirectory", "").map_err(js)?;
        } else {
            let accept: Vec<String> =
                photocraft_ui_egui::user_fonts_ui::user_fonts::FONT_EXTENSIONS.iter().chain(&["woff2"]).map(|e| format!(".{e}")).collect();
            input.set_accept(&accept.join(","));
        }
        input.style().set_property("display", "none").map_err(js)?;
        document.body().ok_or("no body")?.append_child(&input).map_err(js)?;
        let changed = {
            let input = input.clone();
            let sink = sink.clone();
            Closure::once_into_js(move || {
                let files: Vec<web_sys::File> = input.files().map(|l| (0..l.length()).filter_map(|i| l.get(i)).collect()).unwrap_or_default();
                input.remove();
                wasm_bindgen_futures::spawn_local(async move {
                    read_files(files, &sink).await;
                    sink.done(false);
                });
            })
        };
        // Cancel leaves the (hidden) input on the page: Chromium also cancels a picker it won't
        // show (no user activation, e.g. a command from the embedding API), and files can still be
        // set on it then. The next pick replaces it.
        let cancelled = {
            let sink = sink.clone();
            Closure::once_into_js(move || sink.done(true))
        };
        input.add_event_listener_with_callback("change", changed.unchecked_ref()).map_err(js)?;
        input.add_event_listener_with_callback("cancel", cancelled.unchecked_ref()).map_err(js)?;
        input.click();
        Ok(())
    })();
    if let Err(e) = started {
        log::error!("font picker: {e}");
        sink.done(true);
    }
}

/// The name a file is reported under: its path inside the picked folder when there is one.
fn display_name(file: &web_sys::File) -> String {
    Reflect::get(file, &JsValue::from_str("webkitRelativePath")).ok().and_then(|v| v.as_string()).filter(|p| !p.is_empty()).unwrap_or_else(|| file.name())
}

async fn read_files(files: Vec<web_sys::File>, sink: &FontSink) {
    let mut n = 0;
    for file in files {
        let name = display_name(&file);
        if !photocraft_ui_egui::user_fonts_ui::user_fonts::is_font_file(&name) {
            continue;
        }
        n += 1;
        if n > MAX_FILES {
            sink.skipped(name, format!("more than {MAX_FILES} font files in one pick"));
            break;
        }
        read_one(&name, &file, sink).await;
    }
}

/// Reads one file into the sink, once the app has caught up with the ones before it.
async fn read_one(name: &str, file: &web_sys::Blob, sink: &FontSink) {
    while sink.backlog() >= BACKLOG {
        sleep(16).await;
    }
    if file.size() > photocraft_ui_egui::user_fonts_ui::user_fonts::MAX_FONT_BYTES as f64 {
        sink.skipped(name.to_string(), format!("larger than {} MB", photocraft_ui_egui::user_fonts_ui::user_fonts::MAX_FONT_BYTES >> 20));
        return;
    }
    match JsFuture::from(file.array_buffer()).await {
        Ok(buffer) => sink.file(name.to_string(), Uint8Array::new(&buffer).to_vec()),
        Err(e) => sink.skipped(name.to_string(), js(e)),
    }
}

async fn sleep(ms: i32) {
    let promise = Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(promise).await;
}

/// `handle[method](...args)`, awaited.
async fn call(handle: &JsValue, method: &str) -> Result<JsValue, String> {
    let f: Function = Reflect::get(handle, &JsValue::from_str(method)).map_err(js)?.dyn_into().map_err(|_| format!("no {method}()"))?;
    let v = f.call0(handle).map_err(js)?;
    match v.dyn_into::<Promise>() {
        Ok(p) => JsFuture::from(p).await.map_err(js),
        Err(v) => Ok(v),
    }
}

/// Walks a `FileSystemDirectoryHandle` (and its subfolders) for font files.
async fn walk_directory(root: JsValue, sink: &FontSink) -> Result<(), String> {
    let mut stack: Vec<(JsValue, String, usize)> = vec![(root, String::new(), 0)];
    let mut n = 0;
    while let Some((dir, prefix, depth)) = stack.pop() {
        let iterator = call(&dir, "values").await?;
        loop {
            let step = call(&iterator, "next").await?;
            if Reflect::get(&step, &JsValue::from_str("done")).ok().and_then(|d| d.as_bool()).unwrap_or(true) {
                break;
            }
            let entry = Reflect::get(&step, &JsValue::from_str("value")).map_err(js)?;
            let kind = Reflect::get(&entry, &JsValue::from_str("kind")).ok().and_then(|k| k.as_string()).unwrap_or_default();
            let name = Reflect::get(&entry, &JsValue::from_str("name")).ok().and_then(|k| k.as_string()).unwrap_or_default();
            let path = format!("{prefix}{name}");
            if kind == "directory" {
                if depth < MAX_DEPTH {
                    stack.push((entry, format!("{path}/"), depth + 1));
                }
            } else if kind == "file" && photocraft_ui_egui::user_fonts_ui::user_fonts::is_font_file(&name) {
                n += 1;
                if n > MAX_FILES {
                    sink.skipped(path, format!("more than {MAX_FILES} font files in one pick"));
                    return Ok(());
                }
                match call(&entry, "getFile").await.and_then(|f| f.dyn_into::<web_sys::Blob>().map_err(|_| "not a file".to_string())) {
                    Ok(file) => read_one(&path, &file, sink).await,
                    Err(e) => sink.skipped(path, e),
                }
            }
        }
    }
    Ok(())
}
