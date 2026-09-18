use super::{contracts::*, sidebar_model::SidebarSection, FileItem, Location};
use gio::prelude::*;

/// Back/forward history for a single Explorer tab.
#[derive(Clone, Debug, Default)]
pub struct NavigationHistory {
    entries: Vec<Location>,
    cursor: Option<usize>,
}

impl NavigationHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, path: Location) {
        if self.current() == Some(&path) {
            return;
        }

        if let Some(cursor) = self.cursor {
            self.entries.truncate(cursor + 1);
        }
        self.entries.push(path);
        self.cursor = Some(self.entries.len() - 1);
    }

    pub fn back(&mut self) -> Option<&Location> {
        let cursor = self.cursor?;
        if cursor == 0 {
            return None;
        }
        self.cursor = Some(cursor - 1);
        self.current()
    }

    pub fn forward(&mut self) -> Option<&Location> {
        let cursor = self.cursor?;
        if cursor + 1 >= self.entries.len() {
            return None;
        }
        self.cursor = Some(cursor + 1);
        self.current()
    }

    pub fn current(&self) -> Option<&Location> {
        self.cursor.and_then(|cursor| self.entries.get(cursor))
    }

    pub fn back_target(&self) -> Option<&Location> {
        self.cursor
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| self.entries.get(i))
    }

    pub fn forward_target(&self) -> Option<&Location> {
        self.cursor.and_then(|i| self.entries.get(i + 1))
    }

    pub fn can_go_back(&self) -> bool {
        self.cursor.is_some_and(|cursor| cursor > 0)
    }

    pub fn can_go_forward(&self) -> bool {
        self.cursor
            .is_some_and(|cursor| cursor + 1 < self.entries.len())
    }
}

#[derive(Clone, Debug)]
pub enum TabContent {
    Directory(Location),
    SectionOverview {
        section: SidebarSection,
        underlying: Location,
    },
    Search {
        underlying: Location,
        items: Vec<FileItem>,
    },
    Unavailable(Location),
}

#[derive(Clone, Debug)]
pub enum NavigationStatus {
    Idle,
    Loading(Location),
    Ready,
    Failed {
        location: Location,
        error: OperationError,
    },
    Closed,
}

#[derive(Clone, Copy, Debug)]
enum HistoryCommit {
    Push,
    Replace,
    Back,
    Forward,
}

#[derive(Clone, Debug)]
pub struct NavigationRequest {
    pub generation: u64,
    pub request: DirectoryRequest,
    commit: HistoryCommit,
}

/// Main-context tab state. Pending requests never replace the successful view.
#[derive(Clone, Debug)]
pub struct TabState {
    pub location: Location,
    pub history: NavigationHistory,
    pub content: TabContent,
    pub navigation_generation: u64,
    pub selection_keys: Vec<String>,
    pub directory: Option<DirectorySnapshot>,
    pub status: NavigationStatus,
    pending: Option<NavigationRequest>,
    retry: Option<(Location, HistoryCommit)>,
}

impl TabState {
    pub fn new(location: Location) -> Self {
        Self {
            content: TabContent::Unavailable(location.clone()),
            location,
            history: NavigationHistory::new(),
            navigation_generation: 0,
            selection_keys: vec![],
            directory: None,
            status: NavigationStatus::Idle,
            pending: None,
            retry: None,
        }
    }
    pub fn begin_navigation(&mut self, location: Location) -> Option<NavigationRequest> {
        self.begin(location, HistoryCommit::Push)
    }
    pub fn begin_retry(&mut self) -> Option<NavigationRequest> {
        let (location, commit) = self.retry.clone()?;
        self.begin(location, commit)
    }
    pub fn begin_back(&mut self) -> Option<NavigationRequest> {
        self.begin(self.history.back_target()?.clone(), HistoryCommit::Back)
    }
    pub fn begin_forward(&mut self) -> Option<NavigationRequest> {
        self.begin(
            self.history.forward_target()?.clone(),
            HistoryCommit::Forward,
        )
    }
    pub fn begin_refresh(&mut self) -> Option<NavigationRequest> {
        let (location, commit) = self
            .pending
            .as_ref()
            .map(|p| (p.request.location.clone(), p.commit))
            .unwrap_or_else(|| {
                (
                    self.location.clone(),
                    if self.history.current().is_some() {
                        HistoryCommit::Replace
                    } else {
                        HistoryCommit::Push
                    },
                )
            });
        self.begin(location, commit)
    }
    fn begin(&mut self, location: Location, commit: HistoryCommit) -> Option<NavigationRequest> {
        if matches!(self.status, NavigationStatus::Closed) {
            return None;
        }
        self.cancel_pending();
        self.navigation_generation = self
            .navigation_generation
            .checked_add(1)
            .expect("navigation generation exhausted");
        let request = NavigationRequest {
            generation: self.navigation_generation,
            request: DirectoryRequest {
                id: RequestId::allocate(),
                location: location.clone(),
                page: PageRequest {
                    offset: 0,
                    limit: usize::MAX,
                },
                cancellable: gio::Cancellable::new(),
            },
            commit,
        };
        self.status = NavigationStatus::Loading(location);
        self.retry = None;
        self.pending = Some(request.clone());
        Some(request)
    }
    pub fn apply_directory_result(
        &mut self,
        generation: u64,
        result: RequestCompletion<DirectorySnapshot>,
    ) -> bool {
        let Some(pending) = self.pending.as_ref() else {
            return false;
        };
        if generation != self.navigation_generation || result.id != pending.request.id {
            return false;
        }
        let pending = self.pending.take().unwrap();
        let outcome = match result.outcome {
            RequestOutcome::Succeeded(snapshot)
                if snapshot.location.requested != pending.request.location
                    || snapshot.next_page.is_some() =>
            {
                RequestOutcome::Failed(OperationError::new(
                    OperationErrorKind::InvalidRequest,
                    Some(&pending.request.location),
                    None,
                ))
            }
            outcome => outcome,
        };
        match outcome {
            RequestOutcome::Succeeded(snapshot) => {
                let location = snapshot.location.resolved.clone();
                match pending.commit {
                    HistoryCommit::Push => self.history.push(location.clone()),
                    HistoryCommit::Back => {
                        self.history.back();
                    }
                    HistoryCommit::Forward => {
                        self.history.forward();
                    }
                    HistoryCommit::Replace => {}
                }
                if let Some(cursor) = self.history.cursor {
                    self.history.entries[cursor] = location.clone();
                }
                if self.location != location {
                    self.selection_keys.clear();
                }
                self.selection_keys.retain(|key| {
                    snapshot
                        .entries
                        .iter()
                        .any(|entry| entry.location.uri() == key)
                });
                self.content = TabContent::Directory(location.clone());
                self.location = location;
                self.directory = Some(snapshot);
                self.status = NavigationStatus::Ready;
            }
            RequestOutcome::Failed(error) => {
                self.retry = Some((pending.request.location.clone(), pending.commit));
                self.status = NavigationStatus::Failed {
                    location: pending.request.location,
                    error,
                };
            }
            RequestOutcome::Cancelled => {
                self.status = if self.directory.is_some() {
                    NavigationStatus::Ready
                } else {
                    NavigationStatus::Idle
                }
            }
        }
        true
    }
    pub fn cancel_pending(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.request.cancellable.cancel();
        }
    }
    pub fn close(&mut self) {
        self.cancel_pending();
        self.retry = None;
        self.status = NavigationStatus::Closed;
    }
    pub fn show_section(&mut self, section: SidebarSection) {
        self.cancel_pending();
        self.status = if self.directory.is_some() {
            NavigationStatus::Ready
        } else {
            NavigationStatus::Idle
        };
        self.content = TabContent::SectionOverview {
            section,
            underlying: self.location.clone(),
        };
    }
    pub fn restore_directory(&mut self) {
        self.content = TabContent::Directory(self.location.clone());
    }
}
