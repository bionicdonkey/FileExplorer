use gio::prelude::*;
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
