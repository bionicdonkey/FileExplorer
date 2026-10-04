use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gio::prelude::*;
use serde::{Deserialize, Serialize};

use crate::core::{
    contracts::{DirectorySnapshot, EntryKind},
    Location, RecoverableStateNotice,
};

fn default_expanded_section() -> String {
    "Quick access".into()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarLocation {
    location: Location,
    local_path: Option<PathBuf>,
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
            location: Location::from_local_path(&path),
            local_path: Some(path),
            label: label.into(),
            icon_name: icon_name.into(),
        }
    }

    pub fn from_uri(
        uri: impl Into<String>,
        label: impl Into<String>,
        icon_name: impl Into<String>,
    ) -> Result<Self, crate::core::LocationError> {
        let location = Location::from_uri(&uri.into())?;
        Ok(Self::from_location(location, label, icon_name))
    }

    pub fn from_location(
        location: Location,
        label: impl Into<String>,
        icon_name: impl Into<String>,
    ) -> Self {
        Self {
            local_path: location.local_path(),
            location,
            label: label.into(),
            icon_name: icon_name.into(),
        }
    }

    pub fn local_path(&self) -> Option<&Path> {
        self.local_path.as_deref()
    }

    pub fn uri(&self) -> String {
        self.location.uri().to_owned()
    }
    pub fn location(&self) -> &Location {
        &self.location
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
        location: Location,
        label: impl Into<String>,
        icon_name: impl Into<String>,
        expandable: bool,
    ) -> Self {
        Self {
            location: SidebarLocation::from_location(location, label, icon_name),
            expandable,
            children: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SidebarSection {
    pub title: String,
    pub nodes: Vec<SidebarNode>,
    /// Devices GIO reports as present but not mounted. They are kept apart from
    /// `nodes` because they have no location to navigate to until they are
    /// mounted, which is the whole reason they were previously invisible.
    pub mountables: Vec<MountableVolume>,
}

/// A volume that is attached but has nothing mounted. GIO gives a local one no
/// activation root, so there is no URI to open; activating it has to mount it
/// first and navigate to wherever that landed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountableVolume {
    /// Identity used to find the volume again when the row is activated.
    pub id: String,
    pub label: String,
    pub icon_name: String,
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
    pins: Vec<Location>,
    remote_pins: Vec<Location>,
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
                    Ok(location) => preferences.pin_location(location),
                    Err(_) => preferences
                        .recoverable_notices
                        .push(RecoverableStateNotice::MalformedRecord),
                }
            }
        } else {
            for value in stored.pins {
                match value.as_str() {
                    // Legacy paths predate URI identity. Preserve their old
                    // canonicalization rule only at this migration boundary.
                    Some(pin) => preferences.pin(normalized_path(PathBuf::from(pin))),
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

    pub fn pins(&self) -> Vec<PathBuf> {
        self.pins.iter().filter_map(Location::local_path).collect()
    }
    pub fn remote_pins(&self) -> Vec<String> {
        self.remote_pins
            .iter()
            .map(|location| location.uri().to_owned())
            .collect()
    }
    pub fn locations(&self) -> impl Iterator<Item = &Location> {
        self.pins.iter().chain(self.remote_pins.iter())
    }
    pub fn pin_location(&mut self, location: Location) {
        let pins = if location.local_path().is_some() {
            &mut self.pins
        } else {
            &mut self.remote_pins
        };
        if !pins.contains(&location) {
            pins.push(location);
        }
    }
    pub fn unpin_location(&mut self, location: &Location) {
        self.pins.retain(|pin| pin != location);
        self.remote_pins.retain(|pin| pin != location);
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
        self.pin_location(Location::from_local_path(&path));
    }

    pub fn unpin(&mut self, path: &Path) {
        self.unpin_location(&Location::from_local_path(path));
    }

    pub fn is_pinned(&self, path: &Path) -> bool {
        self.pins.contains(&Location::from_local_path(path))
    }

    pub fn pin_remote(&mut self, uri: String) -> Result<(), String> {
        let uri = normalized_remote_uri(&uri)?;
        let location = Location::from_uri(&uri).map_err(|error| error.to_string())?;
        self.pin_location(location);
        Ok(())
    }

    pub fn unpin_remote(&mut self, uri: &str) {
        self.remote_pins.retain(|pinned| pinned.uri() != uri);
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
                .locations()
                .map(|location| location.uri().to_owned())
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

    /// Converts a repository-owned directory snapshot into expandable tree
    /// nodes. Enumeration and URI resolution stay behind `DirectoryRepository`;
    /// the sidebar model only transforms immutable snapshot data on the UI
    /// context.
    pub fn children_from_snapshot(snapshot: &DirectorySnapshot) -> Vec<SidebarNode> {
        let providers_only = snapshot.location.requested.uri() == "computer:///";
        let mut children = snapshot
            .entries
            .iter()
            .filter(|entry| !providers_only || is_online_provider(entry))
            // Shortcuts as well as directories: everything `network:///` lists is
            // a shortcut to a server, so filtering to directories left Browse
            // Network permanently empty even when discovery had found something.
            .filter(|entry| matches!(entry.kind, EntryKind::Directory | EntryKind::Shortcut))
            .map(|entry| {
                SidebarNode::remote(
                    entry.location.clone(),
                    entry.display_name.clone(),
                    if entry.kind == EntryKind::Shortcut {
                        "network-server-symbolic"
                    } else {
                        "folder-symbolic"
                    },
                    true,
                )
            })
            .collect::<Vec<_>>();
        children.sort_by(|left, right| left.location.label.cmp(&right.location.label));
        children
    }

    fn quick_access(&self) -> SidebarSection {
        let nodes = self
            .preferences
            .pins()
            .iter()
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
            mountables: Vec::new(),
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
            mountables: Vec::new(),
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
            mountables: self.mountable_volumes(),
        }
    }

    /// Attached devices with nothing mounted. Without these a stick that has not
    /// been mounted -- because auto-mounting is off, or it was ejected while left
    /// plugged in -- is absent from the sidebar entirely, with no way to mount it.
    fn mountable_volumes(&self) -> Vec<MountableVolume> {
        let mut volumes = gio::VolumeMonitor::get()
            .volumes()
            .into_iter()
            .filter(|volume| volume.get_mount().is_none() && volume.can_mount())
            .filter_map(|volume| {
                // Network volumes reach the sidebar through `computer:///` under
                // Network & online already; repeating them here would show one
                // account twice.
                if volume
                    .identifier("class")
                    .is_some_and(|class| class.as_str() == "network")
                {
                    return None;
                }
                let icon_name = themed_icon_name(volume.icon())
                    .unwrap_or_else(|| "drive-removable-media-symbolic".to_owned());
                Some(MountableVolume {
                    id: volume_identity(&volume),
                    label: volume.name().to_string(),
                    icon_name,
                })
            })
            .collect::<Vec<_>>();
        volumes.sort_by(|left, right| {
            device_kind_rank(&left.icon_name)
                .cmp(&device_kind_rank(&right.icon_name))
                .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
        });
        volumes
    }

    /// The volume behind a `MountableVolume`, or `None` once it has gone away.
    pub fn volume_for_id(&self, id: &str) -> Option<gio::Volume> {
        gio::VolumeMonitor::get()
            .volumes()
            .into_iter()
            .find(|volume| volume_identity(volume) == id)
    }

    fn network_and_online(&self) -> SidebarSection {
        let mut nodes = vec![
            SidebarNode::remote(
                Location::from_uri("network:///").unwrap(),
                "Browse Network",
                "network-workgroup-symbolic",
                true,
            ),
            SidebarNode::remote(
                Location::from_uri("computer:///").unwrap(),
                "Online accounts",
                "folder-remote-symbolic",
                true,
            ),
        ];
        nodes.extend(self.preferences.remote_pins.iter().map(|location| {
            SidebarNode::remote(
                location.clone(),
                remote_label(location.uri()),
                "folder-remote-symbolic",
                true,
            )
        }));
        nodes.extend(self.remote_mounts());
        SidebarSection {
            mountables: Vec::new(),
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
                let class = mount.volume().and_then(|volume| volume.identifier("class"));
                if !lists_as_its_own_remote(mount.is_shadowed(), class.as_ref().map(|c| c.as_str()))
                {
                    return None;
                }
                let location = Location::try_from_file(root.clone()).ok()?;
                (!is_local_mount_uri(&root.uri())).then(|| {
                    SidebarNode::remote(
                        location,
                        mount.name(),
                        mount_icon_name(&mount, "folder-remote-symbolic"),
                        true,
                    )
                })
            })
            .collect()
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

/// Whether a `computer:///` child is an online account rather than a disk.
///
/// `computer:///` lists physical drives, the filesystem root and provider
/// accounts alike, all of them `Mountable` with no content type, so the only
/// thing GIO distinguishes them by is the `.volume` / `.drive` / `.link` suffix
/// on the entry name. Showing the lot turns Online accounts into a second,
/// worse copy of Devices.
///
/// The suffix survives in the entry's URI only while the entry has no target to
/// resolve to, so both forms are checked: a mounted account resolves to its own
/// scheme (`onedrive://…`), while an unmounted one stays `computer:///….volume`.
/// A drive resolves to `file://…`, or keeps a `.drive` suffix when it has
/// nothing mounted.
fn is_online_provider(entry: &crate::core::contracts::EntrySnapshot) -> bool {
    let uri = entry.location.uri();
    if uri.starts_with("file:") {
        return false;
    }
    if uri.starts_with("computer:") {
        return uri.ends_with(".volume");
    }
    true
}

/// A volume's identity, stable enough to find it again between renders. GIO
/// supplies a UUID for local partitions and a URI for provider volumes; name is
/// the last resort, and duplicates there only mean the first match wins.
fn volume_identity(volume: &gio::Volume) -> String {
    volume
        .uuid()
        .or_else(|| volume.identifier("unix-device"))
        .map(|id| id.to_string())
        .unwrap_or_else(|| volume.name().to_string())
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

/// Whether a non-local mount earns a row of its own under Network & online.
///
/// GIO reports one online account as two mounts: the `GDaemonMount` the backend
/// actually created, and a shadow mount that the provider's volume monitor puts
/// in front of it to carry the account name. The real one answers true to
/// `is_shadowed`, and listing it shows the account a second time under its
/// internal id -- `0123456789abcdef` rather than `alex@example.com`.
///
/// Dropping it leaves the shadow mount, which is still a duplicate: its volume
/// is one of the network-class volumes `computer:///` already lists under
/// Online accounts, the same reason `mountable_volumes` skips that class. A
/// mount with no volume -- a share mounted by hand, `gio mount smb://...` --
/// belongs to no other row, so it stays.
fn lists_as_its_own_remote(shadowed: bool, volume_class: Option<&str>) -> bool {
    !shadowed && volume_class != Some("network")
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

#[cfg(test)]
mod tests {

    #[test]
    fn online_accounts_lists_providers_not_disks() {
        // `computer:///` lists drives, the filesystem root and accounts alike,
        // every one of them Mountable with no content type. Without the suffix
        // filter Online accounts becomes a second copy of Devices. These are the
        // entries a real machine reports, after target-uri resolution.
        use crate::core::contracts::{
            CapabilitySnapshot, DirectorySnapshot, EntryKind, EntrySnapshot,
        };
        let root = crate::core::Location::from_uri("computer:///").unwrap();
        let entry = |uri: &str, name: &str| EntrySnapshot {
            location: crate::core::Location::from_uri(uri).unwrap(),
            display_name: name.into(),
            kind: EntryKind::Directory,
            size: None,
            modified: None,
            content_type: None,
            capabilities: CapabilitySnapshot::default(),
        };
        let snapshot = DirectorySnapshot {
            location: crate::core::ResolvedLocation {
                requested: root.clone(),
                resolved: root,
            },
            entries: vec![
                entry("file:///media/user/Storage2", "CT1000P1SSD8: Storage"),
                entry("computer:///CT1000P510SSD8.drive", "CT1000P510SSD8"),
                entry(
                    "file:///media/user/writable",
                    "USB SanDisk 3.2Gen1: writable",
                ),
                entry("onedrive://0123456789abcdef/", "alex@example.com"),
                entry(
                    "computer:///someone%40example.com.volume",
                    "someone@example.com",
                ),
                entry("file:///", "File System"),
            ],
            capabilities: CapabilitySnapshot::default(),
            next_page: None,
        };

        let children = super::SidebarModel::children_from_snapshot(&snapshot);
        let labels: Vec<&str> = children
            .iter()
            .map(|child| child.location.label())
            .collect();
        assert_eq!(
            labels,
            vec!["alex@example.com", "someone@example.com"],
            "only provider accounts belong here, mounted or not"
        );
    }

    #[test]
    fn an_account_is_listed_once_rather_than_once_per_gio_mount() {
        // One OneDrive account, as `gio mount -l` reports it on the machine that
        // showed three rows for it: a network-class volume whose shadow mount
        // carries the account name, and behind it the daemon mount named after
        // the account's internal id. The account's row is the one `computer:///`
        // already provides under Online accounts, so neither mount adds a row.
        assert!(
            !super::lists_as_its_own_remote(true, None),
            "the shadowed daemon mount would appear as 0123456789abcdef"
        );
        assert!(
            !super::lists_as_its_own_remote(false, Some("network")),
            "the shadow mount repeats the account listed under Online accounts"
        );
        // A share mounted by hand has no volume behind it, so nothing else lists
        // it and Network & online is the only place it can appear.
        assert!(super::lists_as_its_own_remote(false, None));
        assert!(super::lists_as_its_own_remote(false, Some("device")));
    }

    #[test]
    fn network_shortcuts_become_sidebar_children() {
        // Everything `network:///` lists is a shortcut to a server, not a
        // directory, so filtering to directories left Browse Network empty even
        // when discovery had found something.
        use crate::core::contracts::{
            CapabilitySnapshot, DirectorySnapshot, EntryKind, EntrySnapshot,
        };
        let root = crate::core::Location::from_uri("network:///").unwrap();
        let snapshot = DirectorySnapshot {
            location: crate::core::ResolvedLocation {
                requested: root.clone(),
                resolved: root,
            },
            entries: vec![
                EntrySnapshot {
                    location: crate::core::Location::from_uri("smb://octavia.local/").unwrap(),
                    display_name: "Octavia".into(),
                    kind: EntryKind::Shortcut,
                    size: None,
                    modified: None,
                    content_type: None,
                    capabilities: CapabilitySnapshot::default(),
                },
                EntrySnapshot {
                    location: crate::core::Location::from_uri("smb://other/share").unwrap(),
                    display_name: "A share".into(),
                    kind: EntryKind::Directory,
                    size: None,
                    modified: None,
                    content_type: None,
                    capabilities: CapabilitySnapshot::default(),
                },
            ],
            capabilities: CapabilitySnapshot::default(),
            next_page: None,
        };

        let children = super::SidebarModel::children_from_snapshot(&snapshot);
        let labels: Vec<&str> = children.iter().map(|c| c.location.label()).collect();
        assert_eq!(labels, vec!["A share", "Octavia"]);
    }

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
        )
        .unwrap();
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

        let mut preferences = SidebarPreferences::load_from(path.clone()).unwrap();
        assert_eq!(preferences.pins(), [link.clone()]);
        assert!(preferences.is_pinned(&link));
        preferences.unpin(&link);
        assert!(preferences.pins().is_empty());
        assert_eq!(preferences.remote_pins(), ["google-drive://account/root"]);
        assert_eq!(
            preferences.recoverable_notices(),
            [crate::core::RecoverableStateNotice::MalformedRecord]
        );
        preferences.save().unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(
            saved["locations"],
            serde_json::json!(["google-drive://account/root"])
        );
    }
}
