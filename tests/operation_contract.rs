use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gio::prelude::*;
use zorin_explorer::core::contracts::*;
use zorin_explorer::core::ports::*;
use zorin_explorer::core::Location;
use zorin_explorer::infrastructure::mock_provider::*;

mod operation_contract {
    use super::*;
    use zorin_explorer::application::operations::{
        CancelResult, ChangeExtent, OperationCoordinator, OperationState,
    };

    #[test]
    fn containment_locks_and_cancel_race_preserve_backend_outcome() {
        let context = glib::MainContext::new();
        let _guard = context.acquire().unwrap();
        let mock = provider(&context);
        mock.set_capabilities(capabilities(1));
        mock.set_operation_behavior(MockOperationBehavior::SucceedDespiteCancellation);
        let coordinator = OperationCoordinator::new(Rc::new(mock.clone()));
        let mut recursive = operation(90, 1);
        recursive.sources = vec![Location::from_uri("file:///a").unwrap()];
        recursive.destination = Some(Location::from_uri("file:///a/child").unwrap());
        assert!(coordinator.submit(recursive, capabilities(1)).is_err());
        let id = coordinator
            .submit(operation(1, 1), capabilities(1))
            .unwrap();
        let second = coordinator
            .submit(operation(2, 1), capabilities(1))
            .unwrap();
        for _ in 0..4 {
            mock.advance(2);
            pump(&context);
        }
        assert_eq!(coordinator.records()[1].state, OperationState::Queued);
        assert!(!coordinator.active_lock_keys().is_empty());
        assert_eq!(coordinator.cancel(id), CancelResult::Requested);
        assert_eq!(
            coordinator.records()[0].state,
            OperationState::CancelRequested
        );
        assert!(!coordinator.active_lock_keys().is_empty());
        assert_eq!(
            coordinator.cancel(second),
            CancelResult::CancelledBeforeStart
        );
        for _ in 0..12 {
            mock.advance(2);
            pump(&context);
        }
        let records = coordinator.records();
        assert_eq!(records[0].state, OperationState::Succeeded);
        assert_eq!(
            records[0].events.iter().filter(|e| e.is_terminal()).count(),
            1
        );
        assert!(coordinator.active_lock_keys().is_empty());
    }

    #[test]
    fn coordinator_retains_partial_results_and_rejects_unconfirmed_delete() {
        let context = glib::MainContext::new();
        let _guard = context.acquire().unwrap();
        let mock = provider(&context);
        mock.set_capabilities(capabilities(1));
        mock.set_operation_behavior(MockOperationBehavior::FailAfter {
            completed: 1,
            kind: OperationErrorKind::PermissionDenied,
        });
        let coordinator = OperationCoordinator::new(Rc::new(mock.clone()));
        coordinator
            .submit(operation(1, 3), capabilities(1))
            .unwrap();
        let mut delete = operation(2, 1);
        delete.kind = MutationKind::Delete;
        assert!(coordinator.submit(delete, capabilities(1)).is_err());
        for _ in 0..20 {
            mock.advance(2);
            pump(&context);
        }
        let records = coordinator.records();
        assert_eq!(records[0].change, ChangeExtent::Partial);
        assert_eq!(records[0].completion.completed, [location("source-0")]);
        assert!(matches!(records[0].state, OperationState::Failed(_)));
    }

    #[test]
    fn elevated_retry_requires_unchanged_local_permission_failure_and_a_new_id() {
        use zorin_explorer::application::operations::elevated_retry_request;
        let context = glib::MainContext::new();
        let _guard = context.acquire().unwrap();
        let mock = provider(&context);
        mock.set_capabilities(capabilities(1));
        mock.set_operation_behavior(MockOperationBehavior::FailAfter {
            completed: 0,
            kind: OperationErrorKind::PermissionDenied,
        });
        let coordinator = OperationCoordinator::new(Rc::new(mock.clone()));
        let mut request = operation(1, 1);
        request.sources = vec![Location::from_uri("file:///source").unwrap()];
        request.destination = Some(Location::from_uri("file:///destination").unwrap());
        coordinator.submit(request, capabilities(1)).unwrap();
        for _ in 0..12 {
            mock.advance(2);
            pump(&context);
        }
        let original = coordinator.records().remove(0);
        let retry = elevated_retry_request(&original, OperationId(2)).unwrap();
        assert_eq!(
            retry.sources,
            [Location::from_uri("file:///source").unwrap()]
        );
        assert!(elevated_retry_request(&original, OperationId(1)).is_err());
        let mut remote = original.clone();
        remote.request.sources = vec![location("remote")];
        assert!(elevated_retry_request(&remote, OperationId(3)).is_err());
        let mut uncertain = original.clone();
        uncertain.change = ChangeExtent::Unknown;
        assert!(elevated_retry_request(&uncertain, OperationId(3)).is_err());
        mock.set_operation_behavior(MockOperationBehavior::Succeed);
        coordinator
            .submit_linked_retry(retry, capabilities(1), original.id)
            .unwrap();
        for _ in 0..12 {
            mock.advance(2);
            pump(&context);
        }
        let records = coordinator.records();
        assert_eq!(records[0].state, original.state);
        assert_eq!(records[0].events, original.events);
        assert_eq!(records[1].retry_of, Some(original.id));
        assert_eq!(records[1].state, OperationState::Succeeded);
    }

    #[test]
    fn skip_and_cancel_remaining_never_reach_the_backend() {
        for decision in [ConflictDecision::Skip, ConflictDecision::CancelRemaining] {
            let context = glib::MainContext::new();
            let _guard = context.acquire().unwrap();
            let mock = provider(&context);
            mock.set_capabilities(capabilities(1));
            let coordinator = OperationCoordinator::new(Rc::new(mock.clone()));
            coordinator
                .submit_with_decision(operation(1, 1), capabilities(1), decision)
                .unwrap();
            for _ in 0..12 {
                mock.advance(2);
                pump(&context);
            }
            let p = coordinator.records().remove(0);
            assert_eq!(p.change, ChangeExtent::Unchanged);
            assert!(!p
                .events
                .iter()
                .any(|e| matches!(e, OperationEvent::Started { .. })));
            assert_eq!(p.completion.remaining, [location("source-0")]);
            assert_eq!(p.events.len(), 1);
        }
    }

    #[test]
    fn opaque_request_conflicts_with_a_trusted_request_and_local_keys_are_sorted() {
        use zorin_explorer::application::operations::lock_keys;
        let context = glib::MainContext::new();
        let _guard = context.acquire().unwrap();
        let mock = provider(&context);
        mock.set_capabilities(capabilities(1));
        let coordinator = OperationCoordinator::new(Rc::new(mock.clone()));
        coordinator
            .submit(operation(1, 3), capabilities(1))
            .unwrap();
        let mut other = operation(2, 1);
        other.sources = vec![location("different")];
        other.destination = Some(location("elsewhere"));
        let mut trusted = capabilities(1);
        trusted.containment = Containment::KnownAncestors(vec![location("ancestor")]);
        coordinator.submit(other, trusted).unwrap();
        for _ in 0..5 {
            mock.advance(2);
            pump(&context);
        }
        assert_eq!(coordinator.records()[1].state, OperationState::Queued);
        let mut request = operation(3, 1);
        request.sources = vec![Location::from_uri("file:///tmp/a").unwrap()];
        request.destination = Some(Location::from_uri("file:///var/out").unwrap());
        assert_eq!(
            lock_keys(&request, &capabilities(1)).unwrap(),
            [
                "file:///",
                "file:///tmp",
                "file:///tmp/a",
                "file:///var",
                "file:///var/out",
                "file:///var/out/a"
            ]
        );
    }
}

fn location(name: &str) -> Location {
    Location::from_uri(&format!("mock://host/{name}")).unwrap()
}

fn capabilities(version: u64) -> CapabilitySnapshot {
    CapabilitySnapshot {
        version,
        supported: [
            Capability::List,
            Capability::Copy,
            Capability::Create,
            Capability::SearchFilename,
        ]
        .into_iter()
        .collect(),
        stable_order: true,
        containment: Containment::Opaque,
    }
}

fn entry(name: &str) -> EntrySnapshot {
    EntrySnapshot {
        location: location(name),
        display_name: name.into(),
        kind: EntryKind::File,
        size: None,
        modified: None,
        content_type: None,
        capabilities: capabilities(1),
    }
}

fn provider(context: &glib::MainContext) -> MockProvider {
    MockProvider::new(
        context.clone(),
        MockConfig {
            latency_ticks: 2,
            ..Default::default()
        },
    )
}

fn pump(context: &glib::MainContext) {
    while context.pending() {
        context.iteration(false);
    }
}

fn finish_search(
    mock: &MockProvider,
    context: &glib::MainContext,
    results: &RefCell<Vec<SearchCompletion>>,
) {
    for _ in 0..32 {
        mock.advance(2);
        pump(context);
        if !results.borrow().is_empty() {
            return;
        }
    }
    panic!("small search fixture did not reach a terminal outcome");
}

fn operation(id: u64, count: usize) -> OperationRequest {
    OperationRequest {
        id: OperationId(id),
        kind: MutationKind::Copy,
        sources: (0..count)
            .map(|i| location(&format!("source-{i}")))
            .collect(),
        destination: Some(location("destination")),
        explicit_target: None,
        capability_version: 1,
        cancellable: gio::Cancellable::new(),
    }
}

#[test]
fn reducer_rejects_duplicate_out_of_order_wrong_id_and_post_terminal_events() {
    let id = OperationId(7);
    let mut reducer = OperationReducer::new(id);
    assert!(reducer.apply(OperationEvent::Started { id, sequence: 1 }));
    assert!(!reducer.apply(OperationEvent::Started { id, sequence: 1 }));
    assert!(!reducer.apply(OperationEvent::Started {
        id: OperationId(8),
        sequence: 2
    }));
    assert!(reducer.apply(OperationEvent::Progress {
        id,
        sequence: 3,
        progress: OperationProgress::default()
    }));
    assert!(!reducer.apply(OperationEvent::Progress {
        id,
        sequence: 2,
        progress: OperationProgress::default()
    }));
    assert!(reducer.apply(OperationEvent::Succeeded {
        id,
        sequence: 4,
        completion: OperationCompletion::default()
    }));
    assert!(!reducer.apply(OperationEvent::Progress {
        id,
        sequence: 5,
        progress: OperationProgress::default()
    }));
    assert!(!reducer.apply(OperationEvent::Succeeded {
        id,
        sequence: 6,
        completion: OperationCompletion::default()
    }));
    assert_eq!(reducer.events().len(), 3);
    assert!(reducer.is_terminal());
}

#[test]
fn cancellation_request_is_not_terminal_and_backend_success_wins_the_race() {
    let id = OperationId(1);
    let mut reducer = OperationReducer::new(id);
    assert!(reducer.apply(OperationEvent::Started { id, sequence: 1 }));
    assert!(reducer.apply(OperationEvent::CancelRequested { id, sequence: 2 }));
    assert!(!reducer.is_terminal());
    assert!(reducer.apply(OperationEvent::Succeeded {
        id,
        sequence: 3,
        completion: OperationCompletion::default()
    }));
    assert!(reducer.is_terminal());
}

#[test]
fn structured_gio_error_preserves_code_and_redacts_backend_message_and_paths() {
    let raw = glib::Error::new(
        gio::IOErrorEnum::PermissionDenied,
        "secret command /private/password",
    );
    let error = OperationError::from_gio(
        &raw,
        Some(&location("private/source")),
        Some(&location("private/destination")),
    );
    assert_eq!(error.kind, OperationErrorKind::PermissionDenied);
    assert_eq!(error.gio_domain, Some(raw.domain()));
    assert_eq!(
        error.gio_code,
        Some(gio::IOErrorEnum::PermissionDenied.into_glib())
    );
    assert_eq!(error.source.as_deref(), Some("mock://host/…"));
    assert_eq!(error.destination.as_deref(), Some("mock://host/…"));
    assert_eq!(error.retry, RetryClass::AfterUserAction);
    assert!(!format!("{error:?} {error}").contains("private"));
    assert!(!format!("{error:?} {error}").contains("password"));
}

use glib::translate::IntoGlib;

#[test]
fn active_search_cancellation_retains_first_accepted_match_and_completes_once() {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let mock = provider(&context);
    mock.set_directory(
        location("root"),
        vec![entry("a.txt"), entry("b.txt"), entry("c.txt")],
        capabilities(1),
    );
    let scope = SearchScope {
        root: location("root"),
        max_depth: 1,
        max_results: 100,
        timeout: Duration::from_secs(2),
    };
    let cancel = gio::Cancellable::new();
    let results = Rc::new(RefCell::new(Vec::new()));
    let received = results.clone();
    let owner = context.clone();
    mock.search(
        SearchRequest {
            id: RequestId(42),
            scope: scope.clone(),
            query: "txt".into(),
            mode: SearchMode::Filename,
            cancellable: cancel.clone(),
        },
        Box::new(move |result| {
            assert!(owner.is_owner());
            received.borrow_mut().push(result);
        }),
    );
    // The first virtual stage accepts a.txt and yields before inspecting b.txt.
    mock.advance(2);
    pump(&context);
    assert!(
        results.borrow().is_empty(),
        "active search must yield before terminal completion"
    );
    cancel.cancel();
    mock.advance(2);
    assert!(
        results.borrow().is_empty(),
        "acknowledgement must be dispatched on the main context"
    );
    pump(&context);
    for _ in 0..5 {
        mock.advance(2);
        pump(&context);
    }
    let results = results.borrow();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, RequestId(42));
    let SearchOutcome::Incomplete {
        matches,
        reason,
        declared_scope,
        retry,
        ..
    } = &results[0].outcome
    else {
        panic!("expected acknowledged active cancellation");
    };
    assert_eq!(*reason, SearchIncompleteReason::Cancelled);
    assert_eq!(*declared_scope, scope);
    assert_eq!(*retry, RetryClass::AfterRefresh);
    assert_eq!(
        matches
            .iter()
            .map(|entry| entry.location.clone())
            .collect::<Vec<_>>(),
        [location("a.txt")]
    );
}

#[test]
fn search_time_bound_keeps_only_items_accepted_before_the_deadline() {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let mock = provider(&context);
    mock.set_directory(
        location("root"),
        vec![entry("a"), entry("b"), entry("c")],
        capabilities(1),
    );
    let outcomes = Rc::new(RefCell::new(Vec::new()));
    let received = outcomes.clone();
    mock.search(
        SearchRequest {
            id: RequestId(1),
            scope: SearchScope {
                root: location("root"),
                max_depth: 1,
                max_results: 100,
                timeout: Duration::from_micros(1500),
            },
            query: "".into(),
            mode: SearchMode::Filename,
            cancellable: gio::Cancellable::new(),
        },
        Box::new(move |result| received.borrow_mut().push(result)),
    );
    finish_search(&mock, &context, &outcomes);
    let SearchOutcome::Completed {
        matches, limits, ..
    } = outcomes.borrow_mut().pop().unwrap().outcome
    else {
        panic!("time bound is successful scoped completion");
    };
    assert_eq!(
        matches
            .iter()
            .map(|entry| entry.display_name.as_str())
            .collect::<Vec<_>>(),
        ["a"]
    );
    assert!(limits.contains(&SearchLimit::Time));
}

#[test]
fn losing_stable_pages_during_search_retains_previously_accepted_matches() {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let mock = provider(&context);
    let mut folder = entry("subfolder");
    folder.kind = EntryKind::Directory;
    mock.set_directory(
        location("root"),
        vec![entry("a.txt"), folder],
        capabilities(1),
    );
    let mut unstable = capabilities(1);
    unstable.stable_order = false;
    mock.set_directory(location("subfolder"), vec![entry("b.txt")], unstable);
    let outcomes = Rc::new(RefCell::new(Vec::new()));
    let received = outcomes.clone();
    mock.search(
        SearchRequest {
            id: RequestId(1),
            scope: SearchScope {
                root: location("root"),
                max_depth: 2,
                max_results: 100,
                timeout: Duration::from_secs(2),
            },
            query: "txt".into(),
            mode: SearchMode::Filename,
            cancellable: gio::Cancellable::new(),
        },
        Box::new(move |result| received.borrow_mut().push(result)),
    );
    finish_search(&mock, &context, &outcomes);
    let SearchOutcome::Incomplete {
        matches, reason, ..
    } = outcomes.borrow_mut().pop().unwrap().outcome
    else {
        panic!("mid-search provider contract failure must retain results");
    };
    assert_eq!(reason, SearchIncompleteReason::MalformedPage);
    assert_eq!(matches[0].location, location("a.txt"));
}

#[test]
fn invalid_pages_and_redirect_cycles_complete_once_with_safe_errors() {
    for cycle in [false, true] {
        let context = glib::MainContext::new();
        let _guard = context.acquire().unwrap();
        let mock = provider(&context);
        mock.set_directory(location("root"), vec![entry("a")], capabilities(1));
        if cycle {
            mock.set_redirect(location("root"), location("loop"));
            mock.set_redirect(location("loop"), location("root"));
        }
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let received = outcomes.clone();
        mock.list(
            DirectoryRequest {
                id: RequestId(1),
                location: location("root"),
                page: PageRequest {
                    offset: 0,
                    limit: if cycle { 1 } else { 0 },
                },
                cancellable: gio::Cancellable::new(),
            },
            Box::new(move |result| received.borrow_mut().push(result)),
        );
        mock.advance(2);
        pump(&context);
        mock.advance(100);
        pump(&context);
        assert_eq!(outcomes.borrow().len(), 1);
        assert!(
            matches!(&outcomes.borrow()[0].outcome, RequestOutcome::Failed(error) if error.kind == OperationErrorKind::InvalidRequest)
        );
    }
}

#[test]
fn error_classification_keeps_distinct_gio_failures_and_unknown_domain_codes() {
    for (code, kind) in [
        (
            gio::IOErrorEnum::NotMounted,
            OperationErrorKind::MountUnavailable,
        ),
        (gio::IOErrorEnum::Exists, OperationErrorKind::Conflict),
        (gio::IOErrorEnum::NotFound, OperationErrorKind::NotFound),
        (gio::IOErrorEnum::ReadOnly, OperationErrorKind::ReadOnly),
        (
            gio::IOErrorEnum::NotSupported,
            OperationErrorKind::Unsupported,
        ),
    ] {
        let raw = glib::Error::new(code, "sensitive transport text");
        assert_eq!(OperationError::from_gio(&raw, None, None).kind, kind);
    }
    let raw = glib::Error::new(glib::FileError::Failed, "sensitive transport text");
    let safe = OperationError::from_gio(&raw, None, None);
    assert_eq!(safe.kind, OperationErrorKind::Unknown);
    assert_eq!(safe.gio_domain, Some(raw.domain()));
    assert_eq!(safe.gio_code, Some(glib::FileError::Failed.into_glib()));
    assert!(!format!("{safe:?}").contains("sensitive"));
}

#[test]
fn directory_pages_are_async_stable_redirected_and_correlated() {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let mock = provider(&context);
    mock.set_directory(
        location("resolved"),
        vec![entry("c"), entry("a"), entry("b")],
        capabilities(1),
    );
    mock.set_redirect(location("requested"), location("resolved"));
    let repository: &dyn DirectoryRepository = &mock;
    let results = Rc::new(RefCell::new(Vec::new()));
    let received = results.clone();
    let owner = context.clone();
    repository.list(
        DirectoryRequest {
            id: RequestId(11),
            location: location("requested"),
            page: PageRequest {
                offset: 0,
                limit: 2,
            },
            cancellable: gio::Cancellable::new(),
        },
        Box::new(move |result| {
            assert!(owner.is_owner());
            received.borrow_mut().push(result);
        }),
    );
    mock.advance(1);
    pump(&context);
    assert!(
        results.borrow().is_empty(),
        "virtual latency must not complete early"
    );
    mock.advance(1);
    assert!(
        results.borrow().is_empty(),
        "advance must not invoke application callbacks inline"
    );
    pump(&context);
    let result = results.borrow_mut().pop().unwrap();
    assert_eq!(result.id, RequestId(11));
    let RequestOutcome::Succeeded(snapshot) = result.outcome else {
        panic!("expected listing");
    };
    assert_eq!(snapshot.location.requested, location("requested"));
    assert_eq!(snapshot.location.resolved, location("resolved"));
    assert_eq!(
        snapshot
            .entries
            .iter()
            .map(|entry| entry.display_name.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(
        snapshot.next_page,
        Some(PageRequest {
            offset: 2,
            limit: 2
        })
    );
    let received = results.clone();
    repository.list(
        DirectoryRequest {
            id: RequestId(12),
            location: location("requested"),
            page: snapshot.next_page.unwrap(),
            cancellable: gio::Cancellable::new(),
        },
        Box::new(move |result| received.borrow_mut().push(result)),
    );
    mock.advance(2);
    pump(&context);
    let RequestOutcome::Succeeded(snapshot) = results.borrow_mut().pop().unwrap().outcome else {
        panic!("expected listing");
    };
    assert_eq!(snapshot.entries[0].display_name, "c");
    assert_eq!(snapshot.next_page, None);
    mock.advance(100);
    pump(&context);
    assert!(
        results.borrow().is_empty(),
        "completion must occur only once"
    );
}

#[test]
fn cancelled_listing_acknowledges_once_even_after_it_is_ready_for_dispatch() {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let mock = provider(&context);
    let cancellable = gio::Cancellable::new();
    let results = Rc::new(RefCell::new(Vec::new()));
    let received = results.clone();
    mock.list(
        DirectoryRequest {
            id: RequestId(1),
            location: location("any"),
            page: PageRequest {
                offset: 0,
                limit: 2,
            },
            cancellable: cancellable.clone(),
        },
        Box::new(move |result| received.borrow_mut().push(result)),
    );
    mock.advance(2);
    cancellable.cancel();
    pump(&context);
    mock.advance(100);
    pump(&context);
    assert_eq!(results.borrow().len(), 1);
    assert!(matches!(
        results.borrow()[0].outcome,
        RequestOutcome::Cancelled
    ));
}

#[test]
fn execution_rechecks_changed_capabilities_before_reporting_any_work() {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let mock = provider(&context);
    mock.set_capabilities(capabilities(1));
    let events = Rc::new(RefCell::new(Vec::new()));
    let received = events.clone();
    let operations: &dyn FileOperations = &mock;
    operations.submit(
        operation(1, 2),
        Box::new(move |event| received.borrow_mut().push(event)),
    );
    mock.set_capabilities(capabilities(2));
    mock.advance(2);
    pump(&context);
    assert_eq!(events.borrow().len(), 1);
    let OperationEvent::Failed {
        error, completion, ..
    } = &events.borrow()[0]
    else {
        panic!("expected capability failure");
    };
    assert_eq!(error.kind, OperationErrorKind::CapabilityChanged);
    assert_eq!(completion.effect, MutationEffect::Unchanged);
    assert!(completion.completed.is_empty());
}

#[test]
fn unsupported_action_is_rejected_even_with_matching_capability_version() {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let mock = provider(&context);
    mock.set_capabilities(capabilities(1));
    let mut request = operation(1, 1);
    request.kind = MutationKind::Trash;
    let events = Rc::new(RefCell::new(Vec::new()));
    let received = events.clone();
    mock.submit(
        request,
        Box::new(move |event| received.borrow_mut().push(event)),
    );
    mock.advance(2);
    pump(&context);
    assert!(
        matches!(&events.borrow()[0], OperationEvent::Failed { error, .. } if error.kind == OperationErrorKind::CapabilityChanged)
    );
}

#[test]
fn partial_failure_retains_completed_items_and_has_one_terminal_event() {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let mock = provider(&context);
    mock.set_capabilities(capabilities(1));
    mock.set_operation_behavior(MockOperationBehavior::FailAfter {
        completed: 1,
        kind: OperationErrorKind::PermissionDenied,
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let received = events.clone();
    mock.submit(
        operation(4, 3),
        Box::new(move |event| received.borrow_mut().push(event)),
    );
    for _ in 0..6 {
        mock.advance(2);
        pump(&context);
    }
    let events = events.borrow();
    assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
    let OperationEvent::Failed {
        error, completion, ..
    } = events.last().unwrap()
    else {
        panic!("expected partial failure");
    };
    assert_eq!(error.kind, OperationErrorKind::PermissionDenied);
    assert_eq!(completion.effect, MutationEffect::PartiallyChanged);
    assert_eq!(completion.completed, [location("source-0")]);
    assert_eq!(
        completion.remaining,
        [location("source-1"), location("source-2")]
    );
    assert!(events
        .windows(2)
        .all(|pair| pair[0].sequence() < pair[1].sequence()));
}

#[test]
fn running_cancellation_waits_for_acknowledgement_and_reports_actual_partial_effects() {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let mock = provider(&context);
    mock.set_capabilities(capabilities(1));
    let request = operation(8, 3);
    let cancel = request.cancellable.clone();
    let events = Rc::new(RefCell::new(Vec::new()));
    let received = events.clone();
    mock.submit(
        request,
        Box::new(move |event| received.borrow_mut().push(event)),
    );
    mock.advance(2);
    pump(&context);
    mock.advance(2);
    pump(&context);
    cancel.cancel();
    assert!(!events.borrow().iter().any(OperationEvent::is_terminal));
    mock.advance(1);
    pump(&context);
    assert!(!events.borrow().iter().any(OperationEvent::is_terminal));
    mock.advance(1);
    pump(&context);
    for _ in 0..5 {
        mock.advance(2);
        pump(&context);
    }
    let events = events.borrow();
    assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
    let OperationEvent::Cancelled { outcome, .. } = events.last().unwrap() else {
        panic!("expected cancellation acknowledgement");
    };
    assert_eq!(outcome.completion.effect, MutationEffect::PartiallyChanged);
    assert_eq!(outcome.completion.completed, [location("source-0")]);
}

#[test]
fn search_port_returns_uri_matches_and_declared_scope_or_unsupported() {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let mock = provider(&context);
    mock.set_directory(
        location("root"),
        vec![entry("b.txt"), entry("a.txt"), entry("z.png")],
        capabilities(1),
    );
    let scope = SearchScope {
        root: location("root"),
        max_depth: 1,
        max_results: 1,
        timeout: Duration::from_secs(2),
    };
    let results = Rc::new(RefCell::new(Vec::new()));
    let received = results.clone();
    let repository: &dyn SearchRepository = &mock;
    repository.search(
        SearchRequest {
            id: RequestId(9),
            scope: scope.clone(),
            query: "txt".into(),
            mode: SearchMode::Filename,
            cancellable: gio::Cancellable::new(),
        },
        Box::new(move |result| received.borrow_mut().push(result)),
    );
    finish_search(&mock, &context, &results);
    let result = results.borrow_mut().pop().unwrap();
    assert_eq!(result.id, RequestId(9));
    let SearchOutcome::Completed {
        matches,
        declared_scope,
        scope_limited,
        limits,
    } = result.outcome
    else {
        panic!("expected search completion");
    };
    assert_eq!(
        matches
            .iter()
            .map(|entry| entry.location.clone())
            .collect::<Vec<_>>(),
        [location("a.txt")]
    );
    assert_eq!(declared_scope, scope);
    assert!(scope_limited);
    assert!(limits.contains(&SearchLimit::ResultCount));
    let received = results.clone();
    repository.search(
        SearchRequest {
            id: RequestId(10),
            scope,
            query: "txt".into(),
            mode: SearchMode::Content,
            cancellable: gio::Cancellable::new(),
        },
        Box::new(move |result| received.borrow_mut().push(result)),
    );
    mock.advance(2);
    pump(&context);
    assert!(matches!(
        results.borrow()[0].outcome,
        SearchOutcome::Unsupported { .. }
    ));
}

#[test]
fn execution_rechecks_each_source_and_destination_snapshot() {
    for changed in ["source-0", "destination"] {
        let context = glib::MainContext::new();
        let _guard = context.acquire().unwrap();
        let mock = provider(&context);
        mock.set_capabilities(capabilities(1));
        mock.set_directory(location(changed), Vec::new(), capabilities(1));
        let events = Rc::new(RefCell::new(Vec::new()));
        let received = events.clone();
        mock.submit(
            operation(1, 1),
            Box::new(move |event| received.borrow_mut().push(event)),
        );
        mock.set_directory(location(changed), Vec::new(), capabilities(2));
        mock.advance(2);
        pump(&context);
        assert!(
            matches!(&events.borrow()[0], OperationEvent::Failed { error, .. } if error.kind == OperationErrorKind::CapabilityChanged),
            "must recheck {changed}"
        );
    }
}

#[test]
fn cancellation_after_last_item_reports_all_effects_as_changed() {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let mock = provider(&context);
    mock.set_capabilities(capabilities(1));
    let request = operation(1, 1);
    let cancel = request.cancellable.clone();
    let events = Rc::new(RefCell::new(Vec::new()));
    let received = events.clone();
    mock.submit(
        request,
        Box::new(move |event| received.borrow_mut().push(event)),
    );
    mock.advance(2);
    pump(&context);
    mock.advance(2);
    pump(&context);
    cancel.cancel();
    mock.advance(2);
    pump(&context);
    let events = events.borrow();
    let OperationEvent::Cancelled { outcome, .. } = events.last().unwrap() else {
        panic!("expected acknowledgement");
    };
    assert_eq!(outcome.completion.effect, MutationEffect::Changed);
    assert_eq!(outcome.completion.completed, [location("source-0")]);
    assert!(outcome.completion.remaining.is_empty());
}

#[test]
fn search_failure_and_cancellation_retain_the_scope_and_accepted_results() {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let mock = provider(&context);
    mock.set_directory(
        location("root"),
        vec![entry("a.txt"), entry("b.txt")],
        capabilities(1),
    );
    mock.set_search_behavior(MockSearchBehavior::FailAfter {
        matches: 1,
        reason: SearchIncompleteReason::ProviderFailure,
    });
    let scope = SearchScope {
        root: location("root"),
        max_depth: 1,
        max_results: 5,
        timeout: Duration::from_secs(2),
    };
    let outcomes = Rc::new(RefCell::new(Vec::new()));
    let received = outcomes.clone();
    mock.search(
        SearchRequest {
            id: RequestId(1),
            scope: scope.clone(),
            query: "txt".into(),
            mode: SearchMode::Filename,
            cancellable: gio::Cancellable::new(),
        },
        Box::new(move |result| received.borrow_mut().push(result)),
    );
    finish_search(&mock, &context, &outcomes);
    let SearchOutcome::Incomplete {
        matches,
        reason,
        declared_scope,
        retry,
        ..
    } = outcomes.borrow_mut().pop().unwrap().outcome
    else {
        panic!("expected partial search");
    };
    assert_eq!(matches[0].location, location("a.txt"));
    assert_eq!(matches.len(), 1);
    assert_eq!(reason, SearchIncompleteReason::ProviderFailure);
    assert_eq!(declared_scope, scope);
    assert_eq!(retry, RetryClass::AfterRefresh);
    let cancel = gio::Cancellable::new();
    let received = outcomes.clone();
    mock.search(
        SearchRequest {
            id: RequestId(2),
            scope: scope.clone(),
            query: "txt".into(),
            mode: SearchMode::Filename,
            cancellable: cancel.clone(),
        },
        Box::new(move |result| received.borrow_mut().push(result)),
    );
    cancel.cancel();
    mock.advance(2);
    pump(&context);
    assert!(
        matches!(&outcomes.borrow()[0].outcome, SearchOutcome::Incomplete { matches, reason: SearchIncompleteReason::Cancelled, declared_scope, .. } if matches.is_empty() && *declared_scope == scope)
    );
}

#[test]
fn queued_cancellation_does_not_start_and_committed_success_can_win_running_cancel() {
    for cancel_before_start in [true, false] {
        let context = glib::MainContext::new();
        let _guard = context.acquire().unwrap();
        let mock = provider(&context);
        mock.set_capabilities(capabilities(1));
        mock.set_operation_behavior(MockOperationBehavior::SucceedDespiteCancellation);
        let request = operation(1, 1);
        let cancel = request.cancellable.clone();
        let events = Rc::new(RefCell::new(Vec::new()));
        let received = events.clone();
        mock.submit(
            request,
            Box::new(move |event| received.borrow_mut().push(event)),
        );
        if !cancel_before_start {
            mock.advance(2);
            pump(&context);
        }
        cancel.cancel();
        for _ in 0..5 {
            mock.advance(2);
            pump(&context);
        }
        let events = events.borrow();
        assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
        if cancel_before_start {
            assert_eq!(events.len(), 1);
            assert!(
                matches!(&events[0], OperationEvent::Cancelled { outcome, .. } if outcome.completion.effect == MutationEffect::Unchanged)
            );
        } else {
            assert!(
                matches!(events.last().unwrap(), OperationEvent::Succeeded { completion, .. } if completion.completed == [location("source-0")])
            );
        }
    }
}

#[test]
fn capability_inspection_is_async_correlated_and_cancellable() {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let mock = provider(&context);
    mock.set_directory(location("directory"), Vec::new(), capabilities(3));
    let results = Rc::new(RefCell::new(Vec::new()));
    let received = results.clone();
    mock.inspect_capabilities(
        CapabilityRequest {
            id: RequestId(3),
            location: location("directory"),
            cancellable: gio::Cancellable::new(),
        },
        Box::new(move |result| received.borrow_mut().push(result)),
    );
    assert!(results.borrow().is_empty());
    mock.advance(2);
    pump(&context);
    assert_eq!(results.borrow()[0].id, RequestId(3));
    assert!(
        matches!(&results.borrow()[0].outcome, RequestOutcome::Succeeded(snapshot) if snapshot.version == 3 && !snapshot.supports(Capability::Trash))
    );
    let cancel = gio::Cancellable::new();
    let received = results.clone();
    mock.inspect_capabilities(
        CapabilityRequest {
            id: RequestId(4),
            location: location("directory"),
            cancellable: cancel.clone(),
        },
        Box::new(move |result| received.borrow_mut().push(result)),
    );
    cancel.cancel();
    mock.advance(2);
    pump(&context);
    assert!(matches!(
        results.borrow()[1].outcome,
        RequestOutcome::Cancelled
    ));
}
