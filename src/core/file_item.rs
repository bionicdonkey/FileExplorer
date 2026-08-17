use std::fs::{DirEntry, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gio::prelude::*;

/// Metadata needed to render one row/tile in a directory view.
#[derive(Clone, Debug)]
pub struct FileItem {
    path: PathBuf,
    name: String,
    is_dir: bool,
    size: u64,
    modified: SystemTime,
    content_type: String,
    uri: Option<String>,
}

impl FileItem {
    pub fn from_dir_entry(entry: &DirEntry) -> io::Result<Self> {
        // DirEntry::metadata() describes the link itself. fs::metadata follows
        // it, which lets a symbolic link to a folder behave like a folder.
        // Keep the link metadata as a fallback so broken links are still shown.
        let metadata = std::fs::metadata(entry.path()).or_else(|_| entry.metadata())?;
        Ok(Self::from_metadata(
            entry.path(),
            entry.file_name().to_string_lossy().into_owned(),
            metadata,
        ))
    }

    fn from_metadata(path: PathBuf, name: String, metadata: Metadata) -> Self {
        let content_type = content_type_for(&path);
        Self {
            path,
            name,
            is_dir: metadata.is_dir(),
            size: metadata.len(),
            modified: metadata.modified().unwrap_or(UNIX_EPOCH),
            content_type,
            uri: None,
        }
    }

    pub fn from_gio(parent: &gio::File, info: &gio::FileInfo) -> Self {
        let name = info.display_name().to_string();
        let child = parent.resolve_relative_path(info.name());
        let content_type = info.content_type().map(|value| value.to_string()).unwrap_or_else(|| "application/octet-stream".into());
        let modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(info.attribute_uint64("time::modified"));
        Self {
            path: PathBuf::new(), name,
            is_dir: matches!(info.file_type(), gio::FileType::Directory | gio::FileType::Mountable),
            size: info.size() as u64,
            modified,
            content_type,
            uri: Some(child.uri().to_string()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
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

    pub fn uri(&self) -> Option<&str> { self.uri.as_deref() }

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

    use super::{format_size, FileItem};

    #[test]
    fn formats_human_readable_sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(1024 * 1024 * 20), "20 MB");
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

        let entry = fs::read_dir(directory.path()).unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name() == "linked-folder")
            .expect("symbolic link should be visible in its parent directory");

        assert!(FileItem::from_dir_entry(&entry).unwrap().is_dir());
    }
}
