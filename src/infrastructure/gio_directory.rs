//! Cancellable GIO enumeration. Only owned domain snapshots cross the worker
//! boundary; callbacks are deferred onto the repository's owning main context.
use crate::core::{contracts::*, ports::*, FileItem, Location, ResolvedLocation};
use gio::prelude::*;

#[derive(Clone)]
pub struct GioDirectoryRepository {
    context: glib::MainContext,
}

impl GioDirectoryRepository {
    pub fn new(context: glib::MainContext) -> Self {
        Self { context }
    }
}

impl DirectoryRepository for GioDirectoryRepository {
    fn list(&self, request: DirectoryRequest, done: DirectoryCallback) {
        let worker_request = request.clone();
        self.context.spawn_local(async move {
            let result = gio::spawn_blocking(move || enumerate(&worker_request)).await;
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
        "standard::type,access::can-read,access::can-write,access::can-trash,access::can-delete",
        gio::FileQueryInfoFlags::NONE, Some(&request.cancellable),
    ).map_err(map_error)?;
    let mut capabilities = FileItem::from_gio(&file, &info).capabilities().clone();
    capabilities.stable_order = false;
    let enumerator = file.enumerate_children(
        "standard::name,standard::display-name,standard::type,standard::content-type,standard::size,standard::target-uri,time::modified,access::can-read,access::can-write,access::can-trash,access::can-delete",
        gio::FileQueryInfoFlags::NONE, Some(&request.cancellable),
    ).map_err(map_error)?;
    let result = (|| {
        let mut entries = Vec::new();
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
                size: info.has_attribute("standard::size").then(|| item.size()),
                modified: info
                    .has_attribute("time::modified")
                    .then(|| item.modified()),
                content_type: info.content_type().map(|s| s.to_string()),
                capabilities: item.capabilities().clone(),
            });
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
