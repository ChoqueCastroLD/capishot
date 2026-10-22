//! The AI model download service (`ai.models.*`): which model weights are on this device,
//! downloading them (a queue, one file at a time, cancellable), deleting them. Separate from
//! running models: Clean with AI, the Remove tool's AI mode and Upscale with AI only use
//! downloaded models (an agent may still pass `allowDownload` / `download` to them), and the
//! AI Model Manager (Window › AI Models…) drives these commands, as automation can.
//!
//! The shell injects a [`ModelStore`] ([`Session::set_ai_models`]): natively
//! `photocraft_ai::native_store::NativeModelStore` on the config dir's cache, in the browser a
//! store backed by the AI Web Worker. Without one every command fails with [`UNAVAILABLE`].
//!
//! Models are named by catalog id (`photocraft_ai::catalog`) or weight file name; entries that
//! share a file (Real-ESRGAN Compact ×2 and ×4) are one download.

use std::sync::Arc;

use photocraft_ai::cards::{self, Card, CardState};
use photocraft_ai::catalog::ModelInfo;
use photocraft_ai::models::{self, ModelStore, Status};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

/// Why the commands fail without a store.
pub const UNAVAILABLE: &str = "AI model downloads are not available in this build";

/// Every "model not downloaded" error contains this (the UI turns it into its dialog).
pub const NOT_DOWNLOADED: &str = "isn't downloaded";

/// The error of an AI command whose model isn't on this device (`param`: the command's
/// download-consent param, for agents).
pub fn not_downloaded(title: &str, bytes: u64, param: &str) -> String {
    format!(
        "{title} {NOT_DOWNLOADED} ({:.1} MB): download it in Window › AI Models… (agents: `{DOWNLOAD}`, or run again with \"{param}\": true)",
        bytes as f64 / 1e6
    )
}

pub const LIST: &str = "ai.models.list";
pub const STATUS: &str = "ai.models.status";
pub const DOWNLOAD: &str = "ai.models.download";
pub const CANCEL: &str = "ai.models.cancel";
pub const DELETE: &str = "ai.models.delete";
pub const PAUSE: &str = "ai.models.pause";

impl Session {
    /// Injects (or removes) the model download service.
    pub fn set_ai_models(&mut self, store: Option<Arc<dyn ModelStore>>) {
        self.ai_models = store;
    }

    /// The injected model download service, if any.
    pub fn ai_models(&self) -> Option<Arc<dyn ModelStore>> {
        self.ai_models.clone()
    }
}

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn store(s: &Session) -> Result<Arc<dyn ModelStore>> {
    s.ai_models.clone().ok_or_else(|| EngineError::Other(UNAVAILABLE.into()))
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    s.ai_models.as_ref().map(|_| ()).ok_or_else(|| UNAVAILABLE.into())
}

/// One model by `id` (catalog id or file name).
fn model_param(p: &Value, cmd: &str) -> Result<Option<&'static ModelInfo>> {
    match p.get("id") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(id)) => models::by_id_or_file(id).map(Some).ok_or_else(|| bad(cmd, format!("unknown model `{id}`; `{LIST}` lists them"))),
        Some(_) => Err(bad(cmd, "`id` must be a model id")),
    }
}

/// A model's row: what it is, what uses it, where it runs, and its status.
pub fn entry(model: &'static ModelInfo, status: &Status) -> Value {
    let ids: Vec<&str> = models::sharing_file(model).map(|m| m.id).collect();
    let mut v = json!({
        "id": model.id,
        "ids": ids,
        "file": model.file,
        "title": models::file_title(model),
        "group": models::group(model),
        "tools": models::tools(model),
        "runs": models::runs(model),
        "lite": model.lite,
        "bytes": model.bytes,
        "license": model.license,
        "author": model.author,
        "source": model.source,
        "status": status.id(),
    });
    match status {
        Status::Downloading { done, total, bytes_per_sec } => {
            v["done"] = json!(done);
            v["total"] = json!(total);
            v["bytesPerSec"] = json!(bytes_per_sec.round());
            if let Some(eta) = status.eta_secs() {
                v["etaSecs"] = json!(eta.round());
            }
        }
        Status::Failed(why) | Status::Unavailable(why) => v["error"] = json!(why),
        _ => {}
    }
    v
}

/// The queue at a glance: the file downloading, how many wait, bytes done of the queue's total.
pub fn queue_summary(store: &dyn ModelStore) -> Value {
    let mut active = Value::Null;
    let (mut queued, mut done, mut total) = (0u64, 0u64, 0u64);
    for m in models::files() {
        match store.status(m) {
            Status::Queued => {
                queued += 1;
                total = total.saturating_add(m.bytes);
            }
            s @ Status::Downloading { .. } => {
                if let Status::Downloading { done: d, total: t, .. } = s {
                    done = done.saturating_add(d);
                    total = total.saturating_add(t);
                }
                active = entry(m, &s);
            }
            _ => {}
        }
    }
    json!({"active": active, "queued": queued, "done": done.min(total), "total": total, "busy": !active.is_null() || queued > 0})
}

/// The cards "Download recommended" points at: the full or lite cleaner and upscalers as the
/// device profile picks them, and the Generative Fill tier auto mode would run.
pub fn recommended_cards(s: &Session) -> Vec<&'static str> {
    let lite = recommended(s).first().is_some_and(|m| m.lite);
    let mut v = if lite { vec!["migan", "realesr-compact"] } else { vec!["manga-lama", "mangajanai", "illustrationjanai"] };
    if let Ok(t) = crate::genfill_cmds::tier_for(s, &json!({})) {
        v.push(match t {
            photocraft_ai_sd::service::Tier::Ultra => cards::GENFILL_ULTRA_CARD,
            photocraft_ai_sd::service::Tier::Fast => cards::GENFILL_FAST_CARD,
            _ => cards::GENFILL_STANDARD_CARD,
        });
    }
    v
}

/// A card's row: what it is and its state over all its files.
pub fn card_entry(st: &dyn ModelStore, c: &'static Card, recommended: bool) -> Value {
    let files = c.models();
    let state = CardState::of(&files.iter().map(|m| (m.bytes, st.status(m))).collect::<Vec<_>>());
    let mut v = json!({
        "id": c.id, "section": c.section, "title": c.title, "description": c.description, "bytes": c.bytes(),
        "quality": c.quality, "speed": c.speed, "gpu": c.gpu, "precision": c.precision, "memoryMb": c.memory_mb,
        "files": files.iter().map(|m| m.id).collect::<Vec<_>>(), "recommended": recommended, "state": state.id(),
    });
    match state {
        CardState::Downloading { done, total, bytes_per_sec } => {
            v["done"] = json!(done);
            v["total"] = json!(total);
            v["bytesPerSec"] = json!(bytes_per_sec);
        }
        CardState::Paused { done, total } => {
            v["done"] = json!(done);
            v["total"] = json!(total);
        }
        CardState::Error(e) | CardState::Unavailable(e) => v["error"] = json!(e),
        _ => {}
    }
    v
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let st = store(s)?;
    let rows: Vec<Value> = models::files().into_iter().map(|m| entry(m, &st.status(m))).collect();
    let storage = st.storage();
    let available = st.available();
    let recommended: Vec<&str> = recommended(s).iter().map(|m| m.id).collect();
    let rc = recommended_cards(s);
    let card_rows: Vec<Value> = cards::CARDS.iter().map(|c| card_entry(st.as_ref(), c, rc.contains(&c.id))).collect();
    Ok(json!({
        "cards": card_rows,
        "available": available.is_ok(),
        "reason": available.err(),
        "storage": storage,
        "queue": queue_summary(st.as_ref()),
        "recommended": recommended,
        "models": rows,
    }))
}

fn status(s: &mut Session, p: &Value) -> Result<Value> {
    let st = store(s)?;
    match model_param(p, STATUS)? {
        Some(m) => Ok(entry(m, &st.status(m))),
        None => Ok(queue_summary(st.as_ref())),
    }
}

/// The models "Download recommended" queues: full models with a usable GPU, else lite ones;
/// MangaJaNai only for the open documents' page heights (all heights when none is open).
pub fn recommended(s: &Session) -> Vec<&'static ModelInfo> {
    let heights: Vec<u32> = s.documents().iter().map(|d| d.doc.size.height).collect();
    models::recommended(&s.ai_profile(), &heights)
}

fn download(s: &mut Session, p: &Value) -> Result<Value> {
    let st = store(s)?;
    let flag = |k: &str| -> Result<bool> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(false),
            Some(v) => v.as_bool().ok_or_else(|| bad(DOWNLOAD, format!("`{k}` must be true or false"))),
        }
    };
    let wanted: Vec<&'static ModelInfo> = if let Some(c) = card_param(p, DOWNLOAD)? {
        c.models()
    } else if let Some(m) = model_param(p, DOWNLOAD)? {
        vec![m]
    } else if let Some(ids) = p.get("ids") {
        let ids = ids.as_array().ok_or_else(|| bad(DOWNLOAD, "`ids` must be a list of model ids"))?;
        if ids.len() > 256 {
            return Err(bad(DOWNLOAD, "too many `ids`"));
        }
        ids.iter()
            .map(|v| v.as_str().and_then(models::by_id_or_file).ok_or_else(|| bad(DOWNLOAD, format!("unknown model {v}"))))
            .collect::<Result<_>>()?
    } else if flag("all")? {
        models::files()
    } else if flag("recommended")? {
        recommended(s)
    } else {
        return Err(bad(DOWNLOAD, "give `id`, `ids`, `all: true` or `recommended: true`"));
    };
    st.available().map_err(EngineError::Other)?;
    let mut queued = Vec::new();
    let mut skipped = Vec::new();
    for m in wanted {
        if queued.iter().chain(skipped.iter()).any(|q: &&ModelInfo| q.file == m.file) {
            continue;
        }
        match st.status(m) {
            Status::Downloaded | Status::Queued | Status::Downloading { .. } => skipped.push(m),
            _ => {
                st.download(m).map_err(|e| EngineError::Other(e.to_string()))?;
                queued.push(m);
            }
        }
    }
    let ids = |v: &[&ModelInfo]| v.iter().map(|m| m.id).collect::<Vec<_>>();
    Ok(json!({"queued": ids(&queued), "skipped": ids(&skipped), "queue": queue_summary(st.as_ref())}))
}

fn cancel(s: &mut Session, p: &Value) -> Result<Value> {
    let st = store(s)?;
    if let Some(c) = card_param(p, CANCEL)? {
        for m in c.models() {
            st.cancel(Some(m));
        }
        return Ok(json!({"cancelled": c.id, "queue": queue_summary(st.as_ref())}));
    }
    let m = model_param(p, CANCEL)?;
    st.cancel(m);
    Ok(json!({"cancelled": m.map_or("all", |m| m.id), "queue": queue_summary(st.as_ref())}))
}

fn pause(s: &mut Session, p: &Value) -> Result<Value> {
    let st = store(s)?;
    if let Some(c) = card_param(p, PAUSE)? {
        for m in c.models() {
            st.pause(Some(m));
        }
        return Ok(json!({"paused": c.id, "queue": queue_summary(st.as_ref())}));
    }
    let m = model_param(p, PAUSE)?;
    st.pause(m);
    Ok(json!({"paused": m.map_or("all", |m| m.id), "queue": queue_summary(st.as_ref())}))
}

/// One card by `card` id.
fn card_param(p: &Value, cmd: &str) -> Result<Option<&'static Card>> {
    match p.get("card") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(id)) => cards::card(id).map(Some).ok_or_else(|| bad(cmd, format!("unknown card `{id}`; `{LIST}` lists them"))),
        Some(_) => Err(bad(cmd, "`card` must be a card id")),
    }
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    let st = store(s)?;
    let all = match p.get("all") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or_else(|| bad(DELETE, "`all` must be true or false"))?,
    };
    if let Some(c) = card_param(p, DELETE)? {
        // Files another installed card still uses (TAESD) stay.
        let installed = |o: &Card| o.models().iter().all(|m| st.status(m).is_downloaded());
        let mut deleted = Vec::new();
        for m in cards::deletable(c, &installed) {
            let was = st.status(m);
            st.delete(m).map_err(|e| EngineError::Other(e.to_string()))?;
            if matches!(was, Status::Downloaded | Status::UpdateAvailable) {
                deleted.push(m.id);
            }
        }
        return Ok(json!({"deleted": deleted, "card": c.id, "storage": st.storage()}));
    }
    let targets: Vec<&'static ModelInfo> = match (model_param(p, DELETE)?, all) {
        (Some(m), _) => vec![m],
        (None, true) => {
            st.cancel(None);
            models::files()
        }
        (None, false) => return Err(bad(DELETE, "give `id` or `all: true`")),
    };
    let mut deleted = Vec::new();
    for m in targets {
        let was = st.status(m);
        st.delete(m).map_err(|e| EngineError::Other(e.to_string()))?;
        if matches!(was, Status::Downloaded | Status::UpdateAvailable) {
            deleted.push(m.id);
        }
    }
    Ok(json!({"deleted": deleted, "storage": st.storage()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: LIST,
            label: "List AI Models",
            menu: &[],
            shortcut: None,
            params: r#"{} -> {"available", "reason"?, "storage": {"models", "used", "quota"}, "queue": {"active", "queued", "done", "total", "busy"}, "recommended": [id], "models": [{"id", "ids", "file", "title", "group": "cleaning"|"upscaling"|"generative", "tools": [command id], "runs": "bestOnGpu"|"both", "lite", "bytes", "license", "author", "source", "status": "notDownloaded"|"queued"|"downloading"|"paused"|"downloaded"|"updateAvailable"|"failed"|"checking"|"unavailable", "done"?, "total"?, "bytesPerSec"?, "etaSecs"?, "error"?}]} (one row per weight file), "cards": [{"id", "section": "clean"|"upscale"|"generative", "title", "description", "bytes", "quality", "speed", "gpu", "precision", "memoryMb", "files": [id], "recommended", "state": "notDownloaded"|"downloading"|"paused"|"ready"|"error"|"checking"|"unavailable", "done"?, "total"?, "bytesPerSec"?, "error"?}]"#,
            enabled,
            run: list,
            journal: false,
        },
        CommandSpec {
            id: STATUS,
            label: "AI Model Status",
            menu: &[],
            shortcut: None,
            params: r#"{"id"?: model id or file} -> that model's row as in ai.models.list, or without `id` the queue: {"active", "queued", "done", "total", "busy"}"#,
            enabled,
            run: status,
            journal: false,
        },
        CommandSpec {
            id: DOWNLOAD,
            label: "Download AI Models",
            menu: &[],
            shortcut: None,
            params: r#"{"id": model id or file} | {"ids": [id]} | {"card": card id (every file of a card or Generative Fill tier)} | {"all": true} | {"recommended": true} -> {"queued": [id], "skipped": [id] (already downloaded or queued), "queue"}. Downloads run in the background, one at a time, and resume where a pause or an interruption left them; poll ai.models.status."#,
            enabled,
            run: download,
            journal: false,
        },
        CommandSpec {
            id: CANCEL,
            label: "Cancel AI Model Download",
            menu: &[],
            shortcut: None,
            params: r#"{"id"?: model id or file | "card"?: card id (default: every queued and running download)} -> {"cancelled", "queue"}. The bytes downloaded so far are dropped (`ai.models.pause` keeps them)."#,
            enabled,
            run: cancel,
            journal: false,
        },
        CommandSpec {
            id: PAUSE,
            label: "Pause AI Model Download",
            menu: &[],
            shortcut: None,
            params: r#"{"id"?: model id or file | "card"?: card id (default: every download)} -> {"paused", "queue"}. Keeps what was downloaded; `ai.models.download` resumes it (status "paused")."#,
            enabled,
            run: pause,
            journal: false,
        },
        CommandSpec {
            id: DELETE,
            label: "Delete AI Models",
            menu: &[],
            shortcut: None,
            params: r#"{"id": model id or file} | {"card": card id (keeps files another installed card uses)} | {"all": true} -> {"deleted": [id], "storage"}. Cancels the model's download first."#,
            enabled,
            run: delete,
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_ai::catalog::{COMPACT_2X_ID, COMPACT_4X_ID, LAMA_ID, MIGAN_ID};
    use photocraft_ai::models::FakeStore;

    fn session() -> (Session, Arc<FakeStore>) {
        let mut s = Session::new();
        let fake = Arc::new(FakeStore::default());
        s.set_ai_models(Some(fake.clone()));
        (s, fake)
    }

    #[test]
    fn without_a_store_every_command_fails_cleanly() {
        let mut s = Session::new();
        for id in [LIST, STATUS, DOWNLOAD, CANCEL, PAUSE, DELETE] {
            let e = s.execute(id, json!({"id": LAMA_ID})).unwrap_err().to_string();
            assert!(e.contains("not available"), "{id}: {e}");
        }
    }

    #[test]
    fn list_has_one_row_per_file_with_tools_and_status() {
        let (mut s, fake) = session();
        fake.put(photocraft_ai::catalog::model(MIGAN_ID).unwrap());
        let v = s.execute(LIST, json!({})).unwrap();
        let rows = v["models"].as_array().unwrap();
        assert_eq!(rows.len(), photocraft_ai::models::files().len());
        let row = |id: &str| rows.iter().find(|r| r["ids"].as_array().unwrap().iter().any(|i| i == id)).unwrap().clone();
        assert_eq!(row(MIGAN_ID)["status"], "downloaded");
        assert_eq!(row(LAMA_ID)["status"], "notDownloaded");
        assert_eq!(row(LAMA_ID)["tools"], json!(["edit.inpaintAi", "paint.remove"]));
        assert_eq!(row(LAMA_ID)["group"], "cleaning");
        assert_eq!(row(LAMA_ID)["runs"], "bestOnGpu");
        assert_eq!(row(COMPACT_2X_ID), row(COMPACT_4X_ID));
        assert_eq!(v["available"], true);
        assert!(v["storage"]["models"].as_u64().unwrap() > 0);
    }

    #[test]
    fn download_queues_in_order_and_cancel_and_delete_work() {
        let (mut s, fake) = session();
        let v = s.execute(DOWNLOAD, json!({"ids": [LAMA_ID, MIGAN_ID, COMPACT_2X_ID, COMPACT_4X_ID]})).unwrap();
        assert_eq!(v["queued"], json!([LAMA_ID, MIGAN_ID, COMPACT_2X_ID]), "shared files once");
        assert_eq!(s.execute(STATUS, json!({"id": LAMA_ID})).unwrap()["status"], "downloading");
        assert_eq!(s.execute(STATUS, json!({"id": MIGAN_ID})).unwrap()["status"], "queued");
        let q = s.execute(STATUS, json!({})).unwrap();
        assert_eq!((q["queued"].as_u64(), q["busy"].as_bool()), (Some(2), Some(true)));
        // Asking again skips what is queued.
        assert_eq!(s.execute(DOWNLOAD, json!({"id": MIGAN_ID})).unwrap()["skipped"], json!([MIGAN_ID]));
        fake.step();
        assert_eq!(s.execute(STATUS, json!({"id": LAMA_ID})).unwrap()["status"], "downloaded");
        s.execute(CANCEL, json!({"id": MIGAN_ID})).unwrap();
        assert_eq!(s.execute(STATUS, json!({"id": MIGAN_ID})).unwrap()["status"], "notDownloaded");
        s.execute(CANCEL, json!({})).unwrap();
        assert_eq!(s.execute(STATUS, json!({})).unwrap()["busy"], false);
        assert_eq!(s.execute(DELETE, json!({"id": LAMA_ID})).unwrap()["deleted"], json!([LAMA_ID]));
        assert_eq!(s.execute(STATUS, json!({"id": LAMA_ID})).unwrap()["status"], "notDownloaded");
        // Download all, then delete all.
        fake.inner.lock().unwrap().instant = true;
        let v = s.execute(DOWNLOAD, json!({"all": true})).unwrap();
        assert_eq!(v["queued"].as_array().unwrap().len(), photocraft_ai::models::files().len());
        let v = s.execute(DELETE, json!({"all": true})).unwrap();
        assert_eq!(v["deleted"].as_array().unwrap().len(), photocraft_ai::models::files().len());
        assert_eq!(v["storage"]["models"], 0);
    }

    #[test]
    fn recommended_follows_the_profile() {
        let (mut s, _) = session();
        // No upscaler: the profile is unknown, so lite models.
        let v = s.execute(DOWNLOAD, json!({"recommended": true})).unwrap();
        let q = v["queued"].as_array().unwrap();
        assert!(q.contains(&json!(MIGAN_ID)) && !q.contains(&json!(LAMA_ID)), "{v}");
    }

    #[test]
    fn bad_params_are_errors() {
        let (mut s, fake) = session();
        for (cmd, p) in [
            (DOWNLOAD, json!({})),
            (DOWNLOAD, json!({"id": "nope"})),
            (DOWNLOAD, json!({"id": 5})),
            (DOWNLOAD, json!({"ids": "x"})),
            (DOWNLOAD, json!({"ids": [1]})),
            (DOWNLOAD, json!({"all": "yes"})),
            (STATUS, json!({"id": []})),
            (CANCEL, json!({"id": "nope"})),
            (DELETE, json!({})),
            (DELETE, json!({"all": 1})),
        ] {
            assert!(s.execute(cmd, p.clone()).is_err(), "{cmd} {p}");
        }
        fake.inner.lock().unwrap().unavailable = Some("offline".into());
        assert!(s.execute(DOWNLOAD, json!({"id": LAMA_ID})).unwrap_err().to_string().contains("offline"));
        assert_eq!(s.execute(LIST, json!({})).unwrap()["reason"], "offline");
    }

    #[test]
    fn cards_download_pause_resume_and_delete_keeps_shared_files() {
        let (mut s, fake) = session();
        let v = s.execute(DOWNLOAD, json!({"card": cards::GENFILL_ULTRA_CARD})).unwrap();
        assert_eq!(v["queued"].as_array().unwrap().len(), 3);
        fake.step();
        let v = s.execute(PAUSE, json!({"card": cards::GENFILL_ULTRA_CARD})).unwrap();
        assert_eq!(v["paused"], cards::GENFILL_ULTRA_CARD);
        let list = s.execute(LIST, json!({})).unwrap();
        let card = |l: &Value, id: &str| l["cards"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap().clone();
        assert_eq!(card(&list, cards::GENFILL_ULTRA_CARD)["state"], "paused");
        s.execute(DOWNLOAD, json!({"card": cards::GENFILL_ULTRA_CARD})).unwrap();
        while fake.step() {}
        assert_eq!(card(&s.execute(LIST, json!({})).unwrap(), cards::GENFILL_ULTRA_CARD)["state"], "ready");
        s.execute(DOWNLOAD, json!({"card": cards::GENFILL_STANDARD_CARD})).unwrap();
        while fake.step() {}
        let v = s.execute(DELETE, json!({"card": cards::GENFILL_ULTRA_CARD})).unwrap();
        assert!(!v["deleted"].as_array().unwrap().iter().any(|i| i == "genfill-taesd"), "the Standard tier still uses TAESD");
        assert_eq!(card(&s.execute(LIST, json!({})).unwrap(), cards::GENFILL_STANDARD_CARD)["state"], "ready");
        assert!(s.execute(DOWNLOAD, json!({"card": "nope"})).is_err());
    }
}
