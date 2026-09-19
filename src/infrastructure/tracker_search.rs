//! URI search boundary. An installed index is not itself a usable client API:
//! builds without a Tracker client explicitly advertise indexed search as
//! unavailable. Filename fallback is local, metadata-only and deterministic.
use crate::core::{contracts::*, ports::*, FileItem, Location};
use gio::prelude::*;
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    time::Instant,
};

pub struct TrackerSearchRepository {
    context: glib::MainContext,
}

impl TrackerSearchRepository {
    pub fn new(context: glib::MainContext) -> Self {
        Self { context }
    }
}

impl SearchRepository for TrackerSearchRepository {
    fn search(&self, request: SearchRequest, done: SearchCallback) {
        self.context.spawn_local(async move {
            let worker = request.clone();
            let outcome = gio::spawn_blocking(move || fallback(&worker))
                .await
                .unwrap_or_else(|_| {
                    incomplete(
                        &request,
                        vec![],
                        SearchIncompleteReason::ProviderFailure,
                        None,
                    )
                });
            done(SearchCompletion {
                id: request.id,
                outcome,
            });
        });
    }
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
