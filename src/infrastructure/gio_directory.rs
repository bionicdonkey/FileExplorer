//! Cancellable GIO directory work. Native GIO remains on the owned executor;
//! only URI-based DTOs cross back to the requesting main context.
use crate::{
    core::{contracts::*, ports::*, FileItem, Location, ResolvedLocation},
    infrastructure::gio_executor::{CancellationSignal, GioExecutor, GioRequest},
};
use futures_util::StreamExt;
use gio::prelude::*;
use std::{cell::RefCell, rc::Rc, sync::Arc};

#[derive(Clone)]
pub struct GioDirectoryRepository {
    context: glib::MainContext,
    executor: Arc<GioExecutor>,
    mount_operation: Rc<RefCell<Option<Rc<dyn Fn() -> gio::MountOperation>>>>,
}

/// The caller owns `Location` and its `gio::File`; the executor receives only
/// canonical URIs and creates its GIO values on its own thread-default context.
#[derive(Clone)]
struct DirectoryRequestData {
    id: RequestId,
    location: String,
    page: PageRequest,
}

impl From<&DirectoryRequest> for DirectoryRequestData {
    fn from(request: &DirectoryRequest) -> Self {
        Self {
            id: request.id,
            location: request.location.uri().to_owned(),
            page: request.page,
        }
    }
}

impl DirectoryRequestData {
    fn into_native(self, cancellable: gio::Cancellable) -> DirectoryRequest {
        DirectoryRequest {
            id: self.id,
            location: location_from_uri(self.location),
            page: self.page,
            cancellable,
        }
    }
}

#[derive(Clone)]
struct VisibleMetadataRequestData {
    id: RequestId,
    locations: Vec<String>,
}

impl From<&VisibleMetadataRequest> for VisibleMetadataRequestData {
    fn from(request: &VisibleMetadataRequest) -> Self {
        Self {
            id: request.id,
            locations: request
                .locations
                .iter()
                .map(|location| location.uri().to_owned())
                .collect(),
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

#[derive(Clone)]
struct DirectorySnapshotData {
    requested: String,
    resolved: String,
    entries: Vec<EntrySnapshotData>,
    capabilities: CapabilitySnapshotData,
    next_page: Option<PageRequest>,
}

impl From<DirectorySnapshot> for DirectorySnapshotData {
    fn from(snapshot: DirectorySnapshot) -> Self {
        Self {
            requested: snapshot.location.requested.uri().to_owned(),
            resolved: snapshot.location.resolved.uri().to_owned(),
            entries: snapshot.entries.into_iter().map(Into::into).collect(),
            capabilities: snapshot.capabilities.into(),
            next_page: snapshot.next_page,
        }
    }
}

impl DirectorySnapshotData {
    fn into_snapshot(self) -> DirectorySnapshot {
        DirectorySnapshot {
            location: ResolvedLocation {
                requested: location_from_uri(self.requested),
                resolved: location_from_uri(self.resolved),
            },
            entries: self
                .entries
                .into_iter()
                .map(EntrySnapshotData::into_snapshot)
                .collect(),
            capabilities: self.capabilities.into_snapshot(),
            next_page: self.next_page,
        }
    }
}

#[derive(Clone)]
struct VisibleMetadataData {
    location: String,
    size: Option<u64>,
    modified: Option<std::time::SystemTime>,
    content_type: Option<String>,
}

impl VisibleMetadataData {
    fn into_metadata(self) -> VisibleMetadata {
        VisibleMetadata {
            location: location_from_uri(self.location),
            size: self.size,
            modified: self.modified,
            content_type: self.content_type,
        }
    }
}

enum DirectoryOutcomeData {
    MountRequired { mountable: bool },
    Succeeded(DirectorySnapshotData),
    Failed(OperationError),
    Cancelled,
}

enum VisibleMetadataOutcomeData {
    Succeeded(Vec<VisibleMetadataData>),
    Failed(OperationError),
    Cancelled,
}

fn location_from_uri(uri: String) -> Location {
    Location::from_uri(&uri).expect("directory URI was validated at the public boundary")
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

impl GioDirectoryRepository {
    pub fn new(context: glib::MainContext) -> Self {
        Self::with_executor(context, GioExecutor::shared())
    }

    pub fn with_executor(context: glib::MainContext, executor: Arc<GioExecutor>) -> Self {
        Self {
            context,
            executor,
            mount_operation: Rc::new(RefCell::new(None)),
        }
    }

    pub fn set_mount_operation(&self, factory: impl Fn() -> gio::MountOperation + 'static) {
        *self.mount_operation.borrow_mut() = Some(Rc::new(factory));
    }

    fn deliver_list(&self, request: DirectoryRequest, done: DirectoryCallback) {
        let data = DirectoryRequestData::from(&request);
        let cancellation = cancellation_signal(&request.cancellable);
        let owner_cancellable = request.cancellable.clone();
        let failure_location = request.location.clone();
        let callback_data = data.clone();
        let callback_failure_location = failure_location.clone();
        let mount_operation = self.mount_operation.borrow().clone();
        let mount_retry_enabled = mount_operation.is_some();
        let executor = self.executor.clone();
        let context = self.context.clone();
        let (send, mut receive) = futures_channel::mpsc::unbounded();
        let failure_send = send.clone();
        let completion_cancellation = cancellation.clone();

        self.context.spawn_local(async move {
            // The caller-owned cancellation bridge and any GTK mount operation
            // stay on this context; only URI DTOs enter the executor.
            let _owner_cancellable = owner_cancellable;
            if let Some(outcome) = receive.next().await {
                match outcome {
                    DirectoryOutcomeData::MountRequired { mountable } => {
                        let Some(factory) = mount_operation else {
                            done(RequestCompletion {
                                id: data.id,
                                outcome: RequestOutcome::Failed(OperationError::new(
                                    OperationErrorKind::MountUnavailable,
                                    Some(&callback_failure_location),
                                    None,
                                )),
                            });
                            return;
                        };
                        mount_on_owner(
                            context,
                            request,
                            callback_data,
                            mountable,
                            factory,
                            completion_cancellation,
                            executor,
                            done,
                        );
                    }
                    outcome => done(RequestCompletion {
                        id: data.id,
                        outcome: into_directory_outcome(outcome),
                    }),
                }
            }
        });

        if self
            .executor
            .submit(cancellation, move |native_request| {
                start_list(native_request, data, mount_retry_enabled, send);
            })
            .is_err()
        {
            let _ = failure_send.unbounded_send(DirectoryOutcomeData::Failed(OperationError::new(
                OperationErrorKind::Unknown,
                Some(&failure_location),
                None,
            )));
        }
    }
}

impl DirectoryRepository for GioDirectoryRepository {
    fn watch(&self, location: &Location, changed: Box<dyn Fn()>) -> Option<gio::FileMonitor> {
        let monitor = gio::File::for_uri(location.uri())
            .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            .ok()?;
        monitor.connect_changed(move |_, _, _, event| {
            if matches!(
                event,
                gio::FileMonitorEvent::ChangesDoneHint
                    | gio::FileMonitorEvent::Created
                    | gio::FileMonitorEvent::Deleted
                    | gio::FileMonitorEvent::MovedIn
                    | gio::FileMonitorEvent::MovedOut
                    | gio::FileMonitorEvent::Renamed
                    | gio::FileMonitorEvent::AttributeChanged
            ) {
                changed();
            }
        });
        Some(monitor)
    }

    fn inspect_visible_metadata(
        &self,
        request: VisibleMetadataRequest,
        done: VisibleMetadataCallback,
    ) {
        let data = VisibleMetadataRequestData::from(&request);
        let cancellation = cancellation_signal(&request.cancellable);
        let owner_cancellable = request.cancellable.clone();
        let failure_location = request.locations.first().cloned();
        let (send, mut receive) = futures_channel::mpsc::unbounded();
        let failure_send = send.clone();

        self.context.spawn_local(async move {
            let _owner_cancellable = owner_cancellable;
            if let Some(outcome) = receive.next().await {
                done(RequestCompletion {
                    id: data.id,
                    outcome: into_metadata_outcome(outcome),
                });
            }
        });

        if self
            .executor
            .submit(cancellation, move |native_request| {
                let outcome = inspect_metadata(&data, &native_request);
                native_request.complete();
                let _ = send.unbounded_send(outcome);
            })
            .is_err()
        {
            let _ = failure_send.unbounded_send(VisibleMetadataOutcomeData::Failed(
                OperationError::new(OperationErrorKind::Unknown, failure_location.as_ref(), None),
            ));
        }
    }

    fn list_pages(&self, request: DirectoryRequest, mut sink: DirectoryPageSink) {
        let data = DirectoryRequestData::from(&request);
        let cancellation = cancellation_signal(&request.cancellable);
        let owner_cancellable = request.cancellable.clone();
        let failure_location = request.location.clone();
        let (send, receive) = std::sync::mpsc::sync_channel(2);
        let callback_failure_location = failure_location.clone();
        let failure_send = send.clone();
        let repository = self.clone();

        self.context.spawn_local(async move {
            let _owner_cancellable = owner_cancellable;
            loop {
                match receive.try_recv() {
                    Ok(DirectoryOutcomeData::MountRequired { .. }) => {
                        repository.deliver_list(request, Box::new(move |result| sink(result)));
                        return;
                    }
                    Ok(DirectoryOutcomeData::Failed(error))
                        if error.kind == OperationErrorKind::MountUnavailable =>
                    {
                        repository.deliver_list(request, Box::new(move |result| sink(result)));
                        return;
                    }
                    Ok(outcome) => {
                        let terminal = !matches!(
                            &outcome,
                            DirectoryOutcomeData::Succeeded(snapshot) if snapshot.next_page.is_some()
                        );
                        sink(RequestCompletion {
                            id: data.id,
                            outcome: into_directory_outcome(outcome),
                        });
                        if terminal {
                            return;
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        sink(RequestCompletion {
                            id: data.id,
                            outcome: RequestOutcome::Failed(OperationError::new(
                                OperationErrorKind::Unknown,
                                Some(&callback_failure_location),
                                None,
                            )),
                        });
                        return;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
                glib::timeout_future(std::time::Duration::from_millis(1)).await;
            }
        });

        if self
            .executor
            .submit(cancellation, move |native_request| {
                let native = data
                    .clone()
                    .into_native(native_request.cancellable().clone());
                let mut emit = |snapshot: DirectorySnapshot| {
                    send.send(DirectoryOutcomeData::Succeeded(snapshot.into()))
                        .is_ok()
                };
                let result = enumerate_with_pages(&native, Some(&mut emit));
                let outcome = terminal_directory_outcome(&native_request, result);
                drop(native);
                native_request.complete();
                let _ = send.send(outcome);
            })
            .is_err()
        {
            let _ = failure_send.send(DirectoryOutcomeData::Failed(OperationError::new(
                OperationErrorKind::Unknown,
                Some(&failure_location),
                None,
            )));
        }
    }

    fn list(&self, request: DirectoryRequest, done: DirectoryCallback) {
        self.deliver_list(request, done);
    }
}

fn into_directory_outcome(outcome: DirectoryOutcomeData) -> RequestOutcome<DirectorySnapshot> {
    match outcome {
        DirectoryOutcomeData::MountRequired { .. } => {
            unreachable!("mount request is handled on its owner context")
        }
        DirectoryOutcomeData::Succeeded(snapshot) => {
            RequestOutcome::Succeeded(snapshot.into_snapshot())
        }
        DirectoryOutcomeData::Failed(error) => RequestOutcome::Failed(error),
        DirectoryOutcomeData::Cancelled => RequestOutcome::Cancelled,
    }
}

fn into_metadata_outcome(
    outcome: VisibleMetadataOutcomeData,
) -> RequestOutcome<Vec<VisibleMetadata>> {
    match outcome {
        VisibleMetadataOutcomeData::Succeeded(metadata) => RequestOutcome::Succeeded(
            metadata
                .into_iter()
                .map(VisibleMetadataData::into_metadata)
                .collect(),
        ),
        VisibleMetadataOutcomeData::Failed(error) => RequestOutcome::Failed(error),
        VisibleMetadataOutcomeData::Cancelled => RequestOutcome::Cancelled,
    }
}

fn inspect_metadata(
    data: &VisibleMetadataRequestData,
    request: &GioRequest,
) -> VisibleMetadataOutcomeData {
    if request.synchronize_cancellation() {
        return VisibleMetadataOutcomeData::Cancelled;
    }
    let metadata = data
        .locations
        .iter()
        .filter_map(|uri| {
            if request.synchronize_cancellation() {
                return None;
            }
            let info = gio::File::for_uri(uri)
                .query_info(
                    "standard::fast-content-type,standard::size,time::modified",
                    gio::FileQueryInfoFlags::NONE,
                    Some(request.cancellable()),
                )
                .ok()?;
            Some(VisibleMetadataData {
                location: uri.clone(),
                size: info
                    .has_attribute("standard::size")
                    .then(|| info.size() as u64),
                modified: info.has_attribute("time::modified").then(|| {
                    std::time::UNIX_EPOCH
                        + std::time::Duration::from_secs(info.attribute_uint64("time::modified"))
                }),
                content_type: info
                    .attribute_string("standard::fast-content-type")
                    .map(|value| value.to_string()),
            })
        })
        .collect();
    if request.synchronize_cancellation() {
        VisibleMetadataOutcomeData::Cancelled
    } else {
        VisibleMetadataOutcomeData::Succeeded(metadata)
    }
}

fn terminal_directory_outcome(
    request: &GioRequest,
    result: Result<DirectorySnapshot, OperationError>,
) -> DirectoryOutcomeData {
    if request.synchronize_cancellation() {
        DirectoryOutcomeData::Cancelled
    } else {
        match result {
            Ok(snapshot) => DirectoryOutcomeData::Succeeded(snapshot.into()),
            Err(error) => DirectoryOutcomeData::Failed(error),
        }
    }
}

fn start_list(
    native_request: GioRequest,
    data: DirectoryRequestData,
    mount_retry_enabled: bool,
    send: futures_channel::mpsc::UnboundedSender<DirectoryOutcomeData>,
) {
    let native = data
        .clone()
        .into_native(native_request.cancellable().clone());
    let result = enumerate(&native);
    let mountable = matches!(&result, Err(error) if error.kind == OperationErrorKind::MountUnavailable)
        && gio::File::for_uri(&data.location)
            .query_info(
                "standard::type",
                gio::FileQueryInfoFlags::NONE,
                Some(native_request.cancellable()),
            )
            .is_ok_and(|info| info.file_type() == gio::FileType::Mountable);
    let mount_required = matches!(&result, Err(error) if error.kind == OperationErrorKind::MountUnavailable)
        && mount_retry_enabled
        && !native_request.synchronize_cancellation();
    let outcome = if mount_required {
        DirectoryOutcomeData::MountRequired { mountable }
    } else {
        terminal_directory_outcome(&native_request, result)
    };
    drop(native);
    native_request.complete();
    let _ = send.unbounded_send(outcome);
}

/// GTK's `MountOperation` is thread-affine. Create, use, and release it on the
/// requesting context; the resulting URI alone is sent back to the executor.
fn mount_on_owner(
    context: glib::MainContext,
    request: DirectoryRequest,
    data: DirectoryRequestData,
    mountable: bool,
    factory: Rc<dyn Fn() -> gio::MountOperation>,
    cancellation: CancellationSignal,
    executor: Arc<GioExecutor>,
    done: DirectoryCallback,
) {
    let operation = factory();
    let file = gio::File::for_uri(&data.location);
    let cancellable = request.cancellable.clone();
    let requested = request.location.clone();
    if mountable {
        let callback_operation = operation.clone();
        file.mount_mountable(
            gio::MountMountFlags::NONE,
            Some(&operation),
            Some(&cancellable.clone()),
            move |mounted| {
                drop(callback_operation);
                let outcome = if cancellation.is_cancelled() {
                    Err(RequestOutcome::Cancelled)
                } else {
                    mounted.map(|file| file.uri().to_string()).map_err(|error| {
                        RequestOutcome::Failed(OperationError::from_gio(
                            &error,
                            Some(&requested),
                            None,
                        ))
                    })
                };
                match outcome {
                    Ok(resolved_uri) => retry_after_owner_mount(
                        context,
                        executor,
                        cancellation,
                        data,
                        cancellable,
                        resolved_uri,
                        done,
                    ),
                    Err(outcome) => done(RequestCompletion {
                        id: data.id,
                        outcome,
                    }),
                }
            },
        );
    } else {
        let callback_file = file.clone();
        let callback_operation = operation.clone();
        file.mount_enclosing_volume(
            gio::MountMountFlags::NONE,
            Some(&operation),
            Some(&cancellable.clone()),
            move |mounted| {
                drop(callback_operation);
                let outcome = if cancellation.is_cancelled() {
                    Err(RequestOutcome::Cancelled)
                } else {
                    match mounted {
                        Ok(()) => Ok(callback_file.uri().to_string()),
                        Err(error) if error.matches(gio::IOErrorEnum::AlreadyMounted) => {
                            Ok(callback_file.uri().to_string())
                        }
                        Err(error) => Err(RequestOutcome::Failed(OperationError::from_gio(
                            &error,
                            Some(&requested),
                            None,
                        ))),
                    }
                };
                drop(callback_file);
                match outcome {
                    Ok(resolved_uri) => retry_after_owner_mount(
                        context,
                        executor,
                        cancellation,
                        data,
                        cancellable,
                        resolved_uri,
                        done,
                    ),
                    Err(outcome) => done(RequestCompletion {
                        id: data.id,
                        outcome,
                    }),
                }
            },
        );
    }
}

fn retry_after_owner_mount(
    context: glib::MainContext,
    executor: Arc<GioExecutor>,
    cancellation: CancellationSignal,
    data: DirectoryRequestData,
    owner_cancellable: gio::Cancellable,
    resolved_uri: String,
    done: DirectoryCallback,
) {
    let (send, mut receive) = futures_channel::mpsc::unbounded();
    let failure_send = send.clone();
    let failure_location = location_from_uri(data.location.clone());
    context.spawn_local(async move {
        let _owner_cancellable = owner_cancellable;
        if let Some(outcome) = receive.next().await {
            done(RequestCompletion {
                id: data.id,
                outcome: into_directory_outcome(outcome),
            });
        }
    });
    if executor
        .submit(cancellation, move |native_request| {
            let native = DirectoryRequestData {
                location: resolved_uri,
                ..data.clone()
            }
            .into_native(native_request.cancellable().clone());
            let result = enumerate(&native).map(|mut snapshot| {
                snapshot.location.requested = location_from_uri(data.location.clone());
                snapshot
            });
            let outcome = terminal_directory_outcome(&native_request, result);
            drop(native);
            native_request.complete();
            let _ = send.unbounded_send(outcome);
        })
        .is_err()
    {
        let _ = failure_send.unbounded_send(DirectoryOutcomeData::Failed(OperationError::new(
            OperationErrorKind::Unknown,
            Some(&failure_location),
            None,
        )));
    }
}
fn enumerate(request: &DirectoryRequest) -> Result<DirectorySnapshot, OperationError> {
    enumerate_with_pages(request, None)
}

fn enumerate_with_pages(
    request: &DirectoryRequest,
    mut emit: Option<&mut dyn FnMut(DirectorySnapshot) -> bool>,
) -> Result<DirectorySnapshot, OperationError> {
    // GIO does not promise stable ordering between enumerations. Return one
    // complete snapshot and reject offset paging, rather than silently losing rows.
    if request.page.offset != 0 || request.page.limit != usize::MAX {
        return Err(OperationError::new(
            OperationErrorKind::Unsupported,
            Some(&request.location),
            None,
        ));
    }
    let map_error =
        |error: glib::Error| OperationError::from_gio(&error, Some(&request.location), None);
    let file = gio::File::for_uri(request.location.uri());
    let info = file.query_info(
        "standard::type,access::can-read,access::can-write,access::can-rename,access::can-trash,access::can-delete",
        gio::FileQueryInfoFlags::NONE, Some(&request.cancellable),
    ).map_err(map_error)?;
    if info.file_type() == gio::FileType::Mountable {
        return Err(OperationError::new(
            OperationErrorKind::MountUnavailable,
            Some(&request.location),
            None,
        ));
    }
    let mut capabilities = FileItem::from_gio(&file, &info).capabilities().clone();
    capabilities.stable_order = false;
    capabilities.supported.insert(Capability::List);
    let location = ResolvedLocation {
        requested: request.location.clone(),
        resolved: Location::try_from_file(file.clone()).map_err(|_| {
            OperationError::new(
                OperationErrorKind::InvalidRequest,
                Some(&request.location),
                None,
            )
        })?,
    };
    // Fast content type comes from directory/name metadata. Do not sniff every
    // file's contents just to populate a model; image decoding is visible-only.
    let enumerator = file
        .enumerate_children(
            "standard::name,standard::display-name,standard::type,standard::target-uri",
            gio::FileQueryInfoFlags::NONE,
            Some(&request.cancellable),
        )
        .map_err(map_error)?;
    let result = (|| {
        let mut entries = Vec::new();
        let mut offset = 0;
        while let Some(info) = enumerator
            .next_file(Some(&request.cancellable))
            .map_err(map_error)?
        {
            let item = FileItem::from_gio(&file, &info);
            entries.push(EntrySnapshot {
                location: item.location().clone(),
                display_name: item.name().to_owned(),
                kind: match info.file_type() {
                    gio::FileType::Directory | gio::FileType::Mountable => EntryKind::Directory,
                    gio::FileType::Shortcut => EntryKind::Shortcut,
                    gio::FileType::Regular => EntryKind::File,
                    _ => EntryKind::Other,
                },
                size: None,
                modified: None,
                content_type: None,
                capabilities: item.capabilities().clone(),
            });
            if entries.len() == 128 {
                if let Some(emit) = &mut emit {
                    offset += entries.len();
                    if !emit(DirectorySnapshot {
                        location: location.clone(),
                        entries: std::mem::take(&mut entries),
                        capabilities: capabilities.clone(),
                        next_page: Some(PageRequest { offset, limit: 128 }),
                    }) {
                        return Err(OperationError::new(
                            OperationErrorKind::Unknown,
                            Some(&request.location),
                            None,
                        ));
                    }
                }
            }
        }
        capabilities.supported.insert(Capability::List);
        Ok(DirectorySnapshot {
            location: ResolvedLocation {
                requested: request.location.clone(),
                resolved: Location::try_from_file(file.clone()).map_err(|_| {
                    OperationError::new(
                        OperationErrorKind::InvalidRequest,
                        Some(&request.location),
                        None,
                    )
                })?,
            },
            entries,
            capabilities,
            next_page: None,
        })
    })();
    let close = enumerator.close(gio::Cancellable::NONE).map_err(map_error);
    result.and_then(|snapshot| close.map(|_| snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc, time::Duration};

    #[gtk::test]
    fn configured_gtk_mount_operation_is_created_and_used_on_its_owner_context() {
        let context = glib::MainContext::default();
        let location = Location::from_uri("smb://fixture/mount-operation").unwrap();
        let cancellable = gio::Cancellable::new();
        let cancellation = CancellationSignal::new();
        cancellable.cancel();
        cancellation.cancel();
        let created = Rc::new(Cell::new(0));
        let completed = Rc::new(Cell::new(false));
        let created_factory = created.clone();
        let completed_callback = completed.clone();
        let executor = GioExecutor::isolated_for_tests();
        let request = DirectoryRequest {
            id: RequestId(9),
            location: location.clone(),
            page: PageRequest {
                offset: 0,
                limit: usize::MAX,
            },
            cancellable,
        };

        mount_on_owner(
            context.clone(),
            request,
            DirectoryRequestData {
                id: RequestId(9),
                location: location.uri().to_owned(),
                page: PageRequest {
                    offset: 0,
                    limit: usize::MAX,
                },
            },
            false,
            Rc::new(move || {
                created_factory.set(created_factory.get() + 1);
                gtk::MountOperation::new(None::<&gtk::Window>).upcast()
            }),
            cancellation,
            executor.clone(),
            Box::new(move |completion| {
                assert!(matches!(completion.outcome, RequestOutcome::Cancelled));
                completed_callback.set(true);
            }),
        );

        assert_eq!(created.get(), 1);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !completed.get() {
            assert!(std::time::Instant::now() < deadline);
            context.iteration(false);
        }
        assert_eq!(executor.active_request_count_for_tests(), 0);
        executor.shutdown_for_tests();
    }

    #[gtk::test]
    fn retry_after_owner_mount_is_drained_by_executor_shutdown() {
        use std::{
            sync::{
                atomic::{AtomicBool, Ordering},
                mpsc, Arc,
            },
            time::Duration,
        };

        let context = glib::MainContext::default();
        let fixture = tempfile::tempdir().unwrap();
        let location = Location::from_local_path(fixture.path());
        let executor = GioExecutor::isolated_for_tests();
        let release_gate = Arc::new(AtomicBool::new(false));
        let (started, started_receiver) = mpsc::sync_channel(1);
        executor
            .submit(CancellationSignal::new(), {
                let release_gate = release_gate.clone();
                move |request| {
                    started.send(()).unwrap();
                    while !release_gate.load(Ordering::SeqCst) {
                        std::thread::yield_now();
                    }
                    request.complete();
                }
            })
            .unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("gate request must occupy the executor before retry submission");

        let completed = Rc::new(Cell::new(false));
        let completion = completed.clone();
        // Mount authentication belongs to the owner context and requires an
        // external provider. Enter the production retry continuation directly
        // so the executor-owned half is deterministic and still exercises its
        // real URI DTO conversion, native request, and terminal callback path.
        retry_after_owner_mount(
            context.clone(),
            executor.clone(),
            CancellationSignal::new(),
            DirectoryRequestData {
                id: RequestId(10),
                location: location.uri().to_owned(),
                page: PageRequest {
                    offset: 0,
                    limit: usize::MAX,
                },
            },
            gio::Cancellable::new(),
            location.uri().to_owned(),
            Box::new(move |result| {
                assert!(matches!(result.outcome, RequestOutcome::Cancelled));
                completion.set(true);
            }),
        );

        assert_eq!(executor.active_request_count_for_tests(), 2);
        release_gate.store(true, Ordering::SeqCst);
        executor.shutdown_for_tests();

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !completed.get() {
            assert!(std::time::Instant::now() < deadline);
            context.iteration(false);
        }
        assert_eq!(executor.active_request_count_for_tests(), 0);
    }
}
