//! Page files (TypeR's "Open PSD directory", N4 and N5): the chapter's PSD / PNG / JPG pages
//! are chosen once, and the current line's page opens (or comes to the front) as the cursor moves
//! to another page.
//!
//! - **Mapping** is the engine's (`typer.images.set`, `State::current_image`): page N is the file
//!   whose name ends in the number N (`chap016_023.psd` → 23), else the N-th file in natural order.
//! - **Files.** The desktop dialog answers paths, which are the page file names the engine keeps
//!   (so the sync survives a restart). The browser answers contents: they stay in memory in
//!   [`PageFiles`], by name, up to [`MAX_PAGE_BYTES`]; after a reload the names are still there
//!   but the files must be chosen again (the footer's dot turns amber).
//! - **Sync** ([`sync`], every frame): when the current page's file differs from the last one
//!   opened for TypeR (`typer.images.opened`), its document is activated when it's open, else
//!   opened. With *Close the previous page* on, the previous page's document is closed through
//!   File › Close, so unsaved changes get Photoshop's Save / Don't Save / Cancel prompt (TypeR
//!   saves silently; Capishot never does).

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::file_dialog::{FileDialogAnswer, FileDialogRequest};
use crate::file_open::display_name;

/// Most bytes of page files kept in memory for a TypeR workspace (the browser hands over
/// contents; the desktop keeps paths and holds nothing).
pub const MAX_PAGE_BYTES: usize = 1 << 30;

/// The page file types TypeR picks (`.psd .png .jpg .jpeg`), plus the other formats Capishot
/// opens that scanlation teams use for pages.
pub const PAGE_EXTS: [&str; 8] = ["psd", "psb", "png", "jpg", "jpeg", "webp", "tif", "tiff"];

/// Page files picked in the browser: contents by work tab, then by page file name (two
/// chapters may both have a `001.psd`). The cap is for all tabs together.
#[derive(Clone, Default)]
pub struct PageFiles {
    by_tab: BTreeMap<String, BTreeMap<String, Arc<Vec<u8>>>>,
    total: usize,
    /// The document each (tab, page file) opened as, so two chapters' `001.psd` stay apart.
    claims: BTreeMap<(String, String), photocraft_doc::DocId>,
}

impl std::fmt::Debug for PageFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageFiles").field("tabs", &self.by_tab.len()).field("total", &self.total).finish()
    }
}

impl PartialEq for PageFiles {
    fn eq(&self, other: &Self) -> bool {
        self.total == other.total
            && self.by_tab.iter().map(|(t, f)| (t, f.keys().collect::<Vec<_>>())).eq(other.by_tab.iter().map(|(t, f)| (t, f.keys().collect::<Vec<_>>())))
    }
}

impl PageFiles {
    /// Bytes held.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Is tab `tab`'s page file `image` available to open (in memory, or a path on the desktop)?
    pub fn available(&self, tab: &str, image: &str) -> bool {
        self.get(tab, image).is_some() || is_local_path(image)
    }

    fn get(&self, tab: &str, image: &str) -> Option<&Arc<Vec<u8>>> {
        self.by_tab.get(tab)?.get(image)
    }

    /// Drop tab `tab`'s files.
    pub fn clear_tab(&mut self, tab: &str) {
        if let Some(files) = self.by_tab.remove(tab) {
            self.total = self.total.saturating_sub(files.values().map(|b| b.len()).sum());
        }
    }

    /// Drop the files of tabs that are gone.
    fn retain_tabs(&mut self, tabs: &[photocraft_typer::Tab]) {
        let gone: Vec<String> = self.by_tab.keys().filter(|k| !tabs.iter().any(|t| &t.id == *k)).cloned().collect();
        for k in gone {
            self.clear_tab(&k);
        }
    }

    /// Keep `bytes` as tab `tab`'s page file `name` unless the store would go over `cap` bytes.
    fn insert(&mut self, tab: &str, name: String, bytes: Vec<u8>, cap: usize) -> bool {
        let Some(total) = self.total.checked_add(bytes.len()).filter(|t| *t <= cap) else { return false };
        self.total = total;
        if let Some(old) = self.by_tab.entry(tab.to_string()).or_default().insert(name, Arc::new(bytes)) {
            self.total = self.total.saturating_sub(old.len());
        }
        true
    }
}

/// An absolute path the desktop can read (never on the web, where names are not paths).
fn is_local_path(image: &str) -> bool {
    !cfg!(target_arch = "wasm32") && std::path::Path::new(image).is_absolute()
}

/// Is `name` a page file (by extension)?
pub fn is_page_file(name: &str) -> bool {
    std::path::Path::new(name).extension().and_then(|e| e.to_str()).is_some_and(|e| PAGE_EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// Choose the chapter's page files (footer button). The current page's file opens at once.
pub fn pick(app: &mut PhotocraftApp) {
    let initial_dir = crate::file_dialog::last_used_dir(&app.ui.recent_files);
    let extensions = Some(PAGE_EXTS.iter().map(|e| (*e).to_string()).collect());
    let r = app.ask_file(FileDialogRequest::Open { multiple: true, initial_dir, extensions }, |app, answer| {
        let (names, files) = match answer {
            FileDialogAnswer::Paths(paths) => (paths, Vec::new()),
            FileDialogAnswer::Contents(name, bytes) => (vec![name.clone()], vec![(name, bytes)]),
            FileDialogAnswer::ContentsMany(files) => (files.iter().map(|(n, _)| n.clone()).collect(), files),
            FileDialogAnswer::SaveTo(_) | FileDialogAnswer::FolderTo(_) | FileDialogAnswer::Unavailable(_) => return Err("the file dialog gave an unexpected answer".into()),
        };
        set_files(app, names, files)
    });
    if let Err(e) = r
        && e != crate::file_dialog::CANCELLED
    {
        app.ui.typer.message = e;
    }
}

/// Use `names` as the page files, with the contents of those the browser handed over. Files
/// that aren't pages are skipped; contents past [`MAX_PAGE_BYTES`] aren't kept (in natural
/// order, so the first pages stay openable) and the message says so.
pub fn set_files(app: &mut PhotocraftApp, names: Vec<String>, files: Vec<(String, Vec<u8>)>) -> Result<Value, String> {
    set_files_capped(app, names, files, MAX_PAGE_BYTES)
}

fn set_files_capped(app: &mut PhotocraftApp, names: Vec<String>, mut files: Vec<(String, Vec<u8>)>, cap: usize) -> Result<Value, String> {
    let names: Vec<String> = names.into_iter().filter(|n| is_page_file(n)).collect();
    if names.is_empty() {
        let e = tl!("No page files were chosen: pick the chapter's PSD, PNG or JPG files.").to_string();
        app.ui.typer.message = e.clone();
        return Err(e);
    }
    files.retain(|(n, _)| is_page_file(n));
    files.sort_by(|a, b| photocraft_typer::pages::natural_cmp(&a.0, &b.0));
    let r = super::run(app, "typer.images.set", json!({"images": names}))?;
    // The tab's earlier files go; the new ones fit beside the other tabs'.
    let tab = app.session.typer.state.current_tab_id.clone();
    let store = &mut app.ui.typer.page_files;
    store.clear_tab(&tab);
    let mut dropped = 0usize;
    for (name, bytes) in files {
        if !store.insert(&tab, name, bytes, cap) {
            dropped += 1;
        }
    }
    if dropped > 0 {
        app.ui.typer.message = format!(
            "{} ({dropped}): {}",
            tl!("Some page files are too big to keep"),
            tl!("the browser keeps up to 1 GB of pages. Choose the remaining pages again when you get to them.")
        );
    }
    Ok(r)
}

/// Stop syncing page files (TypeR's "Desync PSDs"); open documents stay open.
pub fn desync(app: &mut PhotocraftApp) {
    super::act(app, "typer.images.clear", json!({}));
    let tab = app.session.typer.state.current_tab_id.clone();
    app.ui.typer.page_files.clear_tab(&tab);
}

/// The open document showing the current tab's page file `image`: the one it opened as before,
/// else the one read from that path, else one with its file name that no other page file claims.
/// A match by path or name is claimed for this tab and file.
fn find_doc(app: &mut PhotocraftApp, image: &str) -> Option<usize> {
    let tab = app.session.typer.state.current_tab_id.clone();
    let key = (tab, image.to_string());
    let docs = app.session.documents();
    let files = &mut app.ui.typer.page_files;
    files.claims.retain(|_, id| docs.iter().any(|d| d.doc.id == *id));
    if let Some(id) = files.claims.get(&key) {
        return docs.iter().position(|d| d.doc.id == *id);
    }
    let name = display_name(image);
    let claimed = |id: photocraft_doc::DocId| files.claims.iter().any(|(k, c)| *c == id && *k != key);
    let i = docs
        .iter()
        .position(|d| d.path.as_deref() == Some(image) && !claimed(d.doc.id))
        .or_else(|| docs.iter().position(|d| d.doc.name == name && !claimed(d.doc.id)))?;
    if let Some(d) = docs.get(i) {
        files.claims.insert(key, d.doc.id);
    }
    Some(i)
}

/// Bring page file `image` to the front: activate its document, else open it.
pub fn open_page(app: &mut PhotocraftApp, image: &str) -> Result<(), String> {
    if let Some(i) = find_doc(app, image) {
        return app.run("document.activate", json!({"document": i})).map(|_| ());
    }
    let name = display_name(image);
    // Already opening in the background.
    if app.jobs.opens.iter().any(|o| o.name == name || o.path.as_deref() == Some(image)) {
        return Ok(());
    }
    let tab = app.session.typer.state.current_tab_id.clone();
    if let Some(bytes) = app.ui.typer.page_files.get(&tab, image).cloned() {
        app.open_bytes(&name, &bytes)?;
    } else if is_local_path(image) {
        app.open_path(image)?;
    } else {
        return Err(tl!("this page file isn't loaded any more: choose the page files again").into());
    }
    // Opened at once (not in the background): the new document is the active one.
    if let Some(d) = app.session.active().filter(|d| d.doc.name == name) {
        let id = d.doc.id;
        app.ui.typer.page_files.claims.insert((tab, image.to_string()), id);
    }
    Ok(())
}

/// Open the current page's file when the cursor reaches another page (see the module docs).
/// Waits while a dialog or prompt is up or a button is held.
pub fn sync(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let st = &app.session.typer.state;
    if app.ui.typer.page_files.total > 0 {
        app.ui.typer.page_files.retain_tabs(&st.tabs);
    }
    if st.images.is_empty() {
        return;
    }
    let Some(image) = st.current_image().and_then(|i| st.images.get(i)).cloned() else { return };
    if st.last_opened_image.as_deref() == Some(image.as_str()) {
        return;
    }
    if app.file_dialog_open() || app.discard.is_some() || ctx.input(|i| i.pointer.any_down()) {
        return;
    }
    let previous = st.last_opened_image.clone();
    let auto_close = st.settings.auto_close_psd;
    // Recorded first: a page that fails to open is reported once, not every frame.
    if let Err(e) = app.run("typer.images.opened", json!({"image": image})) {
        app.ui.typer.message = e;
        return;
    }
    if let Err(e) = open_page(app, &image) {
        app.ui.typer.message = format!("{}: {e}", display_name(&image));
        return;
    }
    if auto_close
        && let Some(prev) = previous.filter(|p| *p != image)
        && let Some(i) = find_doc(app, &prev)
        && find_doc(app, &image) != Some(i)
    {
        // File › Close: an unsaved page asks Save / Don't Save / Cancel first.
        if let Err(e) = crate::menus::invoke(app, ctx, "file.close", json!({"document": i})) {
            app.ui.typer.message = format!("{}: {e}", display_name(&prev));
        }
    }
}

/// The footer's page-file control: a button to choose the files, then a chip with the sync dot
/// (green: every file can open; amber: some must be chosen again) and a menu.
pub(super) fn footer(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let st = &app.session.typer.state;
    if st.images.is_empty() {
        if crate::icons::button_with_icon_size(ui, "image", 24.0, 15.0, false, tl!("Choose the chapter's page files (PSD, PNG, JPG)…")).clicked() {
            pick(app);
        }
        return;
    }
    let files = &app.ui.typer.page_files;
    // Not missing: in memory, a path on the desktop, or already open.
    let docs = app.session.documents();
    let open = |i: &str| docs.iter().any(|d| d.path.as_deref() == Some(i) || d.doc.name == display_name(i));
    let missing = st.images.iter().filter(|i| !files.available(&st.current_tab_id, i) && !open(i)).count();
    let current = st.current_image().and_then(|i| st.images.get(i)).map(|s| display_name(s));
    let n = st.images.len();
    let label = crate::i18n::trn(crate::i18n::current(), n as u64, "{n} page", "{n} pages");
    let dot = if missing == 0 { t.accent } else { t.warning };
    let galley = ui.painter().layout_no_wrap(label.clone(), egui::FontId::proportional(11.0), t.text_dim);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(galley.size().x + 22.0, 18.0), egui::Sense::click());
    ui.painter().rect_filled(rect, 9.0, if resp.hovered() { t.hover } else { t.field });
    ui.painter().circle_filled(egui::pos2(rect.left() + 9.0, rect.center().y), 3.5, dot);
    ui.painter().galley(egui::pos2(rect.left() + 16.0, rect.center().y - galley.size().y / 2.0), galley, t.text_dim);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    let mut tip = match &current {
        Some(c) => format!("{}: {c}", tl!("This page's file")),
        None => tl!("No file for this page").to_string(),
    };
    if missing > 0 {
        tip.push('\n');
        tip.push_str(&format!("{} ({missing})", tl!("Not loaded: choose the page files again")));
    }
    let resp = resp.on_hover_text(tip);
    let mut action: Option<u8> = None;
    egui::Popup::menu(&resp).show(|ui| {
        if ui.add_enabled(current.is_some(), egui::Button::new(tl!("Open this page's file"))).clicked() {
            action = Some(0);
        }
        if ui.button(tl!("Choose page files…")).clicked() {
            action = Some(1);
        }
        if ui.button(tl!("Stop syncing page files")).clicked() {
            action = Some(2);
        }
    });
    match action {
        Some(0) => {
            let st = &app.session.typer.state;
            if let Some(image) = st.current_image().and_then(|i| st.images.get(i)).cloned() {
                match open_page(app, &image) {
                    Ok(()) => {
                        let _ = app.run("typer.images.opened", json!({"image": image}));
                    }
                    Err(e) => app.ui.typer.message = format!("{}: {e}", display_name(&image)),
                }
            }
        }
        Some(1) => pick(app),
        Some(2) => desync(app),
        _ => {}
    }
}

#[cfg(test)]
#[path = "pages_tests.rs"]
mod tests;
