use std::rc::Rc;

#[gtk::test]
fn slice3_multiple_selection_routes_one_uri_command_and_drag_payload() {
    use std::cell::RefCell;
    use zorin_explorer::{
        application::commands::ClipboardAction,
        core::{GroupField, ViewMode, ViewOptions},
        ui::file_view::FileView,
    };
    let context = glib::MainContext::default();
    let provider = Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
    let root = Location::from_uri("smb://host/share").unwrap();
    let capabilities = CapabilitySnapshot {
        version: 1,
        stable_order: true,
        supported: [
            Capability::List,
            Capability::Copy,
            Capability::Move,
            Capability::Create,
            Capability::Open,
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    provider.set_directory(
        root.clone(),
        ["a", "b"]
            .into_iter()
            .map(|name| EntrySnapshot {
                location: Location::from_uri(&format!("smb://host/share/{name}")).unwrap(),
                display_name: name.into(),
                kind: EntryKind::File,
                size: None,
                modified: None,
                content_type: None,
                capabilities: capabilities.clone(),
            })
            .collect(),
        capabilities,
    );
    let view = FileView::new_with_repository(root.clone(), provider.clone());
    view.set_options(ViewOptions {
        view_mode: ViewMode::DetailedList,
        group_field: GroupField::None,
        ..Default::default()
    });
    provider.advance(1);
    while context.pending() {
        context.iteration(false);
    }
    let model = view.column_view().unwrap().model().unwrap();
    model.select_item(0, true);
    model.select_item(1, false);
    assert_eq!(view.selected_items().len(), 2);
    assert!(view.command_availability().can_copy);
    let commands = Rc::new(RefCell::new(Vec::new()));
    let output = commands.clone();
    view.set_application_handler(move |command| output.borrow_mut().push(command));
    view.dispatch_context_command(view.selected_target().unwrap(), ContextCommand::Copy);
    assert_eq!(commands.borrow()[0].selection.len(), 2);
    let application = gio::AppInfo::create_from_commandline(
        "true",
        Some("Fixture viewer"),
        gio::AppInfoCreateFlags::SUPPORTS_URIS,
    )
    .unwrap();
    view.dispatch_open_with(application);
    assert_eq!(
        commands.borrow().last().unwrap().kind,
        ContextCommand::OpenWith
    );
    assert_eq!(commands.borrow().last().unwrap().selection.len(), 2);
    commands.borrow_mut().pop();
    let payload = view.drag_payload(ClipboardAction::Copy).unwrap();
    assert_eq!(payload.locations.len(), 2);
    assert!(view.dispatch_drop(&payload.uri_list(), ClipboardAction::Copy));
    assert_eq!(commands.borrow()[1].selection.len(), 2);
    assert_eq!(commands.borrow()[1].destination, root);
    assert!(!view.dispatch_drop("smb://user:password@host/share/a", ClipboardAction::Copy));
    let folder = zorin_explorer::core::FileItem::from_snapshot(&EntrySnapshot {
        location: Location::from_uri("smb://host/share/folder").unwrap(),
        display_name: "folder".into(),
        kind: EntryKind::Directory,
        size: None,
        modified: None,
        content_type: None,
        capabilities: CapabilitySnapshot {
            supported: [Capability::Create].into_iter().collect(),
            ..Default::default()
        },
    });
    assert!(view.dispatch_drop_on(&folder, &payload.uri_list(), ClipboardAction::Copy));
    assert_eq!(
        commands.borrow().last().unwrap().destination.uri(),
        "smb://host/share/folder"
    );
    view.set_options(ViewOptions {
        view_mode: ViewMode::IconGrid,
        group_field: GroupField::None,
        ..Default::default()
    });
    assert_eq!(view.tab_state().borrow().selection_keys.len(), 2);
    let window = gtk::Window::new();
    window.set_child(Some(&view.widget));
    window.present();
    while context.pending() {
        context.iteration(false);
    }
    assert_eq!(
        view.selected_items().len(),
        2,
        "mapping the grid must retain the URI selection"
    );
    fn flow(widget: &gtk::Widget) -> Option<gtk::FlowBox> {
        if let Ok(flow) = widget.clone().downcast::<gtk::FlowBox>() {
            return Some(flow);
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            if let Some(found) = flow(&next) {
                return Some(found);
            }
            child = next.next_sibling();
        }
        None
    }
    fn menu_button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Ok(button) = widget.clone().downcast::<gtk::Button>() {
            if button.label().as_deref() == Some(label) {
                return Some(button);
            }
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            if let Some(found) = menu_button(&next, label) {
                return Some(found);
            }
            child = next.next_sibling();
        }
        None
    }
    let grid = flow(view.widget.upcast_ref()).unwrap();
    let tile = grid.child_at_index(0).unwrap().child().unwrap();
    let controllers = tile.observe_controllers();
    let touch = (0..controllers.n_items())
        .find_map(|i| controllers.item(i).and_downcast::<gtk::GestureLongPress>())
        .unwrap();
    touch.emit_by_name::<()>("pressed", &[&1.0_f64, &1.0_f64]);
    menu_button(&tile, "Copy ›").unwrap().emit_clicked();
    menu_button(&tile, "Copy").unwrap().emit_clicked();
    assert_eq!(commands.borrow().last().unwrap().selection.len(), 2);
    assert_eq!(commands.borrow().last().unwrap().kind, ContextCommand::Copy);
    window.close();
    view.close();
}

#[gtk::test]
fn slice3_properties_reports_async_errors_and_unavailable_fields() {
    use zorin_explorer::ui::properties_dialog::PropertiesDialog;
    let directory = tempfile::tempdir().unwrap();
    let dialog = PropertiesDialog::present(
        None,
        Location::from_local_path(&directory.path().join("missing")),
        || {},
    );
    fn labels(widget: &gtk::Widget) -> String {
        let mut text = widget
            .clone()
            .downcast::<gtk::Label>()
            .ok()
            .map(|l| l.text().to_string())
            .unwrap_or_default();
        let mut child = widget.first_child();
        while let Some(next) = child {
            text.push_str(&labels(&next));
            child = next.next_sibling();
        }
        text
    }
    assert!(labels(dialog.upcast_ref()).contains("Loading"));
    let context = glib::MainContext::default();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while labels(dialog.upcast_ref()).contains("Loading") {
        context.iteration(false);
        assert!(std::time::Instant::now() < end);
    }
    let text = labels(dialog.upcast_ref());
    assert!(text.contains("Unavailable"));
    assert!(text.contains("operation.not-found"));
    dialog.close();
}

#[gtk::test]
#[allow(deprecated)]
fn slice3_properties_close_restores_focus_and_operation_center_reports_completion() {
    use zorin_explorer::{
        core::{OperationManager, OperationState},
        ui::{operations_page, properties_dialog::PropertiesDialog},
    };
    let directory = tempfile::tempdir().unwrap();
    let parent = gtk::Window::new();
    let entry = gtk::Entry::new();
    parent.set_child(Some(&entry));
    parent.present();
    entry.grab_focus();
    let focused = gtk::prelude::GtkWindowExt::focus(&parent).unwrap();
    let dialog = PropertiesDialog::present(
        Some(&parent),
        Location::from_local_path(directory.path()),
        || {},
    );
    dialog.response(gtk::ResponseType::Close);
    let context = glib::MainContext::default();
    while context.pending() {
        context.iteration(false);
    }
    assert_eq!(gtk::prelude::GtkWindowExt::focus(&parent), Some(focused));
    let manager = OperationManager::default();
    let path = directory.path().join("created");
    let id = manager.submit_locations(
        MutationKind::Create,
        vec![Location::from_local_path(&path)],
        None,
        None,
        None,
        false,
    );
    let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while manager
        .records()
        .iter()
        .find(|r| r.id == id)
        .unwrap()
        .finished_at
        .is_none()
    {
        context.iteration(false);
        assert!(std::time::Instant::now() < end);
    }
    assert_eq!(manager.records()[0].state, OperationState::Completed);
    let center = operations_page::build(manager);
    fn text(widget: &gtk::Widget) -> String {
        let mut result = widget
            .clone()
            .downcast::<gtk::Label>()
            .ok()
            .map(|l| l.text().to_string())
            .unwrap_or_default();
        let mut child = widget.first_child();
        while let Some(next) = child {
            result.push_str(&text(&next));
            child = next.next_sibling();
        }
        result
    }
    assert!(text(center.upcast_ref()).contains("Completed"));
    parent.close();
}

use gtk::prelude::*;

#[gtk::test]
#[allow(deprecated)]
fn slice3_permanent_delete_confirmation_cancels_or_deletes_all_uri_items() {
    use adw::prelude::*;
    use zorin_explorer::{
        core::OperationManager,
        ui::{file_view::FileView, window::confirm_permanent_delete},
    };
    let fixture = tempfile::tempdir().unwrap();
    let locations: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|name| {
            let path = fixture.path().join(name);
            std::fs::write(&path, "data").unwrap();
            Location::from_local_path(&path)
        })
        .collect();
    let view = FileView::new(Location::from_local_path(fixture.path()));
    let parent = gtk::Window::new();
    parent.set_child(Some(&view.widget));
    parent.present();
    let manager = OperationManager::default();
    let cancelled = confirm_permanent_delete(&view, locations.clone(), manager.clone());
    cancelled.response("cancel");
    assert!(manager.records().is_empty());
    assert!(fixture.path().join("a").exists());
    let confirmed = confirm_permanent_delete(&view, locations, manager.clone());
    confirmed.response("delete");
    let context = glib::MainContext::default();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while manager.active_count() > 0 {
        context.iteration(false);
        assert!(std::time::Instant::now() < end);
    }
    assert!(!fixture.path().join("a").exists() && !fixture.path().join("b").exists());
    assert_eq!(
        manager
            .summary(manager.records()[0].id)
            .unwrap()
            .completed
            .len(),
        2
    );
    parent.close();
    view.close();
}
use zorin_explorer::application::operations::{CancelResult, OperationCoordinator, OperationState};
use zorin_explorer::core::contracts::*;
use zorin_explorer::core::Location;
use zorin_explorer::infrastructure::mock_provider::{MockConfig, MockProvider};
use zorin_explorer::ui::context_menu::{
    configure_command_button, CommandAvailability, ContextCommand,
};

// GTK owns the default main context for the lifetime of its initializing thread.
// Its test attribute runs every GTK test on that same persistent worker.
#[gtk::test]
fn slice2_search_scope_incomplete_retry_clear_and_remote_activation() {
    use std::cell::RefCell;
    use zorin_explorer::{core::ports::*, ui::file_view::FileView};
    #[derive(Default)]
    struct SearchDouble(RefCell<Vec<(SearchRequest, SearchCallback)>>);
    impl SearchRepository for SearchDouble {
        fn search(&self, request: SearchRequest, done: SearchCallback) {
            self.0.borrow_mut().push((request, done));
        }
    }
    fn texts(widget: &gtk::Widget) -> String {
        let mut text = widget
            .clone()
            .downcast::<gtk::Label>()
            .ok()
            .map(|l| l.text().to_string())
            .unwrap_or_default();
        let mut child = widget.first_child();
        while let Some(current) = child {
            text.push_str(&texts(&current));
            child = current.next_sibling();
        }
        text
    }
    fn button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Ok(button) = widget.clone().downcast::<gtk::Button>() {
            if button.label().as_deref() == Some(label) {
                return Some(button);
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(button) = button(&current, label) {
                return Some(button);
            }
            child = current.next_sibling();
        }
        None
    }
    fn column(widget: &gtk::Widget) -> Option<gtk::ColumnView> {
        if let Ok(column) = widget.clone().downcast::<gtk::ColumnView>() {
            return Some(column);
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(column) = column(&current) {
                return Some(column);
            }
            child = current.next_sibling();
        }
        None
    }
    gtk::init().expect("headless GTK must initialize");
    let context = glib::MainContext::default();
    let _guard = context.acquire().unwrap();
    let provider = Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
    let root = Location::from_uri("smb://host/share").unwrap();
    let saved = EntrySnapshot {
        location: Location::from_uri("smb://host/share/saved").unwrap(),
        display_name: "saved".into(),
        kind: EntryKind::File,
        size: None,
        modified: None,
        content_type: None,
        capabilities: CapabilitySnapshot::default(),
    };
    provider.set_directory(
        root.clone(),
        vec![saved],
        CapabilitySnapshot {
            stable_order: true,
            supported: [Capability::List].into_iter().collect(),
            ..Default::default()
        },
    );
    let search = Rc::new(SearchDouble::default());
    let view = FileView::new_with_repositories(root.clone(), provider.clone(), search.clone());
    view.set_options(zorin_explorer::core::ViewOptions {
        view_mode: zorin_explorer::core::ViewMode::DetailedList,
        group_field: zorin_explorer::core::GroupField::None,
        ..Default::default()
    });
    provider.advance(1);
    while context.pending() {
        context.iteration(false);
    }
    column(view.widget.upcast_ref())
        .unwrap()
        .model()
        .unwrap()
        .downcast::<gtk::MultiSelection>()
        .unwrap()
        .select_item(0, true);
    view.search("report", SearchMode::Filename, false);
    let (request, done) = search.0.borrow_mut().remove(0);
    let result_location = Location::from_uri("smb://host/share/report%20one").unwrap();
    let entry = EntrySnapshot {
        location: result_location.clone(),
        display_name: "report one".into(),
        kind: EntryKind::Directory,
        size: None,
        modified: None,
        content_type: None,
        capabilities: CapabilitySnapshot::default(),
    };
    done(SearchCompletion {
        id: request.id,
        outcome: SearchOutcome::Incomplete {
            matches: vec![entry],
            declared_scope: request.scope.clone(),
            reason: SearchIncompleteReason::ProviderFailure,
            retry: RetryClass::AfterRefresh,
            error: None,
        },
    });
    let rendered = texts(view.widget.upcast_ref());
    assert!(rendered.contains("Search incomplete"));
    assert!(rendered.contains("smb://host/share"));
    assert!(
        rendered.contains("5000") && rendered.contains("2 seconds") && rendered.contains("depth 1")
    );
    let activated = Rc::new(RefCell::new(None));
    let output = activated.clone();
    view.set_navigation_handler(move |location| *output.borrow_mut() = Some(location));
    let result_column = column(view.widget.upcast_ref()).unwrap();
    result_column
        .model()
        .unwrap()
        .downcast::<gtk::MultiSelection>()
        .unwrap()
        .select_item(0, true);
    result_column.emit_by_name::<()>("activate", &[&0u32]);
    assert_eq!(*activated.borrow(), Some(result_location));
    button(view.widget.upcast_ref(), "Retry search")
        .unwrap()
        .emit_clicked();
    let (retry, done) = search.0.borrow_mut().remove(0);
    assert_eq!(retry.scope, request.scope);
    done(SearchCompletion {
        id: retry.id,
        outcome: SearchOutcome::Completed {
            matches: vec![],
            declared_scope: retry.scope,
            scope_limited: true,
            limits: vec![
                SearchLimit::Depth,
                SearchLimit::ResultCount,
                SearchLimit::Time,
            ],
        },
    });
    assert!(view.selected_target().is_none());
    assert!(button(view.widget.upcast_ref(), "Expand search scope…").is_some());
    view.clear_search_results();
    assert!(!view.is_showing_search_results());
    assert_eq!(
        view.tab_state().borrow().selection_keys,
        ["smb://host/share/saved"]
    );
    assert!(!view.tab_state().borrow().history.can_go_back());
    // Failed navigation after leaving search must restore the directory's
    // selected target, not keep an invisible search result actionable.
    view.search("report", SearchMode::Filename, false);
    let (request, done) = search.0.borrow_mut().remove(0);
    let result = EntrySnapshot {
        location: Location::from_uri("smb://host/share/report").unwrap(),
        display_name: "report".into(),
        kind: EntryKind::Directory,
        size: None,
        modified: None,
        content_type: None,
        capabilities: CapabilitySnapshot::default(),
    };
    done(SearchCompletion {
        id: request.id,
        outcome: SearchOutcome::Completed {
            matches: vec![result],
            declared_scope: request.scope,
            scope_limited: true,
            limits: vec![],
        },
    });
    column(view.widget.upcast_ref())
        .unwrap()
        .model()
        .unwrap()
        .downcast::<gtk::MultiSelection>()
        .unwrap()
        .select_item(0, true);
    view.navigate(Location::from_uri("smb://host/missing").unwrap());
    provider.advance(1);
    while context.pending() {
        context.iteration(false);
    }
    let Some(zorin_explorer::ui::context_menu::ContextTarget::File(selected)) =
        view.selected_target()
    else {
        panic!("saved file must remain selected")
    };
    assert_eq!(selected.location().uri(), "smb://host/share/saved");
}

#[gtk::test]
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
        .downcast::<gtk::MultiSelection>()
        .unwrap();
    model.select_item(0, true);
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
        .downcast::<gtk::MultiSelection>()
        .unwrap();
    assert!(model.is_selected(0));
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

    // Tab selection must replace the editable URI even when an overview is visible.
    let header =
        zorin_explorer::ui::header::HeaderControls::new(Rc::new(RefCell::new(root.clone())));
    let tabs = adw::TabView::new();
    let directory_page = tabs.append(&gtk::Label::new(Some("Directory")));
    let overview_page = tabs.append(&gtk::Label::new(Some("Overview")));
    tabs.set_selected_page(&directory_page);
    let overview_content = zorin_explorer::core::navigation::TabContent::SectionOverview {
        section: zorin_explorer::core::sidebar_model::SidebarSection {
            title: "Network & online".into(),
            nodes: vec![],
        },
        underlying: Location::from_uri("smb://other/retained").unwrap(),
    };
    tabs.connect_selected_page_notify({
        let header = header.clone();
        let overview_page = overview_page.clone();
        let root = root.clone();
        move |tabs| {
            let content = if tabs.selected_page().as_ref() == Some(&overview_page) {
                overview_content.clone()
            } else {
                zorin_explorer::core::navigation::TabContent::Directory(root.clone())
            };
            header.set_tab_content(&content);
        }
    });
    tabs.set_selected_page(&overview_page);
    header.focus_location();
    let mut header_child = header.widget.first_child();
    let stack = loop {
        let child = header_child.expect("header contains a location stack");
        if let Some(stack) = child.downcast_ref::<gtk::Stack>() {
            break stack.clone();
        }
        header_child = child.next_sibling();
    };
    let entry = stack
        .child_by_name("entry")
        .unwrap()
        .downcast::<gtk::Entry>()
        .unwrap();
    assert_eq!(entry.text(), "smb://other/retained");
    tabs.set_selected_page(&directory_page);
    tabs.set_selected_page(&overview_page);
    header.focus_location();
    assert_eq!(entry.text(), "smb://other/retained");

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
    // GMount::root() identifies the SMB share, not the server-browsing URI.
    // Real GVfs considers this share a root with no parent.
    let removed_root = Location::from_uri("smb://host/missing").unwrap();
    view.refresh();
    assert!(view.tab_state().borrow().has_pending_navigation());
    view.mark_unavailable(&removed_root);
    assert!(
        matches!(&view.tab_state().borrow().content, zorin_explorer::core::navigation::TabContent::Unavailable(location) if location.uri() == "smb://host/missing")
    );
    assert!(!view.tab_state().borrow().has_pending_navigation());
    // A late completion from the refresh must not resurrect the removed mount.
    provider.advance(1);
    while context.pending() {
        context.iteration(false);
    }
    assert!(
        matches!(&view.tab_state().borrow().content, zorin_explorer::core::navigation::TabContent::Unavailable(location) if location.uri() == "smb://host/missing")
    );
    assert_eq!(view.current_location().uri(), "smb://host/missing");
    assert_eq!(
        view.tab_state().borrow().history.current(),
        Some(&removed_root)
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
    unavailable_local.show_section_overview(zorin_explorer::core::sidebar_model::SidebarSection {
        title: "This PC".into(),
        nodes: vec![],
    });
    assert!(!unavailable_local.command_availability().can_paste);
    let dispatched = Rc::new(std::cell::Cell::new(false));
    unavailable_local.set_context_handler({
        let dispatched = dispatched.clone();
        move |_, _| dispatched.set(true)
    });
    unavailable_local.dispatch_context_command(
        zorin_explorer::ui::context_menu::ContextTarget::Background(root.clone()),
        ContextCommand::Paste,
    );
    assert!(
        !dispatched.get(),
        "overview must not permit Paste into a removed mount"
    );
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

#[gtk::test]
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

#[gtk::test]
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
