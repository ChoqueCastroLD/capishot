//! A type layer's language in PSD files. Photoshop keeps it as an index in EngineData (`Language`) whose
//! table isn't published, and a wrong index would change the text's language in Photoshop. So it
//! goes in a block of our own on the layer ([`BLOCK`], as `perspective_type`'s): Photoshop's index
//! stays what it was, and Capishot reads the language back. A block that no longer fits the text
//! (edited elsewhere) is dropped.

use std::sync::Arc;

use photocraft_doc::{Document, Layer, LayerContent};
use serde_json::{Value, json};

/// The layer block holding the language of a type layer's runs (Capishot's own key).
pub const BLOCK: [u8; 4] = *b"CpLn";

fn spans(t: &photocraft_doc::TextLayer) -> Vec<Value> {
    t.char_runs().iter().map(|r| json!([r.len, r.style.language])).collect()
}

fn has_language(l: &Layer) -> bool {
    matches!(&l.content, LayerContent::Text(t) if t.char_runs().iter().any(|r| r.style.language.is_some()))
}

fn has_block(l: &Layer) -> bool {
    l.psd_blocks.iter().any(|(k, _)| *k == BLOCK)
}

/// `doc` with each type layer's language in its block, or `None` when there's nothing to change.
pub(crate) fn mark_for_psd(doc: &Document) -> Option<Document> {
    if !doc.walk().iter().any(|(_, _, l)| has_language(l) || has_block(l)) {
        return None;
    }
    let mut out = doc.clone();
    mark_layers(&mut out.layers);
    Some(out)
}

fn mark_layers(layers: &mut [Layer]) {
    for l in layers {
        if let LayerContent::Group(g) = &mut l.content {
            mark_layers(&mut g.children);
            continue;
        }
        l.psd_blocks.retain(|(k, _)| *k != BLOCK);
        if has_language(l)
            && let LayerContent::Text(t) = &l.content
            && let Ok(bytes) = serde_json::to_vec(&json!({"text": t.text.len(), "runs": spans(t)}))
        {
            l.psd_blocks.push((BLOCK, Arc::new(bytes)));
        }
    }
}

/// Puts back the language each type layer's block holds, and takes the block off.
pub(crate) fn unmark_after_import(doc: &mut Document) {
    if doc.walk().iter().any(|(_, _, l)| has_block(l)) {
        unmark_layers(&mut doc.layers);
    }
}

fn unmark_layers(layers: &mut [Layer]) {
    for l in layers {
        if let LayerContent::Group(g) = &mut l.content {
            unmark_layers(&mut g.children);
            continue;
        }
        let Some((_, bytes)) = l.psd_blocks.iter().find(|(k, _)| *k == BLOCK).cloned() else { continue };
        l.psd_blocks.retain(|(k, _)| *k != BLOCK);
        if let (LayerContent::Text(t), Ok(v)) = (&mut l.content, serde_json::from_slice::<Value>(&bytes)) {
            apply(t, &v);
        }
    }
}

/// The spans over `t`'s bytes, when `v` was written for a text of this length.
fn parse(t: &photocraft_doc::TextLayer, v: &Value) -> Option<Vec<(usize, Option<String>)>> {
    if v.get("text")?.as_u64()? as usize != t.text.len() {
        return None;
    }
    let spans = v.get("runs")?.as_array()?.iter().map(|s| Some((s.get(0)?.as_u64()? as usize, s.get(1)?.as_str().map(str::to_string)))).collect::<Option<Vec<_>>>()?;
    let mut at = 0_usize;
    for (len, _) in &spans {
        at = at.checked_add(*len)?;
        if !t.text.is_char_boundary(at) {
            return None;
        }
    }
    (at == t.text.len()).then_some(spans)
}

fn apply(t: &mut photocraft_doc::TextLayer, v: &Value) {
    let Some(spans) = parse(t, v) else { return };
    let runs = t.char_runs();
    let (mut out, mut r, mut in_run) = (Vec::new(), 0, 0);
    for (mut left, language) in spans {
        while left > 0 && r < runs.len() {
            let take = left.min(runs[r].len - in_run);
            let mut style = runs[r].style.clone();
            style.language.clone_from(&language);
            out.push(photocraft_doc::text::TextRun { len: take, style });
            left -= take;
            in_run += take;
            if in_run == runs[r].len {
                (r, in_run) = (r + 1, 0);
            }
        }
    }
    t.runs = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::TextLayer;

    /// A block written for another text, or cut mid-character, is ignored.
    #[test]
    fn a_block_that_doesnt_fit_the_text_is_ignored() {
        let t = TextLayer { text: "añb".into(), ..Default::default() };
        let fits = json!({"text": 4, "runs": [[1, "es"], [3, null]]});
        assert_eq!(parse(&t, &fits), Some(vec![(1, Some("es".to_string())), (3, None)]));
        assert_eq!(parse(&t, &json!({"text": 5, "runs": [[5, "es"]]})), None, "another text");
        assert_eq!(parse(&t, &json!({"text": 4, "runs": [[2, "es"], [2, "pt"]]})), None, "mid-character");
        assert_eq!(parse(&t, &json!({"text": 4, "runs": [[1, "es"]]})), None, "short");
        assert_eq!(parse(&t, &json!({"text": 4, "runs": [[usize::MAX, "es"], [5, "es"]]})), None, "overflow");
    }
}
