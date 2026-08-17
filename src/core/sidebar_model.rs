use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gio::prelude::*;
use serde::{Deserialize, Serialize};

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
    pub fn from_path(path: PathBuf, label: impl Into<String>, icon_name: impl Into<String>) -> Self {
        Self { kind: SidebarLocationKind::Local(path), label: label.into(), icon_name: icon_name.into() }
    }

    pub fn from_uri(uri: impl Into<String>, label: impl Into<String>, icon_name: impl Into<String>) -> Self {
        Self { kind: SidebarLocationKind::Uri(uri.into()), label: label.into(), icon_name: icon_name.into() }
    }

    pub fn local_path(&self) -> Option<&Path> {
        match &self.kind { SidebarLocationKind::Local(path) => Some(path), SidebarLocationKind::Uri(_) => None }
    }

    pub fn uri(&self) -> String {
        match &self.kind {
            SidebarLocationKind::Local(path) => gio::File::for_path(path).uri().to_string(),
            SidebarLocationKind::Uri(uri) => uri.clone(),
        }
    }

    pub fn label(&self) -> &str { &self.label }
    pub fn icon_name(&self) -> &str { &self.icon_name }
}

#[derive(Clone, Debug)]
pub struct SidebarNode {
    pub location: SidebarLocation,
    pub expandable: bool,
    pub children: Vec<SidebarNode>,
}

impl SidebarNode {
    fn local(path: PathBuf, label: impl Into<String>, icon_name: impl Into<String>, expandable: bool) -> Self {
        Self { location: SidebarLocation::from_path(path, label, icon_name), expandable, children: Vec::new() }
    }

    fn remote(uri: String, label: impl Into<String>, icon_name: impl Into<String>) -> Self {
        Self { location: SidebarLocation::from_uri(uri, label, icon_name), expandable: false, children: Vec::new() }
    }
}

#[derive(Clone, Debug)]
pub struct SidebarSection {
    pub title: &'static str,
    pub nodes: Vec<SidebarNode>,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredPreferences {
    #[serde(default)]
    pins: Vec<PathBuf>,
}

pub struct SidebarPreferences {
    path: Option<PathBuf>,
    pins: Vec<PathBuf>,
}

impl SidebarPreferences {
    pub fn in_memory() -> Self { Self { path: None, pins: Vec::new() } }

    pub fn load_default() -> io::Result<Self> {
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory is unavailable"))?;
        Self::load_from(config_home.join("zorin-explorer/sidebar.json"))
    }

    pub fn load_from(path: PathBuf) -> io::Result<Self> {
        if !path.exists() { return Ok(Self { path: Some(path), pins: Vec::new() }); }
        let stored = serde_json::from_slice::<StoredPreferences>(&fs::read(&path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let mut preferences = Self { path: Some(path), pins: Vec::new() };
        for pin in stored.pins { preferences.pin(pin); }
        Ok(preferences)
    }

    pub fn pins(&self) -> &[PathBuf] { &self.pins }

    pub fn pin(&mut self, path: PathBuf) {
        let path = normalized_path(path);
        if !self.pins.iter().any(|pinned| pinned == &path) { self.pins.push(path); }
    }

    pub fn unpin(&mut self, path: &Path) {
        let path = normalized_path(path.to_path_buf());
        self.pins.retain(|pinned| pinned != &path);
    }

    pub fn is_pinned(&self, path: &Path) -> bool { self.pins.iter().any(|pinned| pinned == &normalized_path(path.to_path_buf())) }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else { return Ok(()); };
        let parent = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "sidebar preference path has no parent"))?;
        fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(&StoredPreferences { pins: self.pins.clone() })
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)
    }
}

pub struct SidebarModel {
    preferences: SidebarPreferences,
    home: PathBuf,
}

impl SidebarModel {
    pub fn new(preferences: SidebarPreferences, home: PathBuf) -> Self { Self { preferences, home } }

    pub fn sections(&self) -> Vec<SidebarSection> {
        vec![self.quick_access(), self.this_pc(), self.network_and_online()]
    }

    pub fn preferences(&self) -> &SidebarPreferences { &self.preferences }
    pub fn preferences_mut(&mut self) -> &mut SidebarPreferences { &mut self.preferences }

    pub fn children_for(&self, path: &Path) -> Vec<SidebarNode> {
        let mut children = match fs::read_dir(path) { Ok(entries) => entries.flatten().filter_map(|entry| {
            let child = entry.path();
            child.is_dir().then(|| SidebarNode::local(child.clone(), entry.file_name().to_string_lossy(), "folder-symbolic", true))
        }).collect::<Vec<_>>(), Err(_) => Vec::new() };
        children.sort_by(|left, right| left.location.label.cmp(&right.location.label));
        children
    }

    fn quick_access(&self) -> SidebarSection {
        let nodes = self.preferences.pins().iter().filter(|path| path.exists()).map(|path| {
            SidebarNode::local(path.clone(), display_name(path), standard_folder_icon(path), true)
        }).collect();
        SidebarSection { title: "Quick access", nodes }
    }

    fn this_pc(&self) -> SidebarSection {
        let mut nodes = vec![SidebarNode::local(self.home.clone(), "Home", "user-home-symbolic", true)];
        for (name, icon) in [("Desktop", "user-desktop-symbolic"), ("Documents", "folder-documents-symbolic"), ("Downloads", "folder-download-symbolic"), ("Music", "folder-music-symbolic"), ("Pictures", "folder-pictures-symbolic"), ("Videos", "folder-videos-symbolic")] {
            let path = self.home.join(name);
            if path.exists() { nodes.push(SidebarNode::local(path, name, icon, true)); }
        }
        nodes.push(SidebarNode::local(PathBuf::from("/"), "File System", "drive-harddisk-symbolic", true));
        nodes.extend(self.local_mounts());
        SidebarSection { title: "This PC", nodes }
    }

    fn network_and_online(&self) -> SidebarSection {
        SidebarSection { title: "Network & online", nodes: self.remote_mounts() }
    }

    fn local_mounts(&self) -> Vec<SidebarNode> {
        gio::VolumeMonitor::get().mounts().into_iter().filter_map(|mount| {
            let root = mount.root();
            let path = root.path()?;
            if path == Path::new("/") || path.starts_with(&self.home) { return None; }
            Some(SidebarNode::local(path, mount.name(), mount_icon_name(&mount, "drive-harddisk-symbolic"), true))
        }).collect()
    }

    fn remote_mounts(&self) -> Vec<SidebarNode> {
        gio::VolumeMonitor::get().mounts().into_iter().filter_map(|mount| {
            let root = mount.root();
            root.path().is_none().then(|| SidebarNode::remote(root.uri().to_string(), mount.name(), mount_icon_name(&mount, "folder-remote-symbolic")))
        }).collect()
    }
}

fn normalized_path(path: PathBuf) -> PathBuf { fs::canonicalize(&path).unwrap_or(path) }

fn display_name(path: &Path) -> String {
    path.file_name().and_then(|name| name.to_str()).filter(|name| !name.is_empty()).map(ToOwned::to_owned).unwrap_or_else(|| path.display().to_string())
}

fn standard_folder_icon(path: &Path) -> &'static str {
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
    mount.icon()
        .downcast::<gio::ThemedIcon>()
        .ok()
        .and_then(|icon| icon.names().into_iter().next())
        .map(|name| name.to_string())
        .unwrap_or_else(|| fallback.to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{SidebarLocation, SidebarPreferences};

    #[test]
    fn pinning_a_path_is_persisted_once() {
        let mut preferences = SidebarPreferences::in_memory();
        preferences.pin(PathBuf::from("/tmp/one"));
        preferences.pin(PathBuf::from("/tmp/one"));

        assert_eq!(preferences.pins(), &[PathBuf::from("/tmp/one")]);
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
        let location = SidebarLocation::from_uri("google-drive://account/root", "Google Drive", "folder-remote-symbolic");
        assert!(location.local_path().is_none());
        assert_eq!(location.uri(), "google-drive://account/root");
    }

    #[test]
    fn local_sidebar_location_provides_a_navigation_path() {
        let location = SidebarLocation::from_path(PathBuf::from("/tmp"), "tmp", "folder-symbolic");
        assert_eq!(location.local_path(), Some(Path::new("/tmp")));
    }
}
