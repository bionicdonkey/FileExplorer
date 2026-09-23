//! Main-context operation scheduling. Only accepted events change projections.
use crate::core::{contracts::*, ports::FileOperations, Location};
use gio::prelude::*;
use std::{cell::RefCell, collections::BTreeSet, rc::Rc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationState {
    Queued,
    Running,
    CancelRequested,
    Succeeded,
    Failed(OperationError),
    Cancelled(CancellationOutcome),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeExtent {
    Unchanged,
    Complete,
    Partial,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelResult {
    Requested,
    CancelledBeforeStart,
    AlreadyTerminal,
    NotFound,
}
pub use crate::core::contracts::ConflictDecision;
#[derive(Clone, Debug)]
pub struct OperationProjection {
    pub id: OperationId,
    pub request: OperationRequest,
    pub state: OperationState,
    pub change: ChangeExtent,
    pub completion: OperationCompletion,
    pub progress: OperationProgress,
    pub events: Vec<OperationEvent>,
    pub retry_of: Option<OperationId>,
}
struct Record {
    projection: OperationProjection,
    reducer: OperationReducer,
    locks: Vec<String>,
    dispatched: bool,
    backend_sequence: Option<u64>,
    decision: Option<ConflictDecision>,
}
#[derive(Clone)]
pub struct OperationCoordinator {
    backend: Rc<dyn FileOperations>,
    records: Rc<RefCell<Vec<Record>>>,
}

fn invalid(request: &OperationRequest) -> OperationError {
    OperationError::new(
        OperationErrorKind::InvalidRequest,
        request.sources.first(),
        request.destination.as_ref(),
    )
}

pub fn elevated_retry_request(
    original: &OperationProjection,
    id: OperationId,
) -> Result<OperationRequest, OperationError> {
    let eligible = matches!(&original.state, OperationState::Failed(error) if error.kind == OperationErrorKind::PermissionDenied)
        && original.change == ChangeExtent::Unchanged
        && original.request.sources.len() == 1
        && id != original.id
        && original
            .request
            .sources
            .iter()
            .chain(original.request.destination.iter())
            .chain(original.request.explicit_target.iter())
            .all(|l| {
                l.uri().starts_with("file:")
                    && l.local_path()
                        .is_some_and(|p| !p.components().any(|c| c.as_os_str() == "gvfs"))
            });
    if !eligible {
        return Err(invalid(&original.request));
    }
    Ok(OperationRequest {
        id,
        cancellable: gio::Cancellable::new(),
        ..original.request.clone()
    })
}

/// GIO parent/child identities are used for local URIs only. Opaque providers
/// serialize globally; no path-prefix inference is made for remote locations.
pub fn lock_keys(
    request: &OperationRequest,
    capabilities: &CapabilitySnapshot,
) -> Result<Vec<String>, OperationError> {
    if request.sources.is_empty()
        || (request.explicit_target.is_some() && request.sources.len() != 1)
    {
        return Err(invalid(request));
    }
    let mut locations = request.sources.clone();
    if let Some(destination) = &request.destination {
        locations.push(destination.clone());
    }
    if let Some(target) = &request.explicit_target {
        locations.push(target.clone());
    }
    if matches!(
        request.kind,
        MutationKind::Copy | MutationKind::Move | MutationKind::Rename
    ) {
        if request.destination.is_none() && request.explicit_target.is_none() {
            return Err(invalid(request));
        }
        for target in request
            .destination
            .iter()
            .chain(request.explicit_target.iter())
        {
            validate_provider_target(request, target, capabilities)?;
        }
        for source in &request.sources {
            if let Some(destination) = &request.destination {
                if source.uri().starts_with("file:") && destination.uri().starts_with("file:") {
                    let from = gio::File::for_uri(source.uri());
                    let to = gio::File::for_uri(destination.uri());
                    if from.equal(&to) || to.has_prefix(&from) {
                        return Err(invalid(request));
                    }
                } else if source == destination {
                    return Err(invalid(request));
                }
                if request.explicit_target.is_none() {
                    let name = gio::File::for_uri(source.uri())
                        .basename()
                        .ok_or_else(|| invalid(request))?;
                    locations.push(
                        Location::try_from_file(gio::File::for_uri(destination.uri()).child(name))
                            .map_err(|_| invalid(request))?,
                    );
                }
            }
            if let Some(target) = &request.explicit_target {
                let from = gio::File::for_uri(source.uri());
                let to = gio::File::for_uri(target.uri());
                if source == target
                    || (source.uri().starts_with("file:")
                        && target.uri().starts_with("file:")
                        && to.has_prefix(&from))
                {
                    return Err(invalid(request));
                }
            }
        }
    }
    let mut keys = BTreeSet::new();
    for location in locations {
        keys.insert(location.uri().to_owned());
        if location.uri().starts_with("file:") {
            let mut parent = gio::File::for_uri(location.uri()).parent();
            while let Some(file) = parent {
                keys.insert(file.uri().to_string());
                parent = file.parent();
            }
        } else {
            keys.insert("opaque-provider:*".into());
        }
    }
    if let Containment::KnownAncestors(ancestors) = &capabilities.containment {
        keys.extend(ancestors.iter().map(|l| l.uri().to_owned()));
    }
    Ok(keys.into_iter().collect())
}

fn validate_provider_target(
    request: &OperationRequest,
    target: &Location,
    capabilities: &CapabilitySnapshot,
) -> Result<(), OperationError> {
    if request
        .sources
        .iter()
        .all(|source| source.uri().starts_with("file:"))
        && target.uri().starts_with("file:")
    {
        return Ok(());
    }
    // Without a trusted, complete ancestry snapshot, absence of a URI prefix
    // proves nothing: aliases and opaque provider identifiers may be descendants.
    match &capabilities.containment {
        Containment::KnownAncestors(ancestors)
            if request
                .sources
                .iter()
                .all(|source| source != target && !ancestors.contains(source)) =>
        {
            Ok(())
        }
        _ => Err(invalid(request)),
    }
}

impl OperationCoordinator {
    pub fn new(backend: Rc<dyn FileOperations>) -> Self {
        Self {
            backend,
            records: Rc::new(RefCell::new(Vec::new())),
        }
    }
    pub fn submit(
        &self,
        request: OperationRequest,
        capabilities: CapabilitySnapshot,
    ) -> Result<OperationId, OperationError> {
        if request.kind == MutationKind::Delete {
            return Err(invalid(&request));
        }
        self.submit_inner(request, capabilities, None, None)
    }
    /// The caller must have obtained explicit permanent-delete confirmation.
    pub fn submit_confirmed_delete(
        &self,
        request: OperationRequest,
        capabilities: CapabilitySnapshot,
    ) -> Result<OperationId, OperationError> {
        if request.kind != MutationKind::Delete {
            return Err(invalid(&request));
        }
        self.submit_inner(request, capabilities, None, None)
    }
    pub fn submit_linked_retry(
        &self,
        request: OperationRequest,
        capabilities: CapabilitySnapshot,
        original: OperationId,
    ) -> Result<OperationId, OperationError> {
        let prior = self
            .records()
            .into_iter()
            .find(|r| r.id == original)
            .ok_or_else(|| invalid(&request))?;
        let expected = elevated_retry_request(&prior, request.id)?;
        if expected.sources != request.sources
            || expected.destination != request.destination
            || expected.explicit_target != request.explicit_target
            || expected.kind != request.kind
        {
            return Err(invalid(&request));
        }
        self.submit_inner(request, capabilities, Some(original), None)
    }
    fn submit_inner(
        &self,
        request: OperationRequest,
        capabilities: CapabilitySnapshot,
        retry_of: Option<OperationId>,
        decision: Option<ConflictDecision>,
    ) -> Result<OperationId, OperationError> {
        let locks = lock_keys(&request, &capabilities)?;
        let id = request.id;
        if self.records.borrow().iter().any(|r| r.projection.id == id) {
            return Err(invalid(&request));
        }
        self.records.borrow_mut().push(Record {
            projection: OperationProjection {
                id,
                completion: OperationCompletion {
                    remaining: request.sources.clone(),
                    ..Default::default()
                },
                request,
                state: OperationState::Queued,
                change: ChangeExtent::Unchanged,
                progress: OperationProgress::default(),
                events: Vec::new(),
                retry_of,
            },
            reducer: OperationReducer::new(id),
            locks,
            dispatched: false,
            backend_sequence: None,
            decision,
        });
        if matches!(
            decision,
            Some(ConflictDecision::Skip | ConflictDecision::CancelRemaining)
        ) {
            let completion = self
                .records
                .borrow()
                .last()
                .unwrap()
                .projection
                .completion
                .clone();
            let event = if decision == Some(ConflictDecision::Skip) {
                OperationEvent::Succeeded {
                    id,
                    sequence: 0,
                    completion,
                }
            } else {
                OperationEvent::Cancelled {
                    id,
                    sequence: 0,
                    outcome: CancellationOutcome { completion },
                }
            };
            self.accept(event, false);
        } else {
            self.schedule();
        }
        Ok(id)
    }
    pub fn submit_with_decision(
        &self,
        request: OperationRequest,
        capabilities: CapabilitySnapshot,
        decision: ConflictDecision,
    ) -> Result<OperationId, OperationError> {
        if request.kind == MutationKind::Delete {
            return Err(invalid(&request));
        }
        self.submit_inner(request, capabilities, None, Some(decision))
    }
    pub fn records(&self) -> Vec<OperationProjection> {
        self.records
            .borrow()
            .iter()
            .map(|r| r.projection.clone())
            .collect()
    }
    pub fn active_lock_keys(&self) -> Vec<String> {
        self.records
            .borrow()
            .iter()
            .filter(|r| r.dispatched && !r.reducer.is_terminal())
            .flat_map(|r| r.locks.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    pub fn cancel(&self, id: OperationId) -> CancelResult {
        let (dispatched, cancel) = {
            let records = self.records.borrow();
            let Some(record) = records.iter().find(|r| r.projection.id == id) else {
                return CancelResult::NotFound;
            };
            if record.reducer.is_terminal() {
                return CancelResult::AlreadyTerminal;
            }
            if record.projection.state == OperationState::CancelRequested {
                return CancelResult::Requested;
            }
            (
                record.dispatched,
                record.projection.request.cancellable.clone(),
            )
        };
        cancel.cancel();
        if dispatched {
            self.accept(OperationEvent::CancelRequested { id, sequence: 0 }, false);
            CancelResult::Requested
        } else {
            let remaining = self
                .records
                .borrow()
                .iter()
                .find(|r| r.projection.id == id)
                .unwrap()
                .projection
                .request
                .sources
                .clone();
            self.accept(
                OperationEvent::Cancelled {
                    id,
                    sequence: 0,
                    outcome: CancellationOutcome {
                        completion: OperationCompletion {
                            remaining,
                            ..Default::default()
                        },
                    },
                },
                false,
            );
            CancelResult::CancelledBeforeStart
        }
    }
    fn schedule(&self) {
        let ready = {
            let mut records = self.records.borrow_mut();
            let mut occupied: BTreeSet<String> = records
                .iter()
                .filter(|r| r.dispatched && !r.reducer.is_terminal())
                .flat_map(|r| r.locks.iter().cloned())
                .collect();
            let mut ready = Vec::new();
            for record in records
                .iter_mut()
                .filter(|r| !r.dispatched && !r.reducer.is_terminal())
            {
                if record.locks.iter().any(|k| occupied.contains(k)) {
                    occupied.extend(record.locks.clone());
                    continue;
                }
                occupied.extend(record.locks.clone());
                record.dispatched = true;
                ready.push(record.projection.request.clone());
            }
            ready
        };
        for request in ready {
            self.inspect(request, 0);
        }
    }
    fn inspect(&self, request: OperationRequest, index: usize) {
        let locations: Vec<_> = request
            .sources
            .iter()
            .chain(request.destination.iter())
            .chain(request.explicit_target.iter())
            .cloned()
            .collect();
        if index == locations.len() {
            let owner = self.clone();
            let decision = self
                .records
                .borrow()
                .iter()
                .find(|r| r.projection.id == request.id)
                .unwrap()
                .decision;
            let expected_id = request.id;
            let sink = Box::new(move |event: OperationEvent| {
                if event.id() == expected_id {
                    owner.accept(event, true);
                }
            });
            if let Some(decision) = decision {
                self.backend.submit_with_decision(request, decision, sink);
            } else {
                self.backend.submit(request, sink);
            }
            return;
        }
        let owner = self.clone();
        let elevated = self
            .records
            .borrow()
            .iter()
            .find(|r| r.projection.id == request.id)
            .is_some_and(|r| r.projection.retry_of.is_some());
        let capability_request_id = RequestId::allocate();
        self.backend.inspect_operation_capabilities(
            CapabilityRequest {
                id: capability_request_id,
                location: locations[index].clone(),
                cancellable: request.cancellable.clone(),
            },
            if index < request.sources.len() {
                request.kind.capability()
            } else {
                Capability::Create
            },
            Box::new(move |result| {
                if result.id != capability_request_id {
                    return;
                }
                if index >= request.sources.len()
                    && matches!(
                        request.kind,
                        MutationKind::Copy | MutationKind::Move | MutationKind::Rename
                    )
                {
                    if let RequestOutcome::Succeeded(snapshot) = &result.outcome {
                        if let Err(error) =
                            validate_provider_target(&request, &locations[index], snapshot)
                        {
                            owner.accept(
                                OperationEvent::Failed {
                                    id: request.id,
                                    sequence: 0,
                                    error,
                                    completion: OperationCompletion {
                                        remaining: request.sources.clone(),
                                        ..Default::default()
                                    },
                                },
                                false,
                            );
                            return;
                        }
                    }
                }
                match result.outcome {
                    RequestOutcome::Succeeded(snapshot)
                        if snapshot.version == request.capability_version
                            && snapshot.supports(if index < request.sources.len() {
                                request.kind.capability()
                            } else {
                                Capability::Create
                            }) =>
                    {
                        owner.inspect(request, index + 1)
                    }
                    RequestOutcome::Failed(error)
                        if elevated && error.kind == OperationErrorKind::PermissionDenied =>
                    {
                        owner.inspect(request, index + 1)
                    }
                    outcome => {
                        let completion = OperationCompletion {
                            remaining: request.sources.clone(),
                            ..Default::default()
                        };
                        let event = match outcome {
                            RequestOutcome::Cancelled => OperationEvent::Cancelled {
                                id: request.id,
                                sequence: 0,
                                outcome: CancellationOutcome { completion },
                            },
                            RequestOutcome::Failed(error) => OperationEvent::Failed {
                                id: request.id,
                                sequence: 0,
                                error,
                                completion,
                            },
                            _ => OperationEvent::Failed {
                                id: request.id,
                                sequence: 0,
                                error: OperationError::new(
                                    OperationErrorKind::CapabilityChanged,
                                    request.sources.first(),
                                    request.destination.as_ref(),
                                ),
                                completion,
                            },
                        };
                        owner.accept(event, false);
                    }
                }
            }),
        );
    }
    fn accept(&self, mut event: OperationEvent, backend: bool) {
        let terminal = event.is_terminal();
        {
            let mut records = self.records.borrow_mut();
            let Some(record) = records.iter_mut().find(|r| r.projection.id == event.id()) else {
                return;
            };
            if record.reducer.is_terminal() {
                return;
            }
            if backend {
                if record
                    .backend_sequence
                    .is_some_and(|last| event.sequence() <= last)
                {
                    return;
                }
                record.backend_sequence = Some(event.sequence());
            }
            let next = record
                .reducer
                .events()
                .last()
                .map_or(1, |e| e.sequence() + 1);
            match &mut event {
                OperationEvent::Started { sequence, .. }
                | OperationEvent::Progress { sequence, .. }
                | OperationEvent::CancelRequested { sequence, .. }
                | OperationEvent::Succeeded { sequence, .. }
                | OperationEvent::Failed { sequence, .. }
                | OperationEvent::Cancelled { sequence, .. } => *sequence = next,
            }
            if !record.reducer.apply(event.clone()) {
                return;
            }
            let p = &mut record.projection;
            match &event {
                OperationEvent::Started { .. } => {
                    if p.state != OperationState::CancelRequested {
                        p.state = OperationState::Running;
                    }
                }
                OperationEvent::CancelRequested { .. } => p.state = OperationState::CancelRequested,
                OperationEvent::Progress { progress, .. } => p.progress = progress.clone(),
                OperationEvent::Succeeded { completion, .. } => {
                    p.state = OperationState::Succeeded;
                    p.completion = completion.clone();
                }
                OperationEvent::Failed {
                    error, completion, ..
                } => {
                    p.state = OperationState::Failed(error.clone());
                    p.completion = completion.clone();
                }
                OperationEvent::Cancelled { outcome, .. } => {
                    p.state = OperationState::Cancelled(outcome.clone());
                    p.completion = outcome.completion.clone();
                }
            }
            p.change = match p.completion.effect {
                MutationEffect::Unchanged => ChangeExtent::Unchanged,
                MutationEffect::Changed => ChangeExtent::Complete,
                MutationEffect::PartiallyChanged => ChangeExtent::Partial,
                MutationEffect::Unknown => ChangeExtent::Unknown,
            };
            p.events.push(event);
        }
        if terminal {
            let mut records = self.records.borrow_mut();
            let mut excess = records
                .iter()
                .filter(|record| record.reducer.is_terminal())
                .count()
                .saturating_sub(crate::core::application_preferences::MAX_HISTORY_RECORDS);
            records.retain(|record| {
                if excess > 0 && record.reducer.is_terminal() {
                    excess -= 1;
                    false
                } else {
                    true
                }
            });
            drop(records);
            self.schedule();
        }
    }
}
