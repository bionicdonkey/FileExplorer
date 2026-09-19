use std::rc::Rc;

use gtk::prelude::*;
use zorin_explorer::application::operations::{CancelResult, OperationCoordinator, OperationState};
use zorin_explorer::core::contracts::*;
use zorin_explorer::core::Location;
use zorin_explorer::infrastructure::mock_provider::{MockConfig, MockProvider};
use zorin_explorer::ui::context_menu::{
    configure_command_button, CommandAvailability, ContextCommand,
};

#[test]
fn slice2_navigation_commits_after_success_and_refresh_restores_selection() {
    use std::cell::RefCell;
    use zorin_explorer::{core::navigation::NavigationStatus, ui::file_view::FileView};
    gtk::init().expect("headless GTK must initialize");
    let context = glib::MainContext::default();
    let _guard = context.acquire().unwrap();
    let provider = Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
    let root = Location::from_uri("file:///test").unwrap();
    let entry = EntrySnapshot {
        location: Location::from_uri("file:///test/selected").unwrap(),
        display_name: "selected".into(),
        kind: EntryKind::File,
        size: Some(4),
        modified: None,
        content_type: None,
        capabilities: CapabilitySnapshot::default(),
    };
    provider.set_directory(
        root.clone(),
        vec![entry.clone()],
        CapabilitySnapshot {
            stable_order: true,
            supported: [Capability::List].into_iter().collect(),
            ..Default::default()
        },
    );
    let view = FileView::new_with_repository(root.clone(), provider.clone());
    view.set_options(zorin_explorer::core::ViewOptions {
        view_mode: zorin_explorer::core::ViewMode::DetailedList,
        group_field: zorin_explorer::core::GroupField::None,
        ..Default::default()
    });
    assert!(view.tab_state().borrow().history.current().is_none());
    provider.advance(1);
    while context.pending() {
        context.iteration(false);
    }
    assert_eq!(view.current_location(), root);
    let column = view.column_view().unwrap();
    let model = column
        .model()
        .unwrap()
        .downcast::<gtk::SingleSelection>()
        .unwrap();
    model.set_selected(0);
    assert_eq!(
        view.tab_state().borrow().selection_keys,
        ["file:///test/selected"]
    );
    view.refresh();
    provider.advance(1);
    while context.pending() {
        context.iteration(false);
    }
    assert_eq!(
        view.tab_state().borrow().selection_keys,
        ["file:///test/selected"]
    );
    let column = view.column_view().unwrap();
    let model = column
        .model()
        .unwrap()
        .downcast::<gtk::SingleSelection>()
        .unwrap();
    assert_eq!(model.selected(), 0);
    view.set_uri("smb://host/missing".into());
    assert_eq!(view.current_location(), root);
    provider.advance(1);
    while context.pending() {
        context.iteration(false);
    }
    assert!(matches!(
        view.tab_state().borrow().status,
        NavigationStatus::Failed { .. }
    ));
    assert_eq!(view.current_location(), root);
    assert_eq!(view.tab_state().borrow().history.current(), Some(&root));

    view.show_section_overview(zorin_explorer::core::sidebar_model::SidebarSection {
        title: "Network & online".into(),
        nodes: vec![],
    });
    view.set_uri("smb://host/missing".into());
    let generation = view.tab_state().borrow().navigation_generation;
    view.refresh();
    assert!(
        view.tab_state().borrow().navigation_generation > generation,
        "refresh must replace the pending request even while a section remains visible"
    );
    provider.advance(1);
    while context.pending() {
        context.iteration(false);
    }
    assert_eq!(view.section_title().as_deref(), Some("Network & online"));
    assert_eq!(view.current_location(), root);
    assert!(matches!(
        view.tab_state().borrow().status,
        NavigationStatus::Failed { .. }
    ));
    fn retry_button(widget: &gtk::Widget) -> Option<gtk::Button> {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.label().as_deref() == Some("Retry") {
                return Some(button.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(button) = retry_button(&widget) {
                return Some(button);
            }
            child = widget.next_sibling();
        }
        None
    }
    let retry =
        retry_button(view.widget.upcast_ref()).expect("failed section navigation must offer Retry");
    provider.set_directory(
        Location::from_uri("smb://host/missing").unwrap(),
        vec![],
        CapabilitySnapshot {
            stable_order: true,
            supported: [Capability::List].into_iter().collect(),
            ..Default::default()
        },
    );
    retry.emit_clicked();
    provider.advance(1);
    while context.pending() {
        context.iteration(false);
    }
    assert_eq!(view.section_title(), None);
    assert_eq!(view.current_location().uri(), "smb://host/missing");

    // The same address component is used by the window and folder picker.
    let bar = zorin_explorer::ui::header::LocationBar::new(&root);
    let entered = Rc::new(RefCell::new(Vec::new()));
    bar.connect_location_changed({
        let entered = entered.clone();
        move |location| entered.borrow_mut().push(location)
    });
    bar.set_uri("smb://host/missing");
    bar.set_section("Network & online");
    bar.focus_location();
    assert_eq!(bar.widget.visible_child_name().as_deref(), Some("entry"));
    let entry = bar
        .widget
        .child_by_name("entry")
        .unwrap()
        .downcast::<gtk::Entry>()
        .unwrap();
    assert_eq!(
        entry.text(),
        "smb://host/missing",
        "overview editing retains the underlying URI"
    );
    entry.set_text("trash:///");
    entry.emit_activate();
    assert_eq!(entered.borrow()[0].uri(), "trash:///");
    entry.set_text("smb://user:secret@host/share");
    entry.emit_activate();
    assert_eq!(entered.borrow().len(), 1);
    assert!(entry.has_css_class("error"));

    fn named_button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.label().as_deref() == Some(label) {
                return Some(button.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(button) = named_button(&widget, label) {
                return Some(button);
            }
            child = widget.next_sibling();
        }
        None
    }
    let removed_root = Location::from_uri("smb://host/").unwrap();
    view.mark_unavailable(&removed_root);
    assert!(
        matches!(&view.tab_state().borrow().content, zorin_explorer::core::navigation::TabContent::Unavailable(location) if location.uri() == "smb://host/missing")
    );
    assert_eq!(view.tab_state().borrow().history.back_target(), Some(&root));
    named_button(view.widget.upcast_ref(), "Reconnect")
        .unwrap()
        .emit_clicked();
    provider.advance(1);
    while context.pending() {
        context.iteration(false);
    }
    assert!(matches!(
        view.tab_state().borrow().status,
        NavigationStatus::Ready
    ));
    assert_eq!(view.tab_state().borrow().history.back_target(), Some(&root));

    let activated = Rc::new(RefCell::new(None));
    view.set_navigation_handler({
        let activated = activated.clone();
        move |location| *activated.borrow_mut() = Some(location)
    });
    let result_location = Location::from_uri("smb://host/result").unwrap();
    let result = EntrySnapshot {
        location: result_location.clone(),
        display_name: "A remote result".into(),
        kind: EntryKind::Directory,
        size: None,
        modified: None,
        content_type: None,
        capabilities: CapabilitySnapshot::default(),
    };
    view.show_search_results(vec![zorin_explorer::core::FileItem::from_snapshot(&result)]);
    view.column_view()
        .unwrap()
        .emit_by_name::<()>("activate", &[&0u32]);
    assert_eq!(*activated.borrow(), Some(result_location));

    let closed = Rc::new(std::cell::Cell::new(false));
    view.set_close_handler({
        let closed = closed.clone();
        move || closed.set(true)
    });
    view.mark_unavailable(&removed_root);
    named_button(view.widget.upcast_ref(), "Close")
        .unwrap()
        .emit_clicked();
    assert!(closed.get());
    view.close();

    let unavailable_local = FileView::new_with_repository(root.clone(), provider.clone());
    provider.advance(1);
    while context.pending() {
        context.iteration(false);
    }
    unavailable_local.mark_unavailable(&root);
    assert!(!unavailable_local.command_availability().can_paste);
    assert_eq!(unavailable_local.current_location(), root);
    unavailable_local.close();
}

fn remote_item() -> zorin_explorer::core::FileItem {
    let parent = gio::File::for_uri("smb://host/share");
    let info = gio::FileInfo::new();
    info.set_name("report.txt");
    info.set_display_name("report.txt");
    info.set_file_type(gio::FileType::Regular);
    zorin_explorer::core::FileItem::from_gio(&parent, &info)
}

#[test]
fn slice1_disabled_trash_action_exposes_a_safe_name_and_reason() {
    gtk::init().expect("headless GTK must initialize");
    let availability = CommandAvailability::for_selection(&[remote_item()], None);
    let button = gtk::Button::with_label("Move to Trash");

    configure_command_button(&button, &availability, ContextCommand::MoveToTrash);

    assert_eq!(button.label().as_deref(), Some("Move to Trash"));
    assert!(!button.is_sensitive());
    assert_eq!(
        button.tooltip_text().as_deref(),
        Some("This location does not support Trash")
    );
}

#[test]
fn slice1_reenabled_action_clears_the_stale_disabled_explanation() {
    gtk::init().expect("headless GTK must initialize");
    let button = gtk::Button::with_label("Move to Trash");
    configure_command_button(
        &button,
        &CommandAvailability::for_selection(&[remote_item()], None),
        ContextCommand::MoveToTrash,
    );
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("local.txt");
    std::fs::write(&path, "contents").unwrap();
    let local = zorin_explorer::core::FileItem::from_path(&path).unwrap();

    configure_command_button(
        &button,
        &CommandAvailability::for_selection(&[local], None),
        ContextCommand::MoveToTrash,
    );

    assert!(button.is_sensitive());
    assert_eq!(button.tooltip_text(), None);
}

#[test]
fn slice1_cancel_state_and_error_text_remain_explicit_and_redacted() {
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let provider = Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
    let coordinator = OperationCoordinator::new(provider.clone());
    let source = Location::from_uri("file:///source").unwrap();
    let destination = Location::from_uri("file:///destination").unwrap();
    let mut capabilities = CapabilitySnapshot::default();
    capabilities
        .supported
        .extend([Capability::Copy, Capability::Create]);
    coordinator
        .submit(
            OperationRequest {
                id: OperationId(7),
                kind: MutationKind::Copy,
                sources: vec![source],
                destination: Some(destination),
                explicit_target: None,
                capability_version: 1,
                cancellable: gio::Cancellable::new(),
            },
            capabilities,
        )
        .unwrap();

    assert_eq!(coordinator.cancel(OperationId(7)), CancelResult::Requested);
    assert_eq!(
        coordinator.records()[0].state,
        OperationState::CancelRequested
    );
    let error = OperationError::new(
        OperationErrorKind::PermissionDenied,
        Some(&Location::from_uri("smb://host/private/report.txt").unwrap()),
        None,
    );
    assert_eq!(error.message_key, "operation.permission-denied");
    assert_eq!(error.source.as_deref(), Some("smb://host/…"));
}
