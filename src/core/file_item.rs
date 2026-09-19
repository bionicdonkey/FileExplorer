use std::fs::{DirEntry, Metadata};
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use gio::prelude::*;

use super::{
    contracts::{Capability, CapabilitySnapshot},
    Location,
};

/// Metadata needed to render one row/tile in a directory view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileItem {
    location: Location,
    identity_key: String,
    capabilities: CapabilitySnapshot,
    name: String,
    is_dir: bool,
    size: u64,
    modified: SystemTime,
    content_type: String,
}

impl FileItem {
    /// Projects owned repository metadata into a rendered row without I/O.
    pub fn from_snapshot(entry: &super::contracts::EntrySnapshot) -> Self {
        Self {
            location: entry.location.clone(),
            identity_key: entry.location.uri().to_owned(),
            capabilities: entry.capabilities.clone(),
            name: entry.display_name.clone(),
            is_dir: matches!(
                entry.kind,
                super::contracts::EntryKind::Directory | super::contracts::EntryKind::Shortcut
            ),
            size: entry.size.unwrap_or(0),
            modified: entry.modified.unwrap_or(UNIX_EPOCH),
            content_type: entry
                .content_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".into()),
        }
    }
    pub fn from_dir_entry(entry: &DirEntry) -> io::Result<Self> {
        // DirEntry::metadata() describes the link itself. fs::metadata follows
        // it, which lets a symbolic link to a folder behave like a folder.
        // Keep the link metadata as a fallback so broken links are still shown.
        let metadata = std::fs::metadata(entry.path()).or_else(|_| entry.metadata())?;
        Ok(Self::from_metadata(
            Location::from_local_path(&entry.path()),
            entry.file_name().to_string_lossy().into_owned(),
            metadata,
        ))
    }

    /// Builds an item for a search result path, which may live outside the
    /// folder currently on screen.
    pub fn from_path(path: &Path) -> io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Ok(Self::from_metadata(
            Location::from_local_path(path),
            name,
            metadata,
        ))
    }

    fn from_metadata(location: Location, name: String, metadata: Metadata) -> Self {
        let content_type = location
            .local_path()
            .map(|path| content_type_for(&path))
            .unwrap_or_else(|| "application/octet-stream".into());
        Self {
            identity_key: location.uri().to_owned(),
            location,
            capabilities: local_capabilities(),
            name,
            is_dir: metadata.is_dir(),
            size: metadata.len(),
            modified: metadata.modified().unwrap_or(UNIX_EPOCH),
            content_type,
        }
    }

    pub fn from_gio(parent: &gio::File, info: &gio::FileInfo) -> Self {
        let entry_name = info
            .has_attribute("standard::name")
            .then(|| info.name())
            .unwrap_or_default();
        let file_type = info
            .has_attribute("standard::type")
            .then(|| info.file_type())
            .unwrap_or(gio::FileType::Unknown);
        let name = info
            .has_attribute("standard::display-name")
            .then(|| info.display_name().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| entry_name.to_string_lossy().into_owned());
        // GIO names are native filename bytes. Only display labels may be lossy;
        // constructing a child from a UTF-8 replacement string changes identity.
        let child = parent.child(&entry_name);
        let child_uri = info
            .attribute_string("standard::target-uri")
            .map(|uri| uri.to_string())
            .unwrap_or_else(|| child.uri().to_string());
        // A listing must remain renderable if optional provider metadata is
        // absent or malformed. The validated parent is the safe fallback;
        // unsupported capabilities stay unavailable rather than making this
        // entry disappear.
        let location = Location::from_uri(&child_uri)
            .or_else(|_| Location::try_from_file(child))
            .unwrap_or_else(|_| Location::home());
        let content_type = gio_content_type(info);
        let modified = gio_modified(info);
        Self {
            identity_key: location.uri().to_owned(),
            location,
            capabilities: gio_capabilities(info, file_type),
            name,
            is_dir: matches!(
                file_type,
                gio::FileType::Directory | gio::FileType::Mountable | gio::FileType::Shortcut
            ),
            size: gio_size(info),
            modified,
            content_type,
        }
    }

    pub fn location(&self) -> &Location {
        &self.location
    }

    /// Canonical URI identity used for selection, operations, and drag/drop.
    pub fn identity_key(&self) -> &str {
        &self.identity_key
    }

    pub fn capabilities(&self) -> &CapabilitySnapshot {
        &self.capabilities
    }

    /// A local execution adapter may opt into this representation. Remote
    /// items have no synthetic local path.
    pub fn local_path(&self) -> Option<std::path::PathBuf> {
        self.location.local_path()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_dir(&self) -> bool {
        self.is_dir
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn modified(&self) -> SystemTime {
        self.modified
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn uri(&self) -> &str {
        self.location.uri()
    }

    pub fn type_name(&self) -> String {
        if self.is_dir {
            "Folder".into()
        } else {
            gio::content_type_get_description(&self.content_type).to_string()
        }
    }

    pub fn is_visual_media(&self) -> bool {
        self.content_type.starts_with("image/") || self.content_type.starts_with("video/")
    }

    /// Formats a byte count with binary units for compact display.
    pub fn display_size(&self) -> String {
        format_size(self.size)
    }
}

fn local_capabilities() -> CapabilitySnapshot {
    let mut snapshot = CapabilitySnapshot::default();
    snapshot.supported.extend([
        Capability::List,
        Capability::Copy,
        Capability::Move,
        Capability::Create,
        Capability::Rename,
        Capability::Trash,
        Capability::Delete,
        Capability::Open,
        Capability::SearchFilename,
    ]);
    snapshot
}

fn gio_capabilities(info: &gio::FileInfo, file_type: gio::FileType) -> CapabilitySnapshot {
    let mut snapshot = CapabilitySnapshot::default();
    // The ability to display a listing entry is enough to attempt Open. Other
    // operations require explicit provider metadata; an omitted attribute is
    // unknown/unavailable, never an invalid FileItem.
    snapshot.supported.insert(Capability::Open);
    if info.has_attribute("access::can-read") && info.boolean("access::can-read") {
        snapshot.supported.insert(Capability::Copy);
    }
    if info.has_attribute("access::can-write") && info.boolean("access::can-write") {
        snapshot
            .supported
            .extend([Capability::Move, Capability::Rename]);
        if file_type == gio::FileType::Directory {
            snapshot.supported.insert(Capability::Create);
        }
    }
    if info.has_attribute("access::can-trash") && info.boolean("access::can-trash") {
        snapshot.supported.insert(Capability::Trash);
    }
    if info.has_attribute("access::can-delete") && info.boolean("access::can-delete") {
        snapshot.supported.insert(Capability::Delete);
    }
    snapshot
}

fn gio_size(info: &gio::FileInfo) -> u64 {
    info.has_attribute("standard::size")
        .then(|| info.size() as u64)
        .unwrap_or(0)
}

fn gio_modified(info: &gio::FileInfo) -> SystemTime {
    let seconds = info
        .has_attribute("time::modified")
        .then(|| info.attribute_uint64("time::modified"))
        .unwrap_or(0);
    SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds)
}

fn gio_content_type(info: &gio::FileInfo) -> String {
    info.has_attribute("standard::content-type")
        .then(|| info.content_type().map(|value| value.to_string()))
        .flatten()
        .unwrap_or_else(|| "application/octet-stream".into())
}

fn content_type_for(path: &Path) -> String {
    let (content_type, _) = gio::content_type_guess(Some(path), &[]);
    content_type.to_string()
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{format_size, gio_content_type, gio_size, FileItem};

    #[test]
    fn formats_human_readable_sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(1024 * 1024 * 20), "20 MB");
    }

    #[test]
    fn missing_gio_metadata_uses_safe_defaults() {
        let info = gio::FileInfo::new();

        assert_eq!(gio_size(&info), 0);
        assert_eq!(gio_content_type(&info), "application/octet-stream");
    }

    #[test]
    fn gio_network_shortcuts_are_openable_remote_locations() {
        let parent = gio::File::for_uri("network:///");
        let info = gio::FileInfo::new();
        info.set_name("dnssd-server-Octavia._smb._tcp");
        info.set_display_name("Octavia");
        info.set_file_type(gio::FileType::Shortcut);
        info.set_attribute_string("standard::target-uri", "smb://octavia.local/");

        let item = FileItem::from_gio(&parent, &info);

        assert!(item.is_dir());
        assert_eq!(item.uri(), "smb://octavia.local/");
    }

    #[test]
    fn incomplete_gio_metadata_keeps_a_safe_uri_item() {
        let parent = gio::File::for_uri("smb://host/share");
        let info = gio::FileInfo::new();
        info.set_name("report.txt");

        let item = FileItem::from_gio(&parent, &info);

        assert_eq!(item.identity_key(), "smb://host/share/report.txt");
        assert_eq!(item.content_type(), "application/octet-stream");
        assert!(!item
            .capabilities()
            .supports(crate::core::contracts::Capability::Trash));
    }

    #[test]
    fn from_path_reads_the_file_named_at_that_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.txt");
        fs::write(&path, "contents").unwrap();

        let item = FileItem::from_path(&path).unwrap();

        assert_eq!(item.name(), "report.txt");
        assert!(!item.is_dir());
        assert_eq!(item.size(), 8);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_a_directory_is_classified_as_a_folder() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let link = directory.path().join("linked-folder");
        fs::create_dir(&target).unwrap();
        symlink(&target, &link).unwrap();

        let entry = fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name() == "linked-folder")
            .expect("symbolic link should be visible in its parent directory");

        assert!(FileItem::from_dir_entry(&entry).unwrap().is_dir());
    }
}
