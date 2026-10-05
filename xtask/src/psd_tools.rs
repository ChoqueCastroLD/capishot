//! `cargo xtask corpus --psd-tools`: fetches the psd-tools test PSDs
//! (MIT, https://github.com/psd-tools/psd-tools/tree/main/tests/psd_files) at a
//! pinned commit into `corpus/psd-tools/` (gitignored, never committed) and
//! verifies every file against `xtask/psd-tools-corpus.sha256`.
//!
//! Re-running is cheap: an already complete, verified copy is left alone.
//! To move to a newer upstream commit, change `COMMIT`, delete
//! `corpus/psd-tools`, run with `--psd-tools --update-manifest` and review the
//! manifest diff (and the corpus floors in `crates/io/tests/corpus.rs`).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{root, run, sha256};

/// psd-tools commit the corpus is pinned to (main, 2026-10-05).
pub const COMMIT: &str = "96eb134c17b2c65edf4c4151c0f00b802ada86c2";
/// Generic User-Agent: no personal details in requests.
const USER_AGENT: &str = "Photocraft-dev";
const MANIFEST: &str = "xtask/psd-tools-corpus.sha256";

/// `(sha256, relative path)` pairs from the manifest.
fn manifest() -> Result<Vec<(String, String)>, String> {
    let path = root().join(MANIFEST);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (hash, file) = l.split_once("  ").ok_or_else(|| format!("{MANIFEST}: bad line {l:?}"))?;
            if hash.len() != 64 || file.contains("..") || file.starts_with('/') {
                return Err(format!("{MANIFEST}: bad line {l:?}"));
            }
            Ok((hash.to_string(), file.to_string()))
        })
        .collect()
}

/// Files in `dest` that are missing or do not match the manifest.
fn mismatches(dest: &Path, files: &[(String, String)]) -> Vec<String> {
    files
        .iter()
        .filter_map(|(hash, file)| match std::fs::read(dest.join(file)) {
            Ok(bytes) if sha256::hex(&bytes) == *hash => None,
            Ok(_) => Some(format!("{file}: sha256 mismatch")),
            Err(_) => Some(format!("{file}: missing")),
        })
        .collect()
}

fn collect_psds(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_psds(&p, base, out);
        } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("psd") || e.eq_ignore_ascii_case("psb")) {
            out.push(p.strip_prefix(base).unwrap_or(&p).to_path_buf());
        }
    }
}

pub fn fetch(update_manifest: bool) -> Result<(), String> {
    let corpus = root().join("corpus");
    let dest = corpus.join("psd-tools");
    let files = if update_manifest { Vec::new() } else { manifest()? };
    if !update_manifest && dest.is_dir() && mismatches(&dest, &files).is_empty() {
        println!("psd-tools: {} files in {} already match {MANIFEST}", files.len(), dest.display());
        return Ok(());
    }
    let staging = corpus.join(".psd-tools-download");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("create {}: {e}", staging.display()))?;
    let tgz = staging.join("psd-tools.tar.gz");
    let url = format!("https://codeload.github.com/psd-tools/psd-tools/tar.gz/{COMMIT}");
    let mut curl = Command::new("curl");
    curl.args(["-fsSL", "-A", USER_AGENT, "-o"]).arg(&tgz).arg(&url);
    run(curl, &format!("curl {url}"))?;
    let top = format!("psd-tools-{COMMIT}");
    let mut tar = Command::new("tar");
    tar.arg("-xzf").arg(&tgz).arg("-C").arg(&staging).arg(format!("{top}/tests/psd_files")).arg(format!("{top}/LICENSE"));
    run(tar, "tar -xzf psd-tools.tar.gz")?;
    let extracted = staging.join(&top).join("tests/psd_files");
    let fresh = if update_manifest {
        let mut found = Vec::new();
        collect_psds(&extracted, &extracted, &mut found);
        found.sort();
        let mut lines = String::new();
        let mut out = Vec::new();
        for f in found {
            let rel = f.to_string_lossy().replace('\\', "/");
            let bytes = std::fs::read(extracted.join(&f)).map_err(|e| format!("read {rel}: {e}"))?;
            let hash = sha256::hex(&bytes);
            lines.push_str(&format!("{hash}  {rel}\n"));
            out.push((hash, rel));
        }
        std::fs::write(root().join(MANIFEST), lines).map_err(|e| format!("write {MANIFEST}: {e}"))?;
        println!("psd-tools: wrote {} entries to {MANIFEST}", out.len());
        out
    } else {
        files
    };
    let bad = mismatches(&extracted, &fresh);
    if !bad.is_empty() {
        return Err(format!("psd-tools download does not match {MANIFEST} ({} problems): {}", bad.len(), bad.join(", ")));
    }
    // Keep only the verified PSD/PSB files (plus the licence), then swap into place.
    let _ = std::fs::remove_dir_all(&dest);
    for (_, file) in &fresh {
        let to = dest.join(file);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::fs::rename(extracted.join(file), &to).map_err(|e| format!("move {file}: {e}"))?;
    }
    std::fs::rename(staging.join(&top).join("LICENSE"), dest.join("LICENSE")).map_err(|e| format!("move LICENSE: {e}"))?;
    std::fs::write(
        dest.join("SOURCES.md"),
        format!(
            "# psd-tools test corpus\n\nThe PSD/PSB files of https://github.com/psd-tools/psd-tools/tree/{COMMIT}/tests/psd_files, \
             unmodified. MIT licence, Copyright (c) 2019 Kota Yamaguchi (see `LICENSE`). Fetched and sha256-verified by \
             `cargo xtask corpus --psd-tools` against `{MANIFEST}`. Gitignored; never commit these files.\n"
        ),
    )
    .map_err(|e| format!("write SOURCES.md: {e}"))?;
    let _ = std::fs::remove_dir_all(&staging);
    println!("psd-tools: {} verified files in {} (commit {COMMIT})", fresh.len(), dest.display());
    Ok(())
}
