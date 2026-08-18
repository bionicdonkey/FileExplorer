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
    let escaped = term.replace('\\', "\\\\").replace('"', "\\\"");
    format!("SELECT DISTINCT nie:url(?u) WHERE {{ ?u a nfo:FileDataObject ; nfo:fileName ~name . }}",).replace("~name", &format!("\"{escaped}\""))
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
        .args(["sparql", "--dbus-service", "org.freedesktop.Tracker3.Miner.Files", "--query", &tracker_filename_query(query.term())])
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
    fn fallback_search_only_matches_the_requested_folder() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("report.txt"), "content").unwrap();
        assert_eq!(folder_search(&SearchQuery::new("report", false), directory.path()).unwrap(), vec![directory.path().join("report.txt")]);
    }
}
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
