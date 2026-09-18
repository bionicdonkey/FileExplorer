//! Network-free contract adapter driven by an explicit virtual clock.
//!
//! `advance` makes due work ready; iterate the supplied GLib main context to
//! deliver it. Each operation stage consumes at least one tick, so cancellation
//! between stages has deterministic acknowledgement and no wall-clock sleeps.
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

    fn search_outcome(&self, request: &SearchRequest) -> SearchOutcome {
        let incomplete = |matches, reason, error| SearchOutcome::Incomplete {
            matches,
            reason,
            declared_scope: request.scope.clone(),
            retry: RetryClass::AfterRefresh,
            error,
        };
        if request.cancellable.is_cancelled() {
            return incomplete(Vec::new(), SearchIncompleteReason::Cancelled, None);
        }
        let resolved = match self.resolve(&request.scope.root) {
            Ok(location) => location,
            Err(error) => {
                return incomplete(
                    Vec::new(),
                    SearchIncompleteReason::ProviderFailure,
                    Some(error),
                )
            }
        };
        let state = self.state.borrow();
        let unsupported = || SearchOutcome::Unsupported {
            declared_scope: request.scope.clone(),
            error: OperationError::new(
                OperationErrorKind::Unsupported,
                Some(&request.scope.root),
                None,
            ),
        };
        let Some(root) = state.directories.get(&resolved) else {
            return incomplete(
                Vec::new(),
                SearchIncompleteReason::ProviderFailure,
                Some(OperationError::new(
                    OperationErrorKind::NotFound,
                    Some(&request.scope.root),
                    None,
                )),
            );
        };
        // This mock implements bounded filename search only; content is never
        // inferred from filenames or simulated by reading file contents.
        if request.mode != SearchMode::Filename
            || !root.capabilities.supports(Capability::SearchFilename)
            || !root.capabilities.stable_order
        {
            return unsupported();
        }
        let mut matches = Vec::new();
        let mut accepted = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let mut queue = VecDeque::from([(resolved, 0)]);
        let mut elapsed = Duration::ZERO;
        let mut limits = vec![SearchLimit::Depth];
        while let Some((directory, depth)) = queue.pop_front() {
            if !visited.insert(directory.uri().to_owned()) || depth >= request.scope.max_depth {
                continue;
            }
            let Some(fixture) = state.directories.get(&directory) else {
                return incomplete(
                    matches,
                    SearchIncompleteReason::ProviderFailure,
                    Some(OperationError::new(
                        OperationErrorKind::NotFound,
                        Some(&directory),
                        None,
                    )),
                );
            };
            if !fixture.capabilities.stable_order {
                return incomplete(matches, SearchIncompleteReason::MalformedPage, None);
            }
            if !fixture.capabilities.supports(Capability::List) {
                return incomplete(
                    matches,
                    SearchIncompleteReason::AccessLost,
                    Some(OperationError::new(
                        OperationErrorKind::PermissionDenied,
                        Some(&directory),
                        None,
                    )),
                );
            }
            for entry in &fixture.entries {
                if let MockSearchBehavior::FailAfter {
                    matches: count,
                    reason,
                } = state.search_behavior
                {
                    if matches.len() == count {
                        return incomplete(matches, reason, None);
                    }
                }
                if matches.len() >= request.scope.max_results {
                    limits.push(SearchLimit::ResultCount);
                    return search_completed(request, matches, limits);
                }
                elapsed = elapsed.saturating_add(state.config.search_entry_time);
                if elapsed >= request.scope.timeout {
                    limits.push(SearchLimit::Time);
                    return search_completed(request, matches, limits);
                }
                if entry.display_name.contains(&request.query)
                    && accepted.insert(entry.location.uri().to_owned())
                {
                    matches.push(entry.clone());
                }
                if entry.kind == EntryKind::Directory {
                    queue.push_back((entry.location.clone(), depth + 1));
                }
            }
        }
        search_completed(request, matches, limits)
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
    fn list(&self, request: DirectoryRequest, done: DirectoryCallback) {
        self.schedule(Box::new(move |provider| {
            let outcome = if request.cancellable.is_cancelled() {
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
        self.schedule(Box::new(move |provider| {
            done(SearchCompletion {
                id: request.id,
                outcome: provider.search_outcome(&request),
            })
        }));
    }
}
