use std::fs;
use std::io;
use std::path::Path;

use super::file_item::FileItem;

/// Loads visible entries from a local directory in deterministic display order.
pub struct DirectoryModel;

impl DirectoryModel {
    pub fn load(path: &Path) -> io::Result<Vec<FileItem>> {
        let mut items = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            items.push(FileItem::from_dir_entry(&entry)?);
        }

        items.sort_by(|left, right| {
            left.name()
                .to_lowercase()
                .cmp(&right.name().to_lowercase())
                .then_with(|| left.name().cmp(right.name()))
        });
        Ok(items)
    }
}
