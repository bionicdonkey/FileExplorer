//! Cancellable GIO enumeration. Only owned domain snapshots cross the worker
//! boundary; callbacks are deferred onto the repository's owning main context.
use crate::core::{contracts::*, ports::*, FileItem, Location, ResolvedLocation};
use gio::prelude::*;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
pub struct GioDirectoryRepository {
    context: glib::MainContext,
    mount_operation: Rc<RefCell<Option<Rc<dyn Fn() -> gio::MountOperation>>>>,
}

impl GioDirectoryRepository {
    pub fn new(context: glib::MainContext) -> Self {
        Self {
            context,
            mount_operation: Rc::new(RefCell::new(None)),
        }
    }
    pub fn set_mount_operation(&self, factory: impl Fn() -> gio::MountOperation + 'static) {
        *self.mount_operation.borrow_mut() = Some(Rc::new(factory));
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
    fn list_pages(&self, request: DirectoryRequest, mut sink: DirectoryPageSink) {
        let (send, receive) = std::sync::mpsc::sync_channel(2);
        let worker = request.clone();
        std::thread::spawn(move || {
            let mut emit = |snapshot| send.send(RequestOutcome::Succeeded(snapshot)).is_ok();
            let result = enumerate_with_pages(&worker, Some(&mut emit));
            let outcome = if worker.cancellable.is_cancelled() {
                RequestOutcome::Cancelled
            } else {
                match result {
                    Ok(snapshot) => RequestOutcome::Succeeded(snapshot),
                    Err(error) => RequestOutcome::Failed(error),
                }
            };
            let _ = send.send(outcome);
        });
        let repository = self.clone();
        self.context.spawn_local(async move {
            loop {
                match receive.try_recv() {
                    Ok(RequestOutcome::Failed(error)) if error.kind == OperationErrorKind::MountUnavailable => {
                        // Preserve the established native authentication/mount
                        // interaction. The fallback owns cancellation and retry.
                        repository.list(request, Box::new(move |result| sink(result)));
                        return;
                    }
                    Ok(outcome) => {
                        let terminal = !matches!(&outcome, RequestOutcome::Succeeded(snapshot) if snapshot.next_page.is_some());
                        sink(RequestCompletion { id: request.id, outcome });
                        if terminal { return; }
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        sink(RequestCompletion { id: request.id, outcome: RequestOutcome::Failed(OperationError::new(OperationErrorKind::Unknown, Some(&request.location), None)) });
                        return;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
                // Yield between bounded pages so input and the first frame can
                // run while the same enumeration cursor continues in the worker.
                glib::timeout_future(std::time::Duration::from_millis(1)).await;
            }
        });
    }
    fn list(&self, request: DirectoryRequest, done: DirectoryCallback) {
        let worker_request = request.clone();
        let mount_operation = self.mount_operation.clone();
        self.context.spawn_local(async move {
            let mut result = gio::spawn_blocking(move || enumerate(&worker_request)).await;
            if matches!(&result, Ok(Err(error)) if error.kind == OperationErrorKind::MountUnavailable)
                && !request.cancellable.is_cancelled()
            {
                let factory = mount_operation.borrow().clone();
                if let Some(factory) = factory {
                    let operation = factory();
                    let type_request = request.clone();
                    let mountable = gio::spawn_blocking(move || {
                        gio::File::for_uri(type_request.location.uri()).query_info(
                            "standard::type", gio::FileQueryInfoFlags::NONE, Some(&type_request.cancellable),
                        ).is_ok_and(|info| info.file_type() == gio::FileType::Mountable)
                    }).await.unwrap_or(false);
                    let (send, receive) = futures_channel::oneshot::channel();
                    let file = gio::File::for_uri(request.location.uri());
                    if mountable {
                        file.mount_mountable(gio::MountMountFlags::NONE, Some(&operation), Some(&request.cancellable),
                            move |result| { let _ = send.send(result); });
                    } else {
                        let mounted_file = file.clone();
                        file.mount_enclosing_volume(
                            gio::MountMountFlags::NONE, Some(&operation), Some(&request.cancellable),
                            move |result| {
                                let result = match result {
                                    Err(error) if error.matches(gio::IOErrorEnum::AlreadyMounted) => Ok(mounted_file),
                                    result => result.map(|_| mounted_file),
                                };
                                let _ = send.send(result);
                            },
                        );
                    }
                    let mounted = receive.await;
                    // The operation and its authentication UI live for precisely
                    // this request; navigation cancellation cancels the GIO call.
                    drop(operation);
                    match mounted {
                        Ok(Ok(file)) => {
                            if let Ok(resolved) = Location::try_from_file(file) {
                                let mut worker_request = request.clone();
                                worker_request.location = resolved;
                                result = gio::spawn_blocking(move || enumerate(&worker_request)).await;
                                if let Ok(Ok(snapshot)) = &mut result { snapshot.location.requested = request.location.clone(); }
                            } else {
                                result = Ok(Err(OperationError::new(OperationErrorKind::InvalidRequest, Some(&request.location), None)));
                            }
                        }
                        Ok(Err(error)) => result = Ok(Err(OperationError::from_gio(&error, Some(&request.location), None))),
                        Err(_) => {}
                    }
                }
            }
            let outcome = if request.cancellable.is_cancelled() {
                RequestOutcome::Cancelled
            } else {
                match result {
                    Ok(Ok(snapshot)) => RequestOutcome::Succeeded(snapshot),
                    Ok(Err(error)) => RequestOutcome::Failed(error),
                    Err(_) => RequestOutcome::Failed(OperationError::new(
                        OperationErrorKind::Unknown,
                        Some(&request.location),
                        None,
                    )),
                }
            };
            done(RequestCompletion {
                id: request.id,
                outcome,
            });
        });
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
    let enumerator = file.enumerate_children(
        "standard::name,standard::display-name,standard::type,standard::fast-content-type,standard::size,standard::target-uri,time::modified,access::can-read,access::can-write,access::can-rename,access::can-trash,access::can-delete",
        gio::FileQueryInfoFlags::NONE, Some(&request.cancellable),
    ).map_err(map_error)?;
    let result = (|| {
        let mut entries = Vec::new();
        let mut offset = 0;
        while let Some(info) = enumerator
            .next_file(Some(&request.cancellable))
            .map_err(map_error)?
        {
            if let Some(content_type) = info.attribute_string("standard::fast-content-type") {
                info.set_content_type(&content_type);
            }
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
                size: info.has_attribute("standard::size").then(|| item.size()),
                modified: info
                    .has_attribute("time::modified")
                    .then(|| item.modified()),
                content_type: info.content_type().map(|s| s.to_string()),
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
