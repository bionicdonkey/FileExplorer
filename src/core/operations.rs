use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
        if source.is_dir() { copy_dir(source, target) } else { fs::copy(source, target).map(|_| ()) }
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
        for index in 2.. {
            let name = match extension { Some(extension) => format!("{stem} ({index}).{extension}"), None => format!("{stem} ({index})") };
            let candidate = parent.join(name);
            if !candidate.exists() { return candidate; }
        }
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

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
