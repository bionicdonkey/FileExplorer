//! Chooses the directory adapter for a location.
//!
//! A local archive is served in-process by [`ArchiveDirectoryRepository`], so
//! a `.7z` lists the same way a `.zip` does. Everything else -- local folders,
//! and remote archives gvfs can still mount -- goes to the gio adapter. The
//! split is by location, not by caller, so every window, sidebar and picker
//! gets the same answer.

use crate::{
    core::{contracts::*, ports::*, Location},
    infrastructure::archive_directory::{self, ArchiveDirectoryRepository},
    infrastructure::gio_directory::GioDirectoryRepository,
};
use gio::prelude::*;
use std::rc::Rc;

#[derive(Clone)]
pub struct DirectoryRouter {
    gio: Rc<GioDirectoryRepository>,
    archives: Rc<ArchiveDirectoryRepository>,
}

impl DirectoryRouter {
    pub fn new(context: glib::MainContext) -> Self {
        Self {
            gio: Rc::new(GioDirectoryRepository::new(context.clone())),
            archives: Rc::new(ArchiveDirectoryRepository::new(context)),
        }
    }

    pub fn set_mount_operation(&self, factory: impl Fn() -> gio::MountOperation + 'static) {
        self.gio.set_mount_operation(factory);
    }

    /// Blocks until both adapters' executors hold no reference into the owner
    /// context. Dropping the last handle does this too; call it explicitly when
    /// the owner context outlives the router handles.
    pub fn drain_owner_requests(&self) {
        self.gio.drain_owner_requests();
        self.archives.drain_owner_requests();
    }

    #[doc(hidden)]
    pub fn outstanding_owner_requests_for_tests(&self) -> usize {
        self.gio.outstanding_owner_requests_for_tests()
            + self.archives.outstanding_owner_requests_for_tests()
    }
}

impl DirectoryRepository for DirectoryRouter {
    fn list(&self, request: DirectoryRequest, done: DirectoryCallback) {
        if archive_directory::serves(&request.location) {
            self.archives.list(request, done);
        } else {
            self.gio.list(request, done);
        }
    }

    fn list_pages(&self, request: DirectoryRequest, sink: DirectoryPageSink) {
        if archive_directory::serves(&request.location) {
            self.archives.list_pages(request, sink);
        } else {
            self.gio.list_pages(request, sink);
        }
    }

    fn inspect_visible_metadata(
        &self,
        request: VisibleMetadataRequest,
        done: VisibleMetadataCallback,
    ) {
        if !request.locations.is_empty()
            && request
                .locations
                .iter()
                .all(archive_directory::serves)
        {
            // Archive entries already carry their size, type and modified time
            // from the listing; there is nothing left to query.
            let outcome = if request.cancellable.is_cancelled() {
                RequestOutcome::Cancelled
            } else {
                RequestOutcome::Succeeded(Vec::new())
            };
            done(RequestCompletion {
                id: request.id,
                outcome,
            });
        } else {
            self.gio.inspect_visible_metadata(request, done);
        }
    }

    fn watch(&self, location: &Location, changed: Box<dyn Fn()>) -> Option<gio::FileMonitor> {
        if archive_directory::serves(location) {
            None
        } else {
            self.gio.watch(location, changed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::compression::{compress_blocking, ArchiveFormat};
    use crate::core::contracts::{PageRequest, RequestId};
    use std::{
        cell::RefCell,
        rc::Rc,
        time::{Duration, Instant},
    };

    #[gtk::test]
    fn an_archive_location_is_listed_by_the_archive_adapter() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("src");
        std::fs::create_dir_all(source.join("kit")).unwrap();
        std::fs::write(source.join("kit/readme.md"), "hello").unwrap();
        let created = compress_blocking(
            &[source.join("kit")],
            ArchiveFormat::Zip,
            &mut |_, _| {},
            &|| false,
        )
        .unwrap();

        let context = glib::MainContext::default();
        let router = DirectoryRouter::new(context.clone());
        let inside = Location::from_local_path(&created)
            .archive_contents()
            .unwrap();
        let received = Rc::new(RefCell::new(None));
        let sink = received.clone();
        router.list(
            DirectoryRequest {
                id: RequestId::allocate(),
                location: inside,
                page: PageRequest {
                    offset: 0,
                    limit: usize::MAX,
                },
                cancellable: gio::Cancellable::new(),
            },
            Box::new(move |completion| {
                *sink.borrow_mut() = Some(completion);
            }),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        while received.borrow().is_none() {
            assert!(Instant::now() < deadline, "listing did not finish");
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        let completion = received.borrow_mut().take().unwrap();
        let RequestOutcome::Succeeded(snapshot) = completion.outcome else {
            panic!("the archive was not listed");
        };
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].display_name, "kit");
        router.drain_owner_requests();
    }
}
