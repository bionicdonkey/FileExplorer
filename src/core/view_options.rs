#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{GridSize, ViewMode, ViewOptions, ViewPreferencesStore};

    #[test]
    fn explicit_folder_options_round_trip() {
        let path = Path::new("/tmp/Pictures");
        let mut store = ViewPreferencesStore::in_memory();
        let expected = ViewOptions {
            view_mode: ViewMode::IconGrid,
            grid_size: GridSize::Large,
            ..ViewOptions::default()
        };

        store.set(path, expected.clone());
        assert_eq!(store.options_for(path, &[]), expected);
    }

    #[test]
    fn picture_folder_uses_grid_when_no_override_exists() {
        let store = ViewPreferencesStore::in_memory();

        assert_eq!(store.options_for(Path::new("/tmp/Pictures"), &[]).view_mode, ViewMode::IconGrid);
    }
}
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::FileItem;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewMode {
    #[default]
    DetailedList,
    CompactList,
    IconGrid,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GridSize {
    Small,
    #[default]
    Medium,
    Large,
    ExtraLarge,
}

impl GridSize {
    pub fn icon_size(self) -> i32 {
        match self {
            Self::Small => 48,
            Self::Medium => 80,
            Self::Large => 128,
            Self::ExtraLarge => 192,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortField {
    #[default]
    Name,
    DateModified,
    Type,
    Size,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortDirection {
    #[default]
    Ascending,
    Descending,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GroupField {
    #[default]
    None,
    Type,
    Size,
    DateModified,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewOptions {
    pub view_mode: ViewMode,
    pub grid_size: GridSize,
    pub sort_field: SortField,
    pub sort_direction: SortDirection,
    pub folders_first: bool,
    pub group_field: GroupField,
    pub thumbnails: bool,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            view_mode: ViewMode::DetailedList,
            grid_size: GridSize::Medium,
            sort_field: SortField::Name,
            sort_direction: SortDirection::Ascending,
            folders_first: true,
            group_field: GroupField::None,
            thumbnails: true,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct StoredOptions {
    folders: BTreeMap<String, ViewOptions>,
}

pub struct ViewPreferencesStore {
    path: Option<PathBuf>,
    folders: BTreeMap<String, ViewOptions>,
}

impl ViewPreferencesStore {
    pub fn in_memory() -> Self {
        Self { path: None, folders: BTreeMap::new() }
    }

    pub fn load_default() -> io::Result<Self> {
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory is unavailable"))?;
        Self::load_from(config_home.join("zorin-explorer/view-options.json"))
    }

    pub fn load_from(path: PathBuf) -> io::Result<Self> {
        if !path.exists() {
            return Ok(Self { path: Some(path), folders: BTreeMap::new() });
        }
        let bytes = fs::read(&path)?;
        let stored: StoredOptions = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(Self { path: Some(path), folders: stored.folders })
    }

    pub fn options_for(&self, path: &Path, items: &[FileItem]) -> ViewOptions {
        self.folders.get(&folder_key(path)).cloned().unwrap_or_else(|| automatic_options(path, items))
    }

    pub fn set(&mut self, path: &Path, options: ViewOptions) {
        self.folders.insert(folder_key(path), options);
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else { return Ok(()); };
        let parent = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "preference path has no parent"))?;
        fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(&StoredOptions { folders: self.folders.clone() })
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)
    }
}

fn folder_key(path: &Path) -> String {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().into_owned()
}

fn automatic_options(path: &Path, items: &[FileItem]) -> ViewOptions {
    let visual_folder = path.file_name().is_some_and(|name| matches!(name.to_string_lossy().to_ascii_lowercase().as_str(), "pictures" | "videos"));
    let visual_majority = !items.is_empty() && items.iter().filter(|item| item.is_visual_media()).count() * 2 > items.len();
    if visual_folder || visual_majority {
        ViewOptions { view_mode: ViewMode::IconGrid, ..ViewOptions::default() }
    } else {
        ViewOptions::default()
    }
}
