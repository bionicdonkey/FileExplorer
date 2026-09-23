use std::path::PathBuf;
use std::{env, fs, io};

use gio::prelude::*;
use serde::{Deserialize, Serialize};

use crate::core::Location;

const URI_STATE_VERSION: u32 = 2;
pub const MAX_SESSION_TABS: usize = 50;
pub const MAX_HISTORY_RECORDS: usize = 100;
pub const MAX_STATE_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn read_bounded_state(path: &std::path::Path) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_STATE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "saved state exceeds its size limit",
        ));
    }
    Ok(bytes)
}

pub(crate) fn write_bounded_state(path: &std::path::Path, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_STATE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "saved state exceeds its size limit",
        ));
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}

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

/// Version 2 state persists only canonical, credential-free URIs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UriStateV2 {
    pub schema_version: u32,
    pub locations: Vec<String>,
}

/// State could not be read safely. Its display text never contains stored input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoverableStateError {
    InvalidState,
}

/// A user-safe explanation that a persisted record was skipped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoverableStateNotice {
    MalformedRecord,
    UnsupportedSchema,
}

/// Successfully decoded state together with recoverable record-level notices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedUriState {
    pub locations: Vec<Location>,
    pub notices: Vec<RecoverableStateNotice>,
}

impl std::fmt::Display for RecoverableStateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("saved location state could not be recovered")
    }
}
impl std::error::Error for RecoverableStateError {}

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
            return Ok(Self {
                path: Some(path),
                ..Self::default()
            });
        }
        let mut value: Self = read_bounded_state(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        value.path = Some(path);
        Ok(value)
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        fs::create_dir_all(path.parent().expect("preferences path has a parent"))?;
        write_bounded_state(
            path,
            &serde_json::to_vec_pretty(self).expect("preferences serialize"),
        )
    }

    pub fn history_path(&self) -> Option<PathBuf> {
        self.path.as_ref().and_then(|path| {
            path.parent()
                .map(|parent| parent.join("operation-history.json"))
        })
    }

    pub fn session_path(&self) -> Option<PathBuf> {
        self.path
            .as_ref()
            .and_then(|path| path.parent().map(|parent| parent.join("session.json")))
    }

    pub fn search_history_path(&self) -> Option<PathBuf> {
        self.path.as_ref().and_then(|path| {
            path.parent()
                .map(|parent| parent.join("search-history.json"))
        })
    }

    pub fn save_session(&self, locations: &[Location]) -> io::Result<()> {
        if !self.restore_previous_tabs {
            return self.clear_session();
        }
        let Some(path) = self.session_path() else {
            return Ok(());
        };
        fs::create_dir_all(path.parent().expect("session path has a parent"))?;
        let bytes = encode_session(locations)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        write_bounded_state(&path, &bytes)
    }

    /// Loads saved tabs without discarding recovery notices. The application
    /// boundary can keep startup safe while making skipped records observable.
    pub fn load_session(&self) -> io::Result<DecodedUriState> {
        let Some(path) = self.session_path() else {
            return Ok(DecodedUriState {
                locations: Vec::new(),
                notices: Vec::new(),
            });
        };
        if !path.exists() {
            return Ok(DecodedUriState {
                locations: Vec::new(),
                notices: Vec::new(),
            });
        }
        decode_session_state(&read_bounded_state(&path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub fn clear_session(&self) -> io::Result<()> {
        if let Some(path) = self.session_path().filter(|path| path.exists()) {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

fn default_record_search_history() -> bool {
    true
}
fn default_search_history_retention_days() -> u32 {
    30
}

pub(crate) fn default_new_tab_location() -> String {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    gio::File::for_path(home).uri().to_string()
}

pub fn encode_session(locations: &[Location]) -> Result<Vec<u8>, RecoverableStateError> {
    let bytes = serde_json::to_vec_pretty(&UriStateV2 {
        schema_version: URI_STATE_VERSION,
        locations: locations
            .iter()
            .take(MAX_SESSION_TABS)
            .map(|location| location.uri().to_owned())
            .collect(),
    })
    .map_err(|_| RecoverableStateError::InvalidState)?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(RecoverableStateError::InvalidState);
    }
    Ok(bytes)
}

/// Decodes V2 URI state or performs the one-release, read-only V1 path import.
/// Invalid records are intentionally skipped so one stale tab cannot block startup.
pub fn decode_session(bytes: &[u8]) -> Result<Vec<Location>, RecoverableStateError> {
    Ok(decode_session_state(bytes)?.locations)
}

/// Decodes session state while retaining user-safe notices for each skipped
/// record. Callers can show recovery feedback without ever exposing saved URI
/// contents or credentials.
pub fn decode_session_state(bytes: &[u8]) -> Result<DecodedUriState, RecoverableStateError> {
    if bytes.len() > MAX_STATE_BYTES {
        return Err(RecoverableStateError::InvalidState);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| RecoverableStateError::InvalidState)?;
    let object = value
        .as_object()
        .ok_or(RecoverableStateError::InvalidState)?;
    let locations = object
        .get("locations")
        .and_then(serde_json::Value::as_array)
        .ok_or(RecoverableStateError::InvalidState)?;
    match object
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(version) if version == URI_STATE_VERSION as u64 => decode_uri_records(locations),
        None | Some(1) => {
            let mut decoded = Vec::new();
            let mut notices = Vec::new();
            for value in locations.iter().take(MAX_SESSION_TABS) {
                let Some(value) = value.as_str() else {
                    notices.push(RecoverableStateNotice::MalformedRecord);
                    continue;
                };
                match Location::from_uri(value) {
                    Ok(location) if location.uri().starts_with("file:") => decoded.push(location),
                    Ok(_) => notices.push(RecoverableStateNotice::MalformedRecord),
                    _ => decoded.push(Location::from_local_path(&PathBuf::from(value))),
                }
            }
            Ok(DecodedUriState {
                locations: decoded,
                notices,
            })
        }
        _ => Err(RecoverableStateError::InvalidState),
    }
}

fn decode_uri_records(
    locations: &[serde_json::Value],
) -> Result<DecodedUriState, RecoverableStateError> {
    let mut decoded = Vec::new();
    let mut notices = Vec::new();
    for value in locations.iter().take(MAX_SESSION_TABS) {
        match value.as_str().and_then(|uri| Location::from_uri(uri).ok()) {
            Some(location) => decoded.push(location),
            None => notices.push(RecoverableStateNotice::MalformedRecord),
        }
    }
    Ok(DecodedUriState {
        locations: decoded,
        notices,
    })
}

/// Read-only V1 migration boundary. It does not mutate input or write state.
pub fn import_legacy_local_paths(paths: &[PathBuf]) -> Vec<Location> {
    paths
        .iter()
        .map(|path| Location::from_local_path(path))
        .collect()
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
    fn session_codec_preserves_non_file_uris() {
        let locations = vec![Location::from_uri("smb://server/share").unwrap()];
        assert_eq!(
            decode_session(&encode_session(&locations).unwrap()).unwrap(),
            locations
        );
    }

    #[test]
    fn load_session_exposes_skipped_record_notices_to_the_application_boundary() {
        let temporary = tempfile::tempdir().unwrap();
        let preferences = ApplicationPreferences {
            path: Some(temporary.path().join("preferences.json")),
            ..ApplicationPreferences::default()
        };
        std::fs::write(
            preferences.session_path().unwrap(),
            br#"{"schema_version":2,"locations":["file:///tmp/ok","smb://user@host/no"]}"#,
        )
        .unwrap();

        let state = preferences.load_session().unwrap();

        assert_eq!(state.locations[0].uri(), "file:///tmp/ok");
        assert_eq!(state.notices, [RecoverableStateNotice::MalformedRecord]);
    }

    #[test]
    fn conflict_handling_defaults_to_asking() {
        assert_eq!(ConflictDefault::default(), ConflictDefault::Ask);
        assert_eq!(
            ApplicationPreferences::default().conflict_default,
            ConflictDefault::Ask
        );
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
