use std::{cell::RefCell, rc::Rc};
use lorin_explorer::{
    core::{contracts::*, ports::FileOperations, Location},
    infrastructure::{gio_executor::GioExecutor, gio_operations::GioOperations},
};

use gio::prelude::*;

#[test]
fn daily_uri_adapter_queries_nonlocal_gio_backend_without_a_mount() {
    use lorin_explorer::core::ports::FileOperations;
    let context = glib::MainContext::new();
    context.with_thread_default(|| {
        let adapter = lorin_explorer::infrastructure::gio_operations::GioOperations::new_uri(context.clone());
        let result = std::rc::Rc::new(std::cell::RefCell::new(None));
        let output = result.clone();
        adapter.inspect_capabilities(CapabilityRequest {
            id: RequestId(9100), location: Location::from_uri("resource:///missing-daily-workflow-file").unwrap(), cancellable: gio::Cancellable::new(),
        }, Box::new(move |completion| *output.borrow_mut() = Some(completion.outcome)));
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while result.borrow().is_none() { context.iteration(false); assert!(std::time::Instant::now() < end); }
        assert!(matches!(result.borrow().as_ref().unwrap(), RequestOutcome::Failed(error) if error.kind == OperationErrorKind::NotFound));
    }).unwrap();
}

#[test]
fn daily_registered_gio_uri_backend_executes_transfers_and_preserves_uri_history() {
    use lorin_explorer::core::{OperationManager, OperationState};
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().to_path_buf();
    std::fs::create_dir(root.join("source")).unwrap();
    std::fs::create_dir(root.join("destination")).unwrap();
    std::fs::write(root.join("source/a.txt"), "alpha").unwrap();
    std::fs::write(root.join("source/b.txt"), "bravo").unwrap();
    // A registered GIO backend resolves non-file identities to controlled test
    // files. All inspection/mutation calls run through the production adapter.
    let vfs = gio::Vfs::default();
    let fixture_root = root.clone();
    assert!(vfs.register_uri_scheme(
        "dailyfixture",
        Some(Box::new(move |_, uri| {
            gio::File::for_path(fixture_root.join(uri.strip_prefix("dailyfixture:///").unwrap()))
        })),
        None
    ));
    let uri = |relative: &str| Location::from_uri(&format!("dailyfixture:///{relative}")).unwrap();
    assert!(
        uri("source").local_path().is_none(),
        "a provider's local projection must not grant local-only application actions"
    );
    let remote_folder = lorin_explorer::core::FileItem::from_snapshot(&EntrySnapshot {
        location: uri("source"),
        display_name: "source".into(),
        kind: EntryKind::Directory,
        size: None,
        modified: None,
        content_type: None,
        capabilities: CapabilitySnapshot {
            supported: [Capability::Open].into_iter().collect(),
            ..Default::default()
        },
    });
    let availability = lorin_explorer::ui::context_menu::CommandAvailability::for_capabilities(
        &[remote_folder],
        None,
    );
    assert!(!availability.can_open_terminal);
    let manager = OperationManager::default();
    let wait = |id| {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let record = manager.records().into_iter().find(|r| r.id == id).unwrap();
            if record.finished_at.is_some() {
                return record;
            }
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    };
    let sources = vec![uri("source/a.txt"), uri("source/b.txt")];
    std::fs::create_dir(root.join("source/nested")).unwrap();
    let descendant = manager.submit_locations(
        MutationKind::Copy,
        vec![uri("source")],
        Some(uri("source/nested")),
        None,
        None,
        false,
    );
    assert!(matches!(wait(descendant).state, OperationState::Failed(_)));
    assert!(!root.join("source/nested/source").exists());
    let id = manager.submit_locations(
        MutationKind::Copy,
        sources.clone(),
        Some(uri("destination")),
        None,
        None,
        false,
    );
    assert_eq!(wait(id).state, OperationState::Completed);
    assert_eq!(
        manager.summary(id).unwrap().completed,
        [
            "dailyfixture:///source/a.txt",
            "dailyfixture:///source/b.txt"
        ]
    );
    assert_eq!(
        std::fs::read_to_string(root.join("destination/a.txt")).unwrap(),
        "alpha"
    );
    let conflict = manager.submit_locations(
        MutationKind::Copy,
        sources.clone(),
        Some(uri("destination")),
        None,
        None,
        false,
    );
    assert!(matches!(wait(conflict).state, OperationState::Failed(_)));
    assert_eq!(
        manager.summary(conflict).unwrap().error_kind.as_deref(),
        Some("Conflict")
    );
    let keep = manager.submit_locations(
        MutationKind::Copy,
        sources.clone(),
        Some(uri("destination")),
        None,
        Some(ConflictDecision::KeepBoth),
        false,
    );
    assert_eq!(wait(keep).state, OperationState::Completed);
    assert_eq!(
        std::fs::read_dir(root.join("destination")).unwrap().count(),
        4
    );
    std::fs::create_dir(root.join("partial")).unwrap();
    std::fs::write(root.join("partial/b.txt"), "existing").unwrap();
    let partial = manager.submit_locations(
        MutationKind::Copy,
        sources.clone(),
        Some(uri("partial")),
        None,
        None,
        false,
    );
    assert!(matches!(wait(partial).state, OperationState::Failed(_)));
    let outcome = manager.summary(partial).unwrap();
    assert_eq!(outcome.effect, "Partial");
    assert_eq!(outcome.completed, ["dailyfixture:///source/a.txt"]);
    assert_eq!(outcome.remaining, ["dailyfixture:///source/b.txt"]);
    let moved = manager.submit_locations(
        MutationKind::Move,
        sources,
        Some(uri("destination")),
        None,
        Some(ConflictDecision::Replace),
        false,
    );
    assert_eq!(wait(moved).state, OperationState::Completed);
    assert!(!root.join("source/a.txt").exists());
    let created = manager.submit_locations(
        MutationKind::Create,
        vec![uri("created")],
        None,
        None,
        None,
        false,
    );
    assert_eq!(wait(created).state, OperationState::Completed);
    let renamed = manager.submit_locations(
        MutationKind::Rename,
        vec![uri("created")],
        Some(uri("destination")),
        Some(uri("destination/renamed")),
        None,
        false,
    );
    assert_eq!(wait(renamed).state, OperationState::Completed);
    let deleted = manager.submit_locations(
        MutationKind::Delete,
        vec![uri("destination/renamed")],
        None,
        None,
        None,
        true,
    );
    assert_eq!(wait(deleted).state, OperationState::Completed);
    assert!(!root.join("destination/renamed").exists());
    assert!(vfs.unregister_uri_scheme("dailyfixture"));
}
fn search_result(scope: SearchScope, mode: SearchMode, cancelled: bool) -> SearchOutcome {
    use lorin_explorer::{
        core::ports::SearchRepository, infrastructure::tracker_search::TrackerSearchRepository,
    };
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let adapter = TrackerSearchRepository::new(context.clone());
    let received = Rc::new(RefCell::new(None));
    let out = received.clone();
    let cancellable = gio::Cancellable::new();
    if cancelled {
        cancellable.cancel();
    }
    let callback_context = context.clone();
    adapter.search(
        SearchRequest {
            id: RequestId(123),
            scope,
            query: "report".into(),
            mode,
            cancellable,
        },
        Box::new(move |completion| {
            assert!(callback_context.is_owner());
            assert_eq!(completion.id, RequestId(123));
            *out.borrow_mut() = Some(completion.outcome);
        }),
    );
    assert!(received.borrow().is_none());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while received.borrow().is_none() {
        context.iteration(false);
        assert!(std::time::Instant::now() < deadline);
    }
    let result = received.borrow_mut().take().unwrap();
    result
}

#[test]
fn tracker_fallback_waits_for_executor_context_and_keeps_unavailable_explicit() {
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        },
        time::Duration,
    };
    use lorin_explorer::{
        core::ports::SearchRepository,
        infrastructure::{
            gio_executor::{CancellationSignal, GioExecutor},
            tracker_search::TrackerSearchRepository,
        },
    };

    // This gate catches a fallback that escapes onto gio::spawn_blocking: it
    // would complete despite the executor being fully occupied. A single worker
    // is what makes "occupied" total, so the gate proves ownership rather than
    // racing the rest of the pool.
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let executor = GioExecutor::isolated_with_workers_for_tests(1);
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
        .unwrap();

    let adapter = TrackerSearchRepository::with_executor(context.clone(), executor.clone());
    let received = Rc::new(RefCell::new(None));
    let output = received.clone();
    let temporary = tempfile::tempdir().unwrap();
    adapter.search(
        SearchRequest {
            id: RequestId(124),
            scope: SearchScope::local_fallback(Location::from_local_path(temporary.path())),
            query: "report".into(),
            mode: SearchMode::Content,
            cancellable: gio::Cancellable::new(),
        },
        Box::new(move |completion| *output.borrow_mut() = Some(completion.outcome)),
    );

    let blocked_until = std::time::Instant::now() + Duration::from_millis(100);
    while std::time::Instant::now() < blocked_until {
        context.iteration(false);
        assert!(
            received.borrow().is_none(),
            "fallback must execute on the injected executor context"
        );
        std::thread::yield_now();
    }

    release_gate.store(true, Ordering::SeqCst);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while received.borrow().is_none() {
        context.iteration(false);
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(matches!(
        received.borrow().as_ref(),
        Some(SearchOutcome::Unsupported { .. })
    ));
    executor.shutdown_for_tests();
}

#[test]
fn slice2_search_local_depth_stable_order_result_and_time_bounds() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("child")).unwrap();
    std::fs::write(temp.path().join("report-z"), "").unwrap();
    std::fs::write(temp.path().join("report-a"), "").unwrap();
    std::fs::write(temp.path().join("child/report-nested"), "").unwrap();
    let mut scope = SearchScope::local_fallback(Location::from_local_path(temp.path()));
    let SearchOutcome::Completed {
        matches,
        scope_limited,
        ..
    } = search_result(scope.clone(), SearchMode::Filename, false)
    else {
        panic!("fallback must complete")
    };
    assert!(scope_limited);
    assert_eq!(
        matches
            .iter()
            .map(|e| e.display_name.as_str())
            .collect::<Vec<_>>(),
        ["report-a", "report-z"]
    );
    scope.max_depth = 0;
    assert!(
        matches!(search_result(scope.clone(), SearchMode::Filename, false), SearchOutcome::Completed { matches, .. } if matches.is_empty())
    );
    scope.max_depth = 2;
    assert!(
        matches!(search_result(scope.clone(), SearchMode::Filename, false), SearchOutcome::Completed { matches, .. } if matches.len() == 3)
    );
    scope.max_results = 1;
    assert!(
        matches!(search_result(scope.clone(), SearchMode::Filename, false), SearchOutcome::Completed { matches, limits, .. } if matches.len() == 1 && matches[0].display_name == "report-a" && limits.contains(&SearchLimit::ResultCount))
    );
    scope.timeout = std::time::Duration::ZERO;
    assert!(
        matches!(search_result(scope, SearchMode::Filename, false), SearchOutcome::Completed { matches, limits, .. } if matches.is_empty() && limits.contains(&SearchLimit::Time))
    );
}

#[test]
fn slice2_search_cancellation_failure_and_unavailable_modes_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("unrelated"), "report secret contents").unwrap();
    let scope = SearchScope::local_fallback(Location::from_local_path(temp.path()));
    assert!(matches!(
        search_result(scope.clone(), SearchMode::Content, false),
        SearchOutcome::Unsupported { .. }
    ));
    assert!(matches!(
        search_result(scope, SearchMode::Filename, true),
        SearchOutcome::Incomplete {
            reason: SearchIncompleteReason::Cancelled,
            ..
        }
    ));
    let remote = SearchScope::local_fallback(Location::from_uri("smb://host/share").unwrap());
    assert!(matches!(
        search_result(remote, SearchMode::Filename, false),
        SearchOutcome::Unsupported { .. }
    ));
    let missing =
        SearchScope::local_fallback(Location::from_local_path(&temp.path().join("missing")));
    assert!(matches!(
        search_result(missing, SearchMode::Filename, false),
        SearchOutcome::Incomplete {
            reason: SearchIncompleteReason::ProviderFailure,
            ..
        }
    ));
}

#[test]
fn slice2_search_never_follows_symlinks_or_decodes_uri_identity_as_a_path() {
    use std::{
        ffi::OsString,
        os::unix::{ffi::OsStringExt, fs::symlink},
    };
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("report-secret"), "private").unwrap();
    symlink(outside.path(), temp.path().join("shortcut")).unwrap();
    let name = OsString::from_vec(b"report-% space\xff".to_vec());
    let expected = Location::from_local_path(&temp.path().join(&name));
    std::fs::write(temp.path().join(name), "").unwrap();
    let mut scope = SearchScope::local_fallback(Location::from_local_path(temp.path()));
    scope.max_depth = 5;
    let SearchOutcome::Completed { matches, .. } =
        search_result(scope, SearchMode::Filename, false)
    else {
        panic!("expected completed metadata search")
    };
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].location, expected);
}

fn directory_result(
    location: Location,
    cancelled: bool,
    page: PageRequest,
) -> RequestOutcome<DirectorySnapshot> {
    use gio::prelude::*;
    use lorin_explorer::{
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
fn directory_listing_returns_owned_uri_identity_without_eager_advisory_metadata() {
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
    // Paged enumeration returns identity and display data immediately; advisory metadata
    // is loaded only when a view marks the corresponding row visible.
    assert_eq!(file.size, None);
    assert_eq!(file.modified, None);
    assert_eq!(file.content_type, None);
    assert_eq!(file.kind, EntryKind::File);
    assert_eq!(
        file.location,
        Location::from_local_path(&dir.path().join("data.txt"))
    );
    // Advisory metadata is deferred, but the access rights are not: every
    // command that mutates or copies an entry is gated on them, so a listing
    // without them leaves Copy, Move, Rename, Trash and Delete permanently off.
    for capability in [
        Capability::Open,
        Capability::Copy,
        Capability::Move,
        Capability::Rename,
        Capability::Trash,
        Capability::Delete,
    ] {
        assert!(
            file.capabilities.supports(capability),
            "a writable local file must list {capability:?} as supported"
        );
    }
    // Exercise the active repository projection, not a hand-built FileInfo.
    let item = lorin_explorer::core::FileItem::from_snapshot(file);
    let availability = lorin_explorer::ui::context_menu::CommandAvailability::for_capabilities(
        &[item],
        Some(&snapshot.capabilities),
    );
    use lorin_explorer::application::commands::ContextCommand;
    for command in [
        ContextCommand::Copy,
        ContextCommand::Cut,
        ContextCommand::CopyTo,
        ContextCommand::MoveTo,
        ContextCommand::Rename,
        ContextCommand::MoveToTrash,
        ContextCommand::DeletePermanently,
    ] {
        assert!(
            availability.allows(command),
            "{command:?} must be available for a file in a writable folder"
        );
    }
    assert!(snapshot.capabilities.supports(Capability::Rename));
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

#[test]
fn directory_cancellation_retains_executor_request_until_terminal_dto_is_released() {
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc, Arc,
        },
        time::Duration,
    };
    use lorin_explorer::{
        core::ports::DirectoryRepository,
        infrastructure::{
            gio_directory::GioDirectoryRepository,
            gio_executor::{CancellationSignal, GioExecutor},
        },
    };

    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    // One worker, occupied by the gate below, is what makes the pre-cancellation
    // request count deterministic: the listing cannot start, let alone finish,
    // before this test has observed that the executor registered it.
    let executor = GioExecutor::isolated_with_workers_for_tests(1);
    let release_gate = Arc::new(AtomicBool::new(false));
    let start_listing = Arc::new(AtomicBool::new(false));
    let (started, started_receiver) = mpsc::sync_channel(1);
    executor
        .submit(CancellationSignal::new(), {
            let release_gate = release_gate.clone();
            let start_listing = start_listing.clone();
            move |request| {
                started.send(()).unwrap();
                // Hold the only worker until the listing has been registered and
                // cancelled, then keep holding the request itself.
                while !start_listing.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
                let mut request = Some(request);
                let source = glib::timeout_source_new(
                    Duration::from_millis(1),
                    None,
                    glib::Priority::DEFAULT,
                    move || {
                        if !release_gate.load(Ordering::SeqCst) {
                            return glib::ControlFlow::Continue;
                        }
                        request
                            .take()
                            .expect("gate completion runs once")
                            .complete();
                        glib::ControlFlow::Break
                    },
                );
                source.attach(Some(&glib::MainContext::ref_thread_default()));
            }
        })
        .unwrap();
    started_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("the gate must occupy the only worker before the listing is queued");

    let directory = tempfile::tempdir().unwrap();
    let cancelled = gio::Cancellable::new();
    let adapter = GioDirectoryRepository::with_executor(context.clone(), executor.clone());
    let received = Rc::new(RefCell::new(None));
    let out = received.clone();
    let active_at_terminal_delivery = Arc::new(AtomicUsize::new(usize::MAX));
    let observed_active = active_at_terminal_delivery.clone();
    let terminal_executor = executor.clone();
    adapter.list(
        DirectoryRequest {
            id: RequestId(88),
            location: Location::from_local_path(directory.path()),
            page: PageRequest {
                offset: 0,
                limit: usize::MAX,
            },
            cancellable: cancelled.clone(),
        },
        Box::new(move |completion| {
            observed_active.store(
                terminal_executor.active_request_count_for_tests(),
                Ordering::SeqCst,
            );
            *out.borrow_mut() = Some(completion.outcome);
        }),
    );
    cancelled.cancel();
    assert_eq!(
        executor.active_request_count_for_tests(),
        2,
        "the cancelled listing must be registered alongside the held gate request"
    );
    start_listing.store(true, Ordering::SeqCst);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while received.borrow().is_none() {
        context.iteration(false);
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(matches!(
        received.borrow().as_ref(),
        Some(RequestOutcome::Cancelled)
    ));
    assert_eq!(
        active_at_terminal_delivery.load(Ordering::SeqCst),
        1,
        "only the deliberately held request may remain when the terminal DTO is delivered"
    );
    release_gate.store(true, Ordering::SeqCst);
    while executor.active_request_count_for_tests() != 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    executor.shutdown_for_tests();
}

#[test]
fn executor_shutdown_cancels_parallel_owner_requests_without_stale_ui_callbacks() {
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc, Arc,
        },
        time::Duration,
    };
    use lorin_explorer::{
        core::ports::{DirectoryRepository, SearchRepository},
        infrastructure::{
            gio_directory::GioDirectoryRepository,
            gio_executor::{CancellationSignal, GioExecutor},
            gio_operations::GioOperations,
            tracker_search::TrackerSearchRepository,
        },
    };

    const REQUESTS_PER_CLASS: u64 = 16;

    // A single worker is what makes the gate below occupy the whole executor, so
    // every request stays queued and live until shutdown cancels it.
    let executor = GioExecutor::isolated_with_workers_for_tests(1);
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
        .expect("gate request must occupy the executor before owner teardown");

    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("preserve-after-owner-close.txt");
    std::fs::write(&source, "must survive cancelled operations").unwrap();
    let location = Location::from_local_path(&source);
    let directory = Location::from_local_path(fixture.path());
    let stale_ui_mutations = Arc::new(AtomicUsize::new(0));
    let mut owner_cancellables = Vec::new();
    // An owner may only release its MainContext once the executor has dropped
    // every bridge into it, so these owners stay alive until shutdown has run.
    // `local_copy_move_conflict_and_delete_preserve_actual_effects_after_owner_teardown`
    // covers the teardown-while-in-flight path that enforces that contract.
    let mut owners: Vec<Box<dyn std::any::Any>> = Vec::new();

    {
        let context = glib::MainContext::new();
        let _owner = context.acquire().unwrap();
        let operations = GioOperations::with_executor(context.clone(), executor.clone());
        for index in 0..REQUESTS_PER_CLASS {
            let cancellable = gio::Cancellable::new();
            let stale_ui_mutations = stale_ui_mutations.clone();
            operations.submit(
                OperationRequest {
                    id: OperationId(index),
                    kind: MutationKind::Delete,
                    sources: vec![location.clone()],
                    destination: None,
                    explicit_target: None,
                    capability_version: 1,
                    cancellable: cancellable.clone(),
                },
                Box::new(move |_| {
                    stale_ui_mutations.fetch_add(1, Ordering::SeqCst);
                }),
            );
            owner_cancellables.push(cancellable);
        }
        owners.push(Box::new(operations));
    }

    {
        let context = glib::MainContext::new();
        let _owner = context.acquire().unwrap();
        let repository = GioDirectoryRepository::with_executor(context.clone(), executor.clone());
        for index in 0..REQUESTS_PER_CLASS {
            let cancellable = gio::Cancellable::new();
            let stale_ui_mutations = stale_ui_mutations.clone();
            repository.list(
                DirectoryRequest {
                    id: RequestId(100 + index),
                    location: directory.clone(),
                    page: PageRequest {
                        offset: 0,
                        limit: usize::MAX,
                    },
                    cancellable: cancellable.clone(),
                },
                Box::new(move |_| {
                    stale_ui_mutations.fetch_add(1, Ordering::SeqCst);
                }),
            );
            owner_cancellables.push(cancellable);
        }
        owners.push(Box::new(repository));
    }

    {
        let context = glib::MainContext::new();
        let _owner = context.acquire().unwrap();
        let repository = GioDirectoryRepository::with_executor(context.clone(), executor.clone());
        for index in 0..REQUESTS_PER_CLASS {
            let cancellable = gio::Cancellable::new();
            let stale_ui_mutations = stale_ui_mutations.clone();
            repository.inspect_visible_metadata(
                VisibleMetadataRequest {
                    id: RequestId(200 + index),
                    locations: vec![location.clone()],
                    cancellable: cancellable.clone(),
                },
                Box::new(move |_| {
                    stale_ui_mutations.fetch_add(1, Ordering::SeqCst);
                }),
            );
            owner_cancellables.push(cancellable);
        }
        owners.push(Box::new(repository));
    }

    {
        let context = glib::MainContext::new();
        let _owner = context.acquire().unwrap();
        let repository = TrackerSearchRepository::with_executor(context.clone(), executor.clone());
        for index in 0..REQUESTS_PER_CLASS {
            let cancellable = gio::Cancellable::new();
            let stale_ui_mutations = stale_ui_mutations.clone();
            repository.search(
                SearchRequest {
                    id: RequestId(300 + index),
                    scope: SearchScope::local_fallback(directory.clone()),
                    query: "preserve".into(),
                    mode: SearchMode::Filename,
                    cancellable: cancellable.clone(),
                },
                Box::new(move |_| {
                    stale_ui_mutations.fetch_add(1, Ordering::SeqCst);
                }),
            );
            owner_cancellables.push(cancellable);
        }
        owners.push(Box::new(repository));
    }

    {
        let context = glib::MainContext::new();
        let _owner = context.acquire().unwrap();
        let operations = GioOperations::with_executor(context.clone(), executor.clone());
        for index in 0..REQUESTS_PER_CLASS {
            let cancellable = gio::Cancellable::new();
            let stale_ui_mutations = stale_ui_mutations.clone();
            operations.inspect_capabilities(
                CapabilityRequest {
                    id: RequestId(400 + index),
                    location: location.clone(),
                    cancellable: cancellable.clone(),
                },
                Box::new(move |_| {
                    stale_ui_mutations.fetch_add(1, Ordering::SeqCst);
                }),
            );
            owner_cancellables.push(cancellable);
        }
        owners.push(Box::new(operations));
    }

    {
        let context = glib::MainContext::new();
        let _owner = context.acquire().unwrap();
        let repository = GioDirectoryRepository::with_executor(context.clone(), executor.clone());
        for index in 0..REQUESTS_PER_CLASS {
            let cancellable = gio::Cancellable::new();
            let stale_ui_mutations = stale_ui_mutations.clone();
            repository.list_pages(
                DirectoryRequest {
                    id: RequestId(500 + index),
                    location: directory.clone(),
                    page: PageRequest {
                        offset: 0,
                        limit: 1,
                    },
                    cancellable: cancellable.clone(),
                },
                Box::new(move |_| {
                    stale_ui_mutations.fetch_add(1, Ordering::SeqCst);
                }),
            );
            owner_cancellables.push(cancellable);
        }
        owners.push(Box::new(repository));
    }

    assert_eq!(
        executor.active_request_count_for_tests(),
        1 + (REQUESTS_PER_CLASS * 6) as usize,
        "all operation, capability, directory, paged, metadata, and search requests must be live before owner contexts close"
    );
    for cancellable in owner_cancellables {
        cancellable.cancel();
    }
    release_gate.store(true, Ordering::SeqCst);
    executor.shutdown_for_tests();

    assert_eq!(executor.active_request_count_for_tests(), 0);
    // Now the owners close. Their drains are already satisfied, and no queued
    // callback may run against the state they are tearing down.
    drop(owners);
    assert_eq!(
        stale_ui_mutations.load(Ordering::SeqCst),
        0,
        "callbacks queued for closing owner contexts must not mutate stale UI state"
    );
    assert!(
        source.exists(),
        "a cancelled operation must not mutate its source after owner teardown"
    );
}

#[test]
fn owner_teardown_waits_for_executor_bridges_before_releasing_its_context() {
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        },
        time::Duration,
    };
    use lorin_explorer::{
        core::ports::DirectoryRepository,
        infrastructure::{
            gio_directory::GioDirectoryRepository,
            gio_executor::{CancellationSignal, GioExecutor},
        },
    };

    // A DTO sender held by the executor carries a GLib waker for a source owned
    // by the requesting context, and both sending and dropping it unref that
    // source from an executor worker. Releasing the owner context first is
    // therefore a use-after-free, so owner teardown has to wait.
    let executor = GioExecutor::isolated_with_workers_for_tests(1);
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
        .expect("the gate must occupy the executor before the owner submits");

    let fixture = tempfile::tempdir().unwrap();
    let location = Location::from_local_path(fixture.path());
    let (torn_down, teardown_receiver) = mpsc::sync_channel(1);
    let executor_for_owner = executor.clone();
    let owner = std::thread::spawn(move || {
        let context = glib::MainContext::new();
        let _guard = context.acquire().unwrap();
        let adapter = GioDirectoryRepository::with_executor(context.clone(), executor_for_owner);
        adapter.list(
            DirectoryRequest {
                id: RequestId(77),
                location,
                page: PageRequest {
                    offset: 0,
                    limit: usize::MAX,
                },
                cancellable: gio::Cancellable::new(),
            },
            Box::new(|_| {}),
        );
        assert_eq!(adapter.outstanding_owner_requests_for_tests(), 1);
        drop(adapter);
        torn_down
            .send(())
            .expect("teardown observer must outlive the owner");
    });

    assert_eq!(
        teardown_receiver.recv_timeout(Duration::from_millis(250)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "owner teardown must wait while the executor still holds a bridge into its context"
    );
    release_gate.store(true, Ordering::SeqCst);
    teardown_receiver
        .recv_timeout(Duration::from_secs(3))
        .expect("owner teardown must finish once the executor releases its bridge");
    owner.join().expect("owner thread must not panic");
    executor.shutdown_for_tests();
}

fn run(
    kind: MutationKind,
    sources: Vec<Location>,
    target: Option<Location>,
) -> Vec<OperationEvent> {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let adapter = GioOperations::with_executor(context.clone(), GioExecutor::shared());
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
fn local_copy_move_conflict_and_delete_preserve_actual_effects_after_owner_teardown() {
    // `run` drops its adapter and owner context immediately after accepting the
    // terminal event. Repeating the full lifecycle catches a worker that
    // publishes terminal state before it has dropped its GIO objects.
    for _ in 0..64 {
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
        assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
        std::fs::write(&target, "keep").unwrap();
        let events = run(
            MutationKind::Move,
            vec![Location::from_local_path(&source)],
            Some(Location::from_local_path(&target)),
        );
        assert!(
            matches!(events.last(), Some(OperationEvent::Failed { error, completion, .. }) if error.kind == OperationErrorKind::Conflict && completion.effect == MutationEffect::Unchanged)
        );
        assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
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
        assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
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
        assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
        assert!(!moved.exists());
    }
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
    use lorin_explorer::core::contracts::ConflictDecision;
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
        lorin_explorer::infrastructure::gio_operations::execute_with_decision(
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
    lorin_explorer::infrastructure::gio_operations::execute(request, &mut |event| {
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
    use lorin_explorer::application::operations::{
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

/// Writes a one-file tar archive, so the fixture needs no archiver installed
/// and no crate beyond the standard library. Fields are the ustar ones: name,
/// mode, uid, gid, size, mtime, checksum, type, then the magic.
fn tar_with_one_file(path: &std::path::Path, name: &str, contents: &str) {
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..107].copy_from_slice(b"0000644");
    header[108..115].copy_from_slice(b"0000000");
    header[116..123].copy_from_slice(b"0000000");
    header[124..135].copy_from_slice(format!("{:011o}", contents.len()).as_bytes());
    header[136..147].copy_from_slice(b"00000000000");
    // The checksum is computed with its own field read as spaces.
    header[148..156].copy_from_slice(b"        ");
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let checksum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
    header[148..155].copy_from_slice(format!("{checksum:06o}\0").as_bytes());
    header[155] = b' ';

    let mut archive = header.to_vec();
    archive.extend_from_slice(contents.as_bytes());
    archive.resize(512 + contents.len().div_ceil(512) * 512, 0);
    // Two empty blocks end the archive.
    archive.resize(archive.len() + 1024, 0);
    std::fs::write(path, archive).unwrap();
}

fn archive_backend_installed() -> bool {
    ["/usr/libexec/gvfsd-archive", "/usr/lib/gvfs/gvfsd-archive"]
        .iter()
        .any(|path| std::path::Path::new(path).exists())
}

/// Lists a location that has to be mounted first.
///
/// Mount-on-demand is enabled by the presence of a mount-operation factory --
/// the window supplies a GTK one so authentication can be parented -- so a
/// repository without one reports `MountUnavailable` rather than mounting. An
/// archive needs no credentials, so a plain GIO operation is enough here.
fn directory_result_mounting(location: Location) -> RequestOutcome<DirectorySnapshot> {
    use lorin_explorer::{
        core::ports::DirectoryRepository, infrastructure::gio_directory::GioDirectoryRepository,
    };
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let adapter = GioDirectoryRepository::new(context.clone());
    adapter.set_mount_operation(gio::MountOperation::new);
    let received = Rc::new(RefCell::new(None));
    let out = received.clone();
    adapter.list(
        DirectoryRequest {
            id: RequestId(88),
            location,
            page: PageRequest {
                offset: 0,
                limit: usize::MAX,
            },
            cancellable: gio::Cancellable::new(),
        },
        Box::new(move |result| *out.borrow_mut() = Some(result.outcome)),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while received.borrow().is_none() {
        context.iteration(false);
        assert!(
            std::time::Instant::now() < deadline,
            "mounting and listing the archive timed out"
        );
        std::thread::yield_now();
    }
    received.take().unwrap()
}

#[test]
fn opening_an_archive_lists_its_contents_in_the_application() {
    // The end of the path an archive takes: the activation policy turns the
    // file into an `archive://` location, and the directory adapter mounts it
    // on demand and lists what is inside. Anything short of this still opens
    // another file manager, which is what it used to do.
    if !archive_backend_installed() {
        eprintln!("skipped: gvfs has no archive backend on this machine");
        return;
    }
    use lorin_explorer::core::{activation_for, Activation, FileItem};
    let fixture = tempfile::tempdir().unwrap();
    let archive = fixture.path().join("Holiday Photos.tar");
    tar_with_one_file(&archive, "packing-list.txt", "one tent\n");

    let item = FileItem::from_path(&archive).unwrap();
    let Activation::Browse(inside) = activation_for(&item) else {
        panic!("a tar must open in the window");
    };
    assert_eq!(inside.display_name(), "Holiday Photos.tar");

    let outcome = directory_result_mounting(inside);
    let RequestOutcome::Succeeded(snapshot) = outcome else {
        panic!("listing an archive must succeed, got {outcome:?}");
    };
    let names: Vec<&str> = snapshot
        .entries
        .iter()
        .map(|entry| entry.display_name.as_str())
        .collect();
    assert_eq!(names, vec!["packing-list.txt"]);
}
