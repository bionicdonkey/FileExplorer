//! GIO boundary. Native I/O runs on the owned executor; DTO callbacks return
//! on the requesting context. GIO owns move semantics, including cross-filesystem fallback.
use crate::{
    core::{contracts::*, ports::*, Location},
    infrastructure::gio_executor::{CancellationSignal, GioExecutor, OwnerContextGuard},
};
use futures_util::StreamExt;
use gio::prelude::*;
use std::sync::Arc;

#[derive(Clone)]
pub struct GioOperations {
    context: glib::MainContext,
    executor: Arc<GioExecutor>,
    uri_operations: bool,
    elevated: Arc<std::sync::Mutex<std::collections::HashSet<OperationId>>>,
    // Dropping the last handle waits for the executor to release every
    // reference into `context`, so the owner performs its own final unref.
    owner: OwnerContextGuard,
}

/// Pure request data that may cross from the caller to the executor.
/// `Location` owns a `gio::File`, so only its validated URI crosses threads.
#[derive(Clone)]
struct OperationRequestData {
    id: OperationId,
    kind: MutationKind,
    sources: Vec<String>,
    destination: Option<String>,
    explicit_target: Option<String>,
    capability_version: u64,
}

impl OperationRequestData {
    fn from_request(request: &OperationRequest) -> Self {
        Self {
            id: request.id,
            kind: request.kind,
            sources: request
                .sources
                .iter()
                .map(|location| location.uri().to_owned())
                .collect(),
            destination: request
                .destination
                .as_ref()
                .map(|location| location.uri().to_owned()),
            explicit_target: request
                .explicit_target
                .as_ref()
                .map(|location| location.uri().to_owned()),
            capability_version: request.capability_version,
        }
    }

    fn into_native(self, cancellable: gio::Cancellable) -> OperationRequest {
        OperationRequest {
            id: self.id,
            kind: self.kind,
            sources: locations_from_uris(self.sources),
            destination: self.destination.map(|uri| location_from_uri(uri)),
            explicit_target: self.explicit_target.map(|uri| location_from_uri(uri)),
            capability_version: self.capability_version,
            cancellable,
        }
    }
}

#[derive(Clone)]
struct CapabilityRequestData {
    id: RequestId,
    location: String,
}

impl From<&CapabilityRequest> for CapabilityRequestData {
    fn from(request: &CapabilityRequest) -> Self {
        Self {
            id: request.id,
            location: request.location.uri().to_owned(),
        }
    }
}

#[derive(Clone)]
struct CompletionData {
    effect: MutationEffect,
    completed: Vec<String>,
    remaining: Vec<String>,
}

impl From<OperationCompletion> for CompletionData {
    fn from(completion: OperationCompletion) -> Self {
        Self {
            effect: completion.effect,
            completed: completion
                .completed
                .into_iter()
                .map(|location| location.uri().to_owned())
                .collect(),
            remaining: completion
                .remaining
                .into_iter()
                .map(|location| location.uri().to_owned())
                .collect(),
        }
    }
}

impl CompletionData {
    fn into_completion(self) -> OperationCompletion {
        OperationCompletion {
            effect: self.effect,
            completed: locations_from_uris(self.completed),
            remaining: locations_from_uris(self.remaining),
        }
    }
}

enum OperationEventData {
    Started {
        id: OperationId,
        sequence: u64,
    },
    Progress {
        id: OperationId,
        sequence: u64,
        progress: OperationProgress,
    },
    CancelRequested {
        id: OperationId,
        sequence: u64,
    },
    Succeeded {
        id: OperationId,
        sequence: u64,
        completion: CompletionData,
    },
    Failed {
        id: OperationId,
        sequence: u64,
        error: OperationError,
        completion: CompletionData,
    },
    Cancelled {
        id: OperationId,
        sequence: u64,
        completion: CompletionData,
    },
}

impl From<OperationEvent> for OperationEventData {
    fn from(event: OperationEvent) -> Self {
        match event {
            OperationEvent::Started { id, sequence } => Self::Started { id, sequence },
            OperationEvent::Progress {
                id,
                sequence,
                progress,
            } => Self::Progress {
                id,
                sequence,
                progress,
            },
            OperationEvent::CancelRequested { id, sequence } => {
                Self::CancelRequested { id, sequence }
            }
            OperationEvent::Succeeded {
                id,
                sequence,
                completion,
            } => Self::Succeeded {
                id,
                sequence,
                completion: completion.into(),
            },
            OperationEvent::Failed {
                id,
                sequence,
                error,
                completion,
            } => Self::Failed {
                id,
                sequence,
                error,
                completion: completion.into(),
            },
            OperationEvent::Cancelled {
                id,
                sequence,
                outcome,
            } => Self::Cancelled {
                id,
                sequence,
                completion: outcome.completion.into(),
            },
        }
    }
}

impl OperationEventData {
    fn into_event(self) -> OperationEvent {
        match self {
            Self::Started { id, sequence } => OperationEvent::Started { id, sequence },
            Self::Progress {
                id,
                sequence,
                progress,
            } => OperationEvent::Progress {
                id,
                sequence,
                progress,
            },
            Self::CancelRequested { id, sequence } => {
                OperationEvent::CancelRequested { id, sequence }
            }
            Self::Succeeded {
                id,
                sequence,
                completion,
            } => OperationEvent::Succeeded {
                id,
                sequence,
                completion: completion.into_completion(),
            },
            Self::Failed {
                id,
                sequence,
                error,
                completion,
            } => OperationEvent::Failed {
                id,
                sequence,
                error,
                completion: completion.into_completion(),
            },
            Self::Cancelled {
                id,
                sequence,
                completion,
            } => OperationEvent::Cancelled {
                id,
                sequence,
                outcome: CancellationOutcome {
                    completion: completion.into_completion(),
                },
            },
        }
    }
}

enum ContainmentData {
    Opaque,
    KnownAncestors(Vec<String>),
}

struct CapabilitySnapshotData {
    version: u64,
    supported: std::collections::BTreeSet<Capability>,
    stable_order: bool,
    containment: ContainmentData,
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
                ContainmentData::KnownAncestors(locations) => {
                    Containment::KnownAncestors(locations_from_uris(locations))
                }
            },
        }
    }
}

enum RequestOutcomeData {
    Succeeded(CapabilitySnapshotData),
    Failed(OperationError),
    Cancelled,
}

fn location_from_uri(uri: String) -> Location {
    Location::from_uri(&uri).expect("operation URI was validated at the public boundary")
}

fn locations_from_uris(uris: Vec<String>) -> Vec<Location> {
    uris.into_iter().map(location_from_uri).collect()
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
impl GioOperations {
    pub fn new(context: glib::MainContext) -> Self {
        Self::with_executor(context, GioExecutor::shared())
    }
    pub fn with_executor(context: glib::MainContext, executor: Arc<GioExecutor>) -> Self {
        Self {
            owner: OwnerContextGuard::new(context.clone()),
            context,
            executor,
            uri_operations: false,
            elevated: Default::default(),
        }
    }

    /// Blocks until the executor holds no reference into this adapter's owner
    /// context. Dropping the adapter does this too; call it explicitly when the
    /// owner context outlives the adapter handles.
    pub fn drain_owner_requests(&self) {
        self.owner.drain();
    }

    #[doc(hidden)]
    pub fn outstanding_owner_requests_for_tests(&self) -> usize {
        self.owner.outstanding()
    }
    /// Production GIO/GVfs boundary. Unsupported actions are determined from
    /// provider metadata and GIO outcomes, not from the URI's scheme.
    pub fn new_uri(context: glib::MainContext) -> Self {
        Self::new_uri_with_executor(context, GioExecutor::shared())
    }
    pub fn new_uri_with_executor(context: glib::MainContext, executor: Arc<GioExecutor>) -> Self {
        Self {
            uri_operations: true,
            ..Self::with_executor(context, executor)
        }
    }
    pub(crate) fn authorize_elevated_retry(&self, id: OperationId) {
        self.elevated.lock().unwrap().insert(id);
    }
    pub fn local_capabilities() -> CapabilitySnapshot {
        CapabilitySnapshot {
            version: 1,
            supported: [
                Capability::Copy,
                Capability::Move,
                Capability::Create,
                Capability::Rename,
                Capability::Trash,
                Capability::Delete,
                Capability::Open,
            ]
            .into_iter()
            .collect(),
            stable_order: true,
            containment: Containment::KnownAncestors(Vec::new()),
        }
    }
    pub fn launch(
        &self,
        location: Location,
        cancellable: gio::Cancellable,
        done: Box<dyn FnOnce(Result<(), OperationError>)>,
    ) {
        let uri_operations = self.uri_operations;
        self.context.spawn_local(async move {
            if !uri_operations && !local(&location) {
                done(Err(OperationError::new(
                    OperationErrorKind::Unsupported,
                    Some(&location),
                    None,
                )));
                return;
            }
            let uri = location.uri().to_owned();
            gio::AppInfo::launch_default_for_uri_async(
                &uri,
                gio::AppLaunchContext::NONE,
                Some(&cancellable),
                move |result| {
                    done(result.map_err(|e| OperationError::from_gio(&e, Some(&location), None)))
                },
            );
        });
    }
}
impl FileOperations for GioOperations {
    fn inspect_capabilities(&self, request: CapabilityRequest, done: CapabilityCallback) {
        self.inspect(request, None, done);
    }
    fn inspect_operation_capabilities(
        &self,
        request: CapabilityRequest,
        capability: Capability,
        done: CapabilityCallback,
    ) {
        self.inspect(request, Some(capability), done);
    }
    fn submit(&self, request: OperationRequest, sink: OperationSink) {
        self.dispatch(request, None, sink);
    }
    fn submit_with_decision(
        &self,
        request: OperationRequest,
        decision: ConflictDecision,
        sink: OperationSink,
    ) {
        self.dispatch(request, Some(decision), sink);
    }
}
impl GioOperations {
    fn inspect(
        &self,
        request: CapabilityRequest,
        capability: Option<Capability>,
        done: CapabilityCallback,
    ) {
        let uri_operations = self.uri_operations;
        let executor = self.executor.clone();
        let data = CapabilityRequestData::from(&request);
        let cancellation = cancellation_signal(&request.cancellable);
        let owner_cancellable = request.cancellable.clone();
        let failure_location = request.location.clone();
        let (send, mut receive) = futures_channel::mpsc::unbounded::<RequestOutcomeData>();
        let failure_send = send.clone();
        let bridge = self.owner.bridge(send);

        self.context.spawn_local(async move {
            // Retain the request-side native cancellable on its owner context
            // until the executor has acknowledged a terminal result.
            let _owner_cancellable = owner_cancellable;
            if let Some(outcome) = receive.next().await {
                done(RequestCompletion {
                    id: data.id,
                    outcome: match outcome {
                        RequestOutcomeData::Succeeded(snapshot) => {
                            RequestOutcome::Succeeded(snapshot.into_snapshot())
                        }
                        RequestOutcomeData::Failed(error) => RequestOutcome::Failed(error),
                        RequestOutcomeData::Cancelled => RequestOutcome::Cancelled,
                    },
                });
            }
        });

        if executor
            .submit(cancellation, move |native_request| {
                let location = location_from_uri(data.location);
                let outcome = if native_request.synchronize_cancellation() {
                    RequestOutcomeData::Cancelled
                } else {
                    let result = if uri_operations {
                        inspect_uri_for(&location, native_request.cancellable(), capability)
                    } else {
                        inspect_local_for(&location, native_request.cancellable(), capability)
                    };
                    if native_request.synchronize_cancellation() {
                        RequestOutcomeData::Cancelled
                    } else {
                        match result {
                            Ok(snapshot) => RequestOutcomeData::Succeeded(snapshot.into()),
                            Err(error) => RequestOutcomeData::Failed(error),
                        }
                    }
                };

                let mut completion = Some((native_request, outcome, bridge));
                let source = glib::idle_source_new(None, glib::Priority::DEFAULT, move || {
                    let (native_request, outcome, bridge) = completion
                        .take()
                        .expect("executor completion source runs once");
                    native_request.complete();
                    bridge.send(outcome);
                    glib::ControlFlow::Break
                });
                source.attach(Some(&glib::MainContext::ref_thread_default()));
            })
            .is_err()
        {
            let _ = failure_send.unbounded_send(RequestOutcomeData::Failed(OperationError::new(
                OperationErrorKind::Unknown,
                Some(&failure_location),
                None,
            )));
        }
    }
    fn dispatch(
        &self,
        request: OperationRequest,
        decision: Option<ConflictDecision>,
        mut sink: OperationSink,
    ) {
        let (send, mut receive) = futures_channel::mpsc::unbounded::<OperationEventData>();
        let failure_send = send.clone();
        let bridge = self.owner.bridge(send);
        let failure_sources = request.sources.clone();
        let failure_destination = request.destination.clone();
        let failure_id = request.id;
        let data = OperationRequestData::from_request(&request);
        let cancellation = cancellation_signal(&request.cancellable);
        let owner_cancellable = request.cancellable.clone();
        let elevated = self.elevated.lock().unwrap().remove(&request.id);
        let uri_operations = self.uri_operations;
        let executor = self.executor.clone();

        self.context.spawn_local(async move {
            let _owner_cancellable = owner_cancellable;
            let mut terminal = None;
            while let Some(event) = receive.next().await {
                let event = event.into_event();
                if event.is_terminal() {
                    terminal = Some(event);
                } else if terminal.is_none() {
                    sink(event);
                }
            }
            if let Some(event) = terminal {
                sink(event);
            }
        });

        if executor
            .submit(cancellation.clone(), move |native_request| {
                let request = data.into_native(native_request.cancellable().clone());
                let mut terminal = None;
                let mut emit = |event: OperationEvent| {
                    let event = OperationEventData::from(event);
                    if matches!(
                        event,
                        OperationEventData::Succeeded { .. }
                            | OperationEventData::Failed { .. }
                            | OperationEventData::Cancelled { .. }
                    ) {
                        terminal = Some(event);
                    } else {
                        bridge.send(event);
                    }
                };
                if elevated {
                    execute_elevated(request, &mut emit);
                } else {
                    native_request.synchronize_cancellation();
                    execute_inner_mode(request, decision, uri_operations, &mut emit);
                }

                let mut completion = Some((native_request, terminal, bridge));
                let source = glib::idle_source_new(None, glib::Priority::DEFAULT, move || {
                    let (native_request, terminal, bridge) = completion
                        .take()
                        .expect("executor completion source runs once");
                    native_request.complete();
                    if let Some(event) = terminal {
                        bridge.send(event);
                    }
                    glib::ControlFlow::Break
                });
                source.attach(Some(&glib::MainContext::ref_thread_default()));
            })
            .is_err()
        {
            let _ = failure_send.unbounded_send(OperationEventData::Failed {
                id: failure_id,
                sequence: 1,
                error: OperationError::new(
                    OperationErrorKind::Unknown,
                    failure_sources.first(),
                    failure_destination.as_ref(),
                ),
                completion: OperationCompletion {
                    remaining: failure_sources,
                    ..Default::default()
                }
                .into(),
            });
        }
    }
}

fn execute_elevated(request: OperationRequest, sink: &mut dyn FnMut(OperationEvent)) {
    let id = request.id;
    let mut completion = OperationCompletion {
        remaining: request.sources.clone(),
        ..Default::default()
    };
    if request.cancellable.is_cancelled() {
        sink(OperationEvent::Cancelled {
            id,
            sequence: 1,
            outcome: CancellationOutcome { completion },
        });
        return;
    }
    let Some(mut command) = crate::core::elevated_operations::retry_request_command(&request)
    else {
        sink(OperationEvent::Failed {
            id,
            sequence: 1,
            error: OperationError::new(
                OperationErrorKind::InvalidRequest,
                request.sources.first(),
                request.destination.as_ref(),
            ),
            completion,
        });
        return;
    };
    sink(OperationEvent::Started { id, sequence: 1 });
    // The helper may have committed before cancellation: wait for its actual exit.
    // Neither an auth dialog nor cancel intent can establish rollback.
    match command.status() {
        Ok(status) if status.success() => {
            completion.effect = MutationEffect::Changed;
            completion.completed = request.sources;
            completion.remaining.clear();
            sink(OperationEvent::Succeeded {
                id,
                sequence: 2,
                completion,
            });
        }
        result => {
            if result.is_ok() {
                completion.effect = MutationEffect::Unknown;
            }
            sink(OperationEvent::Failed {
                id,
                sequence: 2,
                error: OperationError::new(
                    OperationErrorKind::Unknown,
                    request.sources.first(),
                    request.destination.as_ref(),
                ),
                completion,
            });
        }
    }
}
fn local(location: &Location) -> bool {
    location.uri().starts_with("file:") && location.local_path().is_some()
}

fn inspect_local_for(
    location: &Location,
    cancel: &gio::Cancellable,
    capability: Option<Capability>,
) -> Result<CapabilitySnapshot, OperationError> {
    let snapshot = inspect_local(location, cancel)?;
    if capability.is_some_and(|required| {
        GioOperations::local_capabilities().supports(required) && !snapshot.supports(required)
    }) {
        return Err(OperationError::from_gio(
            &glib::Error::new(
                gio::IOErrorEnum::PermissionDenied,
                "operation.permission-denied",
            ),
            Some(location),
            None,
        ));
    }
    Ok(snapshot)
}

fn inspect_local(
    location: &Location,
    cancel: &gio::Cancellable,
) -> Result<CapabilitySnapshot, OperationError> {
    if !local(location) {
        return Err(OperationError::new(
            OperationErrorKind::Unsupported,
            Some(location),
            None,
        ));
    }
    let file = gio::File::for_uri(location.uri());
    let query = |file: &gio::File| {
        file.query_info(
            "standard::type,access::*",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            Some(cancel),
        )
    };
    let info = match query(&file) {
        Ok(info) => info,
        Err(error) if error.matches(gio::IOErrorEnum::NotFound) => query(
            &file
                .parent()
                .ok_or_else(|| OperationError::from_gio(&error, Some(location), None))?,
        )
        .map_err(|e| OperationError::from_gio(&e, Some(location), None))?,
        Err(error) => return Err(OperationError::from_gio(&error, Some(location), None)),
    };
    let mut capabilities = GioOperations::local_capabilities();
    for (attribute, capability) in [
        ("access::can-read", Capability::Copy),
        ("access::can-rename", Capability::Move),
        ("access::can-rename", Capability::Rename),
        ("access::can-delete", Capability::Delete),
        ("access::can-trash", Capability::Trash),
    ] {
        if info.has_attribute(attribute) && !info.boolean(attribute) {
            capabilities.supported.remove(&capability);
        }
    }
    let parent = if info.file_type() == gio::FileType::Directory {
        info.clone()
    } else if let Some(parent) = file.parent() {
        query(&parent).map_err(|e| OperationError::from_gio(&e, Some(location), None))?
    } else {
        info
    };
    if parent.has_attribute("access::can-write") && !parent.boolean("access::can-write") {
        capabilities.supported.remove(&Capability::Create);
    }
    Ok(capabilities)
}

fn inspect_uri_for(
    location: &Location,
    cancel: &gio::Cancellable,
    capability: Option<Capability>,
) -> Result<CapabilitySnapshot, OperationError> {
    if local(location) {
        return inspect_local_for(location, cancel, capability);
    }
    let requested = gio::File::for_uri(location.uri());
    let attributes = "standard::type,standard::target-uri,access::*";
    let query = |file: &gio::File| {
        file.query_info(
            attributes,
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            Some(cancel),
        )
    };
    let (file, info) = match query(&requested) {
        Ok(info) => (requested, info),
        Err(error)
            if capability == Some(Capability::Create)
                && error.matches(gio::IOErrorEnum::NotFound) =>
        {
            let parent = requested
                .parent()
                .ok_or_else(|| OperationError::from_gio(&error, Some(location), None))?;
            let info =
                query(&parent).map_err(|e| OperationError::from_gio(&e, Some(location), None))?;
            (parent, info)
        }
        Err(error) => return Err(OperationError::from_gio(&error, Some(location), None)),
    };
    let mut snapshot = CapabilitySnapshot {
        version: 1,
        ..Default::default()
    };
    snapshot.supported.insert(Capability::Open);
    for (attribute, required) in [
        ("access::can-read", Capability::Copy),
        ("access::can-rename", Capability::Move),
        ("access::can-rename", Capability::Rename),
        ("access::can-trash", Capability::Trash),
        ("access::can-delete", Capability::Delete),
    ] {
        if info.has_attribute(attribute) && info.boolean(attribute) {
            snapshot.supported.insert(required);
        }
    }
    if info.file_type() == gio::FileType::Directory
        && info.has_attribute("access::can-write")
        && info.boolean("access::can-write")
    {
        snapshot.supported.insert(Capability::Create);
    }
    // GIO supplies the parent relation. Resolve provider shortcuts before
    // retaining ancestry; opaque backends with no parent stay conservative.
    let resolved = if let Some(uri) = info.attribute_string("standard::target-uri") {
        Location::from_uri(&uri).map_err(|_| {
            OperationError::new(OperationErrorKind::InvalidRequest, Some(location), None)
        })?
    } else {
        Location::try_from_file(file).map_err(|_| {
            OperationError::new(OperationErrorKind::InvalidRequest, Some(location), None)
        })?
    };
    let mut ancestors = vec![resolved.clone()];
    let mut next = resolved.parent();
    while let Some(parent) = next {
        if ancestors.contains(&parent) || ancestors.len() >= 256 {
            return Err(OperationError::new(
                OperationErrorKind::InvalidRequest,
                Some(location),
                None,
            ));
        }
        next = parent.parent();
        ancestors.push(parent);
    }
    if ancestors.len() > 1 || info.file_type() == gio::FileType::Directory {
        snapshot.containment = Containment::KnownAncestors(ancestors);
    }
    if capability.is_some_and(|c| !snapshot.supports(c)) {
        return Err(OperationError::new(
            OperationErrorKind::Unsupported,
            Some(location),
            None,
        ));
    }
    Ok(snapshot)
}

pub fn execute(request: OperationRequest, sink: &mut dyn FnMut(OperationEvent)) {
    execute_inner(request, None, sink);
}
pub fn execute_with_decision(
    request: OperationRequest,
    decision: ConflictDecision,
    sink: &mut dyn FnMut(OperationEvent),
) {
    execute_inner(request, Some(decision), sink);
}
fn execute_inner(
    request: OperationRequest,
    decision: Option<ConflictDecision>,
    sink: &mut dyn FnMut(OperationEvent),
) {
    execute_inner_mode(request, decision, false, sink);
}

fn execute_inner_mode(
    request: OperationRequest,
    decision: Option<ConflictDecision>,
    uri_operations: bool,
    sink: &mut dyn FnMut(OperationEvent),
) {
    let mut sequence = 0;
    let mut send = |mut event: OperationEvent| {
        sequence += 1;
        match &mut event {
            OperationEvent::Started { sequence: s, .. }
            | OperationEvent::Progress { sequence: s, .. }
            | OperationEvent::CancelRequested { sequence: s, .. }
            | OperationEvent::Succeeded { sequence: s, .. }
            | OperationEvent::Failed { sequence: s, .. }
            | OperationEvent::Cancelled { sequence: s, .. } => *s = sequence,
        }
        sink(event);
    };
    let mut completion = OperationCompletion {
        remaining: request.sources.clone(),
        ..Default::default()
    };
    let id = request.id;
    let fail = |kind, completion| OperationEvent::Failed {
        id,
        sequence: 0,
        error: OperationError::new(kind, request.sources.first(), request.destination.as_ref()),
        completion,
    };
    if !uri_operations
        && !request
            .sources
            .iter()
            .chain(request.destination.iter())
            .chain(request.explicit_target.iter())
            .all(local)
    {
        send(fail(OperationErrorKind::Unsupported, completion));
        return;
    }
    if request.capability_version != 1 {
        send(fail(OperationErrorKind::CapabilityChanged, completion));
        return;
    }
    if crate::application::operations::lock_keys(&request, &GioOperations::local_capabilities())
        .is_err()
    {
        send(fail(OperationErrorKind::InvalidRequest, completion));
        return;
    }
    if request.cancellable.is_cancelled() {
        send(OperationEvent::Cancelled {
            id,
            sequence: 0,
            outcome: CancellationOutcome { completion },
        });
        return;
    }
    for (index, location) in request
        .sources
        .iter()
        .chain(request.destination.iter())
        .chain(request.explicit_target.iter())
        .enumerate()
    {
        let inspect = if uri_operations {
            inspect_uri_for
        } else {
            inspect_local_for
        };
        match inspect(
            location,
            &request.cancellable,
            Some(if index < request.sources.len() {
                request.kind.capability()
            } else {
                Capability::Create
            }),
        ) {
            Ok(capabilities)
                if capabilities.supports(if index < request.sources.len() {
                    request.kind.capability()
                } else {
                    Capability::Create
                }) =>
            {
                if index >= request.sources.len()
                    && matches!(
                        request.kind,
                        MutationKind::Copy | MutationKind::Move | MutationKind::Rename
                    )
                {
                    if let Err(error) =
                        crate::application::operations::lock_keys(&request, &capabilities)
                    {
                        send(OperationEvent::Failed {
                            id,
                            sequence: 0,
                            error,
                            completion,
                        });
                        return;
                    }
                }
            }
            Ok(_) => {
                send(fail(OperationErrorKind::CapabilityChanged, completion));
                return;
            }
            Err(error) => {
                if request.cancellable.is_cancelled() {
                    send(OperationEvent::Cancelled {
                        id,
                        sequence: 0,
                        outcome: CancellationOutcome { completion },
                    });
                } else {
                    send(OperationEvent::Failed {
                        id,
                        sequence: 0,
                        error,
                        completion,
                    });
                }
                return;
            }
        }
    }
    if decision == Some(ConflictDecision::Skip) {
        send(OperationEvent::Succeeded {
            id,
            sequence: 0,
            completion,
        });
        return;
    }
    if decision == Some(ConflictDecision::CancelRemaining) {
        send(OperationEvent::Cancelled {
            id,
            sequence: 0,
            outcome: CancellationOutcome { completion },
        });
        return;
    }
    if request.cancellable.is_cancelled() {
        send(OperationEvent::Cancelled {
            id,
            sequence: 0,
            outcome: CancellationOutcome { completion },
        });
        return;
    }
    send(OperationEvent::Started { id, sequence: 0 });
    let total = if request.kind == MutationKind::Copy {
        request
            .sources
            .iter()
            .map(|l| count_bytes(&gio::File::for_uri(l.uri()), &request.cancellable).unwrap_or(0))
            .sum()
    } else {
        0
    };
    let mut bytes = 0;
    for (index, source) in request.sources.iter().enumerate() {
        if request.cancellable.is_cancelled() {
            send(OperationEvent::Cancelled {
                id,
                sequence: 0,
                outcome: CancellationOutcome { completion },
            });
            return;
        }
        let file = gio::File::for_uri(source.uri());
        let target = request
            .explicit_target
            .as_ref()
            .map(|l| gio::File::for_uri(l.uri()))
            .or_else(|| {
                request.destination.as_ref().and_then(|l| {
                    file.basename()
                        .map(|name| gio::File::for_uri(l.uri()).child(name))
                })
            });
        let target = if decision == Some(ConflictDecision::KeepBoth) {
            target.map(|t| keep_both(&t, &request.cancellable))
        } else {
            target
        };
        let mut changed = false;
        let result = perform_with_flags(
            request.kind,
            &file,
            target.as_ref(),
            &request.cancellable,
            decision == Some(ConflictDecision::Replace),
            &mut changed,
            &mut |delta| {
                bytes += delta;
                send(OperationEvent::Progress {
                    id,
                    sequence: 0,
                    progress: OperationProgress {
                        completed_items: index as u64,
                        total_items: Some(request.sources.len() as u64),
                        completed_bytes: Some(bytes),
                        total_bytes: Some(total),
                    },
                });
            },
        );
        if let Err(error) = result {
            completion.effect = if changed {
                MutationEffect::Unknown
            } else if completion.completed.is_empty() {
                MutationEffect::Unchanged
            } else {
                MutationEffect::PartiallyChanged
            };
            if error.matches(gio::IOErrorEnum::Cancelled) {
                send(OperationEvent::Cancelled {
                    id,
                    sequence: 0,
                    outcome: CancellationOutcome { completion },
                });
            } else {
                send(OperationEvent::Failed {
                    id,
                    sequence: 0,
                    error: OperationError::from_gio(
                        &error,
                        Some(source),
                        request
                            .destination
                            .as_ref()
                            .or(request.explicit_target.as_ref()),
                    ),
                    completion,
                });
            }
            return;
        }
        completion.completed.push(source.clone());
        completion.remaining.remove(0);
        completion.effect = if completion.remaining.is_empty() {
            MutationEffect::Changed
        } else {
            MutationEffect::PartiallyChanged
        };
        send(OperationEvent::Progress {
            id,
            sequence: 0,
            progress: OperationProgress {
                completed_items: (index + 1) as u64,
                total_items: Some(request.sources.len() as u64),
                completed_bytes: (request.kind == MutationKind::Copy).then_some(bytes),
                total_bytes: (request.kind == MutationKind::Copy).then_some(total),
            },
        });
    }
    send(OperationEvent::Succeeded {
        id,
        sequence: 0,
        completion,
    });
}
fn invalid() -> glib::Error {
    glib::Error::new(
        gio::IOErrorEnum::InvalidArgument,
        "operation.invalid-request",
    )
}
/// Synchronous boundary used only by adapter workers and the privileged helper.
pub fn perform(
    kind: MutationKind,
    source: &gio::File,
    target: Option<&gio::File>,
    cancel: &gio::Cancellable,
    changed: &mut bool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), glib::Error> {
    perform_with_flags(kind, source, target, cancel, false, changed, progress)
}
fn perform_with_flags(
    kind: MutationKind,
    source: &gio::File,
    target: Option<&gio::File>,
    cancel: &gio::Cancellable,
    replace: bool,
    changed: &mut bool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), glib::Error> {
    let flags = gio::FileCopyFlags::NOFOLLOW_SYMLINKS
        | if replace {
            gio::FileCopyFlags::OVERWRITE
        } else {
            gio::FileCopyFlags::NONE
        };
    if cancel.is_cancelled() {
        return Err(glib::Error::new(
            gio::IOErrorEnum::Cancelled,
            "operation.cancelled",
        ));
    }
    if matches!(
        kind,
        MutationKind::Copy | MutationKind::Move | MutationKind::Rename
    ) {
        let target = target.ok_or_else(invalid)?;
        // Logical URI identity is preserved, but physical local containment
        // must also reject destination aliases back into the source tree.
        if let (Some(source), Some(target)) = (source.path(), target.path()) {
            let resolved_source = std::fs::canonicalize(&source);
            let resolved_target = std::fs::canonicalize(&target).or_else(|_| {
                let parent = target.parent().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing parent")
                })?;
                Ok::<_, std::io::Error>(
                    std::fs::canonicalize(parent)?.join(target.file_name().unwrap_or_default()),
                )
            });
            if let (Ok(source), Ok(target)) = (resolved_source, resolved_target) {
                if target.starts_with(source) {
                    return Err(invalid());
                }
            }
        }
        if !replace && target.query_exists(Some(cancel)) {
            return Err(glib::Error::new(
                gio::IOErrorEnum::Exists,
                "operation.conflict",
            ));
        }
    }
    match kind {
        MutationKind::Copy => copy_tree(
            source,
            target.ok_or_else(invalid)?,
            cancel,
            flags,
            changed,
            progress,
        ),
        MutationKind::Move | MutationKind::Rename => {
            // A failure after GIO starts may have effects the provider cannot prove.
            source.query_info(
                "standard::type",
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                Some(cancel),
            )?;
            *changed = true;
            source.move_(target.ok_or_else(invalid)?, flags, Some(cancel), None)
        }
        MutationKind::Create => {
            let result = source.make_directory(Some(cancel));
            if result.is_ok() {
                *changed = true;
            }
            result
        }
        MutationKind::Trash => {
            source.query_info(
                "standard::type",
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                Some(cancel),
            )?;
            *changed = true;
            source.trash(Some(cancel))
        }
        MutationKind::Delete => delete_tree(source, cancel, changed),
    }
}
fn copy_tree(
    source: &gio::File,
    target: &gio::File,
    cancel: &gio::Cancellable,
    flags: gio::FileCopyFlags,
    changed: &mut bool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), glib::Error> {
    let info = source.query_info(
        "standard::type",
        gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
        Some(cancel),
    )?;
    if info.file_type() == gio::FileType::Directory {
        target.make_directory(Some(cancel))?;
        *changed = true;
        let entries = source.enumerate_children(
            "standard::name",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            Some(cancel),
        )?;
        while let Some(entry) = entries.next_file(Some(cancel))? {
            copy_tree(
                &source.child(entry.name()),
                &target.child(entry.name()),
                cancel,
                flags,
                changed,
                progress,
            )?;
        }
        entries.close(Some(cancel))?;
        Ok(())
    } else {
        *changed = true;
        let mut previous = 0;
        source.copy(
            target,
            flags,
            Some(cancel),
            Some(&mut |current, _| {
                let current = current.max(0) as u64;
                progress(current.saturating_sub(previous));
                previous = current;
            }),
        )
    }
}
fn delete_tree(
    source: &gio::File,
    cancel: &gio::Cancellable,
    changed: &mut bool,
) -> Result<(), glib::Error> {
    let info = source.query_info(
        "standard::type",
        gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
        Some(cancel),
    )?;
    if info.file_type() == gio::FileType::Directory {
        let entries = source.enumerate_children(
            "standard::name",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            Some(cancel),
        )?;
        while let Some(entry) = entries.next_file(Some(cancel))? {
            delete_tree(&source.child(entry.name()), cancel, changed)?;
        }
        entries.close(Some(cancel))?;
    }
    source.delete(Some(cancel))?;
    *changed = true;
    Ok(())
}
pub fn count_bytes(source: &gio::File, cancel: &gio::Cancellable) -> Result<u64, glib::Error> {
    let info = source.query_info(
        "standard::type,standard::size",
        gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
        Some(cancel),
    )?;
    if info.file_type() != gio::FileType::Directory {
        return Ok(info.size().max(0) as u64);
    }
    let entries = source.enumerate_children(
        "standard::name",
        gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
        Some(cancel),
    )?;
    let mut bytes = 0;
    while let Some(entry) = entries.next_file(Some(cancel))? {
        bytes += count_bytes(&source.child(entry.name()), cancel)?;
    }
    entries.close(Some(cancel))?;
    Ok(bytes)
}

fn keep_both(target: &gio::File, cancel: &gio::Cancellable) -> gio::File {
    if !target.query_exists(Some(cancel)) {
        return target.clone();
    }
    let Some(parent) = target.parent() else {
        return target.clone();
    };
    let name = target.basename().unwrap_or_default();
    let stem = name.file_stem().unwrap_or_default().to_string_lossy();
    let extension = name.extension().map(|e| e.to_string_lossy());
    for index in 2.. {
        let candidate = parent.child(match &extension {
            Some(ext) => format!("{stem} ({index}).{ext}"),
            None => format!("{stem} ({index})"),
        });
        if cancel.is_cancelled() || !candidate.query_exists(Some(cancel)) {
            return candidate;
        }
    }
    unreachable!()
}
