//! `kind: "adapter"` scenarios: the real adapters over a real temporary filesystem.
//!
//! `operate` runs `gio_operations::execute` / `execute_with_decision`, the same entry the
//! GIO adapter drives, which is synchronous and delivers its events to a closure. Its
//! `capability_version` is the adapter's current one (`local_capabilities().version`)
//! unless the step names one.
//!
//! `list`, `list_pages`, `inspect` and `search` go through the production adapters on a private
//! GLib main context. Each runs to completion inside its own step; a step may name when to
//! cancel (`"cancel": "immediately"` or `"after_first_page"`). The runner also asserts the port
//! rule that no callback runs inline.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::Value;
use gio::prelude::CancellableExt;
use lorin_explorer::core::contracts::*;
use lorin_explorer::core::ports::{DirectoryRepository, FileOperations, SearchRepository};
use lorin_explorer::application::compression::{compress_blocking, ArchiveFormat};
use lorin_explorer::application::extraction::extract;
use lorin_explorer::core::operation_manager::OperationKind;
use lorin_explorer::core::{Location, OperationManager, OperationState};
use lorin_explorer::infrastructure::{
    archive_operations::ArchiveOperations, directory_router::DirectoryRouter, gio_executor::GioExecutor,
    gio_operations, gio_operations::GioOperations, tracker_search::TrackerSearchRepository,
};

use crate::sandbox::Sandbox;
use crate::{effective_steps, Check, Mode};

/// The recorded run of one `operate` step.
struct Run {
    events: Vec<OperationEvent>,
}

/// What a step's `as` name stands for.
enum Bound {
    Run(Run),
    /// Every completion a listing delivered, in order.
    Pages(Vec<RequestOutcome<DirectorySnapshot>>),
    Search(SearchOutcome),
    Inspect(RequestOutcome<CapabilitySnapshot>),
    /// An operation manager that persisted its history to a file of its own.
    History(History),
    /// The advisory metadata delivered for visible rows.
    Visible(RequestOutcome<Vec<VisibleMetadata>>),
    /// Extraction: `Ok` or the reason it gave.
    Extract(Result<(), String>),
    /// Compression: the archive's path relative to the sandbox, or the reason it gave.
    Compress(Result<String, String>),
}

struct History {
    file: std::path::PathBuf,
    ids: Vec<u64>,
    states: Vec<String>,
    _dir: tempfile::TempDir,
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
    if paths(&scenario["given"]["requires"]).contains(&"unprivileged") && sandbox.is_privileged()? {
        // A process that can read mode-000 files cannot exercise permission failures.
        eprintln!("    note: permission scenario not exercised by a privileged process");
        return Ok(());
    }
    sandbox.build(&scenario["given"]["tree"])?;
    let mut runs: HashMap<String, Bound> = HashMap::new();
    // A private context for the asynchronous adapters; acquired for the whole scenario.
    let context = glib::MainContext::new();
    let _guard = context.acquire().map_err(|e| e.to_string())?;
    let mut next_id = 1u64;

    for (index, step) in effective_steps(scenario).iter().enumerate() {
        let at = |e: String| format!("step {index}: {e}");
        if let Some(action) = step["do"].as_str() {
            match action {
                "operate" => {
                    let run = operate(&context, &sandbox, step, next_id).map_err(at)?;
                    next_id += 1;
                    if mode == Mode::Excluded {
                        return expect_unsupported(&run).map_err(at);
                    }
                    runs.insert(step["as"].as_str().unwrap_or("_").to_string(), Bound::Run(run));
                }
                "list" | "list_pages" => {
                    let pages = list(&context, &sandbox, step, action == "list_pages").map_err(at)?;
                    if mode == Mode::Excluded {
                        return expect_unsupported_pages(&pages).map_err(at);
                    }
                    runs.insert(step["as"].as_str().unwrap_or("_").to_string(), Bound::Pages(pages));
                }
                "search" => {
                    let outcome = search(&context, &sandbox, step).map_err(at)?;
                    if mode == Mode::Excluded {
                        return match outcome {
                            SearchOutcome::Unsupported { .. } => Ok(()),
                            _ => Err(at("excluded feature did not report unsupported".into())),
                        };
                    }
                    runs.insert(step["as"].as_str().unwrap_or("_").to_string(), Bound::Search(outcome));
                }
                "inspect_visible" => {
                    let outcome = inspect_visible(&context, &sandbox, step).map_err(at)?;
                    runs.insert(step["as"].as_str().unwrap_or("_").to_string(), Bound::Visible(outcome));
                }
                "extract" => {
                    let result = extract_archive(&sandbox, step).map_err(at)?;
                    runs.insert(step["as"].as_str().unwrap_or("_").to_string(), Bound::Extract(result));
                }
                "compress" => {
                    let result = compress_sources(&sandbox, step).map_err(at)?;
                    runs.insert(step["as"].as_str().unwrap_or("_").to_string(), Bound::Compress(result));
                }
                "manage" => {
                    let history = manage(&sandbox, step).map_err(at)?;
                    runs.insert(step["as"].as_str().unwrap_or("_").to_string(), Bound::History(history));
                }
                "inspect" => {
                    let outcome = inspect(&context, &sandbox, step).map_err(at)?;
                    runs.insert(step["as"].as_str().unwrap_or("_").to_string(), Bound::Inspect(outcome));
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

fn operate(context: &glib::MainContext, sandbox: &Sandbox, step: &Value, id: u64) -> Result<Run, String> {
    let kind = match step["kind"].as_str().ok_or("operate needs `kind`")? {
        "copy" => MutationKind::Copy,
        "move" => MutationKind::Move,
        "create" => MutationKind::Create,
        "rename" => MutationKind::Rename,
        "trash" => MutationKind::Trash,
        "delete" => MutationKind::Delete,
        other => return Err(format!("unknown operation kind {other:?}")),
    };
    let location = |relative: &str| locate(sandbox, relative);
    let request = OperationRequest {
        id: OperationId(id),
        kind,
        sources: paths(&step["sources"]).into_iter().map(location).collect::<Result<_, _>>()?,
        destination: step["destination"].as_str().map(location).transpose()?,
        explicit_target: step["target"].as_str().map(location).transpose()?,
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
    let events = if request.sources.iter().any(Location::is_inside_archive) {
        // Entries inside an archive are taken out by the archive adapter, asynchronously.
        let adapter = ArchiveOperations::new(Rc::new(GioOperations::new(context.clone())), context.clone(), GioExecutor::shared());
        let received: Rc<RefCell<Vec<OperationEvent>>> = Rc::default();
        let out = received.clone();
        let sink: lorin_explorer::core::ports::OperationSink = Box::new(move |event| out.borrow_mut().push(event));
        match decision {
            Some(decision) => adapter.submit_with_decision(request, decision, sink),
            None => adapter.submit(request, sink),
        }
        if !received.borrow().is_empty() {
            return Err("an event ran inline before the adapter returned".into());
        }
        pump(context, || received.borrow().last().is_some_and(OperationEvent::is_terminal))?;
        let collected = received.borrow().clone();
        collected
    } else {
        let mut events = Vec::new();
        let mut sink = |event| events.push(event);
        match decision {
            Some(decision) => gio_operations::execute_with_decision(request, decision, &mut sink),
            None => gio_operations::execute(request, &mut sink),
        }
        events
    };
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

/// A sandbox-relative path, or a URI when the text names a scheme.
fn locate(sandbox: &Sandbox, text: &str) -> Result<Location, String> {
    // `in-archive:ARCHIVE!/INNER` names an entry inside a local archive; INNER is URI-escaped.
    if let Some(rest) = text.strip_prefix("in-archive:") {
        let (archive, inner) = rest.split_once("!/").ok_or("in-archive: needs `ARCHIVE!/INNER`")?;
        let root = Location::from_local_path(&sandbox.path(archive)).archive_contents().map_err(|e| format!("{archive}: {e:?}"))?;
        return Location::from_uri(&format!("{}{inner}", root.uri())).map_err(|e| format!("bad archive entry {inner:?}: {e:?}"));
    }
    if text.contains("://") {
        Location::from_uri(text).map_err(|e| format!("bad location {text:?}: {e:?}"))
    } else {
        Ok(Location::from_local_path(&sandbox.path(text)))
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

/// Pumps the context until `done` holds. Nothing here sleeps for a fixed time: the loop ends
/// when the result arrives, and fails if it has not within five seconds.
fn pump(context: &glib::MainContext, mut done: impl FnMut() -> bool) -> Check {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        context.iteration(false);
        if Instant::now() > deadline {
            return Err("no terminal result within five seconds".into());
        }
        std::thread::yield_now();
    }
    Ok(())
}

fn list(context: &glib::MainContext, sandbox: &Sandbox, step: &Value, paged: bool) -> Result<Vec<RequestOutcome<DirectorySnapshot>>, String> {
    let router = DirectoryRouter::new(context.clone());
    let cancellable = gio::Cancellable::new();
    let cancel = step["cancel"].as_str();
    if cancel == Some("immediately") {
        cancellable.cancel();
    }
    let request = DirectoryRequest {
        id: RequestId(87),
        location: locate(sandbox, step["path"].as_str().ok_or("list needs `path`")?)?,
        page: PageRequest {
            offset: step["offset"].as_u64().unwrap_or(0) as usize,
            limit: step["limit"].as_u64().map_or(usize::MAX, |l| l as usize),
        },
        cancellable: cancellable.clone(),
    };
    let pages: Rc<RefCell<Vec<RequestOutcome<DirectorySnapshot>>>> = Rc::default();
    let sink_pages = pages.clone();
    let after_first_page = cancel == Some("after_first_page");
    let record = move |completion: RequestCompletion<DirectorySnapshot>| {
        assert_eq!(completion.id, RequestId(87), "a completion carries its request's id");
        sink_pages.borrow_mut().push(completion.outcome);
        if after_first_page {
            cancellable.cancel();
        }
    };
    let finished = |pages: &[RequestOutcome<DirectorySnapshot>]| match pages.last() {
        Some(RequestOutcome::Succeeded(snapshot)) => snapshot.next_page.is_none(),
        Some(_) => true,
        None => false,
    };
    if paged {
        router.list_pages(request, Box::new(record));
    } else {
        router.list(request, Box::new(record));
    }
    if !pages.borrow().is_empty() {
        return Err("a callback ran inline before the adapter returned".into());
    }
    pump(context, || finished(&pages.borrow()))?;
    router.drain_owner_requests();
    let collected = pages.borrow().clone();
    Ok(collected)
}

fn search(context: &glib::MainContext, sandbox: &Sandbox, step: &Value) -> Result<SearchOutcome, String> {
    let adapter = TrackerSearchRepository::new(context.clone());
    let mut scope = SearchScope::local_fallback(locate(sandbox, step["root"].as_str().unwrap_or("."))?);
    if let Some(depth) = step["max_depth"].as_u64() {
        scope.max_depth = depth as u32;
    }
    if let Some(results) = step["max_results"].as_u64() {
        scope.max_results = results as usize;
    }
    if let Some(ms) = step["timeout_ms"].as_u64() {
        scope.timeout = Duration::from_millis(ms);
    }
    let cancellable = gio::Cancellable::new();
    if step["cancel"].as_str() == Some("immediately") {
        cancellable.cancel();
    }
    let received: Rc<RefCell<Option<SearchOutcome>>> = Rc::default();
    let out = received.clone();
    adapter.search(
        SearchRequest {
            id: RequestId(123),
            scope,
            query: step["query"].as_str().ok_or("search needs `query`")?.into(),
            mode: match step["mode"].as_str().unwrap_or("filename") {
                "filename" => SearchMode::Filename,
                "content" => SearchMode::Content,
                other => return Err(format!("unknown search mode {other:?}")),
            },
            cancellable,
        },
        Box::new(move |completion| *out.borrow_mut() = Some(completion.outcome)),
    );
    if received.borrow().is_some() {
        return Err("a callback ran inline before the adapter returned".into());
    }
    pump(context, || received.borrow().is_some())?;
    let outcome = received.borrow_mut().take().unwrap();
    Ok(outcome)
}

fn inspect(context: &glib::MainContext, sandbox: &Sandbox, step: &Value) -> Result<RequestOutcome<CapabilitySnapshot>, String> {
    let adapter = GioOperations::new(context.clone());
    let received: Rc<RefCell<Option<RequestOutcome<CapabilitySnapshot>>>> = Rc::default();
    let out = received.clone();
    adapter.inspect_capabilities(
        CapabilityRequest {
            id: RequestId(1),
            location: locate(sandbox, step["path"].as_str().ok_or("inspect needs `path`")?)?,
            cancellable: gio::Cancellable::new(),
        },
        Box::new(move |completion| *out.borrow_mut() = Some(completion.outcome)),
    );
    if received.borrow().is_some() {
        return Err("a callback ran inline before the adapter returned".into());
    }
    pump(context, || received.borrow().is_some())?;
    let outcome = received.borrow_mut().take().unwrap();
    adapter.drain_owner_requests();
    Ok(outcome)
}

/// Runs operations through the operation manager with history persisted to a file, and waits
/// for each to finish. The file is outside the sandbox, so it never shows up in a tree check.
fn manage(sandbox: &Sandbox, step: &Value) -> Result<History, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let file = dir.path().join("history.json");
    let manager = OperationManager::load_history(Some(file.clone()), 7);
    let mut ids = Vec::new();
    let mut states = Vec::new();
    for operation in step["operations"].as_array().ok_or("manage needs `operations`")? {
        let kind = match operation["kind"].as_str().unwrap_or("") {
            "copy" => OperationKind::Copy,
            "move" => OperationKind::Move,
            "delete" => OperationKind::Delete,
            "trash" => OperationKind::Trash,
            other => return Err(format!("unknown operation kind {other:?}")),
        };
        let sources = paths(&operation["sources"]).into_iter().map(|p| sandbox.path(p)).collect();
        let id = manager.submit(kind, sources, operation["destination"].as_str().map(|d| sandbox.path(d)));
        let record = {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(r) = manager.records().into_iter().find(|r| r.id == id && r.finished_at.is_some()) {
                    break r;
                }
                if Instant::now() > deadline {
                    return Err(format!("operation {id} did not finish in time"));
                }
                std::thread::yield_now();
            }
        };
        states.push(match record.state {
            OperationState::Completed => "completed".to_string(),
            OperationState::Failed(_) => "failed".to_string(),
            OperationState::Cancelled => "cancelled".to_string(),
            other => format!("{other:?}").to_lowercase(),
        });
        ids.push(id);
    }
    // The manager saves history itself once each job ends; wait for the file to hold them all.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let saved = std::fs::read_to_string(&file).unwrap_or_default();
        if serde_json::from_str::<Value>(&saved).is_ok_and(|v| v["records"].as_array().map_or(0, Vec::len) >= ids.len()) {
            break;
        }
        if Instant::now() > deadline {
            return Err("history was not saved in time".into());
        }
        std::thread::yield_now();
    }
    Ok(History { file, ids, states, _dir: dir })
}

fn expect_history(sandbox: &Sandbox, history: &History, step: &Value) -> Check {
    if step.get("states").is_some() {
        let want: Vec<String> = paths(&step["states"]).into_iter().map(str::to_string).collect();
        if history.states != want {
            return Err(format!("operation states are {:?}, expected {want:?}", history.states));
        }
    }
    let root = sandbox.path("").display().to_string();
    let forbidden: Vec<String> = paths(&step["persisted_lacks"])
        .into_iter()
        .map(|text| if text == "$sandbox" { root.trim_end_matches('/').to_string() } else { text.to_string() })
        .collect();
    let raw = std::fs::read_to_string(&history.file).map_err(|e| e.to_string())?;
    for text in &forbidden {
        if raw.contains(text.as_str()) {
            return Err(format!("the saved history reveals {text:?}"));
        }
    }
    if step.get("reloaded_records").is_some() || !forbidden.is_empty() {
        let reloaded = OperationManager::load_history(Some(history.file.clone()), 7);
        if let Some(count) = step["reloaded_records"].as_u64() {
            if reloaded.records().len() as u64 != count {
                return Err(format!("history reloads {} records, expected {count}", reloaded.records().len()));
            }
        }
        let shown = format!(
            "{:?} {:?}",
            reloaded.records(),
            history.ids.iter().map(|id| reloaded.summary(*id)).collect::<Vec<_>>()
        );
        for text in &forbidden {
            if shown.contains(text.as_str()) {
                return Err(format!("the reloaded history reveals {text:?}"));
            }
        }
    }
    Ok(())
}

/// Extracts through the application's own entry point and waits for the operation to finish.
/// That entry point schedules on the process-wide default context, so this pumps that one.
fn extract_archive(sandbox: &Sandbox, step: &Value) -> Result<Result<(), String>, String> {
    let default = glib::MainContext::default();
    let _guard = default.acquire().map_err(|e| e.to_string())?;
    let manager = OperationManager::default();
    let archive = locate(sandbox, step["archive"].as_str().ok_or("extract needs `archive`")?)?;
    let parent = locate(sandbox, step["into"].as_str().unwrap_or("."))?;
    let failure: Rc<RefCell<Option<String>>> = Rc::default();
    let reported = failure.clone();
    if !extract(&manager, &archive, &parent, move |message| *reported.borrow_mut() = Some(message)) {
        return Ok(Err("extraction did not begin".into()));
    }
    pump(&default, || {
        let records = manager.records();
        !records.is_empty() && records.iter().all(|r| r.finished_at.is_some())
    })?;
    let state = manager.records().last().map(|r| r.state.clone());
    let reason = failure.borrow().clone();
    let outcome = match (state, reason) {
        (Some(OperationState::Completed), None) => Ok(()),
        (_, Some(message)) => Err(message),
        (state, None) => Err(format!("extraction ended as {state:?}")),
    };
    Ok(outcome)
}

fn compress_sources(sandbox: &Sandbox, step: &Value) -> Result<Result<String, String>, String> {
    let format = match step["format"].as_str().ok_or("compress needs `format`")? {
        "zip" => ArchiveFormat::Zip,
        "7z" => ArchiveFormat::SevenZip,
        "tar_gz" => ArchiveFormat::TarGz,
        other => return Err(format!("unknown archive format {other:?}")),
    };
    let sources: Vec<std::path::PathBuf> = paths(&step["sources"]).into_iter().map(|p| sandbox.path(p)).collect();
    let mut progress = Vec::new();
    let outcome = compress_blocking(&sources, format, &mut |done, total| progress.push((done, total)), &|| false);
    Ok(match outcome {
        Ok(path) => {
            // Progress climbs to the size of the data and ends there.
            if let (Some(first), Some(last)) = (progress.first(), progress.last()) {
                if first.0 != 0 || last.0 != last.1 {
                    return Err(format!("progress ran {first:?} to {last:?}"));
                }
            }
            Ok(path.strip_prefix(sandbox.path("")).map_err(|e| e.to_string())?.to_string_lossy().into_owned())
        }
        Err(error) => Err(error.message()),
    })
}

fn inspect_visible(context: &glib::MainContext, sandbox: &Sandbox, step: &Value) -> Result<RequestOutcome<Vec<VisibleMetadata>>, String> {
    let router = DirectoryRouter::new(context.clone());
    let cancellable = gio::Cancellable::new();
    if step["cancel"].as_str() == Some("immediately") {
        cancellable.cancel();
    }
    let locations = paths(&step["paths"]).into_iter().map(|p| locate(sandbox, p)).collect::<Result<Vec<_>, _>>()?;
    let received: Rc<RefCell<Option<RequestOutcome<Vec<VisibleMetadata>>>>> = Rc::default();
    let out = received.clone();
    router.inspect_visible_metadata(
        VisibleMetadataRequest { id: RequestId(55), locations, cancellable },
        Box::new(move |completion| {
            assert_eq!(completion.id, RequestId(55), "a completion carries its request's id");
            *out.borrow_mut() = Some(completion.outcome);
        }),
    );
    if received.borrow().is_some() {
        return Err("a callback ran inline before the adapter returned".into());
    }
    pump(context, || received.borrow().is_some())?;
    router.drain_owner_requests();
    let outcome = received.borrow_mut().take().unwrap();
    Ok(outcome)
}

fn expect_unsupported_pages(pages: &[RequestOutcome<DirectorySnapshot>]) -> Check {
    match pages.last() {
        Some(RequestOutcome::Failed(error)) if error.kind == OperationErrorKind::Unsupported => Ok(()),
        _ => Err("excluded feature did not report unsupported".into()),
    }
}

fn outcome_label<T>(outcome: &RequestOutcome<T>) -> &'static str {
    match outcome {
        RequestOutcome::Succeeded(_) => "succeeded",
        RequestOutcome::Failed(_) => "failed",
        RequestOutcome::Cancelled => "cancelled",
    }
}

fn check_failure<T>(outcome: &RequestOutcome<T>, expected: &Value) -> Check {
    if let (RequestOutcome::Failed(error), Some(expected)) = (outcome, expected.as_object()) {
        if let Some(kind) = expected.get("kind").and_then(Value::as_str) {
            if error_kind_name(error.kind) != kind {
                return Err(format!("error kind is {}, expected {kind}", error_kind_name(error.kind)));
            }
        }
    }
    Ok(())
}

fn expectation(sandbox: &Sandbox, runs: &HashMap<String, Bound>, expect: &str, step: &Value) -> Check {
    let bound = || -> Result<&Bound, String> {
        let name = step["ref"].as_str().ok_or("expectation needs `ref`")?.trim_start_matches('$');
        runs.get(name).ok_or_else(|| format!("no step bound {name:?}"))
    };
    let find = || -> Result<&Run, String> {
        match bound()? {
            Bound::Run(run) => Ok(run),
            _ => Err("that step did not run an operation".into()),
        }
    };
    let snapshot = || -> Result<Option<&DirectorySnapshot>, String> {
        match bound()? {
            Bound::Pages(pages) => Ok(pages.iter().find_map(|p| match p {
                RequestOutcome::Succeeded(s) => Some(s),
                _ => None,
            })),
            Bound::Inspect(_) => Ok(None),
            _ => Err("that step did not list a directory".into()),
        }
    };
    match expect {
        "outcome" => {
            let want = step["is"].as_str().ok_or("outcome needs `is`")?;
            match bound()? {
                Bound::Run(run) => {
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
                Bound::Pages(pages) => {
                    let last = pages.last().ok_or("the listing delivered nothing")?;
                    if outcome_label(last) != want {
                        return Err(format!("outcome is {}, expected {want}", outcome_label(last)));
                    }
                    check_failure(last, &step["error"])
                }
                Bound::Visible(outcome) => {
                    if outcome_label(outcome) != want {
                        return Err(format!("outcome is {}, expected {want}", outcome_label(outcome)));
                    }
                    Ok(())
                }
                Bound::Inspect(outcome) => {
                    if outcome_label(outcome) != want {
                        return Err(format!("outcome is {}, expected {want}", outcome_label(outcome)));
                    }
                    check_failure(outcome, &step["error"])
                }
                Bound::Extract(result) => {
                    let got = if result.is_ok() { "succeeded" } else { "failed" };
                    if got == want { Ok(()) } else { Err(format!("extraction {got}, expected {want}: {result:?}")) }
                }
                Bound::Compress(result) => {
                    let got = if result.is_ok() { "succeeded" } else { "failed" };
                    if got != want {
                        return Err(format!("compression {got}, expected {want}: {result:?}"));
                    }
                    match (result, step["archive"].as_str()) {
                        (Ok(path), Some(expected)) if path != expected => Err(format!("archive is {path:?}, expected {expected:?}")),
                        _ => Ok(()),
                    }
                }
                Bound::Search(_) | Bound::History(_) => Err("this step has no single outcome".into()),
            }
        }
        "metadata" => {
            let Bound::Visible(RequestOutcome::Succeeded(rows)) = bound()? else { return Err("no visible metadata was delivered".into()) };
            let path = step["path"].as_str().ok_or("metadata needs `path`")?;
            let wanted = locate(sandbox, path)?;
            let row = rows.iter().find(|r| r.location == wanted).ok_or_else(|| format!("no metadata for {path}"))?;
            if let Some(size) = step["size"].as_u64() {
                if row.size != Some(size) {
                    return Err(format!("{path} size is {:?}, expected {size}", row.size));
                }
            }
            for (field, present) in [("has_modified", row.modified.is_some()), ("has_content_type", row.content_type.is_some())] {
                if let Some(want) = step[field].as_bool() {
                    if present != want {
                        return Err(format!("{path} {field} is {present}, expected {want}"));
                    }
                }
            }
            Ok(())
        }
        "history" => match bound()? {
            Bound::History(history) => expect_history(sandbox, history, step),
            _ => Err("that step did not run the operation manager".into()),
        },
        "effect" => {
            let completion = find()?.completion()?;
            let got = effect_name(completion.effect);
            let want = step["is"].as_str().ok_or("effect needs `is`")?;
            if got != want {
                return Err(format!("effect is {got}, expected {want}"));
            }
            let relative = |list: &[Location]| -> Vec<String> {
                list.iter()
                    .map(|l| {
                        l.local_path()
                            .and_then(|p| p.strip_prefix(sandbox.path("")).ok().map(|r| r.to_string_lossy().into_owned()))
                            .unwrap_or_else(|| l.uri().to_string())
                    })
                    .collect()
            };
            for (field, list) in [("completed", &completion.completed), ("remaining", &completion.remaining)] {
                if step.get(field).is_some() {
                    let want: Vec<String> = paths(&step[field]).into_iter().map(str::to_string).collect();
                    if relative(list) != want {
                        return Err(format!("{field} is {:?}, expected {want:?}", relative(list)));
                    }
                }
            }
            Ok(())
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
        "entries" => {
            let Bound::Pages(pages) = bound()? else { return Err("that step did not list a directory".into()) };
            let mut names: Vec<String> = pages
                .iter()
                .filter_map(|p| match p {
                    RequestOutcome::Succeeded(s) => Some(s.entries.iter().map(|e| e.display_name.clone())),
                    _ => None,
                })
                .flatten()
                .collect();
            let mut want: Vec<String> = paths(&step["names"]).into_iter().map(str::to_string).collect();
            if step["order"].as_str() != Some("exact") {
                names.sort();
                want.sort();
            }
            if names == want { Ok(()) } else { Err(format!("entries are {names:?}, expected {want:?}")) }
        }
        "pages" => {
            let Bound::Pages(pages) = bound()? else { return Err("that step did not list a directory".into()) };
            let counts: Vec<usize> = pages
                .iter()
                .filter_map(|p| match p {
                    RequestOutcome::Succeeded(s) => Some(s.entries.len()),
                    _ => None,
                })
                .collect();
            if let Some(max) = step["first_page_max"].as_u64() {
                let first = counts.first().copied().ok_or("no page delivered")?;
                if first as u64 > max {
                    return Err(format!("the first page holds {first} entries, more than {max}"));
                }
            }
            if let Some(total) = step["total"].as_u64() {
                if counts.iter().sum::<usize>() as u64 != total {
                    return Err(format!("pages hold {} entries, expected {total}", counts.iter().sum::<usize>()));
                }
            }
            if step["terminal"].as_str() == Some("exactly_one") {
                let terminals = pages
                    .iter()
                    .filter(|p| !matches!(p, RequestOutcome::Succeeded(s) if s.next_page.is_some()))
                    .count();
                if terminals != 1 || !matches!(pages.last(), Some(p) if !matches!(p, RequestOutcome::Succeeded(s) if s.next_page.is_some())) {
                    return Err(format!("expected exactly one terminal page, last, saw {terminals}"));
                }
            }
            Ok(())
        }
        "entry" => {
            let snapshot = snapshot()?.ok_or("the listing did not succeed")?;
            let name = step["name"].as_str().ok_or("entry needs `name`")?;
            let entry = snapshot.entries.iter().find(|e| e.display_name == name).ok_or_else(|| format!("no entry named {name:?}"))?;
            if let Some(kind) = step["kind"].as_str() {
                let got = match entry.kind {
                    EntryKind::File => "file",
                    EntryKind::Directory => "directory",
                    EntryKind::Shortcut => "shortcut",
                    _ => "other",
                };
                if got != kind {
                    return Err(format!("{name} is a {got}, expected {kind}"));
                }
            }
            for (field, got_none) in [
                ("size", entry.size.is_none()),
                ("modified", entry.modified.is_none()),
                ("content_type", entry.content_type.is_none()),
            ] {
                match step.get(field) {
                    Some(Value::Null) if !got_none => return Err(format!("{name} {field} was loaded eagerly")),
                    Some(Value::Null) | None => {}
                    Some(other) => return Err(format!("entry {field} can only be checked for null, not {other}")),
                }
            }
            for cap in paths(&step["has"]) {
                if !entry.capabilities.supports(capability(cap)?) {
                    return Err(format!("{name} lacks {cap}"));
                }
            }
            for cap in paths(&step["lacks"]) {
                if entry.capabilities.supports(capability(cap)?) {
                    return Err(format!("{name} has {cap}"));
                }
            }
            if let Some(path) = step["path"].as_str() {
                if entry.location != locate(sandbox, path)? {
                    return Err(format!("{name} is at {}, expected {path}", entry.location.uri()));
                }
            }
            Ok(())
        }
        "capability" => {
            let caps = match bound()? {
                Bound::Pages(_) => snapshot()?.ok_or("the listing did not succeed")?.capabilities.clone(),
                Bound::Inspect(RequestOutcome::Succeeded(caps)) => caps.clone(),
                _ => return Err("no capability snapshot to inspect".into()),
            };
            for cap in paths(&step["has"]) {
                if !caps.supports(capability(cap)?) {
                    return Err(format!("capabilities lack {cap}"));
                }
            }
            for cap in paths(&step["lacks"]) {
                if caps.supports(capability(cap)?) {
                    return Err(format!("capabilities have {cap}"));
                }
            }
            if let Some(want) = step["stable_order"].as_bool() {
                if caps.stable_order != want {
                    return Err(format!("stable_order is {}, expected {want}", caps.stable_order));
                }
            }
            Ok(())
        }
        "search" => {
            let Bound::Search(outcome) = bound()? else { return Err("that step did not run a search".into()) };
            let (label, matches, limits, scope_limited, reason) = match outcome {
                SearchOutcome::Completed { matches, limits, scope_limited, .. } => ("completed", Some(matches), limits.clone(), Some(*scope_limited), None),
                SearchOutcome::Incomplete { matches, reason, .. } => ("incomplete", Some(matches), vec![], None, Some(*reason)),
                SearchOutcome::Unsupported { .. } => ("unsupported", None, vec![], None, None),
            };
            let want = step["is"].as_str().ok_or("search needs `is`")?;
            if label != want {
                return Err(format!("search is {label}, expected {want}"));
            }
            if step.get("names").is_some() {
                let got: Vec<&str> = matches.into_iter().flatten().map(|e| e.display_name.as_str()).collect();
                let want = paths(&step["names"]);
                if got != want {
                    return Err(format!("matches are {got:?}, expected {want:?}"));
                }
            }
            // `limits` names bounds that were in force, so the expectation is inclusion: a
            // platform must list a limit that cut the search short and may list the others.
            for want in paths(&step["limits"]) {
                let present = limits.iter().any(|l| {
                    matches!(
                        (l, want),
                        (SearchLimit::Depth, "depth") | (SearchLimit::ResultCount, "result_count") | (SearchLimit::Time, "time")
                    )
                });
                if !present {
                    return Err(format!("limits {limits:?} do not include {want}"));
                }
            }
            if step.get("paths").is_some() {
                let got: Vec<Location> = matches.into_iter().flatten().map(|e| e.location.clone()).collect();
                let want: Vec<Location> = paths(&step["paths"]).into_iter().map(|p| locate(sandbox, p)).collect::<Result<_, _>>()?;
                if got != want {
                    return Err(format!("match locations are {:?}, expected {:?}", got.iter().map(|l| l.uri()).collect::<Vec<_>>(), want.iter().map(|l| l.uri()).collect::<Vec<_>>()));
                }
            }
            if let Some(want) = step["scope_limited"].as_bool() {
                if scope_limited != Some(want) {
                    return Err(format!("scope_limited is {scope_limited:?}, expected {want}"));
                }
            }
            if let Some(want) = step["reason"].as_str() {
                let got = reason.map(|r| match r {
                    SearchIncompleteReason::Cancelled => "cancelled",
                    SearchIncompleteReason::ProviderFailure => "provider_failure",
                    SearchIncompleteReason::MalformedPage => "malformed_page",
                    SearchIncompleteReason::AccessLost => "access_lost",
                });
                if got != Some(want) {
                    return Err(format!("reason is {got:?}, expected {want}"));
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
