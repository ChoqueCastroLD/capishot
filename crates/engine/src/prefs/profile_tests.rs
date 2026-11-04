use super::*;

fn get(s: &mut Session, path: &str) -> Value {
    s.execute("prefs.get", json!({"path": path})).unwrap()
}

/// A session with something in every section.
fn customised() -> Session {
    let mut s = Session::new();
    let set = |s: &mut Session, path: &str, value: Value| s.execute("prefs.set", json!({"path": path, "value": value})).unwrap();
    set(&mut s, "interface.customThemes", json!([{"id": "tinta", "name": "Tinta", "css": ":root { --base: studio; --accent: #ff8800; }"}]));
    set(&mut s, "interface.customTheme", json!("tinta"));
    set(&mut s, "shortcuts.file.open", json!("Cmd+Shift+O"));
    set(&mut s, "workspaceLocked", json!(true));
    set(&mut s, "panelLayout", json!({"workspace": "Typesetting"}));
    set(&mut s, "general.beepWhenDone", json!(true));
    set(&mut s, "interface.notificationDurationSeconds", json!(12));
    set(&mut s, "type.favoriteFonts", json!(["Bangers"]));
    set(&mut s, "type.fontAliases", json!({"Bangers": "Gritos"}));
    set(&mut s, "fontProjects", json!({"projects": [{"id": "op", "name": "One Piece"}], "active": "op"}));
    set(&mut s, "fileHandling.recentFiles", json!(["/home/someone/page.psd"]));
    set(&mut s, "integrations.allowAgentControl", json!(false));
    s.execute("brush.presets.save", json!({"name": "Mi pincel"})).unwrap();
    s.execute("typer.style.save", json!({"style": {"name": "Diálogo", "prefixes": ["d:"]}})).unwrap();
    s
}

fn all() -> Vec<&'static str> {
    SECTIONS.iter().map(|(id, _)| *id).collect()
}

#[test]
fn every_section_round_trips_into_a_fresh_session() {
    let mut a = customised();
    let profile = capture(&mut a, &all());
    assert_eq!(profile.len(), SECTIONS.len(), "{:?}", profile.keys());
    // Through JSON, as `profile.json` carries it.
    let profile: Map<String, Value> = serde_json::from_str(&serde_json::to_string(&profile).unwrap()).unwrap();
    let mut b = Session::new();
    let r = b.execute("profile.apply", json!({"profile": profile})).unwrap();
    assert_eq!(r["applied"].as_array().unwrap().len(), SECTIONS.len(), "{r}");
    for path in [
        "interface.customThemes",
        "interface.customTheme",
        "shortcuts",
        "workspaceLocked",
        "panelLayout",
        "general.beepWhenDone",
        "interface.notificationDurationSeconds",
        "type.favoriteFonts",
        "type.fontAliases",
        "fontProjects",
    ] {
        assert_eq!(get(&mut b, path), get(&mut a, path), "{path}");
    }
    assert!(b.tools.presets.iter().any(|p| p.name == "Mi pincel"), "brushes");
    assert!(b.typer.state.library.styles.iter().any(|st| st.name == "Diálogo"), "TypeR styles");
    // Never this machine's files or its agent access.
    assert_eq!(get(&mut b, "fileHandling.recentFiles"), json!([]));
    assert_eq!(get(&mut b, "integrations.allowAgentControl"), json!(true), "the default");
}

#[test]
fn a_section_only_sets_the_paths_it_owns() {
    let mut s = Session::new();
    let hostile = json!({"theme": {"integrations.allowAgentControl": false, "shortcuts": {"file.open": "F1"}, "interface.customTheme": "X"}});
    s.execute("profile.apply", json!({"profile": hostile})).unwrap();
    assert_eq!(get(&mut s, "integrations.allowAgentControl"), json!(true));
    assert_eq!(get(&mut s, "shortcuts"), json!({}));
    assert_eq!(get(&mut s, "interface.customTheme"), json!("X"));
    assert!(s.execute("profile.apply", json!({"profile": {"theme": 3}})).is_err());
    assert!(s.execute("profile.apply", json!({})).is_err());
}

#[test]
fn merge_keeps_what_the_profile_lacks_and_replace_takes_the_profile() {
    let mut s = Session::new();
    s.execute("prefs.set", json!({"path": "shortcuts.file.open", "value": "F2"})).unwrap();
    s.execute("prefs.set", json!({"path": "interface.customThemes", "value": [{"id": "mine", "name": "Mine", "css": "--base: pro;"}, {"id": "shared", "name": "Shared", "css": "--base: pro;"}]})).unwrap();
    s.execute("prefs.set", json!({"path": "type.favoriteFonts", "value": ["Arial"]})).unwrap();
    let profile = json!({
        "shortcuts": {"shortcuts": {"file.save": "F3"}},
        "theme": {"interface.customThemes": [{"id": "shared", "name": "Shared", "css": "--base: studio;"}, {"id": "new", "name": "New", "css": "--base: classic;"}]},
        "fonts": {"type.favoriteFonts": ["Bangers", "Arial"]},
    });
    s.execute("profile.apply", json!({"profile": profile, "merge": true})).unwrap();
    assert_eq!(get(&mut s, "shortcuts"), json!({"file.open": "F2", "file.save": "F3"}));
    let themes = get(&mut s, "interface.customThemes");
    let names: Vec<&str> = themes.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Mine", "Shared", "New"]);
    assert_eq!(themes[1]["css"], "--base: studio;", "the same id is replaced");
    assert_eq!(get(&mut s, "type.favoriteFonts"), json!(["Arial", "Bangers"]));
    s.execute("profile.apply", json!({"profile": profile, "merge": false})).unwrap();
    assert_eq!(get(&mut s, "shortcuts"), json!({"file.save": "F3"}));
    assert_eq!(get(&mut s, "interface.customThemes").as_array().unwrap().len(), 2);
    assert_eq!(get(&mut s, "type.favoriteFonts"), json!(["Bangers", "Arial"]));
}

#[test]
fn only_the_chosen_sections_apply() {
    let mut a = customised();
    let profile = capture(&mut a, &all());
    let mut b = Session::new();
    let r = b.execute("profile.apply", json!({"profile": profile, "sections": ["shortcuts", "nope"]})).unwrap();
    assert_eq!(r["applied"], json!(["shortcuts"]));
    assert_eq!(r["ignored"], json!(["nope"]));
    assert_eq!(get(&mut b, "shortcuts.file.open"), json!("Cmd+Shift+O"));
    assert_eq!(get(&mut b, "interface.customThemes"), json!([]));
}

#[test]
fn undo_restores_the_profile_before_the_import() {
    let mut s = Session::new();
    assert!(s.execute("profile.undoImport", json!({})).is_err(), "nothing to undo");
    assert!(!s.is_enabled("profile.undoImport"));
    s.execute("prefs.set", json!({"path": "shortcuts.file.open", "value": "F2"})).unwrap();
    s.execute("prefs.set", json!({"path": "general.beepWhenDone", "value": true})).unwrap();
    let before = get(&mut s, "");
    let mut other = customised();
    let profile = capture(&mut other, &all());
    s.execute("profile.apply", json!({"profile": profile, "extra": {"bubble": {"kept": 1}}})).unwrap();
    assert_eq!(get(&mut s, "interface.customTheme"), json!("tinta"));
    assert!(s.is_enabled("profile.undoImport"));
    let r = s.execute("profile.undoImport", json!({})).unwrap();
    assert_eq!(r["extra"], json!({"bubble": {"kept": 1}}), "the shell's sections come back to it");
    let mut after = get(&mut s, "");
    after["profileBackup"] = Value::Null;
    let mut before = before;
    before["profileBackup"] = Value::Null;
    assert_eq!(after, before);
    assert!(s.execute("profile.undoImport", json!({})).is_err(), "one undo");
}

#[test]
fn choosing_a_built_in_theme_leaves_the_custom_one() {
    let mut s = Session::new();
    s.execute("prefs.set", json!({"values": {"interface.customThemes": [{"id": "t", "name": "T", "css": "--base: pro;"}], "interface.customTheme": "t"}})).unwrap();
    assert_eq!(s.prefs().interface.active_custom_theme().map(|t| t.name.as_str()), Some("T"));
    s.execute("prefs.set", json!({"path": "interface.notificationDurationSeconds", "value": 9})).unwrap();
    assert_eq!(s.prefs().interface.custom_theme, "t", "other settings keep it");
    s.execute("prefs.set", json!({"path": "interface.darkTheme", "value": "studio"})).unwrap();
    assert_eq!(s.prefs().interface.custom_theme, "");
    assert!(s.prefs().interface.active_custom_theme().is_none());
    // A custom theme chosen with the base theme in the same change stays.
    s.execute("prefs.set", json!({"values": {"interface.darkTheme": "pro", "interface.customTheme": "t"}})).unwrap();
    assert_eq!(s.prefs().interface.custom_theme, "t");
}

#[test]
fn custom_themes_survive_a_restart() {
    let s = customised();
    let saved = s.prefs_to_json();
    let mut t = Session::new();
    t.load_prefs_json(&saved).unwrap();
    assert_eq!(t.prefs().interface.active_custom_theme().map(|c| c.css.as_str()), Some(":root { --base: studio; --accent: #ff8800; }"));
}

/// `profile.export` → `profile.import` in another session, then undo.
#[test]
fn a_profile_file_round_trips_through_the_commands() {
    let mut a = customised();
    let out = a.execute("profile.export", json!({"sections": ["theme", "shortcuts"], "name": "Equipo", "meta": {"author": "Equipo"}})).unwrap();
    assert!(out["file"].as_str().unwrap().ends_with(".proj.capishot"), "{out}");
    let mut b = Session::new();
    let r = b.execute("profile.import", json!({"data": out["data"]})).unwrap();
    assert_eq!(r["applied"], json!(["shortcuts", "theme"]));
    assert_eq!(get(&mut b, "interface.customThemes"), get(&mut a, "interface.customThemes"));
    assert_eq!(get(&mut b, "shortcuts.file.open"), json!("Cmd+Shift+O"));
    b.execute("profile.undoImport", json!({})).unwrap();
    assert_eq!(get(&mut b, "interface.customThemes"), json!([]));
    assert!(b.execute("profile.import", json!({"data": "%%"})).is_err());
}

#[test]
fn merge_rules() {
    let mut cur = json!({"a": [1, 2], "m": {"x": 1, "y": {"deep": 1}}, "s": 1});
    merge(&mut cur, json!({"a": [2, 3], "m": {"y": {"other": 2}}, "s": 2, "n": 0}));
    assert_eq!(cur, json!({"a": [1, 2, 3], "m": {"x": 1, "y": {"other": 2}}, "s": 2, "n": 0}));
}
