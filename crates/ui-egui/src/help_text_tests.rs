use serde_json::json;

use super::*;
use crate::PhotocraftApp;
use crate::state::Tool;

/// Every tool says what it does in English, Spanish and Portuguese (`xtask tool-help` checks the
/// same from the sources).
#[test]
fn every_tool_has_a_description_in_three_languages() {
    let missing: Vec<String> = Tool::ALL
        .iter()
        .filter(|t| entry(&format!("tool:{t:?}")).is_none_or(|h| h.en.trim().is_empty() || h.es.trim().is_empty() || h.pt.trim().is_empty()))
        .map(|t| format!("{t:?}"))
        .collect();
    assert!(missing.is_empty(), "tools without a description: {missing:?}");
    for t in Tool::ALL {
        let h = entry(&format!("tool:{t:?}")).unwrap();
        assert!(h.en != h.es && h.es != h.pt, "{t:?}: three different languages");
    }
}

/// The described commands are real menu commands, at least 120 of them, each listed once.
#[test]
fn the_described_commands_exist_and_are_many() {
    let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let menus = crate::menus::menu_items(&app);
    let commands: Vec<&str> = HELP.iter().map(|h| h.id).filter(|id| !id.starts_with("tool:")).collect();
    let unknown: Vec<&&str> = commands.iter().filter(|id| !menus.iter().any(|m| m.id == **id)).collect();
    assert!(unknown.is_empty(), "described ids that aren't menu commands: {unknown:?}");
    assert!(commands.len() >= 120, "{} commands described", commands.len());
    let mut ids: Vec<&str> = HELP.iter().map(|h| h.id).collect();
    ids.sort_unstable();
    let n = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), n, "an id is described twice");
    for w in WHICH {
        for m in w.members {
            assert!(entry(m).is_some(), "{m} in a \"which\" hint has no description");
        }
    }
}

#[test]
fn description_words_match_by_stem() {
    assert!(words_score("borrar texto", "Pinta sobre texto u objetos y la IA los borra rellenando el fondo").is_some());
    assert!(words_score("borrar texto", "Borra pintando, sin IA").is_none(), "every word must be there");
    assert!(words_score("apagar texto", "Pinte sobre texto ou objetos e a IA os apaga").is_some());
    assert!(words_score("tamaño", "Cambia el tamano en píxeles").is_some(), "accents don't matter");
    assert!(words_score("de", "de la imagen").is_none(), "short words are not searched");
}

fn hits(app: &PhotocraftApp, q: &str) -> Vec<String> {
    let ctx = egui::Context::default();
    crate::palette::hits_for(app, &ctx, q).into_iter().map(|h| h.1).collect()
}

/// Searching what a tool does finds it: "borrar texto" is Remove with AI first, in each language.
#[test]
fn the_palette_searches_the_descriptions() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "es"}})).unwrap();
    assert_eq!(hits(&app, "borrar texto").first().map(String::as_str), Some("tool:RemoveAi"));
    assert_eq!(hits(&app, "apagar texto").first().map(String::as_str), Some("tool:RemoveAi"), "Portuguese in a Spanish UI");
    assert_eq!(hits(&app, "erase text").first().map(String::as_str), Some("tool:RemoveAi"), "English");
    assert_eq!(hits(&app, "globo texto").first().map(String::as_str), Some("tool:BubbleType"));
    // One everyday word still finds its synonym first.
    assert_eq!(hits(&app, "borrar").first().map(String::as_str), Some("tool:Eraser"));
}

/// The rows come grouped (tools, commands, panels, AI: each group's rows together) and similar
/// tools among them bring the "Which one should I use?" hint.
#[test]
fn results_are_grouped_and_similar_tools_explained() {
    let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    for q in ["borrar texto", "layer", "select", "blur"] {
        let ids = hits(&app, q);
        let groups: Vec<Group> = ids.iter().filter(|id| !id.starts_with("quick:")).map(|id| group_of(id)).collect();
        let mut seen: Vec<Group> = Vec::new();
        for (i, g) in groups.iter().enumerate() {
            if i == 0 || groups[i - 1] != *g {
                assert!(!seen.contains(g), "{q:?}: the {g:?} rows are split: {ids:?}");
                seen.push(*g);
            }
        }
    }
    let ids = hits(&app, "borrar texto");
    let w = which_for(ids.iter().map(String::as_str)).expect("a hint for the removal tools");
    assert!(w.members.contains(&"tool:RemoveAi"));
    assert_eq!(group_of("tool:RemoveAi"), Group::Ai);
    assert_eq!(group_of("window.panel.layers"), Group::Panels);
    assert_eq!(group_of("tool:Brush"), Group::Tools);
    assert_eq!(group_of("image.imageSize"), Group::Commands);
}
