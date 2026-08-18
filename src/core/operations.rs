use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gio::prelude::*;

pub struct FileOperations;

impl FileOperations {
    pub fn create_folder(parent: &Path, name: &str) -> io::Result<PathBuf> {
        let path = parent.join(name);
        fs::create_dir(&path)?;
        Ok(path)
    }

    pub fn rename(source: &Path, name: &str) -> io::Result<PathBuf> {
        let destination = source
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source has no parent"))?
            .join(name);
        fs::rename(source, &destination)?;
        Ok(destination)
    }

    pub fn copy(sources: &[PathBuf], destination: &Path) -> io::Result<()> {
        for source in sources {
            let target = destination.join(source.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "source has no filename")
            })?);
            if source.is_dir() {
                copy_dir(source, &target)?;
            } else {
                fs::copy(source, target)?;
            }
        }
        Ok(())
    }

    pub fn copy_to_target(source: &Path, target: &Path) -> io::Result<()> {
        if target.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "destination already exists",
            ));
        }
        if source.is_dir() {
            copy_dir(source, target)
        } else {
            fs::copy(source, target).map(|_| ())
        }
    }

    /// Total bytes under `sources`, for progress reporting. A single huge
    /// file is one "unit" but many bytes, so byte-based progress is what
    /// actually animates for both a big single file and a big folder;
    /// counting files instead left single-file copies frozen at 0% the
    /// whole transfer, then jumping straight to 100%.
    pub fn count_bytes(sources: &[PathBuf]) -> u64 {
        sources.iter().map(|source| count_bytes_for(source)).sum()
    }

    /// Same behaviour as [`copy`], but checks `cancel` throughout a file's
    /// transfer (not just between files) and calls `on_progress` with the
    /// number of bytes just written, so both a single large file and a
    /// large folder report smooth progress and respond to Cancel promptly
    /// instead of running as one opaque step.
    pub fn copy_with_progress(sources: &[PathBuf], destination: &Path, cancel: &AtomicBool, on_progress: &mut dyn FnMut(u64)) -> io::Result<()> {
        for source in sources {
            let target = destination.join(source.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "source has no filename")
            })?);
            copy_path_with_progress(source, &target, cancel, on_progress)?;
        }
        Ok(())
    }

    /// Same behaviour as [`copy_to_target`], but cancellable and reporting
    /// byte-level progress; see [`copy_with_progress`].
    pub fn copy_to_target_with_progress(source: &Path, target: &Path, cancel: &AtomicBool, on_progress: &mut dyn FnMut(u64)) -> io::Result<()> {
        if target.exists() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination already exists"));
        }
        copy_path_with_progress(source, target, cancel, on_progress)
    }

    pub fn move_to(sources: &[PathBuf], destination: &Path) -> io::Result<()> {
        for source in sources {
            let target = destination.join(source.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "source has no filename")
            })?);
            fs::rename(source, target)?;
        }
        Ok(())
    }

    pub fn move_to_target(source: &Path, target: &Path) -> io::Result<()> { fs::rename(source, target) }

    pub fn move_to_trash(sources: &[PathBuf]) -> io::Result<()> {
        for source in sources {
            gio::File::for_path(source)
                .trash(gio::Cancellable::NONE)
                .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
        }
        Ok(())
    }

    pub fn delete_permanently(sources: &[PathBuf]) -> io::Result<()> {
        for source in sources {
            if source.is_dir() { fs::remove_dir_all(source)?; } else { fs::remove_file(source)?; }
        }
        Ok(())
    }

    pub fn keep_both_destination(target: &Path) -> PathBuf {
        if !target.exists() { return target.to_path_buf(); }
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        let stem = target.file_stem().and_then(|value| value.to_str()).unwrap_or("item");
        let extension = target.extension().and_then(|value| value.to_str());
        let (base, start) = split_copy_suffix(stem);
        for index in start.. {
            let name = match extension {
                Some(extension) => format!("{base} ({index}).{extension}"),
                None => format!("{base} ({index})"),
            };
            let candidate = parent.join(name);
            if !candidate.exists() { return candidate; }
        }
        unreachable!()
    }
}

fn split_copy_suffix(stem: &str) -> (&str, u32) {
    let Some(open) = stem.rfind(" (") else { return (stem, 2); };
    if !stem.ends_with(')') {
        return (stem, 2);
    }
    let Some(number) = stem.get(open + 2..stem.len() - 1) else { return (stem, 2); };
    if let Ok(number) = number.parse::<u32>() {
        return (&stem[..open], number.saturating_add(1).max(2));
    }
    (stem, 2)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::FileOperations;

    #[test]
    fn move_to_places_a_file_in_the_selected_directory() {
        let source_dir = std::env::temp_dir().join("zorin-explorer-move-source");
        let destination_dir = std::env::temp_dir().join("zorin-explorer-move-destination");
        let source = source_dir.join("report.txt");
        let _ = fs::remove_dir_all(&source_dir);
        let _ = fs::remove_dir_all(&destination_dir);
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&destination_dir).unwrap();
        fs::write(&source, "contents").unwrap();

        FileOperations::move_to(&[source.clone()], &destination_dir).unwrap();

        assert!(!source.exists());
        assert_eq!(fs::read_to_string(destination_dir.join("report.txt")).unwrap(), "contents");
        let _ = fs::remove_dir_all(&source_dir);
        let _ = fs::remove_dir_all(&destination_dir);
    }

    #[test]
    fn keep_both_name_increments_until_destination_is_available() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("report.txt"), "old").unwrap();
        fs::write(directory.path().join("report (2).txt"), "old").unwrap();
        assert_eq!(FileOperations::keep_both_destination(&directory.path().join("report.txt")), directory.path().join("report (3).txt"));
    }

    #[test]
    fn keep_both_name_continues_an_existing_numeric_suffix_without_nesting() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("New Folder (2)"), "old").unwrap();
        fs::write(directory.path().join("New Folder (3)"), "old").unwrap();
        assert_eq!(FileOperations::keep_both_destination(&directory.path().join("New Folder (2)")), directory.path().join("New Folder (4)"));
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

        assert_eq!(FileOperations::count_bytes(&[directory.path().to_path_buf()]), 5);
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

        FileOperations::copy_with_progress(&[source], &destination, &cancel, &mut |chunk| bytes += chunk).unwrap();

        assert_eq!(bytes, 5);
        assert_eq!(fs::read_to_string(destination.join("source/a.txt")).unwrap(), "12");
    }

    #[test]
    fn a_pre_cancelled_copy_stops_before_touching_the_filesystem() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.txt");
        let target = directory.path().join("target.txt");
        fs::write(&source, "contents").unwrap();
        let cancel = AtomicBool::new(true);

        let error = FileOperations::copy_to_target_with_progress(&source, &target, &cancel, &mut |_| {}).unwrap_err();

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

        let error = FileOperations::copy_with_progress(&[source], &destination, &cancel, &mut |_| {
            chunks += 1;
            cancel.store(true, Ordering::Relaxed);
        }).unwrap_err();

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
        }).unwrap();

        assert!(chunk_calls > 1, "expected multiple progress callbacks for a multi-chunk file, got {chunk_calls}");
        assert_eq!(total_bytes, 5 * (1 << 20));
    }
}

fn copy_dir(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn count_bytes_for(path: &Path) -> u64 {
    if path.is_dir() {
        fs::read_dir(path).map(|entries| entries.flatten().map(|entry| count_bytes_for(&entry.path())).sum()).unwrap_or(0)
    } else {
        fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0)
    }
}

fn cancelled_error() -> io::Error { io::Error::new(io::ErrorKind::Interrupted, "operation cancelled") }

fn copy_path_with_progress(source: &Path, target: &Path, cancel: &AtomicBool, on_progress: &mut dyn FnMut(u64)) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) { return Err(cancelled_error()); }
    if source.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_path_with_progress(&entry.path(), &target.join(entry.file_name()), cancel, on_progress)?;
        }
        Ok(())
    } else {
        copy_file_cancelable(source, target, cancel, on_progress)
    }
}

/// Copies in chunks (rather than one `fs::copy` syscall) so `cancel` is
/// checked throughout a single large file's transfer (not just between
/// files) and `on_progress` is called after every chunk — otherwise a
/// single large file reports no progress until it's entirely done, and
/// Cancel has no effect until the current file finishes.
fn copy_file_cancelable(source: &Path, target: &Path, cancel: &AtomicBool, on_progress: &mut dyn FnMut(u64)) -> io::Result<()> {
    let mut reader = fs::File::open(source)?;
    let mut writer = fs::File::create(target)?;
    let mut buffer = [0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) { return Err(cancelled_error()); }
        let read = reader.read(&mut buffer)?;
        if read == 0 { break; }
        writer.write_all(&buffer[..read])?;
        on_progress(read as u64);
    }
    Ok(())
}
