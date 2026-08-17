use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PermissionBits {
    pub owner_read: bool,
    pub owner_write: bool,
    pub owner_execute: bool,
    pub group_read: bool,
    pub group_write: bool,
    pub group_execute: bool,
    pub other_read: bool,
    pub other_write: bool,
    pub other_execute: bool,
}

impl PermissionBits {
    pub fn from_mode(mode: u32) -> Self {
        Self {
            owner_read: mode & 0o400 != 0, owner_write: mode & 0o200 != 0, owner_execute: mode & 0o100 != 0,
            group_read: mode & 0o040 != 0, group_write: mode & 0o020 != 0, group_execute: mode & 0o010 != 0,
            other_read: mode & 0o004 != 0, other_write: mode & 0o002 != 0, other_execute: mode & 0o001 != 0,
        }
    }

    pub fn to_mode(self) -> u32 {
        (self.owner_read as u32 * 0o400) | (self.owner_write as u32 * 0o200) | (self.owner_execute as u32 * 0o100)
            | (self.group_read as u32 * 0o040) | (self.group_write as u32 * 0o020) | (self.group_execute as u32 * 0o010)
            | (self.other_read as u32 * 0o004) | (self.other_write as u32 * 0o002) | (self.other_execute as u32 * 0o001)
    }
}

#[derive(Clone, Debug)]
pub struct PropertyDetails {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub content_type: String,
    pub size: u64,
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub permissions: PermissionBits,
}

impl PropertyDetails {
    pub fn load(path: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(path)?;
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        #[cfg(unix)]
        let mode = metadata.permissions().mode();
        #[cfg(not(unix))]
        let mode = 0o644;
        let (content_type, _) = gio::content_type_guess(Some(path), &[]);
        Ok(Self {
            path: path.to_path_buf(),
            name: path.file_name().and_then(|name| name.to_str()).unwrap_or("/").to_owned(),
            is_dir: metadata.is_dir(), content_type: content_type.to_string(), size: metadata.len(),
            created: metadata.created().ok(), modified: metadata.modified().ok(), accessed: metadata.accessed().ok(),
            permissions: PermissionBits::from_mode(mode),
        })
    }
}
