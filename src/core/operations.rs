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

    /// Counts the copy "units" (files, plus one for each otherwise-empty
    /// directory) under `sources`, for progress reporting. Always at least 1
    /// so a progress fraction never divides by zero.
    pub fn count_units(sources: &[PathBuf]) -> usize {
        sources.iter().map(|source| count_units_for(source)).sum::<usize>().max(1)
    }

    /// Same behaviour as [`copy`], but checks `cancel` between files (not
    /// just between whole top-level sources) and calls `on_unit` after each
    /// file finishes, so a single large folder still reports progress and
    /// responds to Cancel promptly instead of running as one opaque step.
    pub fn copy_with_progress(sources: &[PathBuf], destination: &Path, cancel: &AtomicBool, on_unit: &mut dyn FnMut()) -> io::Result<()> {
        for source in sources {
            let target = destination.join(source.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "source has no filename")
            })?);
            copy_path_with_progress(source, &target, cancel, on_unit)?;
        }
        Ok(())
    }

    /// Same behaviour as [`copy_to_target`], but cancellable and reporting
    /// per-file progress; see [`copy_with_progress`].
    pub fn copy_to_target_with_progress(source: &Path, target: &Path, cancel: &AtomicBool, on_unit: &mut dyn FnMut()) -> io::Result<()> {
        if target.exists() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination already exists"));
        }
        copy_path_with_progress(source, target, cancel, on_unit)
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
    fn counting_units_treats_each_file_as_one_unit() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("a.txt"), "1").unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("nested/b.txt"), "2").unwrap();

        assert_eq!(FileOperations::count_units(&[directory.path().to_path_buf()]), 2);
    }

    #[test]
    fn counting_units_is_never_zero_for_an_empty_source_list() {
        assert_eq!(FileOperations::count_units(&[]), 1);
    }

    #[test]
    fn copy_with_progress_reports_one_unit_per_file() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("a.txt"), "1").unwrap();
        fs::write(source.join("b.txt"), "2").unwrap();
        let cancel = AtomicBool::new(false);
        let mut units = 0;

        FileOperations::copy_with_progress(&[source], &destination, &cancel, &mut || units += 1).unwrap();

        assert_eq!(units, 2);
        assert_eq!(fs::read_to_string(destination.join("source/a.txt")).unwrap(), "1");
    }

    #[test]
    fn a_pre_cancelled_copy_stops_before_touching_the_filesystem() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.txt");
        let target = directory.path().join("target.txt");
        fs::write(&source, "contents").unwrap();
        let cancel = AtomicBool::new(true);

        let error = FileOperations::copy_to_target_with_progress(&source, &target, &cancel, &mut || {}).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(!target.exists());
    }

    #[test]
    fn cancelling_between_files_stops_the_remaining_copy() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("a.txt"), "1").unwrap();
        fs::write(source.join("b.txt"), "2").unwrap();
        let cancel = AtomicBool::new(false);
        let mut units = 0;

        let error = FileOperations::copy_with_progress(&[source], &destination, &cancel, &mut || {
            units += 1;
            cancel.store(true, Ordering::Relaxed);
        }).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert_eq!(units, 1);
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

fn count_units_for(path: &Path) -> usize {
    if path.is_dir() {
        let count = fs::read_dir(path).map(|entries| entries.flatten().map(|entry| count_units_for(&entry.path())).sum()).unwrap_or(0);
        count.max(1)
    } else {
        1
    }
}

fn cancelled_error() -> io::Error { io::Error::new(io::ErrorKind::Interrupted, "operation cancelled") }

fn copy_path_with_progress(source: &Path, target: &Path, cancel: &AtomicBool, on_unit: &mut dyn FnMut()) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) { return Err(cancelled_error()); }
    if source.is_dir() {
        fs::create_dir_all(target)?;
        let mut had_children = false;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            had_children = true;
            copy_path_with_progress(&entry.path(), &target.join(entry.file_name()), cancel, on_unit)?;
        }
        if !had_children { on_unit(); }
        Ok(())
    } else {
        copy_file_cancelable(source, target, cancel)?;
        on_unit();
        Ok(())
    }
}

/// Copies in chunks (rather than one `fs::copy` syscall) so `cancel` is
/// checked throughout a single large file's transfer, not just between
/// files — otherwise Cancel has no effect until the current file finishes.
fn copy_file_cancelable(source: &Path, target: &Path, cancel: &AtomicBool) -> io::Result<()> {
    let mut reader = fs::File::open(source)?;
    let mut writer = fs::File::create(target)?;
    let mut buffer = [0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) { return Err(cancelled_error()); }
        let read = reader.read(&mut buffer)?;
        if read == 0 { break; }
        writer.write_all(&buffer[..read])?;
    }
    Ok(())
}
