use std::{env, fs, io};
use std::path::PathBuf;

use gio::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictDefault {
    #[default]
    Ask,
    Replace,
    KeepBoth,
    Skip,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplicationPreferences {
    pub operation_history_days: u32,
    #[serde(default)]
    pub close_on_last_tab: bool,
    #[serde(default)]
    pub restore_previous_tabs: bool,
    #[serde(default = "default_new_tab_location")]
    pub new_tab_location: String,
    #[serde(default)]
    pub conflict_default: ConflictDefault,
    #[serde(default)]
    pub content_search_enabled: bool,
    #[serde(default = "default_record_search_history")]
    pub record_search_history: bool,
    #[serde(default = "default_search_history_retention_days")]
    pub search_history_retention_days: u32,
    #[serde(skip)]
    path: Option<PathBuf>,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredSession {
    locations: Vec<String>,
}

impl Default for ApplicationPreferences {
    fn default() -> Self {
        Self {
            operation_history_days: 7,
            close_on_last_tab: false,
            restore_previous_tabs: false,
            new_tab_location: default_new_tab_location(),
            conflict_default: ConflictDefault::Ask,
            content_search_enabled: false,
            record_search_history: default_record_search_history(),
            search_history_retention_days: default_search_history_retention_days(),
            path: None,
        }
    }
}

impl ApplicationPreferences {
    pub fn load_default() -> io::Result<Self> {
        let path = config_home()?.join("zorin-explorer/preferences.json");
        if !path.exists() {
            return Ok(Self { path: Some(path), ..Self::default() });
        }
        let mut value: Self = serde_json::from_slice(&fs::read(&path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        value.path = Some(path);
        Ok(value)
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else { return Ok(()); };
        fs::create_dir_all(path.parent().expect("preferences path has a parent"))?;
        fs::write(path, serde_json::to_vec_pretty(self).expect("preferences serialize"))
    }

    pub fn history_path(&self) -> Option<PathBuf> {
        self.path.as_ref().and_then(|path| path.parent().map(|parent| parent.join("operation-history.json")))
    }

    pub fn session_path(&self) -> Option<PathBuf> {
        self.path.as_ref().and_then(|path| path.parent().map(|parent| parent.join("session.json")))
    }

    pub fn search_history_path(&self) -> Option<PathBuf> {
        self.path.as_ref().and_then(|path| path.parent().map(|parent| parent.join("search-history.json")))
    }

    pub fn save_session(&self, locations: &[String]) -> io::Result<()> {
        if !self.restore_previous_tabs {
            return self.clear_session();
        }
        let Some(path) = self.session_path() else { return Ok(()); };
        fs::create_dir_all(path.parent().expect("session path has a parent"))?;
        let session = StoredSession { locations: file_session_locations(locations) };
        let bytes = serde_json::to_vec_pretty(&session)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        fs::write(path, bytes)
    }

    pub fn load_session(&self) -> io::Result<Vec<String>> {
        let Some(path) = self.session_path() else { return Ok(Vec::new()); };
        if !path.exists() {
            return Ok(Vec::new());
        }
        let session: StoredSession = serde_json::from_slice(&fs::read(path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(file_session_locations(&session.locations))
    }

    pub fn clear_session(&self) -> io::Result<()> {
        if let Some(path) = self.session_path().filter(|path| path.exists()) {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

fn default_record_search_history() -> bool { true }
fn default_search_history_retention_days() -> u32 { 30 }

pub(crate) fn default_new_tab_location() -> String {
    let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    gio::File::for_path(home).uri().to_string()
}

pub(crate) fn file_session_locations(locations: &[String]) -> Vec<String> {
    locations.iter().filter(|uri| uri.starts_with("file://")).cloned().collect()
}

fn config_home() -> io::Result<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home unavailable"))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_seven_days() {
        assert_eq!(ApplicationPreferences::default().operation_history_days, 7);
    }

    #[test]
    fn defaults_do_not_restore_tabs_and_open_new_tabs_at_home() {
        let preferences = ApplicationPreferences::default();

        assert!(!preferences.restore_previous_tabs);
        assert_eq!(preferences.new_tab_location, default_new_tab_location());
    }

    #[test]
    fn session_locations_keep_only_file_uris() {
        let locations = vec!["file:///tmp".to_owned(), "preferences:".to_owned(), "smb://server/share".to_owned()];

        assert_eq!(file_session_locations(&locations), vec!["file:///tmp"]);
    }

    #[test]
    fn conflict_handling_defaults_to_asking() {
        assert_eq!(ConflictDefault::default(), ConflictDefault::Ask);
        assert_eq!(ApplicationPreferences::default().conflict_default, ConflictDefault::Ask);
    }

    #[test]
    fn search_defaults_favour_privacy_over_content_indexing() {
        let preferences = ApplicationPreferences::default();

        assert!(!preferences.content_search_enabled);
        assert!(preferences.record_search_history);
        assert_eq!(preferences.search_history_retention_days, 30);
    }

    #[test]
    fn existing_preferences_json_without_search_fields_still_deserializes() {
        let stored = serde_json::json!({ "operation_history_days": 7 });
        let preferences: ApplicationPreferences = serde_json::from_value(stored).unwrap();

        assert!(!preferences.content_search_enabled);
        assert!(preferences.record_search_history);
        assert_eq!(preferences.search_history_retention_days, 30);
    }
}
