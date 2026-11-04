//! Stable layer ids (`uid` in `document.inspect`): the PSD layer id (`lyid`,
//! [`photocraft_doc::Layer::psd_id`]), given to every layer as soon as it exists, so a client can
//! address a layer by an id that survives save and reopen (PSD and `.pcraft` keep it). The
//! session's [`photocraft_doc::LayerId`] is renumbered on every open.
//!
//! A layer without one (new, or duplicated: [`photocraft_doc::Layer::duplicate`] clears it) gets
//! the next number above every id the document has held this session, so a deleted layer's id is
//! never handed to another. When two layers share an id (a copy made without `duplicate`), the
//! older layer (lower session id) keeps it.

use std::collections::HashSet;

use photocraft_doc::{Document, LayerId};

/// Give every layer of `doc` a unique [`photocraft_doc::Layer::psd_id`]. `high` is the highest id
/// handed out or seen so far for this document (raised here). Returns whether anything changed.
pub fn assign(doc: &mut Document, high: &mut u32) -> bool {
    let mut layers: Vec<(LayerId, Option<u32>)> = doc.walk().into_iter().map(|(_, _, l)| (l.id, l.psd_id)).collect();
    if let Some(max) = layers.iter().filter_map(|(_, p)| *p).max() {
        *high = (*high).max(max);
    }
    layers.sort_by_key(|(id, _)| *id);
    let mut used = HashSet::with_capacity(layers.len());
    let mut fresh = Vec::new();
    for (id, psd) in layers {
        match psd {
            Some(p) if used.insert(p) => {}
            _ => fresh.push(id),
        }
    }
    let changed = !fresh.is_empty();
    for id in fresh {
        let Some(next) = next_free(high, &used) else { break };
        used.insert(next);
        if let Some(l) = doc.layer_mut(id) {
            l.psd_id = Some(next);
        }
    }
    changed
}

/// The next unused id above `high` (wrapping past `u32::MAX` to 1, skipping ids in use); `None`
/// only when every id is taken.
fn next_free(high: &mut u32, used: &HashSet<u32>) -> Option<u32> {
    for _ in 0..=used.len() {
        *high = high.checked_add(1).unwrap_or(1);
        if !used.contains(high) {
            return Some(*high);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::Layer;

    fn ids(doc: &Document) -> Vec<Option<u32>> {
        doc.walk().into_iter().map(|(_, _, l)| l.psd_id).collect()
    }

    fn layer(name: &str) -> Layer {
        Layer::group(name, Vec::new())
    }

    fn doc_with(n: usize) -> Document {
        let mut d = Document::new("t", photocraft_geom::Size::new(8, 8), photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U8);
        for i in 0..n {
            d.layers.push(layer(&format!("L{i}")));
        }
        d
    }

    #[test]
    fn new_layers_get_ids_above_the_highest() {
        let mut d = doc_with(3);
        d.layers[1].psd_id = Some(28);
        let mut high = 0;
        assert!(assign(&mut d, &mut high));
        assert_eq!(ids(&d), [Some(29), Some(28), Some(30)]);
        assert!(!assign(&mut d, &mut high), "already assigned: nothing changes");
    }

    #[test]
    fn a_deleted_layers_id_is_not_reused() {
        let mut d = doc_with(2);
        let mut high = 0;
        assign(&mut d, &mut high);
        d.layers.pop();
        d.layers.push(layer("new"));
        assign(&mut d, &mut high);
        assert_eq!(ids(&d), [Some(1), Some(3)]);
    }

    #[test]
    fn a_copy_sharing_an_id_gets_a_new_one_and_the_original_keeps_it() {
        let mut d = doc_with(1);
        d.layers[0].psd_id = Some(7);
        let mut copy = d.layers[0].clone();
        copy.id = LayerId::fresh();
        d.layers.insert(0, copy);
        let dup = d.layers[1].duplicate();
        d.layers.push(dup);
        let mut high = 0;
        assign(&mut d, &mut high);
        assert_eq!(ids(&d), [Some(8), Some(7), Some(9)]);
    }

    #[test]
    fn ids_wrap_past_the_maximum_without_panicking() {
        let mut d = doc_with(2);
        d.layers[0].psd_id = Some(u32::MAX);
        let mut high = 0;
        assign(&mut d, &mut high);
        assert_eq!(ids(&d), [Some(u32::MAX), Some(1)]);
    }
}
