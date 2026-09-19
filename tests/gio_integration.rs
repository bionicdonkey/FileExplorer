use std::{cell::RefCell, rc::Rc};
use zorin_explorer::{
    core::{contracts::*, ports::FileOperations, Location},
    infrastructure::gio_operations::GioOperations,
};

fn directory_result(
    location: Location,
    cancelled: bool,
    page: PageRequest,
) -> RequestOutcome<DirectorySnapshot> {
    use gio::prelude::*;
    use zorin_explorer::{
        core::ports::DirectoryRepository, infrastructure::gio_directory::GioDirectoryRepository,
    };
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let adapter = GioDirectoryRepository::new(context.clone());
    let received = Rc::new(RefCell::new(None));
    let out = received.clone();
    let cancel = gio::Cancellable::new();
    if cancelled {
        cancel.cancel();
    }
    let owner = std::thread::current().id();
    let callback_context = context.clone();
    adapter.list(
        DirectoryRequest {
            id: RequestId(87),
            location,
            page,
            cancellable: cancel,
        },
        Box::new(move |result| {
            assert_eq!(std::thread::current().id(), owner);
            assert!(callback_context.is_owner());
            assert_eq!(result.id, RequestId(87));
            *out.borrow_mut() = Some(result.outcome);
        }),
    );
    assert!(received.borrow().is_none(), "callback must not run inline");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while received.borrow().is_none() {
        context.iteration(false);
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    received.take().unwrap()
}

#[test]
#[cfg(unix)]
fn directory_listing_preserves_non_utf8_filename_identity() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let dir = tempfile::tempdir().unwrap();
    let raw_path = dir
        .path()
        .join(OsString::from_vec(b"name-\xff.txt".to_vec()));
    let unicode_path = dir.path().join("name-\u{fffd}.txt");
    std::fs::write(&raw_path, "raw bytes").unwrap();
    std::fs::write(&unicode_path, "unicode replacement").unwrap();
    let RequestOutcome::Succeeded(snapshot) = directory_result(
        Location::from_local_path(dir.path()),
        false,
        PageRequest {
            offset: 0,
            limit: usize::MAX,
        },
    ) else {
        panic!("listing failed")
    };
    assert_eq!(snapshot.entries.len(), 2);
    let raw = snapshot
        .entries
        .iter()
        .find(|entry| entry.location.local_path().as_ref() == Some(&raw_path))
        .expect("the raw filename must survive enumeration without replacement characters");
    let unicode = snapshot
        .entries
        .iter()
        .find(|entry| entry.location.local_path().as_ref() == Some(&unicode_path))
        .unwrap();
    assert_ne!(raw.location, unicode.location);
    assert_eq!(
        std::fs::read(raw.location.local_path().unwrap()).unwrap(),
        b"raw bytes"
    );
    assert_eq!(
        std::fs::read(unicode.location.local_path().unwrap()).unwrap(),
        b"unicode replacement"
    );
}

#[test]
fn directory_listing_returns_owned_uri_metadata_on_owner_context() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("data.txt"), "hello").unwrap();
    std::fs::create_dir(dir.path().join("folder")).unwrap();
    std::fs::write(dir.path().join(".hidden"), "x").unwrap();
    let location = Location::from_local_path(dir.path());
    let RequestOutcome::Succeeded(snapshot) = directory_result(
        location.clone(),
        false,
        PageRequest {
            offset: 0,
            limit: usize::MAX,
        },
    ) else {
        panic!("listing failed")
    };
    assert_eq!(snapshot.location.requested, location);
    assert_eq!(snapshot.location.resolved, location);
    assert_eq!(snapshot.entries.len(), 3);
    let file = snapshot
        .entries
        .iter()
        .find(|entry| entry.display_name == "data.txt")
        .unwrap();
    assert_eq!(file.size, Some(5));
    assert_eq!(file.kind, EntryKind::File);
    assert_eq!(
        file.location,
        Location::from_local_path(&dir.path().join("data.txt"))
    );
    assert!(file.capabilities.supports(Capability::Copy));
    assert!(snapshot.capabilities.supports(Capability::List));
    assert!(!snapshot.capabilities.stable_order);
    assert!(snapshot.next_page.is_none());
}

#[test]
fn directory_cancellation_errors_and_unstable_paging_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let location = Location::from_local_path(dir.path());
    assert!(matches!(
        directory_result(
            location.clone(),
            true,
            PageRequest {
                offset: 0,
                limit: usize::MAX
            }
        ),
        RequestOutcome::Cancelled
    ));
    assert!(matches!(
        directory_result(
            Location::from_local_path(&dir.path().join("missing")),
            false,
            PageRequest {
                offset: 0,
                limit: usize::MAX
            }
        ),
        RequestOutcome::Failed(OperationError {
            kind: OperationErrorKind::NotFound,
            ..
        })
    ));
    assert!(matches!(
        directory_result(
            location,
            false,
            PageRequest {
                offset: 1,
                limit: 10
            }
        ),
        RequestOutcome::Failed(OperationError {
            kind: OperationErrorKind::Unsupported,
            ..
        })
    ));
}

fn run(
    kind: MutationKind,
    sources: Vec<Location>,
    target: Option<Location>,
) -> Vec<OperationEvent> {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let adapter = GioOperations::new(context.clone());
    let received = Rc::new(RefCell::new(Vec::new()));
    let out = received.clone();
    adapter.submit(
        OperationRequest {
            id: OperationId(1),
            kind,
            sources,
            destination: None,
            explicit_target: target,
            capability_version: 1,
            cancellable: gio::Cancellable::new(),
        },
        Box::new(move |event| out.borrow_mut().push(event)),
    );
    assert!(received.borrow().is_empty());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !received
        .borrow()
        .last()
        .is_some_and(OperationEvent::is_terminal)
    {
        context.iteration(false);
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    received.take()
}
#[test]
fn local_copy_move_conflict_and_delete_preserve_actual_effects() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    let moved = dir.path().join("moved");
    std::fs::write(&source, "data").unwrap();
    let events = run(
        MutationKind::Copy,
        vec![Location::from_local_path(&source)],
        Some(Location::from_local_path(&target)),
    );
    assert!(matches!(
        events.last(),
        Some(OperationEvent::Succeeded { .. })
    ));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "data");
    std::fs::write(&target, "keep").unwrap();
    let events = run(
        MutationKind::Move,
        vec![Location::from_local_path(&source)],
        Some(Location::from_local_path(&target)),
    );
    assert!(
        matches!(events.last(), Some(OperationEvent::Failed { error, completion, .. }) if error.kind == OperationErrorKind::Conflict && completion.effect == MutationEffect::Unchanged)
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    let events = run(
        MutationKind::Move,
        vec![Location::from_local_path(&source)],
        Some(Location::from_local_path(&moved)),
    );
    assert!(matches!(
        events.last(),
        Some(OperationEvent::Succeeded { .. })
    ));
    assert!(!source.exists());
    assert!(moved.exists());
    let events = run(
        MutationKind::Delete,
        vec![
            Location::from_local_path(&moved),
            Location::from_local_path(&source),
        ],
        None,
    );
    assert!(
        matches!(events.last(), Some(OperationEvent::Failed { completion, .. }) if completion.effect == MutationEffect::PartiallyChanged && completion.completed == [Location::from_local_path(&moved)])
    );
    assert!(!moved.exists());
}
#[test]
fn local_adapter_rejects_remote_requests_without_mutation() {
    let events = run(
        MutationKind::Trash,
        vec![Location::from_uri("smb://host/share/item").unwrap()],
        None,
    );
    assert!(
        matches!(events.last(), Some(OperationEvent::Failed { error, completion, .. }) if error.kind == OperationErrorKind::Unsupported && completion.effect == MutationEffect::Unchanged)
    );
}

#[test]
fn explicit_replace_and_keep_both_are_applied_by_the_adapter() {
    use zorin_explorer::core::contracts::ConflictDecision;
    for decision in [ConflictDecision::Replace, ConflictDecision::KeepBoth] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let target = directory.path().join("target");
        std::fs::write(&source, "new").unwrap();
        std::fs::write(&target, "old").unwrap();
        let request = OperationRequest {
            id: OperationId(1),
            kind: MutationKind::Copy,
            sources: vec![Location::from_local_path(&source)],
            destination: None,
            explicit_target: Some(Location::from_local_path(&target)),
            capability_version: 1,
            cancellable: gio::Cancellable::new(),
        };
        let mut events = Vec::new();
        zorin_explorer::infrastructure::gio_operations::execute_with_decision(
            request,
            decision,
            &mut |e| events.push(e),
        );
        assert!(matches!(
            events.last(),
            Some(OperationEvent::Succeeded { .. })
        ));
        if decision == ConflictDecision::Replace {
            assert_eq!(std::fs::read_to_string(target).unwrap(), "new");
        } else {
            assert_eq!(std::fs::read_to_string(target).unwrap(), "old");
            assert_eq!(
                std::fs::read_to_string(directory.path().join("target (2)")).unwrap(),
                "new"
            );
        }
    }
}

#[test]
fn capability_inspection_reports_missing_parent_instead_of_claiming_support() {
    let dir = tempfile::tempdir().unwrap();
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let adapter = GioOperations::new(context.clone());
    let result = Rc::new(RefCell::new(None));
    let received = result.clone();
    adapter.inspect_capabilities(
        CapabilityRequest {
            id: RequestId(1),
            location: Location::from_local_path(&dir.path().join("absent/child")),
            cancellable: gio::Cancellable::new(),
        },
        Box::new(move |outcome| *received.borrow_mut() = Some(outcome)),
    );
    while result.borrow().is_none() {
        context.iteration(true);
    }
    assert!(
        matches!(&result.borrow().as_ref().unwrap().outcome, RequestOutcome::Failed(error) if error.kind == OperationErrorKind::NotFound)
    );
}

#[cfg(unix)]
#[test]
fn local_containment_rejects_a_symlink_back_into_the_source_without_creating_a_target() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let alias = directory.path().join("alias");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("a"), "x").unwrap();
    std::os::unix::fs::symlink(&source, &alias).unwrap();
    let target = alias.join("nested");
    let cancel = gio::Cancellable::new();
    let shared = cancel.clone();
    let request = OperationRequest {
        id: OperationId(1),
        kind: MutationKind::Copy,
        sources: vec![Location::from_local_path(&source)],
        destination: None,
        explicit_target: Some(Location::from_local_path(&target)),
        capability_version: 1,
        cancellable: cancel,
    };
    let mut events = Vec::new();
    zorin_explorer::infrastructure::gio_operations::execute(request, &mut |event| {
        // Bound the broken implementation: stop once its first write happens.
        if matches!(event, OperationEvent::Progress { .. }) {
            use gio::prelude::*;
            shared.cancel();
        }
        events.push(event);
    });
    assert!(
        matches!(events.last(), Some(OperationEvent::Failed { error, completion, .. }) if error.kind == OperationErrorKind::InvalidRequest && completion.effect == MutationEffect::Unchanged)
    );
    assert!(!target.exists());
}

#[cfg(unix)]
#[test]
fn denied_local_source_and_destination_remain_eligible_for_linked_elevation() {
    use std::os::unix::fs::PermissionsExt;
    use zorin_explorer::application::operations::{
        elevated_retry_request, ChangeExtent, OperationCoordinator, OperationState,
    };
    for restrict_source in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        std::fs::write(&source, "private").unwrap();
        std::fs::create_dir(&destination).unwrap();
        let restricted = if restrict_source {
            &source
        } else {
            &destination
        };
        let original = std::fs::metadata(restricted).unwrap().permissions();
        std::fs::set_permissions(
            restricted,
            std::fs::Permissions::from_mode(if restrict_source { 0 } else { 0o555 }),
        )
        .unwrap();
        if restrict_source && std::fs::read(&source).is_ok() {
            std::fs::set_permissions(restricted, original).unwrap();
            eprintln!("permission-restriction scenario unavailable to a process that can read mode-000 files");
            return;
        }
        let context = glib::MainContext::new();
        let _guard = context.acquire().unwrap();
        let coordinator = OperationCoordinator::new(Rc::new(GioOperations::new(context.clone())));
        coordinator
            .submit(
                OperationRequest {
                    id: OperationId(1),
                    kind: MutationKind::Copy,
                    sources: vec![Location::from_local_path(&source)],
                    destination: Some(Location::from_local_path(&destination)),
                    explicit_target: None,
                    capability_version: 1,
                    cancellable: gio::Cancellable::new(),
                },
                GioOperations::local_capabilities(),
            )
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !coordinator.records()[0]
            .events
            .last()
            .is_some_and(OperationEvent::is_terminal)
        {
            context.iteration(false);
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        std::fs::set_permissions(restricted, original).unwrap();
        let result = coordinator.records().remove(0);
        assert!(
            matches!(&result.state, OperationState::Failed(error) if error.kind == OperationErrorKind::PermissionDenied),
            "{result:?}"
        );
        assert_eq!(result.change, ChangeExtent::Unchanged);
        assert!(elevated_retry_request(&result, OperationId(2)).is_ok());
        assert!(!destination.join("source").exists());
        assert!(!result
            .events
            .iter()
            .any(|e| matches!(e, OperationEvent::Started { .. })));
    }
}
