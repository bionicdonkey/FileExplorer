use std::rc::Rc;

use gtk::prelude::*;
use zorin_explorer::application::operations::{CancelResult, OperationCoordinator, OperationState};
use zorin_explorer::core::contracts::*;
use zorin_explorer::core::Location;
use zorin_explorer::infrastructure::mock_provider::{MockConfig, MockProvider};
use zorin_explorer::ui::context_menu::{
    configure_command_button, CommandAvailability, ContextCommand,
};

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
