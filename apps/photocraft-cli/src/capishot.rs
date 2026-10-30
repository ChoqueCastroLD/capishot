//! Capishot's own subcommand and options: `compile` (psd-compiler) and `--allow-online-fonts`
//! with the AI services for headless sessions.

use std::io::Write;
use std::path::PathBuf;

use photocraft_automation::Headless;
use serde_json::{Value, json};

use crate::args::CompileArgs;
use crate::output::warn_all;
use crate::{CliError, Result};

/// Gives a headless session the Google Fonts download services: fonts downloaded earlier are
/// registered (from disk, no network) and `type.fonts.*` work once `type.allowOnlineFonts` is on.
/// The CLI never reads or writes the preferences file, so the preference starts off; the person
/// launching the CLI turns it on for this process with `--allow-online-fonts` (never persisted).
/// A `run` script can also set it with `prefs.set`; an untrusted MCP or control client cannot
/// change it at all (the automation guard), with or without the flag. Never fatal.
pub(crate) fn with_fonts(h: Headless, allow_online: bool) -> Headless {
    with_fonts_in(h, allow_online, photocraft_fontfetch::default_config_dir())
}

pub(crate) fn with_fonts_in(mut h: Headless, allow_online: bool, config_dir: Option<PathBuf>) -> Headless {
    // The desktop app's models folder, so models it downloaded run here too (`PHOTOCRAFT_CONFIG_DIR`
    // keeps everything under that folder instead).
    #[cfg(feature = "ai")]
    let models = config_dir.as_deref().map(|dir| {
        let platform = std::env::var_os("PHOTOCRAFT_CONFIG_DIR").is_none_or(|v| v.is_empty());
        photocraft_ai::cache::models_dir(dir, platform, |k| std::env::var_os(k))
    });
    // `edit.inpaintAi` and the Remove tool's AI mode: Manga LaMa on this machine's GPU or CPU,
    // weights from the models folder or CAPISHOT_LAMA_WEIGHTS (feature `ai`).
    #[cfg(feature = "ai")]
    if let Some(i) = photocraft_ai_lama::native::NativeInpainter::locate(models.as_deref()) {
        h.session.set_inpainter(Some(std::sync::Arc::new(i)));
    }
    // `edit.generativeFill`: Stable Diffusion inpainting on this machine (feature `ai`), files from
    // the models folder or CAPISHOT_SD_DIR.
    #[cfg(feature = "ai")]
    h.session.set_genfill(Some(std::sync::Arc::new(photocraft_ai_sd::native::NativeGenFill::locate(models.as_deref()))));
    if let Some(dir) = config_dir {
        photocraft_fontfetch::attach(&mut h.session, &dir);
    }
    // Image › Upscale with AI (`image.upscaleAi`), in-process, and `ai.models.*`, which download
    // into the same folder (the `ai` feature).
    #[cfg(feature = "ai")]
    if let Some(dir) = &models {
        h.session.set_ai_upscaler(Some(std::sync::Arc::new(photocraft_ai::native::NativeUpscaler::from_env(dir))));
        h.session.set_ai_models(Some(std::sync::Arc::new(photocraft_ai::native_store::NativeModelStore::from_env(dir))));
    }
    if allow_online {
        h.session.edit_prefs(|p| p.type_.allow_online_fonts = true);
    }
    h
}

pub(crate) fn compile(a: &CompileArgs, out: &mut dyn Write, err: &mut dyn Write) -> Result {
    let input = a.input.to_string_lossy().into_owned();
    let fonts: Vec<String> = a.fonts.iter().map(|f| f.to_string_lossy().into_owned()).collect();
    let (rerender, system) = (!a.keep_text, !a.no_system_fonts);
    let fail = |m: String| CliError::Io(std::io::Error::other(m));
    if a.input.is_dir() {
        let format = a.format.as_deref().unwrap_or("png");
        let output = a.out.clone().unwrap_or_else(|| a.input.join("compiled")).to_string_lossy().into_owned();
        let mut params = json!({"input": input, "output": output, "format": format, "rerenderText": rerender, "fonts": fonts, "systemFonts": system});
        if let Some(q) = a.quality {
            params["quality"] = json!(q);
        }
        let mut s = photocraft_engine::Session::new();
        let r = s.execute("file.automate.compileFolder", params)?;
        for w in r["warnings"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            let _ = writeln!(err, "warning: {w}");
        }
        for f in r["failed"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            let _ = writeln!(err, "error: {f}");
        }
        let files = r["files"].as_array().map_or(0, Vec::len);
        let failed = r["failed"].as_array().map_or(0, Vec::len);
        let _ = writeln!(out, "compiled {files} file(s) into {output}");
        return if failed == 0 { Ok(()) } else { Err(CliError::FilesFailed(failed)) };
    }
    let ext = match (&a.format, &a.out) {
        (Some(f), _) => f.trim_start_matches('.').to_ascii_lowercase(),
        (None, Some(o)) => o.extension().and_then(|e| e.to_str()).unwrap_or("png").to_ascii_lowercase(),
        (None, None) => "png".into(),
    };
    let output = a.out.clone().unwrap_or_else(|| a.input.with_extension(&ext));
    let bytes = std::fs::read(&a.input).map_err(|e| fail(format!("{input}: {e}")))?;
    let opts = photocraft_io::psdc::CompileOptions {
        rerender_text: rerender,
        font_dirs: fonts.iter().map(Into::into).collect(),
        system_fonts: system,
        ..Default::default()
    };
    // The bundled and craft fonts after the user's, as the folder compile does.
    let opts = if rerender { opts.with_app_fonts() } else { opts };
    let c = photocraft_io::psdc::compile(&bytes, &opts).map_err(|e| fail(format!("{input}: {e}")))?;
    warn_all(err, &c.warnings);
    let encoded = c.encode(&ext, a.quality).map_err(|e| fail(e.to_string()))?;
    std::fs::write(&output, encoded).map_err(|e| fail(format!("{}: {e}", output.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::invoke;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("capishot-cli-fonts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        d
    }

    #[test]
    fn the_flag_is_taken_by_the_subcommands_that_attach_fonts() {
        let ok = |args: &[&str]| crate::args::parse(&args.iter().map(std::ffi::OsString::from).collect::<Vec<_>>()).is_ok();
        assert!(ok(&["run", "--new", "{}", "--cmd", "x", "--allow-online-fonts"]));
        assert!(ok(&["batch", "--actions", "a.json", "--in", "i", "--out", "o", "--allow-online-fonts"]));
        assert!(ok(&["mcp", "--allow-online-fonts"]));
        assert!(ok(&["serve", "--allow-online-fonts"]));
        assert!(!ok(&["run", "--new", "{}", "--cmd", "x", "--allow-online-fonts=1"]), "takes no value");
        assert!(!ok(&["convert", "a.png", "b.png", "--allow-online-fonts"]));
        assert!(!ok(&["info", "a.png", "--allow-online-fonts"]));
        let help = invoke(&["run", "--help"]);
        assert!(help.stdout.contains("--allow-online-fonts") && help.stdout.contains("Google Fonts"), "{}", help.stdout);
    }

    #[test]
    fn the_flag_turns_downloads_on_for_this_session_only() {
        let dir = temp("flag");
        let prefs = dir.join("preferences.json");
        std::fs::write(&prefs, b"{\"version\":1}").expect("write prefs");
        let before = std::fs::read(&prefs).expect("read");

        let mut off = with_fonts_in(Headless::trusted_local(), false, Some(dir.clone()));
        assert!(!off.session.prefs().type_.allow_online_fonts);
        let e = off.session.execute("type.fonts.install", json!({"family": "Roboto"})).expect_err("off").to_string();
        assert!(e.contains("turned off"), "{e}");

        let mut on = with_fonts_in(Headless::trusted_local(), true, Some(dir.clone()));
        assert!(on.session.prefs().type_.allow_online_fonts);
        // Past the preference check: this build has no pinned index, so it stops there.
        let e = on.session.execute("type.fonts.install", json!({"family": "Roboto"})).expect_err("no index").to_string();
        assert!(!e.contains("turned off") && e.contains("not configured"), "{e}");

        // Nothing was written: the saved preferences are untouched and no new file appeared.
        assert_eq!(std::fs::read(&prefs).expect("read"), before);
        let fresh = temp("flag-fresh");
        let _ = with_fonts_in(Headless::trusted_local(), true, Some(fresh.clone()));
        assert!(!fresh.join("preferences.json").exists());
        // A new session without the flag is off again.
        assert!(!with_fonts_in(Headless::trusted_local(), false, Some(fresh)).session.prefs().type_.allow_online_fonts);
        let _ = std::fs::remove_dir_all(dir);
    }
}
