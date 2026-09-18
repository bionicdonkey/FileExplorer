use std::fs;
use std::io;
use std::cmp::Ordering;
use std::path::Path;
use std::time::{Duration, SystemTime};

use gio::prelude::*;

use super::file_item::FileItem;
use super::view_options::{GroupField, SortDirection, SortField, ViewOptions};

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

        sort_items(&mut items, &ViewOptions::default());
        Ok(items)
    }

    pub fn load_with_options(path: &Path, options: &ViewOptions) -> io::Result<Vec<FileItem>> {
        let mut items = Self::load_unsorted(path, options)?;
        sort_items(&mut items, options);
        Ok(items)
    }

    pub fn load_uri_with_options(uri: &str, options: &ViewOptions) -> io::Result<Vec<FileItem>> {
        let file = gio::File::for_uri(uri);
        let enumerator = file.enumerate_children(
            "standard::name,standard::display-name,standard::type,standard::icon,standard::content-type,standard::size,time::modified,standard::target-uri,access::can-read,access::can-write,access::can-trash,access::can-delete",
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        ).map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
        let mut items = Vec::new();
        while let Some(info) = enumerator.next_file(gio::Cancellable::NONE).map_err(|error| io::Error::new(io::ErrorKind::Other, error))? {
            if options.show_hidden || !info.name().to_string_lossy().starts_with('.') { items.push(FileItem::from_gio(&file, &info)); }
        }
        sort_items(&mut items, options);
        Ok(items)
    }

    fn load_unsorted(path: &Path, options: &ViewOptions) -> io::Result<Vec<FileItem>> {
        let mut items = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if !options.show_hidden && entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            items.push(FileItem::from_dir_entry(&entry)?);
        }
        Ok(items)
    }
}

pub fn group_label(item: &FileItem, field: GroupField, now: SystemTime) -> Option<String> {
    match field {
        GroupField::None => None,
        GroupField::Type => Some(if item.is_dir() { "Folders".into() } else { item.type_name() }),
        GroupField::Size => Some(match item.size() {
            0 => "Empty".into(),
            1..=1_048_575 => "Small".into(),
            1_048_576..=104_857_599 => "Medium".into(),
            104_857_600..=1_073_741_823 => "Large".into(),
            _ => "Very large".into(),
        }),
        GroupField::DateModified => Some(date_group(item.modified(), now).into()),
    }
}

pub fn sort_items(items: &mut [FileItem], options: &ViewOptions) {
    items.sort_by(|left, right| {
        let folders = if options.folders_first { right.is_dir().cmp(&left.is_dir()) } else { Ordering::Equal };
        if folders != Ordering::Equal {
            return folders;
        }
        let order = match options.sort_field {
            SortField::Name => sort_name(left, right),
            SortField::DateModified => left.modified().cmp(&right.modified()).then_with(|| sort_name(left, right)),
            SortField::Type => left.content_type().cmp(right.content_type()).then_with(|| sort_name(left, right)),
            SortField::Size => left.size().cmp(&right.size()).then_with(|| sort_name(left, right)),
        };
        match options.sort_direction {
            SortDirection::Ascending => order,
            SortDirection::Descending => order.reverse(),
        }
    });
}

fn sort_name(left: &FileItem, right: &FileItem) -> Ordering {
    left.name().to_lowercase().cmp(&right.name().to_lowercase()).then_with(|| left.name().cmp(right.name()))
}

fn date_group(modified: SystemTime, now: SystemTime) -> &'static str {
    let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
    match age {
        age if age < Duration::from_secs(24 * 60 * 60) => "Today",
        age if age < Duration::from_secs(48 * 60 * 60) => "Yesterday",
        age if age < Duration::from_secs(7 * 24 * 60 * 60) => "This week",
        age if age < Duration::from_secs(14 * 24 * 60 * 60) => "Last week",
        age if age < Duration::from_secs(31 * 24 * 60 * 60) => "Earlier this month",
        age if age < Duration::from_secs(366 * 24 * 60 * 60) => "Earlier this year",
        _ => "Older",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::SystemTime;

    use gio::prelude::FileExt;
    use tempfile::tempdir;

    use super::{group_label, DirectoryModel};
    use crate::core::{GroupField, SortField, ViewOptions};

    #[test]
    fn folders_first_sort_keeps_folders_before_larger_files() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("folder")).unwrap();
        fs::write(directory.path().join("large-file"), vec![0_u8; 4096]).unwrap();
        let options = ViewOptions { sort_field: SortField::Size, folders_first: true, ..ViewOptions::default() };

        let items = DirectoryModel::load_with_options(directory.path(), &options).unwrap();

        assert!(items[0].is_dir());
    }

    #[test]
    fn date_group_uses_today_for_an_item_modified_today() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("today.txt"), "today").unwrap();
        let item = DirectoryModel::load(directory.path()).unwrap().pop().unwrap();

        assert_eq!(group_label(&item, GroupField::DateModified, SystemTime::now()), Some("Today".into()));
    }

    #[test]
    fn gio_listing_capabilities_reach_the_file_item() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("readable.txt"), "contents").unwrap();
        let uri = gio::File::for_path(directory.path()).uri();

        let items = DirectoryModel::load_uri_with_options(&uri, &ViewOptions::default()).unwrap();
        let item = items.iter().find(|item| item.name() == "readable.txt").unwrap();

        assert!(item
            .capabilities()
            .supports(crate::core::contracts::Capability::Copy));
    }
}
