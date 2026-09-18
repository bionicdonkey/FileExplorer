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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextCommand {
    Open,
    OpenWith,
    OpenInNewTab,
    OpenInNewWindow,
    OpenInTerminal,
    NewFolder,
    Cut,
    Copy,
    CopyFilename,
    CopyFullPath,
    CopyTo,
    MoveTo,
    Paste,
    Rename,
    MoveToTrash,
    DeletePermanently,
    Properties,
    Refresh,
    CloseTab,
    CloseOtherTabs,
    DuplicateTab,
    ToggleQuickAccess,
}

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
    pub disabled_reason: Option<&'static str>,
}

impl CommandAvailability {
    pub fn for_selection(selection: &[FileItem], destination: Option<&Location>) -> Self {
        let supports_all = |capability| {
            !selection.is_empty()
                && selection
                    .iter()
                    .all(|item| item.capabilities().supports(capability))
        };
        let can_paste = destination.is_some_and(|location| location.uri().starts_with("file:"));
        let can_open = supports_all(Capability::Open);
        let can_use_local_adapter = selection.len() == 1 && selection[0].local_path().is_some();
        let can_copy = supports_all(Capability::Copy);
        let can_cut = supports_all(Capability::Move);
        let can_rename = selection.len() == 1 && supports_all(Capability::Rename);
        let can_open_terminal = can_use_local_adapter && selection[0].is_dir() && can_open;
        let can_trash = supports_all(Capability::Trash);
        let can_delete = supports_all(Capability::Delete);
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
            disabled_reason,
        }
    }

    pub fn allows(&self, command: ContextCommand) -> bool {
        match command {
            ContextCommand::Open | ContextCommand::OpenWith => self.can_open,
            ContextCommand::OpenInNewTab
            | ContextCommand::OpenInNewWindow
            | ContextCommand::Properties => self.can_open && self.can_use_local_adapter,
            ContextCommand::OpenInTerminal => self.can_open_terminal,
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
    if let Some(reason) = availability.disabled_reason_for(command) {
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
        ContextTarget::File(_) => vec![
            ContextCommand::Open,
            ContextCommand::OpenWith,
            ContextCommand::Cut,
            ContextCommand::Copy,
            ContextCommand::CopyFilename,
            ContextCommand::CopyFullPath,
            ContextCommand::CopyTo,
            ContextCommand::MoveTo,
            ContextCommand::Rename,
            ContextCommand::MoveToTrash,
            ContextCommand::Properties,
        ],
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
