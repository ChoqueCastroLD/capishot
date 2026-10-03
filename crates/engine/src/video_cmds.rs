//! Layer › Video Layers (and Rasterize › Video). A video layer holds a stack of frames; the
//! layer's displayed raster content is the frame at the timeline playhead, kept in sync by [`sync`]
//! after any edit or navigation. Headless and scriptable. Frame persistence in `.pcraft` is a
//! follow-up; a saved document keeps the current frame as the layer's content.

use photocraft_doc::{Document, Layer, LayerContent, Timeline, VideoData};
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

/// Set every video layer's displayed content to its frame at the playhead.
pub fn sync(doc: &mut Document) {
    let cur = doc.timeline.as_ref().map_or(0, |t| t.current);
    sync_layers(&mut doc.layers, cur);
}

fn sync_layers(layers: &mut [Layer], cur: usize) {
    for l in layers.iter_mut() {
        let frame = l.video.as_ref().filter(|v| !v.frames.is_empty()).map(|v| v.frames[cur.min(v.frames.len() - 1)].clone());
        if let Some(f) = frame
            && let LayerContent::Raster(s) = &mut l.content
        {
            *s = f;
        }
        if let Some(ch) = l.children_mut() {
            sync_layers(ch, cur);
        }
    }
}

/// The active layer, if it's a video layer.
fn active_video(s: &Session) -> std::result::Result<(), String> {
    let st = s.active().ok_or("no document")?;
    match st.active_layer.and_then(|id| st.doc.layer(id)) {
        Some(l) if l.video.is_some() => Ok(()),
        _ => Err("select a video layer".into()),
    }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document".into())
}

/// Edit the active video layer's [`VideoData`] at the playhead, then re-sync the displayed frame.
fn edit_video(s: &mut Session, label: &str, f: impl FnOnce(&mut VideoData, usize) -> Result<()>) -> Result<Value> {
    let count = s.edit(label, |doc, active| {
        let cur = doc.timeline.as_ref().map_or(0, |t| t.current);
        let id = active.ok_or(EngineError::NoDocument)?;
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let v = l.video.as_mut().ok_or_else(|| EngineError::BadParams { cmd: label.into(), msg: "the active layer is not a video layer".into() })?;
        f(v, cur)?;
        let n = v.frames.len();
        if let Some(t) = &mut doc.timeline {
            t.duration = t.duration.max(n.max(1));
            t.work_end = t.work_end.max(t.duration);
            t.clamp();
        }
        sync(doc);
        Ok(n)
    })?;
    Ok(json!({"frames": count}))
}

fn blank_of(fmt: photocraft_color::PixelFormat) -> Surface {
    Surface::new(fmt)
}

fn new_blank(s: &mut Session, _p: &Value) -> Result<Value> {
    let id = s.edit("New Blank Video Layer", |doc, active| {
        let fmt = doc.pixel_format();
        if doc.timeline.is_none() {
            doc.timeline = Some(Timeline::new(1, 30.0));
        }
        let fps = doc.timeline.as_ref().map_or(30.0, |t| t.fps);
        let n = doc.timeline.as_ref().map_or(1, |t| t.duration).max(1);
        let frames: Vec<Surface> = (0..n).map(|_| Surface::new(fmt)).collect();
        let name = doc.next_layer_name("Video Layer");
        let mut l = Layer::raster(name, fmt);
        l.video = Some(VideoData::new(frames, fps));
        let id = doc.insert_above(*active, l);
        *active = Some(id);
        sync(doc);
        Ok(id)
    })?;
    Ok(json!({"layer": id.0}))
}

fn insert_blank(s: &mut Session, _p: &Value) -> Result<Value> {
    let fmt = s.active().ok_or(EngineError::NoDocument)?.doc.pixel_format();
    edit_video(s, "Insert Blank Frame", move |v, cur| {
        let at = (cur + 1).min(v.frames.len());
        v.frames.insert(at, blank_of(fmt));
        Ok(())
    })
}

fn duplicate_frame(s: &mut Session, _p: &Value) -> Result<Value> {
    edit_video(s, "Duplicate Frame", |v, cur| {
        if let Some(f) = v.frames.get(cur).cloned() {
            v.frames.insert(cur + 1, f);
        }
        Ok(())
    })
}

fn delete_frame(s: &mut Session, _p: &Value) -> Result<Value> {
    edit_video(s, "Delete Frame", |v, cur| {
        if v.frames.len() > 1 && cur < v.frames.len() {
            v.frames.remove(cur);
        }
        Ok(())
    })
}

fn restore_frame(s: &mut Session, _p: &Value) -> Result<Value> {
    let fmt = s.active().ok_or(EngineError::NoDocument)?.doc.pixel_format();
    edit_video(s, "Restore Frame", move |v, cur| {
        if let Some(f) = v.frames.get_mut(cur) {
            *f = blank_of(fmt);
        }
        Ok(())
    })
}

fn restore_all(s: &mut Session, _p: &Value) -> Result<Value> {
    let fmt = s.active().ok_or(EngineError::NoDocument)?.doc.pixel_format();
    edit_video(s, "Restore All Frames", move |v, _| {
        for f in v.frames.iter_mut() {
            *f = blank_of(fmt);
        }
        Ok(())
    })
}

fn show_altered(s: &mut Session, _p: &Value) -> Result<Value> {
    edit_video(s, "Show Altered Video", |v, _| {
        v.show_altered = !v.show_altered;
        Ok(())
    })
}

/// Rasterize: drop the frame stack, keeping the current frame as a normal pixel layer.
fn rasterize(s: &mut Session, _p: &Value) -> Result<Value> {
    s.edit("Rasterize Video", |doc, active| {
        let id = active.ok_or(EngineError::NoDocument)?;
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        // The content already shows the current frame (kept in sync); just drop the stack.
        l.video = None;
        Ok(())
    })?;
    Ok(json!({"rasterized": true}))
}

macro_rules! spec {
    ($id:expr, $label:expr, $menu:expr, $en:expr, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: $menu, shortcut: None, params: "{} → {}", enabled: $en, journal: true, run: $run }
    };
}

pub fn specs() -> Vec<CommandSpec> {
    let vl = &["Layer", "Video Layers"] as &[&str];
    vec![
        spec!("layer.videoLayers.newBlankVideoLayer", "New Blank Video Layer", vl, has_doc, |s, p| new_blank(s, p)),
        spec!("layer.videoLayers.insertBlankFrame", "Insert Blank Frame", vl, active_video, |s, p| insert_blank(s, p)),
        spec!("layer.videoLayers.duplicateFrame", "Duplicate Frame", vl, active_video, |s, p| duplicate_frame(s, p)),
        spec!("layer.videoLayers.deleteFrame", "Delete Frame", vl, active_video, |s, p| delete_frame(s, p)),
        spec!("layer.videoLayers.restoreFrame", "Restore Frame", vl, active_video, |s, p| restore_frame(s, p)),
        spec!("layer.videoLayers.restoreAllFrames", "Restore All Frames", vl, active_video, |s, p| restore_all(s, p)),
        spec!("layer.videoLayers.showAlteredVideo", "Show Altered Video", vl, active_video, |s, p| show_altered(s, p)),
        spec!("layer.videoLayers.rasterize", "Rasterize", vl, active_video, |s, p| rasterize(s, p)),
        spec!("layer.rasterize.video", "Video", &["Layer", "Rasterize"], active_video, |s, p| rasterize(s, p)),
    ]
}

#[cfg(test)]
mod tests;
