use gio::prelude::*;

#[test]
fn daily_named_children_reject_path_traversal_for_remote_create_and_rename() {
    use zorin_explorer::application::commands::named_child;
    let root = location("smb://host/share");
    assert_eq!(
        named_child(&root, "new folder").unwrap().uri(),
        "smb://host/share/new%20folder"
    );
    for name in ["", ".", "..", "../escape", "child/name", "name\nsecret"] {
        assert!(named_child(&root, name).is_err());
    }
}

#[test]
fn daily_pins_and_sessions_keep_unmounted_uri_identities() {
    use zorin_explorer::core::{
        application_preferences::{decode_session, encode_session},
        SidebarPreferences,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pins.json");
    let locations = vec![
        location("smb://offline/share/reports"),
        location("sftp://offline/home/me"),
        Location::from_local_path(&directory.path().join("missing")),
    ];
    let mut pins = SidebarPreferences::load_from(path.clone()).unwrap();
    for uri in &locations {
        pins.pin_location(uri.clone());
    }
    pins.save().unwrap();
    let restored = SidebarPreferences::load_from(path).unwrap();
    for uri in &locations {
        assert!(restored.locations().any(|pin| pin == uri));
    }
    assert_eq!(
        decode_session(&encode_session(&locations).unwrap()).unwrap(),
        locations
    );
}

#[test]
fn daily_multi_item_conflicts_keep_both_replace_and_skip_report_effects() {
    use zorin_explorer::core::{OperationManager, OperationState};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let destination = directory.path().join("destination");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&destination).unwrap();
    for name in ["a.txt", "b.txt"] {
        std::fs::write(source.join(name), "new").unwrap();
        std::fs::write(destination.join(name), "old").unwrap();
    }
    let manager = OperationManager::default();
    let sources = ["a.txt", "b.txt"]
        .iter()
        .map(|name| Location::from_local_path(&source.join(name)))
        .collect::<Vec<_>>();
    for decision in [
        ConflictDecision::KeepBoth,
        ConflictDecision::Replace,
        ConflictDecision::Skip,
    ] {
        let id = manager.submit_locations(
            MutationKind::Copy,
            sources.clone(),
            Some(Location::from_local_path(&destination)),
            None,
            Some(decision),
            false,
        );
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let record = manager.records().into_iter().find(|r| r.id == id).unwrap();
            if record.finished_at.is_some() {
                assert_eq!(record.state, OperationState::Completed);
                break;
            }
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let summary = manager.summary(id).unwrap();
        if decision == ConflictDecision::Skip {
            assert_eq!(summary.remaining.len(), 2);
            assert_eq!(summary.completed.len(), 0);
        } else {
            assert_eq!(summary.completed.len(), 2);
        }
    }
    for name in ["a.txt", "b.txt"] {
        assert_eq!(
            std::fs::read_to_string(destination.join(name)).unwrap(),
            "new"
        );
    }
    assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 4);
}

#[test]
fn daily_uri_payload_rejects_unsafe_input_and_deduplicates() {
    use zorin_explorer::application::commands::*;
    let payload = ClipboardPayload::parse(
        ClipboardAction::Cut,
        "# Files\r\nsmb://host/share/a\r\nfile:///tmp/b\r\nsmb://host/share/a\r\n",
    )
    .unwrap();
    assert_eq!(payload.locations.len(), 2);
    assert_eq!(
        payload.uri_list(),
        "smb://host/share/a\r\nfile:///tmp/b\r\n"
    );
    for input in [
        "",
        "/tmp/a",
        "smb://user:secret@host/a",
        "file:///tmp/a?secret=x",
        "file:///tmp/a\ninvalid",
    ] {
        assert!(ClipboardPayload::parse(ClipboardAction::Copy, input).is_err());
    }
}

#[test]
fn daily_external_clipboard_preserves_cut_action_and_rejects_unknown_actions() {
    use zorin_explorer::application::commands::*;
    assert_eq!(
        ClipboardPayload::parse_gnome("cut\nfile:///tmp/a\n")
            .unwrap()
            .action,
        ClipboardAction::Cut
    );
    assert_eq!(
        ClipboardPayload::parse_gnome("copy\nsmb://host/share/a\n")
            .unwrap()
            .action,
        ClipboardAction::Copy
    );
    assert!(ClipboardPayload::parse_gnome("delete\nfile:///tmp/a").is_err());
}

#[test]
fn daily_keyboard_toolbar_and_menu_route_identical_commands() {
    use zorin_explorer::application::commands::*;
    assert_eq!(route(Shortcut::Paste), route(ContextCommand::Paste));
    assert_eq!(route(Shortcut::Cut), route(ContextCommand::Cut));
    assert_eq!(route(Shortcut::Copy), route(ContextCommand::Copy));
    assert_eq!(route(Shortcut::Trash), route(ContextCommand::MoveToTrash));
    assert_eq!(
        route(Shortcut::DeletePermanently),
        route(ContextCommand::DeletePermanently)
    );
}

#[test]
fn daily_selection_capabilities_and_drag_action_are_uri_based() {
    use zorin_explorer::{
        application::commands::*, core::FileItem, ui::context_menu::CommandAvailability,
    };
    let entry = |uri: &str, supported: Vec<Capability>| {
        FileItem::from_snapshot(&EntrySnapshot {
            location: location(uri),
            display_name: "item".into(),
            kind: EntryKind::File,
            size: None,
            modified: None,
            content_type: None,
            capabilities: CapabilitySnapshot {
                supported: supported.into_iter().collect(),
                ..Default::default()
            },
        })
    };
    let items = vec![
        entry(
            "smb://host/share/a",
            vec![Capability::Copy, Capability::Move],
        ),
        entry("smb://host/share/b", vec![Capability::Copy]),
    ];
    let destination = CapabilitySnapshot {
        supported: [Capability::Create].into_iter().collect(),
        ..Default::default()
    };
    let availability = CommandAvailability::for_capabilities(&items, Some(&destination));
    assert!(availability.can_copy && availability.can_paste);
    assert!(!availability.can_cut);
    assert!(DragProposal::negotiate(
        &items,
        location("smb://host/destination"),
        &destination,
        ClipboardAction::Copy
    )
    .is_ok());
    assert!(DragProposal::negotiate(
        &items,
        location("smb://host/destination"),
        &destination,
        ClipboardAction::Cut
    )
    .is_err());
}

#[test]
fn daily_write_permission_does_not_imply_rename_capability() {
    use zorin_explorer::{core::FileItem, ui::context_menu::CommandAvailability};
    let info = gio::FileInfo::new();
    info.set_name("file");
    info.set_file_type(gio::FileType::Regular);
    info.set_attribute_boolean("access::can-write", true);
    info.set_attribute_boolean("access::can-rename", false);
    let item = FileItem::from_gio(&gio::File::for_uri("smb://host/share"), &info);
    let availability = CommandAvailability::for_capabilities(&[item], None);
    assert!(!availability.can_cut && !availability.can_rename);
}

#[test]
fn daily_cut_is_cleared_only_for_its_terminal_operation() {
    use zorin_explorer::application::commands::*;
    let payload = ClipboardPayload::parse(ClipboardAction::Cut, "file:///tmp/a").unwrap();
    let mut clipboard = ClipboardState::default();
    clipboard.replace(payload.clone());
    clipboard.begin_paste(7);
    clipboard.finish_paste(8, true);
    assert_eq!(clipboard.payload(), Some(&payload));
    clipboard.finish_paste(7, false);
    assert_eq!(clipboard.payload(), Some(&payload));
    clipboard.finish_paste(7, true);
    assert!(clipboard.payload().is_none());
    clipboard.replace(payload.clone());
    clipboard.begin_paste(9);
    clipboard.replace(ClipboardPayload::parse(ClipboardAction::Copy, "file:///tmp/b").unwrap());
    clipboard.finish_paste(9, true);
    assert_eq!(clipboard.payload().unwrap().action, ClipboardAction::Copy);
}
use std::{cell::RefCell, rc::Rc};
use zorin_explorer::{
    application::navigation::NavigationCoordinator,
    core::{
        contracts::*,
        navigation::{NavigationStatus, TabContent, TabState},
        ports::*,
        Location, ResolvedLocation,
    },
};

#[derive(Default)]
struct ControlledSearch(RefCell<Vec<(SearchRequest, SearchCallback)>>);
impl SearchRepository for ControlledSearch {
    fn search(&self, request: SearchRequest, done: SearchCallback) {
        self.0.borrow_mut().push((request, done));
    }
}

#[test]
fn slice2_search_debounce_sends_only_latest_and_close_cancels_pending() {
    use zorin_explorer::application::search::SearchCoordinator;
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            let repo = Rc::new(ControlledSearch::default());
            let state = Rc::new(RefCell::new(TabState::new(location("file:///home"))));
            success(&mut state.borrow_mut(), "file:///home");
            let search = SearchCoordinator::new(state.clone(), repo.clone());
            for term in ["r", "re", "report"] {
                search.submit_debounced(
                    term,
                    SearchMode::Filename,
                    SearchScope::local_fallback(location("file:///home")),
                    |_| {},
                );
            }
            assert!(repo.0.borrow().is_empty());
            let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while repo.0.borrow().is_empty() {
                context.iteration(false);
                assert!(std::time::Instant::now() < end);
            }
            assert_eq!(repo.0.borrow().len(), 1);
            let (request, done) = repo.0.borrow_mut().remove(0);
            assert_eq!(request.query, "report");
            search.clear();
            state.borrow_mut().close();
            assert!(request.cancellable.is_cancelled());
            done(SearchCompletion {
                id: request.id,
                outcome: SearchOutcome::Completed {
                    matches: vec![],
                    declared_scope: request.scope,
                    scope_limited: true,
                    limits: vec![],
                },
            });
            assert!(search.outcome().is_none());
        })
        .unwrap();
}

#[test]
fn slice2_search_while_directory_is_loading_does_not_cancel_navigation_or_use_old_root() {
    use zorin_explorer::application::search::SearchCoordinator;
    let repo = Rc::new(ControlledSearch::default());
    let state = Rc::new(RefCell::new(TabState::new(location("file:///home"))));
    let search = SearchCoordinator::new(state.clone(), repo.clone());
    for target in ["file:///home", "smb://host/share"] {
        let request = state
            .borrow_mut()
            .begin_navigation(location(target))
            .unwrap();
        search.submit(
            "report",
            SearchMode::Filename,
            SearchScope::local_fallback(location("file:///home")),
            |_| {},
        );
        assert!(
            !request.request.cancellable.is_cancelled(),
            "typing must not cancel directory loading"
        );
        assert!(
            repo.0.borrow().is_empty(),
            "no search may bind to a pending or obsolete location"
        );
        state.borrow_mut().apply_directory_result(
            request.generation,
            RequestCompletion {
                id: request.request.id,
                outcome: RequestOutcome::Succeeded(snapshot(target)),
            },
        );
    }
}

#[test]
fn slice2_search_privacy_disabling_removes_saved_terms_and_submit_honors_policy() {
    use zorin_explorer::core::SearchHistory;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("searches.json");
    let mut history = SearchHistory::load_from(path.clone()).unwrap();
    history.record_submitted("report", true, 30).unwrap();
    assert_eq!(
        SearchHistory::load_from(path.clone())
            .unwrap()
            .entries()
            .len(),
        1
    );
    history.record_submitted("private term", false, 30).unwrap();
    assert!(history.entries().is_empty());
    assert!(SearchHistory::load_from(path).unwrap().entries().is_empty());
}

#[test]
fn slice2_search_provider_ordering_partial_failure_and_time_boundary_reach_real_coordinator() {
    use std::time::Duration;
    use zorin_explorer::{
        application::search::SearchCoordinator,
        infrastructure::mock_provider::{MockConfig, MockProvider, MockSearchBehavior},
    };
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let provider = Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
    let state = Rc::new(RefCell::new(TabState::new(location("smb://host/share"))));
    success(&mut state.borrow_mut(), "smb://host/share");
    let search = SearchCoordinator::new(state, provider.clone());
    let entries = ["report-z", "report-a", "report-b"].map(|name| EntrySnapshot {
        location: location(&format!("smb://host/share/{name}")),
        display_name: name.into(),
        kind: EntryKind::File,
        size: None,
        modified: None,
        content_type: None,
        capabilities: CapabilitySnapshot::default(),
    });
    let mut capabilities = CapabilitySnapshot {
        stable_order: false,
        supported: [Capability::List, Capability::SearchFilename]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    let mut scope = SearchScope::local_fallback(location("smb://host/share"));
    let finish = || {
        for _ in 0..20 {
            provider.advance(1);
            while context.pending() {
                context.iteration(false);
            }
        }
    };
    provider.set_directory(scope.root.clone(), entries.to_vec(), capabilities.clone());
    search.submit("report", SearchMode::Filename, scope.clone(), |_| {});
    finish();
    assert!(matches!(
        search.outcome(),
        Some(SearchOutcome::Unsupported { .. })
    ));
    capabilities.stable_order = true;
    provider.set_directory(scope.root.clone(), entries.to_vec(), capabilities);
    scope.max_results = 1;
    search.submit("report", SearchMode::Filename, scope.clone(), |_| {});
    finish();
    assert!(
        matches!(search.outcome(), Some(SearchOutcome::Completed { matches, .. }) if matches.len() == 1 && matches[0].display_name == "report-a")
    );
    scope.max_results = 100;
    scope.timeout = Duration::from_micros(1500);
    search.submit("report", SearchMode::Filename, scope.clone(), |_| {});
    finish();
    assert!(
        matches!(search.outcome(), Some(SearchOutcome::Completed { matches, limits, .. }) if matches.len() == 1 && limits.contains(&SearchLimit::Time))
    );
    scope.timeout = Duration::from_secs(2);
    provider.set_search_behavior(MockSearchBehavior::FailAfter {
        matches: 1,
        reason: SearchIncompleteReason::AccessLost,
    });
    search.submit("report", SearchMode::Filename, scope, |_| {});
    finish();
    assert!(
        matches!(search.outcome(), Some(SearchOutcome::Incomplete { matches, reason: SearchIncompleteReason::AccessLost, .. }) if matches.len() == 1 && matches[0].display_name == "report-a")
    );
}

#[test]
fn slice2_search_latest_query_cancels_and_clear_restores_saved_selection() {
    use zorin_explorer::application::search::SearchCoordinator;
    let repo = Rc::new(ControlledSearch::default());
    let state = Rc::new(RefCell::new(TabState::new(location("file:///home"))));
    success(&mut state.borrow_mut(), "file:///home");
    state.borrow_mut().selection_keys = vec!["file:///home/chosen".into()];
    let search = SearchCoordinator::new(state.clone(), repo.clone());
    let scope = SearchScope::local_fallback(location("file:///home"));
    assert_eq!(
        (scope.max_depth, scope.max_results, scope.timeout.as_secs()),
        (1, 5000, 2)
    );
    search.submit("one", SearchMode::Filename, scope.clone(), |_| {});
    search.submit("two", SearchMode::Filename, scope.clone(), |_| {});
    let (first, first_done) = repo.0.borrow_mut().remove(0);
    let (second, second_done) = repo.0.borrow_mut().remove(0);
    assert!(first.cancellable.is_cancelled());
    let completed = |scope| SearchOutcome::Completed {
        matches: vec![],
        declared_scope: scope,
        scope_limited: true,
        limits: vec![SearchLimit::Depth],
    };
    second_done(SearchCompletion {
        id: second.id,
        outcome: completed(scope.clone()),
    });
    first_done(SearchCompletion {
        id: first.id,
        outcome: SearchOutcome::Unsupported {
            declared_scope: scope.clone(),
            error: OperationError::new(OperationErrorKind::Unsupported, None, None),
        },
    });
    assert!(matches!(
        search.outcome(),
        Some(SearchOutcome::Completed { .. })
    ));
    state.borrow_mut().selection_keys.clear();
    search.clear();
    assert!(matches!(state.borrow().content, TabContent::Directory(_)));
    assert_eq!(state.borrow().selection_keys, ["file:///home/chosen"]);
    assert!(!state.borrow().history.can_go_back());
}

#[test]
fn slice2_search_incomplete_retains_uri_results_retry_scope_and_rejects_navigation_races() {
    use zorin_explorer::application::search::SearchCoordinator;
    let repo = Rc::new(ControlledSearch::default());
    let state = Rc::new(RefCell::new(TabState::new(location("smb://host/share"))));
    success(&mut state.borrow_mut(), "smb://host/share");
    let search = SearchCoordinator::new(state.clone(), repo.clone());
    let scope = SearchScope::local_fallback(location("smb://host/share"));
    search.submit("report", SearchMode::Filename, scope.clone(), |_| {});
    let (request, done) = repo.0.borrow_mut().remove(0);
    let entry = EntrySnapshot {
        location: location("smb://host/share/report%20one"),
        display_name: "report one".into(),
        kind: EntryKind::File,
        size: None,
        modified: None,
        content_type: None,
        capabilities: CapabilitySnapshot::default(),
    };
    done(SearchCompletion {
        id: request.id,
        outcome: SearchOutcome::Incomplete {
            matches: vec![entry.clone(), entry],
            declared_scope: scope.clone(),
            reason: SearchIncompleteReason::ProviderFailure,
            retry: RetryClass::AfterRefresh,
            error: None,
        },
    });
    let TabContent::Search { items, .. } = state.borrow().content.clone() else {
        panic!("missing retained results")
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].location().uri(), "smb://host/share/report%20one");
    assert!(search.retry(|_| {}));
    let (retried, done) = repo.0.borrow_mut().remove(0);
    assert_eq!(retried.scope, scope);
    assert_eq!(retried.query, "report");
    state
        .borrow_mut()
        .begin_navigation(location("file:///elsewhere"));
    done(SearchCompletion {
        id: retried.id,
        outcome: SearchOutcome::Completed {
            matches: vec![],
            declared_scope: scope,
            scope_limited: true,
            limits: vec![],
        },
    });
    assert!(matches!(
        search.outcome(),
        Some(SearchOutcome::Incomplete { .. })
    ));
}

fn location(uri: &str) -> Location {
    Location::from_uri(uri).unwrap()
}

#[test]
fn slice2_location_input_and_session_keep_nonlocal_identity() {
    use zorin_explorer::core::{
        application_preferences::{decode_session, encode_session},
        location::LocationInput,
    };
    for uri in ["trash:///", "smb://host/share", "network:///"] {
        let parsed = LocationInput::parse(uri).unwrap();
        assert_eq!(parsed.uri(), uri);
        assert_eq!(
            decode_session(&encode_session(&[parsed]).unwrap()).unwrap()[0].uri(),
            uri
        );
    }
    assert_eq!(
        LocationInput::parse("/tmp/a b").unwrap().uri(),
        "file:///tmp/a%20b"
    );
    assert!(LocationInput::parse("smb://user:password@host/share").is_err());
    assert!(LocationInput::parse("").is_err());
}

#[test]
fn slice2_search_status_identifies_the_exact_declared_root() {
    for uri in ["file:///home/reports", "smb://host/share/reports%20one"] {
        let scope = SearchScope::local_fallback(location(uri));
        let outcomes = [
            SearchOutcome::Completed {
                matches: vec![],
                declared_scope: scope.clone(),
                scope_limited: true,
                limits: vec![SearchLimit::Depth],
            },
            SearchOutcome::Incomplete {
                matches: vec![],
                declared_scope: scope,
                reason: SearchIncompleteReason::ProviderFailure,
                retry: RetryClass::AfterRefresh,
                error: None,
            },
        ];
        for outcome in outcomes {
            let text = outcome.status_text();
            assert!(text.contains(uri), "declared root missing from: {text}");
            for bound in ["depth 1", "5000 matches", "2 seconds"] {
                assert!(text.contains(bound), "declared bound missing from: {text}");
            }
        }
    }
}

#[test]
fn slice2_unmount_cancels_pending_and_preserves_history_for_reconnect() {
    let mut tab = TabState::new(location("file:///home"));
    success(&mut tab, "file:///home");
    success(&mut tab, "smb://host/share/folder");
    let pending = tab.begin_refresh().unwrap();
    assert!(!tab.mark_unavailable(&location("smb://other/share")));
    assert!(tab.mark_unavailable(&location("smb://host/share")));
    assert!(pending.request.cancellable.is_cancelled());
    assert!(
        matches!(&tab.content, TabContent::Unavailable(l) if l.uri() == "smb://host/share/folder")
    );
    assert_eq!(
        tab.history.current().unwrap().uri(),
        "smb://host/share/folder"
    );
    assert_eq!(tab.history.back_target().unwrap().uri(), "file:///home");
    assert!(tab.directory.is_none());
    let retry = tab.begin_retry().unwrap();
    assert_eq!(retry.request.location.uri(), "smb://host/share/folder");
    assert!(!tab.apply_directory_result(
        pending.generation,
        RequestCompletion {
            id: pending.request.id,
            outcome: RequestOutcome::Succeeded(snapshot("smb://host/share/folder")),
        }
    ));
    assert!(tab.apply_directory_result(
        retry.generation,
        RequestCompletion {
            id: retry.request.id,
            outcome: RequestOutcome::Failed(OperationError::new(
                OperationErrorKind::MountUnavailable,
                Some(&location("smb://host/share/folder")),
                None,
            )),
        }
    ));
    assert!(
        matches!(&tab.content, TabContent::Unavailable(l) if l.uri() == "smb://host/share/folder")
    );
    assert_eq!(tab.location.uri(), "smb://host/share/folder");
    assert_eq!(
        tab.history.current().unwrap().uri(),
        "smb://host/share/folder"
    );
    assert_eq!(tab.history.back_target().unwrap().uri(), "file:///home");
    assert!(tab.directory.is_none());
    let retry = tab.begin_retry().unwrap();
    assert!(tab.apply_directory_result(
        retry.generation,
        RequestCompletion {
            id: retry.request.id,
            outcome: RequestOutcome::Succeeded(snapshot("smb://host/share/folder")),
        }
    ));
    assert_eq!(tab.history.back_target().unwrap().uri(), "file:///home");
}

#[test]
fn slice2_picker_requests_remote_locations_and_cancels_on_replacement() {
    use zorin_explorer::ui::folder_picker::FolderPicker;
    let repo = Rc::new(ControlledRepository::default());
    let picker = FolderPicker::new(location("trash:///"), repo.clone());
    picker.select_location(location("smb://host/one"));
    picker.select_location(location("smb://host/two"));
    let (first, first_done) = repo.0.borrow_mut().remove(0);
    let (second, second_done) = repo.0.borrow_mut().remove(0);
    assert!(first.cancellable.is_cancelled());
    assert_eq!(second.location.uri(), "smb://host/two");
    second_done(RequestCompletion {
        id: second.id,
        outcome: RequestOutcome::Succeeded(snapshot("smb://host/two")),
    });
    first_done(RequestCompletion {
        id: first.id,
        outcome: RequestOutcome::Succeeded(snapshot("smb://host/one")),
    });
    assert_eq!(picker.current_location().uri(), "smb://host/two");
}

#[test]
fn slice2_first_reconnect_commits_initial_history_only_after_success() {
    let root = location("smb://host/share");
    let mut tab = TabState::new(root.clone());
    tab.mark_unavailable(&root);
    assert!(tab.history.current().is_none());
    let request = tab.begin_retry().unwrap();
    tab.apply_directory_result(
        request.generation,
        RequestCompletion {
            id: request.request.id,
            outcome: RequestOutcome::Succeeded(snapshot(root.uri())),
        },
    );
    assert_eq!(tab.history.current(), Some(&root));
}

#[test]
fn slice2_removed_pending_mount_preserves_unrelated_successful_view() {
    let mut tab = TabState::new(location("file:///home"));
    success(&mut tab, "file:///home");
    tab.selection_keys = vec!["file:///home/selected".into()];
    let request = tab
        .begin_navigation(location("smb://host/share/folder"))
        .unwrap();
    assert!(tab.mark_unavailable(&location("smb://host/share")));
    assert!(request.request.cancellable.is_cancelled());
    assert!(!tab.apply_directory_result(
        request.generation,
        RequestCompletion {
            id: request.request.id,
            outcome: RequestOutcome::Succeeded(snapshot("smb://host/share/folder")),
        }
    ));
    assert_eq!(tab.location.uri(), "file:///home");
    assert_eq!(tab.history.current().unwrap().uri(), "file:///home");
    assert!(matches!(tab.content, TabContent::Directory(_)));
    assert!(tab.directory.is_some());
    assert_eq!(tab.selection_keys, ["file:///home/selected"]);
    assert_eq!(
        tab.begin_retry().unwrap().request.location.uri(),
        "smb://host/share/folder"
    );
}

#[test]
fn slice2_removed_current_mount_preserves_unrelated_pending_navigation() {
    let mut tab = TabState::new(location("smb://host/share"));
    success(&mut tab, "smb://host/share");
    let request = tab.begin_navigation(location("file:///home")).unwrap();
    assert!(tab.mark_unavailable(&location("smb://host/share")));
    assert!(!request.request.cancellable.is_cancelled());
    assert!(tab.apply_directory_result(
        request.generation,
        RequestCompletion {
            id: request.request.id,
            outcome: RequestOutcome::Succeeded(snapshot("file:///home")),
        }
    ));
    assert_eq!(tab.location.uri(), "file:///home");
}

#[test]
fn slice2_section_overlay_does_not_restore_directory_availability() {
    let root = location("file:///mounted");
    let mut tab = TabState::new(root.clone());
    success(&mut tab, root.uri());
    assert!(tab.has_available_directory());
    tab.mark_unavailable(&root);
    tab.show_section(zorin_explorer::core::sidebar_model::SidebarSection {
        title: "This PC".into(),
        nodes: vec![],
    });
    assert!(!tab.has_available_directory());
}

#[test]
fn slice2_picker_includes_and_activates_network_shortcuts() {
    use zorin_explorer::ui::folder_picker::FolderPicker;
    let repo = Rc::new(ControlledRepository::default());
    let picker = FolderPicker::new(location("network:///"), repo.clone());
    picker.select_location(location("network:///"));
    let (request, done) = repo.0.borrow_mut().remove(0);
    let target = location("smb://server/");
    let mut directory = snapshot("network:///");
    directory.entries = vec![
        EntrySnapshot {
            location: target.clone(),
            display_name: "Network server".into(),
            kind: EntryKind::Shortcut,
            size: None,
            modified: None,
            content_type: None,
            capabilities: CapabilitySnapshot::default(),
        },
        EntrySnapshot {
            location: location("network:///file"),
            display_name: "Regular file".into(),
            kind: EntryKind::File,
            size: None,
            modified: None,
            content_type: None,
            capabilities: CapabilitySnapshot::default(),
        },
    ];
    done(RequestCompletion {
        id: request.id,
        outcome: RequestOutcome::Succeeded(directory),
    });
    assert_eq!(picker.entries().len(), 1);
    assert_eq!(picker.entries()[0].location, target);
    picker.activate_entry(0);
    let (activated, _) = repo.0.borrow_mut().remove(0);
    assert_eq!(activated.location, target);
}
fn snapshot(uri: &str) -> DirectorySnapshot {
    let location = location(uri);
    DirectorySnapshot {
        location: ResolvedLocation {
            requested: location.clone(),
            resolved: location,
        },
        entries: vec![],
        capabilities: CapabilitySnapshot::default(),
        next_page: None,
    }
}
fn success(tab: &mut TabState, uri: &str) {
    let request = tab.begin_navigation(location(uri)).unwrap();
    assert!(tab.apply_directory_result(
        request.generation,
        RequestCompletion {
            id: request.request.id,
            outcome: RequestOutcome::Succeeded(snapshot(uri)),
        }
    ));
}

#[test]
fn navigation_refresh_from_section_replaces_pending_directory_request() {
    use zorin_explorer::core::sidebar_model::SidebarSection;
    let mut tab = TabState::new(location("file:///home"));
    success(&mut tab, "file:///home");
    tab.show_section(SidebarSection {
        title: "Network & online".into(),
        nodes: vec![],
    });
    let first = tab.begin_navigation(location("smb://host/share")).unwrap();
    assert!(tab.has_pending_navigation());
    let replacement = tab.begin_refresh().unwrap();
    assert!(first.request.cancellable.is_cancelled());
    assert!(tab.has_pending_navigation());
    assert!(replacement.generation > first.generation);
    assert_eq!(replacement.request.location.uri(), "smb://host/share");
    assert!(matches!(tab.content, TabContent::SectionOverview { .. }));
    assert!(!tab.apply_directory_result(
        first.generation,
        RequestCompletion {
            id: first.request.id,
            outcome: RequestOutcome::Succeeded(snapshot("smb://host/share"))
        }
    ));
    assert!(tab.apply_directory_result(
        replacement.generation,
        RequestCompletion {
            id: replacement.request.id,
            outcome: RequestOutcome::Succeeded(snapshot("smb://host/share"))
        }
    ));
    assert!(!tab.has_pending_navigation());
    assert_eq!(tab.location.uri(), "smb://host/share");
}

#[test]
fn navigation_latest_generation_wins_and_only_success_commits_history() {
    let mut tab = TabState::new(location("file:///home"));
    assert!(tab.history.current().is_none());
    success(&mut tab, "file:///home");
    let old = tab.begin_navigation(location("file:///old")).unwrap();
    let new = tab.begin_navigation(location("smb://host/share")).unwrap();
    assert!(old.request.cancellable.is_cancelled());
    assert_eq!(tab.location.uri(), "file:///home");
    assert!(!tab.apply_directory_result(
        old.generation,
        RequestCompletion {
            id: old.request.id,
            outcome: RequestOutcome::Succeeded(snapshot("file:///old")),
        }
    ));
    assert!(tab.apply_directory_result(
        new.generation,
        RequestCompletion {
            id: new.request.id,
            outcome: RequestOutcome::Succeeded(snapshot("smb://host/share")),
        }
    ));
    assert_eq!(tab.history.current().unwrap().uri(), "smb://host/share");
    assert_eq!(tab.history.back_target().unwrap().uri(), "file:///home");
    assert!(matches!(&tab.content, TabContent::Directory(l) if l.uri() == "smb://host/share"));
}

#[test]
fn navigation_failure_and_failed_back_preserve_view_history_and_retry_target() {
    let mut tab = TabState::new(location("file:///home"));
    success(&mut tab, "file:///home");
    success(&mut tab, "smb://host/share");
    for back in [false, true] {
        let request = if back {
            tab.begin_back()
        } else {
            tab.begin_navigation(location("smb://host/missing"))
        }
        .unwrap();
        let target = request.request.location.clone();
        assert!(tab.apply_directory_result(
            request.generation,
            RequestCompletion {
                id: request.request.id,
                outcome: RequestOutcome::Failed(OperationError::new(
                    OperationErrorKind::NotFound,
                    Some(&target),
                    None
                )),
            }
        ));
        assert_eq!(tab.location.uri(), "smb://host/share");
        assert_eq!(tab.history.current().unwrap().uri(), "smb://host/share");
        assert!(
            matches!(&tab.status, NavigationStatus::Failed { location, .. } if location == &target)
        );
        assert!(matches!(&tab.content, TabContent::Directory(l) if l.uri() == "smb://host/share"));
    }
    let request = tab.begin_back().unwrap();
    tab.apply_directory_result(
        request.generation,
        RequestCompletion {
            id: request.request.id,
            outcome: RequestOutcome::Succeeded(snapshot("file:///home")),
        },
    );
    assert_eq!(
        tab.history.forward_target().unwrap().uri(),
        "smb://host/share"
    );
    success(&mut tab, "file:///branch");
    assert!(tab.history.forward_target().is_none());
}

#[derive(Default)]
struct ControlledRepository(RefCell<Vec<(DirectoryRequest, DirectoryCallback)>>);
impl DirectoryRepository for ControlledRepository {
    fn list(&self, request: DirectoryRequest, callback: DirectoryCallback) {
        self.0.borrow_mut().push((request, callback));
    }
}

#[test]
fn navigation_refresh_replaces_pending_target_and_close_rejects_late_completion() {
    let repo = Rc::new(ControlledRepository::default());
    let state = Rc::new(RefCell::new(TabState::new(location("file:///home"))));
    let coordinator = NavigationCoordinator::new(state.clone(), repo.clone());
    coordinator.navigate(location("smb://host/share"), |_| {});
    coordinator.refresh(|_| {});
    let (first, first_done) = repo.0.borrow_mut().remove(0);
    let (second, second_done) = repo.0.borrow_mut().remove(0);
    assert!(first.cancellable.is_cancelled());
    assert_eq!(second.location.uri(), "smb://host/share");
    assert_ne!(first.id, second.id);
    first_done(RequestCompletion {
        id: first.id,
        outcome: RequestOutcome::Succeeded(snapshot("smb://host/share")),
    });
    assert!(state.borrow().history.current().is_none());
    coordinator.close_tab();
    assert!(second.cancellable.is_cancelled());
    second_done(RequestCompletion {
        id: second.id,
        outcome: RequestOutcome::Succeeded(snapshot("smb://host/share")),
    });
    assert!(state.borrow().history.current().is_none());
    assert!(matches!(state.borrow().status, NavigationStatus::Closed));
    assert!(!coordinator.navigate(location("file:///reopened"), |_| {}));
}

#[test]
fn navigation_refresh_preserves_existing_uri_selection_and_redirect_commits_once() {
    let mut tab = TabState::new(location("file:///a"));
    success(&mut tab, "file:///a");
    tab.selection_keys = vec!["file:///a/keep".into(), "file:///a/gone".into()];
    let refresh = tab.begin_refresh().unwrap();
    let mut refreshed = snapshot("file:///a");
    refreshed.entries.push(EntrySnapshot {
        location: location("file:///a/keep"),
        display_name: "keep".into(),
        kind: EntryKind::File,
        size: None,
        modified: None,
        content_type: None,
        capabilities: CapabilitySnapshot::default(),
    });
    tab.apply_directory_result(
        refresh.generation,
        RequestCompletion {
            id: refresh.request.id,
            outcome: RequestOutcome::Succeeded(refreshed),
        },
    );
    assert_eq!(tab.selection_keys, ["file:///a/keep"]);
    assert!(!tab.history.can_go_back());
    let redirect = tab.begin_navigation(location("smb://host/alias")).unwrap();
    let mut redirected = snapshot("smb://host/alias");
    redirected.location.resolved = location("smb://host/resolved");
    tab.apply_directory_result(
        redirect.generation,
        RequestCompletion {
            id: redirect.request.id,
            outcome: RequestOutcome::Succeeded(redirected),
        },
    );
    assert!(tab.selection_keys.is_empty());
    assert_eq!(tab.history.current().unwrap().uri(), "smb://host/resolved");
    assert_eq!(tab.history.back_target().unwrap().uri(), "file:///a");
}

#[test]
fn navigation_retry_preserves_failed_history_intent() {
    let mut tab = TabState::new(location("file:///a"));
    success(&mut tab, "file:///a");
    success(&mut tab, "file:///b");
    let back = tab.begin_back().unwrap();
    tab.apply_directory_result(
        back.generation,
        RequestCompletion {
            id: back.request.id,
            outcome: RequestOutcome::Failed(OperationError::new(
                OperationErrorKind::MountUnavailable,
                None,
                None,
            )),
        },
    );
    let retry = tab.begin_retry().unwrap();
    assert_eq!(retry.request.location.uri(), "file:///a");
    tab.apply_directory_result(
        retry.generation,
        RequestCompletion {
            id: retry.request.id,
            outcome: RequestOutcome::Succeeded(snapshot("file:///a")),
        },
    );
    assert!(!tab.history.can_go_back());
    assert_eq!(tab.history.forward_target().unwrap().uri(), "file:///b");
}

#[test]
fn navigation_rejects_wrong_request_duplicate_and_mismatched_snapshot() {
    let mut tab = TabState::new(location("file:///home"));
    let request = tab.begin_navigation(location("smb://host/share")).unwrap();
    assert!(!tab.apply_directory_result(
        request.generation,
        RequestCompletion {
            id: RequestId(u64::MAX),
            outcome: RequestOutcome::Succeeded(snapshot("smb://host/share"))
        }
    ));
    assert!(tab.apply_directory_result(
        request.generation,
        RequestCompletion {
            id: request.request.id,
            outcome: RequestOutcome::Succeeded(snapshot("file:///wrong"))
        }
    ));
    assert!(tab.history.current().is_none());
    assert!(matches!(tab.status, NavigationStatus::Failed { .. }));
    assert!(!tab.apply_directory_result(
        request.generation,
        RequestCompletion {
            id: request.request.id,
            outcome: RequestOutcome::Succeeded(snapshot("smb://host/share"))
        }
    ));
}
