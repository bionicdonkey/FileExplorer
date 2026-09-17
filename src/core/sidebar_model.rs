use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gio::prelude::*;
use serde::{Deserialize, Serialize};

use crate::core::{Location, RecoverableStateNotice};

fn default_expanded_section() -> String {
    "Quick access".into()
}

const REMOTE_CHILD_ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,standard::icon,standard::content-type,standard::target-uri";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarLocationKind {
    Local(PathBuf),
    Uri(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarLocation {
    kind: SidebarLocationKind,
    label: String,
    icon_name: String,
}

impl SidebarLocation {
    pub fn from_path(
        path: PathBuf,
        label: impl Into<String>,
        icon_name: impl Into<String>,
    ) -> Self {
        Self {
            kind: SidebarLocationKind::Local(path),
            label: label.into(),
            icon_name: icon_name.into(),
        }
    }

    pub fn from_uri(
        uri: impl Into<String>,
        label: impl Into<String>,
        icon_name: impl Into<String>,
    ) -> Self {
        Self {
            kind: SidebarLocationKind::Uri(uri.into()),
            label: label.into(),
            icon_name: icon_name.into(),
        }
    }

    pub fn local_path(&self) -> Option<&Path> {
        match &self.kind {
            SidebarLocationKind::Local(path) => Some(path),
            SidebarLocationKind::Uri(_) => None,
        }
    }

    pub fn uri(&self) -> String {
        match &self.kind {
            SidebarLocationKind::Local(path) => gio::File::for_path(path).uri().to_string(),
            SidebarLocationKind::Uri(uri) => uri.clone(),
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn icon_name(&self) -> &str {
        &self.icon_name
    }
}

#[derive(Clone, Debug)]
pub struct SidebarNode {
    pub location: SidebarLocation,
    pub expandable: bool,
    pub children: Vec<SidebarNode>,
}

impl SidebarNode {
    fn local(
        path: PathBuf,
        label: impl Into<String>,
        icon_name: impl Into<String>,
        expandable: bool,
    ) -> Self {
        Self {
            location: SidebarLocation::from_path(path, label, icon_name),
            expandable,
            children: Vec::new(),
        }
    }

    fn remote(
        uri: String,
        label: impl Into<String>,
        icon_name: impl Into<String>,
        expandable: bool,
    ) -> Self {
        Self {
            location: SidebarLocation::from_uri(uri, label, icon_name),
            expandable,
            children: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SidebarSection {
    pub title: String,
    pub nodes: Vec<SidebarNode>,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredPreferences {
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    locations: Vec<serde_json::Value>,
    #[serde(default)]
    pins: Vec<serde_json::Value>,
    #[serde(default)]
    remote_pins: Vec<serde_json::Value>,
    #[serde(default)]
    section_expanded: BTreeMap<String, bool>,
    #[serde(default)]
    expanded_locations: BTreeMap<String, bool>,
    #[serde(default = "default_expanded_section")]
    default_expanded_section: String,
}

pub struct SidebarPreferences {
    path: Option<PathBuf>,
    pins: Vec<PathBuf>,
    remote_pins: Vec<String>,
    section_expanded: BTreeMap<String, bool>,
    expanded_locations: BTreeMap<String, bool>,
    default_expanded_section: String,
    needs_defaults: bool,
    recoverable_notices: Vec<RecoverableStateNotice>,
}

impl SidebarPreferences {
    pub fn in_memory() -> Self {
        Self {
            path: None,
            pins: Vec::new(),
            remote_pins: Vec::new(),
            section_expanded: BTreeMap::new(),
            expanded_locations: BTreeMap::new(),
            default_expanded_section: default_expanded_section(),
            needs_defaults: false,
            recoverable_notices: Vec::new(),
        }
    }

    pub fn load_default() -> io::Result<Self> {
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "home directory is unavailable")
            })?;
        Self::load_from(config_home.join("zorin-explorer/sidebar.json"))
    }

    pub fn load_from(path: PathBuf) -> io::Result<Self> {
        if !path.exists() {
            return Ok(Self {
                path: Some(path),
                pins: Vec::new(),
                remote_pins: Vec::new(),
                section_expanded: BTreeMap::new(),
                expanded_locations: BTreeMap::new(),
                default_expanded_section: default_expanded_section(),
                needs_defaults: true,
                recoverable_notices: Vec::new(),
            });
        }
        let stored = serde_json::from_slice::<StoredPreferences>(&fs::read(&path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let mut preferences = Self {
            path: Some(path),
            pins: Vec::new(),
            remote_pins: Vec::new(),
            section_expanded: stored.section_expanded,
            expanded_locations: stored.expanded_locations,
            default_expanded_section: stored.default_expanded_section,
            needs_defaults: false,
            recoverable_notices: Vec::new(),
        };
        if stored.schema_version == Some(2) {
            for value in stored.locations {
                let Some(uri) = value.as_str() else {
                    preferences
                        .recoverable_notices
                        .push(RecoverableStateNotice::MalformedRecord);
                    continue;
                };
                match Location::from_uri(uri) {
                    Ok(location) => match location.local_path() {
                        // A V2 file URI is already canonical identity. Do not
                        // canonicalize the path on disk: that would collapse a
                        // symlink and change the user's saved pin.
                        Some(path) if !preferences.pins.contains(&path) => {
                            preferences.pins.push(path)
                        }
                        Some(_) => {}
                        None => {
                            if preferences.pin_remote(location.uri().to_owned()).is_err() {
                                preferences
                                    .recoverable_notices
                                    .push(RecoverableStateNotice::MalformedRecord);
                            }
                        }
                    },
                    Err(_) => preferences
                        .recoverable_notices
                        .push(RecoverableStateNotice::MalformedRecord),
                }
            }
        } else {
            for value in stored.pins {
                match value.as_str() {
                    Some(pin) => preferences.pin(PathBuf::from(pin)),
                    None => preferences
                        .recoverable_notices
                        .push(RecoverableStateNotice::MalformedRecord),
                }
            }
            for value in stored.remote_pins {
                let Some(uri) = value.as_str() else {
                    preferences
                        .recoverable_notices
                        .push(RecoverableStateNotice::MalformedRecord);
                    continue;
                };
                if preferences.pin_remote(uri.to_owned()).is_err() {
                    preferences
                        .recoverable_notices
                        .push(RecoverableStateNotice::MalformedRecord);
                }
            }
        }
        Ok(preferences)
    }

    pub fn pins(&self) -> &[PathBuf] {
        &self.pins
    }
    pub fn remote_pins(&self) -> &[String] {
        &self.remote_pins
    }
    pub fn recoverable_notices(&self) -> &[RecoverableStateNotice] {
        &self.recoverable_notices
    }
    pub fn section_expanded(&self, section: &str) -> bool {
        self.section_expanded
            .get(section)
            .copied()
            .unwrap_or_else(|| section == self.default_expanded_section)
    }
    pub fn default_expanded_section(&self) -> &str {
        &self.default_expanded_section
    }
    pub fn set_default_expanded_section(&mut self, section: impl Into<String>) {
        self.default_expanded_section = section.into();
    }
    pub fn clear_section_expansion(&mut self) {
        self.section_expanded.clear();
    }
    pub fn set_section_expanded(&mut self, section: impl Into<String>, expanded: bool) {
        self.section_expanded.insert(section.into(), expanded);
    }
    pub fn devices_expanded(&self) -> bool {
        self.section_expanded
            .get("This PC/Devices")
            .copied()
            .unwrap_or(false)
    }
    pub fn set_devices_expanded(&mut self, expanded: bool) {
        self.section_expanded
            .insert("This PC/Devices".into(), expanded);
    }
    pub fn location_expanded(&self, location: &SidebarLocation) -> bool {
        self.expanded_locations
            .get(&location.uri())
            .copied()
            .unwrap_or(false)
    }
    pub fn set_location_expanded(&mut self, location: &SidebarLocation, expanded: bool) {
        self.expanded_locations.insert(location.uri(), expanded);
    }

    pub fn pin(&mut self, path: PathBuf) {
        let path = normalized_path(path);
        if !self.pins.iter().any(|pinned| pinned == &path) {
            self.pins.push(path);
        }
    }

    pub fn unpin(&mut self, path: &Path) {
        let path = normalized_path(path.to_path_buf());
        self.pins.retain(|pinned| pinned != &path);
    }

    pub fn is_pinned(&self, path: &Path) -> bool {
        self.pins
            .iter()
            .any(|pinned| pinned == &normalized_path(path.to_path_buf()))
    }

    pub fn pin_remote(&mut self, uri: String) -> Result<(), String> {
        let uri = normalized_remote_uri(&uri)?;
        if !self.remote_pins.contains(&uri) {
            self.remote_pins.push(uri);
        }
        Ok(())
    }

    pub fn unpin_remote(&mut self, uri: &str) {
        self.remote_pins.retain(|pinned| pinned != uri);
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "sidebar preference path has no parent",
            )
        })?;
        fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(&StoredPreferences {
            schema_version: Some(2),
            locations: self
                .pins
                .iter()
                .map(|path| Location::from_local_path(path).uri().to_owned())
                .chain(
                    self.remote_pins
                        .iter()
                        .filter_map(|uri| Location::from_uri(uri).ok())
                        .map(|location| location.uri().to_owned()),
                )
                .map(serde_json::Value::String)
                .collect(),
            pins: Vec::new(),
            remote_pins: Vec::new(),
            section_expanded: self.section_expanded.clone(),
            expanded_locations: self.expanded_locations.clone(),
            default_expanded_section: self.default_expanded_section.clone(),
        })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)
    }

    fn install_default_pins(&mut self, home: &Path) {
        if !self.needs_defaults {
            return;
        }
        self.needs_defaults = false;
        for folder in [
            home.to_path_buf(),
            home.join("Desktop"),
            home.join("Documents"),
            home.join("Downloads"),
            home.join("Pictures"),
        ] {
            if folder.exists() {
                self.pin(folder);
            }
        }
    }
}

pub struct SidebarModel {
    preferences: SidebarPreferences,
    home: PathBuf,
}

impl SidebarModel {
    pub fn new(mut preferences: SidebarPreferences, home: PathBuf) -> Self {
        preferences.install_default_pins(&home);
        Self { preferences, home }
    }

    pub fn sections(&self) -> Vec<SidebarSection> {
        vec![
            self.quick_access(),
            self.user_folders(),
            self.this_pc(),
            self.network_and_online(),
        ]
    }

    pub fn preferences(&self) -> &SidebarPreferences {
        &self.preferences
    }
    pub fn preferences_mut(&mut self) -> &mut SidebarPreferences {
        &mut self.preferences
    }

    pub fn mount_for_location(&self, location: &SidebarLocation) -> Option<gio::Mount> {
        let uri = location.uri();
        gio::VolumeMonitor::get()
            .mounts()
            .into_iter()
            .find(|mount| mount.root().uri() == uri)
    }

    pub fn mount_for_uri(&self, uri: &str) -> Option<gio::Mount> {
        gio::VolumeMonitor::get()
            .mounts()
            .into_iter()
            .find(|mount| mount.root().uri() == uri)
    }

    pub fn children_for_location(&self, location: &SidebarLocation) -> Vec<SidebarNode> {
        match location.local_path() {
            Some(path) => self.local_children(path),
            None => self.remote_children(&location.uri()),
        }
    }

    fn local_children(&self, path: &Path) -> Vec<SidebarNode> {
        let mut children = match fs::read_dir(path) {
            Ok(entries) => entries
                .flatten()
                .filter_map(|entry| {
                    let child = entry.path();
                    child.is_dir().then(|| {
                        SidebarNode::local(
                            child.clone(),
                            entry.file_name().to_string_lossy(),
                            "folder-symbolic",
                            true,
                        )
                    })
                })
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        children.sort_by(|left, right| left.location.label.cmp(&right.location.label));
        children
    }

    fn quick_access(&self) -> SidebarSection {
        let nodes = self
            .preferences
            .pins()
            .iter()
            .filter(|path| path.exists())
            .map(|path| {
                SidebarNode::local(
                    path.clone(),
                    display_name(path),
                    standard_folder_icon(path),
                    true,
                )
            })
            .collect();
        SidebarSection {
            title: "Quick access".into(),
            nodes,
        }
    }

    fn user_folders(&self) -> SidebarSection {
        let mut nodes = vec![SidebarNode::local(
            self.home.clone(),
            "Home",
            "user-home-symbolic",
            true,
        )];
        for (name, icon) in [
            ("Desktop", "user-desktop-symbolic"),
            ("Documents", "folder-documents-symbolic"),
            ("Downloads", "folder-download-symbolic"),
            ("Music", "folder-music-symbolic"),
            ("Pictures", "folder-pictures-symbolic"),
            ("Videos", "folder-videos-symbolic"),
        ] {
            let path = self.home.join(name);
            if path.exists() {
                nodes.push(SidebarNode::local(path, name, icon, true));
            }
        }
        SidebarSection {
            title: user_section_title(&glib::real_name(), &self.home),
            nodes,
        }
    }

    fn this_pc(&self) -> SidebarSection {
        let mut nodes = vec![SidebarNode::local(
            PathBuf::from("/"),
            "File System",
            "drive-harddisk-symbolic",
            true,
        )];
        nodes.extend(self.local_mounts());
        SidebarSection {
            title: "This PC".into(),
            nodes,
        }
    }

    fn network_and_online(&self) -> SidebarSection {
        let mut nodes = vec![
            SidebarNode::remote(
                "network:///".into(),
                "Browse Network",
                "network-workgroup-symbolic",
                true,
            ),
            SidebarNode::remote(
                "computer:///".into(),
                "Online accounts",
                "folder-remote-symbolic",
                true,
            ),
        ];
        nodes.extend(self.preferences.remote_pins().iter().map(|uri| {
            SidebarNode::remote(
                uri.clone(),
                remote_label(uri),
                "folder-remote-symbolic",
                true,
            )
        }));
        nodes.extend(self.remote_mounts());
        SidebarSection {
            title: "Network & online".into(),
            nodes,
        }
    }

    fn local_mounts(&self) -> Vec<SidebarNode> {
        let mut mounts = gio::VolumeMonitor::get()
            .mounts()
            .into_iter()
            .filter_map(|mount| {
                let root = mount.root();
                if !is_local_mount_uri(&root.uri()) {
                    return None;
                }
                let path = root.path()?;
                if path == Path::new("/") || path.starts_with(&self.home) {
                    return None;
                }
                let icon = mount_icon_name(&mount, "drive-harddisk-symbolic");
                Some((
                    device_kind_rank(&icon),
                    mount.name().to_lowercase(),
                    SidebarNode::local(path, mount.name(), icon, true),
                ))
            })
            .collect::<Vec<_>>();
        mounts.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        mounts.into_iter().map(|(_, _, node)| node).collect()
    }

    fn remote_mounts(&self) -> Vec<SidebarNode> {
        gio::VolumeMonitor::get()
            .mounts()
            .into_iter()
            .filter_map(|mount| {
                let root = mount.root();
                (!is_local_mount_uri(&root.uri())).then(|| {
                    SidebarNode::remote(
                        root.uri().to_string(),
                        mount.name(),
                        mount_icon_name(&mount, "folder-remote-symbolic"),
                        true,
                    )
                })
            })
            .collect()
    }

    fn remote_children(&self, uri: &str) -> Vec<SidebarNode> {
        let file = gio::File::for_uri(uri);
        let Ok(enumerator) = file.enumerate_children(
            REMOTE_CHILD_ATTRIBUTES,
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        ) else {
            return Vec::new();
        };
        let mut children = Vec::new();
        while let Ok(Some(info)) = enumerator.next_file(gio::Cancellable::NONE) {
            if uri == "computer:///" && !info.name().to_string_lossy().ends_with(".volume") {
                continue;
            }
            let icon = info
                .icon()
                .and_then(themed_icon_name)
                .unwrap_or_else(|| "folder-remote-symbolic".into());
            let child_uri = info
                .attribute_string("standard::target-uri")
                .map(|uri| uri.to_string())
                .unwrap_or_else(|| canonical_child_uri(uri, &info.name()));
            children.push(SidebarNode::remote(
                child_uri,
                info.display_name(),
                icon,
                true,
            ));
        }
        children.sort_by(|left, right| left.location.label.cmp(&right.location.label));
        children
    }
}

fn normalized_path(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

fn user_section_title(real_name: &std::ffi::OsStr, home: &Path) -> String {
    let real_name = real_name.to_string_lossy().trim().to_owned();
    if real_name.is_empty() || real_name.eq_ignore_ascii_case("unknown") {
        display_name(home)
    } else {
        real_name
    }
}

pub(crate) fn standard_folder_icon(path: &Path) -> &'static str {
    match path.file_name().and_then(|name| name.to_str()) {
        Some("Desktop") => "user-desktop-symbolic",
        Some("Documents") => "folder-documents-symbolic",
        Some("Downloads") => "folder-download-symbolic",
        Some("Music") => "folder-music-symbolic",
        Some("Pictures") => "folder-pictures-symbolic",
        Some("Videos") => "folder-videos-symbolic",
        _ => "folder-symbolic",
    }
}

fn mount_icon_name(mount: &gio::Mount, fallback: &str) -> String {
    themed_icon_name(mount.icon()).unwrap_or_else(|| fallback.to_owned())
}

fn device_kind_rank(icon_name: &str) -> u8 {
    if icon_name.contains("removable") || icon_name.contains("media") {
        0
    } else if icon_name.contains("harddisk") {
        1
    } else {
        2
    }
}

fn is_local_mount_uri(uri: &str) -> bool {
    uri.starts_with("file://")
}

fn themed_icon_name(icon: gio::Icon) -> Option<String> {
    icon.downcast::<gio::ThemedIcon>()
        .ok()
        .and_then(|icon| icon.names().into_iter().next())
        .map(|name| name.to_string())
}

fn normalized_remote_uri(uri: &str) -> Result<String, String> {
    let location = Location::from_uri(uri.trim()).map_err(|_| {
        "Enter a valid provider address without credentials or query data".to_owned()
    })?;
    if location.local_path().is_some() {
        return Err("Choose a provider address rather than a local file".into());
    }
    Ok(location.uri().to_owned())
}

fn remote_label(uri: &str) -> String {
    uri.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(uri)
        .to_owned()
}

fn canonical_child_uri(parent_uri: &str, name: &Path) -> String {
    gio::File::for_uri(parent_uri)
        .resolve_relative_path(name)
        .uri()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use gio::prelude::*;

    use super::{
        device_kind_rank, is_local_mount_uri, user_section_title, SidebarLocation, SidebarModel,
        SidebarPreferences,
    };

    #[test]
    fn pinning_a_path_is_persisted_once() {
        let mut preferences = SidebarPreferences::in_memory();
        preferences.pin(PathBuf::from("/tmp/one"));
        preferences.pin(PathBuf::from("/tmp/one"));

        assert_eq!(preferences.pins(), &[PathBuf::from("/tmp/one")]);
    }

    #[test]
    fn sidebar_section_state_round_trips_with_preferences() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("sidebar.json");
        let mut preferences = SidebarPreferences::load_from(path.clone()).unwrap();

        preferences.set_section_expanded("This PC", false);
        preferences.save().unwrap();

        let restored = SidebarPreferences::load_from(path).unwrap();
        assert!(!restored.section_expanded("This PC"));
        assert!(restored.section_expanded("Quick access"));
    }

    #[test]
    fn expanded_tree_locations_round_trip_with_preferences() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("sidebar.json");
        let location = SidebarLocation::from_path(
            PathBuf::from("/tmp/projects"),
            "Projects",
            "folder-symbolic",
        );
        let mut preferences = SidebarPreferences::load_from(path.clone()).unwrap();

        preferences.set_location_expanded(&location, true);
        preferences.save().unwrap();

        let restored = SidebarPreferences::load_from(path).unwrap();
        assert!(restored.location_expanded(&location));
    }

    #[test]
    fn sidebar_separates_personal_folders_from_devices() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("user");
        fs::create_dir_all(&home).unwrap();
        let model = SidebarModel::new(SidebarPreferences::in_memory(), home);

        let titles = model
            .sections()
            .into_iter()
            .map(|section| section.title)
            .collect::<Vec<_>>();
        assert_eq!(titles[0], "Quick access");
        assert_eq!(
            titles[1],
            user_section_title(&glib::real_name(), Path::new("/home/user"))
        );
        assert_eq!(titles[2], "This PC");
        assert_eq!(titles[3], "Network & online");
    }

    #[test]
    fn device_sorting_places_removable_media_before_fixed_drives() {
        assert!(
            device_kind_rank("drive-removable-media-symbolic")
                < device_kind_rank("drive-harddisk-symbolic")
        );
        assert!(
            device_kind_rank("drive-harddisk-symbolic")
                < device_kind_rank("folder-remote-symbolic")
        );
    }

    #[test]
    fn devices_group_defaults_to_collapsed_and_persists() {
        let mut preferences = SidebarPreferences::in_memory();
        assert!(!preferences.devices_expanded());
        preferences.set_devices_expanded(true);
        assert!(preferences.devices_expanded());
    }

    #[test]
    fn user_section_prefers_the_account_display_name() {
        assert_eq!(
            user_section_title(std::ffi::OsStr::new("Alex Example"), Path::new("/home/user")),
            "Alex Example"
        );
        assert_eq!(
            user_section_title(std::ffi::OsStr::new(""), Path::new("/home/user")),
            "user"
        );
    }

    #[test]
    fn only_file_uris_are_classified_as_local_devices() {
        assert!(is_local_mount_uri("file:///media/user/Storage"));
        assert!(!is_local_mount_uri("smb://octavia.local/software"));
    }

    #[test]
    fn unpinning_removes_the_matching_path() {
        let mut preferences = SidebarPreferences::in_memory();
        preferences.pin(PathBuf::from("/tmp/one"));
        preferences.unpin(Path::new("/tmp/one"));

        assert!(preferences.pins().is_empty());
    }

    #[test]
    fn uri_location_without_path_is_not_local() {
        let location = SidebarLocation::from_uri(
            "google-drive://account/root",
            "Google Drive",
            "folder-remote-symbolic",
        );
        assert!(location.local_path().is_none());
        assert_eq!(location.uri(), "google-drive://account/root");
    }

    #[test]
    fn local_sidebar_location_provides_a_navigation_path() {
        let location = SidebarLocation::from_path(PathBuf::from("/tmp"), "tmp", "folder-symbolic");
        assert_eq!(location.local_path(), Some(Path::new("/tmp")));
    }

    #[test]
    fn remote_pin_is_saved_once_and_can_be_removed() {
        let mut preferences = SidebarPreferences::in_memory();
        preferences
            .pin_remote("smb://nas.local/media".into())
            .unwrap();
        preferences
            .pin_remote("smb://nas.local/media".into())
            .unwrap();

        assert_eq!(preferences.remote_pins(), ["smb://nas.local/media"]);
        preferences.unpin_remote("smb://nas.local/media");
        assert!(preferences.remote_pins().is_empty());
    }

    #[test]
    fn v2_sidebar_pins_persist_canonical_uris_and_report_bad_records() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("sidebar.json");
        std::fs::write(
            &path,
            r#"{"schema_version":2,"locations":["SMB://HOST/share","smb://user@host/no"]}"#,
        )
        .unwrap();
        let preferences = SidebarPreferences::load_from(path.clone()).unwrap();
        assert_eq!(preferences.remote_pins(), ["smb://host/share"]);
        assert_eq!(
            preferences.recoverable_notices(),
            [crate::core::RecoverableStateNotice::MalformedRecord]
        );
        preferences.save().unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["schema_version"], 2);
        assert_eq!(saved["locations"], serde_json::json!(["smb://host/share"]));
    }

    #[test]
    fn v2_pin_loading_preserves_symlink_identity_and_other_valid_providers() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        let link = temporary.path().join("link");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let path = temporary.path().join("sidebar.json");
        let link_uri = gio::File::for_path(&link).uri().to_string();
        std::fs::write(
            &path,
            serde_json::json!({
                "schema_version": 2,
                "locations": [link_uri, "google-drive://account/root", 9]
            })
            .to_string(),
        )
        .unwrap();

        let preferences = SidebarPreferences::load_from(path.clone()).unwrap();
        assert_eq!(preferences.pins(), [link]);
        assert_eq!(preferences.remote_pins(), ["google-drive://account/root"]);
        assert_eq!(
            preferences.recoverable_notices(),
            [crate::core::RecoverableStateNotice::MalformedRecord]
        );
        preferences.save().unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(
            saved["locations"][0],
            gio::File::for_path(temporary.path().join("link"))
                .uri()
                .as_str()
        );
        assert_eq!(saved["locations"][1], "google-drive://account/root");
    }

    #[test]
    fn remote_enumeration_requests_the_name_required_for_child_uris() {
        assert!(super::REMOTE_CHILD_ATTRIBUTES.contains("standard::name"));
    }

    #[test]
    fn remote_enumeration_requests_shortcut_target_uris() {
        assert!(super::REMOTE_CHILD_ATTRIBUTES.contains("standard::target-uri"));
    }

    #[test]
    fn remote_enumeration_requests_content_type_for_provider_backends() {
        assert!(super::REMOTE_CHILD_ATTRIBUTES.contains("standard::content-type"));
    }

    #[test]
    fn virtual_network_children_can_be_enumerated_without_a_gio_critical() {
        let model = SidebarModel::new(SidebarPreferences::in_memory(), PathBuf::from("/tmp"));
        let network = SidebarLocation::from_uri(
            "network:///",
            "Browse Network",
            "network-workgroup-symbolic",
        );
        let _children = model.children_for_location(&network);
    }

    #[test]
    fn online_provider_root_can_be_enumerated_without_a_gio_critical() {
        let model = SidebarModel::new(SidebarPreferences::in_memory(), PathBuf::from("/tmp"));
        let providers =
            SidebarLocation::from_uri("computer:///", "Online accounts", "folder-remote-symbolic");
        let _children = model.children_for_location(&providers);
    }

    #[test]
    fn child_uri_keeps_the_gio_canonical_entry_name() {
        assert_eq!(
            super::canonical_child_uri("computer:///", Path::new("alex@example.com.volume")),
            "computer:///alex@example.com.volume"
        );
    }
}
