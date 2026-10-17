//! Extracting an archive into a folder beside it.
//!
//! gvfs reads an archive as a folder once `gvfsd-archive` has mounted it, so
//! extracting is mounting plus the copy the operation pipeline already
//! performs -- which keeps progress, cancellation and conflict handling the
//! same as any other copy.

use crate::application::commands::named_child;
use crate::application::compression::{
    missing_tool, run_with_progress, seven_zip_program, ArchiveError, ArchiveFormat, Workspace,
};
use crate::core::contracts::MutationKind;
use crate::core::operation_manager::{OperationKind, OperationManager};
use crate::core::Location;
use gio::prelude::*;
use std::path::Path;
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

/// A child of `parent` that does not exist yet: `Trip`, then `Trip (2)`.
pub fn unique_child(parent: &Location, base: &str) -> std::io::Result<Location> {
    let mut candidate = named_child(parent, base)?;
    let mut suffix = 2;
    while gio::File::for_uri(candidate.uri()).query_exists(gio::Cancellable::NONE) {
        candidate = named_child(parent, &format!("{base} ({suffix})"))?;
        suffix += 1;
    }
    Ok(candidate)
}

/// A new folder beside the archive, named like `unique_child`.
pub fn reserve_destination(parent: &Location, base: &str) -> std::io::Result<Location> {
    let candidate = unique_child(parent, base)?;
    gio::File::for_uri(candidate.uri())
        .make_directory(gio::Cancellable::NONE)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error.to_string()))?;
    Ok(candidate)
}

/// One top-level entry of a mounted archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveEntry {
    pub uri: String,
    pub name: String,
    pub is_dir: bool,
}

/// Where an archive's top-level entries go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    /// The archive is one folder: it lands beside the archive as it is,
    /// rather than inside a second folder of the same name.
    Beside(ArchiveEntry),
    /// Anything else gets a folder of its own, named after the archive.
    Folder,
    Empty,
}

pub fn placement(entries: &[ArchiveEntry]) -> Placement {
    match entries {
        [] => Placement::Empty,
        [only] if only.is_dir => Placement::Beside(only.clone()),
        _ => Placement::Folder,
    }
}

/// The top-level entries of the archive mounted at `uri`. Blocks.
fn list_root(uri: &str) -> Result<Vec<ArchiveEntry>, glib::Error> {
    let root = gio::File::for_uri(uri);
    let enumerator = root.enumerate_children(
        "standard::name,standard::type",
        gio::FileQueryInfoFlags::NONE,
        gio::Cancellable::NONE,
    )?;
    let mut entries = Vec::new();
    while let Some(info) = enumerator.next_file(gio::Cancellable::NONE)? {
        let name = info.name();
        entries.push(ArchiveEntry {
            uri: root.child(&name).uri().to_string(),
            name: name.to_string_lossy().into_owned(),
            is_dir: info.file_type() == gio::FileType::Directory,
        });
    }
    Ok(entries)
}

fn is_seven_zip_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".7z")
}

/// Unpacks a 7-Zip archive in a scratch folder, then moves the result into
/// place the way `placement` describes. Blocks.
fn extract_seven_zip_blocking(
    archive: &Path,
    parent: &Path,
    progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<(), ArchiveError> {
    let program = seven_zip_program().ok_or_else(|| missing_tool(ArchiveFormat::SevenZip))?;
    let workspace = Workspace::create(parent)
        .map_err(|error| format!("Could not write to this folder: {error}"))?;
    let unpacked = workspace.0.join("contents");
    let total = std::fs::metadata(archive).map(|meta| meta.len()).unwrap_or(0);
    let mut command = std::process::Command::new(program);
    let mut output = std::ffi::OsString::from("-o");
    output.push(&unpacked);
    command.args(["x", "-y", "-bd"]).arg(output).arg(archive);
    command.current_dir(parent);
    progress(0, total);
    run_with_progress(&mut command, total, progress, cancelled)?;
    if cancelled() {
        return Err(ArchiveError::Cancelled);
    }
    progress(total, total);

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&unpacked)
        .map_err(|_| "The archive is empty".to_owned())?
        .flatten()
    {
        entries.push(ArchiveEntry {
            uri: Location::from_local_path(&entry.path()).uri().to_owned(),
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir: entry.path().is_dir() && !entry.path().is_symlink(),
        });
    }
    let parent_location = Location::from_local_path(parent);
    let (from, name) = match placement(&entries) {
        Placement::Empty => return Err("The archive is empty".to_owned().into()),
        Placement::Beside(entry) => (unpacked.join(&entry.name), entry.name),
        Placement::Folder => (
            unpacked.clone(),
            extraction_folder_name(&Location::from_local_path(archive)),
        ),
    };
    let target = unique_child(&parent_location, &name)
        .ok()
        .and_then(|location| location.local_path())
        .ok_or_else(|| "Could not name the extracted folder".to_owned())?;
    std::fs::rename(&from, &target)
        .map_err(|error| format!("Could not save the extracted files: {error}"))?;
    Ok(())
}

fn extract_seven_zip(
    operations: &OperationManager,
    archive: &Location,
    parent: &Location,
    on_failure: impl Fn(String) + 'static,
) -> bool {
    let (Some(archive_path), Some(parent_path)) = (archive.local_path(), parent.local_path())
    else {
        return false;
    };
    let job = operations.begin_job(
        OperationKind::Extract,
        vec![archive.clone()],
        Some(parent.clone()),
    );
    glib::MainContext::default().spawn_local(async move {
        let result = gio::spawn_blocking({
            let job = job.clone();
            move || {
                let result = extract_seven_zip_blocking(
                    &archive_path,
                    &parent_path,
                    &mut |done, total| job.set_progress(done, total),
                    &|| job.is_cancelled(),
                );
                job.finish(result.as_ref().map(|_| ()).map_err(ArchiveError::message));
                result
            }
        })
        .await
        .unwrap_or_else(|_| Err(ArchiveError::Failed("The extractor stopped unexpectedly.".into())));
        match result {
            Ok(()) | Err(ArchiveError::Cancelled) => {}
            Err(error) => on_failure(error.message()),
        }
    });
    true
}

/// Mounts `archive` and copies its contents out beside it.
///
/// The work shows in the operations queue from the start: first as the archive
/// being opened -- gvfs reads a compressed archive through to index it, which
/// is the slow part for a large `.tar.gz` -- then as the copy, with its byte
/// progress, once the mount is ready. Cancelling during the first phase stops
/// the mount.
///
/// An archive holding a single folder extracts to that folder beside the
/// archive; anything else goes into a new folder named after the archive.
///
/// Returns whether extraction began, which it does not when the location
/// cannot be addressed as an archive; `on_failure` reports anything that goes
/// wrong afterwards.
pub fn extract(
    operations: &OperationManager,
    archive: &Location,
    parent: &Location,
    on_failure: impl Fn(String) + 'static,
) -> bool {
    if is_seven_zip_name(&archive.display_name()) {
        return extract_seven_zip(operations, archive, parent, on_failure);
    }
    let Ok(contents) = archive.archive_contents() else {
        return false;
    };
    let job = operations.begin_job(
        OperationKind::Extract,
        vec![archive.clone()],
        Some(parent.clone()),
    );
    let file = gio::File::for_uri(contents.uri());
    let cancellable = gio::Cancellable::new();
    // Cancelling the queue row reaches the mount through this.
    {
        let job = job.clone();
        let cancellable = cancellable.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            if job.is_cancelled() {
                cancellable.cancel();
            }
            if job.is_finished() || cancellable.is_cancelled() {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }
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
    let fail: Rc<dyn Fn(String, bool)> = {
        let job = job.clone();
        let on_failure = on_failure.clone();
        Rc::new(move |message: String, cancelled: bool| {
            job.finish(Err(message.clone()));
            if !cancelled {
                on_failure(message);
            }
        })
    };
    let start_copy: Rc<dyn Fn()> = {
        let operations = operations.clone();
        let archive = archive.clone();
        let parent = parent.clone();
        let job = job.clone();
        let fail = fail.clone();
        let root_uri = contents.uri().to_owned();
        Rc::new(move || {
            if job.is_cancelled() {
                fail("Cancelled".to_owned(), true);
                return;
            }
            let operations = operations.clone();
            let archive = archive.clone();
            let parent = parent.clone();
            let job = job.clone();
            let fail = fail.clone();
            let root_uri = root_uri.clone();
            glib::MainContext::default().spawn_local(async move {
                let listing = gio::spawn_blocking(move || list_root(&root_uri)).await;
                let entries = match listing {
                    Ok(Ok(entries)) => entries,
                    Ok(Err(error)) => return fail(error.to_string(), false),
                    Err(_) => return fail("Could not read the archive".to_owned(), false),
                };
                if job.is_cancelled() {
                    return fail("Cancelled".to_owned(), true);
                }
                let submitted = match placement(&entries) {
                    Placement::Empty => return fail("The archive is empty".to_owned(), false),
                    Placement::Beside(entry) => {
                        let target = unique_child(&parent, &entry.name);
                        let source = Location::from_uri(&entry.uri);
                        match (source, target) {
                            (Ok(source), Ok(target)) => Ok(operations.submit_locations(
                                MutationKind::Copy,
                                vec![source],
                                Some(parent.clone()),
                                Some(target),
                                None,
                                false,
                            )),
                            _ => Err("Could not name the extracted folder".to_owned()),
                        }
                    }
                    Placement::Folder => {
                        match reserve_destination(&parent, &extraction_folder_name(&archive)) {
                            Ok(folder) => {
                                let sources: Vec<Location> = entries
                                    .iter()
                                    .filter_map(|entry| Location::from_uri(&entry.uri).ok())
                                    .collect();
                                Ok(operations.submit_locations(
                                    MutationKind::Copy,
                                    sources,
                                    Some(folder),
                                    None,
                                    None,
                                    false,
                                ))
                            }
                            Err(_) => Err("Could not create a folder beside the archive".to_owned()),
                        }
                    }
                };
                match submitted {
                    Ok(id) => job.hand_over(id, OperationKind::Extract),
                    Err(message) => fail(message, false),
                }
            });
        })
    };
    let operation = gio::MountOperation::new();
    let on_mounted = {
        let start_copy = start_copy.clone();
        let fail = fail.clone();
        move |result: Result<(), glib::Error>| match result {
            Ok(()) => start_copy(),
            // Browsing the archive earlier already mounted it.
            Err(error) if error.matches(gio::IOErrorEnum::AlreadyMounted) => start_copy(),
            Err(error) => fail(
                error.to_string(),
                error.matches(gio::IOErrorEnum::Cancelled),
            ),
        }
    };
    if mountable {
        file.mount_mountable(
            gio::MountMountFlags::NONE,
            Some(&operation),
            Some(&cancellable),
            move |result| on_mounted(result.map(|_| ())),
        );
    } else {
        file.mount_enclosing_volume(
            gio::MountMountFlags::NONE,
            Some(&operation),
            Some(&cancellable),
            on_mounted,
        );
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{
        extraction_folder_name, placement, reserve_destination, unique_child, ArchiveEntry,
        Placement,
    };
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

    fn entry(name: &str, is_dir: bool) -> ArchiveEntry {
        ArchiveEntry {
            uri: format!("archive://x/{name}"),
            name: name.to_owned(),
            is_dir,
        }
    }

    #[test]
    fn a_lone_folder_lands_beside_the_archive_and_anything_else_gets_a_wrapper() {
        assert_eq!(placement(&[]), Placement::Empty);
        assert_eq!(
            placement(&[entry("kit", true)]),
            Placement::Beside(entry("kit", true))
        );
        assert_eq!(placement(&[entry("notes.txt", false)]), Placement::Folder);
        assert_eq!(
            placement(&[entry("a", true), entry("b", true)]),
            Placement::Folder
        );
    }

    #[test]
    fn a_unique_child_is_named_but_not_created() {
        let parent = tempfile::tempdir().unwrap();
        let parent_location = Location::from_local_path(parent.path());
        std::fs::create_dir(parent.path().join("kit")).unwrap();

        let child = unique_child(&parent_location, "kit").unwrap();

        assert_eq!(child.local_path(), Some(parent.path().join("kit (2)")));
        assert!(!parent.path().join("kit (2)").exists());
    }

    /// Runs the whole flow against the real gvfs archive backend.
    fn extract_and_wait(archive: &std::path::Path) -> Vec<crate::core::OperationRecord> {
        use crate::core::OperationManager;
        let operations = OperationManager::default();
        let archive = Location::from_local_path(archive);
        let parent = Location::from_local_path(archive.local_path().unwrap().parent().unwrap());
        assert!(super::extract(&operations, &archive, &parent, |message| {
            panic!("extraction failed: {message}")
        }));
        let context = glib::MainContext::default();
        let started = std::time::Instant::now();
        while started.elapsed().as_secs() < 30 {
            context.iteration(false);
            let records = operations.records();
            if records.len() == 2 && records.iter().all(|record| record.finished_at.is_some()) {
                return records;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("extraction did not finish: {:?}", operations.records());
    }

    #[gtk::test]
    #[ignore = "requires the gvfs archive backend"]
    fn a_zip_holding_one_folder_extracts_to_that_folder_beside_it() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("src");
        std::fs::create_dir_all(source.join("kit/sub")).unwrap();
        std::fs::write(source.join("kit/readme.md"), "hello").unwrap();
        std::fs::write(source.join("kit/sub/inner.txt"), "inner").unwrap();
        std::fs::create_dir(root.path().join("out")).unwrap();
        let moved = root.path().join("out/kit.zip");
        let status = std::process::Command::new("zip")
            .arg("-r")
            .arg("-q")
            .arg(&moved)
            .arg("kit")
            .current_dir(&source)
            .status()
            .unwrap();
        assert!(status.success());

        let records = extract_and_wait(&moved);

        assert!(records
            .iter()
            .all(|record| record.state == crate::core::OperationState::Completed));
        let extracted = root.path().join("out/kit");
        assert_eq!(std::fs::read_to_string(extracted.join("readme.md")).unwrap(), "hello");
        assert_eq!(
            std::fs::read_to_string(extracted.join("sub/inner.txt")).unwrap(),
            "inner"
        );
        assert!(!extracted.join("kit").exists(), "no second folder inside");
    }

    #[gtk::test]
    #[ignore = "requires the gvfs archive backend"]
    fn a_zip_of_loose_files_extracts_into_a_folder_named_after_it() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "a").unwrap();
        std::fs::write(root.path().join("b.txt"), "b").unwrap();
        let status = std::process::Command::new("zip")
            .args(["-q", "loose.zip", "a.txt", "b.txt"])
            .current_dir(root.path())
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_file(root.path().join("a.txt")).unwrap();
        std::fs::remove_file(root.path().join("b.txt")).unwrap();

        extract_and_wait(&root.path().join("loose.zip"));

        assert_eq!(std::fs::read_to_string(root.path().join("loose/a.txt")).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(root.path().join("loose/b.txt")).unwrap(), "b");
    }

    fn seven_zip(archive: &std::path::Path, inputs: &[&str], from: &std::path::Path) -> bool {
        let Some(program) = crate::application::compression::seven_zip_program() else {
            return false;
        };
        std::process::Command::new(program)
            .args(["a", "-bd"])
            .arg(archive)
            .args(inputs)
            .current_dir(from)
            .output()
            .unwrap()
            .status
            .success()
    }

    #[test]
    fn a_seven_zip_holding_one_folder_extracts_to_that_folder_beside_it() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("src");
        std::fs::create_dir_all(source.join("kit/sub")).unwrap();
        std::fs::write(source.join("kit/sub/inner.txt"), "inner").unwrap();
        std::fs::create_dir(root.path().join("out")).unwrap();
        let archive = root.path().join("out/kit.7z");
        if !seven_zip(&archive, &["kit"], &source) {
            return;
        }
        let mut seen = Vec::new();

        super::extract_seven_zip_blocking(
            &archive,
            &root.path().join("out"),
            &mut |done, total| seen.push((done, total)),
            &|| false,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(root.path().join("out/kit/sub/inner.txt")).unwrap(),
            "inner"
        );
        assert!(!root.path().join("out/kit/kit").exists());
        assert_eq!(seen.last().map(|(done, total)| done == total), Some(true));
        assert_eq!(
            std::fs::read_dir(root.path().join("out")).unwrap().count(),
            2,
            "only the archive and the folder; the scratch folder is gone"
        );
    }

    #[test]
    fn a_seven_zip_of_loose_files_extracts_into_a_folder_named_after_it() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "a").unwrap();
        std::fs::write(root.path().join("b.txt"), "b").unwrap();
        if !seven_zip(&root.path().join("loose.7z"), &["a.txt", "b.txt"], root.path()) {
            return;
        }
        std::fs::remove_file(root.path().join("a.txt")).unwrap();
        std::fs::remove_file(root.path().join("b.txt")).unwrap();

        super::extract_seven_zip_blocking(
            &root.path().join("loose.7z"),
            root.path(),
            &mut |_, _| {},
            &|| false,
        )
        .unwrap();

        assert_eq!(std::fs::read_to_string(root.path().join("loose/a.txt")).unwrap(), "a");
    }

    #[test]
    fn a_broken_seven_zip_reports_why_and_leaves_nothing() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("bad.7z"), "not an archive").unwrap();
        if crate::application::compression::seven_zip_program().is_none() {
            return;
        }

        let error = super::extract_seven_zip_blocking(
            &root.path().join("bad.7z"),
            root.path(),
            &mut |_, _| {},
            &|| false,
        )
        .unwrap_err();

        assert!(!error.message().is_empty());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
