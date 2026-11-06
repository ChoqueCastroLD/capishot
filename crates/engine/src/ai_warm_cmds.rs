//! `ai.warm`: loads an inpainting model before its first use, off the frame, by running it on a small
//! blank tile. The first run in a process builds the model's session; a host calls this at start-up
//! (`Host::warm_ai`) so that the first removal doesn't wait for it.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::ai_inpaint_cmds::{ai_fill_rgb_async, ai_options, ready_inpainter};
use crate::cmd::CommandSpec;
use crate::{Result, Session};

pub const ID: &str = "ai.warm";
const LABEL: &str = "Load AI Model";
/// The blank tile's side: one tile, whatever the model's working size.
const SIDE: usize = 256;

fn warm(s: &mut Session, p: &Value) -> Result<Value> {
    let opts = ai_options(s, p, ID, false)?;
    let svc = ready_inpainter(s, &opts)?;
    crate::jobs::run_async(
        s,
        LABEL,
        false,
        move |ctx, done| {
            let rgb = vec![0.5_f32; SIDE * SIDE * 3];
            // A hole in the middle, with room around it to fill from.
            let cover: Vec<f32> = (0..SIDE * SIDE)
                .map(|i| {
                    let (x, y) = (i % SIDE, i / SIDE);
                    let middle = (SIDE / 4..SIDE * 3 / 4).contains(&x) && (SIDE / 4..SIDE * 3 / 4).contains(&y);
                    if middle { 1.0 } else { 0.0 }
                })
                .collect();
            let started = web_time::Instant::now();
            let device = Arc::clone(&svc);
            ai_fill_rgb_async(
                svc,
                ctx.clone(),
                rgb,
                cover,
                SIDE,
                SIDE,
                opts,
                Box::new(move |r| {
                    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    done.finish(r.map(|(_, run)| json!({
                        "model": run.model.id,
                        "device": device.device(),
                        "cpu": run.cpu,
                        "fellBack": run.fell_back,
                        "ms": ms,
                    })));
                }),
            );
        },
        |_, v: Value| Ok(v),
    )
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    if s.inpainter().is_none() {
        return Err(crate::ai_inpaint_cmds::UNAVAILABLE.into());
    }
    Ok(())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: ID,
        label: LABEL,
        menu: &[],
        shortcut: None,
        params: r#"{"model"?: "manga-lama"|"migan-512"|"auto" (default auto)}; loads the model off the frame, as a job; answers {model, device, cpu, fellBack, ms}. An error when the model isn't downloaded."#,
        enabled,
        journal: false,
        run: warm,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_runs_the_model_once_and_edits_nothing() {
        let mut s = Session::new();
        assert!(s.execute(ID, json!({})).is_err(), "no inpainter");
        let fake = Arc::new(photocraft_ai_lama::service::FakeInpainter::default());
        s.set_inpainter(Some(fake.clone()));
        let r = s.execute(ID, json!({"model": "manga-lama"})).unwrap();
        assert_eq!(r["model"], "manga-lama");
        assert!(r["ms"].is_u64(), "{r}");
        assert_eq!(fake.sizes.lock().unwrap().len(), 1, "one tile");
        assert!(s.active().is_none(), "no document needed");
    }

    #[test]
    fn warm_refuses_a_model_that_isnt_downloaded() {
        let mut s = Session::new();
        let fake = photocraft_ai_lama::service::FakeInpainter { state: Some(photocraft_ai_lama::service::ModelState::NeedsDownload { bytes: 1 }), ..Default::default() };
        s.set_inpainter(Some(Arc::new(fake)));
        assert!(s.execute(ID, json!({"model": "manga-lama"})).is_err());
    }
}
