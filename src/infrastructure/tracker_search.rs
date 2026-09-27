//! URI search boundary. An installed index is not itself a usable client API:
//! builds without a Tracker client explicitly advertise indexed search as
//! unavailable. Filename fallback is local, metadata-only and deterministic.
use crate::{
    core::{contracts::*, ports::*, FileItem, Location},
    infrastructure::gio_executor::{CancellationSignal, GioExecutor, GioRequest},
};
use futures_util::StreamExt;
use gio::prelude::*;
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

pub struct TrackerSearchRepository {
    context: glib::MainContext,
    executor: Arc<GioExecutor>,
}

/// Search request data contains only owned values, so no caller-context GIO
/// objects cross to the executor.
#[derive(Clone)]
struct SearchRequestData {
    id: RequestId,
    scope: SearchScopeData,
    query: String,
    mode: SearchMode,
}

#[derive(Clone)]
struct SearchScopeData {
    root: String,
    max_depth: u32,
    max_results: usize,
    timeout: Duration,
}

impl From<&SearchRequest> for SearchRequestData {
    fn from(request: &SearchRequest) -> Self {
        Self {
            id: request.id,
            scope: (&request.scope).into(),
            query: request.query.clone(),
            mode: request.mode,
        }
    }
}

impl From<&SearchScope> for SearchScopeData {
    fn from(scope: &SearchScope) -> Self {
        Self {
            root: scope.root.uri().to_owned(),
            max_depth: scope.max_depth,
            max_results: scope.max_results,
            timeout: scope.timeout,
        }
    }
}

impl SearchScopeData {
    fn into_scope(self) -> SearchScope {
        SearchScope {
            root: location_from_uri(self.root),
            max_depth: self.max_depth,
            max_results: self.max_results,
            timeout: self.timeout,
        }
    }
}

impl SearchRequestData {
    fn into_native(self, cancellable: gio::Cancellable) -> SearchRequest {
        SearchRequest {
            id: self.id,
            scope: self.scope.into_scope(),
            query: self.query,
            mode: self.mode,
            cancellable,
        }
    }
}

#[derive(Clone)]
struct CapabilitySnapshotData {
    version: u64,
    supported: std::collections::BTreeSet<Capability>,
    stable_order: bool,
    containment: ContainmentData,
}

#[derive(Clone)]
enum ContainmentData {
    Opaque,
    KnownAncestors(Vec<String>),
}

impl From<CapabilitySnapshot> for CapabilitySnapshotData {
    fn from(snapshot: CapabilitySnapshot) -> Self {
        Self {
            version: snapshot.version,
            supported: snapshot.supported,
            stable_order: snapshot.stable_order,
            containment: match snapshot.containment {
                Containment::Opaque => ContainmentData::Opaque,
                Containment::KnownAncestors(locations) => ContainmentData::KnownAncestors(
                    locations
                        .into_iter()
                        .map(|location| location.uri().to_owned())
                        .collect(),
                ),
            },
        }
    }
}

impl CapabilitySnapshotData {
    fn into_snapshot(self) -> CapabilitySnapshot {
        CapabilitySnapshot {
            version: self.version,
            supported: self.supported,
            stable_order: self.stable_order,
            containment: match self.containment {
                ContainmentData::Opaque => Containment::Opaque,
                ContainmentData::KnownAncestors(locations) => Containment::KnownAncestors(
                    locations.into_iter().map(location_from_uri).collect(),
                ),
            },
        }
    }
}

#[derive(Clone)]
struct EntrySnapshotData {
    location: String,
    display_name: String,
    kind: EntryKind,
    size: Option<u64>,
    modified: Option<std::time::SystemTime>,
    content_type: Option<String>,
    capabilities: CapabilitySnapshotData,
}

impl From<EntrySnapshot> for EntrySnapshotData {
    fn from(snapshot: EntrySnapshot) -> Self {
        Self {
            location: snapshot.location.uri().to_owned(),
            display_name: snapshot.display_name,
            kind: snapshot.kind,
            size: snapshot.size,
            modified: snapshot.modified,
            content_type: snapshot.content_type,
            capabilities: snapshot.capabilities.into(),
        }
    }
}

impl EntrySnapshotData {
    fn into_snapshot(self) -> EntrySnapshot {
        EntrySnapshot {
            location: location_from_uri(self.location),
            display_name: self.display_name,
            kind: self.kind,
            size: self.size,
            modified: self.modified,
            content_type: self.content_type,
            capabilities: self.capabilities.into_snapshot(),
        }
    }
}

enum SearchOutcomeData {
    Completed {
        matches: Vec<EntrySnapshotData>,
        declared_scope: SearchScopeData,
        scope_limited: bool,
        limits: Vec<SearchLimit>,
    },
    Incomplete {
        matches: Vec<EntrySnapshotData>,
        reason: SearchIncompleteReason,
        declared_scope: SearchScopeData,
        retry: RetryClass,
        error: Option<OperationError>,
    },
    Unsupported {
        declared_scope: SearchScopeData,
        error: OperationError,
    },
}

impl From<SearchOutcome> for SearchOutcomeData {
    fn from(outcome: SearchOutcome) -> Self {
        match outcome {
            SearchOutcome::Completed {
                matches,
                declared_scope,
                scope_limited,
                limits,
            } => Self::Completed {
                matches: matches.into_iter().map(Into::into).collect(),
                declared_scope: (&declared_scope).into(),
                scope_limited,
                limits,
            },
            SearchOutcome::Incomplete {
                matches,
                reason,
                declared_scope,
                retry,
                error,
            } => Self::Incomplete {
                matches: matches.into_iter().map(Into::into).collect(),
                reason,
                declared_scope: (&declared_scope).into(),
                retry,
                error,
            },
            SearchOutcome::Unsupported {
                declared_scope,
                error,
            } => Self::Unsupported {
                declared_scope: (&declared_scope).into(),
                error,
            },
        }
    }
}

impl SearchOutcomeData {
    fn into_outcome(self) -> SearchOutcome {
        match self {
            Self::Completed {
                matches,
                declared_scope,
                scope_limited,
                limits,
            } => SearchOutcome::Completed {
                matches: matches
                    .into_iter()
                    .map(EntrySnapshotData::into_snapshot)
                    .collect(),
                declared_scope: declared_scope.into_scope(),
                scope_limited,
                limits,
            },
            Self::Incomplete {
                matches,
                reason,
                declared_scope,
                retry,
                error,
            } => SearchOutcome::Incomplete {
                matches: matches
                    .into_iter()
                    .map(EntrySnapshotData::into_snapshot)
                    .collect(),
                reason,
                declared_scope: declared_scope.into_scope(),
                retry,
                error,
            },
            Self::Unsupported {
                declared_scope,
                error,
            } => SearchOutcome::Unsupported {
                declared_scope: declared_scope.into_scope(),
                error,
            },
        }
    }

    fn executor_failure(scope: SearchScopeData) -> Self {
        Self::Incomplete {
            matches: vec![],
            reason: SearchIncompleteReason::ProviderFailure,
            declared_scope: scope,
            retry: RetryClass::AfterRefresh,
            error: None,
        }
    }
}

fn location_from_uri(uri: String) -> Location {
    Location::from_uri(&uri).expect("search URI was validated at the public boundary")
}

fn cancellation_signal(cancellable: &gio::Cancellable) -> CancellationSignal {
    let signal = CancellationSignal::new();
    if cancellable.is_cancelled() {
        signal.cancel();
    } else {
        let signal_for_callback = signal.clone();
        cancellable.connect_cancelled(move |_| signal_for_callback.cancel());
    }
    signal
}

impl TrackerSearchRepository {
    pub fn new(context: glib::MainContext) -> Self {
        Self::with_executor(context, GioExecutor::shared())
    }

    pub fn with_executor(context: glib::MainContext, executor: Arc<GioExecutor>) -> Self {
        Self { context, executor }
    }
}

impl SearchRepository for TrackerSearchRepository {
    fn search(&self, request: SearchRequest, done: SearchCallback) {
        let data = SearchRequestData::from(&request);
        let callback_id = data.id;
        let failure_scope = data.scope.clone();
        let cancellation = cancellation_signal(&request.cancellable);
        let owner_cancellable = request.cancellable.clone();
        let (send, mut receive) = futures_channel::mpsc::unbounded::<SearchOutcomeData>();
        let failure_send = send.clone();

        self.context.spawn_local(async move {
            // The caller owns this cancellable; its cancellation signal is all
            // the executor receives.
            let _owner_cancellable = owner_cancellable;
            if let Some(outcome) = receive.next().await {
                done(SearchCompletion {
                    id: callback_id,
                    outcome: outcome.into_outcome(),
                });
            }
        });

        if self
            .executor
            .submit(cancellation, move |native_request| {
                start_fallback(native_request, data, send);
            })
            .is_err()
        {
            let _ = failure_send.unbounded_send(SearchOutcomeData::executor_failure(failure_scope));
        }
    }
}

fn start_fallback(
    native_request: GioRequest,
    data: SearchRequestData,
    send: futures_channel::mpsc::UnboundedSender<SearchOutcomeData>,
) {
    let request = data.into_native(native_request.cancellable().clone());
    let mut outcome = fallback(&request);
    if native_request.synchronize_cancellation() {
        outcome = cancelled_outcome(&request, outcome);
    }
    let outcome = SearchOutcomeData::from(outcome);
    drop(request);
    native_request.complete();
    let _ = send.unbounded_send(outcome);
}

fn cancelled_outcome(request: &SearchRequest, outcome: SearchOutcome) -> SearchOutcome {
    let matches = match outcome {
        SearchOutcome::Completed { matches, .. } | SearchOutcome::Incomplete { matches, .. } => {
            matches
        }
        SearchOutcome::Unsupported { .. } => vec![],
    };
    incomplete(request, matches, SearchIncompleteReason::Cancelled, None)
}

fn incomplete(
    request: &SearchRequest,
    matches: Vec<EntrySnapshot>,
    reason: SearchIncompleteReason,
    error: Option<OperationError>,
) -> SearchOutcome {
    SearchOutcome::Incomplete {
        matches,
        reason,
        declared_scope: request.scope.clone(),
        retry: RetryClass::AfterRefresh,
        error,
    }
}

fn completed(request: &SearchRequest, matches: Vec<EntrySnapshot>) -> SearchOutcome {
    // Every declared bound is active even when traversal happened to finish early.
    SearchOutcome::Completed {
        matches,
        declared_scope: request.scope.clone(),
        scope_limited: true,
        limits: vec![
            SearchLimit::Depth,
            SearchLimit::ResultCount,
            SearchLimit::Time,
        ],
    }
}

fn fallback(request: &SearchRequest) -> SearchOutcome {
    let start = Instant::now();
    let mut matches = vec![];
    if request.cancellable.is_cancelled() {
        return incomplete(request, matches, SearchIncompleteReason::Cancelled, None);
    }
    if request.mode != SearchMode::Filename || request.scope.root.local_path().is_none() {
        return SearchOutcome::Unsupported {
            declared_scope: request.scope.clone(),
            error: OperationError::new(
                OperationErrorKind::Unsupported,
                Some(&request.scope.root),
                None,
            ),
        };
    }
    let mut queue = VecDeque::from([(request.scope.root.clone(), 0)]);
    let mut visited = HashSet::new();
    let mut accepted = HashSet::new();
    let needle = request.query.to_lowercase();
    while let Some((directory, depth)) = queue.pop_front() {
        if request.cancellable.is_cancelled() {
            return incomplete(request, matches, SearchIncompleteReason::Cancelled, None);
        }
        if start.elapsed() >= request.scope.timeout || matches.len() >= request.scope.max_results {
            return completed(request, matches);
        }
        if depth >= request.scope.max_depth || !visited.insert(directory.uri().to_owned()) {
            continue;
        }
        let file = gio::File::for_uri(directory.uri());
        let result = read_sorted(&file, request, start);
        let entries = match result {
            Ok(Some(entries)) => entries,
            Ok(None) => return completed(request, matches),
            Err(error) => {
                let reason = if request.cancellable.is_cancelled() {
                    SearchIncompleteReason::Cancelled
                } else if matches!(
                    error.kind,
                    OperationErrorKind::PermissionDenied | OperationErrorKind::MountUnavailable
                ) {
                    SearchIncompleteReason::AccessLost
                } else {
                    SearchIncompleteReason::ProviderFailure
                };
                return incomplete(request, matches, reason, Some(error));
            }
        };
        for (_, info) in entries {
            if request.cancellable.is_cancelled() {
                return incomplete(request, matches, SearchIncompleteReason::Cancelled, None);
            }
            if start.elapsed() >= request.scope.timeout
                || matches.len() >= request.scope.max_results
            {
                return completed(request, matches);
            }
            let item = FileItem::from_gio(&file, &info);
            let child = file.child(info.name());
            // Symlinks, shortcuts and mount references are leaves. A filesystem
            // boundary never resets depth or silently adds a new search root.
            if info.file_type() == gio::FileType::Directory
                && !info.is_symlink()
                && !info.boolean("unix::is-mountpoint")
            {
                queue.push_back((item.location().clone(), depth + 1));
            }
            if item.name().to_lowercase().contains(&needle)
                && accepted.insert(item.identity_key().to_owned())
            {
                let Ok(location) = Location::try_from_file(child) else {
                    return incomplete(
                        request,
                        matches,
                        SearchIncompleteReason::MalformedPage,
                        None,
                    );
                };
                let entry = EntrySnapshot {
                    location,
                    display_name: item.name().into(),
                    kind: if item.is_dir() {
                        EntryKind::Directory
                    } else {
                        EntryKind::File
                    },
                    size: info.has_attribute("standard::size").then(|| item.size()),
                    modified: info
                        .has_attribute("time::modified")
                        .then(|| item.modified()),
                    content_type: info.content_type().map(|s| s.to_string()),
                    capabilities: item.capabilities().clone(),
                };
                if request.cancellable.is_cancelled() {
                    return incomplete(request, matches, SearchIncompleteReason::Cancelled, None);
                }
                if start.elapsed() >= request.scope.timeout {
                    return completed(request, matches);
                }
                matches.push(entry);
            }
        }
    }
    if request.cancellable.is_cancelled() {
        incomplete(request, matches, SearchIncompleteReason::Cancelled, None)
    } else {
        completed(request, matches)
    }
}

/// GIO's native enumeration order is unstable. Buffer a directory's metadata in
/// canonical URI order before accepting anything from that directory. If the
/// deadline expires while buffering, no arbitrary prefix is returned.
fn read_sorted(
    file: &gio::File,
    request: &SearchRequest,
    start: Instant,
) -> Result<Option<BTreeMap<String, gio::FileInfo>>, OperationError> {
    let map_error =
        |error: glib::Error| OperationError::from_gio(&error, Some(&request.scope.root), None);
    let enumerator = file.enumerate_children(
        "standard::name,standard::display-name,standard::type,standard::is-symlink,standard::size,standard::content-type,time::modified,unix::is-mountpoint,access::*",
        gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, Some(&request.cancellable)).map_err(map_error)?;
    let result = (|| {
        let mut sorted = BTreeMap::new();
        loop {
            if request.cancellable.is_cancelled() {
                return Err(OperationError::new(
                    OperationErrorKind::Unknown,
                    Some(&request.scope.root),
                    None,
                ));
            }
            if start.elapsed() >= request.scope.timeout {
                return Ok(None);
            }
            let Some(info) = enumerator
                .next_file(Some(&request.cancellable))
                .map_err(map_error)?
            else {
                break;
            };
            sorted.insert(file.child(info.name()).uri().to_string(), info);
        }
        Ok(Some(sorted))
    })();
    let close = enumerator.close(gio::Cancellable::NONE).map_err(map_error);
    result.and_then(|entries| close.map(|_| entries))
}
