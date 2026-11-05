//! The contract's steps, written against `capishot_host` alone (no egui): the app's buttons call
//! them and `tests/contract.rs` runs them headless. Every step returns `capishot_host::Result`; a
//! failure is shown to the person, never a panic.

use capishot_host::{
    DocumentId, Error, Event, ExecOptions, GetPixels, Host, InspectOptions, JobEnd, JobHandle, LayerId, OpenOptions, Result, Started,
};
use serde_json::{Value, json};

/// The sample page's size: no PSD is committed, the page is built here and goes through PSD bytes
/// like a page from the host's server would.
pub const PAGE: (u32, u32) = (320, 240);
/// `[x, y, width, height]` of the red blob on the sample page (what Remove with AI erases).
pub const BLOB: [i32; 4] = [120, 90, 48, 36];

/// A one-layer PSD (white page, red blob), as bytes.
///
/// Built in a scratch document that is closed again, so the host sees only the page it opens.
pub fn sample_psd(host: &mut Host) -> Result<Vec<u8>> {
    host.execute("file.new", json!({"width": PAGE.0, "height": PAGE.1, "background": "white", "name": "sample.psd"}))?;
    let scratch = host.active_document().ok_or(Error::NoActiveDocument)?;
    let on = ExecOptions::on(scratch);
    let built = (|| {
        host.execute_with("select.rect", json!({"x": BLOB[0], "y": BLOB[1], "width": BLOB[2], "height": BLOB[3]}), on)?;
        host.execute_with("edit.fill", json!({"contents": "color", "color": "#e02020"}), on)?;
        host.execute_with("select.deselect", json!({}), on)?;
        host.save_psd(scratch)
    })();
    // Closed whether or not the page was built (an error is the one to report).
    let closed = host.close(scratch, true);
    let bytes = built?;
    closed?;
    Ok(bytes)
}

/// Step 1: open a PSD from bytes and show it (`name` only names the document; PSD bytes are
/// recognised by their signature). Returns the document and the import warnings.
pub fn open(host: &mut Host, name: &str, bytes: Vec<u8>) -> Result<(DocumentId, Vec<String>)> {
    let opened = host.open(bytes, OpenOptions::named(name))?;
    host.show(opened.document, false)?;
    Ok((opened.document, opened.warnings))
}

/// Step 2: the host picks the editor's tool (the embed API's tool ids: `removeAi`, `brush`,
/// `eraser`, `cloneStamp`, `eyedropper`, `marquee`…).
pub fn set_tool(host: &mut Host, tool: &str) -> Result<()> {
    host.set_tool(tool)
}

/// A stroke across the blob, as the Remove with AI tool records one (`[x, y, pressure]`).
pub fn blob_stroke() -> Value {
    let y = BLOB[1] + BLOB[3] / 2;
    let points: Vec<Value> = (BLOB[0] - 8..=BLOB[0] + BLOB[2] + 8).step_by(4).map(|x| json!([x, y, 1.0])).collect();
    json!({"strokes": [points], "size": BLOB[3] + 16})
}

/// How step 3 ran.
#[derive(Clone, Debug, PartialEq)]
pub enum Removal {
    /// Remove with AI (Manga LaMa) runs as a background job: wait for its `JobFinished`.
    Job(JobHandle),
    /// Remove with AI finished inline (a build that runs jobs inline); its result.
    Done(Value),
    /// No AI on this build or device (`edit.removeAi` disabled): the plain Remove tool
    /// (`paint.remove`) ran instead, at once, so the contract's remove-then-undo still runs.
    Plain(Value),
}

/// The layer Remove paints on: the selected layer when it holds pixels, else the first pixel
/// layer (a real page often opens with a type layer selected, where Remove is disabled).
pub fn paint_target(host: &Host, doc: DocumentId) -> Result<Option<LayerRow>> {
    let rows = layers(host, doc)?;
    let selected = rows.iter().find(|r| r.selected && r.kind == "pixel");
    Ok(selected.or_else(|| rows.iter().find(|r| r.kind == "pixel")).cloned())
}

/// Step 3: Remove with AI over the blob, as one undo step, on [`paint_target`] (selected first
/// when it isn't). Starts a job; without AI here, falls back to the non-AI Remove so undo has
/// something to undo. `allow_download` lets the engine fetch Manga LaMa (about 100 MB) when it
/// isn't cached.
pub fn remove_ai(host: &mut Host, doc: DocumentId, allow_download: bool) -> Result<Removal> {
    if let Some(target) = paint_target(host, doc)?
        && !target.selected
    {
        select(host, doc, target.id)?;
    }
    let mut p = blob_stroke();
    p["allowDownload"] = json!(allow_download);
    match host.start("edit.removeAi", p, Some(doc)) {
        Ok(Started::Job(job)) => Ok(Removal::Job(job)),
        Ok(Started::Done(v)) => Ok(Removal::Done(v)),
        Err(Error::Disabled { .. }) => Ok(Removal::Plain(host.execute_with("paint.remove", blob_stroke(), ExecOptions::on(doc))?)),
        Err(e) => Err(e),
    }
}

/// The end of `job` among `events`, if it is there.
pub fn job_end(events: &[Event], job: JobHandle) -> Option<JobEnd> {
    events.iter().find_map(|e| match e {
        Event::JobFinished { job: j, end, .. } if *j == job => Some(end.clone()),
        _ => None,
    })
}

/// Step 4: undo the last step (Remove with AI is exactly one). False when there was nothing to
/// undo.
pub fn undo(host: &mut Host, doc: DocumentId) -> Result<bool> {
    host.undo(doc)
}

/// Step 5: add a type layer; its layer id.
pub fn add_text(host: &mut Host, doc: DocumentId, text: &str) -> Result<LayerId> {
    let r = host.execute_with("type.create", json!({"x": 24, "y": 48, "text": text, "size": 28, "color": "#202020"}), ExecOptions::on(doc))?;
    r.get("layer").and_then(Value::as_u64).map(LayerId).ok_or_else(|| Error::Engine(format!("type.create returned no layer: {r}")))
}

/// Step 6: save as PSD bytes (incremental: layers unchanged since the last save are not encoded
/// again).
pub fn save(host: &mut Host, doc: DocumentId) -> Result<Vec<u8>> {
    host.save_psd(doc)
}

/// The blob's pixels on `layer` (raw RGBA rows), to see Remove and Undo land.
pub fn blob_pixels(host: &mut Host, doc: DocumentId, layer: LayerId) -> Result<Vec<u8>> {
    Ok(host.get_pixels(doc, layer, GetPixels { rect: Some(BLOB), mask: false, raw: true })?.data)
}

/// One row of the host-drawn layer list.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerRow {
    /// Session id (what commands' `layer` takes).
    pub id: LayerId,
    /// Name.
    pub name: String,
    /// `pixel`, `type`, `group`, `smart object`… (lower case)
    pub kind: String,
    /// Shown.
    pub visible: bool,
    /// Selected in the document.
    pub selected: bool,
    /// `[x, y, width, height]` in document pixels, if the layer has pixel content.
    pub bounds: Option<[f32; 4]>,
    /// Nesting depth (0: top level).
    pub depth: usize,
}

/// The document's layers, top to bottom, flattened, from `document.inspect`.
pub fn layers(host: &Host, doc: DocumentId) -> Result<Vec<LayerRow>> {
    let v = host.inspect(doc, InspectOptions::default())?;
    let mut out = Vec::new();
    if let Some(list) = v.get("layers").and_then(Value::as_array) {
        flatten(list, 0, &mut out);
    }
    Ok(out)
}

/// Groups deeper than this are listed no further (documents can nest deep).
const MAX_DEPTH: usize = 32;

fn flatten(list: &[Value], depth: usize, out: &mut Vec<LayerRow>) {
    for l in list {
        let Some(id) = l.get("id").and_then(Value::as_u64) else { continue };
        let bounds = l.get("bounds").and_then(Value::as_array).and_then(|b| match b.as_slice() {
            [x, y, w, h] => Some([x.as_f64()? as f32, y.as_f64()? as f32, w.as_f64()? as f32, h.as_f64()? as f32]),
            _ => None,
        });
        out.push(LayerRow {
            id: LayerId(id),
            name: l.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
            // `document.inspect` says `Pixel`, `Type`, `Group`, `Smart Object`…
            kind: l.get("kind").and_then(Value::as_str).unwrap_or_default().to_ascii_lowercase(),
            visible: l.get("visible").and_then(Value::as_bool).unwrap_or(true),
            selected: l.get("selected").and_then(Value::as_bool).unwrap_or(false),
            bounds,
            depth,
        });
        if depth < MAX_DEPTH
            && let Some(children) = l.get("children").and_then(Value::as_array)
        {
            flatten(children, depth + 1, out);
        }
    }
}

/// Show or hide a layer from the host's list (one undo step, like the Layers panel's eye).
pub fn set_visible(host: &mut Host, doc: DocumentId, layer: LayerId, visible: bool) -> Result<()> {
    host.execute_with("layer.setProps", json!({"layer": layer.0, "visible": visible}), ExecOptions::on(doc)).map(|_| ())
}

/// The named view of the app's side-by-side tiles.
pub const COMPARE_VIEW: &str = "without the selected layer";

/// Define (or redefine) [`COMPARE_VIEW`]: `layer` hidden. No history step.
pub fn define_compare_view(host: &mut Host, doc: DocumentId, layer: LayerId) -> Result<()> {
    host.define_view(doc, COMPARE_VIEW, json!({"hidden": [layer.0]})).map(|_| ())
}

/// Select a layer from the host's list.
pub fn select(host: &mut Host, doc: DocumentId, layer: LayerId) -> Result<()> {
    host.execute_with("layer.select", json!({"layer": layer.0}), ExecOptions::on(doc)).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stroke_crosses_the_blob() {
        let s = blob_stroke();
        let pts = s["strokes"][0].as_array().unwrap();
        let xs: Vec<i64> = pts.iter().map(|p| p[0].as_i64().unwrap()).collect();
        assert!(xs.first().unwrap() < &i64::from(BLOB[0]));
        assert!(xs.last().unwrap() >= &i64::from(BLOB[0] + BLOB[2]));
    }

    #[test]
    fn inspect_layers_flatten_with_depth_and_skip_bad_rows() {
        let v = json!([
            {"id": 3, "name": "G", "kind": "group", "children": [{"id": 2, "name": "T", "kind": "type", "bounds": [1, 2, 3, 4]}]},
            {"name": "no id"},
            {"id": 1, "name": "Background", "kind": "Pixel", "visible": false, "bounds": [0, 0]}
        ]);
        let mut out = Vec::new();
        flatten(v.as_array().unwrap(), 0, &mut out);
        assert_eq!(out.iter().map(|r| (r.id.0, r.depth)).collect::<Vec<_>>(), [(3, 0), (2, 1), (1, 0)]);
        assert_eq!(out[1].bounds, Some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(out[2].bounds, None);
        assert!(!out[2].visible);
        assert_eq!(out[2].kind, "pixel", "kinds in lower case");
    }

    #[test]
    fn job_end_finds_only_its_job() {
        let done = JobEnd::Done { result: json!({"ok": true}) };
        let events = vec![
            Event::ToolChanged { tool: "brush".into() },
            Event::JobFinished { job: JobHandle(7), command: "edit.removeAi".into(), document: None, end: JobEnd::Cancelled },
            Event::JobFinished { job: JobHandle(8), command: "edit.removeAi".into(), document: None, end: done.clone() },
        ];
        assert_eq!(job_end(&events, JobHandle(8)), Some(done));
        assert_eq!(job_end(&events, JobHandle(7)), Some(JobEnd::Cancelled));
        assert_eq!(job_end(&events, JobHandle(9)), None);
    }
}
