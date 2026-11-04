//! `type.layoutDigest`: a fingerprint of how type layers lay out, so a host running the same
//! document in the browser and in `capishot-cli` can check that both place every glyph alike
//! (layout is deterministic given the same font files; see `docs/control-protocol.md`).

use photocraft_doc::{LayerContent, LayerId, TextLayer};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

const ID: &str = "type.layoutDigest";

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: ID.into(), msg: msg.into() }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: ID,
        label: "Type Layout Digest",
        menu: &[],
        shortcut: None,
        params: r##"{"layer":id? (default every type layer),"fontHashes":bool? (sha256 of each font file, slower)} → {"layer","digest","lines","glyphs","fonts":[{"names":[…],"bytes","index","sha256"?}]} for one layer, else {"digest" (of all),"layers":[{"layer","digest","lines","glyphs"}]}. Equal digests = identical layout bit for bit"##,
        enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document".into()),
        journal: false,
        run: layout_digest,
    }]
}

fn layout_digest(s: &mut Session, p: &Value) -> Result<Value> {
    match p {
        Value::Object(o) => {
            if let Some(k) = o.keys().find(|k| !matches!(k.as_str(), "layer" | "fontHashes")) {
                return Err(bad(format!("unknown field `{k}` (fields: layer, fontHashes)")));
            }
        }
        Value::Null => {}
        _ => return Err(bad("params must be an object")),
    }
    let hashes = match p.get("fontHashes") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or_else(|| bad("`fontHashes` must be true or false"))?,
    };
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let dpi = st.doc.resolution_dpi;
    match p.get("layer").filter(|v| !v.is_null()) {
        Some(v) => {
            let id = LayerId(v.as_u64().ok_or_else(|| bad("`layer` must be a layer id"))?);
            let l = st.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
            let LayerContent::Text(t) = &l.content else { return Err(bad(format!("layer {} is not a type layer", id.0))) };
            Ok(one(id, t, dpi, true, hashes))
        }
        None => {
            let layers: Vec<Value> = st
                .doc
                .walk()
                .into_iter()
                .filter_map(|(_, _, l)| match &l.content {
                    LayerContent::Text(t) => Some(one(l.id, t, dpi, false, false)),
                    _ => None,
                })
                .collect();
            let all: String = layers.iter().filter_map(|l| l["digest"].as_str()).collect::<Vec<_>>().join(",");
            Ok(json!({ "digest": photocraft_text::sha256::sha256_hex(all.as_bytes()), "layers": layers }))
        }
    }
}

/// One layer's digest, line and glyph counts, and (`fonts`) the font files its glyphs use.
fn one(id: LayerId, t: &TextLayer, dpi: f32, fonts: bool, hashes: bool) -> Value {
    let layout = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner()).layout(t, dpi);
    let mut out = json!({
        "layer": id.0,
        "digest": photocraft_text::layout::layout_digest(&layout),
        "lines": layout.lines.len(),
        "glyphs": layout.glyphs.len(),
    });
    if fonts {
        let mut seen: Vec<(usize, u32)> = Vec::new();
        let mut list = Vec::new();
        for face in &layout.faces {
            let data = face.font.data.as_ref();
            let key = (data.as_ptr() as usize, face.font.index);
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let mut f = json!({ "names": photocraft_text::served::face_names(data).into_iter().take(3).collect::<Vec<_>>(), "bytes": data.len(), "index": face.font.index });
            if hashes {
                f["sha256"] = json!(photocraft_text::sha256::sha256_hex(data));
            }
            list.push(f);
        }
        out["fonts"] = json!(list);
    }
    out
}

#[cfg(test)]
#[path = "type_layout_cmds_tests.rs"]
mod tests;
