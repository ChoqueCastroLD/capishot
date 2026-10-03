use super::*;
use serde_json::json;
use photocraft_doc::LayerId;

fn session() -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 16, "height": 12})).unwrap();
    s
}

fn frames(s: &Session, id: LayerId) -> usize {
    s.active().unwrap().doc.layer(id).unwrap().video.as_ref().unwrap().frames.len()
}

#[test]
fn new_blank_insert_duplicate_delete_frames() {
    let mut s = session();
    let r = s.execute("layer.videoLayers.newBlankVideoLayer", json!({})).unwrap();
    let id = LayerId(r["layer"].as_u64().unwrap());
    assert!(s.active().unwrap().doc.layer(id).unwrap().video.is_some());
    assert!(s.active().unwrap().doc.timeline.is_some());
    let start = frames(&s, id);
    s.execute("layer.videoLayers.insertBlankFrame", json!({})).unwrap();
    assert_eq!(frames(&s, id), start + 1);
    s.execute("layer.videoLayers.duplicateFrame", json!({})).unwrap();
    assert_eq!(frames(&s, id), start + 2);
    s.execute("layer.videoLayers.deleteFrame", json!({})).unwrap();
    assert_eq!(frames(&s, id), start + 1);
    assert!(s.active().unwrap().doc.timeline.as_ref().unwrap().duration >= frames(&s, id));
}

#[test]
fn rasterize_drops_the_stack() {
    let mut s = session();
    s.execute("layer.videoLayers.newBlankVideoLayer", json!({})).unwrap();
    let r = s.execute("layer.videoLayers.rasterize", json!({})).unwrap();
    assert_eq!(r["rasterized"], true);
    let id = s.active().unwrap().active_layer.unwrap();
    assert!(s.active().unwrap().doc.layer(id).unwrap().video.is_none());
    assert!(matches!(s.active().unwrap().doc.layer(id).unwrap().content, LayerContent::Raster(_)));
}

#[test]
fn frame_ops_need_a_video_layer() {
    let mut s = session();
    assert!(s.execute("layer.videoLayers.insertBlankFrame", json!({})).is_err());
}

#[test]
fn scrub_keeps_content_a_raster() {
    let mut s = session();
    let r = s.execute("layer.videoLayers.newBlankVideoLayer", json!({})).unwrap();
    let id = LayerId(r["layer"].as_u64().unwrap());
    s.execute("layer.videoLayers.insertBlankFrame", json!({})).unwrap();
    s.execute("timeline.setFrame", json!({"frame": 1})).unwrap();
    assert!(matches!(s.active().unwrap().doc.layer(id).unwrap().content, LayerContent::Raster(_)));
}
