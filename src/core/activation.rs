//! What opening an entry does.
//!
//! Activation used to be a single question -- directory or not -- with
//! everything that was not a directory handed to the desktop. An archive then
//! left the window and came back in whatever the system has registered for
//! `application/zip`, which on a GNOME desktop is another file manager. The
//! answer belongs here rather than in the view, because it is a decision about
//! what the application can do with a file, and it has to be the same decision
//! wherever an entry is opened from.

use crate::core::file_item::FileItem;
use crate::core::Location;

/// What the application does when an entry is opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activation {
    /// Show this location in the window, the way a folder is shown.
    Browse(Location),
    /// Hand the file to whatever the desktop has registered for its type.
    LaunchExternally,
}

/// Decides what opening `item` should do.
pub fn activation_for(item: &FileItem) -> Activation {
    if item.is_dir() {
        return Activation::Browse(item.location().clone());
    }
    if is_browsable_archive(&resolved_content_type(item)) {
        if let Ok(contents) = item.location().archive_contents() {
            return Activation::Browse(contents);
        }
    }
    Activation::LaunchExternally
}

/// The entry's content type, named by the entry itself when GIO has not been
/// asked for it.
///
/// Content types are fetched for visible rows only, so an entry that has
/// scrolled into view a moment ago may carry a placeholder. Guessing from the
/// name costs nothing and gives the same answer for every archive that has an
/// extension -- and an archive without one is a file the desktop should handle
/// anyway, because nothing on the system would recognise it either.
fn resolved_content_type(item: &FileItem) -> String {
    if item.content_type_resolved() {
        return item.content_type().to_owned();
    }
    gio::content_type_guess(Some(item.name()), &[])
        .0
        .to_string()
}

/// Whether `gvfsd-archive` should be asked to open this as a folder.
///
/// Matched exactly, never with `g_content_type_is_a`: shared-mime-info makes
/// every zip-based document a subtype of `application/zip`, so a subtype test
/// would send `.docx`, `.odt`, `.epub` and `.apk` to the archive backend
/// instead of to the applications that own them. Single-file compression
/// (`.gz`, `.xz` on their own) is left out for the same reason -- there is no
/// folder inside one, only the file it wraps.
fn is_browsable_archive(content_type: &str) -> bool {
    matches!(
        content_type,
        "application/zip"
            | "application/x-zip-compressed"
            | "application/x-7z-compressed"
            | "application/vnd.rar"
            | "application/x-rar"
            | "application/x-rar-compressed"
            | "application/x-tar"
            | "application/x-compressed-tar"
            | "application/x-bzip-compressed-tar"
            | "application/x-bzip2-compressed-tar"
            | "application/x-xz-compressed-tar"
            | "application/x-lzma-compressed-tar"
            | "application/x-zstd-compressed-tar"
            | "application/x-tarz"
            | "application/x-cpio"
            | "application/x-xar"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::contracts::{CapabilitySnapshot, EntryKind, EntrySnapshot};

    fn entry(name: &str, kind: EntryKind, content_type: Option<&str>) -> FileItem {
        FileItem::from_snapshot(&EntrySnapshot {
            location: Location::from_uri(&format!("file:///home/user/{name}")).unwrap(),
            display_name: name.to_owned(),
            kind,
            size: None,
            modified: None,
            content_type: content_type.map(ToOwned::to_owned),
            capabilities: CapabilitySnapshot::default(),
        })
    }

    #[test]
    fn an_archive_opens_in_the_window_rather_than_in_another_application() {
        let archive = entry("holiday.zip", EntryKind::File, Some("application/zip"));
        let Activation::Browse(location) = activation_for(&archive) else {
            panic!("a zip must be browsable, not handed to the desktop");
        };
        assert!(location.is_inside_archive());
        assert_eq!(location.display_name(), "holiday.zip");
    }

    #[test]
    fn an_archive_is_recognised_before_its_content_type_arrives() {
        // Content types are fetched for visible rows, so a row opened straight
        // after it appears has none yet.
        let pending = entry("holiday.tar.gz", EntryKind::File, None);
        assert!(!pending.content_type_resolved());
        assert!(matches!(activation_for(&pending), Activation::Browse(_)));
    }

    #[test]
    fn a_zip_based_document_belongs_to_its_own_application() {
        // Every one of these is a zip underneath, and `g_content_type_is_a`
        // says so. Opening a report in a file manager instead of a word
        // processor is the failure this guards.
        for name in ["report.docx", "notes.odt", "book.epub", "app.apk"] {
            let document = entry(name, EntryKind::File, None);
            assert_eq!(
                activation_for(&document),
                Activation::LaunchExternally,
                "{name} must open in the application that owns it"
            );
        }
    }

    #[test]
    fn a_folder_still_opens_as_a_folder_and_a_plain_file_still_leaves() {
        let folder = entry("Pictures", EntryKind::Directory, None);
        assert_eq!(
            activation_for(&folder),
            Activation::Browse(folder.location().clone())
        );
        let text = entry("notes.txt", EntryKind::File, Some("text/plain"));
        assert_eq!(activation_for(&text), Activation::LaunchExternally);
    }
}
