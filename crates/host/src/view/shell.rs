//! The editor's services in a host app: decoding and encoding are built in; what needs the
//! computer (file pickers, writing files, opening links) goes to the host as a [`ViewEvent`].

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use photocraft_ui_egui::file_dialog::{FileDialogAnswer, FileDialogReply, FileDialogRequest};
use photocraft_ui_egui::Services;

use super::ViewEvent;

/// What the services asked of the host since the last frame.
#[derive(Clone, Default)]
pub(crate) struct Queue(Rc<RefCell<Vec<ViewEvent>>>);

impl Queue {
    fn push(&self, e: ViewEvent) {
        self.0.borrow_mut().push(e);
    }

    pub(crate) fn take(&self) -> Vec<ViewEvent> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

/// What a file picker asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileRequest {
    /// Files to read. Answer with [`FileDialog::contents`] (or [`FileDialog::paths`] natively).
    Open {
        /// Several may be chosen (File › Open); else one (Place, a preset…).
        multiple: bool,
        /// The extensions it can use, without dots; `None`: every format the editor opens.
        extensions: Option<Vec<String>>,
    },
    /// A folder to read from. Answer with [`FileDialog::folder`].
    Folder,
    /// Where to write, starting from `suggested`. Answer with [`FileDialog::save_to`]: the name
    /// comes back with [`ViewEvent::WriteFile`].
    Save {
        /// The document's file name.
        suggested: String,
    },
}

/// A file picker the editor asked for. The host shows its own and answers once, any time later;
/// dropping it unanswered cancels (the editor reports nothing).
#[derive(Clone)]
pub struct FileDialog {
    request: FileRequest,
    reply: Rc<RefCell<Option<FileDialogReply>>>,
}

impl std::fmt::Debug for FileDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileDialog").field("request", &self.request).finish()
    }
}

impl PartialEq for FileDialog {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.reply, &other.reply)
    }
}

impl FileDialog {
    /// What it asks for.
    pub fn request(&self) -> &FileRequest {
        &self.request
    }

    fn answer(&self, answer: Option<FileDialogAnswer>) {
        if let Some(reply) = self.reply.borrow_mut().take() {
            reply.send(answer);
        }
    }

    /// The chosen files' names and contents.
    pub fn contents(&self, files: Vec<(String, Vec<u8>)>) {
        self.answer(Some(FileDialogAnswer::ContentsMany(files)));
    }

    /// The chosen files' paths; the editor reads them (native).
    pub fn paths(&self, paths: Vec<String>) {
        self.answer(Some(FileDialogAnswer::Paths(paths)));
    }

    /// The name or path to save under.
    pub fn save_to(&self, name: impl Into<String>) {
        self.answer(Some(FileDialogAnswer::SaveTo(name.into())));
    }

    /// The chosen folder's path.
    pub fn folder(&self, path: impl Into<String>) {
        self.answer(Some(FileDialogAnswer::FolderTo(path.into())));
    }

    /// The person cancelled.
    pub fn cancel(&self) {
        self.answer(None);
    }

    /// No picker could be shown: `why` is reported to the person.
    pub fn unavailable(&self, why: impl Into<String>) {
        self.answer(Some(FileDialogAnswer::Unavailable(why.into())));
    }
}

/// The services the host didn't set: files through the host, decoding and encoding built in.
pub(crate) fn fill(services: &mut Services, queue: &Queue) {
    if services.import.is_none() {
        services.import = Some(Box::new(|name, bytes, opts| {
            photocraft_io::import_with_options(name, bytes, opts).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string())
        }));
    }
    if services.export.is_none() {
        services.export = Some(Box::new(|doc, path, settings| {
            photocraft_io::export(doc, path, &settings.io_options()).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string())
        }));
    }
    if services.encode_png.is_none() {
        services.encode_png = Some(Box::new(|w, h, rgba| {
            let image = photocraft_codecs::Image::from_u8(w, h, photocraft_codecs::ChannelLayout::Rgba, rgba.to_vec()).map_err(|e| e.to_string())?;
            photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &photocraft_codecs::EncodeOptions::default()).map_err(|e| e.to_string())
        }));
    }
    if services.file_dialog.is_none() {
        let queue = queue.clone();
        services.file_dialog = Some(Box::new(move |request, _parent, reply| {
            let request = match request {
                FileDialogRequest::Open { multiple, extensions, .. } => FileRequest::Open { multiple, extensions },
                FileDialogRequest::Folder { .. } => FileRequest::Folder,
                FileDialogRequest::Save { suggested } => FileRequest::Save { suggested },
            };
            queue.push(ViewEvent::FileDialog(FileDialog { request, reply: Rc::new(RefCell::new(Some(reply))) }));
        }));
    }
    if services.write.is_none() {
        let queue = queue.clone();
        services.write = Some(Box::new(move |name, bytes| {
            queue.push(ViewEvent::WriteFile { name: name.to_string(), bytes: Arc::new(bytes.to_vec()) });
            Ok(())
        }));
    }
    if services.open_url.is_none() {
        let queue = queue.clone();
        services.open_url = Some(Box::new(move |url| {
            queue.push(ViewEvent::OpenUrl(url.to_string()));
            Ok(())
        }));
    }
}
