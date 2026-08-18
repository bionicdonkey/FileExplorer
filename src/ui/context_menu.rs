use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextTarget {
    File { path: PathBuf, content_type: String },
    Folder(PathBuf),
    Background(PathBuf),
    Tab,
    SidebarLocation(PathBuf),
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
}

pub fn delete_command(shift_held: bool) -> ContextCommand {
    if shift_held { ContextCommand::DeletePermanently } else { ContextCommand::MoveToTrash }
}

pub fn delete_label(shift_held: bool) -> &'static str {
    if shift_held { "Delete Permanently…" } else { "Move to Trash" }
}

pub fn item_commands(target: &ContextTarget) -> Vec<ContextCommand> {
    match target {
        ContextTarget::File { .. } => vec![
            ContextCommand::Open, ContextCommand::OpenWith, ContextCommand::Cut,
            ContextCommand::Copy, ContextCommand::CopyFilename, ContextCommand::CopyFullPath,
            ContextCommand::CopyTo, ContextCommand::MoveTo, ContextCommand::Rename, ContextCommand::MoveToTrash,
            ContextCommand::Properties,
        ],
        ContextTarget::Folder(_) | ContextTarget::SidebarLocation(_) => vec![
            ContextCommand::Open, ContextCommand::OpenInNewTab, ContextCommand::OpenInNewWindow,
            ContextCommand::OpenInTerminal, ContextCommand::Cut, ContextCommand::Copy,
            ContextCommand::CopyFilename, ContextCommand::CopyFullPath, ContextCommand::CopyTo,
            ContextCommand::MoveTo, ContextCommand::Rename, ContextCommand::MoveToTrash,
            ContextCommand::Properties,
        ],
        ContextTarget::Background(_) => vec![
            ContextCommand::NewFolder, ContextCommand::Paste, ContextCommand::Refresh,
        ],
        ContextTarget::Tab => vec![
            ContextCommand::CloseTab, ContextCommand::CloseOtherTabs, ContextCommand::DuplicateTab,
        ],
    }
}

pub fn open_with_apps(content_type: &str) -> Vec<gio::AppInfo> {
    gio::AppInfo::recommended_for_type(content_type)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{item_commands, ContextCommand, ContextTarget};

    #[test]
    fn folder_commands_include_terminal_and_navigation_actions() {
        let commands = item_commands(&ContextTarget::Folder(PathBuf::from("/tmp")));

        assert!(commands.contains(&ContextCommand::OpenInTerminal));
        assert!(commands.contains(&ContextCommand::OpenInNewTab));
        assert!(commands.contains(&ContextCommand::OpenInNewWindow));
    }

    #[test]
    fn item_commands_include_copy_and_move_destinations() {
        let commands = item_commands(&ContextTarget::File {
            path: PathBuf::from("/tmp/report.txt"),
            content_type: "text/plain".into(),
        });

        assert!(commands.contains(&ContextCommand::CopyFilename));
        assert!(commands.contains(&ContextCommand::CopyFullPath));
        assert!(commands.contains(&ContextCommand::CopyTo));
        assert!(commands.contains(&ContextCommand::MoveTo));
    }

    #[test]
    fn shift_changes_the_delete_action_label() {
        assert_eq!(super::delete_label(false), "Move to Trash");
        assert_eq!(super::delete_label(true), "Delete Permanently…");
        assert_eq!(super::delete_command(true), ContextCommand::DeletePermanently);
    }
}
