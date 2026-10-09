use crate::core::{contracts::Capability, FileItem, Location};
use gtk::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextTarget {
    File(FileItem),
    Folder(FileItem),
    Background(Location),
    Tab,
    SidebarLocation(Location),
}

pub use crate::application::commands::ContextCommand;

/// One application-level answer for every command entry point.  Capability
/// absence is deliberately treated as unavailable: UI metadata can be
/// incomplete, but an optimistic action would be unsafe for remote providers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandAvailability {
    pub can_open: bool,
    pub can_copy: bool,
    pub can_cut: bool,
    pub can_paste: bool,
    pub can_rename: bool,
    pub can_use_local_adapter: bool,
    pub can_open_terminal: bool,
    pub can_trash: bool,
    pub can_delete: bool,
    pub can_extract: bool,
    pub disabled_reason: Option<&'static str>,
}

impl CommandAvailability {
    pub fn for_selection(selection: &[FileItem], destination: Option<&Location>) -> Self {
        let destination = destination.filter(|l| l.local_path().is_some()).map(|_| {
            crate::core::contracts::CapabilitySnapshot {
                supported: [Capability::Create].into_iter().collect(),
                ..Default::default()
            }
        });
        Self::for_capabilities(selection, destination.as_ref())
    }

    pub fn for_capabilities(
        selection: &[FileItem],
        destination: Option<&crate::core::contracts::CapabilitySnapshot>,
    ) -> Self {
        let supports_all = |capability| {
            !selection.is_empty()
                && selection
                    .iter()
                    .all(|item| item.capabilities().supports(capability))
        };
        let can_paste = destination.is_some_and(|snapshot| snapshot.supports(Capability::Create));
        let can_open = supports_all(Capability::Open);
        let can_use_local_adapter = selection.len() == 1 && selection[0].local_path().is_some();
        // URI operations share the same executor. Unknown or absent provider
        // capabilities remain unavailable for the whole selection.
        let can_copy = supports_all(Capability::Copy);
        let can_cut = supports_all(Capability::Move);
        let can_rename = selection.len() == 1 && supports_all(Capability::Rename);
        let can_open_terminal = can_use_local_adapter && selection[0].is_dir() && can_open;
        let can_trash = supports_all(Capability::Trash);
        let can_delete = supports_all(Capability::Delete);
        let can_extract = !selection.is_empty()
            && selection
                .iter()
                .all(|item| crate::core::activation::is_extractable(item));
        let disabled_reason = if !can_trash && !selection.is_empty() {
            Some("This location does not support Trash")
        } else if !can_delete && !selection.is_empty() {
            Some("This location does not support permanent deletion")
        } else if !can_paste && destination.is_some() {
            Some("This location does not support Paste")
        } else {
            None
        };
        Self {
            can_open,
            can_copy,
            can_cut,
            can_paste,
            can_rename,
            can_use_local_adapter,
            can_open_terminal,
            can_trash,
            can_delete,
            can_extract,
            disabled_reason,
        }
    }

    pub fn allows(&self, command: ContextCommand) -> bool {
        match command {
            ContextCommand::Open | ContextCommand::OpenWith => self.can_open,
            ContextCommand::OpenInNewTab
            | ContextCommand::OpenInNewWindow
            | ContextCommand::Properties => self.can_open,
            ContextCommand::OpenInTerminal => self.can_open_terminal,
            ContextCommand::Extract => self.can_extract,
            ContextCommand::Cut | ContextCommand::MoveTo => self.can_cut,
            ContextCommand::Copy
            | ContextCommand::CopyFilename
            | ContextCommand::CopyFullPath
            | ContextCommand::CopyTo => self.can_copy,
            ContextCommand::Paste | ContextCommand::NewFolder => self.can_paste,
            ContextCommand::Rename => self.can_rename,
            ContextCommand::MoveToTrash => self.can_trash,
            ContextCommand::DeletePermanently => self.can_delete,
            ContextCommand::Refresh
            | ContextCommand::CloseTab
            | ContextCommand::CloseOtherTabs
            | ContextCommand::DuplicateTab
            | ContextCommand::ToggleQuickAccess => true,
        }
    }

    pub fn disabled_reason_for(&self, command: ContextCommand) -> Option<&'static str> {
        if self.allows(command) {
            return None;
        }
        match command {
            ContextCommand::MoveToTrash => Some("This location does not support Trash"),
            ContextCommand::DeletePermanently => {
                Some("This location does not support permanent deletion")
            }
            ContextCommand::Paste | ContextCommand::NewFolder => {
                Some("This location does not support Paste")
            }
            ContextCommand::OpenInTerminal => {
                Some("Open in Terminal is available for local folders only")
            }
            _ => Some("This location does not support this action"),
        }
    }

    pub fn for_target(target: &ContextTarget) -> Self {
        match target {
            ContextTarget::File(item) | ContextTarget::Folder(item) => {
                Self::for_selection(std::slice::from_ref(item), None)
            }
            ContextTarget::Background(location) | ContextTarget::SidebarLocation(location) => {
                Self::for_selection(&[], Some(location))
            }
            ContextTarget::Tab => Self {
                can_open: true,
                can_copy: true,
                can_cut: true,
                can_paste: true,
                can_rename: true,
                can_use_local_adapter: true,
                can_open_terminal: true,
                can_trash: true,
                can_delete: true,
                can_extract: true,
                disabled_reason: None,
            },
        }
    }
}

/// Applies the query result to a visible command control, including the
/// screen-reader description that explains why an unavailable action is off.
pub fn configure_command_button(
    button: &gtk::Button,
    availability: &CommandAvailability,
    command: ContextCommand,
) {
    let enabled = availability.allows(command);
    button.set_sensitive(enabled);
    if enabled {
        button.set_tooltip_text(None);
        button.reset_property(gtk::AccessibleProperty::Description);
    } else if let Some(reason) = availability.disabled_reason_for(command) {
        button.set_tooltip_text(Some(reason));
        button.update_property(&[gtk::accessible::Property::Description(reason)]);
    }
}

pub fn delete_command(shift_held: bool) -> ContextCommand {
    if shift_held {
        ContextCommand::DeletePermanently
    } else {
        ContextCommand::MoveToTrash
    }
}

pub fn delete_label(shift_held: bool) -> &'static str {
    if shift_held {
        "Delete Permanently…"
    } else {
        "Move to Trash"
    }
}

pub fn item_commands(target: &ContextTarget) -> Vec<ContextCommand> {
    match target {
        ContextTarget::File(item) => {
            let mut commands = vec![ContextCommand::Open, ContextCommand::OpenWith];
            if crate::core::activation::is_extractable(item) {
                commands.push(ContextCommand::Extract);
            }
            commands.extend([
                ContextCommand::Cut,
                ContextCommand::Copy,
                ContextCommand::CopyFilename,
                ContextCommand::CopyFullPath,
                ContextCommand::CopyTo,
                ContextCommand::MoveTo,
                ContextCommand::Rename,
                ContextCommand::MoveToTrash,
                ContextCommand::Properties,
            ]);
            commands
        }
        ContextTarget::Folder(_) | ContextTarget::SidebarLocation(_) => vec![
            ContextCommand::Open,
            ContextCommand::OpenInNewTab,
            ContextCommand::OpenInNewWindow,
            ContextCommand::OpenInTerminal,
            ContextCommand::Cut,
            ContextCommand::Copy,
            ContextCommand::CopyFilename,
            ContextCommand::CopyFullPath,
            ContextCommand::CopyTo,
            ContextCommand::MoveTo,
            ContextCommand::Rename,
            ContextCommand::MoveToTrash,
            ContextCommand::Properties,
            ContextCommand::ToggleQuickAccess,
        ],
        ContextTarget::Background(_) => vec![
            ContextCommand::NewFolder,
            ContextCommand::Paste,
            ContextCommand::Refresh,
            ContextCommand::ToggleQuickAccess,
        ],
        ContextTarget::Tab => vec![
            ContextCommand::CloseTab,
            ContextCommand::CloseOtherTabs,
            ContextCommand::DuplicateTab,
        ],
    }
}

pub fn open_with_apps(content_type: &str) -> Vec<gio::AppInfo> {
    gio::AppInfo::recommended_for_type(content_type)
}

#[cfg(test)]
mod tests {
    use super::{item_commands, ContextCommand, ContextTarget};
    use crate::core::FileItem;

    #[test]
    fn folder_commands_include_terminal_and_navigation_actions() {
        let item = FileItem::from_path(std::path::Path::new("/tmp")).unwrap();
        let commands = item_commands(&ContextTarget::Folder(item));

        assert!(commands.contains(&ContextCommand::OpenInTerminal));
        assert!(commands.contains(&ContextCommand::OpenInNewTab));
        assert!(commands.contains(&ContextCommand::OpenInNewWindow));
        assert!(commands.contains(&ContextCommand::ToggleQuickAccess));
    }

    #[test]
    fn current_folder_menu_can_toggle_quick_access() {
        let commands = item_commands(&ContextTarget::Background(
            crate::core::Location::from_local_path(std::path::Path::new("/tmp")),
        ));

        assert!(commands.contains(&ContextCommand::ToggleQuickAccess));
    }

    fn file(name: &str) -> FileItem {
        use crate::core::contracts::{CapabilitySnapshot, EntryKind, EntrySnapshot};
        use crate::core::Location;
        FileItem::from_snapshot(&EntrySnapshot {
            location: Location::from_uri(&format!("file:///tmp/{name}")).unwrap(),
            display_name: name.to_owned(),
            kind: EntryKind::File,
            size: None,
            modified: None,
            // No content type yet: recognition must come from the name, the
            // same situation a row opened straight after appearing is in.
            content_type: None,
            capabilities: CapabilitySnapshot::default(),
        })
    }

    #[test]
    fn an_archive_offers_extract_and_an_ordinary_file_does_not() {
        assert!(
            item_commands(&ContextTarget::File(file("Trip.zip")))
                .contains(&ContextCommand::Extract),
            "an archive's menu must offer Extract"
        );
        assert!(
            !item_commands(&ContextTarget::File(file("notes.txt")))
                .contains(&ContextCommand::Extract),
            "a plain file has nothing to extract"
        );
    }

    #[test]
    fn extract_is_available_only_for_a_selection_of_archives() {
        use super::CommandAvailability;
        assert!(CommandAvailability::for_selection(&[file("Trip.zip")], None).can_extract);
        let mixed = vec![file("Trip.zip"), file("notes.txt")];
        assert!(
            !CommandAvailability::for_selection(&mixed, None).can_extract,
            "a mixed selection extracts nothing as a whole"
        );
        assert!(!CommandAvailability::for_selection(&[], None).can_extract);
    }

    #[test]
    fn item_commands_include_copy_and_move_destinations() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.txt");
        std::fs::write(&path, "report").unwrap();
        let commands = item_commands(&ContextTarget::File(FileItem::from_path(&path).unwrap()));

        assert!(commands.contains(&ContextCommand::CopyFilename));
        assert!(commands.contains(&ContextCommand::CopyFullPath));
        assert!(commands.contains(&ContextCommand::CopyTo));
        assert!(commands.contains(&ContextCommand::MoveTo));
    }

    #[test]
    fn shift_changes_the_delete_action_label() {
        assert_eq!(super::delete_label(false), "Move to Trash");
        assert_eq!(super::delete_label(true), "Delete Permanently…");
        assert_eq!(
            super::delete_command(true),
            ContextCommand::DeletePermanently
        );
    }
}
