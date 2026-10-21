//! Runs the behaviour scenarios of `spec/` against this app.
//!
//! Every scenario of every feature whose `linux.state` is not `pending` is run.
//! `harness = false`, so each scenario prints one `ok` / `FAILED` line.
//!
//!   SPEC_FILTER=<substring>   only scenarios whose id contains it (positional arguments
//!                             that do not start with `-` act the same way)
//!   SPEC_REPEAT=<n>           run every selected scenario n times (soak)
//!
//! Unknown arguments, such as `--nocapture` or `--list`, are ignored so that unrelated
//! `cargo test` invocations never fail on this binary.

mod adapter;
mod domain;
mod sandbox;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use serde_json::Value;

pub type Check = Result<(), String>;

fn spec_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("spec")
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// How a feature's state decides what a scenario must do on this platform.
#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Normal,
    /// `excluded`: the first action must produce the explicit unsupported outcome.
    Excluded,
}

struct Selected {
    id: String,
    feature: String,
    mode: Mode,
    scenario: Value,
}

fn selected_scenarios() -> Vec<Selected> {
    let spec = spec_dir();
    let registry = read_json(&spec.join("features.json"));
    let mut out = Vec::new();
    for feature in registry["features"].as_array().expect("features array") {
        let state = feature["linux"]["state"].as_str().unwrap_or("pending");
        let mode = match state {
            "pending" => continue,
            "excluded" => Mode::Excluded,
            _ => Mode::Normal,
        };
        for id in feature["scenarios"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            let scenario = read_json(&spec.join("scenarios").join(format!("{id}.json")));
            out.push(Selected {
                id: id.to_string(),
                feature: feature["id"].as_str().unwrap_or("?").to_string(),
                mode,
                scenario,
            });
        }
    }
    out
}

/// Applies `platforms.linux` overrides: `replace_step` and `skip_step` (spec 3.4).
pub fn effective_steps(scenario: &Value) -> Vec<Value> {
    let mut steps: Vec<Option<Value>> = scenario["steps"].as_array().cloned().unwrap_or_default().into_iter().map(Some).collect();
    let over = &scenario["platforms"]["linux"];
    if let Some(replace) = over["replace_step"].as_object() {
        for (index, step) in replace {
            if let Ok(i) = index.parse::<usize>() {
                if i < steps.len() {
                    steps[i] = Some(step.clone());
                }
            }
        }
    }
    for i in over["skip_step"].as_array().into_iter().flatten().filter_map(Value::as_u64) {
        if let Some(slot) = steps.get_mut(i as usize) {
            *slot = None;
        }
    }
    steps.into_iter().flatten().collect()
}

fn run_one(selected: &Selected) -> Check {
    let kind = selected.scenario["kind"].as_str().unwrap_or("?");
    match kind {
        "adapter" => adapter::run(&selected.scenario, selected.mode),
        "domain" => domain::run(&selected.scenario, selected.mode),
        other => Err(format!("scenario kind {other:?} is not implemented by this runner yet")),
    }
}

fn main() {
    let mut filters: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    if let Ok(f) = std::env::var("SPEC_FILTER") {
        filters.push(f);
    }
    let repeat: usize = std::env::var("SPEC_REPEAT").ok().and_then(|v| v.parse().ok()).unwrap_or(1).max(1);

    let all = selected_scenarios();
    let chosen: Vec<&Selected> = all
        .iter()
        .filter(|s| filters.is_empty() || filters.iter().any(|f| s.id.contains(f.as_str())))
        .collect();

    println!("running {} scenario(s) x {repeat}", chosen.len());
    let mut failures = 0;
    for selected in chosen {
        let mut result: Check = Ok(());
        for round in 0..repeat {
            result = match catch_unwind(AssertUnwindSafe(|| run_one(selected))) {
                Ok(r) => r,
                Err(panic) => Err(format!(
                    "panicked: {}",
                    panic.downcast_ref::<String>().cloned().or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
                )),
            };
            if let Err(e) = &result {
                result = Err(if repeat > 1 { format!("round {}: {e}", round + 1) } else { e.clone() });
                break;
            }
        }
        match result {
            Ok(()) => println!("scenario {} ({}) ... ok", selected.id, selected.feature),
            Err(e) => {
                failures += 1;
                println!("scenario {} ({}) ... FAILED\n    {e}", selected.id, selected.feature);
            }
        }
    }
    if failures > 0 {
        println!("\nspec scenarios: {failures} failed");
        std::process::exit(1);
    }
    println!("\nspec scenarios: all passed");
}
