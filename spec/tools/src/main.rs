//! Registry checks and status report for `spec/`.
//!
//!   spec-tools [--repo <dir>] check     fail on any inconsistency
//!   spec-tools [--repo <dir>] status    print the per-platform state table
//!
//! The repository root defaults to the one this crate sits in. This is the only
//! code that reads the root README.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::Value;

const PLATFORMS: [&str; 2] = ["linux", "macos"];
const STATES: [&str; 4] = ["parity", "approximate", "excluded", "pending"];

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    if args.first().map(String::as_str) == Some("--repo") {
        if args.len() < 2 {
            return usage();
        }
        repo = PathBuf::from(args.remove(1));
        args.remove(0);
    }
    match args.first().map(String::as_str) {
        Some("check") => run_check(&repo),
        Some("status") => run_status(&repo),
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: spec-tools [--repo <dir>] <check|status>");
    ExitCode::from(2)
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Every `*.json` file under `dir`, recursively, sorted.
fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = fs::read_dir(&next) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "json") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Validates `instance` against the schema file, appending one problem per violation.
fn validate(schema_path: &Path, instance_path: &Path, problems: &mut Vec<String>) -> Option<Value> {
    let instance = match read_json(instance_path) {
        Ok(v) => v,
        Err(e) => {
            problems.push(e);
            return None;
        }
    };
    let schema = match read_json(schema_path) {
        Ok(v) => v,
        Err(e) => {
            problems.push(e);
            return None;
        }
    };
    let validator = match jsonschema::validator_for(&schema) {
        Ok(v) => v,
        Err(e) => {
            problems.push(format!("{}: not a valid schema: {e}", schema_path.display()));
            return None;
        }
    };
    let mut clean = true;
    for error in validator.iter_errors(&instance) {
        clean = false;
        problems.push(format!("{}: {} (at {})", instance_path.display(), error, error.instance_path()));
    }
    clean.then_some(instance)
}

/// First line of each top-level bullet under the README's `## Features` heading.
/// Continuation lines are indented, so they never start with `- `.
fn readme_bullets(readme: &str) -> Vec<String> {
    let mut in_features = false;
    let mut bullets = Vec::new();
    for line in readme.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            in_features = heading.trim() == "Features";
        } else if in_features && line.starts_with("- ") {
            bullets.push(line.to_string());
        }
    }
    bullets
}

struct Registry {
    features: Vec<Value>,
}

fn load_registry(repo: &Path, problems: &mut Vec<String>) -> Option<Registry> {
    let spec = repo.join("spec");
    let value = validate(&spec.join("schema/features.schema.json"), &spec.join("features.json"), problems)?;
    Some(Registry { features: value["features"].as_array().cloned().unwrap_or_default() })
}

fn state_of<'a>(feature: &'a Value, platform: &str) -> &'a str {
    feature[platform]["state"].as_str().unwrap_or("pending")
}

fn run_check(repo: &Path) -> ExitCode {
    let spec = repo.join("spec");
    let mut problems = Vec::new();
    let mut warnings = Vec::new();
    // Features with neither scenarios nor UI tests: expected until the runner lands.
    let mut uncovered: Vec<String> = Vec::new();

    // 1. Every JSON file under spec/ against its schema.
    let registry = load_registry(repo, &mut problems);

    let ignore_path = spec.join("readme-ignore.json");
    let ignored: Vec<String> = if ignore_path.exists() {
        validate(&spec.join("schema/readme-ignore.schema.json"), &ignore_path, &mut problems)
            .map(|v| {
                v["ignore"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e["readme"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    for (file, schema) in [("contracts/errors.json", "errors"), ("contracts/file-types.json", "file-types")] {
        let path = spec.join(file);
        if path.exists() {
            validate(&spec.join(format!("schema/{schema}.schema.json")), &path, &mut problems);
        }
    }

    // Scenario files: schema, and the id must be the path below scenarios/.
    let scenario_root = spec.join("scenarios");
    let mut scenarios: BTreeMap<String, Value> = BTreeMap::new();
    for path in json_files(&scenario_root) {
        let Some(value) = validate(&spec.join("schema/scenario.schema.json"), &path, &mut problems) else {
            continue;
        };
        let id = path
            .strip_prefix(&scenario_root)
            .unwrap_or(&path)
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/");
        if value["id"].as_str() != Some(id.as_str()) {
            problems.push(format!("{}: id {:?} does not match its path ({id})", path.display(), value["id"]));
        }
        if !id.starts_with(&format!("{}/", value["kind"].as_str().unwrap_or("?"))) {
            problems.push(format!("{}: kind {:?} does not match its directory", path.display(), value["kind"]));
        }
        scenarios.insert(id, value);
    }

    if let Some(registry) = &registry {
        // 2. README coverage, and every registry entry tied to a real README line.
        let mut readme = fs::read_to_string(repo.join("README.md")).unwrap_or_else(|e| {
            problems.push(format!("README.md: {e}"));
            String::new()
        });
        let mac_readme = repo.join("macos/README.md");
        let bullets = readme_bullets(&readme);
        if mac_readme.exists() {
            readme.push('\n');
            readme.push_str(&fs::read_to_string(&mac_readme).unwrap_or_default());
        }
        for bullet in &bullets {
            let covered = registry
                .features
                .iter()
                .any(|f| f["readme"].as_str().is_some_and(|r| bullet.contains(r)))
                || ignored.iter().any(|r| bullet.contains(r.as_str()));
            if !covered {
                problems.push(format!("README feature has no registry entry: {bullet}"));
            }
        }
        for feature in &registry.features {
            let id = feature["id"].as_str().unwrap_or("?");
            if let Some(r) = feature["readme"].as_str() {
                if !readme.lines().any(|l| l.contains(r)) {
                    problems.push(format!("{id}: readme text {r:?} matches no README line"));
                }
            }
        }

        // Unique ids.
        let mut seen = BTreeSet::new();
        for feature in &registry.features {
            let id = feature["id"].as_str().unwrap_or("?");
            if !seen.insert(id) {
                problems.push(format!("duplicate feature id {id}"));
            }
        }

        // 3. Scenario references both ways.
        let mut referenced = BTreeSet::new();
        for feature in &registry.features {
            let id = feature["id"].as_str().unwrap_or("?");
            let listed = feature["scenarios"].as_array().cloned().unwrap_or_default();
            let has_ui_tests = PLATFORMS.iter().any(|p| {
                feature["ui_tests"][p].as_array().is_some_and(|t| !t.is_empty())
            });
            if listed.is_empty() && !has_ui_tests {
                uncovered.push(id.to_string());
            }
            for scenario in listed.iter().filter_map(Value::as_str) {
                if scenarios.contains_key(scenario) {
                    referenced.insert(scenario.to_string());
                } else {
                    problems.push(format!("{id}: references missing scenario {scenario}"));
                }
            }
        }
        for id in scenarios.keys() {
            if !referenced.contains(id) {
                problems.push(format!("scenario {id} is referenced by no feature"));
            }
        }

        // 4 and 5. Notes, and per-platform overrides for approximate features.
        for feature in &registry.features {
            let id = feature["id"].as_str().unwrap_or("?");
            for platform in PLATFORMS {
                let state = state_of(feature, platform);
                if !STATES.contains(&state) {
                    continue;
                }
                if matches!(state, "approximate" | "excluded") && feature[platform]["note"].as_str().is_none() {
                    problems.push(format!("{id}: {platform} is {state} but has no note"));
                }
                if state == "approximate" {
                    for scenario in feature["scenarios"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        let has_override = scenarios
                            .get(scenario)
                            .is_some_and(|s| s["platforms"][platform].is_object());
                        if !has_override {
                            problems.push(format!(
                                "{id}: {platform} is approximate but {scenario} has no platforms.{platform} override"
                            ));
                        }
                    }
                }
            }
        }
    }

    if !uncovered.is_empty() {
        warnings.push(format!("{} feature(s) have no scenarios or ui_tests yet", uncovered.len()));
    }
    for warning in &warnings {
        println!("warning: {warning}");
    }
    if problems.is_empty() {
        println!(
            "spec ok: {} features, {} scenarios, {} warnings",
            registry.as_ref().map_or(0, |r| r.features.len()),
            scenarios.len(),
            warnings.len()
        );
        ExitCode::SUCCESS
    } else {
        for problem in &problems {
            eprintln!("error: {problem}");
        }
        eprintln!("spec check failed: {} problem(s)", problems.len());
        ExitCode::FAILURE
    }
}

fn run_status(repo: &Path) -> ExitCode {
    let mut problems = Vec::new();
    let Some(registry) = load_registry(repo, &mut problems) else {
        for problem in &problems {
            eprintln!("error: {problem}");
        }
        return ExitCode::FAILURE;
    };

    print!("{:<12}", "platform");
    for state in STATES {
        print!("{state:>13}");
    }
    println!("{:>8}", "total");
    for platform in PLATFORMS {
        print!("{platform:<12}");
        for state in STATES {
            let count = registry.features.iter().filter(|f| state_of(f, platform) == state).count();
            print!("{count:>13}");
        }
        println!("{:>8}", registry.features.len());
    }
    for platform in PLATFORMS {
        let pending: Vec<&str> = registry
            .features
            .iter()
            .filter(|f| state_of(f, platform) == "pending")
            .filter_map(|f| f["id"].as_str())
            .collect();
        if !pending.is_empty() {
            println!("\npending on {platform} ({}):", pending.len());
            for id in pending {
                println!("  {id}");
            }
        }
    }
    ExitCode::SUCCESS
}
