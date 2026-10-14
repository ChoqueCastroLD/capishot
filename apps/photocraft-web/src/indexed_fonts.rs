//! The user's fonts in this browser's IndexedDB (`capishot.fonts`): one record per font file in
//! two stores written together, `meta` (the index entry, JSON, read at start-up) and `data` (the
//! font bytes, read when needed). Nothing leaves the browser. See `photocraft_text::user_fonts`.
use js_sys::{Array, Promise, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, EventTarget, IdbDatabase, IdbTransaction, IdbTransactionMode};

use crate::indexed_presets::{Listeners, before, js, request};

const DATABASE: &str = "capishot.fonts";
const META: &str = "meta";
const DATA: &str = "data";
/// A store operation gives up after this long (a font file is up to 256 MB).
const TIMEOUT_MS: f64 = 60_000.0;

fn deadline() -> f64 {
    js_sys::Date::now() + TIMEOUT_MS
}

/// Opens (creating on first use) the font database.
pub async fn open() -> Result<IdbDatabase, String> {
    let factory = web_sys::window().ok_or("no window")?.indexed_db().map_err(js)?.ok_or("IndexedDB unavailable")?;
    let req = factory.open_with_u32(DATABASE, 1).map_err(js)?;
    let mut upgrade = Listeners::new();
    let upgrade_req = req.clone();
    upgrade
        .add(&req, "upgradeneeded", move |_| {
            let result = upgrade_req.result().and_then(|v| v.dyn_into::<IdbDatabase>()).and_then(|db| {
                let names = db.object_store_names();
                for store in [META, DATA] {
                    if !names.contains(store) {
                        db.create_object_store(store)?;
                    }
                }
                Ok(())
            });
            if result.is_err()
                && let Some(tx) = upgrade_req.transaction()
            {
                let _ = tx.abort();
            }
        })
        .map_err(js)?;
    let pending = request(&req);
    let value = before(&pending.promise, deadline()).await?;
    drop(upgrade);
    value.dyn_into().map_err(|_| "invalid font database".into())
}

/// Every stored index entry (JSON text), in key order.
pub async fn read_index(db: &IdbDatabase) -> Result<Vec<String>, String> {
    let tx = db.transaction_with_str_and_mode(META, IdbTransactionMode::Readonly).map_err(js)?;
    let req = tx.object_store(META).map_err(js)?.get_all().map_err(js)?;
    let pending = request(&req);
    let all: Array = before(&pending.promise, deadline()).await?.dyn_into().map_err(|_| "invalid font index")?;
    Ok(all.iter().filter_map(|v| v.as_string()).collect())
}

/// The bytes of the stored font `id`.
pub async fn get(db: &IdbDatabase, id: &str) -> Result<Vec<u8>, String> {
    let tx = db.transaction_with_str_and_mode(DATA, IdbTransactionMode::Readonly).map_err(js)?;
    let req = tx.object_store(DATA).map_err(js)?.get(&JsValue::from_str(id)).map_err(js)?;
    let pending = request(&req);
    let value = before(&pending.promise, deadline()).await?;
    let blob: Blob = value.dyn_into().map_err(|_| "the stored font is missing".to_string())?;
    let buffer = before(&blob.array_buffer(), deadline()).await?;
    Ok(Uint8Array::new(&buffer).to_vec())
}

fn both(db: &IdbDatabase) -> Result<IdbTransaction, String> {
    let names = Array::of2(&JsValue::from_str(META), &JsValue::from_str(DATA));
    db.transaction_with_str_sequence_and_mode(&names, IdbTransactionMode::Readwrite).map_err(js)
}

/// Stores a font: its index entry and its bytes, in one transaction.
pub async fn put(db: &IdbDatabase, id: &str, meta: &str, data: &[u8]) -> Result<(), String> {
    let tx = both(db)?;
    let key = JsValue::from_str(id);
    let queued = (|| -> Result<(), String> {
        let blob = Blob::new_with_u8_array_sequence(&Array::of1(&Uint8Array::from(data))).map_err(js)?;
        tx.object_store(DATA).map_err(js)?.put_with_key(&blob, &key).map_err(js)?;
        tx.object_store(META).map_err(js)?.put_with_key(&JsValue::from_str(meta), &key).map_err(js)?;
        Ok(())
    })();
    finish(tx, queued).await
}

/// Deletes a stored font.
pub async fn delete(db: &IdbDatabase, id: &str) -> Result<(), String> {
    let tx = both(db)?;
    let key = JsValue::from_str(id);
    let queued = (|| -> Result<(), String> {
        tx.object_store(META).map_err(js)?.delete(&key).map_err(js)?;
        tx.object_store(DATA).map_err(js)?.delete(&key).map_err(js)?;
        Ok(())
    })();
    finish(tx, queued).await
}

/// Deletes every stored font.
pub async fn clear(db: &IdbDatabase) -> Result<(), String> {
    let tx = both(db)?;
    let queued = (|| -> Result<(), String> {
        tx.object_store(META).map_err(js)?.clear().map_err(js)?;
        tx.object_store(DATA).map_err(js)?.clear().map_err(js)?;
        Ok(())
    })();
    finish(tx, queued).await
}

/// Waits for a write transaction: success only once it completed (a quota error aborts it and
/// is reported as `QuotaExceededError: …`).
async fn finish(tx: IdbTransaction, queued: Result<(), String>) -> Result<(), String> {
    if let Err(e) = queued {
        let _ = tx.abort();
        return Err(e);
    }
    let mut listeners = Listeners::new();
    let promise = Promise::new(&mut |resolve, reject| {
        if let Err(e) = listeners.add(&tx, "complete", move |_| {
            let _ = resolve.call0(&JsValue::NULL);
        }) {
            let _ = reject.call1(&JsValue::NULL, &e);
        }
        for event in ["abort", "error"] {
            let t = tx.clone();
            let failed = reject.clone();
            let target: EventTarget = tx.clone().into();
            if let Err(e) = listeners.add(&target, event, move |_| {
                let e = t.error().map(JsValue::from).unwrap_or_else(|| JsValue::from_str("font transaction aborted"));
                let _ = failed.call1(&JsValue::NULL, &e);
            }) {
                let _ = reject.call1(&JsValue::NULL, &e);
            }
        }
    });
    let result = before(&promise, deadline()).await.map(|_| ());
    if result.is_err() {
        let _ = tx.abort();
    }
    result
}

/// `navigator.storage.<method>()` (`estimate`, `persist`, `persisted`), awaited.
async fn storage(method: &str) -> Result<JsValue, String> {
    use js_sys::{Function, Reflect};
    let navigator = web_sys::window().ok_or("no window")?.navigator();
    let storage = Reflect::get(&navigator, &JsValue::from_str("storage")).map_err(js)?;
    if storage.is_undefined() {
        return Err("no navigator.storage".into());
    }
    let f: Function = Reflect::get(&storage, &JsValue::from_str(method)).map_err(js)?.dyn_into().map_err(|_| format!("no storage.{method}"))?;
    let promise: Promise = f.call0(&storage).map_err(js)?.dyn_into().map_err(|_| "not a promise".to_string())?;
    JsFuture::from(promise).await.map_err(js)
}

/// The origin's storage use and quota (bytes), as the browser estimates them.
pub async fn estimate() -> Result<(u64, u64), String> {
    let v = storage("estimate").await?;
    let num = |k: &str| js_sys::Reflect::get(&v, &JsValue::from_str(k)).ok().and_then(|n| n.as_f64()).map_or(0, |n| n.max(0.0) as u64);
    Ok((num("usage"), num("quota")))
}

/// Asks for persistent storage (no eviction under storage pressure); whether it was granted.
pub async fn persist() -> Result<bool, String> {
    Ok(storage("persist").await?.as_bool().unwrap_or(false))
}

/// Whether storage is persistent already.
pub async fn persisted() -> Result<bool, String> {
    Ok(storage("persisted").await?.as_bool().unwrap_or(false))
}
