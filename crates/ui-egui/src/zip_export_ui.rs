//! File › Export › Export Open Documents as ZIP… (`file.export.openDocsZip`): a dialog to pick the
//! open documents (thumbnails, names, their order: drag a row or use its arrows), the image
//! format, an optional resize and sRGB conversion, how the pages are named and the ZIP's name;
//! then a background job (the progress card, Esc cancels) writes the ZIP as the pages are made:
//! to a file on the desktop (asked for with the save dialog), to a download growing a piece at a
//! time in the browser ([`crate::Services::download_stream`]). A cancelled export leaves nothing.
//! The settings are remembered. With parameters (agents, the embed) it runs at once.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{RichText, vec2};
use photocraft_engine::jobs::Started;
use photocraft_engine::zip_export_cmds::{self, ID};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;

/// Preferences › dialogs key of the remembered settings.
const PREF: &str = "zipExport";

/// One document row of the dialog.
#[derive(Clone, Debug)]
pub struct Row {
    pub doc: u64,
    pub name: String,
    pub checked: bool,
}

/// The dialog's state, and the export running.
#[derive(Default)]
pub struct State {
    pub open: bool,
    pub rows: Vec<Row>,
    pub format: String,
    pub quality: f32,
    pub lossless: bool,
    /// "none", "width" or "percent".
    pub resize: String,
    pub max_width: f32,
    pub percent: f32,
    pub srgb: bool,
    /// "keep" or "sequence".
    pub naming: String,
    pub prefix: String,
    pub zip_name: String,
    thumbs: HashMap<u64, egui::TextureHandle>,
    /// The thumbnail being made off the page's thread, and where it arrives. A chapter's pages
    /// take seconds each, so they come one at a time and the dialog is open meanwhile.
    thumb_job: Option<std::sync::mpsc::Receiver<(u64, photocraft_raster::Rgba8Image)>>,
    /// The running export's result, filled by its job.
    pending: Option<Pending>,
}

/// An export's report and, when collected in memory, its ZIP.
type Outcome = Option<Result<(Value, Option<Vec<u8>>), String>>;

struct Pending {
    name: String,
    result: Arc<wasm_sync::Mutex<Outcome>>,
}

fn settings(st: &State) -> Value {
    json!({
        "format": st.format, "quality": st.quality.round(), "lossless": st.lossless, "resize": st.resize,
        "maxWidth": st.max_width.round(), "percent": st.percent.round(), "srgb": st.srgb, "naming": st.naming, "prefix": st.prefix,
    })
}

/// The command's parameters for the dialog's choices.
pub fn params(st: &State) -> Value {
    let mut p = json!({
        "docs": st.rows.iter().filter(|r| r.checked).map(|r| r.doc).collect::<Vec<_>>(),
        "format": st.format,
        "naming": st.naming,
        "prefix": st.prefix,
        "srgb": st.srgb,
    });
    if st.format == "jpg" || st.format == "webp" {
        p["quality"] = json!(st.quality.round().clamp(1.0, 100.0));
    }
    if st.format == "webp" {
        p["lossless"] = json!(st.lossless);
    }
    match st.resize.as_str() {
        "width" => p["resize"] = json!({"maxWidth": st.max_width.round().max(1.0)}),
        "percent" => p["resize"] = json!({"percent": st.percent.round().clamp(1.0, 100.0)}),
        _ => {}
    }
    p
}

/// Opens the dialog: every open document checked, in tab order; the last settings.
pub fn open(app: &mut PhotocraftApp) {
    let stored = app.session.prefs().dialogs.get(PREF).cloned().unwrap_or(Value::Null);
    let s = |k: &str, d: &str| stored.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
    let f = |k: &str, d: f32| stored.get(k).and_then(Value::as_f64).map_or(d, |v| v as f32);
    let b = |k: &str| stored.get(k).and_then(Value::as_bool).unwrap_or(false);
    let rows = app.session.documents().iter().map(|d| Row { doc: d.doc.id.0, name: d.doc.name.clone(), checked: true }).collect();
    let zip_name = zip_export_cmds::default_zip_name(&app.session);
    let st = &mut app.zip_export;
    st.open = true;
    st.rows = rows;
    st.format = s("format", "png");
    st.quality = f("quality", 90.0);
    st.lossless = b("lossless");
    st.resize = s("resize", "none");
    st.max_width = f("maxWidth", 1600.0);
    st.percent = f("percent", 50.0);
    st.srgb = b("srgb");
    st.naming = s("naming", "keep");
    st.prefix = s("prefix", "");
    st.zip_name = zip_name;
    st.thumbs.clear();
}

/// The menu item and the command: the dialog without parameters; with them (agents, the embed)
/// the export at once (on the desktop a `path` runs the engine command as is).
pub fn invoke(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id != ID {
        return None;
    }
    if params.as_object().is_none_or(|o| o.is_empty()) {
        open(app);
        return Some(Ok(json!({"dialog": "zipExport"})));
    }
    let web = cfg!(target_arch = "wasm32");
    if !web && params.get("path").is_some() && !app.background_jobs {
        return None;
    }
    let name = params
        .get("path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| zip_export_cmds::default_zip_name(&app.session));
    if web || params.get("path").is_some() {
        return Some(start(app, params, &name));
    }
    let p = params.clone();
    Some(app.pick_save(&name, move |app, path| start(app, &p, &path)))
}

/// Starts the export job writing the ZIP to `target` (a file natively, a download's name on the
/// web).
pub fn start(app: &mut PhotocraftApp, params: &Value, target: &str) -> Result<Value, String> {
    if let Some(authorize) = app.session.authorize {
        authorize(ID, params).map_err(|e| e.to_string())?;
    }
    let export = zip_export_cmds::prepare(&app.session, params).map_err(|e| e.to_string())?;
    let target = if target.to_ascii_lowercase().ends_with(".zip") { target.to_string() } else { format!("{target}.zip") };
    let pages = export.pages.len();
    let web = cfg!(target_arch = "wasm32");
    // The browser's download grows a piece at a time; elsewhere without one the ZIP is written
    // once complete.
    let mut stream = match (&app.services.download_stream, web) {
        (Some(open), _) => Some(open(&target)?),
        _ => None,
    };
    let collect = web && stream.is_none();
    let path = target.clone();
    let result: Arc<wasm_sync::Mutex<Outcome>> = Arc::default();
    let out = Arc::clone(&result);
    let label = crate::i18n::fmt(tl!("Exporting {name}"), &[("name", &crate::file_open::display_name(&target))]);
    let started = app.session.start_job(
        ID,
        json!({"pages": pages}),
        &label,
        false,
        move |ctx| {
            let page = crate::i18n::t("Page");
            let mut step = |i: usize, n: usize| {
                ctx.progress(i as f32 / n.max(1) as f32, &format!("{page} {}/{n}", (i + 1).min(n)));
                ctx.check().is_ok()
            };
            let r = if let Some(s) = stream.as_mut() {
                export.run(&mut step, &mut |chunk| s.push(chunk).map_err(photocraft_engine::EngineError::Other)).and_then(|v| {
                    stream.take().map_or(Ok(()), |s| s.finish()).map_err(photocraft_engine::EngineError::Other)?;
                    Ok((v, None))
                })
            } else if collect {
                let mut zip = Vec::new();
                export
                    .run(&mut step, &mut |chunk| {
                        zip.extend(chunk);
                        Ok(())
                    })
                    .map(|v| (v, Some(zip)))
            } else {
                file_write(&export, &path, &mut step).map(|v| (v, None))
            };
            if ctx.check().is_err() {
                return Err(photocraft_engine::EngineError::Cancelled);
            }
            r
        },
        move |_, r: (Value, Option<Vec<u8>>)| {
            let v = r.0.clone();
            *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Ok(r));
            Ok(v)
        },
    );
    let job = started.map_err(|e| e.to_string())?;
    app.zip_export.pending = Some(Pending { name: target.clone(), result });
    if let Started::Done(_) = job {
        return Ok(finish(app).unwrap_or_else(|| json!({"path": target})));
    }
    Ok(json!({"pending": true, "path": target}))
}

#[cfg(not(target_arch = "wasm32"))]
fn file_write(export: &zip_export_cmds::ZipExport, path: &str, step: &mut dyn FnMut(usize, usize) -> bool) -> photocraft_engine::Result<Value> {
    zip_export_cmds::run_to_file(export, path, step)
}

#[cfg(target_arch = "wasm32")]
fn file_write(_: &zip_export_cmds::ZipExport, _: &str, _: &mut dyn FnMut(usize, usize) -> bool) -> photocraft_engine::Result<Value> {
    Err(photocraft_engine::EngineError::Other("no download service".into()))
}

/// An export ended: write a collected ZIP and report. Returns the result once it ran.
fn finish(app: &mut PhotocraftApp) -> Option<Value> {
    let p = app.zip_export.pending.as_ref()?;
    let r = p.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()?;
    let name = p.name.clone();
    app.zip_export.pending = None;
    match r {
        Ok((mut v, bytes)) => {
            if let Some(bytes) = bytes
                && let Some(write) = app.services.write.as_mut()
                && let Err(e) = write(&name, &bytes)
            {
                app.failed(e);
                return None;
            }
            v["path"] = json!(name);
            let files = v["files"].as_array().map_or(0, Vec::len);
            let failed: Vec<String> = v["failed"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| format!("{}: {}", f["document"].as_str().unwrap_or(""), f["error"].as_str().unwrap_or("")))
                .collect();
            app.ui.status =
                crate::i18n::fmt(tl!("Exported {count} pages to {name}"), &[("count", &files.to_string()), ("name", &crate::file_open::display_name(&name))]);
            app.ui.status_error = false;
            if !failed.is_empty() {
                let id = crate::notices::post(app, "Some pages couldn't be exported", failed, true, None);
                let _ = id;
            }
            Some(v)
        }
        Err(e) => {
            app.failed(e);
            None
        }
    }
}

/// An export that ended reports (each frame).
pub fn tick(app: &mut PhotocraftApp) {
    if app.zip_export.pending.is_some() {
        finish(app);
        if app.zip_export.pending.as_ref().is_some_and(|p| Arc::strong_count(&p.result) == 1) {
            // The job ended without a result (cancelled or failed; the job card says which).
            app.zip_export.pending = None;
        }
    }
}

/// The thumbnails the dialog still lacks, one at a time off the page's thread.
fn thumbnails(app: &mut PhotocraftApp, ctx: &egui::Context) {
    use std::sync::mpsc::TryRecvError;
    if let Some(rx) = &app.zip_export.thumb_job {
        match rx.try_recv() {
            Ok((id, t)) => {
                let img = egui::ColorImage::from_rgba_unmultiplied([t.width as usize, t.height as usize], &t.pixels);
                let tex = ctx.load_texture(format!("zip-thumb-{id}"), img, egui::TextureOptions::LINEAR);
                app.zip_export.thumbs.insert(id, tex);
                app.zip_export.thumb_job = None;
            }
            Err(TryRecvError::Disconnected) => app.zip_export.thumb_job = None,
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
        }
    }
    // While an export runs the machine is busy with the pages themselves.
    if app.zip_export.pending.is_some() {
        return;
    }
    let next = app.session.documents().iter().find(|d| !app.zip_export.thumbs.contains_key(&d.doc.id.0)).map(|d| (d.doc.id.0, d.doc.clone()));
    let Some((id, doc)) = next else { return };
    let (tx, rx) = std::sync::mpsc::channel();
    app.zip_export.thumb_job = Some(rx);
    let repaint = ctx.clone();
    crate::jobs_ui::off_thread(move || {
        let _ = tx.send((id, photocraft_compose::thumbnail(&doc, 96)));
        repaint.request_repaint();
    });
    ctx.request_repaint_after(std::time::Duration::from_millis(50));
}

/// Each frame: [`tick`]; the dialog.
pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    tick(app);
    if !app.zip_export.open {
        return;
    }
    // Rows for documents closed meanwhile go; the thumbnails are made once.
    let open_ids: Vec<u64> = app.session.documents().iter().map(|d| d.doc.id.0).collect();
    app.zip_export.rows.retain(|r| open_ids.contains(&r.doc));
    thumbnails(app, ctx);
    let screen = crate::safe_area::rect(ctx);
    let phone = screen.width() < 600.0;
    let mut open = true;
    let mut window = egui::Window::new(tl!("Export Open Documents as ZIP")).id(egui::Id::unique("zip-export-window")).open(&mut open).collapsible(false);
    window = if phone { window.fixed_rect(screen.shrink(4.0)).resizable(false) } else { window.default_size([520.0, 640.0]).resizable(true) };
    let mut go = false;
    let mut cancel = false;
    window.show(ctx, |ui| {
        let t = Tokens::get(ui.ctx());
        let st = &mut app.zip_export;
        let bottom = 44.0;
        egui::ScrollArea::vertical().id_salt("zip-export-scroll").max_height(ui.available_height() - bottom).auto_shrink([false, true]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tl!("Documents")).font(crate::theme::semibold(13.0)));
                if ui.small_button(tl!("All")).clicked() {
                    st.rows.iter_mut().for_each(|r| r.checked = true);
                }
                if ui.small_button(tl!("None")).clicked() {
                    st.rows.iter_mut().for_each(|r| r.checked = false);
                }
            });
            ui.label(RichText::new(tl!("Drag a row, or use its arrows, to change the order of the pages")).small().color(t.text_faint));
            let n = st.rows.len();
            let mut moved: Option<(usize, usize)> = None;
            for i in 0..n {
                let row_id = egui::Id::unique(("zip-row", st.rows[i].doc));
                let resp = ui
                    .dnd_drag_source(row_id, i, |ui| {
                        ui.horizontal(|ui| {
                            let r = &mut st.rows[i];
                            if let Some(tex) = st.thumbs.get(&r.doc) {
                                let s = tex.size_vec2();
                                let k = 40.0 / s.x.max(s.y).max(1.0);
                                ui.add(egui::Image::new((tex.id(), s * k)));
                            } else {
                                // Its thumbnail is on the way.
                                ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
                            }
                            let label = format!("{}. {}", i + 1, r.name);
                            crate::widgets::checkbox(ui, &mut r.checked, &label);
                        });
                    })
                    .response;
                if let Some(from) = resp.dnd_release_payload::<usize>() {
                    moved = Some((*from, i));
                }
                // The arrows, at the row's right.
                let rr = resp.rect;
                let up = egui::Rect::from_min_size(egui::pos2(ui.max_rect().right() - 64.0, rr.top()), vec2(30.0, rr.height().max(28.0)));
                let down = up.translate(vec2(32.0, 0.0));
                if i > 0 && ui.put(up, egui::Button::new("▲")).clicked() {
                    moved = Some((i, i - 1));
                }
                if i + 1 < n && ui.put(down, egui::Button::new("▼")).clicked() {
                    moved = Some((i, i + 1));
                }
            }
            if let Some((from, to)) = moved
                && from != to
                && from < n
                && to < n
            {
                let r = st.rows.remove(from);
                st.rows.insert(to, r);
            }
            crate::widgets::hairline(ui);
            ui.label(RichText::new(tl!("Format")).font(crate::theme::semibold(13.0)));
            ui.horizontal_wrapped(|ui| {
                for (k, l) in [("png", "PNG"), ("jpg", "JPG"), ("webp", "WebP"), ("psd", tl!("PSD (with layers)"))] {
                    if ui.selectable_label(st.format == k, l).clicked() {
                        st.format = k.into();
                    }
                }
            });
            if st.format == "webp" {
                crate::widgets::checkbox(ui, &mut st.lossless, tl!("Lossless"));
            }
            if st.format == "psd" {
                ui.label(RichText::new(tl!("Each document saved with its layers, as Save does. Size and sRGB apply to all its layers.")).small().color(t.text_faint));
            }
            if st.format == "jpg" || (st.format == "webp" && !st.lossless) {
                ui.horizontal(|ui| {
                    ui.label(tl!("Quality"));
                    crate::widgets::value_field(ui, &mut st.quality, 1.0..=100.0, "", 50.0);
                });
            }
            crate::widgets::hairline(ui);
            ui.label(RichText::new(tl!("Size")).font(crate::theme::semibold(13.0)));
            ui.horizontal_wrapped(|ui| {
                for (k, l) in [("none", tl!("Original")), ("width", tl!("Max width")), ("percent", tl!("Percent"))] {
                    if ui.selectable_label(st.resize == k, l).clicked() {
                        st.resize = k.into();
                    }
                }
            });
            match st.resize.as_str() {
                "width" => {
                    ui.horizontal(|ui| {
                        crate::widgets::value_field(ui, &mut st.max_width, 1.0..=100_000.0, "px", 70.0);
                    });
                }
                "percent" => {
                    ui.horizontal(|ui| {
                        crate::widgets::value_field(ui, &mut st.percent, 1.0..=100.0, "%", 60.0);
                    });
                }
                _ => {}
            }
            crate::widgets::checkbox(ui, &mut st.srgb, tl!("Convert to sRGB"));
            crate::widgets::hairline(ui);
            ui.label(RichText::new(tl!("File names")).font(crate::theme::semibold(13.0)));
            ui.horizontal_wrapped(|ui| {
                if ui.selectable_label(st.naming == "keep", tl!("Document names")).clicked() {
                    st.naming = "keep".into();
                }
                if ui.selectable_label(st.naming == "sequence", tl!("Numbered")).clicked() {
                    st.naming = "sequence".into();
                }
            });
            if st.naming == "sequence" {
                ui.horizontal(|ui| {
                    let l = ui.label(tl!("Prefix"));
                    ui.add(egui::TextEdit::singleline(&mut st.prefix).desired_width(120.0).hint_text(tl!("none"))).labelled_by(l.id);
                });
            }
            let names: Vec<String> = st.rows.iter().filter(|r| r.checked).map(|r| r.name.clone()).collect();
            let ext = if st.format == "jpg" { "jpg" } else { st.format.as_str() };
            let preview = zip_export_cmds::entry_names(&names, ext, st.naming == "sequence", &st.prefix);
            let shown: Vec<&str> = preview.iter().take(3).map(String::as_str).collect();
            let more = if preview.len() > 3 { ", …" } else { "" };
            ui.label(RichText::new(format!("{}{more}", shown.join(", "))).small().color(t.text_dim));
            crate::widgets::hairline(ui);
            ui.horizontal(|ui| {
                let l = ui.label(tl!("ZIP name"));
                ui.add(egui::TextEdit::singleline(&mut st.zip_name).desired_width(180.0)).labelled_by(l.id);
            });
        });
        ui.add_space(6.0);
        let count = app.zip_export.rows.iter().filter(|r| r.checked).count();
        ui.horizontal(|ui| {
            let export = ui.add_enabled(
                count > 0,
                egui::Button::new(crate::i18n::fmt(tl!("Export {count}"), &[("count", &count.to_string())])).min_size(vec2(110.0, 30.0)),
            );
            note(ui.ctx(), "export", export.rect);
            if export.clicked() {
                go = true;
            }
            let c = ui.add(egui::Button::new(tl!("Cancel")).min_size(vec2(90.0, 30.0)));
            note(ui.ctx(), "cancel", c.rect);
            if c.clicked() {
                cancel = true;
            }
        });
    });
    if !open || cancel {
        app.zip_export.open = false;
    }
    if go {
        app.zip_export.open = false;
        let new = settings(&app.zip_export);
        if app.session.prefs().dialogs.get(PREF) != Some(&new) {
            app.session.edit_prefs(|p| {
                p.dialogs.insert(PREF.into(), new);
            });
            let _ = crate::prefs_ui::save_preferences(app);
        }
        let p = params(&app.zip_export);
        let name = app.zip_export.zip_name.trim().to_string();
        let name = if name.is_empty() { zip_export_cmds::default_zip_name(&app.session) } else { name };
        // The save dialog natively; in the browser a file to write where it can, else the
        // download's name.
        let name = if name.to_ascii_lowercase().ends_with(".zip") { name } else { format!("{name}.zip") };
        let r = app.pick_save(&name, move |app, path| start(app, &p, &path));
        if let Err(e) = r {
            app.failed(e);
        }
    }
}

fn note(ctx: &egui::Context, what: &str, r: egui::Rect) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique(("zip-export-rect", what)), r));
}

/// Where the dialog's button `what` (`export`, `cancel`) was drawn last, for tests and automation.
pub fn noted(ctx: &egui::Context, what: &str) -> Option<egui::Rect> {
    ctx.data(|d| d.get_temp(egui::Id::unique(("zip-export-rect", what))))
}
