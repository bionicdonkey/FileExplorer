#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::{
        GridSize, ListColumn, ListColumnLayout, ViewMode, ViewOptions, ViewPreferencesStore,
    };

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

        assert_eq!(
            store.options_for(Path::new("/tmp/Pictures"), &[]).view_mode,
            ViewMode::IconGrid
        );
    }

    #[test]
    fn explicit_folder_options_keep_custom_list_column_order_and_widths() {
        let path = Path::new("/tmp/columns");
        let expected = ViewOptions {
            list_columns: ListColumnLayout {
                order: vec![
                    ListColumn::Size,
                    ListColumn::Name,
                    ListColumn::Type,
                    ListColumn::DateModified,
                ],
                widths: BTreeMap::from([(ListColumn::Name, 420), (ListColumn::Size, 110)]),
            },
            ..ViewOptions::default()
        };
        let mut store = ViewPreferencesStore::in_memory();

        store.set(path, expected.clone());

        assert_eq!(store.options_for(path, &[]), expected);
    }

    #[test]
    fn malformed_column_layout_returns_all_columns_once_in_default_order() {
        let layout = ListColumnLayout {
            order: vec![ListColumn::Name, ListColumn::Name],
            widths: BTreeMap::new(),
        };

        assert_eq!(layout.normalized().order, ListColumn::all());
    }

    #[test]
    fn grid_sizes_expand_tile_geometry_with_icon_size() {
        assert!(
            GridSize::Small.grid_layout().tile_width < GridSize::Medium.grid_layout().tile_width
        );
        assert!(
            GridSize::Medium.grid_layout().tile_width < GridSize::Large.grid_layout().tile_width
        );
        assert!(
            GridSize::Large.grid_layout().tile_width
                < GridSize::ExtraLarge.grid_layout().tile_width
        );
    }

    #[test]
    fn small_icon_tiles_use_compact_padding_without_forcing_height() {
        let small = GridSize::Small.grid_layout();

        assert!(small.vertical_padding < GridSize::Medium.grid_layout().vertical_padding);
        assert!(small.vertical_padding <= 8);
    }

    #[test]
    fn hidden_files_are_off_by_default_and_persist_per_folder() {
        let path = Path::new("/tmp/hidden");
        let mut store = ViewPreferencesStore::in_memory();
        assert!(!ViewOptions::default().show_hidden);
        store.set(
            path,
            ViewOptions {
                show_hidden: true,
                ..ViewOptions::default()
            },
        );
        assert!(store.options_for(path, &[]).show_hidden);
    }

    #[test]
    fn global_defaults_apply_when_a_folder_has_no_override() {
        let mut store = ViewPreferencesStore::in_memory();
        store.set_defaults(ViewOptions {
            view_mode: ViewMode::IconGrid,
            ..ViewOptions::default()
        });

        assert_eq!(
            store.options_for(Path::new("/tmp/plain"), &[]).view_mode,
            ViewMode::IconGrid
        );
    }

    #[test]
    fn explicit_folder_options_override_global_defaults() {
        let path = Path::new("/tmp/custom");
        let mut store = ViewPreferencesStore::in_memory();
        store.set_defaults(ViewOptions {
            view_mode: ViewMode::IconGrid,
            ..ViewOptions::default()
        });
        store.set(
            path,
            ViewOptions {
                view_mode: ViewMode::DetailedList,
                ..ViewOptions::default()
            },
        );

        assert_eq!(
            store.options_for(path, &[]).view_mode,
            ViewMode::DetailedList
        );
    }

    #[test]
    fn clearing_all_folder_options_preserves_global_defaults() {
        let first = Path::new("/tmp/first");
        let second = Path::new("/tmp/second");
        let mut store = ViewPreferencesStore::in_memory();
        store.set_defaults(ViewOptions {
            view_mode: ViewMode::IconGrid,
            ..ViewOptions::default()
        });
        store.set(
            first,
            ViewOptions {
                view_mode: ViewMode::DetailedList,
                ..ViewOptions::default()
            },
        );
        store.set(
            second,
            ViewOptions {
                view_mode: ViewMode::CompactList,
                ..ViewOptions::default()
            },
        );

        assert_eq!(store.clear_all_folders(), 2);
        assert_eq!(store.options_for(first, &[]).view_mode, ViewMode::IconGrid);
        assert_eq!(store.options_for(second, &[]).view_mode, ViewMode::IconGrid);
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
    Hybrid,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridLayout {
    pub tile_width: i32,
    pub vertical_padding: i32,
    pub label_width_chars: i32,
    pub gap: i32,
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

    pub fn list_icon_size(self) -> i32 {
        match self {
            Self::Small => 16,
            Self::Medium => 20,
            Self::Large => 28,
            Self::ExtraLarge => 36,
        }
    }

    pub fn row_spacing(self) -> i32 {
        match self {
            Self::Small => 2,
            Self::Medium => 6,
            Self::Large => 10,
            Self::ExtraLarge => 14,
        }
    }

    pub fn grid_layout(self) -> GridLayout {
        match self {
            Self::Small => GridLayout {
                tile_width: 128,
                vertical_padding: 4,
                label_width_chars: 14,
                gap: 8,
            },
            Self::Medium => GridLayout {
                tile_width: 172,
                vertical_padding: 6,
                label_width_chars: 18,
                gap: 12,
            },
            Self::Large => GridLayout {
                tile_width: 232,
                vertical_padding: 8,
                label_width_chars: 24,
                gap: 16,
            },
            Self::ExtraLarge => GridLayout {
                tile_width: 300,
                vertical_padding: 10,
                label_width_chars: 30,
                gap: 20,
            },
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListColumn {
    Name,
    DateModified,
    Type,
    Size,
}

impl ListColumn {
    pub fn all() -> Vec<Self> {
        vec![Self::Name, Self::DateModified, Self::Type, Self::Size]
    }

    pub fn default_width(self) -> i32 {
        match self {
            Self::Name => 360,
            Self::DateModified => 150,
            Self::Type => 160,
            Self::Size => 100,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListColumnLayout {
    pub order: Vec<ListColumn>,
    pub widths: BTreeMap<ListColumn, i32>,
}

impl Default for ListColumnLayout {
    fn default() -> Self {
        Self {
            order: ListColumn::all(),
            widths: BTreeMap::new(),
        }
    }
}

impl ListColumnLayout {
    pub fn normalized(&self) -> Self {
        let mut order = Vec::with_capacity(ListColumn::all().len());
        for column in &self.order {
            if !order.contains(column) {
                order.push(*column);
            }
        }
        for column in ListColumn::all() {
            if !order.contains(&column) {
                order.push(column);
            }
        }
        let widths = self
            .widths
            .iter()
            .map(|(column, width)| (*column, (*width).clamp(64, 900)))
            .collect();
        Self { order, widths }
    }

    pub fn width_for(&self, column: ListColumn) -> i32 {
        self.widths
            .get(&column)
            .copied()
            .unwrap_or_else(|| column.default_width())
    }
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
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default)]
    pub list_columns: ListColumnLayout,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            view_mode: ViewMode::Hybrid,
            grid_size: GridSize::Medium,
            sort_field: SortField::Name,
            sort_direction: SortDirection::Ascending,
            folders_first: true,
            group_field: GroupField::None,
            thumbnails: true,
            show_hidden: false,
            list_columns: ListColumnLayout::default(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct StoredOptions {
    #[serde(default)]
    defaults: Option<ViewOptions>,
    #[serde(default)]
    folders: BTreeMap<String, ViewOptions>,
}

pub struct ViewPreferencesStore {
    path: Option<PathBuf>,
    defaults: Option<ViewOptions>,
    folders: BTreeMap<String, ViewOptions>,
}

impl ViewPreferencesStore {
    pub fn in_memory() -> Self {
        Self {
            path: None,
            defaults: None,
            folders: BTreeMap::new(),
        }
    }

    pub fn load_default() -> io::Result<Self> {
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "home directory is unavailable")
            })?;
        Self::load_from(config_home.join("lorin-explorer/view-options.json"))
    }

    pub fn load_from(path: PathBuf) -> io::Result<Self> {
        if !path.exists() {
            return Ok(Self {
                path: Some(path),
                defaults: None,
                folders: BTreeMap::new(),
            });
        }
        let bytes = fs::read(&path)?;
        let stored: StoredOptions = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(Self {
            path: Some(path),
            defaults: stored.defaults,
            folders: stored.folders,
        })
    }

    pub fn options_for(&self, path: &Path, items: &[FileItem]) -> ViewOptions {
        self.folders
            .get(&folder_key(path))
            .cloned()
            .or_else(|| self.defaults.clone())
            .unwrap_or_else(|| automatic_options(path, items))
    }

    pub fn defaults(&self) -> ViewOptions {
        self.defaults.clone().unwrap_or_default()
    }

    pub fn set_defaults(&mut self, defaults: ViewOptions) {
        self.defaults = Some(ViewOptions {
            list_columns: defaults.list_columns.normalized(),
            ..defaults
        });
    }

    pub fn clear(&mut self, path: &Path) -> bool {
        self.folders.remove(&folder_key(path)).is_some()
    }

    pub fn clear_all_folders(&mut self) -> usize {
        let count = self.folders.len();
        self.folders.clear();
        count
    }

    pub fn set(&mut self, path: &Path, options: ViewOptions) {
        self.folders.insert(
            folder_key(path),
            ViewOptions {
                list_columns: options.list_columns.normalized(),
                ..options
            },
        );
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "preference path has no parent")
        })?;
        fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(&StoredOptions {
            defaults: self.defaults.clone(),
            folders: self.folders.clone(),
        })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)
    }
}

fn folder_key(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn automatic_options(path: &Path, items: &[FileItem]) -> ViewOptions {
    let visual_folder = path.file_name().is_some_and(|name| {
        matches!(
            name.to_string_lossy().to_ascii_lowercase().as_str(),
            "pictures" | "videos"
        )
    });
    let visual_majority = !items.is_empty()
        && items.iter().filter(|item| item.is_visual_media()).count() * 2 > items.len();
    if visual_folder || visual_majority {
        ViewOptions {
            view_mode: ViewMode::IconGrid,
            ..ViewOptions::default()
        }
    } else {
        ViewOptions::default()
    }
}
