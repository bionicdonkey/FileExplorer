//! The `search` subject (the search coordinator sharing the navigation tab), the `clock`
//! subject (a hand-driven clock behind the debounce), and the `search_history` policy.
//!
//! The scripted search port records each request and answers only when a step says so.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use serde_json::Value;
use lorin_explorer::application::search::{DelayFn, SearchCoordinator};
use lorin_explorer::core::contracts::*;
use lorin_explorer::core::navigation::{TabContent, TabState};
use lorin_explorer::core::ports::*;
use lorin_explorer::core::{Location, SearchHistory};

use super::{error_kind, location, World};
use crate::Check;

struct HeldSearch {
    request: SearchRequest,
    done: RefCell<Option<SearchCallback>>,
}

#[derive(Default)]
pub struct ScriptedSearch {
    held: RefCell<Vec<HeldSearch>>,
}

impl SearchRepository for ScriptedSearch {
    fn search(&self, request: SearchRequest, done: SearchCallback) {
        self.held.borrow_mut().push(HeldSearch { request, done: RefCell::new(Some(done)) });
    }
}

/// Virtual time for the debounce. Nothing sleeps: `advance` completes the delays that fall due.
#[derive(Default)]
pub struct ManualClock {
    now_ms: Cell<u64>,
    waiters: RefCell<Vec<(u64, futures_channel::oneshot::Sender<()>)>>,
}

impl ManualClock {
    fn delay(self: &Rc<Self>) -> DelayFn {
        let clock = self.clone();
        Rc::new(move |duration: Duration| {
            let (sender, receiver) = futures_channel::oneshot::channel();
            let due = clock.now_ms.get() + duration.as_millis() as u64;
            clock.waiters.borrow_mut().push((due, sender));
            Box::pin(async move {
                let _ = receiver.await;
            })
        })
    }

    fn advance(&self, ms: u64) {
        self.now_ms.set(self.now_ms.get() + ms);
        let now = self.now_ms.get();
        let due: Vec<_> = {
            let mut waiters = self.waiters.borrow_mut();
            let (due, later): (Vec<_>, Vec<_>) = waiters.drain(..).partition(|(at, _)| *at <= now);
            *waiters = later;
            due
        };
        for (_, sender) in due {
            let _ = sender.send(());
        }
    }
}

pub struct Search {
    repo: Rc<ScriptedSearch>,
    coordinator: SearchCoordinator,
    clock: Rc<ManualClock>,
    history_dir: RefCell<Option<tempfile::TempDir>>,
    history: RefCell<Option<SearchHistory>>,
}

impl Search {
    pub fn new(tab: Rc<RefCell<TabState>>) -> Self {
        let repo = Rc::new(ScriptedSearch::default());
        let clock = Rc::new(ManualClock::default());
        Self {
            coordinator: SearchCoordinator::new(tab, repo.clone()).with_delay(clock.delay()),
            repo,
            clock,
            history_dir: RefCell::new(None),
            history: RefCell::new(None),
        }
    }
}

/// Runs everything the glib main context has ready, which includes the debounce futures.
fn pump() {
    let context = glib::MainContext::ref_thread_default();
    while context.pending() {
        context.iteration(false);
    }
}

fn mode(step: &Value) -> Result<SearchMode, String> {
    match step["mode"].as_str().unwrap_or("filename") {
        "filename" => Ok(SearchMode::Filename),
        "content" => Ok(SearchMode::Content),
        other => Err(format!("unknown search mode {other:?}")),
    }
}

/// `scope` overrides fields of the default (`local_fallback` of the tab's location).
fn scope(world: &World, over: &Value) -> Result<SearchScope, String> {
    let root = match over["root"].as_str() {
        Some(uri) => location(uri)?,
        None => world.tab.borrow().location.clone(),
    };
    let mut scope = SearchScope::local_fallback(root);
    if let Some(depth) = over["max_depth"].as_u64() {
        scope.max_depth = depth as u32;
    }
    if let Some(results) = over["max_results"].as_u64() {
        scope.max_results = results as usize;
    }
    if let Some(ms) = over["timeout_ms"].as_u64() {
        scope.timeout = Duration::from_millis(ms);
    }
    Ok(scope)
}

fn check_accepted(step: &Value, accepted: bool) -> Check {
    match step["accepted"].as_bool() {
        Some(want) if want != accepted => Err(format!("action accepted is {accepted}, expected {want}")),
        _ => Ok(()),
    }
}

pub fn action(world: &World, action: &str, step: &Value) -> Check {
    let search = &world.search.coordinator;
    match action {
        "submit" | "type" => {
            let text = step["text"].as_str().ok_or("needs `text`")?;
            let scope = scope(world, &step["scope"])?;
            let accepted = if action == "type" {
                search.submit_debounced(text, mode(step)?, scope, |_| {})
            } else {
                search.submit(text, mode(step)?, scope, |_| {})
            };
            check_accepted(step, accepted)
        }
        "retry" => check_accepted(step, search.retry(|_| {})),
        "expand_scope" => check_accepted(step, search.expand_scope(scope(world, &step["scope"])?, |_| {})),
        "clear" => check_accepted(step, search.clear()),
        other => Err(format!("search has no action {other:?}")),
    }
}

pub fn clock_action(world: &World, action: &str, step: &Value) -> Check {
    match action {
        "advance" => {
            world.search.clock.advance(step["ms"].as_u64().ok_or("advance needs `ms`")?);
            pump();
            Ok(())
        }
        other => Err(format!("clock has no action {other:?}")),
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

fn entries(value: &Value) -> Result<Vec<EntrySnapshot>, String> {
    value
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
        .collect()
}

fn reason(name: &str) -> Result<SearchIncompleteReason, String> {
    Ok(match name {
        "cancelled" => SearchIncompleteReason::Cancelled,
        "provider_failure" => SearchIncompleteReason::ProviderFailure,
        "malformed_page" => SearchIncompleteReason::MalformedPage,
        "access_lost" => SearchIncompleteReason::AccessLost,
        other => return Err(format!("unknown incomplete reason {other:?}")),
    })
}

fn limit(name: &str) -> Result<SearchLimit, String> {
    Ok(match name {
        "depth" => SearchLimit::Depth,
        "result_count" => SearchLimit::ResultCount,
        "time" => SearchLimit::Time,
        other => return Err(format!("unknown limit {other:?}")),
    })
}

pub fn script_port(world: &World, step: &Value) -> Check {
    let held = world.search.repo.held.borrow();
    let entry = &held[index(step, held.len())?];
    let done = entry.done.borrow_mut().take().ok_or("that search was already answered")?;
    let declared = entry.request.scope.clone();
    let id = entry.request.id;
    drop(held);
    let respond = &step["respond"];
    let outcome = if let Some(body) = respond.get("completed") {
        SearchOutcome::Completed {
            matches: entries(&body["matches"])?,
            declared_scope: declared,
            scope_limited: body["scope_limited"].as_bool().unwrap_or(false),
            limits: body["limits"].as_array().into_iter().flatten().filter_map(Value::as_str).map(limit).collect::<Result<_, _>>()?,
        }
    } else if let Some(body) = respond.get("incomplete") {
        SearchOutcome::Incomplete {
            matches: entries(&body["matches"])?,
            reason: reason(body["reason"].as_str().ok_or("incomplete needs `reason`")?)?,
            declared_scope: declared,
            retry: RetryClass::AfterRefresh,
            error: None,
        }
    } else if respond.get("unsupported").is_some() {
        SearchOutcome::Unsupported { declared_scope: declared, error: OperationError::new(OperationErrorKind::Unsupported, None, None) }
    } else if let Some(body) = respond.get("wrong_scope") {
        // A provider that answers for a scope other than the one requested.
        let mut other = declared;
        other.max_results = body["max_results"].as_u64().unwrap_or(1) as usize;
        SearchOutcome::Completed { matches: vec![], declared_scope: other, scope_limited: false, limits: vec![] }
    } else {
        return Err(format!("unknown `respond`: {respond}"));
    };
    done(SearchCompletion { id, outcome });
    Ok(())
}

pub fn expect_port_requests(world: &World, step: &Value) -> Check {
    let held = world.search.repo.held.borrow();
    if let Some(count) = step["count"].as_u64() {
        if held.len() as u64 != count {
            return Err(format!("search port received {} request(s), expected {count}", held.len()));
        }
    }
    let last = &step["last"];
    if !last.is_null() {
        let request = &held.last().ok_or("no search request received")?.request;
        let check = |field: &str, got: String, want: &Value| -> Check {
            if want.as_str() == Some(got.as_str()) || want.to_string() == got { Ok(()) } else { Err(format!("last request {field} is {got}, expected {want}")) }
        };
        if !last["query"].is_null() {
            check("query", request.query.clone(), &last["query"])?;
        }
        if !last["root"].is_null() {
            check("root", request.scope.root.uri().to_string(), &last["root"])?;
        }
        if !last["max_depth"].is_null() {
            check("max_depth", request.scope.max_depth.to_string(), &last["max_depth"])?;
        }
        if !last["max_results"].is_null() {
            check("max_results", request.scope.max_results.to_string(), &last["max_results"])?;
        }
        if !last["timeout_ms"].is_null() {
            check("timeout_ms", request.scope.timeout.as_millis().to_string(), &last["timeout_ms"])?;
        }
    }
    Ok(())
}

pub fn expect_cancelled(world: &World, step: &Value) -> Check {
    use gio::prelude::CancellableExt;
    let held = world.search.repo.held.borrow();
    let entry = &held[index(step, held.len())?];
    let want = step["is"].as_bool().unwrap_or(true);
    let got = entry.request.cancellable.is_cancelled();
    if got == want { Ok(()) } else { Err(format!("search request cancelled is {got}, expected {want}")) }
}

fn content_name(content: &TabContent) -> &'static str {
    match content {
        TabContent::Directory(_) => "directory",
        TabContent::SectionOverview { .. } => "section",
        TabContent::Search { .. } => "search",
        TabContent::Unavailable(_) => "unavailable",
    }
}

/// `status` is `none` until an accepted outcome arrives, then `completed`, `incomplete` or
/// `unsupported`. `content` says what the tab shows; `matches` lists the retained results.
pub fn expect_view(world: &World, step: &Value) -> Check {
    let tab = world.tab.borrow();
    let outcome = world.search.coordinator.outcome();
    let mismatch = |field: &str, got: String, want: &Value| format!("{field} is {got}, expected {want}");
    if let Some(want) = step.get("status") {
        let got = match &outcome {
            None => "none",
            Some(SearchOutcome::Completed { .. }) => "completed",
            Some(SearchOutcome::Incomplete { .. }) => "incomplete",
            Some(SearchOutcome::Unsupported { .. }) => "unsupported",
        };
        if want.as_str() != Some(got) {
            return Err(mismatch("status", got.into(), want));
        }
    }
    if let Some(want) = step.get("reason") {
        let got = match &outcome {
            Some(SearchOutcome::Incomplete { reason, .. }) => format!("{reason:?}"),
            _ => "none".into(),
        };
        let wanted = reason(want.as_str().unwrap_or(""))?;
        if got != format!("{wanted:?}") {
            return Err(mismatch("reason", got, want));
        }
    }
    if let Some(want) = step.get("content") {
        let got = content_name(&tab.content);
        if want.as_str() != Some(got) {
            return Err(mismatch("content", got.into(), want));
        }
    }
    if let Some(want) = step.get("matches") {
        let got: Vec<String> = match &tab.content {
            TabContent::Search { items, .. } => items.iter().map(|i| i.location().uri().to_string()).collect(),
            _ => vec![],
        };
        let wanted: Vec<String> = want.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
        if got != wanted {
            return Err(mismatch("matches", format!("{got:?}"), want));
        }
    }
    if let Some(want) = step.get("selection") {
        let wanted: Vec<String> = want.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
        if tab.selection_keys != wanted {
            return Err(mismatch("selection", format!("{:?}", tab.selection_keys), want));
        }
    }
    if let Some(want) = step.get("can_go_back").and_then(Value::as_bool) {
        if tab.history.can_go_back() != want {
            return Err(mismatch("can_go_back", tab.history.can_go_back().to_string(), &Value::Bool(want)));
        }
    }
    Ok(())
}

// Search history policy: `record_submitted` against a file in a temporary directory.

fn history_path(search: &Search) -> std::path::PathBuf {
    let mut dir = search.history_dir.borrow_mut();
    dir.get_or_insert_with(|| tempfile::tempdir().expect("history dir")).path().join("searches.json")
}

pub fn history_action(world: &World, action: &str, step: &Value) -> Check {
    let path = history_path(&world.search);
    let mut slot = world.search.history.borrow_mut();
    if slot.is_none() {
        *slot = Some(SearchHistory::load_from(path).map_err(|e| e.to_string())?);
    }
    let history = slot.as_mut().unwrap();
    match action {
        "record_submitted" => history
            .record_submitted(
                step["text"].as_str().ok_or("needs `text`")?,
                step["enabled"].as_bool().ok_or("needs `enabled`")?,
                step["retention_days"].as_u64().unwrap_or(30) as u32,
            )
            .map_err(|e| e.to_string()),
        other => Err(format!("search_history has no action {other:?}")),
    }
}

pub fn history_expectation(world: &World, step: &Value) -> Check {
    let path = history_path(&world.search);
    if let Some(want) = step["entries"].as_u64() {
        let got = world.search.history.borrow().as_ref().map_or(0, |h| h.entries().len());
        if got as u64 != want {
            return Err(format!("history holds {got} entries, expected {want}"));
        }
    }
    if let Some(want) = step["persisted_entries"].as_u64() {
        let got = SearchHistory::load_from(path).map_err(|e| e.to_string())?.entries().len();
        if got as u64 != want {
            return Err(format!("the saved history holds {got} entries, expected {want}"));
        }
    }
    let _ = (error_kind as fn(&str) -> _, Location::home as fn() -> _);
    Ok(())
}
