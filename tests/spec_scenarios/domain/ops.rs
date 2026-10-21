//! The `operations` subject: the operation coordinator over a scripted backend, plus the
//! event reducer. Two ports are scripted. `capabilities` holds the coordinator's capability
//! inspections, `operations` holds the mutations it dispatches; both answer only when a step
//! says so. Event sequence numbers are assigned in order per dispatched operation.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::Value;
use zorin_explorer::application::operations::{
    elevated_retry_request, lock_keys, CancelResult, ChangeExtent, OperationCoordinator, OperationProjection,
    OperationState,
};
use zorin_explorer::core::contracts::*;
use zorin_explorer::core::ports::*;
use zorin_explorer::core::Location;

use super::{error_kind, location, Bound, World};
use crate::Check;

struct HeldInspection {
    request: CapabilityRequest,
    done: RefCell<Option<CapabilityCallback>>,
}

struct HeldOperation {
    request: OperationRequest,
    sink: Rc<RefCell<OperationSink>>,
    next_sequence: Cell<u64>,
}

#[derive(Default)]
pub struct Scripted {
    inspections: RefCell<Vec<HeldInspection>>,
    operations: RefCell<Vec<HeldOperation>>,
}

impl FileOperations for Scripted {
    fn inspect_capabilities(&self, request: CapabilityRequest, done: CapabilityCallback) {
        self.inspections.borrow_mut().push(HeldInspection { request, done: RefCell::new(Some(done)) });
    }
    fn submit(&self, request: OperationRequest, sink: OperationSink) {
        self.operations.borrow_mut().push(HeldOperation {
            request,
            sink: Rc::new(RefCell::new(sink)),
            next_sequence: Cell::new(1),
        });
    }
}

pub struct Ops {
    scripted: Rc<Scripted>,
    coordinator: OperationCoordinator,
}

impl Ops {
    pub fn new() -> Self {
        let scripted = Rc::new(Scripted::default());
        Self { coordinator: OperationCoordinator::new(scripted.clone()), scripted }
    }
}

fn capability(name: &str) -> Result<Capability, String> {
    Ok(match name {
        "list" => Capability::List,
        "copy" => Capability::Copy,
        "move" => Capability::Move,
        "create" => Capability::Create,
        "rename" => Capability::Rename,
        "trash" => Capability::Trash,
        "delete" => Capability::Delete,
        "open" => Capability::Open,
        "search_filename" => Capability::SearchFilename,
        "search_content" => Capability::SearchContent,
        other => return Err(format!("unknown capability {other:?}")),
    })
}

/// Builds a snapshot from the scenario default overlaid with the step's `capabilities`:
/// `version`, `supported` (replaces), `with`, `without`, `containment` ("opaque" or
/// `{"ancestors": [uri]}`), `stable_order`.
fn snapshot(world: &World, over: &Value) -> Result<CapabilitySnapshot, String> {
    let field = |name: &str| -> &Value {
        if over.get(name).is_some() { &over[name] } else { &world.default_capabilities[name] }
    };
    let mut supported: Vec<Capability> = field("supported")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(capability)
        .collect::<Result<_, _>>()?;
    for name in over["with"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        supported.push(capability(name)?);
    }
    for name in over["without"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        let gone = capability(name)?;
        supported.retain(|c| *c != gone);
    }
    let containment = match field("containment") {
        Value::String(s) if s == "opaque" => Containment::Opaque,
        Value::Null => Containment::Opaque,
        Value::Object(map) => Containment::KnownAncestors(
            map.get("ancestors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(location)
                .collect::<Result<_, _>>()?,
        ),
        other => return Err(format!("bad containment {other}")),
    };
    Ok(CapabilitySnapshot {
        version: field("version").as_u64().unwrap_or(1),
        supported: supported.into_iter().collect(),
        stable_order: field("stable_order").as_bool().unwrap_or(true),
        containment,
    })
}

fn locations(value: &Value) -> Result<Vec<Location>, String> {
    value.as_array().into_iter().flatten().filter_map(Value::as_str).map(location).collect()
}

fn kind(name: &str) -> Result<MutationKind, String> {
    Ok(match name {
        "copy" => MutationKind::Copy,
        "move" => MutationKind::Move,
        "create" => MutationKind::Create,
        "rename" => MutationKind::Rename,
        "trash" => MutationKind::Trash,
        "delete" => MutationKind::Delete,
        other => return Err(format!("unknown operation kind {other:?}")),
    })
}

fn request(step: &Value) -> Result<OperationRequest, String> {
    Ok(OperationRequest {
        id: OperationId(step["id"].as_u64().ok_or("needs a numeric `id`")?),
        kind: kind(step["kind"].as_str().unwrap_or("copy"))?,
        sources: locations(&step["sources"])?,
        destination: step["destination"].as_str().map(location).transpose()?,
        explicit_target: step["target"].as_str().map(location).transpose()?,
        capability_version: step["capability_version"].as_u64().unwrap_or(1),
        cancellable: gio::Cancellable::new(),
    })
}

fn decision(name: &str) -> Result<ConflictDecision, String> {
    Ok(match name {
        "replace" => ConflictDecision::Replace,
        "keep_both" => ConflictDecision::KeepBoth,
        "skip" => ConflictDecision::Skip,
        "cancel_remaining" => ConflictDecision::CancelRemaining,
        other => return Err(format!("unknown decision {other:?}")),
    })
}

fn bind(world: &World, step: &Value, value: Bound) {
    if let Some(name) = step["as"].as_str() {
        world.bound.borrow_mut().insert(name.to_string(), value);
    }
}

fn resolve_id(world: &World, step: &Value, field: &str) -> Result<OperationId, String> {
    match &step[field] {
        Value::Number(n) => Ok(OperationId(n.as_u64().ok_or("bad id")?)),
        Value::String(name) => match world.bound.borrow().get(name.trim_start_matches('$')) {
            Some(Bound::Op(id)) => Ok(*id),
            Some(Bound::Submit(Ok(id))) => Ok(*id),
            _ => Err(format!("{name} is not a bound operation")),
        },
        other => Err(format!("bad `{field}`: {other}")),
    }
}

pub fn action(world: &World, action: &str, step: &Value) -> Check {
    let coordinator = &world.ops.coordinator;
    match action {
        "submit" => {
            let request = request(step)?;
            let id = request.id;
            let caps = snapshot(world, &step["capabilities"])?;
            let result = if step["confirmed_delete"].as_bool() == Some(true) {
                coordinator.submit_confirmed_delete(request, caps)
            } else if let Some(name) = step["decision"].as_str() {
                coordinator.submit_with_decision(request, caps, decision(name)?)
            } else {
                coordinator.submit(request, caps)
            };
            bind(world, step, Bound::Op(id));
            if let Some(name) = step["as_result"].as_str() {
                world.bound.borrow_mut().insert(name.into(), Bound::Submit(result.map_err(|e| e.kind)));
            }
            Ok(())
        }
        "retry_elevated" => {
            let original_id = resolve_id(world, step, "ref")?;
            let original = coordinator
                .records()
                .into_iter()
                .find(|r| r.id == original_id)
                .ok_or("no such operation")?;
            let new_id = OperationId(step["id"].as_u64().ok_or("retry_elevated needs `id`")?);
            let result = elevated_retry_request(&original, new_id).and_then(|retry| {
                coordinator.submit_linked_retry(retry, snapshot(world, &step["capabilities"]).unwrap(), original.id)
            });
            bind(world, step, Bound::Op(new_id));
            if let Some(name) = step["as_result"].as_str() {
                world.bound.borrow_mut().insert(name.into(), Bound::Submit(result.map_err(|e| e.kind)));
            }
            Ok(())
        }
        "cancel" => {
            let id = resolve_id(world, step, "ref")?;
            let result = coordinator.cancel(id);
            if let Some(name) = step["as_result"].as_str() {
                world.bound.borrow_mut().insert(name.into(), Bound::Cancel(result));
            }
            Ok(())
        }
        other => Err(format!("operations has no action {other:?}")),
    }
}

fn index(step: &Value, count: usize) -> Result<usize, String> {
    match &step["request"] {
        Value::String(s) if s == "latest" => count.checked_sub(1).ok_or_else(|| "no request received yet".to_string()),
        Value::Number(n) => {
            let n = n.as_u64().ok_or("bad request index")? as usize;
            if n == 0 || n > count {
                Err(format!("request {n} was never received ({count} so far)"))
            } else {
                Ok(n - 1)
            }
        }
        other => Err(format!("bad `request`: {other}")),
    }
}

fn completion(value: &Value) -> Result<OperationCompletion, String> {
    let effect = match value["effect"].as_str().unwrap_or("unchanged") {
        "unchanged" => MutationEffect::Unchanged,
        "changed" => MutationEffect::Changed,
        "partially_changed" => MutationEffect::PartiallyChanged,
        "unknown" => MutationEffect::Unknown,
        other => return Err(format!("unknown effect {other:?}")),
    };
    Ok(OperationCompletion { effect, completed: locations(&value["completed"])?, remaining: locations(&value["remaining"])? })
}

pub fn script_port(world: &World, step: &Value) -> Check {
    let scripted = &world.ops.scripted;
    match step["port"].as_str() {
        Some("capabilities") => {
            let held = scripted.inspections.borrow();
            let entry = &held[index(step, held.len())?];
            let done = entry.done.borrow_mut().take().ok_or("that inspection was already answered")?;
            let id = step["as_id"].as_u64().map(RequestId).unwrap_or(entry.request.id);
            let respond = &step["respond"];
            let outcome = if respond == "cancelled" {
                RequestOutcome::Cancelled
            } else if let Some(failed) = respond.get("failed") {
                RequestOutcome::Failed(OperationError::new(
                    error_kind(failed["kind"].as_str().ok_or("failed needs `kind`")?)?,
                    Some(&entry.request.location),
                    None,
                ))
            } else if let Some(ok) = respond.get("succeeded") {
                RequestOutcome::Succeeded(snapshot(world, ok)?)
            } else {
                return Err(format!("unknown `respond`: {respond}"));
            };
            drop(held);
            done(RequestCompletion { id, outcome });
            Ok(())
        }
        Some("operations") => {
            let (sink, id, first) = {
                let held = scripted.operations.borrow();
                let entry = &held[index(step, held.len())?];
                let id = step["as_id"].as_u64().map(OperationId).unwrap_or(entry.request.id);
                (entry.sink.clone(), id, entry.next_sequence.get())
            };
            let mut sequence = first;
            for event in step["emit"].as_array().ok_or("operations port needs `emit`")? {
                let built = match event {
                    Value::String(s) if s == "started" => OperationEvent::Started { id, sequence },
                    Value::String(s) if s == "cancel_requested" => OperationEvent::CancelRequested { id, sequence },
                    Value::Object(map) => {
                        let (name, body) = map.iter().next().ok_or("empty event")?;
                        match name.as_str() {
                            "progress" => OperationEvent::Progress {
                                id,
                                sequence,
                                progress: OperationProgress {
                                    completed_items: body[0].as_u64().unwrap_or(0),
                                    total_items: body[1].as_u64(),
                                    completed_bytes: None,
                                    total_bytes: None,
                                },
                            },
                            "succeeded" => OperationEvent::Succeeded { id, sequence, completion: completion(body)? },
                            "failed" => OperationEvent::Failed {
                                id,
                                sequence,
                                error: OperationError::new(error_kind(body["kind"].as_str().ok_or("failed needs `kind`")?)?, None, None),
                                completion: completion(body)?,
                            },
                            "cancelled" => OperationEvent::Cancelled {
                                id,
                                sequence,
                                outcome: CancellationOutcome { completion: completion(body)? },
                            },
                            other => return Err(format!("unknown event {other:?}")),
                        }
                    }
                    other => return Err(format!("unknown event {other}")),
                };
                (sink.borrow_mut())(built);
                sequence += 1;
            }
            let held = scripted.operations.borrow();
            let entry = &held[index(step, held.len())?];
            entry.next_sequence.set(sequence);
            Ok(())
        }
        other => Err(format!("port {other:?} is not an operations port")),
    }
}

pub fn expect_port_requests(world: &World, step: &Value) -> Check {
    let scripted = &world.ops.scripted;
    let got = match step["port"].as_str() {
        Some("capabilities") => scripted.inspections.borrow().len(),
        _ => scripted.operations.borrow().len(),
    };
    let want = step["count"].as_u64().ok_or("port_requests needs `count`")? as usize;
    if got != want {
        return Err(format!("{} port received {got} request(s), expected {want}", step["port"]));
    }
    if let Some(id) = step["last"]["id"].as_u64() {
        let held = scripted.operations.borrow();
        let last = held.last().ok_or("no operation dispatched")?;
        eq("last dispatched operation id", last.request.id.0, id)?;
    }
    Ok(())
}

fn state_name(state: &OperationState) -> &'static str {
    match state {
        OperationState::Queued => "queued",
        OperationState::Running => "running",
        OperationState::CancelRequested => "cancel_requested",
        OperationState::Succeeded => "succeeded",
        OperationState::Failed(_) => "failed",
        OperationState::Cancelled(_) => "cancelled",
    }
}

fn change_name(change: ChangeExtent) -> &'static str {
    match change {
        ChangeExtent::Unchanged => "unchanged",
        ChangeExtent::Complete => "complete",
        ChangeExtent::Partial => "partial",
        ChangeExtent::Unknown => "unknown",
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

fn uris(list: &[Location]) -> Vec<String> {
    list.iter().map(|l| l.uri().to_string()).collect()
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

fn record(world: &World, step: &Value) -> Result<OperationProjection, String> {
    let id = resolve_id(world, step, "ref")?;
    world.ops.coordinator.records().into_iter().find(|r| r.id == id).ok_or_else(|| format!("no record for {id:?}"))
}

fn eq<T: PartialEq + std::fmt::Debug>(field: &str, got: T, want: T) -> Check {
    if got == want { Ok(()) } else { Err(format!("{field} is {got:?}, expected {want:?}")) }
}

pub fn expectation(world: &World, step: &Value) -> Check {
    match step["expect"].as_str() {
        Some("record") => {
            let record = record(world, step)?;
            if let Some(want) = step["state"].as_str() {
                eq("state", state_name(&record.state), want)?;
            }
            if let Some(want) = step["error"].as_str() {
                let got = match &record.state {
                    OperationState::Failed(error) => Some(format!("{:?}", error.kind)),
                    _ => None,
                };
                eq("error", got, Some(format!("{:?}", error_kind(want)?)))?;
            }
            if let Some(want) = step["change"].as_str() {
                eq("change", change_name(record.change), want)?;
            }
            if step.get("completed").is_some() {
                eq("completed", uris(&record.completion.completed), strings(&step["completed"]))?;
            }
            if step.get("remaining").is_some() {
                eq("remaining", uris(&record.completion.remaining), strings(&step["remaining"]))?;
            }
            if step.get("events").is_some() {
                let got: Vec<String> = record.events.iter().map(|e| event_name(e).to_string()).collect();
                eq("events", got, strings(&step["events"]))?;
            }
            for absent in strings(&step["lacks_events"]) {
                if record.events.iter().any(|e| event_name(e) == absent) {
                    return Err(format!("record has a {absent} event"));
                }
            }
            if let Some(count) = step["events_count"].as_u64() {
                eq("event count", record.events.len() as u64, count)?;
            }
            if let Some(count) = step["terminal_events"].as_u64() {
                eq("terminal events", record.events.iter().filter(|e| e.is_terminal()).count() as u64, count)?;
            }
            if step.get("retry_of").is_some() {
                let got = record.retry_of.map(|id| id.0);
                let want = match &step["retry_of"] {
                    Value::Null => None,
                    Value::String(_) => Some(resolve_id(world, step, "retry_of")?.0),
                    other => other.as_u64(),
                };
                eq("retry_of", got, want)?;
            }
            Ok(())
        }
        Some("lock_keys") => {
            if let Some(active) = step.get("active") {
                eq("active lock keys", world.ops.coordinator.active_lock_keys(), strings(active))?;
            }
            if let Some(count) = step["active_count_is"].as_str() {
                let n = world.ops.coordinator.active_lock_keys().len();
                match count {
                    "zero" => eq("active lock keys", n, 0)?,
                    "non_zero" if n > 0 => {}
                    other => return Err(format!("{n} active lock keys do not satisfy {other:?}")),
                }
            }
            if let Some(want) = step.get("for") {
                let caps = snapshot(world, &step["capabilities"])?;
                let mut req = request(want)?;
                req.id = OperationId(0);
                let got = lock_keys(&req, &caps).map_err(|e| format!("lock_keys failed: {:?}", e.kind))?;
                eq("lock keys", got, strings(&step["is"]))?;
            }
            Ok(())
        }
        Some("submit_result") => {
            let name = step["ref"].as_str().ok_or("needs `ref`")?.trim_start_matches('$');
            let bound = world.bound.borrow();
            let Some(Bound::Submit(result)) = bound.get(name) else { return Err(format!("{name} is not a submit result")) };
            match (step["is"].as_str(), result) {
                (Some("accepted"), Ok(_)) => Ok(()),
                (Some("refused"), Err(got)) => match step["error"].as_str() {
                    Some(want) => eq("refusal", format!("{got:?}"), format!("{:?}", error_kind(want)?)),
                    None => Ok(()),
                },
                (want, got) => Err(format!("submit result is {got:?}, expected {want:?}")),
            }
        }
        Some("cancel_result") => {
            let name = step["ref"].as_str().ok_or("needs `ref`")?.trim_start_matches('$');
            let bound = world.bound.borrow();
            let Some(Bound::Cancel(result)) = bound.get(name) else { return Err(format!("{name} is not a cancel result")) };
            let got = match result {
                CancelResult::Requested => "requested",
                CancelResult::CancelledBeforeStart => "cancelled_before_start",
                CancelResult::AlreadyTerminal => "already_terminal",
                CancelResult::NotFound => "not_found",
            };
            eq("cancel result", got, step["is"].as_str().unwrap_or(""))
        }
        Some("reducer") => reducer_expectation(world, step),
        other => Err(format!("expectation {other:?} is not implemented for operations")),
    }
}

// The event reducer, driven directly: `apply` an event and assert whether it was accepted.

thread_local! {
    static REDUCER: RefCell<Option<OperationReducer>> = const { RefCell::new(None) };
}

pub fn reducer_action(_world: &World, action: &str, step: &Value) -> Check {
    match action {
        "start" => {
            let id = OperationId(step["id"].as_u64().ok_or("start needs `id`")?);
            REDUCER.with(|r| *r.borrow_mut() = Some(OperationReducer::new(id)));
            Ok(())
        }
        "apply" => {
            let event = &step["event"];
            let id = OperationId(event["id"].as_u64().ok_or("event needs `id`")?);
            let sequence = event["sequence"].as_u64().ok_or("event needs `sequence`")?;
            let built = match event["kind"].as_str() {
                Some("started") => OperationEvent::Started { id, sequence },
                Some("progress") => OperationEvent::Progress { id, sequence, progress: OperationProgress::default() },
                Some("cancel_requested") => OperationEvent::CancelRequested { id, sequence },
                Some("succeeded") => OperationEvent::Succeeded { id, sequence, completion: OperationCompletion::default() },
                other => return Err(format!("unknown reducer event {other:?}")),
            };
            let accepted = REDUCER
                .with(|r| r.borrow_mut().as_mut().map(|reducer| reducer.apply(built)))
                .ok_or("reducer not started")?;
            if let Some(want) = step["accepted"].as_bool() {
                eq("accepted", accepted, want)?;
            }
            Ok(())
        }
        other => Err(format!("reducer has no action {other:?}")),
    }
}

fn reducer_expectation(_world: &World, step: &Value) -> Check {
    REDUCER.with(|r| {
        let borrowed = r.borrow();
        let reducer = borrowed.as_ref().ok_or("reducer not started")?;
        if let Some(count) = step["events"].as_u64() {
            eq("reducer events", reducer.events().len() as u64, count)?;
        }
        if let Some(terminal) = step["terminal"].as_bool() {
            eq("reducer terminal", reducer.is_terminal(), terminal)?;
        }
        Ok(())
    })
}
