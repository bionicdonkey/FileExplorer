use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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

    pub fn move_to(sources: &[PathBuf], destination: &Path) -> io::Result<()> {
        for source in sources {
            let target = destination.join(source.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "source has no filename")
            })?);
            fs::rename(source, target)?;
        }
        Ok(())
    }

    pub fn move_to_trash(sources: &[PathBuf]) -> io::Result<()> {
        let trash = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".local/share"))
            .join("Trash/files");
        fs::create_dir_all(&trash)?;
        for source in sources {
            let name = source.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "source has no filename")
            })?;
            fs::rename(source, trash.join(name))?;
        }
        Ok(())
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
