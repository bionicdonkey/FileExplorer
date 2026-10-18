//! Taking entries out of an archive.
//!
//! An archive shown in the window is served by libarchive, not mounted through
//! gvfs, so a plain GIO copy has no file to read: `archive://` locations are
//! only names this application understands. This adapter sits behind the same
//! [`FileOperations`] port and, when a copy or move starts inside an archive,
//! unpacks the archive into a scratch folder beside the destination and moves
//! the selected entries into place. Everything else is handed to the GIO
//! adapter unchanged.
//!
//! Archives are read-only, so moving an entry out copies it: the archive keeps
//! its copy, which is the only honest outcome for a file the application cannot
//! write back into. Conflict decisions, progress and cancellation are honoured
//! here so the queue behaves the same as for a normal transfer.

use crate::{
    application::compression::{ReadProgress, Workspace},
    core::{contracts::*, ports::*, Location},
    infrastructure::{
        archive_directory,
        gio_executor::{CancellationSignal, GioExecutor, OwnerContextGuard},
    },
};
use futures_util::StreamExt;
use gio::prelude::*;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

/// Pure request data that may cross from the caller to the executor. Only
/// paths and names cross; every `Location` stays on the owner context.
struct JobSpec {
    capability_version: u64,
    sources: Vec<SourceSpec>,
    destination: Option<PathBuf>,
    explicit_target: Option<PathBuf>,
}

struct SourceSpec {
    archive: PathBuf,
    inner: String,
    name: String,
}

impl JobSpec {
    /// Where each source lands, in the same order as `sources`.
    fn targets(&self) -> Option<Vec<PathBuf>> {
        if self.explicit_target.is_some() && self.sources.len() != 1 {
            return None;
        }
        let mut targets = Vec::with_capacity(self.sources.len());
        for source in &self.sources {
            let target = match (&self.explicit_target, &self.destination) {
                (Some(explicit), _) => explicit.clone(),
                (None, Some(destination)) => destination.join(&source.name),
                (None, None) => return None,
            };
            targets.push(target);
        }
        Some(targets)
    }

    /// The folder the extraction scratch space is created in: the destination
    /// folder, or the parent of an explicit target, so the final move is a
    /// rename on one filesystem.
    fn workspace_parent(&self) -> Option<PathBuf> {
        match (&self.explicit_target, &self.destination) {
            (Some(explicit), _) => explicit.parent().map(Path::to_path_buf),
            (None, Some(destination)) => Some(destination.clone()),
            (None, None) => None,
        }
    }
}

/// A copy or move the archive adapter owns: every source is inside an archive
/// with a local file, and the destination is a local folder.
fn job_spec(request: &OperationRequest) -> Option<JobSpec> {
    if !matches!(request.kind, MutationKind::Copy | MutationKind::Move) {
        return None;
    }
    if request.sources.is_empty() {
        return None;
    }
    let mut sources = Vec::with_capacity(request.sources.len());
    for source in &request.sources {
        let archive = source.archive_source()?.local_path()?;
        let inner = source.archive_inner_path()?;
        let name = inner.rsplit('/').next().unwrap_or(&inner).to_owned();
        sources.push(SourceSpec {
            archive,
            inner,
            name,
        });
    }
    let destination = match &request.destination {
        Some(destination) => Some(destination.local_path()?),
        None => None,
    };
    let explicit_target = match &request.explicit_target {
        Some(target) => Some(target.local_path()?),
        None => None,
    };
    if destination.is_none() && explicit_target.is_none() {
        return None;
    }
    Some(JobSpec {
        capability_version: request.capability_version,
        sources,
        destination,
        explicit_target,
    })
}

fn handles(request: &OperationRequest) -> bool {
    job_spec(request).is_some()
}

/// What the executor reports back to the owner context. Plain data only.
enum ArchiveEvent {
    Progress {
        items: u64,
        total_items: u64,
        bytes: Option<u64>,
        total_bytes: Option<u64>,
    },
    Done(DoneKind),
}

#[derive(Debug)]
enum DoneKind {
    Succeeded {
        completed: Vec<usize>,
        remaining: Vec<usize>,
        effect: MutationEffect,
    },
    Failed {
        error_kind: OperationErrorKind,
        completed: Vec<usize>,
        remaining: Vec<usize>,
        effect: MutationEffect,
    },
    Cancelled {
        completed: Vec<usize>,
        remaining: Vec<usize>,
        effect: MutationEffect,
    },
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
pub struct ArchiveOperations {
    inner: Rc<dyn FileOperations>,
    context: glib::MainContext,
    executor: Arc<GioExecutor>,
    // Dropping the last handle waits for the executor to release every
    // reference into `context`, so the owner performs its own final unref.
    owner: OwnerContextGuard,
}

impl ArchiveOperations {
    pub fn new(
        inner: Rc<dyn FileOperations>,
        context: glib::MainContext,
        executor: Arc<GioExecutor>,
    ) -> Self {
        Self {
            inner,
            owner: OwnerContextGuard::new(context.clone()),
            context,
            executor,
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

    fn inspect(
        &self,
        request: CapabilityRequest,
        capability: Option<Capability>,
        done: CapabilityCallback,
    ) {
        if !archive_directory::serves(&request.location) {
            match capability {
                Some(capability) => self
                    .inner
                    .inspect_operation_capabilities(request, capability, done),
                None => self.inner.inspect_capabilities(request, done),
            }
            return;
        }
        let id = request.id;
        let mut snapshot = archive_directory::archive_entry_capabilities();
        snapshot.supported.insert(Capability::List);
        let outcome = match capability {
            Some(capability) if !snapshot.supports(capability) => {
                RequestOutcome::Failed(OperationError::new(
                    OperationErrorKind::Unsupported,
                    Some(&request.location),
                    None,
                ))
            }
            _ => RequestOutcome::Succeeded(snapshot),
        };
        self.context.spawn_local(async move {
            done(RequestCompletion { id, outcome });
        });
    }

    fn dispatch(
        &self,
        request: OperationRequest,
        decision: Option<ConflictDecision>,
        mut sink: OperationSink,
    ) {
        let Some(spec) = job_spec(&request) else {
            // The caller only reaches here when it asked for a transfer this
            // adapter decided it owns, so an addressable job must exist.
            let id = request.id;
            sink(OperationEvent::Failed {
                id,
                sequence: 1,
                error: OperationError::new(
                    OperationErrorKind::InvalidRequest,
                    request.sources.first(),
                    request.destination.as_ref(),
                ),
                completion: OperationCompletion {
                    remaining: request.sources.clone(),
                    ..Default::default()
                },
            });
            return;
        };
        let id = request.id;
        let count = request.sources.len();
        let sources = request.sources.clone();
        let destination = request.destination.clone().or(request.explicit_target.clone());
        let cancellation = cancellation_signal(&request.cancellable);
        let owner_cancellable = request.cancellable.clone();
        let (send, mut receive) = futures_channel::mpsc::unbounded::<ArchiveEvent>();
        let failure_send = send.clone();
        let bridge = self.owner.bridge(send);

        self.context.spawn_local(async move {
            let _owner_cancellable = owner_cancellable;
            let mut sequence = 0u64;
            sink(OperationEvent::Started {
                id,
                sequence: {
                    sequence += 1;
                    sequence
                },
            });
            while let Some(event) = receive.next().await {
                match event {
                    ArchiveEvent::Progress {
                        items,
                        total_items,
                        bytes,
                        total_bytes,
                    } => {
                        sequence += 1;
                        sink(OperationEvent::Progress {
                            id,
                            sequence,
                            progress: OperationProgress {
                                completed_items: items,
                                total_items: Some(total_items),
                                completed_bytes: bytes,
                                total_bytes,
                            },
                        });
                    }
                    ArchiveEvent::Done(done) => {
                        sequence += 1;
                        sink(done_event(
                            done,
                            id,
                            sequence,
                            &sources,
                            destination.as_ref(),
                        ));
                        return;
                    }
                }
            }
        });

        if self
            .executor
            .submit(cancellation.clone(), move |native_request| {
                let done = if cancellation.is_cancelled() {
                    DoneKind::Cancelled {
                        completed: Vec::new(),
                        remaining: (0..count).collect(),
                        effect: MutationEffect::Unchanged,
                    }
                } else {
                    run_job(
                        &spec,
                        decision,
                        &|| cancellation.is_cancelled(),
                        &mut |event| {
                            bridge.send(event);
                        },
                    )
                };
                native_request.complete();
                bridge.send(ArchiveEvent::Done(done));
            })
            .is_err()
        {
            let _ = failure_send.unbounded_send(ArchiveEvent::Done(DoneKind::Failed {
                error_kind: OperationErrorKind::Unknown,
                completed: Vec::new(),
                remaining: (0..count).collect(),
                effect: MutationEffect::Unchanged,
            }));
        }
    }
}

fn done_event(
    done: DoneKind,
    id: OperationId,
    sequence: u64,
    sources: &[Location],
    destination: Option<&Location>,
) -> OperationEvent {
    let completion = |completed: Vec<usize>, remaining: Vec<usize>, effect| OperationCompletion {
        effect,
        completed: completed
            .into_iter()
            .filter_map(|index| sources.get(index).cloned())
            .collect(),
        remaining: remaining
            .into_iter()
            .filter_map(|index| sources.get(index).cloned())
            .collect(),
    };
    match done {
        DoneKind::Succeeded {
            completed,
            remaining,
            effect,
        } => OperationEvent::Succeeded {
            id,
            sequence,
            completion: completion(completed, remaining, effect),
        },
        DoneKind::Failed {
            error_kind,
            completed,
            remaining,
            effect,
        } => OperationEvent::Failed {
            id,
            sequence,
            error: OperationError::new(error_kind, sources.first(), destination),
            completion: completion(completed, remaining, effect),
        },
        DoneKind::Cancelled {
            completed,
            remaining,
            effect,
        } => OperationEvent::Cancelled {
            id,
            sequence,
            outcome: CancellationOutcome {
                completion: completion(completed, remaining, effect),
            },
        },
    }
}

impl FileOperations for ArchiveOperations {
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
        if handles(&request) {
            self.dispatch(request, None, sink);
        } else {
            self.inner.submit(request, sink);
        }
    }

    fn submit_with_decision(
        &self,
        request: OperationRequest,
        decision: ConflictDecision,
        sink: OperationSink,
    ) {
        if handles(&request) {
            self.dispatch(request, Some(decision), sink);
        } else {
            self.inner.submit_with_decision(request, decision, sink);
        }
    }
}

/// Unpacks each archive once into a scratch folder beside the destination and
/// moves the selected entries into place. Runs on the executor thread.
fn run_job(
    spec: &JobSpec,
    decision: Option<ConflictDecision>,
    cancelled: &dyn Fn() -> bool,
    emit: &mut dyn FnMut(ArchiveEvent),
) -> DoneKind {
    let total_items = spec.sources.len() as u64;
    let mut completed: Vec<usize> = Vec::new();
    let mut remaining: Vec<usize> = (0..spec.sources.len()).collect();
    let unchanged = MutationEffect::Unchanged;

    if spec.capability_version != 1 {
        return DoneKind::Failed {
            error_kind: OperationErrorKind::CapabilityChanged,
            completed,
            remaining,
            effect: unchanged,
        };
    }
    let Some(targets) = spec.targets() else {
        return DoneKind::Failed {
            error_kind: OperationErrorKind::InvalidRequest,
            completed,
            remaining,
            effect: unchanged,
        };
    };
    let Some(workspace_parent) = spec.workspace_parent() else {
        return DoneKind::Failed {
            error_kind: OperationErrorKind::InvalidRequest,
            completed,
            remaining,
            effect: unchanged,
        };
    };
    // A conflict is reported before any work, so the queue can ask what to do
    // without having unpacked anything. KeepBoth and Replace already answer it.
    if decision.is_none() && targets.iter().any(|target| target.symlink_metadata().is_ok()) {
        return DoneKind::Failed {
            error_kind: OperationErrorKind::Conflict,
            completed,
            remaining,
            effect: unchanged,
        };
    }
    if cancelled() {
        return DoneKind::Cancelled {
            completed,
            remaining,
            effect: unchanged,
        };
    }

    let total_bytes: u64 = spec
        .sources
        .iter()
        .map(|source| {
            std::fs::metadata(&source.archive)
                .map(|meta| meta.len())
                .unwrap_or(0)
        })
        .sum();
    emit(ArchiveEvent::Progress {
        items: 0,
        total_items,
        bytes: Some(0),
        total_bytes: Some(total_bytes),
    });

    for (archive, indices) in group_by_archive(&spec.sources) {
        let workspace = match Workspace::create(&workspace_parent) {
            Ok(workspace) => workspace,
            Err(_) => {
                return DoneKind::Failed {
                    error_kind: OperationErrorKind::Unknown,
                    effect: effect_after(&completed),
                    completed,
                    remaining,
                };
            }
        };
        let unpacked = workspace.0.join("contents");
        if std::fs::create_dir(&unpacked).is_err() {
            return DoneKind::Failed {
                error_kind: OperationErrorKind::Unknown,
                effect: effect_after(&completed),
                completed,
                remaining,
            };
        }
        let archive_total = std::fs::metadata(&archive).map(|meta| meta.len()).unwrap_or(0);
        let bytes_before = total_bytes.saturating_sub(archive_total);
        let file = match std::fs::File::open(&archive) {
            Ok(file) => file,
            Err(_) => {
                return DoneKind::Failed {
                    error_kind: OperationErrorKind::NotFound,
                    effect: effect_after(&completed),
                    completed,
                    remaining,
                };
            }
        };
        {
            let mut done = 0u64;
            let moved = completed.len() as u64;
            let mut report = |read: u64, _total: u64| {
                emit(ArchiveEvent::Progress {
                    items: moved,
                    total_items,
                    bytes: Some((bytes_before + read).min(total_bytes)),
                    total_bytes: Some(total_bytes),
                });
            };
            let mut reader =
                ReadProgress::new(file, &mut done, archive_total, &mut report, cancelled);
            if compress_tools::uncompress_archive(
                &mut reader,
                &unpacked,
                compress_tools::Ownership::Ignore,
            )
            .is_err()
            {
                return if cancelled() {
                    DoneKind::Cancelled {
                        effect: effect_after(&completed),
                        completed,
                        remaining,
                    }
                } else {
                    DoneKind::Failed {
                        error_kind: OperationErrorKind::Unknown,
                        effect: effect_after(&completed),
                        completed,
                        remaining,
                    }
                };
            }
        }
        for index in indices {
            if cancelled() {
                return DoneKind::Cancelled {
                    effect: effect_after(&completed),
                    completed,
                    remaining,
                };
            }
            let source = &spec.sources[index];
            let from = unpacked.join(&source.inner);
            let mut target = targets[index].clone();
            if decision == Some(ConflictDecision::KeepBoth) && target.symlink_metadata().is_ok() {
                target = unique_target(&target);
            }
            if decision == Some(ConflictDecision::Replace) && target.symlink_metadata().is_ok() {
                let _ = remove_existing(&target);
            }
            if let Err(error) = std::fs::rename(&from, &target) {
                let error_kind = if error.kind() == std::io::ErrorKind::NotFound {
                    OperationErrorKind::NotFound
                } else {
                    OperationErrorKind::Unknown
                };
                return DoneKind::Failed {
                    error_kind,
                    effect: effect_after(&completed),
                    completed,
                    remaining,
                };
            }
            completed.push(index);
            remaining.retain(|candidate| *candidate != index);
            emit(ArchiveEvent::Progress {
                items: completed.len() as u64,
                total_items,
                bytes: Some(total_bytes),
                total_bytes: Some(total_bytes),
            });
        }
    }

    DoneKind::Succeeded {
        completed,
        remaining,
        effect: MutationEffect::Changed,
    }
}

/// Groups source indices by their archive, keeping first-seen order, so each
/// archive is unpacked once however many entries are taken from it.
fn group_by_archive(sources: &[SourceSpec]) -> Vec<(PathBuf, Vec<usize>)> {
    let mut order: Vec<PathBuf> = Vec::new();
    let mut groups: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (index, source) in sources.iter().enumerate() {
        if !groups.contains_key(&source.archive) {
            order.push(source.archive.clone());
        }
        groups.entry(source.archive.clone()).or_default().push(index);
    }
    order
        .into_iter()
        .map(|archive| {
            let indices = groups.remove(&archive).unwrap_or_default();
            (archive, indices)
        })
        .collect()
}

fn effect_after(completed: &[usize]) -> MutationEffect {
    if completed.is_empty() {
        MutationEffect::Unchanged
    } else {
        MutationEffect::PartiallyChanged
    }
}

fn unique_target(target: &Path) -> PathBuf {
    let parent = target.parent().unwrap_or_else(|| Path::new(""));
    let stem = target
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = target
        .extension()
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .unwrap_or_default();
    let mut suffix = 2;
    loop {
        let candidate = parent.join(format!("{stem} ({suffix}){extension}"));
        if candidate.symlink_metadata().is_err() {
            return candidate;
        }
        suffix += 1;
    }
}

fn remove_existing(target: &Path) {
    if target.symlink_metadata().map(|meta| meta.is_dir()).unwrap_or(false) {
        let _ = std::fs::remove_dir_all(target);
    } else {
        let _ = std::fs::remove_file(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::compression::{compress_blocking, ArchiveFormat};
    use crate::core::operation_manager::OperationManager;
    use std::cell::RefCell;

    fn archive(root: &Path) -> PathBuf {
        let source = root.join("src");
        std::fs::create_dir_all(source.join("kit/sub")).unwrap();
        std::fs::write(source.join("kit/readme.md"), "hello").unwrap();
        std::fs::write(source.join("kit/sub/inner.txt"), "inner").unwrap();
        compress_blocking(&[source.join("kit")], ArchiveFormat::SevenZip, &mut |_, _| {}, &|| {
            false
        })
        .unwrap()
    }

    fn spec(archive_path: &Path, entries: &[&str], destination: &Path) -> JobSpec {
        JobSpec {
            capability_version: 1,
            sources: entries
                .iter()
                .map(|inner| SourceSpec {
                    archive: archive_path.to_path_buf(),
                    inner: (*inner).to_owned(),
                    name: inner.rsplit('/').next().unwrap().to_owned(),
                })
                .collect(),
            destination: Some(destination.to_path_buf()),
            explicit_target: None,
        }
    }

    fn run(spec: &JobSpec, decision: Option<ConflictDecision>) -> (DoneKind, Vec<String>) {
        let seen = RefCell::new(Vec::new());
        let done = run_job(spec, decision, &|| false, &mut |event| {
            if let ArchiveEvent::Progress { items, .. } = event {
                seen.borrow_mut().push(format!("{items}"));
            }
        });
        (done, seen.into_inner())
    }

    fn completed(done: &DoneKind) -> &[usize] {
        match done {
            DoneKind::Succeeded { completed, .. }
            | DoneKind::Failed { completed, .. }
            | DoneKind::Cancelled { completed, .. } => completed,
        }
    }

    #[test]
    fn a_copy_inside_an_archive_is_owned_by_this_adapter() {
        let archive = Location::from_uri("file:///home/user/Trip.7z")
            .unwrap()
            .archive_contents()
            .unwrap();
        let entry = Location::try_from_file(
            gio::File::for_uri(archive.uri()).child("kit/readme.md"),
        )
        .unwrap();
        let destination = Location::from_uri("file:///home/user/Desktop").unwrap();
        let request = OperationRequest {
            id: OperationId(1),
            kind: MutationKind::Copy,
            sources: vec![entry.clone()],
            destination: Some(destination),
            explicit_target: None,
            capability_version: 1,
            cancellable: gio::Cancellable::new(),
        };
        assert!(handles(&request));

        // A normal local source is left to the GIO adapter.
        let plain = OperationRequest {
            kind: MutationKind::Copy,
            sources: vec![Location::from_uri("file:///home/user/notes.txt").unwrap()],
            ..request.clone()
        };
        assert!(!handles(&plain));

        // A move into an archive has no local destination, so it is not owned.
        let into_archive = OperationRequest {
            destination: Some(entry),
            ..request
        };
        assert!(!handles(&into_archive));
    }

    #[test]
    fn a_file_is_taken_out_of_a_seven_zip() {
        let root = tempfile::tempdir().unwrap();
        let created = archive(root.path());
        let out = root.path().join("out");
        std::fs::create_dir(&out).unwrap();

        let (done, _) = run(&spec(&created, &["kit/readme.md"], &out), None);

        assert_eq!(completed(&done), &[0][..], "{done:?}");
        assert_eq!(std::fs::read_to_string(out.join("readme.md")).unwrap(), "hello");
    }

    #[test]
    fn a_folder_is_taken_out_with_everything_under_it() {
        let root = tempfile::tempdir().unwrap();
        let created = archive(root.path());
        let out = root.path().join("out");
        std::fs::create_dir(&out).unwrap();

        let (done, _) = run(&spec(&created, &["kit"], &out), None);

        assert_eq!(completed(&done), &[0][..], "{done:?}");
        assert_eq!(std::fs::read_to_string(out.join("kit/readme.md")).unwrap(), "hello");
        assert_eq!(
            std::fs::read_to_string(out.join("kit/sub/inner.txt")).unwrap(),
            "inner"
        );
    }

    #[test]
    fn an_existing_target_is_a_conflict_before_anything_is_unpacked() {
        let root = tempfile::tempdir().unwrap();
        let created = archive(root.path());
        let out = root.path().join("out");
        std::fs::create_dir(&out).unwrap();
        std::fs::write(out.join("readme.md"), "old").unwrap();

        let (done, _) = run(&spec(&created, &["kit/readme.md"], &out), None);

        assert!(matches!(
            done,
            DoneKind::Failed {
                error_kind: OperationErrorKind::Conflict,
                ..
            }
        ));
        assert_eq!(std::fs::read_to_string(out.join("readme.md")).unwrap(), "old");
        // No scratch folder is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".compressing"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn keeping_both_lands_beside_the_existing_file() {
        let root = tempfile::tempdir().unwrap();
        let created = archive(root.path());
        let out = root.path().join("out");
        std::fs::create_dir(&out).unwrap();
        std::fs::write(out.join("readme.md"), "old").unwrap();

        let (done, _) = run(
            &spec(&created, &["kit/readme.md"], &out),
            Some(ConflictDecision::KeepBoth),
        );

        assert_eq!(completed(&done), &[0][..], "{done:?}");
        assert_eq!(std::fs::read_to_string(out.join("readme.md")).unwrap(), "old");
        assert_eq!(
            std::fs::read_to_string(out.join("readme (2).md")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn replacing_overwrites_the_existing_file() {
        let root = tempfile::tempdir().unwrap();
        let created = archive(root.path());
        let out = root.path().join("out");
        std::fs::create_dir(&out).unwrap();
        std::fs::write(out.join("readme.md"), "old").unwrap();

        let (done, _) = run(
            &spec(&created, &["kit/readme.md"], &out),
            Some(ConflictDecision::Replace),
        );

        assert_eq!(completed(&done), &[0][..], "{done:?}");
        assert_eq!(std::fs::read_to_string(out.join("readme.md")).unwrap(), "hello");
    }

    fn wait_for(manager: &OperationManager) -> Vec<crate::core::OperationRecord> {
        let started = std::time::Instant::now();
        while started.elapsed().as_secs() < 30 {
            let records = manager.records();
            if !records.is_empty() && records.iter().all(|record| record.finished_at.is_some()) {
                return records;
            }
            glib::MainContext::default().iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("operation did not finish: {:?}", manager.records());
    }

    #[gtk::test]
    fn taking_a_file_out_runs_through_the_operation_queue() {
        let root = tempfile::tempdir().unwrap();
        let created = archive(root.path());
        let out = root.path().join("out");
        std::fs::create_dir(&out).unwrap();

        let manager = OperationManager::default();
        let archive_root = Location::from_local_path(&created)
            .archive_contents()
            .unwrap();
        let entry =
            Location::try_from_file(gio::File::for_uri(archive_root.uri()).child("kit/readme.md"))
                .unwrap();
        manager.submit_locations(
            MutationKind::Copy,
            vec![entry],
            Some(Location::from_local_path(&out)),
            None,
            None,
            false,
        );

        let records = wait_for(&manager);

        assert!(records
            .iter()
            .all(|record| record.state == crate::core::OperationState::Completed));
        assert_eq!(std::fs::read_to_string(out.join("readme.md")).unwrap(), "hello");
    }

    #[gtk::test]
    fn moving_out_leaves_the_archive_untouched() {
        let root = tempfile::tempdir().unwrap();
        let created = archive(root.path());
        let out = root.path().join("out");
        std::fs::create_dir(&out).unwrap();

        let manager = OperationManager::default();
        let archive_root = Location::from_local_path(&created)
            .archive_contents()
            .unwrap();
        let entry =
            Location::try_from_file(gio::File::for_uri(archive_root.uri()).child("kit/readme.md"))
                .unwrap();
        manager.submit_locations(
            MutationKind::Move,
            vec![entry],
            Some(Location::from_local_path(&out)),
            None,
            None,
            false,
        );

        let records = wait_for(&manager);

        assert!(records
            .iter()
            .all(|record| record.state == crate::core::OperationState::Completed));
        assert_eq!(std::fs::read_to_string(out.join("readme.md")).unwrap(), "hello");
        // The archive is read-only: the entry is copied out, not removed.
        let index = crate::infrastructure::archive_directory::read_index_for_tests(&created);
        assert!(index.iter().any(|name| name == "kit/readme.md"));
    }
}

