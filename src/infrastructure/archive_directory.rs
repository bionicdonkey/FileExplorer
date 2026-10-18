//! Local archives as directories, read in-process with libarchive.
//!
//! gvfsd-archive mounts a browsable archive by handing libarchive a stream it
//! cannot seek, so a format whose index is not at the front -- 7-Zip above all
//! -- cannot be listed. This adapter opens the archive's own file instead,
//! which libarchive can seek, and serves the `archive://` locations the window
//! already addresses: tabs, breadcrumbs, history and Up keep working unchanged.
//!
//! Listing reads only entry headers, so file data is never decompressed. The
//! parsed index is cached per archive and reused while the file is unchanged.
//! The adapter is read-only: entries report Open, Copy and Move, and the
//! directory reports List, but no Create, Rename, Trash or Delete.

use crate::{
    core::{contracts::*, ports::*, Location, ResolvedLocation},
    infrastructure::gio_executor::{CancellationSignal, GioExecutor, OwnerContextGuard},
};
use futures_util::StreamExt;
use gio::prelude::*;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::SystemTime,
};

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;

/// Whether the archive adapter serves `location`: a location inside an archive
/// whose underlying file is local. A remote archive keeps going through gvfs,
/// which is the only thing that can reach it.
pub fn serves(location: &Location) -> bool {
    location
        .archive_source()
        .and_then(|archive| archive.local_path())
        .is_some()
}

#[derive(Clone, Debug)]
struct FileMeta {
    size: u64,
    modified: Option<SystemTime>,
}

/// One archive's entries as relative, decoded paths.
#[derive(Clone, Debug, Default)]
struct ArchiveIndex {
    dirs: BTreeSet<String>,
    files: BTreeMap<String, FileMeta>,
}

/// A child of the directory being listed, before it becomes a `Location`.
#[derive(Clone, Debug)]
struct Child {
    name: String,
    is_dir: bool,
    size: Option<u64>,
    modified: Option<SystemTime>,
}

impl ArchiveIndex {
    fn children_of(&self, parent: &str) -> Vec<Child> {
        let mut children = Vec::new();
        for dir in self.dirs.iter().filter(|dir| parent_of(dir) == parent) {
            children.push(Child {
                name: name_of(dir).to_owned(),
                is_dir: true,
                size: None,
                modified: None,
            });
        }
        for (path, meta) in self
            .files
            .iter()
            .filter(|(path, _)| parent_of(path) == parent)
        {
            children.push(Child {
                name: name_of(path).to_owned(),
                is_dir: false,
                size: Some(meta.size),
                modified: meta.modified,
            });
        }
        children.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.name.cmp(&right.name))
        });
        children
    }
}

/// The parent directory of a relative archive path, `""` at the root.
fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn name_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// A path inside the archive, split into safe components and rejoined.
///
/// Returns `None` for an entry libarchive should not have produced: empty, or
/// stepping out of the archive with `..`. A leading `./` and empty components
/// are dropped, the way the filesystem would read them.
fn safe_relative_path(name: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in name.trim_end_matches('/').split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Reads every entry header, recording directories (including ones the archive
/// never stored explicitly) and files with their size and modification time.
fn read_index(path: &Path) -> Result<ArchiveIndex, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("Could not open the archive: {error}"))?;
    let mut iterator = compress_tools::ArchiveIterator::from_read(file)
        .map_err(|error| format!("Could not read the archive: {error}"))?;
    let mut index = ArchiveIndex::default();
    while let Some(contents) = iterator.next_header() {
        match contents {
            compress_tools::ArchiveContents::StartOfEntry(name, stat) => {
                let Some(relative) = safe_relative_path(&name) else {
                    continue;
                };
                let mut ancestor = parent_of(&relative);
                while !ancestor.is_empty() {
                    index.dirs.insert(ancestor.to_owned());
                    ancestor = parent_of(ancestor);
                }
                if stat.st_mode & S_IFMT == S_IFDIR {
                    index.dirs.insert(relative);
                } else {
                    index.files.insert(
                        relative,
                        FileMeta {
                            size: stat.st_size.max(0) as u64,
                            modified: (stat.st_mtime >= 0).then(|| {
                                SystemTime::UNIX_EPOCH
                                    + std::time::Duration::from_secs(stat.st_mtime as u64)
                            }),
                        },
                    );
                }
            }
            compress_tools::ArchiveContents::Err(error) => {
                return Err(format!("Could not read the archive: {error}"));
            }
            _ => {}
        }
    }
    Ok(index)
}

enum Listing {
    Succeeded {
        modified: Option<SystemTime>,
        index: Arc<ArchiveIndex>,
        children: Vec<Child>,
    },
    Failed(String),
    Cancelled,
}

fn build_listing(
    path: &Path,
    inner: &str,
    cached: Option<(SystemTime, Arc<ArchiveIndex>)>,
    cancelled: &CancellationSignal,
) -> Listing {
    let modified = std::fs::metadata(path).and_then(|meta| meta.modified()).ok();
    if cancelled.is_cancelled() {
        return Listing::Cancelled;
    }
    let index = match (&cached, modified) {
        (Some((cached_modified, index)), Some(modified)) if *cached_modified == modified => {
            index.clone()
        }
        _ => match read_index(path) {
            Ok(index) => Arc::new(index),
            Err(message) => return Listing::Failed(message),
        },
    };
    if cancelled.is_cancelled() {
        return Listing::Cancelled;
    }
    let children = index.children_of(inner);
    Listing::Succeeded {
        modified,
        index,
        children,
    }
}

/// What an entry inside an archive can do. The archive is read-only, so none
/// of Create, Rename, Trash or Delete is offered; Copy and Move are, because a
/// file can be taken out of the archive and into a folder.
pub(crate) fn archive_entry_capabilities() -> CapabilitySnapshot {
    CapabilitySnapshot {
        version: 1,
        supported: [Capability::Open, Capability::Copy, Capability::Move]
            .into_iter()
            .collect(),
        stable_order: true,
        containment: Containment::Opaque,
    }
}

fn archive_directory_capabilities() -> CapabilitySnapshot {
    CapabilitySnapshot {
        version: 1,
        supported: [Capability::List].into_iter().collect(),
        stable_order: true,
        containment: Containment::Opaque,
    }
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

#[derive(Clone)]
pub struct ArchiveDirectoryRepository {
    context: glib::MainContext,
    executor: Arc<GioExecutor>,
    // Parsed archives, keyed by their file, invalidated by the file's mtime.
    // Main-context only, so navigating a large archive does not re-read it.
    index: Rc<RefCell<BTreeMap<PathBuf, (SystemTime, Arc<ArchiveIndex>)>>>,
    // Dropping the last handle waits for the executor to release every
    // reference into `context`, so the owner performs its own final unref.
    owner: OwnerContextGuard,
}

impl ArchiveDirectoryRepository {
    pub fn new(context: glib::MainContext) -> Self {
        Self::with_executor(context, GioExecutor::shared())
    }

    pub fn with_executor(context: glib::MainContext, executor: Arc<GioExecutor>) -> Self {
        Self {
            owner: OwnerContextGuard::new(context.clone()),
            context,
            executor,
            index: Rc::new(RefCell::new(BTreeMap::new())),
        }
    }

    /// Blocks until the executor holds no reference into this adapter's owner
    /// context. Dropping the last handle does this too; call it explicitly when
    /// the owner context outlives the adapter handles.
    pub fn drain_owner_requests(&self) {
        self.owner.drain();
    }

    #[doc(hidden)]
    pub fn outstanding_owner_requests_for_tests(&self) -> usize {
        self.owner.outstanding()
    }

    fn fail(
        &self,
        id: RequestId,
        location: &Location,
        kind: OperationErrorKind,
        done: DirectoryCallback,
    ) {
        let error = OperationError::new(kind, Some(location), None);
        self.context.spawn_local(async move {
            done(RequestCompletion {
                id,
                outcome: RequestOutcome::Failed(error),
            });
        });
    }
}

impl DirectoryRepository for ArchiveDirectoryRepository {
    fn list(&self, request: DirectoryRequest, done: DirectoryCallback) {
        let id = request.id;
        let location = request.location.clone();
        let Some(archive) = location.archive_source() else {
            return self.fail(id, &location, OperationErrorKind::Unsupported, done);
        };
        let Some(path) = archive.local_path() else {
            return self.fail(id, &location, OperationErrorKind::Unsupported, done);
        };
        let inner = location.archive_inner_path().unwrap_or_default();
        let cached = self.index.borrow().get(&path).cloned();
        let cancellation = cancellation_signal(&request.cancellable);
        let owner_cancellable = request.cancellable.clone();
        let (send, mut receive) = futures_channel::mpsc::unbounded::<Listing>();
        let failure_send = send.clone();
        let bridge = self.owner.bridge(send);
        let cache = self.index.clone();
        self.context.spawn_local({
            let location = location.clone();
            let path = path.clone();
            async move {
                // Retain the request-side cancellable on its owner context
                // until the executor has acknowledged a terminal result.
                let _owner_cancellable = owner_cancellable;
                let Some(listing) = receive.next().await else {
                    return;
                };
                match listing {
                    Listing::Succeeded {
                        modified,
                        index,
                        children,
                    } => {
                        if let Some(modified) = modified {
                            cache.borrow_mut().insert(path, (modified, index));
                        }
                        let entries = children
                            .into_iter()
                            .map(|child| entry_snapshot(&location, child))
                            .collect();
                        done(RequestCompletion {
                            id,
                            outcome: RequestOutcome::Succeeded(DirectorySnapshot {
                                location: ResolvedLocation {
                                    requested: location.clone(),
                                    resolved: location,
                                },
                                entries,
                                capabilities: archive_directory_capabilities(),
                                next_page: None,
                            }),
                        });
                    }
                    Listing::Failed(message) => {
                        eprintln!("zorin-explorer: {message}");
                        done(RequestCompletion {
                            id,
                            outcome: RequestOutcome::Failed(OperationError::new(
                                OperationErrorKind::Unknown,
                                Some(&location),
                                None,
                            )),
                        });
                    }
                    Listing::Cancelled => done(RequestCompletion {
                        id,
                        outcome: RequestOutcome::Cancelled,
                    }),
                }
            }
        });
        if self
            .executor
            .submit(cancellation.clone(), move |native_request| {
                let outcome = if cancellation.is_cancelled() {
                    Listing::Cancelled
                } else {
                    build_listing(&path, &inner, cached, &cancellation)
                };
                native_request.complete();
                bridge.send(outcome);
            })
            .is_err()
        {
            let _ = failure_send.unbounded_send(Listing::Failed(
                "The archive reader stopped unexpectedly.".to_owned(),
            ));
        }
    }
}

fn entry_snapshot(directory: &Location, child: Child) -> EntrySnapshot {
    let location = Location::try_from_file(gio::File::for_uri(directory.uri()).child(&child.name))
        .unwrap_or_else(|_| directory.clone());
    let content_type = if child.is_dir {
        "inode/directory".to_owned()
    } else {
        gio::content_type_guess(Some(&child.name), &[]).0.to_string()
    };
    EntrySnapshot {
        location,
        display_name: child.name,
        kind: if child.is_dir {
            EntryKind::Directory
        } else {
            EntryKind::File
        },
        size: child.size,
        modified: child.modified,
        content_type: Some(content_type),
        capabilities: archive_entry_capabilities(),
    }
}

#[cfg(test)]
pub(crate) fn read_index_for_tests(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = read_index(path)
        .map(|index| {
            index
                .dirs
                .into_iter()
                .chain(index.files.into_keys())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::compression::{compress_blocking, ArchiveFormat};
    use crate::core::contracts::{PageRequest, RequestId};
    use std::{
        cell::RefCell,
        path::PathBuf,
        rc::Rc,
        time::{Duration, Instant},
    };

    #[test]
    fn only_a_local_archive_is_served_in_process() {
        let local = Location::from_uri("file:///home/user/Trip.7z")
            .unwrap()
            .archive_contents()
            .unwrap();
        assert!(serves(&local));
        let remote = Location::from_uri("smb://server/Trip.zip")
            .unwrap()
            .archive_contents()
            .unwrap();
        assert!(!serves(&remote), "a remote archive still goes through gvfs");
        assert!(!serves(&Location::from_uri("file:///home/user").unwrap()));
    }

    #[test]
    fn entry_paths_are_normalised_and_unsafe_ones_are_dropped() {
        assert_eq!(safe_relative_path("kit/").as_deref(), Some("kit"));
        assert_eq!(safe_relative_path("./kit/readme.md").as_deref(), Some("kit/readme.md"));
        assert_eq!(safe_relative_path("kit//readme.md").as_deref(), Some("kit/readme.md"));
        assert_eq!(safe_relative_path("../escape"), None);
        assert_eq!(safe_relative_path("kit/../../escape"), None);
        assert_eq!(safe_relative_path("/absolute"), Some("absolute".to_owned()));
        assert_eq!(safe_relative_path(""), None);
    }

    #[test]
    fn children_include_directories_and_files_sorted_by_name() {
        let mut index = ArchiveIndex::default();
        index.dirs.insert("kit".to_owned());
        index.dirs.insert("kit/sub".to_owned());
        index.files.insert(
            "kit/sub/inner.txt".to_owned(),
            FileMeta {
                size: 5,
                modified: None,
            },
        );
        index.files.insert(
            "notes.txt".to_owned(),
            FileMeta {
                size: 1,
                modified: None,
            },
        );

        let root: Vec<_> = index
            .children_of("")
            .into_iter()
            .map(|child| (child.name, child.is_dir))
            .collect();
        assert_eq!(
            root,
            vec![("kit".to_owned(), true), ("notes.txt".to_owned(), false)]
        );
        let kit: Vec<_> = index
            .children_of("kit")
            .into_iter()
            .map(|child| (child.name, child.is_dir))
            .collect();
        assert_eq!(kit, vec![("sub".to_owned(), true)]);
        let sub: Vec<_> = index
            .children_of("kit/sub")
            .into_iter()
            .map(|child| child.name)
            .collect();
        assert_eq!(sub, vec!["inner.txt".to_owned()]);
    }

    fn archive(root: &Path, format: ArchiveFormat) -> PathBuf {
        let source = root.join("src");
        std::fs::create_dir_all(source.join("kit/sub")).unwrap();
        std::fs::write(source.join("kit/readme.md"), "hello").unwrap();
        std::fs::write(source.join("kit/sub/inner.txt"), "inner").unwrap();
        compress_blocking(&[source.join("kit")], format, &mut |_, _| {}, &|| false).unwrap()
    }

    #[test]
    fn a_seven_zip_lists_its_entries_in_order() {
        for format in [ArchiveFormat::SevenZip, ArchiveFormat::TarGz, ArchiveFormat::Zip] {
            let root = tempfile::tempdir().unwrap();
            let created = archive(root.path(), format);
            let index = read_index(&created).unwrap();

            let root_children: Vec<_> = index
                .children_of("")
                .into_iter()
                .map(|child| (child.name, child.is_dir))
                .collect();
            assert_eq!(
                root_children,
                vec![("kit".to_owned(), true)],
                "{format:?}"
            );
            let kit_children: Vec<_> = index
                .children_of("kit")
                .into_iter()
                .map(|child| child.name)
                .collect();
            assert_eq!(
                kit_children,
                vec!["readme.md".to_owned(), "sub".to_owned()],
                "{format:?}"
            );
        }
    }

    fn list(repository: &ArchiveDirectoryRepository, location: Location) -> RequestCompletion<DirectorySnapshot> {
        let context = glib::MainContext::default();
        let request = DirectoryRequest {
            id: RequestId::allocate(),
            location,
            page: PageRequest {
                offset: 0,
                limit: usize::MAX,
            },
            cancellable: gio::Cancellable::new(),
        };
        let result = Rc::new(RefCell::new(None));
        let sink = result.clone();
        repository.list(
            request,
            Box::new(move |completion| {
                *sink.borrow_mut() = Some(completion);
            }),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        while result.borrow().is_none() {
            assert!(Instant::now() < deadline, "listing did not finish");
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        let completion = result.borrow_mut().take().unwrap();
        completion
    }

    #[gtk::test]
    fn a_local_seven_zip_browses_into_subfolders() {
        let root = tempfile::tempdir().unwrap();
        let created = archive(root.path(), ArchiveFormat::SevenZip);
        let context = glib::MainContext::default();
        let executor = GioExecutor::isolated_for_tests();
        let repository =
            ArchiveDirectoryRepository::with_executor(context.clone(), executor.clone());

        let inside = Location::from_local_path(&created).archive_contents().unwrap();
        let RequestOutcome::Succeeded(root_listing) = list(&repository, inside).outcome else {
            panic!("listing the archive root failed");
        };
        assert_eq!(root_listing.entries.len(), 1);
        assert_eq!(root_listing.entries[0].display_name, "kit");
        assert_eq!(root_listing.entries[0].kind, EntryKind::Directory);

        let kit = root_listing.entries[0].location.clone();
        let RequestOutcome::Succeeded(kit_listing) = list(&repository, kit).outcome else {
            panic!("listing the subfolder failed");
        };
        let names: Vec<_> = kit_listing
            .entries
            .iter()
            .map(|entry| entry.display_name.clone())
            .collect();
        assert_eq!(names, vec!["readme.md".to_owned(), "sub".to_owned()]);

        // The second listing reuses the cached index rather than re-reading.
        assert!(repository.index.borrow().contains_key(&created));

        repository.drain_owner_requests();
        executor.shutdown_for_tests();
    }
}
