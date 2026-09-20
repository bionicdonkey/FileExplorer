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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationCommand {
    pub kind: ContextCommand,
    pub selection: Vec<Location>,
    pub destination: Location,
    pub transfer: Option<ClipboardAction>,
    pub application: Option<gio::AppInfo>,
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
