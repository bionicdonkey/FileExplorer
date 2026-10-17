//! Compressing files and folders into an archive beside them.
//!
//! Extracting leans on the GVfs archive backend; there is no writing
//! counterpart for it, so compressing runs the standard archiver for the
//! chosen format. It is limited to local files, and the archive is created in
//! the folder the items live in.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

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

pub(crate) fn find_in_path(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    })
}

/// The 7-Zip binary goes by several names depending on the package.
pub(crate) fn seven_zip_program() -> Option<PathBuf> {
    ["7zz", "7z", "7za", "7zr"]
        .into_iter()
        .find_map(find_in_path)
}

pub(crate) fn missing_tool(format: ArchiveFormat) -> String {
    match format {
        ArchiveFormat::Zip => "The zip program is not installed.".to_owned(),
        ArchiveFormat::SevenZip => "The 7-Zip program (7z) is not installed.".to_owned(),
        ArchiveFormat::TarGz => "The tar program is not installed.".to_owned(),
    }
}

/// Builds the archiver command, run from the items' folder so the archive
/// holds relative names. Names are prefixed with `./` (or follow `--`) so a
/// file called `-r` or `@list` is never read as an option.
fn archive_command(
    format: ArchiveFormat,
    archive: &Path,
    parent: &Path,
    names: &[OsString],
) -> Result<Command, String> {
    let relative = || {
        names.iter().map(|name| {
            let mut path = OsString::from("./");
            path.push(name);
            path
        })
    };
    let mut command = match format {
        ArchiveFormat::Zip => {
            let program = find_in_path("zip").ok_or_else(|| missing_tool(format))?;
            let mut command = Command::new(program);
            // -nw: names are literal, never wildcard patterns.
            command.args(["-r", "-q", "-y", "-nw"]).arg(archive);
            command.args(relative());
            command
        }
        ArchiveFormat::SevenZip => {
            let program = seven_zip_program().ok_or_else(|| missing_tool(format))?;
            let mut command = Command::new(program);
            command.args(["a", "-bd", "-y"]).arg(archive);
            command.args(relative());
            command
        }
        ArchiveFormat::TarGz => {
            let program = find_in_path("tar").ok_or_else(|| missing_tool(format))?;
            let mut command = Command::new(program);
            command.arg("-czf").arg(archive).arg("-C").arg(parent).arg("--");
            command.args(names);
            command
        }
    };
    command.current_dir(parent);
    Ok(command)
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

/// How often progress is sampled and cancellation is noticed.
const POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// The bytes of file data under `sources`, symlinks not followed.
fn total_bytes(sources: &[PathBuf], cancelled: &dyn Fn() -> bool) -> u64 {
    let mut total = 0;
    let mut pending: Vec<PathBuf> = sources.to_vec();
    while let Some(path) = pending.pop() {
        if cancelled() {
            break;
        }
        let Ok(metadata) = path.symlink_metadata() else {
            continue;
        };
        if metadata.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                pending.extend(entries.flatten().map(|entry| entry.path()));
            }
        } else if metadata.is_file() {
            total += metadata.len();
        }
    }
    total
}

/// Bytes the process has read so far. An archiver reads each input file once,
/// so this follows its progress through the data without parsing its output.
fn bytes_read(pid: u32) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/{pid}/io"))
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("rchar:"))
        .and_then(|value| value.trim().parse().ok())
}

fn drain(mut stream: impl std::io::Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stream.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// A scratch folder beside the sources. The archiver writes there, so a
/// half-written archive (and the archiver's own temporary files) never shows
/// in the folder and cancelling can discard it whole.
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

/// Runs `command` to completion, reporting how much of `total` bytes it has
/// read and stopping it if `cancelled` turns true. A failure carries the
/// last line the program printed.
pub(crate) fn run_with_progress(
    command: &mut Command,
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<(), ArchiveError> {
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("Could not start the archiver: {error}"))?;
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let pid = child.id();
    let status = loop {
        if cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ArchiveError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if let Some(read) = bytes_read(pid) {
                    progress(read.min(total), total);
                }
                std::thread::sleep(POLL);
            }
            Err(error) => return Err(format!("The archiver stopped unexpectedly: {error}").into()),
        }
    };
    let stdout = stdout.and_then(|handle| handle.join().ok()).unwrap_or_default();
    let stderr = stderr.and_then(|handle| handle.join().ok()).unwrap_or_default();
    if !status.success() {
        let detail = [stderr.trim(), stdout.trim()]
            .into_iter()
            .find(|text| !text.is_empty())
            .and_then(|text| text.lines().rev().find(|line| !line.trim().is_empty()))
            .unwrap_or("the archiver reported an error");
        return Err(detail.trim().to_owned().into());
    }
    Ok(())
}

/// Archives `sources`, which must all sit in one folder, into a new file there
/// and returns its path.
///
/// `progress` receives (bytes read, bytes to read) while the archiver runs;
/// `cancelled` is polled, and a stop discards everything written so far.
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
            return Err("Items from different folders cannot be compressed together."
                .to_owned()
                .into());
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
    let mut command = archive_command(format, &scratch_archive, &parent, &names)?;

    let total = total_bytes(sources, cancelled);
    if cancelled() {
        return Err(ArchiveError::Cancelled);
    }
    progress(0, total);

    run_with_progress(&mut command, total, progress, cancelled)?;
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
                "The archiver stopped unexpectedly.".to_owned(),
            ))
        });
        on_done(result);
    });
}

#[cfg(test)]
mod tests {
    use super::{
        compress_blocking, default_archive_base, find_in_path, seven_zip_program,
        unique_archive_path, ArchiveFormat, ArchiveError,
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

    fn listing(program: &str, args: &[&str], archive: &std::path::Path) -> String {
        let output = std::process::Command::new(program)
            .args(args)
            .arg(archive)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    #[test]
    fn tar_gz_holds_relative_names_with_awkward_characters() {
        if find_in_path("tar").is_none() || find_in_path("gzip").is_none() {
            return;
        }
        let (root, sources) = fixture();
        let archive = run(&sources, ArchiveFormat::TarGz).unwrap();

        assert_eq!(archive, root.path().join("Archive.tar.gz"));
        let listed = listing("tar", &["-tzf"], &archive);
        assert!(listed.contains("-folder/inner.txt"), "{listed}");
        assert!(listed.contains("a b*.txt"), "{listed}");
    }

    #[test]
    fn zip_holds_relative_names_with_awkward_characters() {
        let Some(unzip) = find_in_path("unzip") else {
            return;
        };
        if find_in_path("zip").is_none() {
            return;
        }
        let (root, sources) = fixture();
        let archive = run(&sources, ArchiveFormat::Zip).unwrap();

        assert_eq!(archive, root.path().join("Archive.zip"));
        let listed = listing(unzip.to_str().unwrap(), &["-Z1"], &archive);
        assert!(listed.contains("-folder/inner.txt"), "{listed}");
        assert!(listed.contains("a b*.txt"), "{listed}");
    }

    #[test]
    fn seven_zip_holds_relative_names_with_awkward_characters() {
        let Some(program) = seven_zip_program() else {
            return;
        };
        let (root, sources) = fixture();
        let archive = run(&sources, ArchiveFormat::SevenZip).unwrap();

        assert_eq!(archive, root.path().join("Archive.7z"));
        let listed = listing(program.to_str().unwrap(), &["l", "-ba"], &archive);
        assert!(listed.contains("-folder/inner.txt"), "{listed}");
        assert!(listed.contains("a b*.txt"), "{listed}");
        assert!(!listed.contains("./"), "{listed}");
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
        if find_in_path("tar").is_none() || find_in_path("gzip").is_none() {
            return;
        }
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
        assert_eq!(leftovers.len(), 2, "only the source and the archive: {leftovers:?}");
    }

    #[test]
    fn cancelling_stops_the_archiver_and_leaves_nothing_behind() {
        if find_in_path("tar").is_none() || find_in_path("gzip").is_none() {
            return;
        }
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
