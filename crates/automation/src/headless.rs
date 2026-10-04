//! Headless backend: a `photocraft_engine::Session` plus file I/O. Synchronous
//! and UI-free; the MCP server and the CLI both drive it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use photocraft_engine::{Session, command_specs};
use photocraft_format::PcraftWriter;
use photocraft_io::ExportOptions;
use serde_json::{Value, json};

use crate::{AutomationError, files};

/// A headless editing session.
#[derive(Default)]
pub struct Headless {
    pub session: Session,
    /// Incremental `.pcraft` writers per document id.
    writers: HashMap<u64, PcraftWriter>,
}

impl Headless {
    pub fn new() -> Self {
        Self::default()
    }

    fn doc_index(&self, index: Option<usize>) -> Result<usize, AutomationError> {
        match index {
            Some(i) if i < self.session.documents().len() => Ok(i),
            Some(i) => Err(AutomationError::BadRequest(format!("no document at index {i}"))),
            None => self.session.active_index().ok_or_else(|| AutomationError::BadRequest("no document open".into())),
        }
    }

    /// Open a file and make it the active document.
    pub fn open(&mut self, path: &Path) -> Result<Value, AutomationError> {
        let o = files::open(path)?;
        let index = self.session.add_document(o.document, Some(path.to_string_lossy().into_owned()));
        let d = &self.session.documents()[index];
        Ok(json!({
            "index": index,
            "name": d.doc.name,
            "width": d.doc.size.width,
            "height": d.doc.size.height,
            "layers": d.doc.layer_count(),
            "warnings": o.warnings,
        }))
    }

    /// Save (`.pcraft` or any export format by extension). With no path,
    /// saves to the document's own path.
    pub fn save(&mut self, index: Option<usize>, path: Option<&Path>, format: Option<&str>, opts: &ExportOptions) -> Result<Value, AutomationError> {
        let i = self.doc_index(index)?;
        let st = &self.session.documents()[i];
        let target: PathBuf = match (path, &st.path) {
            (Some(p), _) => p.to_path_buf(),
            (None, Some(p)) => PathBuf::from(p),
            (None, None) => {
                return Err(AutomationError::BadRequest("document has no path; pass `path`".into()));
            }
        };
        let doc = st.doc.clone();
        let writer = self.writers.entry(doc.id.0).or_default();
        let warnings = files::save(&doc, &target, format, opts, Some(writer))?;
        let is_native = format
            .map(|f| f.trim_start_matches('.').eq_ignore_ascii_case("pcraft"))
            .unwrap_or_else(|| target.extension().is_some_and(|e| e.eq_ignore_ascii_case("pcraft")));
        if is_native {
            self.session.set_active(i);
            if let Some(st) = self.session.active_mut() {
                st.saved_revision = st.revision;
                st.path = Some(target.to_string_lossy().into_owned());
            }
        }
        Ok(json!({ "path": target.to_string_lossy(), "warnings": warnings }))
    }

    pub fn inspect(&self, index: Option<usize>) -> Result<Value, AutomationError> {
        let i = self.doc_index(index)?;
        Ok(photocraft_engine::inspect::document(&self.session.documents()[i]))
    }

    pub fn render_png(&self, index: Option<usize>, max_side: u32) -> Result<Vec<u8>, AutomationError> {
        let i = self.doc_index(index)?;
        files::render_png(&self.session.documents()[i].doc, max_side)
    }

    pub fn session_list(&self) -> Value {
        photocraft_engine::inspect::session(&self.session)
    }

    pub fn select(&mut self, index: usize) -> Result<Value, AutomationError> {
        if self.session.set_active(index) { Ok(self.session_list()) } else { Err(AutomationError::BadRequest(format!("no document at index {index}"))) }
    }

    pub fn close(&mut self, index: Option<usize>) -> Result<Value, AutomationError> {
        let i = self.doc_index(index)?;
        if let Some(d) = self.session.close(i) {
            self.writers.remove(&d.doc.id.0);
        }
        Ok(self.session_list())
    }

    pub fn command_list(&self) -> Value {
        Value::Array(
            command_specs()
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "label": c.label,
                        "menu": c.menu,
                        "shortcut": c.shortcut,
                        "params": c.params,
                        "enabled": self.session.is_enabled(c.id),
                    })
                })
                .collect(),
        )
    }

    pub fn command_run(&mut self, id: &str, params: Value) -> Result<Value, AutomationError> {
        let params = if params.is_null() { json!({}) } else { params };
        Ok(self.session.execute(id, params)?)
    }
}
