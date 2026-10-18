//! Compressing files and folders into an archive beside them.
//!
//! Both directions are handled in-process: reading goes through libarchive
//! (see `extraction`), and the writers here are the pure-Rust `zip`,
//! `sevenz-rust2` and `tar`/`flate2` libraries. No archiver program has to be
//! installed. The work is limited to local files, and the archive is created
//! in the folder the items live in.

use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    SevenZip,
    TarGz,
}

impl ArchiveFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ArchiveFormat::Zip => ".zip",
            ArchiveFormat::SevenZip => ".7z",
            ArchiveFormat::TarGz => ".tar.gz",
        }
    }
}

/// The archive's name before its extension: the item's own name for a single
/// item, a generic one for several.
pub fn default_archive_base(names: &[OsString]) -> String {
    match names {
        [only] => only.to_string_lossy().into_owned(),
        _ => "Archive".to_owned(),
    }
}

/// A path in `parent` that does not exist yet: `Trip.zip`, then `Trip (2).zip`.
pub fn unique_archive_path(parent: &Path, base: &str, format: ArchiveFormat) -> PathBuf {
    let extension = format.extension();
    let mut candidate = parent.join(format!("{base}{extension}"));
    let mut suffix = 2;
    while candidate.symlink_metadata().is_ok() {
        candidate = parent.join(format!("{base} ({suffix}){extension}"));
        suffix += 1;
    }
    candidate
}

#[derive(Debug, PartialEq, Eq)]
pub enum ArchiveError {
    Cancelled,
    Failed(String),
}

impl ArchiveError {
    pub fn message(&self) -> String {
        match self {
            ArchiveError::Cancelled => "Cancelled".to_owned(),
            ArchiveError::Failed(message) => message.clone(),
        }
    }
}

impl From<String> for ArchiveError {
    fn from(message: String) -> Self {
        ArchiveError::Failed(message)
    }
}

impl From<std::io::Error> for ArchiveError {
    fn from(error: std::io::Error) -> Self {
        ArchiveError::Failed(error.to_string())
    }
}

/// A reader that reports how much of `total` it has read and stops with an
/// interrupted error once `cancelled` turns true. Extraction and compression
/// share it so progress and cancellation behave the same while bytes move.
pub(crate) struct ReadProgress<'a, R> {
    inner: R,
    done: &'a mut u64,
    total: u64,
    progress: &'a mut dyn FnMut(u64, u64),
    cancelled: &'a dyn Fn() -> bool,
}

impl<'a, R> ReadProgress<'a, R> {
    pub(crate) fn new(
        inner: R,
        done: &'a mut u64,
        total: u64,
        progress: &'a mut dyn FnMut(u64, u64),
        cancelled: &'a dyn Fn() -> bool,
    ) -> Self {
        Self {
            inner,
            done,
            total,
            progress,
            cancelled,
        }
    }
}

impl<R: Read> Read for ReadProgress<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if (self.cancelled)() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled",
            ));
        }
        let read = self.inner.read(buffer)?;
        *self.done += read as u64;
        (self.progress)((*self.done).min(self.total), self.total);
        Ok(read)
    }
}

impl<R: Seek> Seek for ReadProgress<'_, R> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(position)
    }
}

/// A scratch folder beside the sources. The archive is written there, so a
/// half-written one never shows in the folder and cancelling can discard it
/// whole.
pub(crate) struct Workspace(pub(crate) PathBuf);

impl Workspace {
    pub(crate) fn create(parent: &Path) -> std::io::Result<Self> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = parent.join(format!(".compressing-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One item found while walking the sources, named relative to the folder the
/// archive is being created in.
struct Entry {
    path: PathBuf,
    /// Slash-separated, as archive formats expect.
    name: String,
    kind: EntryKind,
}

enum EntryKind {
    Directory,
    File,
    Symlink(PathBuf),
}

/// Collects the sources and everything below them. `follow_symlinks` mirrors
/// what the format does by convention: zip and tar store links as links, while
/// 7-Zip has no portable link entry here and follows them instead.
fn collect_entries(
    sources: &[PathBuf],
    follow_symlinks: bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<Entry>, ArchiveError> {
    let mut entries = Vec::new();
    let mut visited = std::collections::HashSet::new();
    for source in sources {
        let name = source
            .file_name()
            .ok_or_else(|| "An item has no name to archive.".to_owned())?
            .to_string_lossy()
            .into_owned();
        collect_into(
            source,
            &name,
            follow_symlinks,
            &mut visited,
            &mut entries,
            cancelled,
        )?;
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

fn collect_into(
    path: &Path,
    name: &str,
    follow_symlinks: bool,
    visited: &mut std::collections::HashSet<PathBuf>,
    entries: &mut Vec<Entry>,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), ArchiveError> {
    if cancelled() {
        return Err(ArchiveError::Cancelled);
    }
    let metadata = if follow_symlinks {
        std::fs::metadata(path).or_else(|_| std::fs::symlink_metadata(path))?
    } else {
        std::fs::symlink_metadata(path)?
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        entries.push(Entry {
            path: path.to_path_buf(),
            name: name.to_owned(),
            kind: EntryKind::Symlink(std::fs::read_link(path)?),
        });
    } else if file_type.is_dir() {
        if follow_symlinks {
            // A link that points back up the tree must not be walked twice.
            if let Ok(canonical) = std::fs::canonicalize(path) {
                if !visited.insert(canonical) {
                    return Ok(());
                }
            }
        }
        entries.push(Entry {
            path: path.to_path_buf(),
            name: name.to_owned(),
            kind: EntryKind::Directory,
        });
        let mut children: Vec<std::fs::DirEntry> =
            std::fs::read_dir(path)?.collect::<std::io::Result<_>>()?;
        children.sort_by_key(|child| child.file_name());
        for child in children {
            let child_name = format!("{name}/{}", child.file_name().to_string_lossy());
            collect_into(
                &child.path(),
                &child_name,
                follow_symlinks,
                visited,
                entries,
                cancelled,
            )?;
        }
    } else if file_type.is_file() {
        entries.push(Entry {
            path: path.to_path_buf(),
            name: name.to_owned(),
            kind: EntryKind::File,
        });
    }
    Ok(())
}

fn total_bytes(entries: &[Entry]) -> u64 {
    entries
        .iter()
        .filter(|entry| matches!(entry.kind, EntryKind::File))
        .filter_map(|entry| std::fs::metadata(&entry.path).ok())
        .map(|metadata| metadata.len())
        .sum()
}

fn write_zip(
    archive: &Path,
    entries: &[Entry],
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<(), ArchiveError> {
    let file = std::fs::File::create(archive)?;
    let mut writer = zip::ZipWriter::new(file);
    let mut done = 0u64;
    for entry in entries {
        if cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let metadata = std::fs::symlink_metadata(&entry.path)?;
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(metadata.permissions().mode() & 0o777)
            .last_modified_time(zip_datetime(metadata.modified().ok()));
        match &entry.kind {
            EntryKind::Directory => writer
                .add_directory(entry.name.clone(), options)
                .map_err(|error| ArchiveError::Failed(error.to_string()))?,
            EntryKind::Symlink(target) => writer
                .add_symlink(entry.name.clone(), target.to_string_lossy(), options)
                .map_err(|error| ArchiveError::Failed(error.to_string()))?,
            EntryKind::File => {
                writer
                    .start_file(entry.name.clone(), options)
                    .map_err(|error| ArchiveError::Failed(error.to_string()))?;
                let source = std::fs::File::open(&entry.path)?;
                let mut reader = ReadProgress::new(source, &mut done, total, progress, cancelled);
                std::io::copy(&mut reader, &mut writer)?;
            }
        }
    }
    writer
        .finish()
        .map_err(|error| ArchiveError::Failed(error.to_string()))?;
    Ok(())
}

/// 7-Zip carries a POSIX mode in the high 16 bits of its Windows attributes,
/// and only uses them when the Unix-extension bit is set. Without both,
/// libarchive cannot tell a directory from a file and extracts every entry as
/// a read-write file, leaving folders untraversable.
fn seven_zip_entry(path: &Path, name: String) -> sevenz_rust2::ArchiveEntry {
    let mut entry = sevenz_rust2::ArchiveEntry::from_path(path, name);
    if let Ok(metadata) = std::fs::metadata(path) {
        entry.has_windows_attributes = true;
        entry.windows_attributes = (metadata.permissions().mode() << 16) | 0x8000;
    }
    entry
}

fn write_seven_zip(
    archive: &Path,
    entries: &[Entry],
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<(), ArchiveError> {
    let mut writer = sevenz_rust2::ArchiveWriter::create(archive)
        .map_err(|error| ArchiveError::Failed(error.to_string()))?;
    let mut done = 0u64;
    for entry in entries {
        if cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        match &entry.kind {
            // 7-Zip follows links, so these were already resolved into files
            // and directories by `collect_entries`. A broken link has no data
            // to store and is skipped.
            EntryKind::Symlink(_) => {}
            EntryKind::Directory => {
                writer
                    .push_archive_entry(
                        seven_zip_entry(&entry.path, entry.name.clone()),
                        None::<&[u8]>,
                    )
                    .map_err(|error| ArchiveError::Failed(error.to_string()))?;
            }
            EntryKind::File => {
                let source = std::fs::File::open(&entry.path)?;
                let mut reader = ReadProgress::new(source, &mut done, total, progress, cancelled);
                writer
                    .push_archive_entry(
                        seven_zip_entry(&entry.path, entry.name.clone()),
                        Some(&mut reader),
                    )
                    .map_err(|error| ArchiveError::Failed(error.to_string()))?;
            }
        }
    }
    writer
        .finish()
        .map_err(|error| ArchiveError::Failed(error.to_string()))?;
    Ok(())
}

fn write_tar_gz(
    archive: &Path,
    entries: &[Entry],
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<(), ArchiveError> {
    let file = std::fs::File::create(archive)?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);
    let mut done = 0u64;
    for entry in entries {
        if cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let metadata = std::fs::symlink_metadata(&entry.path)?;
        let mut header = tar::Header::new_gnu();
        header.set_metadata(&metadata);
        match &entry.kind {
            EntryKind::Directory => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                builder.append_data(&mut header, format!("{}/", entry.name), std::io::empty())?;
            }
            EntryKind::Symlink(target) => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_link_name(target)?;
                builder.append_data(&mut header, entry.name.clone(), std::io::empty())?;
            }
            EntryKind::File => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(metadata.len());
                let source = std::fs::File::open(&entry.path)?;
                let mut reader = ReadProgress::new(source, &mut done, total, progress, cancelled);
                builder.append_data(&mut header, entry.name.clone(), &mut reader)?;
            }
        }
    }
    let encoder = builder.into_inner()?;
    encoder.finish()?;
    Ok(())
}

/// A zip timestamp, which only covers 1980 through 2107.
fn zip_datetime(time: Option<std::time::SystemTime>) -> zip::DateTime {
    let Some(time) = time else {
        return zip::DateTime::default();
    };
    let Ok(elapsed) = time.duration_since(std::time::UNIX_EPOCH) else {
        return zip::DateTime::default();
    };
    let seconds = elapsed.as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    if !(1980..=2107).contains(&year) {
        return zip::DateTime::default();
    }
    zip::DateTime::from_date_and_time(
        year as u16,
        month as u8,
        day as u8,
        (of_day / 3_600) as u8,
        ((of_day % 3_600) / 60) as u8,
        (of_day % 60) as u8,
    )
    .unwrap_or_default()
}

/// Days since the Unix epoch to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Archives `sources`, which must all sit in one folder, into a new file there
/// and returns its path.
///
/// `progress` receives (bytes read, bytes to read) while the archive is
/// written; `cancelled` is polled, and a stop discards everything written so
/// far.
pub fn compress_blocking(
    sources: &[PathBuf],
    format: ArchiveFormat,
    progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<PathBuf, ArchiveError> {
    let Some(first) = sources.first() else {
        return Err("Nothing is selected to compress.".to_owned().into());
    };
    let parent = first
        .parent()
        .ok_or_else(|| "This item has no folder to hold an archive.".to_owned())?
        .to_path_buf();
    let mut names = Vec::with_capacity(sources.len());
    for source in sources {
        if source.parent() != Some(parent.as_path()) {
            return Err(
                "Items from different folders cannot be compressed together."
                    .to_owned()
                    .into(),
            );
        }
        names.push(
            source
                .file_name()
                .ok_or_else(|| "An item has no name to archive.".to_owned())?
                .to_owned(),
        );
    }
    let base = default_archive_base(&names);
    let file_name = format!("{base}{}", format.extension());
    let workspace = Workspace::create(&parent)
        .map_err(|error| format!("Could not write to this folder: {error}"))?;
    let scratch_archive = workspace.0.join(&file_name);

    let follow_symlinks = matches!(format, ArchiveFormat::SevenZip);
    let entries = collect_entries(sources, follow_symlinks, cancelled)?;
    let total = total_bytes(&entries);
    if cancelled() {
        return Err(ArchiveError::Cancelled);
    }
    progress(0, total);

    match format {
        ArchiveFormat::Zip => write_zip(&scratch_archive, &entries, total, progress, cancelled)?,
        ArchiveFormat::SevenZip => {
            write_seven_zip(&scratch_archive, &entries, total, progress, cancelled)?
        }
        ArchiveFormat::TarGz => {
            write_tar_gz(&scratch_archive, &entries, total, progress, cancelled)?
        }
    }
    if cancelled() {
        return Err(ArchiveError::Cancelled);
    }
    progress(total, total);
    let archive = unique_archive_path(&parent, &base, format);
    std::fs::rename(&scratch_archive, &archive)
        .map_err(|error| format!("Could not save the archive: {error}"))?;
    Ok(archive)
}

/// Compresses on a worker thread, reporting through `job`, and hands the
/// result to `on_done` on the main loop.
pub fn compress(
    sources: Vec<PathBuf>,
    format: ArchiveFormat,
    job: crate::core::JobHandle,
    on_done: impl FnOnce(Result<PathBuf, ArchiveError>) + 'static,
) {
    glib::MainContext::default().spawn_local(async move {
        let result = gio::spawn_blocking(move || {
            let result = compress_blocking(
                &sources,
                format,
                &mut |done, total| job.set_progress(done, total),
                &|| job.is_cancelled(),
            );
            job.finish(result.as_ref().map(|_| ()).map_err(ArchiveError::message));
            result
        })
        .await
        .unwrap_or_else(|_| {
            Err(ArchiveError::Failed(
                "The compressor stopped unexpectedly.".to_owned(),
            ))
        });
        on_done(result);
    });
}

#[cfg(test)]
mod tests {
    use super::{
        compress_blocking, default_archive_base, unique_archive_path, ArchiveError, ArchiveFormat,
    };
    use std::ffi::OsString;

    fn run(
        sources: &[std::path::PathBuf],
        format: ArchiveFormat,
    ) -> Result<std::path::PathBuf, ArchiveError> {
        compress_blocking(sources, format, &mut |_, _| {}, &|| false)
    }

    fn names(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn listing(archive: &std::path::Path) -> Vec<String> {
        let file = std::fs::File::open(archive).unwrap();
        compress_tools::list_archive_files(file).unwrap()
    }

    #[test]
    fn one_item_names_the_archive_after_itself_and_several_share_a_name() {
        assert_eq!(default_archive_base(&names(&["Trip"])), "Trip");
        assert_eq!(default_archive_base(&names(&["a", "b"])), "Archive");
    }

    #[test]
    fn an_existing_archive_pushes_the_name_to_a_unique_one() {
        let parent = tempfile::tempdir().unwrap();
        std::fs::write(parent.path().join("Trip.tar.gz"), "").unwrap();
        std::fs::write(parent.path().join("Trip (2).tar.gz"), "").unwrap();

        assert_eq!(
            unique_archive_path(parent.path(), "Trip", ArchiveFormat::TarGz),
            parent.path().join("Trip (3).tar.gz")
        );
        assert_eq!(
            unique_archive_path(parent.path(), "Trip", ArchiveFormat::Zip),
            parent.path().join("Trip.zip")
        );
    }

    #[test]
    fn sources_from_different_folders_are_refused() {
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        let a = one.path().join("a");
        let b = two.path().join("b");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();

        assert!(run(&[a, b], ArchiveFormat::TarGz).is_err());
        assert!(run(&[], ArchiveFormat::TarGz).is_err());
    }

    fn fixture() -> (tempfile::TempDir, Vec<std::path::PathBuf>) {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("-folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("inner.txt"), "inner").unwrap();
        let file = root.path().join("a b*.txt");
        std::fs::write(&file, "outer").unwrap();
        (root, vec![folder, file])
    }

    #[test]
    fn tar_gz_holds_relative_names_with_awkward_characters() {
        let (root, sources) = fixture();
        let archive = run(&sources, ArchiveFormat::TarGz).unwrap();

        assert_eq!(archive, root.path().join("Archive.tar.gz"));
        let listed = listing(&archive);
        assert!(
            listed.iter().any(|name| name == "-folder/inner.txt"),
            "{listed:?}"
        );
        assert!(listed.iter().any(|name| name == "a b*.txt"), "{listed:?}");
    }

    #[test]
    fn zip_holds_relative_names_with_awkward_characters() {
        let (root, sources) = fixture();
        let archive = run(&sources, ArchiveFormat::Zip).unwrap();

        assert_eq!(archive, root.path().join("Archive.zip"));
        let listed = listing(&archive);
        assert!(
            listed.iter().any(|name| name == "-folder/inner.txt"),
            "{listed:?}"
        );
        assert!(listed.iter().any(|name| name == "a b*.txt"), "{listed:?}");
    }

    #[test]
    fn seven_zip_holds_relative_names_with_awkward_characters() {
        let (root, sources) = fixture();
        let archive = run(&sources, ArchiveFormat::SevenZip).unwrap();

        assert_eq!(archive, root.path().join("Archive.7z"));
        let listed = listing(&archive);
        assert!(
            listed.iter().any(|name| name == "-folder/inner.txt"),
            "{listed:?}"
        );
        assert!(listed.iter().any(|name| name == "a b*.txt"), "{listed:?}");
        assert!(
            !listed.iter().any(|name| name.starts_with("./")),
            "{listed:?}"
        );
    }

    #[test]
    fn a_failed_archive_leaves_no_partial_file() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("gone");

        let error = run(&[missing], ArchiveFormat::TarGz).unwrap_err();

        assert!(!error.message().is_empty());
        assert!(!root.path().join("gone.tar.gz").exists());
        assert_eq!(
            std::fs::read_dir(root.path()).unwrap().count(),
            0,
            "the scratch folder is removed too"
        );
    }

    #[test]
    fn progress_climbs_to_the_size_of_the_data_and_the_scratch_folder_is_gone() {
        let root = tempfile::tempdir().unwrap();
        let big = root.path().join("big.bin");
        std::fs::write(&big, vec![7u8; 4 * 1024 * 1024]).unwrap();
        let mut seen = Vec::new();

        let archive = compress_blocking(
            &[big],
            ArchiveFormat::TarGz,
            &mut |done, total| seen.push((done, total)),
            &|| false,
        )
        .unwrap();

        assert_eq!(seen.first(), Some(&(0, 4 * 1024 * 1024)));
        assert_eq!(seen.last(), Some(&(4 * 1024 * 1024, 4 * 1024 * 1024)));
        assert!(seen.iter().all(|(done, total)| done <= total));
        assert!(archive.is_file());
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(
            leftovers.len(),
            2,
            "only the source and the archive: {leftovers:?}"
        );
    }

    #[test]
    fn cancelling_stops_the_writer_and_leaves_nothing_behind() {
        let root = tempfile::tempdir().unwrap();
        let big = root.path().join("big.bin");
        std::fs::write(&big, vec![7u8; 1024 * 1024]).unwrap();
        let cancel_after_start = std::cell::Cell::new(false);

        let result = compress_blocking(
            &[big],
            ArchiveFormat::TarGz,
            &mut |_, _| cancel_after_start.set(true),
            &|| cancel_after_start.get(),
        );

        assert_eq!(result, Err(ArchiveError::Cancelled));
        let names: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("big.bin")]);
    }
}
