use super::*;
use photocraft_doc::{Color, ColorMode, Layer, SampleType, Size};

fn base() -> Document {
    Document::with_background("h", Size::new(64, 64), ColorMode::Rgb, SampleType::U8, Color::WHITE)
}

/// Apply an edit the way the engine does: snapshot, mutate a clone, record.
fn edit(h: &mut History, cur: &mut Arc<Document>, label: &str, f: impl FnOnce(&mut Document)) {
    let before = cur.clone();
    let mut d = (**cur).clone();
    f(&mut d);
    *cur = Arc::new(d);
    h.record(label, before, LayerTarget::default());
}

#[test]
fn payload_memo_counts_like_one_walk_and_keeps_no_snapshot_alive() {
    let mut a = base();
    if let Some(s) = a.layers[0].surface_mut() {
        s.write_pixel(1, 1, &[1.0, 0.0, 0.0, 1.0]);
    }
    let a = Arc::new(a);
    let mut b = (*a).clone();
    b.insert_above(None, Layer::raster("L", b.pixel_format()));
    if let Some(s) = b.layers[1].surface_mut() {
        s.write_pixel(3, 3, &[0.0, 1.0, 0.0, 1.0]);
    }
    let b = Arc::new(b);
    let walked = |docs: &[Arc<Document>]| {
        let mut seen = HashSet::new();
        docs.iter().map(|d| document_bytes(d, &mut seen)).sum::<usize>()
    };
    let mut memo = PayloadMemo::default();
    assert_eq!(memo.sync(&[a.clone(), b.clone()]), walked(&[a.clone(), b.clone()]));
    assert_eq!(memo.sync(std::slice::from_ref(&b)), walked(std::slice::from_ref(&b)), "a retired snapshot releases only its own tiles");
    assert_eq!(memo.sync(&[a.clone(), b.clone()]), walked(&[a.clone(), b.clone()]));
    assert_eq!(Arc::strong_count(&a), 1, "the memo holds snapshots weakly");
    drop(a);
    assert_eq!(memo.sync(std::slice::from_ref(&b)), walked(std::slice::from_ref(&b)), "a dropped snapshot is noticed");
    let mut seen = HashSet::new();
    assert_eq!(memo.extra_bytes(&b, &mut seen), 0, "everything in b is counted already");
}

#[test]
fn undo_redo_cycle() {
    let mut h = History::default();
    let mut cur = Arc::new(base());
    edit(&mut h, &mut cur, "New Layer", |d| {
        d.insert_above(None, Layer::raster("L", d.pixel_format()));
    });
    assert_eq!(cur.layers.len(), 2);
    assert_eq!(h.undo_label(), Some("New Layer"));
    cur = h.undo(cur).unwrap().0;
    assert_eq!(cur.layers.len(), 1);
    assert_eq!(h.redo_label(), Some("New Layer"));
    cur = h.redo(cur).unwrap().0;
    assert_eq!(cur.layers.len(), 2);
    assert!(h.redo(cur.clone()).is_none());
}

#[test]
fn undo_and_redo_return_the_layers_each_state_targeted_when_created() {
    let mut h = History::default();
    let open = Arc::new(base());
    let bg = open.layers[0].id;
    let target = |id: LayerId| LayerTarget { active: Some(id), selected: vec![id] };
    h.set_current_layers(target(bg));
    // New Layer targets the new layer; the background is then selected (not a step) and filled.
    let mut d = (*open).clone();
    let new = d.insert_above(None, Layer::raster("L", d.pixel_format()));
    let layered = Arc::new(d);
    h.record("New Layer", open, target(new));
    let mut d = (*layered).clone();
    d.name = "filled".into();
    h.record("Fill", layered, target(bg));
    let (d, layers) = h.undo(Arc::new(d)).unwrap();
    assert_eq!(layers, target(new), "New Layer's state targeted the new layer when created");
    let (d, layers) = h.undo(d).unwrap();
    assert_eq!(layers, target(bg), "the opened document's target");
    let (d, layers) = h.redo(d).unwrap();
    assert_eq!(layers, target(new));
    let (_, layers) = h.redo(d).unwrap();
    assert_eq!(layers, target(bg), "Fill's state targeted the background");
}

#[test]
fn state_accessor_matches_entries() {
    let mut h = History::default();
    let mut cur = Arc::new(base());
    let open = cur.clone();
    edit(&mut h, &mut cur, "A", |d| d.name = "a".into());
    edit(&mut h, &mut cur, "B", |d| d.name = "b".into());
    assert_eq!(h.entries().len(), h.past_len() + 1);
    assert!(Arc::ptr_eq(&h.state(0).unwrap(), &open));
    assert_eq!(h.state(1).unwrap().name, "a");
    assert!(h.state(2).is_none(), "the current document is not held");
    cur = h.undo(cur).unwrap().0;
    h.undo(cur).unwrap();
    assert_eq!(h.redo_labels().collect::<Vec<_>>(), ["A", "B"], "next redo first");
}

#[test]
fn new_edit_clears_redo() {
    let mut h = History::default();
    let mut cur = Arc::new(base());
    edit(&mut h, &mut cur, "A", |d| d.name = "a".into());
    cur = h.undo(cur).unwrap().0;
    edit(&mut h, &mut cur, "B", |d| d.name = "b".into());
    assert!(!h.can_redo());
    assert_eq!(h.entries(), vec!["Open".to_string(), "B".to_string()]);
}

#[test]
fn max_states_bound() {
    let mut h = History::new(3);
    let mut cur = Arc::new(base());
    for i in 0..10 {
        edit(&mut h, &mut cur, &format!("step {i}"), |d| d.name = format!("{i}"));
    }
    let mut n = 0;
    while let Some((d, _)) = h.undo(cur.clone()) {
        cur = d;
        n += 1;
    }
    assert_eq!(n, 3);
    assert_eq!(cur.name, "6");
}

#[test]
fn byte_budget_drops_oldest_states_but_keeps_one() {
    let mut h = History::new(50);
    let mut cur = Arc::new(base());
    let tile = 256 * 256 * 4;
    // Each step repaints the background's tile: one unique tile per state.
    for i in 0..6 {
        edit(&mut h, &mut cur, &format!("paint {i}"), |d| {
            let id = d.layers[0].id;
            d.layer_mut(id).unwrap().surface_mut().unwrap().write_pixel(1, 1, &[i as f32 / 8.0, 0.0, 0.0, 1.0]);
        });
    }
    assert_eq!(h.trim(&cur), 0, "unlimited by default");
    assert_eq!(h.past_len(), 6);
    // The current tile plus three history tiles fit.
    h.max_bytes = 4 * tile;
    assert_eq!(h.trim(&cur), 3);
    assert_eq!(h.past_len(), 3);
    assert_eq!(h.unique_bytes(&cur), 3 * tile);
    assert_eq!(h.pixel_bytes(&cur), 4 * tile, "the current tile plus history's");
    assert_eq!(h.entries()[0], "paint 2");
    // A budget smaller than one state still keeps the last step undoable.
    h.max_bytes = 1;
    h.trim(&cur);
    assert_eq!(h.past_len(), 1);
    assert!(h.undo(cur.clone()).is_some());
}

#[test]
fn history_shares_untouched_tiles() {
    let mut h = History::default();
    let mut cur = Arc::new(base());
    assert_eq!(h.unique_bytes(&cur), 0);
    // rename only: no pixel changes, history holds no unique tiles
    edit(&mut h, &mut cur, "Rename", |d| d.name = "x".into());
    assert_eq!(h.unique_bytes(&cur), 0);
    // paint one pixel: exactly one tile becomes unique to history
    edit(&mut h, &mut cur, "Paint", |d| {
        let id = d.layers[0].id;
        d.layer_mut(id).unwrap().surface_mut().unwrap().write_pixel(1, 1, &[0.0, 0.0, 0.0, 1.0]);
    });
    assert_eq!(h.unique_bytes(&cur), 256 * 256 * 4);
}
#[derive(Debug)]
struct Archive(Result<Arc<Document>, String>);
impl ArchivedDocument for Archive {
    fn load(&self) -> Result<Arc<Document>, String> {
        self.0.clone()
    }
}

#[test]
fn recovery_checkpoint_preserves_cursor_labels_and_both_stack_orders() {
    let mut history = History::new(37);
    history.max_bytes = 1234;
    let mut current = Arc::new(base());
    for name in ["A", "B", "C", "D"] {
        edit(&mut history, &mut current, name, |doc| doc.name = name.into());
    }
    current = history.try_undo(current).unwrap().unwrap().0;
    current = history.try_undo(current).unwrap().unwrap().0;
    assert_eq!(current.name, "B");
    let checkpoint = history.checkpoint();
    assert_eq!(checkpoint.current_label, "B");
    assert_eq!(checkpoint.undo.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["Open", "A"]);
    assert_eq!(checkpoint.redo.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["D", "C"]);
    let mut recovered = History::from_checkpoint(checkpoint).unwrap();
    assert_eq!(recovered.max_states, 37);
    assert_eq!(recovered.max_bytes, 1234);
    for expected in ["C", "D"] {
        current = recovered.try_redo(current).unwrap().unwrap().0;
        assert_eq!(current.name, expected);
    }
    for expected in ["C", "B", "A"] {
        current = recovered.try_undo(current).unwrap().unwrap().0;
        assert_eq!(current.name, expected);
    }
    assert_eq!(recovered.redo_label(), Some("B"));
}

#[test]
fn recovery_checkpoint_keeps_cold_states_lazy_and_failed_navigation_atomic() {
    let mut checkpoint = History::new(50).checkpoint();
    checkpoint.current_label = "Current".into();
    checkpoint.undo.push(HistoryState::from_archive("Before", true, Arc::new(Archive(Err("unreadable recovery".into())))));
    checkpoint.redo.push(HistoryState::from_document("After", Arc::new(base())));
    let mut history = History::from_checkpoint(checkpoint).unwrap();
    let captured = history.checkpoint();
    assert!(captured.undo.first().unwrap().has_selection());
    assert!(captured.undo.first().unwrap().load_document().is_err());
    assert_eq!(history.archived_states(), 1);
    assert!(history.has_past_selection());
    assert!(history.try_undo(Arc::new(base())).is_err());
    assert_eq!(history.entries(), ["Before", "Current"]);
    assert_eq!(history.redo_label(), Some("After"));
    assert_eq!(history.past_len(), 1);
}

#[test]
fn recovered_identity_remaps_resident_and_cold_states_without_changing_sources() {
    let original = Arc::new(base());
    let old_id = original.id;
    let new_id = DocId::fresh();
    let mut checkpoint = History::default().checkpoint();
    checkpoint.undo.push(HistoryState::from_archive("Cold", false, Arc::new(Archive(Ok(original.clone())))));
    checkpoint.redo.push(HistoryState::from_document("Resident", original.clone()));
    let mut history = History::from_checkpoint(checkpoint).unwrap();
    history.remap_document_id(new_id);
    assert_eq!(history.archived_states(), 1, "remapping does not hydrate cold history");
    let checkpoint = history.checkpoint();
    assert_eq!(checkpoint.undo[0].load_document().unwrap().id, new_id);
    assert_eq!(checkpoint.redo[0].load_document().unwrap().id, new_id);
    assert_eq!(original.id, old_id, "other tabs retain their identities");
}

#[test]
fn recovery_checkpoint_preserves_targeting_across_undo_and_redo() {
    let before = Arc::new(base());
    let first = before.layers.first().unwrap().id;
    let target = LayerTarget { active: Some(first), selected: vec![first] };
    let mut history = History::default();
    history.set_current_layers(target.clone());
    history.record("Move", before.clone(), target.clone());
    let mut restored = History::from_checkpoint(history.checkpoint()).unwrap();
    let (doc, layers) = restored.try_undo(before).unwrap().unwrap();
    assert_eq!(layers, target);
    assert_eq!(restored.try_redo(doc).unwrap().unwrap().1, target);
}

#[test]
fn recovery_checkpoint_rejects_oversized_metadata() {
    let mut checkpoint = History::new(50).checkpoint();
    checkpoint.current_label = "x".repeat(4097);
    assert!(History::from_checkpoint(checkpoint).is_err());
    let mut checkpoint = History::new(50).checkpoint();
    let state = HistoryState::from_document("State", Arc::new(base()));
    checkpoint.undo = vec![state.clone(); 10_000];
    checkpoint.redo.push(state);
    assert!(History::from_checkpoint(checkpoint).is_err());
    let mut checkpoint = History::new(50).checkpoint();
    checkpoint.max_states = 0;
    assert!(History::from_checkpoint(checkpoint).is_err());
}

#[test]
fn cold_restore_and_failed_read_preserve_stacks() {
    let mut history = History::new(50);
    let mut current = Arc::new(base());
    let original = current.clone();
    edit(&mut history, &mut current, "A", |doc| doc.name = "a".into());
    let labels = history.entries();
    assert!(history.replace_resident(&original, Arc::new(Archive(Err("corrupt scratch blob".into())))));
    assert_eq!(history.archived_states(), 1);
    assert!(history.try_state(0).is_err());
    assert!(history.try_undo(current.clone()).is_err());
    assert_eq!(history.entries(), labels);
    assert!(!history.can_redo());
    history.clear();
    history.record("A", original.clone(), LayerTarget::default());
    assert!(history.replace_resident(&original, Arc::new(Archive(Ok(original.clone())))));
    let restored = history.try_undo(current.clone()).unwrap().unwrap().0;
    assert!(Arc::ptr_eq(&restored, &original));
    assert!(Arc::ptr_eq(&history.try_redo(restored).unwrap().unwrap().0, &current));
}

#[test]
fn replacement_deduplicates_and_stale_results_are_ignored() {
    let mut history = History::new(50);
    let original = Arc::new(base());
    history.record("A", original.clone(), LayerTarget::default());
    history.record("B", original.clone(), LayerTarget::default());
    assert!(history.spill_candidate().is_none(), "shared immediate undo allocation stays hot");
    assert!(history.replace_resident(&original, Arc::new(Archive(Ok(original.clone())))));
    assert_eq!(history.archived_states(), 2);
    assert!(history.resident_states().is_empty());
    assert!(!history.replace_resident(&original, Arc::new(Archive(Ok(original.clone())))));
    assert!(history.drop_oldest());
    assert!(!history.drop_oldest());
    history.clear();
    assert_eq!(history.archived_states(), 0);
}

#[test]
fn selection_patterns_and_video_pixels_are_accounted_once() {
    let mut doc = base();
    let format = doc.pixel_format();
    let mut surface = photocraft_doc::Surface::new(format);
    surface.write_pixel(0, 0, &[0.25, 0.5, 0.75, 1.0]);
    doc.selection = Some(surface.clone());
    doc.patterns.push(photocraft_doc::Pattern::new("P", surface.clone(), 1, 1));
    doc.layers[0].video = Some(photocraft_doc::VideoData::new(vec![surface], 24.0));
    let mut seen = HashSet::new();
    assert_eq!(document_bytes(&doc, &mut seen), 2 * 256 * 256 * 4);
    assert_eq!(document_bytes(&doc, &mut seen), 0);
}

#[test]
fn cold_redo_failure_keeps_the_next_step() {
    let mut history = History::new(50);
    let mut current = Arc::new(base());
    edit(&mut history, &mut current, "A", |doc| doc.name = "a".into());
    let future = current.clone();
    current = history.try_undo(current).unwrap().unwrap().0;
    assert!(history.replace_resident(&future, Arc::new(Archive(Err("read failed".into())))));
    let label = history.redo_label().map(str::to_owned);
    assert!(history.try_redo(current).is_err());
    assert_eq!(history.redo_label(), label.as_deref());
    assert!(!history.can_undo());
}
#[test]
fn menu_queries_never_load_a_cold_selection() {
    let mut history = History::new(50);
    let mut doc = base();
    doc.selection = Some(photocraft_doc::Surface::new(doc.pixel_format()));
    let doc = Arc::new(doc);
    history.record("Select", doc.clone(), LayerTarget::default());
    assert!(history.replace_resident(&doc, Arc::new(Archive(Err("must not read".into())))));
    assert!(history.has_past_selection());
    assert_eq!(history.latest_selection_state(), Some(0));
    history.record("Clear selection", Arc::new(base()), LayerTarget::default());
    assert_eq!(history.latest_selection_state(), Some(0), "skip newer no-selection states without loading archives");
    assert!(history.resident_state(0).is_none());
    assert!(history.try_state(0).is_err());
}

#[test]
fn spill_and_drop_keep_adjacent_steps_hot_and_contiguous() {
    let mut history = History::new(50);
    let mut current = Arc::new(base());
    let oldest = current.clone();
    for label in ["A", "B", "C"] {
        edit(&mut history, &mut current, label, |doc| doc.name = label.into());
    }
    assert!(Arc::ptr_eq(&history.spill_candidate().unwrap(), &oldest));
    assert!(history.drop_oldest());
    assert_eq!(history.entries(), ["A", "B", "C"]);
    current = history.try_undo(current).unwrap().unwrap().0;
    current = history.try_undo(current).unwrap().unwrap().0;
    assert_eq!(current.name, "A");
    assert!(history.drop_oldest(), "discard farthest redo");
    assert_eq!(history.redo_label(), Some("B"));
    assert!(!history.drop_oldest());
    assert_eq!(history.try_redo(current).unwrap().unwrap().0.name, "B");
}
#[test]
fn explicit_budget_reduction_can_discard_final_cold_state() {
    let mut history = History::new(50);
    let doc = Arc::new(base());
    history.record("A", doc.clone(), LayerTarget::default());
    history.replace_resident(&doc, Arc::new(Archive(Err("must not load".into()))));
    assert!(!history.drop_oldest());
    assert!(history.discard_oldest());
    assert!(!history.can_undo());
    assert!(!history.discard_oldest());
}

#[test]
fn lowered_count_bounds_both_stacks_without_loading() {
    let mut history = History::new(50);
    let mut current = Arc::new(base());
    for label in ["A", "B", "C"] {
        edit(&mut history, &mut current, label, |doc| doc.name = label.into());
    }
    current = history.undo(current).unwrap().0;
    history.max_states = 1;
    assert_eq!(history.enforce_state_limit(), 2);
    assert_eq!(history.past_len(), 0);
    assert_eq!(history.redo_label(), Some("C"));
    assert_eq!(history.redo(current).unwrap().0.name, "C");
}
#[test]
fn asynchronous_candidate_can_become_hot_after_navigation() {
    let mut history = History::new(50);
    let mut current = Arc::new(base());
    let oldest = current.clone();
    for label in ["A", "B", "C"] {
        edit(&mut history, &mut current, label, |doc| doc.name = label.into());
    }
    assert!(!history.is_hot(&oldest));
    current = history.undo(current).unwrap().0;
    let _current = history.undo(current).unwrap().0;
    assert!(history.is_hot(&oldest));
    assert!(history.spill_candidate().is_none_or(|candidate| !Arc::ptr_eq(&candidate, &oldest)));
}
#[test]
fn large_shared_binary_payloads_are_counted_once() {
    let mut doc = base();
    let bytes = Arc::new(vec![17; 4096]);
    doc.icc_profile = Some(bytes.clone());
    doc.metadata.exif = Some(bytes.clone());
    doc.metadata.psd_resources.push((1, "resource".into(), bytes.clone()));
    doc.metadata.psd_global_blocks.push((*b"8BIM", *b"lnk2", bytes.clone()));
    doc.layers[0].psd_blocks.push((*b"test", bytes));
    let baseline = 256 * 256 * 4;
    let mut seen = HashSet::new();
    assert_eq!(document_bytes(&doc, &mut seen), baseline + 4096);
    let mut clone = doc.clone();
    clone.metadata.exif = Some(Arc::new(vec![4; 1024]));
    assert_eq!(document_bytes(&clone, &mut seen), 1024);
}

fn stepped_back(non_linear: bool) -> (History, Arc<Document>) {
    let mut h = History { non_linear, ..History::default() };
    let mut cur = Arc::new(base());
    for n in ["A", "B", "C"] {
        edit(&mut h, &mut cur, n, |d| d.name = n.to_lowercase());
    }
    cur = h.undo(cur).unwrap().0;
    cur = h.undo(cur).unwrap().0;
    assert_eq!(cur.name, "a");
    (h, cur)
}

fn non_linear_history_keeps_later_states_and_appends_the_new_step() {
    let (mut h, mut cur) = stepped_back(true);
    edit(&mut h, &mut cur, "D", |d| d.name = format!("{}+d", d.name));
    assert_eq!(h.entries(), ["Open", "A", "B", "C", "D"], "B and C are kept, D goes to the end");
    assert!(!h.can_redo());
    assert_eq!(cur.name, "a+d", "D was made from the state selected (A), not from C");
    // Stepping back walks the list: C, B, A, Open, each with its own document.
    let mut names = Vec::new();
    while let Some((d, _)) = h.undo(cur.clone()) {
        cur = d;
        names.push(cur.name.clone());
    }
    assert_eq!(names, ["c", "b", "a", "h"]);
    // Linear history (the default) discards them.
    let (mut h, mut cur) = stepped_back(false);
    edit(&mut h, &mut cur, "D", |d| d.name = "d".into());
    assert_eq!(h.entries(), ["Open", "A", "D"]);
}

fn non_linear_steps_respect_the_state_limit_and_the_tile_counter() {
    let mut h = History::new(3);
    h.non_linear = true;
    h.max_bytes = usize::MAX;
    let mut cur = Arc::new(base());
    for i in 0..4 {
        edit(&mut h, &mut cur, &format!("paint {i}"), |d| {
            d.layers[0].surface_mut().unwrap().write_pixel(1, 1, &[i as f32 / 8.0, 0.0, 0.0, 1.0]);
        });
        h.trim(&cur);
    }
    cur = h.undo(cur).unwrap().0;
    cur = h.undo(cur).unwrap().0;
    edit(&mut h, &mut cur, "branch", |d| d.name = "branch".into());
    assert_eq!(h.past_len(), 3, "the oldest states beyond the limit go");
    assert_eq!(h.entries(), ["paint 1", "paint 2", "paint 3", "branch"]);
    assert_counter(&h, &cur);
}

fn remove_deletes_one_past_state_and_drop_current_keeps_the_redo_states() {
    let (mut h, _) = stepped_back(true);
    assert!(!h.remove(0), "the oldest state stays");
    assert!(!h.remove(1), "the current state (A) is not held");
    assert!(!h.remove(99));
    let (mut h, mut cur) = stepped_back(true);
    edit(&mut h, &mut cur, "D", |d| d.name = "d".into());
    assert!(h.remove(1));
    assert_eq!(h.entries(), ["Open", "B", "C", "D"]);
    assert_eq!(h.state(1).unwrap().name, "b", "each state keeps its own document");
    // Deleting the current state: the one before becomes current, the later ones stay.
    let (mut h, _) = stepped_back(true);
    let (d, _) = h.drop_current().unwrap();
    assert_eq!(d.name, "h");
    assert_eq!(h.entries(), ["Open"]);
    assert_eq!(h.redo_labels().collect::<Vec<_>>(), ["B", "C"]);
    assert!(h.drop_current().is_none(), "nothing before Open");
    // The tile counter follows both.
    let mut h = History::new(50);
    h.non_linear = true;
    h.max_bytes = usize::MAX;
    let mut cur = Arc::new(base());
    for i in 0..4 {
        edit(&mut h, &mut cur, "Paint", |d| {
            d.layers[0].surface_mut().unwrap().write_pixel(1, 1, &[i as f32 / 8.0, 0.0, 0.0, 1.0]);
        });
        h.trim(&cur);
    }
    cur = h.undo(cur).unwrap().0;
    assert!(h.remove(2));
    assert_counter(&h, &cur);
    cur = h.drop_current().unwrap().0;
    assert_counter(&h, &cur);
}

fn remove_later_deletes_one_redo_state() {
    let mut h = History::new(50);
    h.non_linear = true;
    h.max_bytes = usize::MAX;
    let mut cur = Arc::new(base());
    for (i, n) in ["A", "B", "C"].into_iter().enumerate() {
        edit(&mut h, &mut cur, n, |d| {
            d.layers[0].surface_mut().unwrap().write_pixel(1, 1, &[i as f32 / 8.0, 0.0, 0.0, 1.0]);
        });
        h.trim(&cur);
    }
    cur = h.undo(cur).unwrap().0;
    cur = h.undo(cur).unwrap().0;
    assert!(!h.remove_later(2) && !h.remove_later(usize::MAX));
    assert!(h.remove_later(0), "B");
    assert_eq!(h.redo_labels().collect::<Vec<_>>(), ["C"]);
    assert_counter(&h, &cur);
    let (c, _) = h.redo(cur).unwrap();
    assert_eq!(h.entries(), ["Open", "A", "C"]);
    assert_counter(&h, &c);
}

fn step_visibility_follows_steps_and_spans_removed_ones() {
    let vis = |h: &History| {
        let mut v: Vec<u64> = h.current_visibility().iter().map(|l| l.0).collect();
        v.sort_unstable();
        v
    };
    // Open, then steps A, B, C, D changing layers 1, 2, 3, 4.
    let steps = |n: usize, non_linear: bool| {
        let mut h = History { non_linear, ..History::default() };
        let mut cur = Arc::new(base());
        for (i, label) in ["A", "B", "C", "D"].into_iter().take(n).enumerate() {
            edit(&mut h, &mut cur, label, |d| d.name = label.to_lowercase());
            h.add_current_visibility(&[LayerId(i as u64 + 1)]);
        }
        (h, cur)
    };
    let (mut h, cur) = steps(3, false);
    assert_eq!(vis(&h), [3]);
    let (cur, _) = h.undo(cur).unwrap();
    assert_eq!(vis(&h), [2]);
    h.redo(cur).unwrap();
    assert_eq!(vis(&h), [3]);
    // Purge: C now starts from A.
    h.purge_last();
    assert_eq!(vis(&h), [2, 3]);
    // Non-linear Delete of B (between A and C): C now follows A.
    let (mut h, cur) = steps(4, true);
    assert!(h.remove(2));
    h.undo(cur).unwrap();
    assert_eq!(vis(&h), [2, 3]);
    // Of the later B (A current): C follows A.
    let (mut h, mut cur) = steps(3, true);
    cur = h.undo(cur).unwrap().0;
    cur = h.undo(cur).unwrap().0;
    assert!(h.remove_later(0));
    h.redo(cur).unwrap();
    assert_eq!(vis(&h), [2, 3]);
    // Of the current B (C later): A is current, C follows it.
    let (mut h, cur) = steps(3, true);
    h.undo(cur).unwrap();
    let cur = h.drop_current().unwrap().0;
    assert_eq!(vis(&h), [1]);
    h.redo(cur).unwrap();
    assert_eq!(vis(&h), [2, 3]);
    // A step after the kept later states B and C (made from A) follows C in the list.
    let (mut h, mut cur) = steps(3, true);
    cur = h.undo(cur).unwrap().0;
    cur = h.undo(cur).unwrap().0;
    edit(&mut h, &mut cur, "D", |d| d.name = "d".into());
    h.add_current_visibility(&[LayerId(4)]);
    assert_eq!(vis(&h), [2, 3, 4]);
    h.rollback().unwrap();
    assert_eq!(vis(&h), [1], "rollback restores A's");
    h.clear();
    assert_eq!(vis(&h), Vec::<u64>::new());
}

fn rollback_takes_the_last_record_back_exactly_in_both_modes() {
    // Non-linear, with redo states: the moved states go back to the redo side and the
    // document the step replaced (A) is current again, not the last former redo state (C).
    let (mut h, mut cur) = stepped_back(true);
    let a = cur.clone();
    let entries = h.entries();
    let redo: Vec<String> = h.redo_labels().map(str::to_string).collect();
    edit(&mut h, &mut cur, "D", |d| d.name = "d".into());
    assert_eq!(h.recorded_before().map(|d| d.name.clone()).as_deref(), Some("a"));
    let (d, _) = h.rollback().unwrap();
    assert!(Arc::ptr_eq(&d, &a));
    assert_eq!(h.entries(), entries);
    assert_eq!(h.redo_labels().map(str::to_string).collect::<Vec<_>>(), redo);
    assert!(h.rollback().is_none(), "only once");
    // Redoing still walks the kept states.
    let (b, _) = h.redo(d).unwrap();
    assert_eq!(b.name, "b");
    // Linear: the same as an undo whose redo state is dropped.
    let (mut h, mut cur) = stepped_back(false);
    edit(&mut h, &mut cur, "D", |d| d.name = "d".into());
    assert_eq!(h.rollback().unwrap().0.name, "a");
    assert_eq!(h.entries(), ["Open", "A"]);
    assert!(!h.can_redo());
    // Anything else since the record: no rollback (the caller undoes instead).
    let (mut h, mut cur) = stepped_back(true);
    edit(&mut h, &mut cur, "D", |d| d.name = "d".into());
    cur = h.undo(cur).unwrap().0;
    assert!(h.recorded_before().is_none() && h.rollback().is_none());
    h.redo(cur).unwrap();
    assert!(h.rollback().is_none());
    // The tile counter follows.
    let mut h = History::new(50);
    h.non_linear = true;
    h.max_bytes = usize::MAX;
    let mut cur = Arc::new(base());
    for i in 0..3 {
        edit(&mut h, &mut cur, "Paint", |d| {
            d.layers[0].surface_mut().unwrap().write_pixel(1, 1, &[i as f32 / 8.0, 0.0, 0.0, 1.0]);
        });
        h.trim(&cur);
    }
    cur = h.undo(cur).unwrap().0;
    cur = h.undo(cur).unwrap().0;
    edit(&mut h, &mut cur, "Paint", |d| {
        d.layers[0].surface_mut().unwrap().write_pixel(2, 2, &[1.0, 0.0, 0.0, 1.0]);
    });
    cur = h.rollback().unwrap().0;
    assert_counter(&h, &cur);
}

fn rollback_brings_back_the_states_the_limit_dropped_for_the_step() {
    for max in 1..=4 {
        let (mut h, mut cur) = stepped_back(true);
        h.max_states = max;
        let a = cur.clone();
        edit(&mut h, &mut cur, "D", |d| d.name = "d".into());
        assert!(h.past_len() <= max);
        assert_eq!(h.recorded_before().map(|d| d.name.clone()).as_deref(), Some("a"), "max {max}");
        let (d, _) = h.rollback().unwrap();
        assert!(Arc::ptr_eq(&d, &a), "max {max}: back on A, not on an Undo of C");
        assert_eq!(h.entries(), ["Open", "A"], "max {max}");
        assert_eq!(h.redo_labels().collect::<Vec<_>>(), ["B", "C"], "max {max}");
        assert!(h.rollback().is_none());
    }
    // Once anything else changed the list, the dropped states stay dropped.
    let (mut h, mut cur) = stepped_back(true);
    h.max_states = 2;
    edit(&mut h, &mut cur, "D", |d| d.name = "d".into());
    cur = h.undo(cur).unwrap().0;
    h.redo(cur).unwrap();
    assert!(h.rollback().is_none());
    assert_eq!(h.entries(), ["B", "C", "D"]);
    // The byte budget: the trim after the step keeps only its newest state; the rollback
    // brings back the rest and the tile counter follows.
    let mut h = History { non_linear: true, ..History::default() };
    let mut cur = Arc::new(base());
    for (i, n) in ["A", "B", "C"].into_iter().enumerate() {
        edit(&mut h, &mut cur, n, |d| {
            d.layers[0].surface_mut().unwrap().write_pixel(1, 1, &[i as f32 / 8.0, 0.0, 0.0, 1.0]);
        });
    }
    cur = h.undo(cur).unwrap().0;
    cur = h.undo(cur).unwrap().0;
    h.max_bytes = 1;
    edit(&mut h, &mut cur, "D", |d| {
        d.layers[0].surface_mut().unwrap().write_pixel(2, 2, &[1.0, 0.0, 0.0, 1.0]);
    });
    assert!(h.trim(&cur) > 0);
    assert_eq!(h.entries(), ["C", "D"]);
    assert_counter(&h, &cur);
    cur = h.rollback().unwrap().0;
    assert_eq!(h.entries(), ["Open", "A"]);
    assert_eq!(h.redo_labels().collect::<Vec<_>>(), ["B", "C"]);
    assert_counter(&h, &cur);
}

fn rollback_puts_states_back_behind_the_pinned_brush_source() {
    let (mut h, mut cur) = stepped_back(true);
    assert!(h.set_brush_source(0));
    h.max_states = 2;
    let a = cur.clone();
    edit(&mut h, &mut cur, "D", |d| d.name = "d".into());
    assert_eq!(h.entries(), ["Open", "C", "D"], "A and B go, the source (Open) stays");
    let (d, _) = h.rollback().unwrap();
    assert!(Arc::ptr_eq(&d, &a));
    assert_eq!(h.entries(), ["Open", "A"]);
    assert_eq!(h.redo_labels().collect::<Vec<_>>(), ["B", "C"]);
    assert_eq!(h.brush_source(), Some(0));
    assert_eq!(h.state(0).unwrap().name, "h");
    // The byte budget, with the source on the oldest state.
    let mut h = History { non_linear: true, ..History::default() };
    let mut cur = Arc::new(base());
    for (i, n) in ["A", "B", "C"].into_iter().enumerate() {
        edit(&mut h, &mut cur, n, |d| {
            d.layers[0].surface_mut().unwrap().write_pixel(1, 1, &[i as f32 / 8.0, 0.0, 0.0, 1.0]);
        });
    }
    cur = h.undo(cur).unwrap().0;
    cur = h.undo(cur).unwrap().0;
    assert!(h.set_brush_source(0));
    h.max_bytes = 1;
    edit(&mut h, &mut cur, "D", |d| {
        d.layers[0].surface_mut().unwrap().write_pixel(2, 2, &[1.0, 0.0, 0.0, 1.0]);
    });
    assert!(h.trim(&cur) > 0);
    assert_eq!(h.entries(), ["Open", "C", "D"]);
    cur = h.rollback().unwrap().0;
    assert_eq!(cur.name, "h");
    assert_eq!(h.entries(), ["Open", "A"]);
    assert_eq!(h.redo_labels().collect::<Vec<_>>(), ["B", "C"]);
    assert_eq!(h.state(0).unwrap().name, "h");
    assert_counter(&h, &cur);
}

/// The history's pixel accounting: the current document's tiles plus what only the history holds
/// (Capishot counts it on demand rather than through a running tile counter).
fn assert_counter(h: &History, current: &Document) {
    let total = document_bytes(current, &mut HashSet::new()).saturating_add(h.unique_bytes(current));
    assert_eq!(h.pixel_bytes(current), total);
}
