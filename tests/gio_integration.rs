use std::{cell::RefCell, rc::Rc};
use zorin_explorer::{
    core::{contracts::*, ports::FileOperations, Location},
    infrastructure::gio_operations::GioOperations,
};

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
