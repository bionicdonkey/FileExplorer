#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    term: String,
    content: bool,
}

impl SearchQuery {
    pub fn new(term: impl Into<String>, content: bool) -> Self {
        Self { term: term.into(), content }
    }

    pub fn includes_content(&self) -> bool { self.content }
    pub fn term(&self) -> &str { &self.term }
}

pub fn tracker_filename_query(term: &str) -> String {
    let escaped = escape_sparql_literal(term);
    format!("SELECT DISTINCT nie:url(?u) WHERE {{ ?u a nfo:FileDataObject ; nfo:fileName ~name . }}",).replace("~name", &format!("\"{escaped}\""))
}

/// Reuses Tracker's existing full-text index rather than scanning file
/// contents directly, so this stays opt-in and never builds a second index.
pub fn tracker_content_query(term: &str) -> String {
    let escaped = escape_sparql_literal(term);
    format!("SELECT DISTINCT nie:url(?u) WHERE {{ ?u a nfo:FileDataObject ; fts:match \"{escaped}\" . }}")
}

fn escape_sparql_literal(term: &str) -> String {
    term.replace('\\', "\\\\").replace('"', "\\\"")
}

fn tracker_query_for(query: &SearchQuery) -> String {
    if query.includes_content() { tracker_content_query(query.term()) } else { tracker_filename_query(query.term()) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchBackend { Tracker, FolderFallback }

pub struct SearchService;

impl SearchService {
    pub fn backend() -> SearchBackend {
        Command::new("tracker3").arg("--version").output().map(|output| output.status.success()).unwrap_or(false)
            .then_some(SearchBackend::Tracker).unwrap_or(SearchBackend::FolderFallback)
    }

    pub fn search(query: &SearchQuery, folder: &Path) -> io::Result<(SearchBackend, Vec<PathBuf>)> {
        if Self::backend() == SearchBackend::Tracker {
            if let Ok(results) = tracker_search(query) { return Ok((SearchBackend::Tracker, results)); }
        }
        Ok((SearchBackend::FolderFallback, folder_search(query, folder)?))
    }
}

fn tracker_search(query: &SearchQuery) -> io::Result<Vec<PathBuf>> {
    let output = Command::new("tracker3")
        .args(["sparql", "--dbus-service", "org.freedesktop.Tracker3.Miner.Files", "--query", &tracker_query_for(query)])
        .output()?;
    if !output.status.success() { return Err(io::Error::new(io::ErrorKind::Other, String::from_utf8_lossy(&output.stderr))); }
    Ok(String::from_utf8_lossy(&output.stdout).lines()
        .map(str::trim).map(|line| line.trim_matches('"'))
        .filter_map(|uri| uri.strip_prefix("file://"))
        .map(PathBuf::from).collect())
}

fn folder_search(query: &SearchQuery, folder: &Path) -> io::Result<Vec<PathBuf>> {
    let needle = query.term().to_lowercase();
    let results = fs::read_dir(folder)?.filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().to_lowercase().contains(&needle))
        .map(|entry| entry.path()).collect::<Vec<_>>();
    Ok(results)
}

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
    pub fn in_memory() -> Self { Self { path: None, entries: Vec::new() } }

    pub fn load_from(path: PathBuf) -> io::Result<Self> {
        if !path.exists() { return Ok(Self { path: Some(path), entries: Vec::new() }); }
        let stored: StoredSearchHistory = serde_json::from_slice(&fs::read(&path)?).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(Self { path: Some(path), entries: stored.entries })
    }

    pub fn entries(&self) -> &[SearchHistoryEntry] { &self.entries }

    pub fn record(&mut self, term: &str) {
        let term = term.trim();
        if term.is_empty() { return; }
        self.entries.retain(|entry| entry.term != term);
        self.entries.push(SearchHistoryEntry { term: term.to_owned(), recorded_at: now() });
    }

    pub fn prune(&mut self, retention_days: u32) {
        let cutoff = now() - retention_days as i64 * 86_400;
        self.entries.retain(|entry| entry.recorded_at >= cutoff);
    }

    pub fn clear(&mut self) { self.entries.clear(); }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else { return Ok(()); };
        if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
        let bytes = serde_json::to_vec_pretty(&StoredSearchHistory { entries: self.entries.clone() }).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        fs::write(path, bytes)
    }
}

fn now() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_query_escapes_user_search_terms() {
        let query = tracker_filename_query("report \"draft\"");
        assert!(query.contains("report \\\"draft\\\""));
    }

    #[test]
    fn content_search_is_opt_in() {
        assert!(!SearchQuery::new("report", false).includes_content());
        assert!(SearchQuery::new("report", true).includes_content());
    }

    #[test]
    fn opting_into_content_search_queries_full_text_instead_of_just_the_filename() {
        let name_only = tracker_query_for(&SearchQuery::new("report", false));
        let with_content = tracker_query_for(&SearchQuery::new("report", true));

        assert!(name_only.contains("nfo:fileName"));
        assert!(!name_only.contains("fts:match"));
        assert!(with_content.contains("fts:match"));
    }

    #[test]
    fn fallback_search_only_matches_the_requested_folder() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("report.txt"), "content").unwrap();
        assert_eq!(folder_search(&SearchQuery::new("report", false), directory.path()).unwrap(), vec![directory.path().join("report.txt")]);
    }

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
        assert_eq!(restored.entries().iter().map(|entry| entry.term.as_str()).collect::<Vec<_>>(), ["invoice"]);
    }
}
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
