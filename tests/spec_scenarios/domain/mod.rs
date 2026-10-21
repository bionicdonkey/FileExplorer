//! `kind: "domain"` scenarios: the domain state machines over a scripted port.
//!
//! The fake `DirectoryRepository` records each request and answers only when a `port` step
//! says so. Locations in domain scenarios are URIs, taken as written: there is no sandbox.
//!
//! Subjects: `navigation` is implemented. Every `do` step and every `view`/`record`/
//! `clipboard` expectation carries a `subject`, so one scenario can drive several.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::Value;
use zorin_explorer::application::navigation::NavigationCoordinator;
use zorin_explorer::core::contracts::*;
use zorin_explorer::core::navigation::{NavigationStatus, TabState};
use zorin_explorer::core::ports::*;
use zorin_explorer::core::{Location, ResolvedLocation};

use std::collections::HashMap;

use crate::{effective_steps, Check, Mode};

mod ops;

type SharedSink = Rc<RefCell<DirectoryPageSink>>;

/// Records directory requests; a request is answered only when a step says so.
#[derive(Default)]
struct ScriptedDirectory {
    held: RefCell<Vec<(DirectoryRequest, SharedSink)>>,
}

impl DirectoryRepository for ScriptedDirectory {
    fn list(&self, request: DirectoryRequest, done: DirectoryCallback) {
        let done = RefCell::new(Some(done));
        let sink: DirectoryPageSink = Box::new(move |result| {
            if let Some(done) = done.borrow_mut().take() {
                done(result);
            }
        });
        self.held.borrow_mut().push((request, Rc::new(RefCell::new(sink))));
    }
    fn list_pages(&self, request: DirectoryRequest, sink: DirectoryPageSink) {
        self.held.borrow_mut().push((request, Rc::new(RefCell::new(sink))));
    }
}

pub fn location(uri: &str) -> Result<Location, String> {
    Location::from_uri(uri).map_err(|e| format!("bad location {uri:?}: {e:?}"))
}

pub fn error_kind(name: &str) -> Result<OperationErrorKind, String> {
    use OperationErrorKind::*;
    Ok(match name {
        "permission_denied" => PermissionDenied,
        "authentication_required" => AuthenticationRequired,
        "mount_unavailable" => MountUnavailable,
        "conflict" => Conflict,
        "not_found" => NotFound,
        "read_only" => ReadOnly,
        "unsupported" => Unsupported,
        "capability_changed" => CapabilityChanged,
        "partial_completion" => PartialCompletion,
        "invalid_request" => InvalidRequest,
        "unknown" => Unknown,
        other => return Err(format!("unknown error kind {other:?}")),
    })
}

fn status_name(status: &NavigationStatus) -> &'static str {
    match status {
        NavigationStatus::Idle => "idle",
        NavigationStatus::Loading(_) => "loading",
        NavigationStatus::Ready => "ready",
        NavigationStatus::Failed { .. } => "failed",
        NavigationStatus::Closed => "closed",
    }
}

/// What a step's `as` name stands for.
pub enum Bound {
    Op(OperationId),
    Submit(Result<OperationId, OperationErrorKind>),
    Cancel(zorin_explorer::application::operations::CancelResult),
}

pub struct World {
    directory: Rc<ScriptedDirectory>,
    tab: Rc<RefCell<TabState>>,
    navigation: NavigationCoordinator,
    ops: ops::Ops,
    pub bound: RefCell<HashMap<String, Bound>>,
    pub default_capabilities: Value,
}

pub fn run(scenario: &Value, mode: Mode) -> Check {
    if mode == Mode::Excluded {
        return Err("excluded domain scenarios are not implemented by this runner yet".into());
    }
    let start = location(scenario["given"]["location"].as_str().unwrap_or("file:///"))?;
    let directory = Rc::new(ScriptedDirectory::default());
    let tab = Rc::new(RefCell::new(TabState::new(start)));
    let world = World {
        navigation: NavigationCoordinator::new(tab.clone(), directory.clone()),
        directory,
        tab,
        ops: ops::Ops::new(),
        bound: RefCell::new(HashMap::new()),
        default_capabilities: scenario["given"]["capabilities"].clone(),
    };
    for (index, step) in effective_steps(scenario).iter().enumerate() {
        step_once(&world, step).map_err(|e| format!("step {index}: {e}"))?;
    }
    Ok(())
}

fn step_once(world: &World, step: &Value) -> Check {
    if let Some(action) = step["do"].as_str() {
        let subject = step["subject"].as_str().ok_or("`do` needs a `subject`")?;
        return match subject {
            "navigation" => navigation_action(world, action, step),
            "operations" => ops::action(world, action, step),
            "reducer" => ops::reducer_action(world, action, step),
            other => Err(format!("subject {other:?} is not implemented by the domain runner yet")),
        };
    }
    if step["port"].is_string() && step["expect"].is_null() {
        return match step["port"].as_str() {
            Some("capabilities") | Some("operations") => ops::script_port(world, step),
            _ => script_port(world, step),
        };
    }
    match step["expect"].as_str() {
        Some("record") | Some("lock_keys") | Some("submit_result") | Some("cancel_result") | Some("reducer") => {
            ops::expectation(world, step)
        }
        Some("port_requests") if matches!(step["port"].as_str(), Some("capabilities") | Some("operations")) => {
            ops::expect_port_requests(world, step)
        }
        Some("port_requests") => expect_port_requests(world, step),
        Some("request_cancelled") => expect_cancelled(world, step),
        Some("view") => expect_view(world, step),
        Some(other) => Err(format!("expectation {other:?} is not implemented by the domain runner yet")),
        None => Err(format!("step has neither `do`, `port` nor `expect`: {step}")),
    }
}

fn navigation_action(world: &World, action: &str, step: &Value) -> Check {
    let started = match action {
        "navigate" => world.navigation.navigate(location(step["to"].as_str().ok_or("navigate needs `to`")?)?, |_| {}),
        "back" => world.navigation.back(|_| {}),
        "forward" => world.navigation.forward(|_| {}),
        "refresh" => world.navigation.refresh(|_| {}),
        "retry" => world.navigation.retry(|_| {}),
        "close" => {
            world.navigation.close_tab();
            true
        }
        other => return Err(format!("navigation has no action {other:?}")),
    };
    // Whether the action started a request is observable through `port_requests`; it is
    // not asserted here so that a refused action (for example after close) stays legal.
    let _ = started;
    Ok(())
}

/// 1-based index into the requests received, or "latest".
fn request_index(world: &World, step: &Value) -> Result<usize, String> {
    let count = world.directory.held.borrow().len();
    match &step["request"] {
        Value::String(s) if s == "latest" => count.checked_sub(1).ok_or_else(|| "no request received yet".to_string()),
        Value::Number(n) => {
            let n = n.as_u64().ok_or("request must be a positive integer")? as usize;
            if n == 0 || n > count {
                return Err(format!("request {n} was never received ({count} so far)"));
            }
            Ok(n - 1)
        }
        other => Err(format!("bad `request`: {other}")),
    }
}

fn script_port(world: &World, step: &Value) -> Check {
    match step["port"].as_str() {
        Some("directory") => {}
        other => return Err(format!("port {other:?} is not implemented by the domain runner yet")),
    }
    let index = request_index(world, step)?;
    let (request, sink) = {
        let held = world.directory.held.borrow();
        (held[index].0.clone(), held[index].1.clone())
    };
    let respond = &step["respond"];
    let outcome = if respond == "cancelled" {
        RequestOutcome::Cancelled
    } else if let Some(failed) = respond.get("failed") {
        let kind = error_kind(failed["kind"].as_str().ok_or("failed needs `kind`")?)?;
        RequestOutcome::Failed(OperationError::new(kind, Some(&request.location), None))
    } else if let Some(done) = respond.get("succeeded") {
        let resolved = match done["resolved"].as_str() {
            Some(uri) => location(uri)?,
            None => request.location.clone(),
        };
        let entries = done["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|uri| {
                let entry = location(uri)?;
                Ok(EntrySnapshot {
                    display_name: entry.display_name(),
                    location: entry,
                    kind: EntryKind::File,
                    size: None,
                    modified: None,
                    content_type: None,
                    capabilities: CapabilitySnapshot::default(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        RequestOutcome::Succeeded(DirectorySnapshot {
            location: ResolvedLocation { requested: request.location.clone(), resolved },
            entries,
            capabilities: CapabilitySnapshot::default(),
            next_page: None,
        })
    } else {
        return Err(format!("unknown `respond`: {respond}"));
    };
    (sink.borrow_mut())(RequestCompletion { id: request.id, outcome });
    Ok(())
}

fn expect_port_requests(world: &World, step: &Value) -> Check {
    if step["port"].as_str() != Some("directory") {
        return Err("only the directory port is implemented".into());
    }
    let held = world.directory.held.borrow();
    if let Some(count) = step["count"].as_u64() {
        if held.len() as u64 != count {
            return Err(format!("directory port received {} request(s), expected {count}", held.len()));
        }
    }
    if let Some(path) = step["last"]["path"].as_str() {
        let last = held.last().ok_or("no request received")?;
        if last.0.location.uri() != path {
            return Err(format!("last request was for {}, expected {path}", last.0.location.uri()));
        }
    }
    Ok(())
}

fn expect_cancelled(world: &World, step: &Value) -> Check {
    let index = request_index(world, step)?;
    let cancelled = {
        use gio::prelude::CancellableExt;
        world.directory.held.borrow()[index].0.cancellable.is_cancelled()
    };
    if cancelled { Ok(()) } else { Err(format!("request {} was not cancelled", index + 1)) }
}

/// A deliberately small projection: location, back and forward targets, status, error kind.
fn expect_view(world: &World, step: &Value) -> Check {
    if step["subject"].as_str() != Some("navigation") {
        return Err("only the navigation view is implemented".into());
    }
    let tab = world.tab.borrow();
    let mismatch = |field: &str, got: String, want: &Value| format!("{field} is {got}, expected {want}");
    if let Some(want) = step.get("location") {
        let got = tab.location.uri().to_string();
        if want.as_str() != Some(got.as_str()) {
            return Err(mismatch("location", got, want));
        }
    }
    if let Some(want) = step.get("status") {
        let got = status_name(&tab.status);
        if want.as_str() != Some(got) {
            return Err(mismatch("status", got.into(), want));
        }
    }
    for (field, got) in [
        ("back", tab.history.back_target().map(|l| l.uri().to_string())),
        ("forward", tab.history.forward_target().map(|l| l.uri().to_string())),
    ] {
        if let Some(want) = step.get(field) {
            let matches = match (want, &got) {
                (Value::Null, None) => true,
                (Value::String(w), Some(g)) => w == g,
                _ => false,
            };
            if !matches {
                return Err(mismatch(field, format!("{got:?}"), want));
            }
        }
    }
    if let Some(want) = step.get("error") {
        let got = match &tab.status {
            NavigationStatus::Failed { error, .. } => Some(format!("{:?}", error.kind)),
            _ => None,
        };
        let want_kind = error_kind(want.as_str().ok_or("`error` must be a kind name")?)?;
        if got != Some(format!("{want_kind:?}")) {
            return Err(mismatch("error", format!("{got:?}"), want));
        }
    }
    Ok(())
}
