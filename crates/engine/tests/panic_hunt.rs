//! Fuzz every command with adversarial params; a command must return `Err`, never panic.
//! (Rule 9 in AGENTS.md.) Params here keep the canvas small on purpose — validating absurd
//! dimensions is a separate concern; this hunts missing-param / empty-state / bad-index panics.
use photocraft_engine::{command_specs, Session};
use serde_json::{json, Value};
use std::panic::{catch_unwind, AssertUnwindSafe};

fn fresh() -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 24, "height": 16})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("select.rect", json!({"x": 1, "y": 1, "width": 6, "height": 5})).unwrap();
    s
}

// Slow (fresh session per command × params, in debug). Opt-in: `cargo test -p photocraft-engine
// --test panic_hunt -- --ignored`. Fast targeted regressions live beside the fixed commands.
#[test]
#[ignore = "slow full-registry fuzz; run with --ignored"]
fn no_command_panics_on_adversarial_params() {
    // None of these resize the canvas, so the fuzz stays fast.
    let params: Vec<Value> = vec![
        json!({}),
        json!({"x": -9, "y": -9, "radius": 0, "amount": 0, "angle": 0, "opacity": 0, "scale": 0, "levels": 0, "tolerance": 0, "gamma": 0, "layer": 999999, "channel": 999, "index": 999999}),
        json!({"x": 1e6, "y": -1e6, "radius": 500, "amount": 1000, "angle": 1e6, "opacity": -100, "scale": -5, "gamma": -1}),
        json!({"points": [], "from": [0, 0], "to": [0, 0], "stops": [], "matrix": [0, 0, 0, 0, 0, 0], "colors": [], "name": "", "mode": "", "style": ""}),
    ];
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut panicked: Vec<String> = Vec::new();
    for spec in command_specs() {
        for p in &params {
            let id = spec.id.to_string();
            let pc = p.clone();
            let crashed = catch_unwind(AssertUnwindSafe(|| {
                let mut s = fresh();
                let _ = s.execute(&id, pc);
            }))
            .is_err();
            if crashed {
                panicked.push(format!("{} <- {}", spec.id, p));
                break;
            }
        }
    }
    std::panic::set_hook(prev);
    assert!(panicked.is_empty(), "{} commands panicked:\n{}", panicked.len(), panicked.join("\n"));
}
