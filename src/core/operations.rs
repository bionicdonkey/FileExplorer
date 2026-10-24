//! Compatibility local-path boundary. Mutation implementations live in GIO.
use crate::core::contracts::MutationKind;
use crate::core::{OperationKind, OperationManager};
use crate::infrastructure::gio_operations;
use gio::prelude::*;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct FileOperations;
impl FileOperations {
    /// Local path entry boundary: enqueue on the application's shared coordinator.
    pub fn create_folder(manager: &OperationManager, parent: &Path, name: &str) -> io::Result<u64> {
        validate_name(name)?;
        let path = parent.join(name);
        Ok(manager.submit(OperationKind::Create, vec![path], None))
    }
    pub fn rename(manager: &OperationManager, source: &Path, name: &str) -> io::Result<u64> {
        validate_name(name)?;
        let target = source
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source has no parent"))?
            .join(name);
        Ok(manager.submit_to_target(OperationKind::Rename, source.to_path_buf(), target))
    }
    pub fn copy(sources: &[PathBuf], destination: &Path) -> io::Result<()> {
        Self::copy_with_progress(sources, destination, &AtomicBool::new(false), &mut |_| {})
    }
    pub fn copy_to_target(source: &Path, target: &Path) -> io::Result<()> {
        Self::copy_to_target_with_progress(source, target, &AtomicBool::new(false), &mut |_| {})
    }
    pub fn count_bytes(sources: &[PathBuf]) -> u64 {
        sources
            .iter()
            .map(|p| {
                gio_operations::count_bytes(&gio::File::for_path(p), &gio::Cancellable::new())
                    .unwrap_or(0)
            })
            .sum()
    }
    pub fn copy_with_progress(
        sources: &[PathBuf],
        destination: &Path,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> io::Result<()> {
        for source in sources {
            let name = source.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "source has no filename")
            })?;
            Self::copy_to_target_with_progress(source, &destination.join(name), cancel, progress)?;
        }
        Ok(())
    }
    pub fn copy_to_target_with_progress(
        source: &Path,
        target: &Path,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> io::Result<()> {
        perform(MutationKind::Copy, source, Some(target), cancel, progress)
    }
    pub fn move_to(sources: &[PathBuf], destination: &Path) -> io::Result<()> {
        for source in sources {
            let name = source.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "source has no filename")
            })?;
            Self::move_to_target(source, &destination.join(name))?;
        }
        Ok(())
    }
    pub fn move_to_target(source: &Path, target: &Path) -> io::Result<()> {
        perform(
            MutationKind::Move,
            source,
            Some(target),
            &AtomicBool::new(false),
            &mut |_| {},
        )
    }
    pub fn move_to_trash(sources: &[PathBuf]) -> io::Result<()> {
        for source in sources {
            perform(
                MutationKind::Trash,
                source,
                None,
                &AtomicBool::new(false),
                &mut |_| {},
            )?;
        }
        Ok(())
    }
    /// Internal helper boundary; application callers use confirmed coordinator submission.
    pub fn delete_permanently(sources: &[PathBuf]) -> io::Result<()> {
        for source in sources {
            perform(
                MutationKind::Delete,
                source,
                None,
                &AtomicBool::new(false),
                &mut |_| {},
            )?;
        }
        Ok(())
    }
    pub fn keep_both_destination(target: &Path) -> PathBuf {
        if !target.exists() {
            return target.to_path_buf();
        }
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        let stem = target
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("item");
        let extension = target.extension().and_then(|value| value.to_str());
        let (base, start) = split_copy_suffix(stem);
        for index in start.. {
            let name = match extension {
                Some(extension) => format!("{base} ({index}).{extension}"),
                None => format!("{base} ({index})"),
            };
            let candidate = parent.join(name);
            if !candidate.exists() {
                return candidate;
            }
        }
        unreachable!()
    }
}

fn validate_name(name: &str) -> io::Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "name must be a single file name",
        ));
    }
    Ok(())
}

fn split_copy_suffix(stem: &str) -> (&str, u32) {
    let Some(open) = stem.rfind(" (") else {
        return (stem, 2);
    };
    if !stem.ends_with(')') {
        return (stem, 2);
    }
    let Some(number) = stem.get(open + 2..stem.len() - 1) else {
        return (stem, 2);
    };
    if let Ok(number) = number.parse::<u32>() {
        return (&stem[..open], number.saturating_add(1).max(2));
    }
    (stem, 2)
}

fn perform(
    kind: MutationKind,
    source: &Path,
    target: Option<&Path>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> io::Result<()> {
    let cancellable = gio::Cancellable::new();
    if cancel.load(Ordering::Relaxed) {
        cancellable.cancel();
    }
    gio_operations::perform(
        kind,
        &gio::File::for_path(source),
        target.map(gio::File::for_path).as_ref(),
        &cancellable,
        &mut false,
        &mut |bytes| {
            if bytes > 0 {
                progress(bytes);
            }
            if cancel.load(Ordering::Relaxed) {
                cancellable.cancel();
            }
        },
    )
    .map_err(|error| {
        io::Error::new(
            match error.kind::<gio::IOErrorEnum>() {
                Some(gio::IOErrorEnum::Exists) => io::ErrorKind::AlreadyExists,
                Some(gio::IOErrorEnum::Cancelled) => io::ErrorKind::Interrupted,
                Some(gio::IOErrorEnum::PermissionDenied) => io::ErrorKind::PermissionDenied,
                Some(gio::IOErrorEnum::NotFound) => io::ErrorKind::NotFound,
                _ => io::ErrorKind::Other,
            },
            error,
        )
    })
}
#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::FileOperations;

    #[test]
    fn move_to_places_a_file_in_the_selected_directory() {
        let source_dir = std::env::temp_dir().join("lorin-explorer-move-source");
        let destination_dir = std::env::temp_dir().join("lorin-explorer-move-destination");
        let source = source_dir.join("report.txt");
        let _ = fs::remove_dir_all(&source_dir);
        let _ = fs::remove_dir_all(&destination_dir);
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&destination_dir).unwrap();
        fs::write(&source, "contents").unwrap();

        FileOperations::move_to(&[source.clone()], &destination_dir).unwrap();

        assert!(!source.exists());
        assert_eq!(
            fs::read_to_string(destination_dir.join("report.txt")).unwrap(),
            "contents"
        );
        let _ = fs::remove_dir_all(&source_dir);
        let _ = fs::remove_dir_all(&destination_dir);
    }

    #[test]
    fn keep_both_name_increments_until_destination_is_available() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("report.txt"), "old").unwrap();
        fs::write(directory.path().join("report (2).txt"), "old").unwrap();
        assert_eq!(
            FileOperations::keep_both_destination(&directory.path().join("report.txt")),
            directory.path().join("report (3).txt")
        );
    }

    #[test]
    fn keep_both_name_continues_an_existing_numeric_suffix_without_nesting() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("New Folder (2)"), "old").unwrap();
        fs::write(directory.path().join("New Folder (3)"), "old").unwrap();
        assert_eq!(
            FileOperations::keep_both_destination(&directory.path().join("New Folder (2)")),
            directory.path().join("New Folder (4)")
        );
    }

    #[test]
    fn copy_to_target_rejects_an_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let target = directory.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();

        let error = FileOperations::copy_to_target(&source, &target).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn counting_bytes_sums_every_file_under_a_folder() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("a.txt"), "12").unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("nested/b.txt"), "345").unwrap();

        assert_eq!(
            FileOperations::count_bytes(&[directory.path().to_path_buf()]),
            5
        );
    }

    #[test]
    fn counting_bytes_of_an_empty_source_list_is_zero() {
        assert_eq!(FileOperations::count_bytes(&[]), 0);
    }

    #[test]
    fn copy_with_progress_reports_bytes_copied_per_file() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("a.txt"), "12").unwrap();
        fs::write(source.join("b.txt"), "345").unwrap();
        let cancel = AtomicBool::new(false);
        let mut bytes = 0u64;

        FileOperations::copy_with_progress(&[source], &destination, &cancel, &mut |chunk| {
            bytes += chunk
        })
        .unwrap();

        assert_eq!(bytes, 5);
        assert_eq!(
            fs::read_to_string(destination.join("source/a.txt")).unwrap(),
            "12"
        );
    }

    #[test]
    fn a_pre_cancelled_copy_stops_before_touching_the_filesystem() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.txt");
        let target = directory.path().join("target.txt");
        fs::write(&source, "contents").unwrap();
        let cancel = AtomicBool::new(true);

        let error =
            FileOperations::copy_to_target_with_progress(&source, &target, &cancel, &mut |_| {})
                .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(!target.exists());
    }

    #[test]
    fn cancelling_mid_transfer_stops_the_remaining_copy() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("a.txt"), "1").unwrap();
        fs::write(source.join("b.txt"), "2").unwrap();
        let cancel = AtomicBool::new(false);
        let mut chunks = 0;

        let error =
            FileOperations::copy_with_progress(&[source], &destination, &cancel, &mut |_| {
                chunks += 1;
                cancel.store(true, Ordering::Relaxed);
            })
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert_eq!(chunks, 1);
    }

    #[test]
    fn a_single_large_file_reports_progress_mid_copy_not_just_at_completion() {
        // Regression guard: progress used to be tracked per *file*, so a
        // single big file (total = 1 unit) never showed intermediate
        // progress -- frozen until the whole copy finished, then 100%.
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("big.bin");
        let target = directory.path().join("big-copy.bin");
        fs::write(&source, vec![0u8; 5 * (1 << 20)]).unwrap(); // 5 MiB, several chunks
        let cancel = AtomicBool::new(false);
        let mut chunk_calls = 0;
        let mut total_bytes = 0u64;

        FileOperations::copy_to_target_with_progress(&source, &target, &cancel, &mut |chunk| {
            chunk_calls += 1;
            total_bytes += chunk;
        })
        .unwrap();

        assert!(
            chunk_calls >= 1,
            "GIO must report byte progress, including fast native/reflink copies"
        );
        assert_eq!(total_bytes, 5 * (1 << 20));
    }
}
