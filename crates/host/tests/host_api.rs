//! The host API end to end, natively: documents from bytes, commands, history, groups, pixels,
//! jobs, events and live sync between two hosts.

use capishot_host::live::{Applied, Envelope, Op};
use capishot_host::{
    BatchSteps, DocumentId, Error, Event, ExecOptions, GetPixels, Host, InspectOptions, JobEnd, LayerId, OpenOptions, Origin, PutPixels, SaveFormat, SaveOptions, SaveStarted, Started,
};
use serde_json::{Value, json};

fn host_with_doc() -> (Host, DocumentId) {
    let mut h = Host::new();
    h.execute("file.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
    let doc = h.active_document().unwrap();
    h.poll();
    (h, doc)
}

fn new_layer(h: &mut Host, name: &str) -> LayerId {
    h.execute("layer.new.layer", json!({"name": name})).unwrap();
    let doc = h.active_document().unwrap();
    active_layer(h, doc)
}

fn active_layer(h: &Host, doc: DocumentId) -> LayerId {
    LayerId(h.inspect(doc, InspectOptions::default()).unwrap()["activeLayer"].as_u64().unwrap())
}

fn layers(h: &Host, doc: DocumentId) -> Vec<Value> {
    h.inspect(doc, InspectOptions::default()).unwrap()["layers"].as_array().unwrap().clone()
}

fn fill_rect(h: &mut Host, color: &str, rect: [i32; 4]) {
    h.execute("select.rect", json!({"x": rect[0], "y": rect[1], "width": rect[2], "height": rect[3], "antiAlias": false})).unwrap();
    h.execute("edit.fill", json!({"contents": "color", "color": color})).unwrap();
    h.execute("select.deselect", json!({})).ok();
}

#[test]
fn commands_report_events_once() {
    let (mut h, doc) = host_with_doc();
    let layer = new_layer(&mut h, "Ink");
    let events = h.poll();
    assert!(events.iter().any(|e| matches!(e, Event::LayersChanged { document, layer: l, what, origin: Origin::Local, .. } if *document == doc && *l == layer && what == &["added"])), "{events:?}");
    assert!(events.iter().any(|e| matches!(e, Event::DocumentChanged { document, dirty: true, .. } if *document == doc)));
    assert!(events.iter().any(|e| matches!(e, Event::HistoryChanged { document, .. } if *document == doc)));
    // Nothing changed since.
    assert!(h.poll().is_empty());

    fill_rect(&mut h, "#ff0000", [8, 8, 16, 16]);
    let events = h.poll();
    let px = events.iter().find_map(|e| match e {
        Event::LayerPixelsChanged { layer: l, rect, uid, .. } if *l == layer => Some((*rect, *uid)),
        _ => None,
    });
    let (rect, uid) = px.expect("pixels changed");
    assert!(rect[2] > 0 && rect[3] > 0 && uid.is_some(), "{rect:?}");
    // Events serialise as the embed's JSON.
    let j = events.iter().find(|e| matches!(e, Event::LayerPixelsChanged { .. })).unwrap().to_json();
    assert_eq!(j["type"], "layerPixelsChanged");
    assert!(j["pixelRev"].is_u64() && j["rect"].is_array(), "{j}");

    h.execute("select.all", json!({})).unwrap();
    assert!(h.poll().contains(&Event::SelectionChanged { document: doc, bounds: Some([0, 0, 64, 48]) }));
}

#[test]
fn subscribers_and_channels_get_the_events() {
    let (mut h, _) = host_with_doc();
    let seen = std::rc::Rc::new(std::cell::Cell::new(0));
    let s = seen.clone();
    let sub = h.subscribe(move |_| s.set(s.get() + 1));
    let rx = h.events();
    new_layer(&mut h, "A");
    let n = h.poll().len();
    assert!(n > 0);
    assert_eq!(seen.get(), n);
    assert_eq!(rx.try_iter().count(), n);
    h.unsubscribe(sub);
    new_layer(&mut h, "B");
    h.poll();
    assert_eq!(seen.get(), n);
}

#[test]
fn tool_changes_are_events() {
    let (mut h, _) = host_with_doc();
    h.set_tool("removeAi").unwrap();
    assert_eq!(h.poll(), vec![Event::ToolChanged { tool: "removeAi".into() }]);
    assert!(h.set_tool("").is_err() && h.set_tool("no tool!").is_err());
    assert_eq!(h.tool(), "removeAi");
}

#[test]
fn psd_round_trips_through_bytes_and_hidden_documents_stay_hidden() {
    let (mut h, doc) = host_with_doc();
    new_layer(&mut h, "Ink");
    fill_rect(&mut h, "#00ff00", [0, 0, 10, 10]);
    let psd = h.save_psd(doc).unwrap();
    assert!(psd.starts_with(b"8BPS"));
    assert!(!h.documents().iter().find(|d| d.id == doc).unwrap().dirty, "a PSD save marks it saved");
    // Incremental: a second save gives the same bytes.
    assert_eq!(h.save_psd(doc).unwrap(), psd);

    let copy = h.open(psd, OpenOptions { name: Some("page.psd".into()), hidden: true }).unwrap().document;
    assert_eq!(h.active_document(), Some(doc), "a hidden open leaves the active document");
    let info = h.documents().into_iter().find(|d| d.id == copy).unwrap();
    assert!(info.hidden && !info.active);
    assert_eq!(layers(&h, copy).len(), layers(&h, doc).len());
    let events = h.poll();
    assert!(!events.iter().any(|e| matches!(e, Event::DocumentsChanged { .. })), "hidden documents aren't listed: {events:?}");

    // Commands reach it by id.
    h.execute_with("layer.new.layer", json!({"name": "Helper"}), ExecOptions::on(copy)).unwrap();
    assert_eq!(h.active_document(), Some(doc));
    assert_eq!(layers(&h, copy).len(), layers(&h, doc).len() + 1);

    h.show(copy, true).unwrap();
    assert_eq!(h.active_document(), Some(copy));
    assert!(h.documents().into_iter().find(|d| d.id == doc).unwrap().hidden);

    assert!(h.close(copy, false).is_err(), "unsaved changes refuse");
    h.close(copy, true).unwrap();
    assert!(h.inspect(copy, InspectOptions::default()).is_err());
}

#[test]
fn background_open_and_save_jobs_finish_at_poll() {
    let (mut h, doc) = host_with_doc();
    let psd = h.save_psd(doc).unwrap();
    let id = match h.start_open(psd.clone(), OpenOptions { name: Some("bg.psd".into()), hidden: true }).unwrap() {
        Started::Done(v) => v["document"].as_u64().unwrap(),
        Started::Job(job) => h.wait(job).unwrap()["document"].as_u64().unwrap(),
    };
    assert!(h.documents().iter().any(|d| d.id == DocumentId(id) && d.hidden));
    let events = h.poll();
    assert!(events.iter().any(|e| matches!(e, Event::JobFinished { end: JobEnd::Done { .. }, .. })), "{events:?}");

    let bytes = match h.start_save(doc, &SaveOptions::default()).unwrap() {
        SaveStarted::Done(b) => b.bytes,
        SaveStarted::Job(job) => {
            let mut tries = 0;
            loop {
                h.poll();
                if let Some(b) = h.take_saved(job) {
                    break b.bytes;
                }
                tries += 1;
                assert!(tries < 10_000, "the save never finished");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    };
    assert_eq!(bytes, psd);
}

#[test]
fn long_commands_run_as_jobs_and_cancel() {
    let (mut h, doc) = host_with_doc();
    new_layer(&mut h, "Ink");
    fill_rect(&mut h, "#0000ff", [4, 4, 30, 30]);
    h.poll();
    match h.start("filter.blur.gaussianBlur", json!({"radius": 3}), Some(doc)).unwrap() {
        Started::Done(_) => {}
        Started::Job(job) => {
            h.wait(job).unwrap();
            let events = h.poll();
            assert!(events.iter().any(|e| matches!(e, Event::JobFinished { job: j, end: JobEnd::Done { .. }, .. } if *j == job)), "{events:?}");
            assert!(events.iter().any(|e| matches!(e, Event::LayerPixelsChanged { .. })));
        }
    }
    // Cancel: the document is unchanged.
    let rev = h.documents()[0].revision;
    if let Started::Job(job) = h.start("filter.blur.gaussianBlur", json!({"radius": 200}), Some(doc)).unwrap() {
        assert!(h.job(job).is_some());
        assert!(h.cancel(job));
        let events = h.poll();
        assert!(events.iter().any(|e| matches!(e, Event::JobFinished { end: JobEnd::Cancelled, .. })), "{events:?}");
        assert_eq!(h.documents()[0].revision, rev);
    }
}

#[test]
fn history_none_survives_the_persons_undo() {
    let (mut h, doc) = host_with_doc();
    let mine = new_layer(&mut h, "Mine");
    let theirs = new_layer(&mut h, "Theirs");
    h.execute("layer.setProps", json!({"layer": mine.0, "opacity": 0.5})).unwrap();
    h.execute_with("layer.setProps", json!({"layer": theirs.0, "opacity": 0.25}), ExecOptions::silent(doc)).unwrap();
    assert!(h.undo(doc).unwrap());
    let ls = layers(&h, doc);
    let opacity = |id: LayerId| ls.iter().find(|l| l["id"] == id.0).unwrap()["opacity"].as_f64().unwrap();
    assert!((opacity(mine) - 1.0).abs() < 1e-3, "the person's step was undone");
    assert!((opacity(theirs) - 0.25).abs() < 1e-3, "the collaborator's change stays");
    assert!(h.redo(doc).unwrap());
}

#[test]
fn a_group_is_one_undo_step_and_an_error_undoes_it() {
    let (mut h, doc) = host_with_doc();
    let before = layers(&h, doc).len();
    h.group(Some(doc), |h| {
        h.execute("layer.new.layer", json!({"name": "A"}))?;
        h.execute("layer.new.layer", json!({"name": "B"}))?;
        Ok(())
    })
    .unwrap();
    assert_eq!(layers(&h, doc).len(), before + 2);
    h.undo(doc).unwrap();
    assert_eq!(layers(&h, doc).len(), before, "one undo takes back the whole group");

    let r: capishot_host::Result<()> = h.group(Some(doc), |h| {
        h.execute("layer.new.layer", json!({"name": "C"}))?;
        h.execute("no.such.command", json!({}))?;
        Ok(())
    });
    assert!(matches!(r, Err(Error::UnknownCommand(_))), "{r:?}");
    assert_eq!(layers(&h, doc).len(), before, "a failed group leaves nothing");
    assert!(!h.in_group());

    h.begin_group(Some(doc)).unwrap();
    assert!(h.begin_group(None).is_err(), "no nesting");
    assert!(h.undo(doc).is_err(), "no undo in a group");
    assert!(h.execute("file.new", json!({"width": 8, "height": 8})).is_err(), "no file commands in a group");
    h.end_group(true).unwrap();
    assert!(h.end_group(true).is_err());
}

#[test]
fn batches_and_requests() {
    let (mut h, doc) = host_with_doc();
    let before = layers(&h, doc).len();
    let r = h.batch(doc, &[("layer.new.layer", json!({"name": "A"})), ("layer.new.layer", json!({"name": "B"}))], BatchSteps::One).unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(layers(&h, doc).len(), before + 2);
    let v = h.execute_request(&json!({"command": "document.inspect", "document": doc.0})).unwrap();
    assert_eq!(v["id"], doc.0);
    assert!(h.execute_request(&json!({"params": {}})).is_err());
    assert!(h.execute_request(&json!({"command": "document.inspect", "history": "sometimes"})).is_err());
}

#[test]
fn pixels_read_and_write() {
    let (mut h, doc) = host_with_doc();
    let a = new_layer(&mut h, "A");
    fill_rect(&mut h, "#ff0000", [0, 0, 8, 8]);
    let px = h.get_pixels(doc, a, GetPixels { rect: Some([0, 0, 8, 8]), ..Default::default() }).unwrap();
    assert_eq!((px.width, px.height, px.channels), (8, 8, 4));
    assert!(px.data.starts_with(b"\x89PNG"));
    let raw = h.get_pixels(doc, a, GetPixels { rect: Some([0, 0, 2, 1]), raw: true, ..Default::default() }).unwrap();
    assert_eq!(raw.data, vec![255, 0, 0, 255, 255, 0, 0, 255]);

    let b = new_layer(&mut h, "B");
    h.poll();
    h.put_pixels(doc, b, &px.data, PutPixels { x: 20, y: 10, ..Default::default() }).unwrap();
    let events = h.poll();
    assert!(events.iter().any(|e| matches!(e, Event::LayerPixelsChanged { layer, .. } if *layer == b)), "{events:?}");
    let back = h.get_pixels(doc, b, GetPixels { rect: Some([20, 10, 1, 1]), raw: true, ..Default::default() }).unwrap();
    assert_eq!(back.data, vec![255, 0, 0, 255]);
}

#[test]
fn views_are_reachable() {
    let (mut h, doc) = host_with_doc();
    let a = new_layer(&mut h, "A");
    h.define_view(doc, "clean", json!({"hidden": [a.0]})).unwrap();
    h.apply_view(doc, "clean").unwrap();
    assert_eq!(layers(&h, doc).iter().find(|l| l["id"] == a.0).unwrap()["visible"], false);
    assert_eq!(h.views(doc).unwrap()["views"].as_array().unwrap().len(), 1);
    assert!(h.define_view(doc, "x", json!(3)).is_err());
}

#[test]
fn bad_input_is_an_error_not_a_panic() {
    let mut h = Host::new();
    assert!(matches!(h.execute("no.such", json!({})), Err(Error::UnknownCommand(_))));
    assert!(h.inspect(DocumentId(999_999), InspectOptions::default()).is_err());
    assert!(h.save_psd(DocumentId(999_999)).is_err());
    assert!(h.undo(DocumentId(999_999)).is_err());
    assert!(h.open(b"not an image".to_vec(), OpenOptions::default()).is_err());
    assert!(h.open(Vec::new(), OpenOptions { name: Some("".into()), hidden: true }).is_err());
    let (mut h, doc) = host_with_doc();
    assert!(h.get_pixels(doc, LayerId(999_999), GetPixels::default()).is_err());
    assert!(h.put_pixels(doc, LayerId(999_999), b"junk", PutPixels::default()).is_err());
    assert!(SaveFormat::parse("../x").is_err());
    assert!(h.save(doc, &SaveOptions::new(SaveFormat::Jpeg { quality: 0 })).is_err());
    assert!(h.save(DocumentId(u64::MAX), &SaveOptions::default()).is_err());
    assert!(h.execute("layer.setProps", json!({"opacity": "loud"})).is_err());
    assert!(h.live_apply(doc, &Envelope { op: Op::Removed { layer: capishot_host::LayerUid(1) }, stamp: 1, origin: "x".into() }).is_err(), "not live");
}

// ---------- live sync ----------

/// Two peers with the same page open (from the same PSD bytes), both live.
fn two_peers() -> (Host, DocumentId, Host, DocumentId) {
    let (mut a, da) = host_with_doc();
    new_layer(&mut a, "Ink");
    fill_rect(&mut a, "#ff0000", [0, 0, 16, 16]);
    new_layer(&mut a, "Top");
    let psd = a.save_psd(da).unwrap();
    let mut b = Host::new();
    let db = b.open(psd.clone(), OpenOptions::named("page.psd")).unwrap().document;
    // Peer A works on its own copy of the same file too.
    let da = a.open(psd, OpenOptions::named("page.psd")).unwrap().document;
    a.poll();
    b.poll();
    a.live_start(da, "alice").unwrap();
    b.live_start(db, "bob").unwrap();
    (a, da, b, db)
}

fn by_name<'a>(ls: &'a [Value], name: &str) -> &'a Value {
    ls.iter().find(|l| l["name"] == name).unwrap()
}

/// Send `from`'s local ops to `to` over JSON, as a server would.
fn sync(from: &mut Host, fd: DocumentId, to: &mut Host, td: DocumentId) -> Vec<Envelope> {
    from.poll();
    let ops = from.live_take_local(fd).unwrap();
    let wire = serde_json::to_string(&ops).unwrap();
    let ops: Vec<Envelope> = serde_json::from_str(&wire).unwrap();
    for op in &ops {
        assert_eq!(to.live_apply(td, op).unwrap(), Applied::Applied, "{:?}", op.op);
    }
    ops
}

#[test]
fn live_pixels_settings_and_order_reach_the_other_peer() {
    let (mut a, da, mut b, db) = two_peers();
    let ink = LayerId(by_name(&layers(&a, da), "Ink")["id"].as_u64().unwrap());
    a.execute("layer.select", json!({"layer": ink.0})).unwrap();
    fill_rect(&mut a, "#0000ff", [20, 20, 8, 8]);
    a.execute("layer.setProps", json!({"layer": ink.0, "opacity": 0.5, "name": "Ink 2"})).unwrap();
    a.execute("layer.arrange.bringToFront", json!({})).unwrap();
    let ops = sync(&mut a, da, &mut b, db);
    assert!(ops.iter().any(|e| matches!(e.op, Op::Pixels { mask: false, .. })), "{ops:?}");
    assert!(ops.iter().any(|e| matches!(e.op, Op::Layer { .. })));
    assert!(ops.iter().any(|e| matches!(e.op, Op::Reordered { .. })));

    let lb = layers(&b, db);
    let ink_b = by_name(&lb, "Ink 2");
    assert!((ink_b["opacity"].as_f64().unwrap() - 0.5).abs() < 1e-3);
    assert_eq!(lb.first().unwrap()["name"], "Ink 2", "brought to front");
    let id_b = LayerId(ink_b["id"].as_u64().unwrap());
    let px = b.get_pixels(db, id_b, GetPixels { rect: Some([21, 21, 1, 1]), raw: true, ..Default::default() }).unwrap();
    assert_eq!(px.data, vec![0, 0, 255, 255]);
    let old = b.get_pixels(db, id_b, GetPixels { rect: Some([1, 1, 1, 1]), raw: true, ..Default::default() }).unwrap();
    assert_eq!(old.data, vec![255, 0, 0, 255], "pixels outside the op keep");

    // Remote changes aren't undo steps and aren't sent back.
    let events = b.poll();
    assert!(events.iter().all(|e| !matches!(e, Event::LayerPixelsChanged { origin: Origin::Local, .. })), "{events:?}");
    assert!(b.live_take_local(db).unwrap().is_empty());
    assert!(!b.undo(db).unwrap() || layers(&b, db).iter().any(|l| l["name"] == "Ink 2"));
}

#[test]
fn live_text_and_new_and_deleted_layers() {
    let (mut a, da, mut b, db) = two_peers();
    a.execute("type.create", json!({"text": "Hola", "point": [10, 30], "size": 12})).unwrap();
    sync(&mut a, da, &mut b, db);
    let text_b = layers(&b, db).into_iter().find(|l| l["kind"] == "text" || l["type"] == "text" || l["text"].is_object());
    assert!(text_b.is_some(), "the text layer was added: {:?}", layers(&b, db));
    let n = layers(&b, db).len();
    assert_eq!(n, layers(&a, da).len());

    let top = by_name(&layers(&a, da), "Top")["id"].as_u64().unwrap();
    a.execute("layer.select", json!({"layer": top})).unwrap();
    a.execute("layer.delete", json!({})).unwrap();
    let ops = sync(&mut a, da, &mut b, db);
    assert!(ops.iter().any(|e| matches!(e.op, Op::Removed { .. })), "{ops:?}");
    assert_eq!(layers(&b, db).len(), n - 1);
    assert!(!layers(&b, db).iter().any(|l| l["name"] == "Top"));
}

#[test]
fn live_last_writer_wins_per_layer() {
    let (mut a, da, mut b, db) = two_peers();
    let ink_a = by_name(&layers(&a, da), "Ink")["id"].as_u64().unwrap();
    a.execute("layer.setProps", json!({"layer": ink_a, "opacity": 0.3})).unwrap();
    a.poll();
    let ops = a.live_take_local(da).unwrap();
    let layer_op = ops.iter().find(|e| matches!(e.op, Op::Layer { .. })).unwrap().clone();

    // Bob changed the same layer later (a higher stamp): Alice's older op loses.
    let ink_b = by_name(&layers(&b, db), "Ink")["id"].as_u64().unwrap();
    for _ in 0..3 {
        b.execute("layer.setProps", json!({"layer": ink_b, "opacity": 0.9})).unwrap();
        b.poll();
        b.live_take_local(db).unwrap();
    }
    let stale = Envelope { stamp: 1, ..layer_op.clone() };
    assert_eq!(b.live_apply(db, &stale).unwrap(), Applied::Stale);
    assert!((by_name(&layers(&b, db), "Ink")["opacity"].as_f64().unwrap() - 0.9).abs() < 1e-3);

    // A newer one wins; replaying it again is stale.
    let newer = Envelope { stamp: 100, ..layer_op };
    assert_eq!(b.live_apply(db, &newer).unwrap(), Applied::Applied);
    assert!((by_name(&layers(&b, db), "Ink")["opacity"].as_f64().unwrap() - 0.3).abs() < 1e-3);
    assert_eq!(b.live_apply(db, &newer).unwrap(), Applied::Stale);

    // Unknown layers are skipped, junk payloads are errors.
    let missing = Envelope { op: Op::Removed { layer: capishot_host::LayerUid(77_777) }, stamp: 200, origin: "alice".into() };
    assert!(matches!(b.live_apply(db, &missing).unwrap(), Applied::Skipped(_)));
    let junk = Envelope { op: Op::Layer { layer: capishot_host::LayerUid(1), parent: None, above: None, pcraft: b"junk".to_vec() }, stamp: 300, origin: "alice".into() };
    assert!(b.live_apply(db, &junk).is_err());
    // A command op.
    let cmd = Envelope { op: Op::Command { command: "layer.new.layer".into(), params: json!({"name": "Agent"}) }, stamp: 301, origin: "agent".into() };
    assert_eq!(b.live_apply(db, &cmd).unwrap(), Applied::Applied);
    assert!(layers(&b, db).iter().any(|l| l["name"] == "Agent"));
}

#[test]
fn saves_are_incremental_flat_formats_work_and_edits_during_a_save_keep_it_dirty() {
    let (mut h, doc) = host_with_doc();
    new_layer(&mut h, "Ink");
    let first = h.save(doc, &SaveOptions::default()).unwrap();
    assert!(first.bytes.starts_with(b"8BPS"));
    assert!(!h.documents()[0].dirty);
    // Nothing changed: the second save encodes no layer again.
    let second = h.save(doc, &SaveOptions::default()).unwrap();
    assert_eq!(second.encoded_layers, 0, "{second:?}");
    assert_eq!(first.bytes.len(), second.bytes.len());
    for f in ["png", "jpg", "webp", "tiff", "pcraft", "psb"] {
        let saved = h.save(doc, &SaveOptions::new(SaveFormat::parse(f).unwrap())).unwrap();
        assert!(!saved.bytes.is_empty(), "{f}");
    }
    // A copy that doesn't mark the document saved.
    new_layer(&mut h, "More");
    h.save(doc, &SaveOptions { mark_saved: false, ..SaveOptions::default() }).unwrap();
    assert!(h.documents()[0].dirty);

    // An edit lands while a background save encodes: still dirty after it ends.
    let started = h.start_save(doc, &SaveOptions::default()).unwrap();
    new_layer(&mut h, "During");
    if let SaveStarted::Job(job) = started {
        h.wait(job).unwrap();
        h.poll();
        assert!(h.take_saved(job).is_some());
    }
    assert!(h.documents()[0].dirty);
}
