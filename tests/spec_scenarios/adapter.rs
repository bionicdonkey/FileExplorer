//! `kind: "adapter"` scenarios: the real adapters over a real temporary filesystem.
//!
//! `operate` runs `gio_operations::execute` / `execute_with_decision`, the same entry the
//! GIO adapter drives, which is synchronous and delivers its events to a closure. Its
//! `capability_version` is the adapter's current one (`local_capabilities().version`)
//! unless the step names one.

use std::collections::HashMap;

use serde_json::Value;
use zorin_explorer::core::contracts::*;
use zorin_explorer::core::Location;
use zorin_explorer::infrastructure::{gio_operations, gio_operations::GioOperations};

use crate::sandbox::Sandbox;
use crate::{effective_steps, Check, Mode};

/// The recorded run of one `operate` step.
struct Run {
    events: Vec<OperationEvent>,
}

impl Run {
    fn terminal(&self) -> Result<&OperationEvent, String> {
        let terminals = self.events.iter().filter(|e| e.is_terminal()).count();
        if terminals != 1 {
            return Err(format!("expected exactly one terminal event, saw {terminals}"));
        }
        self.events.last().filter(|e| e.is_terminal()).ok_or_else(|| "the terminal event is not last".to_string())
    }

    fn completion(&self) -> Result<&OperationCompletion, String> {
        Ok(match self.terminal()? {
            OperationEvent::Succeeded { completion, .. } | OperationEvent::Failed { completion, .. } => completion,
            OperationEvent::Cancelled { outcome, .. } => &outcome.completion,
            _ => unreachable!("terminal"),
        })
    }
}

fn event_name(event: &OperationEvent) -> &'static str {
    match event {
        OperationEvent::Started { .. } => "started",
        OperationEvent::Progress { .. } => "progress",
        OperationEvent::CancelRequested { .. } => "cancel_requested",
        OperationEvent::Succeeded { .. } => "succeeded",
        OperationEvent::Failed { .. } => "failed",
        OperationEvent::Cancelled { .. } => "cancelled",
    }
}

fn sequence(event: &OperationEvent) -> u64 {
    match event {
        OperationEvent::Started { sequence, .. }
        | OperationEvent::Progress { sequence, .. }
        | OperationEvent::CancelRequested { sequence, .. }
        | OperationEvent::Succeeded { sequence, .. }
        | OperationEvent::Failed { sequence, .. }
        | OperationEvent::Cancelled { sequence, .. } => *sequence,
    }
}

fn error_kind_name(kind: OperationErrorKind) -> &'static str {
    use OperationErrorKind::*;
    match kind {
        PermissionDenied => "permission_denied",
        AuthenticationRequired => "authentication_required",
        MountUnavailable => "mount_unavailable",
        Conflict => "conflict",
        NotFound => "not_found",
        ReadOnly => "read_only",
        Unsupported => "unsupported",
        CapabilityChanged => "capability_changed",
        PartialCompletion => "partial_completion",
        InvalidRequest => "invalid_request",
        Unknown => "unknown",
    }
}

fn retry_name(retry: RetryClass) -> &'static str {
    match retry {
        RetryClass::Never => "never",
        RetryClass::AfterRefresh => "after_refresh",
        RetryClass::AfterUserAction => "after_user_action",
        RetryClass::Transient => "transient",
    }
}

fn effect_name(effect: MutationEffect) -> &'static str {
    match effect {
        MutationEffect::Unchanged => "unchanged",
        MutationEffect::Changed => "changed",
        MutationEffect::PartiallyChanged => "partially_changed",
        MutationEffect::Unknown => "unknown",
    }
}

fn paths(value: &Value) -> Vec<&str> {
    value.as_array().into_iter().flatten().filter_map(Value::as_str).collect()
}

pub fn run(scenario: &Value, mode: Mode) -> Check {
    let mut sandbox = Sandbox::new();
    sandbox.build(&scenario["given"]["tree"])?;
    let mut runs: HashMap<String, Run> = HashMap::new();
    let mut next_id = 1u64;

    for (index, step) in effective_steps(scenario).iter().enumerate() {
        let at = |e: String| format!("step {index}: {e}");
        if let Some(action) = step["do"].as_str() {
            match action {
                "operate" => {
                    let run = operate(&sandbox, step, next_id).map_err(at)?;
                    next_id += 1;
                    if mode == Mode::Excluded {
                        return expect_unsupported(&run).map_err(at);
                    }
                    runs.insert(step["as"].as_str().unwrap_or("_").to_string(), run);
                }
                "mutate" => {
                    sandbox.build(&step["tree"]).map_err(at)?;
                    if let Some(remove) = step["remove"].as_str() {
                        sandbox.remove(remove).map_err(at)?;
                    }
                }
                other => return Err(at(format!("action {other:?} is not implemented by the adapter runner yet"))),
            }
        } else if let Some(expect) = step["expect"].as_str() {
            expectation(&sandbox, &runs, expect, step).map_err(at)?;
        } else {
            return Err(at(format!("step has neither `do` nor `expect`: {step}")));
        }
    }
    Ok(())
}

fn operate(sandbox: &Sandbox, step: &Value, id: u64) -> Result<Run, String> {
    let kind = match step["kind"].as_str().ok_or("operate needs `kind`")? {
        "copy" => MutationKind::Copy,
        "move" => MutationKind::Move,
        "create" => MutationKind::Create,
        "rename" => MutationKind::Rename,
        "trash" => MutationKind::Trash,
        "delete" => MutationKind::Delete,
        other => return Err(format!("unknown operation kind {other:?}")),
    };
    let location = |relative: &str| Location::from_local_path(&sandbox.path(relative));
    let request = OperationRequest {
        id: OperationId(id),
        kind,
        sources: paths(&step["sources"]).into_iter().map(location).collect(),
        destination: step["destination"].as_str().map(location),
        explicit_target: step["target"].as_str().map(location),
        capability_version: step["capability_version"]
            .as_u64()
            .unwrap_or_else(|| GioOperations::local_capabilities().version),
        cancellable: gio::Cancellable::new(),
    };
    let decision = match step["decision"].as_str() {
        None => None,
        Some("replace") => Some(ConflictDecision::Replace),
        Some("keep_both") => Some(ConflictDecision::KeepBoth),
        Some("skip") => Some(ConflictDecision::Skip),
        Some("cancel_remaining") => Some(ConflictDecision::CancelRemaining),
        Some(other) => return Err(format!("unknown decision {other:?}")),
    };
    let mut events = Vec::new();
    let mut sink = |event| events.push(event);
    match decision {
        Some(decision) => gio_operations::execute_with_decision(request, decision, &mut sink),
        None => gio_operations::execute(request, &mut sink),
    }
    let run = Run { events };
    // The runner checks this itself; scenarios do not repeat it (spec 3.2).
    let sequences: Vec<u64> = run.events.iter().map(sequence).collect();
    if sequences.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(format!("event sequence numbers are not strictly increasing: {sequences:?}"));
    }
    run.terminal()?;
    Ok(run)
}

/// Spec 3.5: an excluded feature must say so explicitly.
fn expect_unsupported(run: &Run) -> Check {
    match run.terminal()? {
        OperationEvent::Failed { error, .. } if error.kind == OperationErrorKind::Unsupported => Ok(()),
        other => Err(format!("excluded feature did not report unsupported: {}", event_name(other))),
    }
}

fn expectation(sandbox: &Sandbox, runs: &HashMap<String, Run>, expect: &str, step: &Value) -> Check {
    let find = || -> Result<&Run, String> {
        let name = step["ref"].as_str().ok_or("expectation needs `ref`")?.trim_start_matches('$');
        runs.get(name).ok_or_else(|| format!("no step bound {name:?}"))
    };
    match expect {
        "outcome" => {
            let run = find()?;
            let want = step["is"].as_str().ok_or("outcome needs `is`")?;
            let terminal = run.terminal()?;
            let got = event_name(terminal);
            if got != want {
                return Err(format!("outcome is {got}, expected {want}"));
            }
            if let (OperationEvent::Failed { error, .. }, Some(expected)) = (terminal, step["error"].as_object()) {
                if let Some(kind) = expected.get("kind").and_then(Value::as_str) {
                    if error_kind_name(error.kind) != kind {
                        return Err(format!("error kind is {}, expected {kind}", error_kind_name(error.kind)));
                    }
                }
                if let Some(retry) = expected.get("retry").and_then(Value::as_str) {
                    if retry_name(error.retry) != retry {
                        return Err(format!("retry class is {}, expected {retry}", retry_name(error.retry)));
                    }
                }
            }
            Ok(())
        }
        "effect" => {
            let got = effect_name(find()?.completion()?.effect);
            let want = step["is"].as_str().ok_or("effect needs `is`")?;
            if got == want { Ok(()) } else { Err(format!("effect is {got}, expected {want}")) }
        }
        "events" => {
            let run = find()?;
            let names: Vec<&str> = run.events.iter().map(event_name).collect();
            let pattern = step["match"].as_array().ok_or("events needs `match`")?;
            if matches_pattern(&names, pattern) {
                Ok(())
            } else {
                Err(format!("events {names:?} do not match {}", step["match"]))
            }
        }
        "tree" => {
            for path in paths(&step["contains"]) {
                if std::fs::symlink_metadata(sandbox.path(path)).is_err() {
                    return Err(format!("expected {path} to exist"));
                }
            }
            for path in paths(&step["absent"]) {
                if std::fs::symlink_metadata(sandbox.path(path)).is_ok() {
                    return Err(format!("expected {path} to be absent"));
                }
            }
            // Additive to spec 3.2: `contents` maps a path to the text it must hold.
            for (path, want) in step["contents"].as_object().into_iter().flatten() {
                let got = std::fs::read_to_string(sandbox.path(path)).map_err(|e| format!("reading {path}: {e}"))?;
                if Some(got.as_str()) != want.as_str() {
                    return Err(format!("{path} holds {got:?}, expected {want}"));
                }
            }
            Ok(())
        }
        other => Err(format!("expectation {other:?} is not implemented by the adapter runner yet")),
    }
}

/// `["started", {"any":"progress"}, "succeeded"]`: literals, zero-or-more, one-or-more.
fn matches_pattern(names: &[&str], pattern: &[Value]) -> bool {
    let Some((head, rest)) = pattern.split_first() else { return names.is_empty() };
    if let Some(name) = head["any"].as_str() {
        let mut taken = 0;
        loop {
            if matches_pattern(&names[taken..], rest) {
                return true;
            }
            if taken < names.len() && names[taken] == name {
                taken += 1;
            } else {
                return false;
            }
        }
    }
    if let Some(name) = head["some"].as_str() {
        let mut taken = 0;
        while taken < names.len() && names[taken] == name {
            taken += 1;
            if matches_pattern(&names[taken..], rest) {
                return true;
            }
        }
        return false;
    }
    match (head.as_str(), names.split_first()) {
        (Some(literal), Some((first, remaining))) => *first == literal && matches_pattern(remaining, rest),
        _ => false,
    }
}
