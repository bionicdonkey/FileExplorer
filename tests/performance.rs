use std::sync::{Mutex, MutexGuard};
use zorin_explorer::core::{
    application_preferences::{decode_session, encode_session},
    navigation::NavigationHistory,
    Location,
};
#[path = "support/scale_fixture.rs"]
#[allow(dead_code)]
mod scale_fixture;

// GIO's process-wide type system and the main-context ownership exercised by
// the local fixture cannot be driven concurrently by libtest worker threads.
// Each benchmark still uses the production backend; this only serializes
// independent benchmark cases in this integration-test binary.
static PERFORMANCE_TEST_LOCK: Mutex<()> = Mutex::new(());

fn performance_test_guard() -> MutexGuard<'static, ()> {
    PERFORMANCE_TEST_LOCK
        .lock()
        .expect("a previous performance test panicked while holding the test lock")
}

#[test]
fn streaming_commits_first_page_and_rejects_old_generation() {
    let _guard = performance_test_guard();
    use zorin_explorer::core::{contracts::*, navigation::TabState, ResolvedLocation};
    let root = Location::from_uri("smb://fixture/share").unwrap();
    let mut state = TabState::new(root.clone());
    let request = state.begin_navigation(root.clone()).unwrap();
    let snapshot = DirectorySnapshot {
        location: ResolvedLocation {
            requested: root.clone(),
            resolved: root.clone(),
        },
        entries: vec![EntrySnapshot {
            location: Location::from_uri("smb://fixture/share/a").unwrap(),
            display_name: "a".into(),
            kind: EntryKind::File,
            size: None,
            modified: None,
            content_type: None,
            capabilities: CapabilitySnapshot::default(),
        }],
        capabilities: CapabilitySnapshot::default(),
        next_page: Some(PageRequest {
            offset: 1,
            limit: 128,
        }),
    };
    assert!(state.apply_directory_page(
        request.generation,
        RequestCompletion {
            id: request.request.id,
            outcome: RequestOutcome::Succeeded(snapshot.clone())
        }
    ));
    assert!(
        state.directory.is_some(),
        "first content must commit before the last page"
    );
    assert!(state.has_pending_navigation());
    state.begin_navigation(Location::from_uri("smb://fixture/elsewhere").unwrap());
    assert!(!state.apply_directory_page(
        request.generation,
        RequestCompletion {
            id: request.request.id,
            outcome: RequestOutcome::Succeeded(snapshot)
        }
    ));
}

#[test]
fn streaming_merges_unique_uris_and_rejects_invalid_continuations_without_losing_rows() {
    let _guard = performance_test_guard();
    use zorin_explorer::core::{
        contracts::*,
        navigation::{NavigationStatus, TabState},
        ResolvedLocation,
    };
    let root = Location::from_uri("smb://fixture/share").unwrap();
    let entries = scale_fixture::entries(&root, 4);
    let mut state = TabState::new(root.clone());
    let request = state.begin_navigation(root.clone()).unwrap();
    let result = |entries, next_page| RequestCompletion {
        id: request.request.id,
        outcome: RequestOutcome::Succeeded(DirectorySnapshot {
            location: ResolvedLocation {
                requested: root.clone(),
                resolved: root.clone(),
            },
            entries,
            capabilities: CapabilitySnapshot::default(),
            next_page,
        }),
    };
    assert!(state.apply_directory_page(
        request.generation,
        result(
            entries[..2].to_vec(),
            Some(PageRequest {
                offset: 2,
                limit: 2
            })
        )
    ));
    assert!(state.apply_directory_page(request.generation, result(entries[1..].to_vec(), None)));
    assert_eq!(state.directory.as_ref().unwrap().entries.len(), 4);
    assert!(!state.has_pending_navigation());
    assert!(
        !state.history.can_go_back(),
        "pages must not create duplicate history visits"
    );
    let refresh = state.begin_refresh().unwrap();
    let invalid = RequestCompletion {
        id: refresh.request.id,
        outcome: RequestOutcome::Succeeded(DirectorySnapshot {
            location: ResolvedLocation {
                requested: root.clone(),
                resolved: root,
            },
            entries: vec![],
            capabilities: CapabilitySnapshot::default(),
            next_page: Some(PageRequest {
                offset: 0,
                limit: 128,
            }),
        }),
    };
    assert!(state.apply_directory_page(refresh.generation, invalid));
    assert!(matches!(state.status, NavigationStatus::Failed { .. }));
    assert_eq!(state.directory.as_ref().unwrap().entries.len(), 4);
    state.close();
    assert!(!state.apply_directory_page(
        refresh.generation,
        RequestCompletion {
            id: refresh.request.id,
            outcome: RequestOutcome::Cancelled
        }
    ));
}

#[test]
fn local_ten_thousand_items_first_page_meets_budget() {
    let _guard = performance_test_guard();
    println!("Benchmark environment: {:#?}", scale_fixture::environment());
    use std::{
        cell::RefCell,
        rc::Rc,
        time::{Duration, Instant},
    };
    use zorin_explorer::{
        application::{navigation::NavigationCoordinator, performance::assert_p95},
        core::navigation::TabState,
        infrastructure::gio_directory::GioDirectoryRepository,
    };
    let directory = tempfile::tempdir().unwrap();
    for n in 0..10_000 {
        std::fs::write(directory.path().join(format!("file-{n:05}")), b"fixture").unwrap();
    }
    let context = glib::MainContext::new();
    let _owner = context.acquire().unwrap();
    let root = Location::from_local_path(directory.path());
    let mut cold_runs = Vec::new();
    let mut warm_runs = Vec::new();
    // Cold here is application/model cold, not a privileged kernel-cache flush.
    for run in 0..31 {
        let state = Rc::new(RefCell::new(TabState::new(root.clone())));
        let nav = NavigationCoordinator::new(
            state.clone(),
            Rc::new(GioDirectoryRepository::new(context.clone())),
        );
        let first = Rc::new(RefCell::new(None));
        let output = first.clone();
        let started = Instant::now();
        nav.navigate(root.clone(), move |accepted| {
            if accepted && output.borrow().is_none() {
                *output.borrow_mut() = Some(started.elapsed());
            }
        });
        let acknowledgement = started.elapsed();
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.borrow().has_pending_navigation() {
            assert!(Instant::now() < deadline);
            context.iteration(true);
        }
        assert_eq!(
            state.borrow().directory.as_ref().unwrap().entries.len(),
            10_000
        );
        if run > 0 {
            cold_runs.push(first.borrow().expect("first page"));
            warm_runs.push(acknowledgement);
        }
    }
    assert_p95(
        "local first content",
        &cold_runs,
        Duration::from_millis(500),
    );
    assert_p95(
        "navigation acknowledgement",
        &warm_runs,
        Duration::from_millis(100),
    );
}

#[test]
fn page_filter_and_visible_cache_bound_work_and_invalidation() {
    let _guard = performance_test_guard();
    use zorin_explorer::application::performance::{CachePolicy, VisibleCache, VisibleRange};
    let a = Location::from_uri("smb://fixture/a").unwrap();
    let b = Location::from_uri("smb://fixture/b").unwrap();
    let c = Location::from_uri("smb://fixture/c").unwrap();
    let mut cache = VisibleCache::new(CachePolicy { max_entries: 2 });
    cache.insert(a.clone(), 1);
    cache.insert(b.clone(), 2);
    assert_eq!(cache.get(&a), Some(1));
    cache.insert(c.clone(), 3);
    assert_eq!(cache.get(&b), None);
    assert_eq!(cache.get(&a), Some(1));
    cache.invalidate();
    assert_eq!(cache.get(&a), None);
    assert_eq!(cache.get(&c), None);
    assert_eq!(
        VisibleRange {
            start: 10,
            end: 100
        }
        .clamped(20),
        10..20
    );
}

#[test]
fn session_and_navigation_history_are_bounded() {
    let _guard = performance_test_guard();
    let locations: Vec<_> = (0..1000)
        .map(|n| Location::from_uri(&format!("smb://fixture/share/{n}")).unwrap())
        .collect();
    let restored = decode_session(&encode_session(&locations).unwrap()).unwrap();
    assert_eq!(
        restored.len(),
        50,
        "restoring a session must not allocate unbounded tabs"
    );
    let mut history = NavigationHistory::new();
    for location in locations {
        history.push(location);
    }
    let mut count = 1;
    while history.back().is_some() {
        count += 1;
    }
    assert_eq!(count, 100, "back history must release old locations");
}

#[test]
fn oversized_state_is_rejected_before_persistence_or_decode() {
    let _guard = performance_test_guard();
    use zorin_explorer::core::{
        application_preferences::MAX_STATE_BYTES, operation_manager::OperationManager,
    };
    let oversized = Location::from_uri(&format!("smb://fixture/{}", "a".repeat(100_000))).unwrap();
    assert!(encode_session(&vec![oversized; 50]).is_err());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.json");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(MAX_STATE_BYTES as u64 + 1)
        .unwrap();
    let manager = OperationManager::load_history(Some(path.clone()), 7);
    assert!(manager.records().is_empty());
    assert!(!manager.recoverable_notices().is_empty());
    assert!(std::fs::metadata(path).unwrap().len() < MAX_STATE_BYTES as u64);
}

#[test]
fn search_history_bounds_entries_and_recovers_corruption() {
    let _guard = performance_test_guard();
    use zorin_explorer::core::search::SearchHistory;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("search.json");
    std::fs::write(&path, b"{torn").unwrap();
    let mut history = SearchHistory::load_from(path.clone())
        .expect("corrupt search state must recover without losing its save destination");
    for n in 0..1000 {
        history.record(&format!("query {n}"));
    }
    assert_eq!(history.entries().len(), 100);
    assert_eq!(history.entries()[0].term, "query 900");
    history.save().unwrap();
    assert_eq!(SearchHistory::load_from(path).unwrap().entries().len(), 100);
}

#[test]
fn finished_operation_records_are_bounded_and_corruption_recovers() {
    let _guard = performance_test_guard();
    use zorin_explorer::core::operation_manager::OperationManager;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.json");
    let records: Vec<_> = (1..=200).map(|id| serde_json::json!({"id":id,"kind":"Copy","sources":["smb://fixture/share/a"],"destination":null,"completed":1,"total":1,"state":"Completed","finished_at":2000000000})).collect();
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({"schema_version":3,"records":records})).unwrap(),
    )
    .unwrap();
    let manager = OperationManager::load_history(Some(path.clone()), 7);
    assert_eq!(manager.records().len(), 100);
    assert_eq!(manager.records()[0].id, 101);
    std::fs::write(&path, b"{torn write").unwrap();
    let recovered = OperationManager::load_history(Some(path.clone()), 7);
    assert!(recovered.records().is_empty());
    assert!(!recovered.recoverable_notices().is_empty());
    assert!(serde_json::from_slice::<serde_json::Value>(&std::fs::read(path).unwrap()).is_ok());
}

#[test]
fn one_hundred_history_records_meet_cold_and_warm_budgets() {
    let _guard = performance_test_guard();
    use std::time::{Duration, Instant};
    use zorin_explorer::{
        application::performance::assert_p95, core::operation_manager::OperationManager,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.json");
    let records: Vec<_> = (1..=100).map(|id| serde_json::json!({"id":id,"kind":"Copy","sources":["smb://fixture/share/a"],"destination":null,"completed":1,"total":1,"state":"Completed","finished_at":2000000000})).collect();
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({"schema_version":3,"records":records})).unwrap(),
    )
    .unwrap();
    let mut cold = Vec::new();
    let mut warm = Vec::new();
    for run in 0..31 {
        let started = Instant::now();
        let manager = OperationManager::load_history(Some(path.clone()), 7);
        assert_eq!(manager.records().len(), 100);
        if run > 0 {
            cold.push(started.elapsed());
        }
        let started = Instant::now();
        assert_eq!(manager.records().len(), 100);
        assert_eq!(manager.active_count(), 0);
        if run > 0 {
            warm.push(started.elapsed());
        }
    }
    assert_p95(
        "100-record cold history load",
        &cold,
        Duration::from_millis(100),
    );
    assert_p95(
        "100-record warm history projection",
        &warm,
        Duration::from_millis(100),
    );
}

#[test]
fn long_lived_worker_evicts_terminal_projections_with_history() {
    let _guard = performance_test_guard();
    use std::time::{Duration, Instant};
    use zorin_explorer::core::operation_manager::{OperationKind, OperationManager};
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source.txt");
    let destination = temporary.path().join("destination");
    std::fs::write(&source, b"fixture").unwrap();
    std::fs::create_dir(&destination).unwrap();
    let manager = OperationManager::default();
    for _ in 0..130 {
        manager.submit(
            OperationKind::Copy,
            vec![source.clone()],
            Some(destination.clone()),
        );
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while manager.active_count() > 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    // Polling continues after the last terminal event; the worker must not
    // repopulate its public projection cache from unbounded coordinator state.
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(manager.records().len(), 100);
    assert_eq!(manager.projections().len(), 100);
}
