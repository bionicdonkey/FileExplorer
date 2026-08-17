use std::fs::{DirEntry, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Metadata needed to render one row/tile in a directory view.
#[derive(Clone, Debug)]
pub struct FileItem {
    path: PathBuf,
    name: String,
    is_dir: bool,
    size: u64,
    modified: SystemTime,
    content_type: String,
}

impl FileItem {
    pub fn from_dir_entry(entry: &DirEntry) -> io::Result<Self> {
        let metadata = entry.metadata()?;
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

fn format_size(bytes: u64) -> String {
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
    use super::format_size;

    #[test]
    fn formats_human_readable_sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(1024 * 1024 * 20), "20 MB");
    }
}
