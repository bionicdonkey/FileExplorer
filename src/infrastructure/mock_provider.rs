//! Network-free contract adapter driven by an explicit virtual clock.
//!
//! `advance` makes due work ready; iterate the supplied GLib main context to
//! deliver it. Each operation/search stage consumes at least one tick, so
//! cancellation between stages has deterministic acknowledgement and no sleeps.
//! Search accepts at most one entry per stage and yields at directory boundaries.
//! Fixtures model provider results, not an actual mutable filesystem. Clones
//! share fixtures and scheduled work; keep one alive while driving requests.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use gio::prelude::*;

use crate::core::contracts::*;
use crate::core::ports::*;
use crate::core::{Location, ResolvedLocation};

#[derive(Clone, Debug)]
pub struct MockConfig {
    pub latency_ticks: u64,
    /// Virtual elapsed repository time per inspected search item.
    pub search_entry_time: Duration,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            latency_ticks: 1,
            search_entry_time: Duration::from_millis(1),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum MockOperationBehavior {
    #[default]
    Succeed,
    FailAfter {
        completed: usize,
        kind: OperationErrorKind,
    },
    /// Models a provider that has committed before observing cancellation.
    SucceedDespiteCancellation,
}

#[derive(Clone, Debug, Default)]
pub enum MockSearchBehavior {
    #[default]
    Complete,
    FailAfter {
        matches: usize,
        reason: SearchIncompleteReason,
    },
}

#[derive(Clone)]
struct DirectoryFixture {
    entries: Vec<EntrySnapshot>,
    capabilities: CapabilitySnapshot,
}

type Action = Box<dyn FnOnce(&MockProvider)>;

struct Scheduled {
    due: u64,
    action: Action,
}

struct MockState {
    clock: u64,
    config: MockConfig,
    pending: Vec<Scheduled>,
    directories: HashMap<Location, DirectoryFixture>,
    redirects: HashMap<Location, Location>,
    capabilities: CapabilitySnapshot,
    operation_behavior: MockOperationBehavior,
    search_behavior: MockSearchBehavior,
    visible_metadata_requests: Vec<Location>,
    list_results_after_cancellation: bool,
    visible_metadata_results_after_cancellation: bool,
    visible_metadata_cancellables: Vec<gio::Cancellable>,
    active_visible_metadata_requests: usize,
}

#[derive(Clone)]
pub struct MockProvider {
    context: glib::MainContext,
    state: Rc<RefCell<MockState>>,
}

impl MockProvider {
    pub fn new(context: glib::MainContext, config: MockConfig) -> Self {
        assert!(
            context.is_owner(),
            "construct mock on its owning GLib context"
        );
        Self {
            context,
            state: Rc::new(RefCell::new(MockState {
                clock: 0,
                config,
                pending: Vec::new(),
                directories: HashMap::new(),
                redirects: HashMap::new(),
                capabilities: CapabilitySnapshot::default(),
                operation_behavior: MockOperationBehavior::default(),
                search_behavior: MockSearchBehavior::default(),
                visible_metadata_requests: Vec::new(),
                list_results_after_cancellation: false,
                visible_metadata_results_after_cancellation: false,
                visible_metadata_cancellables: Vec::new(),
                active_visible_metadata_requests: 0,
            })),
        }
    }

    pub fn set_directory(
        &self,
        location: Location,
        mut entries: Vec<EntrySnapshot>,
        capabilities: CapabilitySnapshot,
    ) {
        // A stable fixture has a canonical, reproducible order regardless of
        // insertion order. URI identity also prevents duplicate page entries.
        if capabilities.stable_order {
            entries.sort_by(|left, right| left.location.uri().cmp(right.location.uri()));
        }
        let mut seen = BTreeSet::new();
        entries.retain(|entry| seen.insert(entry.location.uri().to_owned()));
        self.state.borrow_mut().directories.insert(
            location,
            DirectoryFixture {
                entries,
                capabilities,
            },
        );
    }

    pub fn set_redirect(&self, requested: Location, resolved: Location) {
        self.state
            .borrow_mut()
            .redirects
            .insert(requested, resolved);
    }

    /// Global operation capability fixture. Directory/search fixtures carry
    /// their own location-specific snapshots.
    pub fn set_capabilities(&self, capabilities: CapabilitySnapshot) {
        self.state.borrow_mut().capabilities = capabilities;
    }

    pub fn set_operation_behavior(&self, behavior: MockOperationBehavior) {
        self.state.borrow_mut().operation_behavior = behavior;
    }

    pub fn set_search_behavior(&self, behavior: MockSearchBehavior) {
        self.state.borrow_mut().search_behavior = behavior;
    }

    pub fn visible_metadata_requests(&self) -> Vec<Location> {
        self.state.borrow().visible_metadata_requests.clone()
    }

    /// Test providers may model a backend that reports an already-completed
    /// list after the caller has cancelled its token.
    pub fn set_list_results_after_cancellation(&self, enabled: bool) {
        self.state.borrow_mut().list_results_after_cancellation = enabled;
    }
    /// Test providers may model a backend that completes metadata inspection
    /// after the caller has cancelled its token.
    pub fn set_visible_metadata_results_after_cancellation(&self, enabled: bool) {
        self.state
            .borrow_mut()
            .visible_metadata_results_after_cancellation = enabled;
    }

    pub fn visible_metadata_request_cancellables(&self) -> Vec<gio::Cancellable> {
        self.state.borrow().visible_metadata_cancellables.clone()
    }

    pub fn active_visible_metadata_requests(&self) -> usize {
        self.state.borrow().active_visible_metadata_requests
    }

    /// Make due work ready in submission order for equal deadlines. This never
    /// calls application code inline. Newly scheduled stages require a later
    /// advance, even when the caller advances by a large number of ticks.
    pub fn advance(&self, ticks: u64) {
        assert!(
            self.context.is_owner(),
            "advance on the owning GLib context"
        );
        let due = {
            let mut state = self.state.borrow_mut();
            state.clock = state.clock.saturating_add(ticks);
            let mut due = Vec::new();
            let mut future = Vec::new();
            for scheduled in std::mem::take(&mut state.pending) {
                if scheduled.due <= state.clock {
                    due.push(scheduled);
                } else {
                    future.push(scheduled);
                }
            }
            state.pending = future;
            due.sort_by_key(|scheduled| scheduled.due);
            due
        };
        if !due.is_empty() {
            let provider = self.clone();
            self.context.spawn_local(async move {
                for scheduled in due {
                    (scheduled.action)(&provider);
                }
            });
        }
    }

    fn schedule(&self, action: Action) {
        assert!(
            self.context.is_owner(),
            "request on the owning GLib context"
        );
        let mut state = self.state.borrow_mut();
        let due = state
            .clock
            .saturating_add(state.config.latency_ticks.max(1));
        state.pending.push(Scheduled { due, action });
    }

    fn resolve(&self, requested: &Location) -> Result<Location, OperationError> {
        let state = self.state.borrow();
        let mut resolved = requested.clone();
        let mut seen = BTreeSet::new();
        while let Some(next) = state.redirects.get(&resolved) {
            if !seen.insert(resolved.uri().to_owned()) {
                return Err(OperationError::new(
                    OperationErrorKind::InvalidRequest,
                    Some(requested),
                    None,
                ));
            }
            resolved = next.clone();
        }
        Ok(resolved)
    }

    fn list_snapshot(
        &self,
        request: &DirectoryRequest,
    ) -> Result<DirectorySnapshot, OperationError> {
        let resolved = self.resolve(&request.location)?;
        let state = self.state.borrow();
        let error = |kind| OperationError::new(kind, Some(&request.location), None);
        let fixture = state
            .directories
            .get(&resolved)
            .ok_or_else(|| error(OperationErrorKind::NotFound))?;
        if request.page.limit == 0 {
            return Err(error(OperationErrorKind::InvalidRequest));
        }
        if !fixture.capabilities.supports(Capability::List) || !fixture.capabilities.stable_order {
            return Err(error(OperationErrorKind::Unsupported));
        }
        let start = request.page.offset.min(fixture.entries.len());
        let end = start
            .saturating_add(request.page.limit)
            .min(fixture.entries.len());
        Ok(DirectorySnapshot {
            location: ResolvedLocation {
                requested: request.location.clone(),
                resolved,
            },
            entries: fixture.entries[start..end].to_vec(),
            capabilities: fixture.capabilities.clone(),
            next_page: (end < fixture.entries.len()).then_some(PageRequest {
                offset: end,
                limit: request.page.limit,
            }),
        })
    }

    fn capabilities_at(&self, location: &Location) -> Result<CapabilitySnapshot, OperationError> {
        let resolved = self.resolve(location)?;
        let state = self.state.borrow();
        Ok(state
            .directories
            .get(&resolved)
            .map(|fixture| &fixture.capabilities)
            .unwrap_or(&state.capabilities)
            .clone())
    }

    fn operation_capabilities_current(&self, request: &OperationRequest) -> bool {
        let supports = |location: &Location, capability| {
            self.capabilities_at(location).is_ok_and(|snapshot| {
                snapshot.version == request.capability_version && snapshot.supports(capability)
            })
        };
        request
            .sources
            .iter()
            .all(|source| supports(source, request.kind.capability()))
            && request
                .destination
                .iter()
                .chain(request.explicit_target.iter())
                .all(|target| supports(target, Capability::Create))
    }

    fn run_operation_stage(&self, mut operation: PendingOperation) {
        let request = &operation.request;
        if request.cancellable.is_cancelled()
            && (!operation.started
                || !matches!(
                    operation.behavior,
                    MockOperationBehavior::SucceedDespiteCancellation
                ))
        {
            let event = OperationEvent::Cancelled {
                id: request.id,
                sequence: operation.next_sequence,
                outcome: CancellationOutcome {
                    completion: operation.completion(),
                },
            };
            (operation.sink)(event);
            return;
        }
        if !operation.started {
            let error_kind = if !self.operation_capabilities_current(request) {
                Some(OperationErrorKind::CapabilityChanged)
            } else if !valid_operation(request) {
                Some(OperationErrorKind::InvalidRequest)
            } else {
                None
            };
            if let Some(kind) = error_kind {
                operation.fail(kind);
                return;
            }
            operation.started = true;
            (operation.sink)(OperationEvent::Started {
                id: request.id,
                sequence: operation.next_sequence,
            });
            operation.next_sequence += 1;
        } else {
            if let MockOperationBehavior::FailAfter { completed, kind } = operation.behavior {
                if operation.completed == completed {
                    operation.fail(kind);
                    return;
                }
            }
            if operation.completed == operation.items().len() {
                let completion = operation.completion();
                (operation.sink)(OperationEvent::Succeeded {
                    id: request.id,
                    sequence: operation.next_sequence,
                    completion,
                });
                return;
            }
            operation.completed += 1;
            let total_items = Some(operation.items().len() as u64);
            (operation.sink)(OperationEvent::Progress {
                id: request.id,
                sequence: operation.next_sequence,
                progress: OperationProgress {
                    completed_items: operation.completed as u64,
                    total_items,
                    completed_bytes: None,
                    total_bytes: None,
                },
            });
            operation.next_sequence += 1;
        }
        self.schedule(Box::new(move |provider| {
            provider.run_operation_stage(operation)
        }));
    }

    fn run_search_stage(&self, mut search: PendingSearch) {
        if let Some(outcome) = self.search_step(&mut search) {
            // The one-shot callback is moved into the terminal path. All
            // provider borrows have ended before application code runs.
            (search.done)(SearchCompletion {
                id: search.request.id,
                outcome,
            });
        } else {
            self.schedule(Box::new(move |provider| provider.run_search_stage(search)));
        }
    }

    fn search_step(&self, search: &mut PendingSearch) -> Option<SearchOutcome> {
        // Every entry/directory boundary returns through here, including the
        // final completion boundary. Check the real token before more work.
        if search.request.cancellable.is_cancelled() {
            return Some(search.incomplete(SearchIncompleteReason::Cancelled, None));
        }
        if !search.initialized {
            let resolved = match self.resolve(&search.request.scope.root) {
                Ok(location) => location,
                Err(error) => {
                    return Some(
                        search.incomplete(SearchIncompleteReason::ProviderFailure, Some(error)),
                    )
                }
            };
            let state = self.state.borrow();
            let Some(root) = state.directories.get(&resolved) else {
                return Some(search.incomplete(
                    SearchIncompleteReason::ProviderFailure,
                    Some(OperationError::new(
                        OperationErrorKind::NotFound,
                        Some(&search.request.scope.root),
                        None,
                    )),
                ));
            };
            // This mock supports filename search only, never content reads.
            if search.request.mode != SearchMode::Filename
                || !root.capabilities.supports(Capability::SearchFilename)
                || !root.capabilities.stable_order
            {
                return Some(SearchOutcome::Unsupported {
                    declared_scope: search.request.scope.clone(),
                    error: OperationError::new(
                        OperationErrorKind::Unsupported,
                        Some(&search.request.scope.root),
                        None,
                    ),
                });
            }
            search.queue.push_back((resolved, 0));
            search.initialized = true;
        }
        if search.current.is_none() {
            let Some((directory, depth)) = search.queue.pop_front() else {
                return Some(search.completed());
            };
            if !search.visited.insert(directory.uri().to_owned())
                || depth >= search.request.scope.max_depth
            {
                return None;
            }
            let state = self.state.borrow();
            let Some(fixture) = state.directories.get(&directory) else {
                return Some(search.incomplete(
                    SearchIncompleteReason::ProviderFailure,
                    Some(OperationError::new(
                        OperationErrorKind::NotFound,
                        Some(&directory),
                        None,
                    )),
                ));
            };
            if !fixture.capabilities.stable_order {
                return Some(search.incomplete(SearchIncompleteReason::MalformedPage, None));
            }
            if !fixture.capabilities.supports(Capability::List) {
                return Some(search.incomplete(
                    SearchIncompleteReason::AccessLost,
                    Some(OperationError::new(
                        OperationErrorKind::PermissionDenied,
                        Some(&directory),
                        None,
                    )),
                ));
            }
            search.current = Some((fixture.entries.clone().into_iter(), depth));
        }
        let (entries, depth) = search
            .current
            .as_mut()
            .expect("directory stage initialized");
        let Some(entry) = entries.next() else {
            search.current = None;
            return None;
        };
        let child_depth = *depth + 1;
        let state = self.state.borrow();
        if let MockSearchBehavior::FailAfter {
            matches: count,
            reason,
        } = state.search_behavior
        {
            if search.matches.len() == count {
                return Some(search.incomplete(reason, None));
            }
        }
        if search.matches.len() >= search.request.scope.max_results {
            search.limits.push(SearchLimit::ResultCount);
            return Some(search.completed());
        }
        search.elapsed = search
            .elapsed
            .saturating_add(state.config.search_entry_time);
        if search.elapsed >= search.request.scope.timeout {
            search.limits.push(SearchLimit::Time);
            return Some(search.completed());
        }
        if entry.kind == EntryKind::Directory {
            search
                .queue
                .push_back((entry.location.clone(), child_depth));
        }
        if entry.display_name.contains(&search.request.query)
            && search.accepted.insert(entry.location.uri().to_owned())
        {
            search.matches.push(entry);
        }
        None
    }
}

struct PendingSearch {
    request: SearchRequest,
    done: SearchCallback,
    initialized: bool,
    matches: Vec<EntrySnapshot>,
    accepted: BTreeSet<String>,
    visited: BTreeSet<String>,
    queue: VecDeque<(Location, u32)>,
    current: Option<(std::vec::IntoIter<EntrySnapshot>, u32)>,
    elapsed: Duration,
    limits: Vec<SearchLimit>,
}

impl PendingSearch {
    fn incomplete(
        &mut self,
        reason: SearchIncompleteReason,
        error: Option<OperationError>,
    ) -> SearchOutcome {
        SearchOutcome::Incomplete {
            matches: std::mem::take(&mut self.matches),
            reason,
            declared_scope: self.request.scope.clone(),
            retry: RetryClass::AfterRefresh,
            error,
        }
    }

    fn completed(&mut self) -> SearchOutcome {
        search_completed(
            &self.request,
            std::mem::take(&mut self.matches),
            std::mem::take(&mut self.limits),
        )
    }
}

fn search_completed(
    request: &SearchRequest,
    matches: Vec<EntrySnapshot>,
    limits: Vec<SearchLimit>,
) -> SearchOutcome {
    SearchOutcome::Completed {
        matches,
        declared_scope: request.scope.clone(),
        scope_limited: !limits.is_empty(),
        limits,
    }
}

fn valid_operation(request: &OperationRequest) -> bool {
    match request.kind {
        MutationKind::Create => request.sources.is_empty() && request.explicit_target.is_some(),
        MutationKind::Rename => request.sources.len() == 1 && request.explicit_target.is_some(),
        MutationKind::Copy | MutationKind::Move => {
            !request.sources.is_empty()
                && (request.destination.is_some() || request.explicit_target.is_some())
                && (request.explicit_target.is_none() || request.sources.len() == 1)
        }
        MutationKind::Trash | MutationKind::Delete => !request.sources.is_empty(),
    }
}

struct PendingOperation {
    request: OperationRequest,
    sink: OperationSink,
    behavior: MockOperationBehavior,
    started: bool,
    completed: usize,
    next_sequence: u64,
}

impl PendingOperation {
    fn items(&self) -> &[Location] {
        if self.request.kind == MutationKind::Create {
            self.request.explicit_target.as_slice()
        } else {
            &self.request.sources
        }
    }

    fn completion(&self) -> OperationCompletion {
        let items = self.items();
        OperationCompletion {
            effect: if self.completed == 0 {
                MutationEffect::Unchanged
            } else if self.completed == items.len() {
                MutationEffect::Changed
            } else {
                MutationEffect::PartiallyChanged
            },
            completed: items[..self.completed].to_vec(),
            remaining: items[self.completed..].to_vec(),
        }
    }

    fn fail(&mut self, kind: OperationErrorKind) {
        let completion = self.completion();
        (self.sink)(OperationEvent::Failed {
            id: self.request.id,
            sequence: self.next_sequence,
            error: OperationError::new(
                kind,
                self.request.sources.get(self.completed),
                self.request
                    .explicit_target
                    .as_ref()
                    .or(self.request.destination.as_ref()),
            ),
            completion,
        });
    }
}

impl DirectoryRepository for MockProvider {
    fn inspect_visible_metadata(
        &self,
        request: VisibleMetadataRequest,
        done: VisibleMetadataCallback,
    ) {
        self.state.borrow_mut().active_visible_metadata_requests += 1;
        self.state
            .borrow_mut()
            .visible_metadata_requests
            .extend(request.locations.clone());
        self.state
            .borrow_mut()
            .visible_metadata_cancellables
            .push(request.cancellable.clone());
        self.schedule(Box::new(move |provider| {
            let outcome = if request.cancellable.is_cancelled()
                && !provider
                    .state
                    .borrow()
                    .visible_metadata_results_after_cancellation
            {
                RequestOutcome::Cancelled
            } else {
                let state = provider.state.borrow();
                let metadata = request
                    .locations
                    .iter()
                    .filter_map(|location| {
                        state
                            .directories
                            .values()
                            .flat_map(|fixture| fixture.entries.iter())
                            .find(|entry| entry.location == *location)
                            .map(|entry| VisibleMetadata {
                                location: location.clone(),
                                size: entry.size,
                                modified: entry.modified,
                                content_type: entry.content_type.clone(),
                            })
                    })
                    .collect();
                RequestOutcome::Succeeded(metadata)
            };
            provider.state.borrow_mut().active_visible_metadata_requests -= 1;
            done(RequestCompletion {
                id: request.id,
                outcome,
            });
        }));
    }

    fn list(&self, request: DirectoryRequest, done: DirectoryCallback) {
        self.schedule(Box::new(move |provider| {
            let outcome = if request.cancellable.is_cancelled()
                && !provider.state.borrow().list_results_after_cancellation
            {
                RequestOutcome::Cancelled
            } else {
                match provider.list_snapshot(&request) {
                    Ok(snapshot) => RequestOutcome::Succeeded(snapshot),
                    Err(error) => RequestOutcome::Failed(error),
                }
            };
            done(RequestCompletion {
                id: request.id,
                outcome,
            });
        }));
    }
}

impl FileOperations for MockProvider {
    fn inspect_capabilities(&self, request: CapabilityRequest, done: CapabilityCallback) {
        self.schedule(Box::new(move |provider| {
            let outcome = if request.cancellable.is_cancelled() {
                RequestOutcome::Cancelled
            } else {
                match provider.capabilities_at(&request.location) {
                    Err(error) => RequestOutcome::Failed(error),
                    Ok(snapshot) => RequestOutcome::Succeeded(snapshot),
                }
            };
            done(RequestCompletion {
                id: request.id,
                outcome,
            });
        }));
    }

    fn submit(&self, request: OperationRequest, sink: OperationSink) {
        let operation = PendingOperation {
            request,
            sink,
            behavior: self.state.borrow().operation_behavior.clone(),
            started: false,
            completed: 0,
            next_sequence: 1,
        };
        self.schedule(Box::new(move |provider| {
            provider.run_operation_stage(operation)
        }));
    }
}

impl SearchRepository for MockProvider {
    fn search(&self, request: SearchRequest, done: SearchCallback) {
        let search = PendingSearch {
            request,
            done,
            initialized: false,
            matches: Vec::new(),
            accepted: BTreeSet::new(),
            visited: BTreeSet::new(),
            queue: VecDeque::new(),
            current: None,
            elapsed: Duration::ZERO,
            limits: vec![SearchLimit::Depth],
        };
        self.schedule(Box::new(move |provider| provider.run_search_stage(search)));
    }
}
