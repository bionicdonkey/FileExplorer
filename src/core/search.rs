#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHistoryEntry {
    pub term: String,
    pub recorded_at: i64,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredSearchHistory {
    #[serde(default)]
    entries: Vec<SearchHistoryEntry>,
}

pub struct SearchHistory {
    path: Option<PathBuf>,
    entries: Vec<SearchHistoryEntry>,
}

impl SearchHistory {
    pub fn in_memory() -> Self {
        Self {
            path: None,
            entries: Vec::new(),
        }
    }

    pub fn load_from(path: PathBuf) -> io::Result<Self> {
        if !path.exists() {
            return Ok(Self {
                path: Some(path),
                entries: Vec::new(),
            });
        }
        let stored: StoredSearchHistory = serde_json::from_slice(&fs::read(&path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(Self {
            path: Some(path),
            entries: stored.entries,
        })
    }

    pub fn entries(&self) -> &[SearchHistoryEntry] {
        &self.entries
    }

    pub fn record(&mut self, term: &str) {
        let term = term.trim();
        if term.is_empty() {
            return;
        }
        self.entries.retain(|entry| entry.term != term);
        self.entries.push(SearchHistoryEntry {
            term: term.to_owned(),
            recorded_at: now(),
        });
    }

    pub fn prune(&mut self, retention_days: u32) {
        let cutoff = now() - retention_days as i64 * 86_400;
        self.entries.retain(|entry| entry.recorded_at >= cutoff);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Shared by startup and preferences so disabled recording also removes
    /// terms left by a previous enabled session.
    pub fn enforce_policy(&mut self, enabled: bool, retention_days: u32) -> io::Result<()> {
        if enabled {
            self.prune(retention_days);
        } else {
            self.clear();
        }
        self.save()
    }

    pub fn record_submitted(
        &mut self,
        term: &str,
        enabled: bool,
        retention_days: u32,
    ) -> io::Result<()> {
        if enabled {
            self.record(term);
        }
        self.enforce_policy(enabled, retention_days)
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(&StoredSearchHistory {
            entries: self.entries.clone(),
        })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        fs::write(path, bytes)
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_a_term_twice_keeps_only_the_latest_entry() {
        let mut history = SearchHistory::in_memory();
        history.record("report");
        history.record("report");

        assert_eq!(history.entries().len(), 1);
    }

    #[test]
    fn pruning_removes_entries_older_than_the_retention_window() {
        let mut history = SearchHistory::in_memory();
        history.record("report");
        history.entries.get_mut(0).unwrap().recorded_at = now() - 40 * 86_400;

        history.prune(30);

        assert!(history.entries().is_empty());
    }

    #[test]
    fn clearing_history_removes_every_recorded_term() {
        let mut history = SearchHistory::in_memory();
        history.record("report");
        history.clear();

        assert!(history.entries().is_empty());
    }

    #[test]
    fn history_round_trips_through_disk() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("search-history.json");
        let mut history = SearchHistory::load_from(path.clone()).unwrap();
        history.record("invoice");
        history.save().unwrap();

        let restored = SearchHistory::load_from(path).unwrap();
        assert_eq!(
            restored
                .entries()
                .iter()
                .map(|entry| entry.term.as_str())
                .collect::<Vec<_>>(),
            ["invoice"]
        );
    }
}
use std::fs;
use std::io;
use std::path::PathBuf;

use super::{contracts::*, FileItem, Location};
use serde::{Deserialize, Serialize};
use std::time::Duration;

impl SearchScope {
    pub fn local_fallback(root: Location) -> Self {
        Self {
            root,
            max_depth: 1,
            max_results: 5_000,
            timeout: Duration::from_secs(2),
        }
    }

    pub fn annotation(&self) -> String {
        format!(
            "{}; depth {} (root is 0); up to {} matches; up to {} seconds",
            self.root.uri(),
            self.max_depth,
            self.max_results,
            self.timeout.as_secs_f64()
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchResult {
    pub item: FileItem,
    pub location: Location,
}

impl From<&EntrySnapshot> for SearchResult {
    fn from(entry: &EntrySnapshot) -> Self {
        Self {
            item: FileItem::from_snapshot(entry),
            location: entry.location.clone(),
        }
    }
}

impl SearchOutcome {
    pub fn status_text(&self) -> String {
        match self {
            Self::Completed { matches, declared_scope, .. } => format!("Found {} items. Searched {}", matches.len(), declared_scope.annotation()),
            Self::Incomplete { matches, declared_scope, reason, .. } => {
                let reason = match reason {
                    SearchIncompleteReason::Cancelled => "cancelled",
                    SearchIncompleteReason::ProviderFailure => "provider failed",
                    SearchIncompleteReason::MalformedPage => "invalid provider response",
                    SearchIncompleteReason::AccessLost => "access or mount lost",
                };
                format!("Search incomplete ({reason}); retained {} items. {}", matches.len(), declared_scope.annotation())
            }
            Self::Unsupported { declared_scope, .. } => format!("Search unavailable for {}: the provider does not support this search mode and scope", declared_scope.root.redacted()),
        }
    }
}
