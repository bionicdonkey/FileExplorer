//! Extracting an archive into a folder beside it.
//!
//! gvfs reads an archive as a folder once `gvfsd-archive` has mounted it, so
//! extracting is mounting plus the copy the operation pipeline already
//! performs -- which keeps progress, cancellation and conflict handling the
//! same as any other copy.

use crate::application::commands::named_child;
use crate::core::contracts::MutationKind;
use crate::core::operation_manager::OperationManager;
use crate::core::Location;
use gio::prelude::*;
use std::rc::Rc;

/// The folder an archive extracts into: its own name minus the archive
/// suffixes, so `logs.tar.gz` becomes `logs` rather than `logs.tar`.
pub fn extraction_folder_name(archive: &Location) -> String {
    const SUFFIXES: [&str; 11] = [
        ".zip", ".7z", ".rar", ".tar", ".gz", ".bz2", ".xz", ".zst", ".lzma", ".cpio", ".xar",
    ];
    let name = archive.display_name();
    let mut stem = name.as_str();
    loop {
        let lower = stem.to_ascii_lowercase();
        let Some(shorter) = SUFFIXES
            .iter()
            .find(|suffix| lower.ends_with(*suffix))
            .map(|suffix| &stem[..stem.len() - suffix.len()])
            .filter(|rest| !rest.is_empty())
        else {
            break;
        };
        stem = shorter;
    }
    stem.to_owned()
}

/// A sibling of the archive that does not exist yet: `Trip`, then `Trip (2)`.
pub fn reserve_destination(parent: &Location, base: &str) -> std::io::Result<Location> {
    let mut candidate = named_child(parent, base)?;
    let mut suffix = 2;
    while gio::File::for_uri(candidate.uri()).query_exists(gio::Cancellable::NONE) {
        candidate = named_child(parent, &format!("{base} ({suffix})"))?;
        suffix += 1;
    }
    gio::File::for_uri(candidate.uri())
        .make_directory(gio::Cancellable::NONE)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error.to_string()))?;
    Ok(candidate)
}

/// Mounts `archive` and copies its contents into a new folder beside it.
///
/// Returns the folder the copy was aimed at, or `None` when the archive has no
/// addressable contents or the folder could not be created; `on_failure`
/// reports a mount that gvfs refused.
/// Mounts `archive` and copies its contents into a new folder beside it.
///
/// Returns the folder the copy was aimed at, or `None` when the archive has no
/// addressable contents or the folder could not be created; `on_failure`
/// reports a mount gvfs refused.
pub fn extract(
    operations: &OperationManager,
    archive: &Location,
    parent: &Location,
    on_failure: impl Fn(String) + 'static,
) -> Option<Location> {
    let contents = archive.archive_contents().ok()?;
    let destination = reserve_destination(parent, &extraction_folder_name(archive)).ok()?;
    let file = gio::File::for_uri(contents.uri());
    let cancellable = gio::Cancellable::new();
    // The same question the directory repository asks before choosing how to
    // mount: gvfs exposes an archive root as mountable, everything else needs
    // its enclosing volume mounted first.
    let mountable = file
        .query_info(
            "standard::type",
            gio::FileQueryInfoFlags::NONE,
            Some(&cancellable),
        )
        .is_ok_and(|info| info.file_type() == gio::FileType::Mountable);
    let on_failure = Rc::new(on_failure);
    let submit = {
        let operations = operations.clone();
        let contents = contents.clone();
        let destination = destination.clone();
        Rc::new(move || {
            operations.submit_locations(
                MutationKind::Copy,
                vec![contents.clone()],
                Some(destination.clone()),
                None,
                None,
                false,
            );
        })
    };
    let operation = gio::MountOperation::new();
    if mountable {
        let submit = submit.clone();
        let on_failure = on_failure.clone();
        file.mount_mountable(
            gio::MountMountFlags::NONE,
            Some(&operation),
            Some(&cancellable),
            move |result| match result {
                Ok(_) => submit(),
                // Browsing the archive earlier already mounted it.
                Err(error) if error.matches(gio::IOErrorEnum::AlreadyMounted) => submit(),
                Err(error) => on_failure(error.to_string()),
            },
        );
    } else {
        let submit = submit.clone();
        let on_failure = on_failure.clone();
        file.mount_enclosing_volume(
            gio::MountMountFlags::NONE,
            Some(&operation),
            Some(&cancellable),
            move |result| match result {
                Ok(()) => submit(),
                Err(error) if error.matches(gio::IOErrorEnum::AlreadyMounted) => submit(),
                Err(error) => on_failure(error.to_string()),
            },
        );
    }
    Some(destination)
}

#[cfg(test)]
mod tests {
    use super::{extraction_folder_name, reserve_destination};
    use crate::core::Location;

    fn location(uri: &str) -> Location {
        Location::from_uri(uri).unwrap()
    }

    #[test]
    fn the_folder_name_drops_every_archive_suffix() {
        assert_eq!(
            extraction_folder_name(&location("file:///tmp/Trip.zip")),
            "Trip"
        );
        assert_eq!(
            extraction_folder_name(&location("file:///tmp/logs.tar.gz")),
            "logs"
        );
        assert_eq!(
            extraction_folder_name(&location("file:///tmp/backup.tar.xz")),
            "backup"
        );
        assert_eq!(
            extraction_folder_name(&location("file:///tmp/archive.7z")),
            "archive"
        );
    }

    #[test]
    fn a_name_that_is_only_suffixes_keeps_itself() {
        assert_eq!(extraction_folder_name(&location("file:///tmp/.gz")), ".gz");
        assert_eq!(
            extraction_folder_name(&location("file:///tmp/no-extension")),
            "no-extension"
        );
    }

    #[test]
    fn an_existing_folder_pushes_the_destination_to_a_unique_name() {
        let parent = tempfile::tempdir().unwrap();
        let parent_location = Location::from_local_path(parent.path());
        std::fs::create_dir(parent.path().join("Trip")).unwrap();
        std::fs::create_dir(parent.path().join("Trip (2)")).unwrap();

        let reserved = reserve_destination(&parent_location, "Trip").unwrap();

        assert_eq!(
            reserved.local_path(),
            Some(parent.path().join("Trip (3)")),
            "the third candidate is the first that does not exist"
        );
        assert!(parent.path().join("Trip (3)").is_dir(), "it is created");
    }
}
