//! Shared, URI-only intent at every interaction boundary. Clipboard state is
//! deliberately transient; neither transport payloads nor cut intent serialize.
use crate::core::{
    contracts::{Capability, CapabilitySnapshot},
    location::LocationError,
    FileItem, Location,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextCommand {
    Open,
    OpenWith,
    Extract,
    CompressZip,
    Compress7z,
    CompressTarGz,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    Cut,
    Copy,
    Paste,
    Trash,
    DeletePermanently,
}
impl From<Shortcut> for ContextCommand {
    fn from(shortcut: Shortcut) -> Self {
        match shortcut {
            Shortcut::Cut => Self::Cut,
            Shortcut::Copy => Self::Copy,
            Shortcut::Paste => Self::Paste,
            Shortcut::Trash => Self::MoveToTrash,
            Shortcut::DeletePermanently => Self::DeletePermanently,
        }
    }
}
pub fn route(command: impl Into<ContextCommand>) -> ContextCommand {
    command.into()
}
use gio::prelude::*;
use std::collections::HashSet;

pub fn named_child(parent: &Location, name: &str) -> std::io::Result<Location> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.chars().any(char::is_control)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Enter a single file name",
        ));
    }
    Location::try_from_file(gio::File::for_uri(parent.uri()).child(name))
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid location"))
}

/// What a transfer to `destination` can do with each source.
///
/// An item moved to the folder it is already in, or anything dropped on itself
/// or inside itself, has nowhere to go and is left out. A copy into the folder
/// it came from stays in: it becomes a duplicate (see `copy_sibling`).
pub fn transfer_sources(
    sources: Vec<Location>,
    destination: &Location,
    action: ClipboardAction,
) -> Vec<Location> {
    sources
        .into_iter()
        .filter(|source| {
            !destination.is_within(source)
                && (action == ClipboardAction::Copy || source.parent().as_ref() != Some(destination))
        })
        .collect()
}

/// A name beside `source` for a duplicate of it: `report (copy).txt`, then
/// `report (copy 2).txt`. Folders and archives keep their whole name, and a
/// duplicate of a duplicate numbers on from the original.
pub fn copy_sibling(source: &Location) -> std::io::Result<Location> {
    let parent = source.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "Nothing to copy beside")
    })?;
    let name = source.display_name();
    let is_dir = gio::File::for_uri(source.uri()).query_file_type(
        gio::FileQueryInfoFlags::NONE,
        gio::Cancellable::NONE,
    ) == gio::FileType::Directory;
    let (stem, extension) = if is_dir {
        (name.as_str(), "")
    } else {
        split_extension(&name)
    };
    let stem = strip_copy_suffix(stem);
    let mut number = 1;
    loop {
        let label = if number == 1 {
            "(copy)".to_owned()
        } else {
            format!("(copy {number})")
        };
        let candidate = named_child(&parent, &format!("{stem} {label}{extension}"))?;
        if !gio::File::for_uri(candidate.uri()).query_exists(gio::Cancellable::NONE) {
            return Ok(candidate);
        }
        number += 1;
    }
}

/// `("report", ".txt")`; a compressed tar keeps both parts as its extension.
fn split_extension(name: &str) -> (&str, &str) {
    let Some(dot) = name.rfind('.').filter(|dot| *dot > 0) else {
        return (name, "");
    };
    let dot = match name[..dot].strip_suffix(".tar") {
        Some(_) => dot - ".tar".len(),
        None => dot,
    };
    (&name[..dot], &name[dot..])
}

fn strip_copy_suffix(stem: &str) -> &str {
    let Some(open) = stem.rfind(" (copy") else {
        return stem;
    };
    let tail = &stem[open + " (copy".len()..];
    let numbered = tail
        .strip_prefix(' ')
        .map_or(false, |n| n.strip_suffix(')').is_some_and(|n| n.parse::<u32>().is_ok()));
    if tail == ")" || numbered {
        &stem[..open]
    } else {
        stem
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardAction {
    Copy,
    Cut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardPayload {
    pub action: ClipboardAction,
    pub locations: Vec<Location>,
}
pub fn parse_uri_list(text: &str) -> Result<Vec<Location>, LocationError> {
    let mut seen = HashSet::new();
    let mut locations = Vec::new();
    for line in text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let location = Location::from_uri(line)?;
        if seen.insert(location.clone()) {
            locations.push(location);
        }
    }
    if locations.is_empty() {
        return Err(LocationError::UnsafeLocation);
    }
    Ok(locations)
}
impl ClipboardPayload {
    pub fn parse_gnome(text: &str) -> Result<Self, LocationError> {
        let (action, uris) = text.split_once('\n').ok_or(LocationError::UnsafeLocation)?;
        let action = match action.trim_end_matches('\r') {
            "copy" => ClipboardAction::Copy,
            "cut" => ClipboardAction::Cut,
            _ => return Err(LocationError::UnsafeLocation),
        };
        Self::parse(action, uris)
    }
    pub fn parse(action: ClipboardAction, text: &str) -> Result<Self, LocationError> {
        Ok(Self {
            action,
            locations: parse_uri_list(text)?,
        })
    }
    pub fn uri_list(&self) -> String {
        self.locations
            .iter()
            .map(|l| format!("{}\r\n", l.uri()))
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ClipboardState {
    payload: Option<ClipboardPayload>,
    pending: Option<u64>,
    generation: u64,
}
impl ClipboardState {
    pub fn payload(&self) -> Option<&ClipboardPayload> {
        self.payload.as_ref()
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn replace(&mut self, payload: ClipboardPayload) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.generation = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.payload = Some(payload);
        self.pending = None;
    }
    pub fn clear(&mut self) {
        self.payload = None;
        self.pending = None;
    }
    pub fn begin_paste(&mut self, id: u64) {
        self.pending = Some(id);
    }
    pub fn finish_paste(&mut self, id: u64, terminal: bool) {
        if terminal && self.pending == Some(id) {
            self.pending = None;
            if self
                .payload
                .as_ref()
                .is_some_and(|p| p.action == ClipboardAction::Cut)
            {
                self.payload = None;
            }
        }
    }
}

/// A one-shot terminal acknowledgement, shared across retries and conflict UI.
/// Losing the command without executing it rejects the transfer safely.
#[derive(Clone)]
pub struct TransferCompletion(std::rc::Rc<CompletionInner>);
struct CompletionInner(std::cell::RefCell<Option<Box<dyn FnOnce(bool)>>>);
impl Drop for CompletionInner {
    fn drop(&mut self) {
        if let Some(done) = self.0.get_mut().take() {
            done(false);
        }
    }
}
impl TransferCompletion {
    pub fn new(done: impl FnOnce(bool) + 'static) -> Self {
        Self(std::rc::Rc::new(CompletionInner(std::cell::RefCell::new(
            Some(Box::new(done)),
        ))))
    }
    pub fn complete(&self, success: bool) {
        let done = self.0 .0.borrow_mut().take();
        if let Some(done) = done {
            done(success);
        }
    }
}
impl std::fmt::Debug for TransferCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TransferCompletion")
    }
}
impl PartialEq for TransferCompletion {
    fn eq(&self, other: &Self) -> bool {
        std::rc::Rc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for TransferCompletion {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationCommand {
    pub kind: ContextCommand,
    pub selection: Vec<Location>,
    pub destination: Location,
    pub transfer: Option<ClipboardAction>,
    pub application: Option<gio::AppInfo>,
    pub completion: Option<TransferCompletion>,
    /// External MOVE protocols ask the source to delete only after we copy
    /// every item successfully and acknowledge the negotiated move.
    pub source_deletes_on_success: bool,
}
impl ApplicationCommand {
    pub fn route(kind: ContextCommand, selection: Vec<Location>, destination: Location) -> Self {
        let mut seen = HashSet::new();
        Self {
            kind,
            selection: selection
                .into_iter()
                .filter(|l| seen.insert(l.clone()))
                .collect(),
            destination,
            transfer: None,
            application: None,
            completion: None,
            source_deletes_on_success: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DragProposal {
    pub sources: Vec<Location>,
    pub destination: Location,
    pub action: ClipboardAction,
}
impl DragProposal {
    pub fn negotiate(
        items: &[FileItem],
        destination: Location,
        capabilities: &CapabilitySnapshot,
        action: ClipboardAction,
    ) -> Result<Self, &'static str> {
        let required = match action {
            ClipboardAction::Copy => Capability::Copy,
            ClipboardAction::Cut => Capability::Move,
        };
        if items.is_empty()
            || !capabilities.supports(Capability::Create)
            || !items.iter().all(|i| i.capabilities().supports(required))
        {
            return Err("This selection or destination does not support the requested transfer");
        }
        Ok(Self {
            sources: items.iter().map(|i| i.location().clone()).collect(),
            destination,
            action,
        })
    }
    pub fn command(self) -> ApplicationCommand {
        let mut command =
            ApplicationCommand::route(ContextCommand::Paste, self.sources, self.destination);
        command.transfer = Some(self.action);
        command
    }
}

#[cfg(test)]
mod transfer_tests {
    use super::{
        copy_sibling, split_extension, strip_copy_suffix, transfer_sources, ClipboardAction,
    };
    use crate::core::Location;

    fn location(uri: &str) -> Location {
        Location::from_uri(uri).unwrap()
    }

    #[test]
    fn a_move_to_the_folder_it_is_in_does_nothing_but_a_copy_duplicates() {
        let folder = location("file:///home/user/Downloads");
        let inside = location("file:///home/user/Downloads/shot.png");
        let elsewhere = location("file:///home/user/Pictures/other.png");

        assert!(transfer_sources(vec![inside.clone()], &folder, ClipboardAction::Cut).is_empty());
        assert_eq!(
            transfer_sources(vec![inside.clone()], &folder, ClipboardAction::Copy),
            vec![inside.clone()]
        );
        assert_eq!(
            transfer_sources(
                vec![inside.clone(), elsewhere.clone()],
                &folder,
                ClipboardAction::Cut
            ),
            vec![elsewhere],
            "only what is not already there still moves"
        );
    }

    #[test]
    fn nothing_is_dropped_on_itself_or_inside_itself() {
        let folder = location("file:///home/user/Downloads");
        let sub = location("file:///home/user/Downloads/sub");
        for action in [ClipboardAction::Copy, ClipboardAction::Cut] {
            assert!(transfer_sources(vec![sub.clone()], &sub, action).is_empty());
            assert!(transfer_sources(
                vec![folder.clone()],
                &location("file:///home/user/Downloads/sub/deeper"),
                action
            )
            .is_empty());
        }
    }

    #[test]
    fn a_duplicate_keeps_the_extension_and_a_compressed_tar_keeps_both() {
        assert_eq!(split_extension("report.txt"), ("report", ".txt"));
        assert_eq!(split_extension("logs.tar.gz"), ("logs", ".tar.gz"));
        assert_eq!(split_extension(".bashrc"), (".bashrc", ""));
        assert_eq!(split_extension("README"), ("README", ""));
        assert_eq!(strip_copy_suffix("report (copy)"), "report");
        assert_eq!(strip_copy_suffix("report (copy 3)"), "report");
        assert_eq!(strip_copy_suffix("report (copying)"), "report (copying)");
    }

    #[test]
    fn duplicates_are_named_beside_the_original_and_number_on() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.txt");
        std::fs::write(&file, "x").unwrap();
        std::fs::create_dir(dir.path().join("v1.2")).unwrap();
        let name = |location: &Location| location.local_path().unwrap();

        let first = copy_sibling(&Location::from_local_path(&file)).unwrap();
        assert_eq!(name(&first), dir.path().join("report (copy).txt"));

        std::fs::write(name(&first), "x").unwrap();
        let second = copy_sibling(&Location::from_local_path(&file)).unwrap();
        assert_eq!(name(&second), dir.path().join("report (copy 2).txt"));

        let of_a_copy = copy_sibling(&first).unwrap();
        assert_eq!(
            name(&of_a_copy),
            dir.path().join("report (copy 2).txt"),
            "a duplicate of a duplicate numbers on from the original"
        );

        let folder = copy_sibling(&Location::from_local_path(&dir.path().join("v1.2"))).unwrap();
        assert_eq!(name(&folder), dir.path().join("v1.2 (copy)"));
    }
}
