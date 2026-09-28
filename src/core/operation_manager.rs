use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::application::operations::{
    OperationCoordinator, OperationProjection, OperationState as ProjectedState,
};
use crate::core::contracts::{ConflictDecision, MutationKind, OperationId, OperationRequest};
use crate::core::{Location, RecoverableStateNotice};
use crate::infrastructure::gio_executor::GioExecutor;
use crate::infrastructure::gio_operations::GioOperations;
use serde::{Deserialize, Serialize};
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationSummary {
    pub completed: Vec<String>,
    pub remaining: Vec<String>,
    pub effect: String,
    pub error_kind: Option<String>,
    pub retry_of: Option<u64>,
    pub explicit_target: Option<String>,
}
/// Plain data sent to the detached operation coordinator worker. Native GIO
/// values are deliberately reconstructed only after the worker owns them.
struct Submission {
    request: OperationRequestData,
    confirmed_delete: bool,
    decision: Option<ConflictDecision>,
    retry_of: Option<OperationId>,
}

struct OperationRequestData {
    id: OperationId,
    kind: MutationKind,
    sources: Vec<String>,
    destination: Option<String>,
    explicit_target: Option<String>,
    capability_version: u64,
}

impl Submission {
    fn from_request(
        request: OperationRequest,
        confirmed_delete: bool,
        decision: Option<ConflictDecision>,
        retry_of: Option<OperationId>,
    ) -> Self {
        Self {
            request: OperationRequestData::from(request),
            confirmed_delete,
            decision,
            retry_of,
        }
    }
}

impl From<OperationRequest> for OperationRequestData {
    fn from(request: OperationRequest) -> Self {
        Self {
            id: request.id,
            kind: request.kind,
            sources: request
                .sources
                .into_iter()
                .map(|location| location.uri().to_owned())
                .collect(),
            destination: request
                .destination
                .map(|location| location.uri().to_owned()),
            explicit_target: request
                .explicit_target
                .map(|location| location.uri().to_owned()),
            capability_version: request.capability_version,
        }
    }
}

impl OperationRequestData {
    fn into_request(self) -> OperationRequest {
        OperationRequest {
            id: self.id,
            kind: self.kind,
            sources: self
                .sources
                .into_iter()
                .map(|uri| Location::from_uri(&uri).expect("validated operation URI"))
                .collect(),
            destination: self
                .destination
                .map(|uri| Location::from_uri(&uri).expect("validated operation destination URI")),
            explicit_target: self
                .explicit_target
                .map(|uri| Location::from_uri(&uri).expect("validated operation target URI")),
            capability_version: self.capability_version,
            cancellable: gio::Cancellable::new(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredOperationHistoryV2 {
    schema_version: u32,
    records: Vec<StoredOperationRecordV2>,
}
#[derive(Serialize, Deserialize)]
struct StoredOperationRecordV2 {
    #[serde(default)]
    outcome: Option<OperationSummary>,
    id: u64,
    kind: OperationKind,
    sources: Vec<String>,
    destination: Option<String>,
    completed: usize,
    total: usize,
    state: OperationState,
    finished_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationKind {
    Copy,
    Move,
    Create,
    Rename,
    Trash,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationState {
    Queued,
    Running,
    Completed,
    CancelRequested,
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationRecord {
    pub id: u64,
    pub kind: OperationKind,
    /// Canonical identities retained for history, including nonlocal sources.
    #[serde(default, skip_serializing)]
    pub source_uris: Vec<String>,
    pub sources: Vec<PathBuf>,
    /// Canonical identity retained for a nonlocal destination.
    #[serde(default, skip_serializing)]
    pub destination_uri: Option<String>,
    pub destination: Option<PathBuf>,
    pub completed: usize,
    pub total: usize,
    pub state: OperationState,
    #[serde(default)]
    pub finished_at: Option<i64>,
}

#[derive(Clone)]
pub struct OperationManager {
    records: Arc<Mutex<Vec<OperationRecord>>>,
    next_id: Arc<AtomicU64>,
    cancelled: Arc<Mutex<Vec<(u64, Arc<AtomicBool>)>>>,
    history_path: Option<Arc<PathBuf>>,
    worker: Arc<Mutex<Option<Sender<Submission>>>>,
    projections: Arc<Mutex<Vec<OperationProjection>>>,
    summaries: Arc<Mutex<std::collections::HashMap<u64, OperationSummary>>>,
    persistence: Arc<Mutex<()>>,
    retention_days: Arc<AtomicU64>,
    recoverable_notices: Arc<Mutex<Vec<RecoverableStateNotice>>>,
    executor: Arc<GioExecutor>,
}

impl Default for OperationManager {
    fn default() -> Self {
        Self {
            records: Arc::new(Mutex::new(Vec::new())),
            next_id: Arc::new(AtomicU64::new(0)),
            cancelled: Arc::new(Mutex::new(Vec::new())),
            history_path: None,
            worker: Arc::new(Mutex::new(None)),
            projections: Arc::new(Mutex::new(Vec::new())),
            summaries: Default::default(),
            persistence: Default::default(),
            retention_days: Arc::new(AtomicU64::new(7)),
            recoverable_notices: Arc::new(Mutex::new(Vec::new())),
            executor: GioExecutor::shared(),
        }
    }
}

impl OperationManager {
    pub fn load_history(path: Option<PathBuf>, retention_days: u32) -> Self {
        let manager = Self {
            history_path: path.clone().map(Arc::new),
            retention_days: Arc::new(AtomicU64::new(retention_days as u64)),
            ..Self::default()
        };
        if let Some(path) = path {
            if let Ok(bytes) = crate::core::application_preferences::read_bounded_state(&path) {
                let redacted_history = serde_json::from_slice::<serde_json::Value>(&bytes)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("schema_version")
                            .and_then(serde_json::Value::as_u64)
                    })
                    == Some(4);
                let (records, mut notices) = decode_history(&bytes);
                if let Ok(stored) = serde_json::from_slice::<StoredOperationHistoryV2>(&bytes) {
                    for record in stored.records {
                        if !records.iter().any(|r| r.id == record.id) {
                            continue;
                        }
                        if let Some(mut outcome) = record.outcome {
                            if redacted_history {
                                manager.summaries.lock().unwrap().insert(record.id, outcome);
                                continue;
                            }
                            let mut valid = true;
                            for uri in outcome
                                .completed
                                .iter_mut()
                                .chain(outcome.remaining.iter_mut())
                                .chain(outcome.explicit_target.iter_mut())
                            {
                                match Location::from_uri(uri) {
                                    Ok(location) => *uri = location.uri().to_owned(),
                                    Err(_) => valid = false,
                                }
                            }
                            if valid {
                                manager.summaries.lock().unwrap().insert(record.id, outcome);
                            } else {
                                notices.push(RecoverableStateNotice::MalformedRecord);
                            }
                        }
                    }
                }
                *manager.records.lock().unwrap() = records;
                *manager.recoverable_notices.lock().unwrap() = notices;
            } else if path.exists() {
                manager
                    .recoverable_notices
                    .lock()
                    .unwrap()
                    .push(RecoverableStateNotice::MalformedRecord);
            }
        }
        // next_id otherwise always restarts at 0, so the first operation of
        // every session would be id 1 -- colliding with whatever record
        // already has id 1 in the loaded history. update() finds the first
        // matching id, so every state write for the new operation would
        // silently land on the old, already-finished record instead: it
        // never appears to start, Cancel affects the wrong row, and the new
        // operation never gets a row of its own in the Operations page.
        let highest_loaded_id = manager
            .records
            .lock()
            .unwrap()
            .iter()
            .map(|record| record.id)
            .max()
            .unwrap_or(0);
        manager.next_id.store(highest_loaded_id, Ordering::Relaxed);
        manager.prune_history();
        manager
    }
    pub fn set_retention_days(&self, days: u32) {
        self.retention_days.store(days as u64, Ordering::Relaxed);
        self.prune_history();
    }
    pub fn submit(
        &self,
        kind: OperationKind,
        sources: Vec<PathBuf>,
        destination: Option<PathBuf>,
    ) -> u64 {
        self.submit_inner(kind, sources, destination, None, false, None)
    }

    /// URI entry point used by every interactive workflow. The coordinator
    /// rechecks provider capabilities immediately before dispatch.
    pub fn submit_locations(
        &self,
        kind: MutationKind,
        sources: Vec<Location>,
        destination: Option<Location>,
        explicit_target: Option<Location>,
        decision: Option<ConflictDecision>,
        confirmed_delete: bool,
    ) -> u64 {
        self.enqueue(Submission {
            request: OperationRequestData {
                id: OperationId(self.next_id.fetch_add(1, Ordering::Relaxed) + 1),
                kind,
                sources: sources
                    .into_iter()
                    .map(|location| location.uri().to_owned())
                    .collect(),
                destination: destination.map(|location| location.uri().to_owned()),
                explicit_target: explicit_target.map(|location| location.uri().to_owned()),
                capability_version: 1,
            },
            confirmed_delete,
            decision,
            retry_of: None,
        })
    }

    pub fn submit_to_target(&self, kind: OperationKind, source: PathBuf, target: PathBuf) -> u64 {
        self.submit_inner(
            kind,
            vec![source],
            target.parent().map(Path::to_path_buf),
            Some(target),
            false,
            None,
        )
    }

    pub fn submit_confirmed_delete(&self, sources: Vec<PathBuf>) -> u64 {
        self.submit_inner(OperationKind::Delete, sources, None, None, true, None)
    }
    pub fn submit_replacing(
        &self,
        kind: OperationKind,
        sources: Vec<PathBuf>,
        destination: PathBuf,
    ) -> u64 {
        self.submit_inner(
            kind,
            sources,
            Some(destination),
            None,
            false,
            Some(ConflictDecision::Replace),
        )
    }
    fn submit_inner(
        &self,
        kind: OperationKind,
        sources: Vec<PathBuf>,
        destination: Option<PathBuf>,
        explicit_target: Option<PathBuf>,
        confirmed_delete: bool,
        decision: Option<ConflictDecision>,
    ) -> u64 {
        self.enqueue(Submission {
            request: OperationRequestData {
                id: OperationId(self.next_id.fetch_add(1, Ordering::Relaxed) + 1),
                kind: match kind {
                    OperationKind::Copy => MutationKind::Copy,
                    OperationKind::Move => MutationKind::Move,
                    OperationKind::Create => MutationKind::Create,
                    OperationKind::Rename => MutationKind::Rename,
                    OperationKind::Trash => MutationKind::Trash,
                    OperationKind::Delete => MutationKind::Delete,
                },
                sources: sources
                    .iter()
                    .map(|path| Location::from_local_path(path).uri().to_owned())
                    .collect(),
                destination: destination
                    .as_ref()
                    .map(|path| Location::from_local_path(path).uri().to_owned()),
                explicit_target: explicit_target
                    .as_ref()
                    .map(|path| Location::from_local_path(path).uri().to_owned()),
                capability_version: 1,
            },
            confirmed_delete,
            decision,
            retry_of: None,
        })
    }
    fn enqueue(&self, submission: Submission) -> u64 {
        let request = &submission.request;
        let id = request.id.0;
        let kind = match request.kind {
            MutationKind::Copy => OperationKind::Copy,
            MutationKind::Move => OperationKind::Move,
            MutationKind::Rename => OperationKind::Rename,
            MutationKind::Trash => OperationKind::Trash,
            MutationKind::Delete => OperationKind::Delete,
            MutationKind::Create => OperationKind::Create,
        };
        self.records.lock().unwrap().push(OperationRecord {
            id,
            kind,
            source_uris: request.sources.clone(),
            sources: request
                .sources
                .iter()
                .filter_map(|uri| {
                    Location::from_uri(uri)
                        .ok()
                        .and_then(|location| location.local_path())
                })
                .collect(),
            destination_uri: request.destination.clone(),
            destination: request
                .destination
                .as_deref()
                .and_then(|uri| Location::from_uri(uri).ok())
                .and_then(|location| location.local_path()),
            completed: 0,
            total: request.sources.len().max(1),
            state: OperationState::Queued,
            finished_at: None,
        });
        self.cancelled
            .lock()
            .unwrap()
            .push((id, Arc::new(AtomicBool::new(false))));
        self.dispatch(submission);
        id
    }

    fn dispatch(&self, submission: Submission) {
        let mut worker = self.worker.lock().unwrap();
        let sender = worker.get_or_insert_with(|| {
            let (send, receive) = mpsc::channel::<Submission>();
            let manager = Self {
                worker: Arc::new(Mutex::new(None)),
                ..self.clone()
            };
            std::thread::spawn(move || {
                let context = glib::MainContext::new();
                let _guard = context.acquire().unwrap();
                let adapter = Rc::new(GioOperations::new_uri_with_executor(
                    context.clone(),
                    manager.executor.clone(),
                ));
                let coordinator = OperationCoordinator::new(adapter.clone());
                loop {
                    match receive.recv_timeout(std::time::Duration::from_millis(2)) {
                        Ok(submission) => {
                            let request = submission.request.into_request();
                            let id = request.id.0;
                            let result = if let Some(original) = submission.retry_of {
                                adapter.authorize_elevated_retry(request.id);
                                coordinator.submit_linked_retry(
                                    request,
                                    GioOperations::local_capabilities(),
                                    original,
                                )
                            } else if submission.confirmed_delete {
                                coordinator.submit_confirmed_delete(
                                    request,
                                    GioOperations::local_capabilities(),
                                )
                            } else if let Some(decision) = submission.decision {
                                coordinator.submit_with_decision(
                                    request,
                                    GioOperations::local_capabilities(),
                                    decision,
                                )
                            } else {
                                coordinator.submit(request, GioOperations::local_capabilities())
                            };
                            if let Err(error) = result {
                                update(&manager.records, id, |r| {
                                    r.state = OperationState::Failed(error.to_string());
                                    r.finished_at = Some(now());
                                });
                                manager.persist_history();
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            for p in coordinator.records() {
                                coordinator.cancel(p.id);
                            }
                            while !coordinator.active_lock_keys().is_empty() {
                                context.iteration(true);
                            }
                            manager.sync_projections(&coordinator);
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    for (id, flag) in manager.cancelled.lock().unwrap().iter() {
                        if flag.load(Ordering::Relaxed) {
                            coordinator.cancel(OperationId(*id));
                        }
                    }
                    while context.pending() {
                        context.iteration(false);
                    }
                    manager.sync_projections(&coordinator);
                }
            });
            send
        });
        let _ = sender.send(submission);
    }

    fn sync_projections(&self, coordinator: &OperationCoordinator) {
        let projections = coordinator.records();
        // A terminal facade record must never precede its event projection.
        *self.projections.lock().unwrap() = projections.clone();
        let mut terminal_changed = false;
        for p in &projections {
            if p.events.last().is_some_and(|e| e.is_terminal()) {
                self.summaries.lock().unwrap().insert(
                    p.id.0,
                    OperationSummary {
                        completed: p
                            .completion
                            .completed
                            .iter()
                            .map(|l| l.uri().to_owned())
                            .collect(),
                        remaining: p
                            .completion
                            .remaining
                            .iter()
                            .map(|l| l.uri().to_owned())
                            .collect(),
                        effect: format!("{:?}", p.change),
                        error_kind: if let ProjectedState::Failed(error) = &p.state {
                            Some(format!("{:?}", error.kind))
                        } else {
                            None
                        },
                        retry_of: p.retry_of.map(|id| id.0),
                        explicit_target: p
                            .request
                            .explicit_target
                            .as_ref()
                            .map(|l| l.uri().to_owned()),
                    },
                );
            }
            update(&self.records, p.id.0, |record| {
                let state = match &p.state {
                    ProjectedState::Queued => OperationState::Queued,
                    ProjectedState::Running => OperationState::Running,
                    ProjectedState::CancelRequested => OperationState::CancelRequested,
                    ProjectedState::Succeeded => OperationState::Completed,
                    ProjectedState::Failed(error) => OperationState::Failed(error.to_string()),
                    ProjectedState::Cancelled(_) => OperationState::Cancelled,
                };
                record.state = state;
                record.completed =
                    p.progress
                        .completed_bytes
                        .unwrap_or(p.progress.completed_items) as usize;
                record.total = p
                    .progress
                    .total_bytes
                    .or(p.progress.total_items)
                    .unwrap_or(record.total as u64) as usize;
                if p.events.last().is_some_and(|e| e.is_terminal()) && record.finished_at.is_none()
                {
                    record.finished_at = Some(now());
                    terminal_changed = true;
                }
            });
        }
        if terminal_changed {
            self.persist_history();
        }
    }

    pub fn projections(&self) -> Vec<OperationProjection> {
        self.projections.lock().unwrap().clone()
    }

    /// An explicit privileged retry creates a new, linked operation.
    pub fn retry_elevated(&self, id: u64) -> Result<(), String> {
        let original = self
            .projections()
            .into_iter()
            .find(|p| p.id.0 == id)
            .ok_or_else(|| "operation record not eligible".to_string())?;
        let new_id = OperationId(self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let request = crate::application::operations::elevated_retry_request(&original, new_id)
            .map_err(|e| e.to_string())?;
        self.enqueue(Submission::from_request(
            request,
            false,
            None,
            Some(original.id),
        ));
        Ok(())
    }
    pub fn summary(&self, id: u64) -> Option<OperationSummary> {
        self.summaries.lock().unwrap().get(&id).cloned()
    }
    pub fn can_retry_elevated(&self, id: u64) -> bool {
        self.projections()
            .iter()
            .find(|p| p.id.0 == id)
            .is_some_and(|p| {
                crate::application::operations::elevated_retry_request(
                    p,
                    OperationId(self.next_id.load(Ordering::Relaxed).saturating_add(1)),
                )
                .is_ok()
            })
    }

    pub fn records(&self) -> Vec<OperationRecord> {
        self.records.lock().unwrap().clone()
    }
    pub fn recoverable_notices(&self) -> Vec<RecoverableStateNotice> {
        self.recoverable_notices.lock().unwrap().clone()
    }
    pub fn active_count(&self) -> usize {
        self.records()
            .iter()
            .filter(|record| {
                matches!(
                    record.state,
                    OperationState::Queued
                        | OperationState::Running
                        | OperationState::CancelRequested
                )
            })
            .count()
    }
    /// Records cancellation intent. Only backend acknowledgement is terminal.
    pub fn cancel(&self, id: u64) {
        if let Some((_, flag)) = self
            .cancelled
            .lock()
            .unwrap()
            .iter()
            .find(|(known, _)| *known == id)
        {
            flag.store(true, Ordering::Relaxed);
        }
        update(&self.records, id, |record| {
            if matches!(
                record.state,
                OperationState::Queued | OperationState::Running | OperationState::CancelRequested
            ) {
                record.state = OperationState::CancelRequested;
            }
        });
    }
    /// Signals every currently active record to stop. Cancellation still
    /// happens on each operation's own background thread, so callers that
    /// need to close the window immediately should not wait on this.
    pub fn cancel_all_active(&self) {
        for (_, flag) in self.cancelled.lock().unwrap().iter() {
            flag.store(true, Ordering::Relaxed);
        }
    }
    pub fn clear_completed(&self) {
        self.records.lock().unwrap().retain(|record| {
            matches!(
                record.state,
                OperationState::Queued | OperationState::Running | OperationState::CancelRequested
            )
        });
        self.persist_history();
    }
    fn prune_history(&self) {
        let cutoff = now() - self.retention_days.load(Ordering::Relaxed) as i64 * 86_400;
        self.records.lock().unwrap().retain(|record| {
            matches!(
                record.state,
                OperationState::Queued | OperationState::Running | OperationState::CancelRequested
            ) || record
                .finished_at
                .is_some_and(|finished| finished >= cutoff)
        });
        self.persist_history();
    }
    fn persist_history(&self) {
        let _guard = self.persistence.lock().unwrap();
        // Active jobs are never evicted. All terminal projections and their
        // associated payloads share the same bounded lifetime as history rows.
        let retained: std::collections::HashSet<_> = {
            let mut records = self.records.lock().unwrap();
            let mut remaining = crate::core::application_preferences::MAX_HISTORY_RECORDS;
            records.reverse();
            records.retain(|record| {
                if record.finished_at.is_none() {
                    return true;
                }
                if remaining == 0 {
                    return false;
                }
                remaining -= 1;
                true
            });
            records.reverse();
            records.iter().map(|record| record.id).collect()
        };
        self.projections
            .lock()
            .unwrap()
            .retain(|projection| retained.contains(&projection.id.0));
        self.summaries
            .lock()
            .unwrap()
            .retain(|id, _| retained.contains(id));
        self.cancelled
            .lock()
            .unwrap()
            .retain(|(id, _)| retained.contains(id));
        let Some(path) = &self.history_path else {
            return;
        };
        let records: Vec<_> = self
            .records()
            .into_iter()
            .filter(|record| {
                !matches!(
                    record.state,
                    OperationState::Queued
                        | OperationState::Running
                        | OperationState::CancelRequested
                ) && record.finished_at.is_some_and(|finished| {
                    finished >= now() - self.retention_days.load(Ordering::Relaxed) as i64 * 86_400
                })
            })
            .collect();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&StoredOperationHistoryV2 {
            schema_version: 4,
            records: records
                .iter()
                .map(|record| {
                    let mut stored = stored_record(record);
                    stored.outcome = self.summary(record.id).map(redacted_summary);
                    stored
                })
                .collect(),
        }) {
            let _ = crate::core::application_preferences::write_bounded_state(path, &bytes);
        }
    }
}

fn stored_record(record: &OperationRecord) -> StoredOperationRecordV2 {
    StoredOperationRecordV2 {
        outcome: None,
        id: record.id,
        kind: record.kind,
        sources: if record.source_uris.is_empty() {
            record
                .sources
                .iter()
                .map(|path| Location::from_local_path(path).redacted())
                .collect()
        } else {
            record
                .source_uris
                .iter()
                .map(|uri| redacted_location(uri))
                .collect()
        },
        destination: record
            .destination_uri
            .clone()
            .or_else(|| {
                record
                    .destination
                    .as_ref()
                    .map(|path| Location::from_local_path(path).redacted())
            })
            .map(|uri| redacted_location(&uri)),
        completed: record.completed,
        total: record.total,
        state: redacted_state(&record.state),
        finished_at: record.finished_at,
    }
}

fn redacted_state(state: &OperationState) -> OperationState {
    match state {
        OperationState::Failed(_) => OperationState::Failed("Operation failed".to_owned()),
        state => state.clone(),
    }
}

fn redacted_location(uri: &str) -> String {
    Location::from_uri(uri)
        .map(|location| location.redacted())
        .unwrap_or_else(|_| "location".to_owned())
}

fn redacted_summary(summary: OperationSummary) -> OperationSummary {
    OperationSummary {
        completed: summary
            .completed
            .iter()
            .map(|uri| redacted_location(uri))
            .collect(),
        remaining: summary
            .remaining
            .iter()
            .map(|uri| redacted_location(uri))
            .collect(),
        effect: summary.effect,
        error_kind: summary.error_kind,
        retry_of: summary.retry_of,
        explicit_target: summary.explicit_target.as_deref().map(redacted_location),
    }
}

fn decode_history(bytes: &[u8]) -> (Vec<OperationRecord>, Vec<RecoverableStateNotice>) {
    if bytes.len() > crate::core::application_preferences::MAX_STATE_BYTES {
        return (Vec::new(), vec![RecoverableStateNotice::MalformedRecord]);
    }
    if let Ok(mut records) = serde_json::from_slice::<Vec<OperationRecord>>(bytes) {
        records.retain(|r| {
            !matches!(
                r.state,
                OperationState::Queued | OperationState::Running | OperationState::CancelRequested
            )
        });
        for record in &mut records {
            if record.source_uris.is_empty() {
                record.source_uris = record
                    .sources
                    .iter()
                    .map(|path| Location::from_local_path(path).uri().to_owned())
                    .collect();
            }
            if record.destination_uri.is_none() {
                record.destination_uri = record
                    .destination
                    .as_ref()
                    .map(|path| Location::from_local_path(path).uri().to_owned());
            }
        }
        return (records, Vec::new());
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return (Vec::new(), vec![RecoverableStateNotice::MalformedRecord]);
    };
    let Some(object) = value.as_object() else {
        return (Vec::new(), vec![RecoverableStateNotice::MalformedRecord]);
    };
    let schema_version = object
        .get("schema_version")
        .and_then(serde_json::Value::as_u64);
    if !matches!(schema_version, Some(2 | 3 | 4)) {
        return (Vec::new(), vec![RecoverableStateNotice::UnsupportedSchema]);
    }
    let Some(stored_records) = object.get("records").and_then(serde_json::Value::as_array) else {
        return (Vec::new(), vec![RecoverableStateNotice::MalformedRecord]);
    };
    let mut records = Vec::new();
    let mut notices = Vec::new();
    let redacted_history = schema_version == Some(4);
    for value in stored_records {
        let Ok(record) = serde_json::from_value::<StoredOperationRecordV2>(value.clone()) else {
            notices.push(RecoverableStateNotice::MalformedRecord);
            continue;
        };
        if matches!(
            record.state,
            OperationState::Queued | OperationState::Running | OperationState::CancelRequested
        ) {
            continue;
        }
        if redacted_history {
            if record.sources.is_empty() {
                notices.push(RecoverableStateNotice::MalformedRecord);
                continue;
            }
            records.push(OperationRecord {
                id: record.id,
                kind: record.kind,
                source_uris: record.sources,
                sources: Vec::new(),
                destination_uri: record.destination,
                destination: None,
                completed: record.completed,
                total: record.total,
                state: record.state,
                finished_at: record.finished_at,
            });
            continue;
        }
        let source_locations = record
            .sources
            .iter()
            .map(|uri| Location::from_uri(uri))
            .collect::<Result<Vec<_>, _>>();
        let Ok(source_locations) = source_locations else {
            notices.push(RecoverableStateNotice::MalformedRecord);
            continue;
        };
        if source_locations.is_empty() {
            notices.push(RecoverableStateNotice::MalformedRecord);
            continue;
        }
        let destination_location = record
            .destination
            .as_deref()
            .map(Location::from_uri)
            .transpose();
        let Ok(destination_location) = destination_location else {
            notices.push(RecoverableStateNotice::MalformedRecord);
            continue;
        };
        let source_uris = source_locations
            .iter()
            .map(|location| location.uri().to_owned())
            .collect::<Vec<_>>();
        records.push(OperationRecord {
            id: record.id,
            kind: record.kind,
            source_uris,
            sources: source_locations
                .iter()
                .filter_map(Location::local_path)
                .collect(),
            destination_uri: destination_location
                .as_ref()
                .map(|location| location.uri().to_owned()),
            destination: destination_location.and_then(|location| location.local_path()),
            completed: record.completed,
            total: record.total,
            state: record.state,
            finished_at: record.finished_at,
        });
    }
    (records, notices)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn update(
    records: &Arc<Mutex<Vec<OperationRecord>>>,
    id: u64,
    change: impl FnOnce(&mut OperationRecord),
) {
    if let Some(record) = records
        .lock()
        .unwrap()
        .iter_mut()
        .find(|record| record.id == id)
    {
        change(record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_submission_reconstructs_native_request_only_on_worker() {
        let submission = OperationRequestData {
            id: OperationId(9),
            kind: MutationKind::Copy,
            sources: vec!["file:///tmp/source".to_owned()],
            destination: Some("file:///tmp/destination".to_owned()),
            explicit_target: None,
            capability_version: 3,
        };
        let (sent, received) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let request = submission.into_request();
            sent.send((
                request.id,
                request.sources[0].uri().to_owned(),
                request.destination.unwrap().uri().to_owned(),
                request.capability_version,
            ))
            .unwrap();
        })
        .join()
        .unwrap();

        assert_eq!(
            received.recv().unwrap(),
            (
                OperationId(9),
                "file:///tmp/source".to_owned(),
                "file:///tmp/destination".to_owned(),
                3,
            )
        );
    }
    #[test]
    fn create_and_rename_facades_submit_uri_operations_with_terminal_events() {
        let directory = tempfile::tempdir().unwrap();
        let manager = OperationManager::default();
        let created = crate::core::operations::FileOperations::create_folder(
            &manager,
            directory.path(),
            "created",
        )
        .unwrap();
        let renamed = crate::core::operations::FileOperations::rename(
            &manager,
            &directory.path().join("created"),
            "renamed",
        )
        .unwrap();
        assert_eq!(
            wait_for_finished(&manager, created).state,
            OperationState::Completed
        );
        assert_eq!(
            wait_for_finished(&manager, renamed).state,
            OperationState::Completed
        );
        assert!(!directory.path().join("created").exists());
        assert!(directory.path().join("renamed").is_dir());
        let projections = manager.projections();
        let create = projections.iter().find(|p| p.id.0 == created).unwrap();
        let rename = projections.iter().find(|p| p.id.0 == renamed).unwrap();
        assert_eq!(create.request.kind, MutationKind::Create);
        assert_eq!(rename.request.kind, MutationKind::Rename);
        assert_eq!(
            create.request.sources,
            [Location::from_local_path(&directory.path().join("created"))]
        );
        assert_eq!(
            rename.request.explicit_target,
            Some(Location::from_local_path(&directory.path().join("renamed")))
        );
        for projection in [create, rename] {
            assert_eq!(
                projection.events.iter().filter(|e| e.is_terminal()).count(),
                1
            );
            assert!(projection
                .events
                .iter()
                .any(|e| matches!(e, crate::core::contracts::OperationEvent::Started { .. })));
        }
    }
    #[test]
    fn active_count_only_includes_running_operations() {
        let manager = OperationManager::default();
        assert_eq!(manager.active_count(), 0);
    }

    #[test]
    fn a_new_operation_never_collides_with_an_id_from_loaded_history() {
        // Regression guard: next_id always restarted at 0, so the first
        // operation of a fresh session got id 1 -- the same id almost every
        // previous session's first operation also used. update() finds the
        // *first* record with a matching id, so every write for the new
        // operation silently landed on the old, already-finished one:
        // it never appeared to start, Cancel hit the wrong row, and it
        // never got a row of its own in the Operations page.
        let directory = tempfile::tempdir().unwrap();
        let history_path = directory.path().join("history.json");
        let previous_session = vec![OperationRecord {
            id: 1,
            kind: OperationKind::Copy,
            source_uris: Vec::new(),
            sources: vec![PathBuf::from("/tmp/old.zip")],
            destination_uri: None,
            destination: Some(PathBuf::from("/tmp")),
            completed: 10,
            total: 10,
            state: OperationState::Completed,
            finished_at: Some(now()),
        }];
        std::fs::write(
            &history_path,
            serde_json::to_vec(&previous_session).unwrap(),
        )
        .unwrap();

        let manager = OperationManager::load_history(Some(history_path), 7);
        let new_id = manager.submit(
            OperationKind::Copy,
            vec![PathBuf::from("/tmp/new.zip")],
            Some(PathBuf::from("/tmp")),
        );

        assert_ne!(
            new_id, 1,
            "new operation reused an id already present in loaded history"
        );
        let record = wait_for_finished(&manager, new_id);
        assert_eq!(record.sources, vec![PathBuf::from("/tmp/new.zip")]);
        // The old record must be untouched by the new operation's updates.
        let old_record = manager
            .records()
            .into_iter()
            .find(|record| record.id == 1)
            .unwrap();
        assert_eq!(old_record.sources, vec![PathBuf::from("/tmp/old.zip")]);
        assert_eq!(old_record.completed, 10);
    }

    #[test]
    fn cancel_records_intent_without_claiming_backend_completion() {
        let manager = OperationManager::default();
        manager.records.lock().unwrap().push(OperationRecord {
            id: 1,
            kind: OperationKind::Copy,
            source_uris: Vec::new(),
            sources: vec![PathBuf::from("/tmp/big.bin")],
            destination_uri: None,
            destination: Some(PathBuf::from("/tmp/dest")),
            completed: 0,
            total: 100,
            state: OperationState::Queued,
            finished_at: None,
        });

        manager.cancel(1);

        let record = manager
            .records()
            .into_iter()
            .find(|record| record.id == 1)
            .unwrap();
        assert_ne!(record.state, OperationState::Cancelled);
        assert!(record.finished_at.is_none());
        assert_eq!(manager.active_count(), 1);
    }

    #[test]
    fn retrying_an_unknown_record_is_rejected() {
        let manager = OperationManager::default();
        assert!(manager.retry_elevated(404).is_err());
    }

    #[test]
    fn persisted_history_redacts_record_and_outcome_locations_without_changing_live_summary() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("private-source-name");
        let target = directory.path().join("private-target-name");
        fs::write(&source, "keep").unwrap();
        fs::create_dir(&target).unwrap();
        let path = directory.path().join("history.json");
        let manager = OperationManager::load_history(Some(path.clone()), 7);
        let id = manager.submit(OperationKind::Delete, vec![source.clone()], None);
        let record = wait_for_finished(&manager, id);
        assert!(matches!(record.state, OperationState::Failed(_)));
        assert!(source.exists());
        let copied = manager.submit(
            OperationKind::Copy,
            vec![source.clone()],
            Some(target.clone()),
        );
        wait_for_finished(&manager, copied);
        manager.persist_history();
        let raw_history = String::from_utf8(fs::read(&path).unwrap()).unwrap();
        let stored: serde_json::Value = serde_json::from_str(&raw_history).unwrap();
        assert_eq!(stored["schema_version"], 4);
        assert_eq!(
            stored["records"][1]["outcome"]["completed"],
            serde_json::json!(["file:///…"])
        );
        assert_eq!(stored["records"][1]["outcome"]["effect"], "Complete");
        let source_path = source.display().to_string();
        let target_path = target.display().to_string();
        for sensitive in [
            &source_path,
            &target_path,
            "private-source-name",
            "private-target-name",
        ] {
            assert!(
                !raw_history.contains(sensitive),
                "persisted history leaked {sensitive}"
            );
        }
        assert_eq!(
            manager.summary(copied).unwrap().completed,
            vec![Location::from_local_path(&source).uri().to_owned()]
        );
        let loaded = OperationManager::load_history(Some(path.clone()), 7);
        assert_eq!(loaded.summary(copied).unwrap().completed, vec!["file:///…"]);
    }

    #[test]
    fn history_rejects_unsafe_outcome_uris_and_does_not_restore_active_jobs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        fs::write(&path, br#"{"schema_version":3,"records":[{"id":1,"kind":"Copy","sources":["file:///a"],"destination":null,"completed":1,"total":1,"state":"Completed","finished_at":2000000000,"outcome":{"completed":["sftp://name:secret@host/item"],"remaining":[],"effect":"Complete","error_kind":null,"retry_of":null,"explicit_target":null}},{"id":2,"kind":"Copy","sources":["file:///b"],"destination":null,"completed":0,"total":1,"state":"Running","finished_at":2000000000}]}"#).unwrap();
        let manager = OperationManager::load_history(Some(path.clone()), 7);
        assert_eq!(manager.records().len(), 1);
        assert!(manager.summary(1).is_none());
        assert!(manager
            .recoverable_notices()
            .contains(&RecoverableStateNotice::MalformedRecord));
        assert!(!String::from_utf8(fs::read(path).unwrap())
            .unwrap()
            .contains("secret"));
    }

    #[test]
    fn retrying_a_non_permission_failure_is_rejected() {
        let manager = OperationManager::default();
        manager.records.lock().unwrap().push(OperationRecord {
            id: 1,
            kind: OperationKind::Delete,
            source_uris: Vec::new(),
            sources: vec![PathBuf::from("/tmp/report.txt")],
            destination_uri: None,
            destination: None,
            completed: 0,
            total: 1,
            state: OperationState::Failed("No such file or directory".into()),
            finished_at: None,
        });
        assert!(manager.retry_elevated(1).is_err());
    }

    #[test]
    fn history_writes_v4_redacted_locations_and_migrates_legacy_paths() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        let legacy = vec![OperationRecord {
            id: 3,
            kind: OperationKind::Copy,
            source_uris: Vec::new(),
            sources: vec![PathBuf::from("/tmp/source")],
            destination_uri: None,
            destination: Some(PathBuf::from("/tmp/destination")),
            completed: 1,
            total: 1,
            state: OperationState::Completed,
            finished_at: Some(now()),
        }];
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let manager = OperationManager::load_history(Some(path.clone()), 7);
        assert_eq!(
            manager.records()[0].sources,
            vec![PathBuf::from("/tmp/source")]
        );
        manager.persist_history();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["schema_version"], 4);
        assert_eq!(
            saved["records"][0]["sources"],
            serde_json::json!(["file:///…"])
        );
    }

    #[test]
    fn v2_history_keeps_remote_uris_and_skips_only_malformed_records() {
        let bytes = br#"{
            "schema_version": 2,
            "records": [
                {"id": 7, "kind": "Copy", "sources": ["smb://host/source"], "destination": "sftp://host/destination", "completed": 1, "total": 1, "state": "Completed", "finished_at": 1},
                {"id": "bad"},
                {"id": 8, "kind": "Delete", "sources": ["file:///tmp/ok"], "destination": null, "completed": 1, "total": 1, "state": "Completed", "finished_at": 1}
            ]
        }"#;

        let (records, notices) = decode_history(bytes);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].source_uris, ["smb://host/source"]);
        assert_eq!(
            records[0].destination_uri.as_deref(),
            Some("sftp://host/destination")
        );
        assert_eq!(records[1].source_uris, ["file:///tmp/ok"]);
        assert_eq!(notices, [RecoverableStateNotice::MalformedRecord]);
    }

    #[test]
    fn re_persisting_v2_history_redacts_remote_location_details() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        std::fs::write(
            &path,
            br#"{"schema_version":2,"records":[{"id":7,"kind":"Copy","sources":["SMB://HOST/source"],"destination":"sftp://host/destination","completed":1,"total":1,"state":"Completed","finished_at":2000000000}]}"#,
        )
        .unwrap();

        let manager = OperationManager::load_history(Some(path.clone()), 7);
        manager.persist_history();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();

        assert_eq!(
            saved["records"][0]["sources"],
            serde_json::json!(["smb://host/…"])
        );
        assert_eq!(saved["records"][0]["destination"], "sftp://host/…");
    }

    #[test]
    fn rewriting_history_redacts_raw_failed_state_details_from_every_supported_schema() {
        let directory = tempfile::tempdir().unwrap();
        for schema_version in [2, 3, 4] {
            let path = directory
                .path()
                .join(format!("history-{schema_version}.json"));
            std::fs::write(
                &path,
                format!(
                    r#"{{"schema_version":{schema_version},"records":[{{"id":7,"kind":"Copy","sources":["file:///private-source"],"destination":null,"completed":1,"total":1,"state":{{"Failed":"Unable to copy file:///private-source"}},"finished_at":2000000000}}]}}"#,
                ),
            )
            .unwrap();

            let manager = OperationManager::load_history(Some(path.clone()), 7);
            manager.persist_history();
            let raw_history = String::from_utf8(fs::read(&path).unwrap()).unwrap();
            let saved: serde_json::Value = serde_json::from_str(&raw_history).unwrap();

            assert_eq!(saved["schema_version"], 4);
            assert_eq!(
                saved["records"][0]["state"],
                serde_json::json!({"Failed": "Operation failed"})
            );
            assert!(
                !raw_history.contains("private-source"),
                "rewriting v{schema_version} history leaked failed-state detail"
            );
        }
    }

    #[test]
    fn cancel_all_active_signals_every_tracked_operation() {
        let manager = OperationManager::default();
        let flag_one = Arc::new(AtomicBool::new(false));
        let flag_two = Arc::new(AtomicBool::new(false));
        manager
            .cancelled
            .lock()
            .unwrap()
            .push((1, flag_one.clone()));
        manager
            .cancelled
            .lock()
            .unwrap()
            .push((2, flag_two.clone()));

        manager.cancel_all_active();

        assert!(flag_one.load(Ordering::Relaxed));
        assert!(flag_two.load(Ordering::Relaxed));
    }

    #[test]
    fn copying_many_files_is_observable_mid_transfer_through_records() {
        // Regression guard for "the progress bar doesn't update": poll
        // records() the way the Operations page does and confirm we can
        // observe completed strictly between 0 and total at least once,
        // not just an initial 0 followed immediately by "Completed".
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        for index in 0..2000 {
            fs::write(source.join(format!("file-{index}.txt")), "x").unwrap();
        }
        let manager = OperationManager::default();

        let id = manager.submit(OperationKind::Copy, vec![source], Some(destination));
        let mut saw_mid_transfer_progress = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if let Some(record) = manager.records().into_iter().find(|record| record.id == id) {
                if record.completed > 0 && record.completed < record.total {
                    saw_mid_transfer_progress = true;
                }
                if record.finished_at.is_some() {
                    break;
                }
            }
        }

        assert!(
            saw_mid_transfer_progress,
            "never observed an intermediate completed/total value while the copy was running"
        );
    }

    #[test]
    fn copying_a_folder_reports_byte_progress_not_per_top_level_source() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("a.txt"), "12").unwrap();
        fs::write(source.join("b.txt"), "345").unwrap();
        let manager = OperationManager::default();

        let id = manager.submit(OperationKind::Copy, vec![source], Some(destination));
        let record = wait_for_finished(&manager, id);

        assert_eq!(
            record.total, 5,
            "total should be bytes copied (2 + 3), not the single top-level source or file count"
        );
        assert_eq!(record.completed, 5);
        assert_eq!(record.state, OperationState::Completed);
    }

    #[test]
    fn copying_a_single_large_file_retains_backend_byte_progress_events() {
        // GIO can clone/reflink a file atomically; elapsed-time sampling cannot
        // require intermediate bytes that the backend never reported.
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("big.bin");
        let destination = directory.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, vec![0u8; 5 * (1 << 20)]).unwrap();
        let manager = OperationManager::default();
        let id = manager.submit(OperationKind::Copy, vec![source], Some(destination));
        let record = wait_for_finished(&manager, id);
        assert_eq!(record.completed, 5 * (1 << 20));
        assert_eq!(record.total, record.completed);
        assert_eq!(record.state, OperationState::Completed);
    }

    #[test]
    fn submitting_a_copy_does_not_block_on_counting_the_source_tree() {
        // count_bytes() must run on the background thread, not the caller's:
        // otherwise submitting a large folder copy blocks the GTK main loop
        // (reported as the app "freezing" / a transfer that never starts).
        // A few hundred files take long enough to count that a regression
        // here would make this test visibly slow, without making a passing
        // run flaky the way asserting on the placeholder value would.
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        for index in 0..500 {
            fs::write(source.join(format!("file-{index}.txt")), "x").unwrap();
        }
        let manager = OperationManager::default();

        let started = std::time::Instant::now();
        let id = manager.submit(OperationKind::Copy, vec![source], Some(destination));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "submit() should return immediately, not after walking the source tree"
        );

        wait_for_finished(&manager, id);
    }

    fn wait_for_finished(manager: &OperationManager, id: u64) -> OperationRecord {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(record) = manager.records().into_iter().find(|record| record.id == id) {
                if record.finished_at.is_some() {
                    return record;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "operation did not finish in time"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
