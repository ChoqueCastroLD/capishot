//! `xtask tool-help`: every tool of the editor (`Tool::ALL` in `crates/ui-egui/src/state.rs`)
//! says what it does in English, Spanish and Portuguese (`crates/ui-egui/src/help_text.rs`),
//! and every described entry has its three sentences.

use std::collections::BTreeMap;
use std::path::Path;

pub fn run(root: &Path) -> Result<(), String> {
    let read = |p: &str| std::fs::read_to_string(root.join(p)).map_err(|e| format!("{p}: {e}"));
    let tools = tools(&read("crates/ui-egui/src/state.rs")?)?;
    let help = entries(&read("crates/ui-egui/src/help_text.rs")?);
    let mut problems = Vec::new();
    for t in &tools {
        match help.get(&format!("tool:{t}")) {
            None => problems.push(format!("tool {t}: no description")),
            Some(s) if s.len() != 3 || s.iter().any(|x| x.trim().is_empty()) => problems.push(format!("tool {t}: needs English, Spanish and Portuguese")),
            Some(_) => {}
        }
    }
    for (id, s) in &help {
        if s.len() != 3 || s.iter().any(|x| x.trim().is_empty()) {
            problems.push(format!("{id}: needs English, Spanish and Portuguese"));
        }
    }
    let commands = help.keys().filter(|k| !k.starts_with("tool:")).count();
    if problems.is_empty() {
        println!("tool-help: {} tools and {commands} commands described in en/es/pt", tools.len());
        Ok(())
    } else {
        Err(format!("tool-help:\n  {}", problems.join("\n  ")))
    }
}

/// The names in `pub const ALL: [Tool; N] = [ Tool::A, … ];`.
fn tools(src: &str) -> Result<Vec<String>, String> {
    let start = src.find("pub const ALL: [Tool;").ok_or("state.rs: no Tool::ALL")?;
    let body = &src[start..];
    let open = body.find("= [").ok_or("state.rs: Tool::ALL has no list")? + 3;
    let close = body[open..].find("];").ok_or("state.rs: Tool::ALL isn't closed")? + open;
    Ok(body[open..close].split(',').filter_map(|s| s.trim().strip_prefix("Tool::")).map(str::to_string).collect())
}

/// `h("id", "en", "es", "pt"),` lines: id → the sentences.
fn entries(src: &str) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for line in src.lines() {
        let Some(rest) = line.trim().strip_prefix("h(") else { continue };
        let lits = literals(rest);
        if let Some((id, texts)) = lits.split_first() {
            out.insert(id.clone(), texts.to_vec());
        }
    }
    out
}

/// The string literals on a line, in order (`\"` kept as a quote).
fn literals(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (&mut cur, c) {
            (None, '"') => cur = Some(String::new()),
            (Some(_), '"') => out.push(cur.take().unwrap_or_default()),
            (Some(t), '\\') => {
                if let Some(n) = chars.next() {
                    t.push(n);
                }
            }
            (Some(t), c) => t.push(c),
            (None, _) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_sources_pass() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
        super::run(&root).unwrap();
    }

    #[test]
    fn a_missing_language_is_reported() {
        let e = super::entries("    h(\"tool:Move\", \"Drag\", \"\", \"Arraste\"),\n");
        assert_eq!(e["tool:Move"], ["Drag", "", "Arraste"]);
        assert_eq!(super::tools("pub const ALL: [Tool; 2] = [\n Tool::Move,\n Tool::Brush,\n];").unwrap(), ["Move", "Brush"]);
    }
}
