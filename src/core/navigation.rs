use std::path::{Path, PathBuf};

/// Back/forward history for a single Explorer tab.
#[derive(Clone, Debug, Default)]
pub struct NavigationHistory {
    entries: Vec<PathBuf>,
    cursor: Option<usize>,
}

impl NavigationHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, path: PathBuf) {
        if self.current() == Some(path.as_path()) {
            return;
        }

        if let Some(cursor) = self.cursor {
            self.entries.truncate(cursor + 1);
        }
        self.entries.push(path);
        self.cursor = Some(self.entries.len() - 1);
    }

    pub fn back(&mut self) -> Option<&Path> {
        let cursor = self.cursor?;
        if cursor == 0 {
            return None;
        }
        self.cursor = Some(cursor - 1);
        self.current()
    }

    pub fn forward(&mut self) -> Option<&Path> {
        let cursor = self.cursor?;
        if cursor + 1 >= self.entries.len() {
            return None;
        }
        self.cursor = Some(cursor + 1);
        self.current()
    }

    pub fn current(&self) -> Option<&Path> {
        self.cursor
            .and_then(|cursor| self.entries.get(cursor).map(PathBuf::as_path))
    }

    pub fn can_go_back(&self) -> bool {
        self.cursor.is_some_and(|cursor| cursor > 0)
    }

    pub fn can_go_forward(&self) -> bool {
        self.cursor
            .is_some_and(|cursor| cursor + 1 < self.entries.len())
    }
}
