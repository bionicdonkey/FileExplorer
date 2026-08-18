use std::{env, fs, io};
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplicationPreferences { pub operation_history_days: u32, #[serde(default)] pub close_on_last_tab: bool, #[serde(skip)] path: Option<PathBuf> }
impl Default for ApplicationPreferences { fn default() -> Self { Self { operation_history_days: 7, close_on_last_tab: false, path: None } } }
impl ApplicationPreferences {
    pub fn load_default() -> io::Result<Self> {
        let base = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home unavailable"))?;
        let path = base.join("zorin-explorer/preferences.json");
        if !path.exists() { return Ok(Self { path: Some(path), ..Self::default() }); }
        let mut value: Self = serde_json::from_slice(&fs::read(&path)?).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?; value.path = Some(path); Ok(value)
    }
    pub fn save(&self) -> io::Result<()> { let Some(path) = &self.path else { return Ok(()); }; fs::create_dir_all(path.parent().unwrap())?; fs::write(path, serde_json::to_vec_pretty(self).unwrap()) }
    pub fn history_path(&self) -> Option<PathBuf> { self.path.as_ref().and_then(|path| path.parent().map(|parent| parent.join("operation-history.json"))) }
}
#[cfg(test)] mod tests { use super::*; #[test] fn defaults_to_seven_days() { assert_eq!(ApplicationPreferences::default().operation_history_days, 7); } }
