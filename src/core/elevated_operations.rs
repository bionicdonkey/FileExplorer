use std::io;
use std::path::Path;
use std::process::Command;

use crate::core::contracts::{MutationKind, OperationRequest};
use crate::core::operation_manager::{OperationKind, OperationRecord, OperationState};

pub fn retry_request_command(request: &OperationRequest) -> Option<Command> {
    if request.sources.len() != 1
        || !request
            .sources
            .iter()
            .chain(request.destination.iter())
            .chain(request.explicit_target.iter())
            .all(|l| {
                l.uri().starts_with("file:") && l.local_path().is_some_and(|p| is_local_path(&p))
            })
    {
        return None;
    }
    let source = request.sources[0].local_path()?;
    let verb = match request.kind {
        MutationKind::Copy => "copy",
        MutationKind::Move | MutationKind::Rename => "move",
        MutationKind::Trash => "trash",
        MutationKind::Delete => "delete",
        MutationKind::Create => return None,
    };
    let mut command = Command::new("pkexec");
    command
        .arg(std::env::current_exe().ok()?)
        .arg("--elevated-op")
        .arg(verb)
        .arg(&source);
    if matches!(
        request.kind,
        MutationKind::Copy | MutationKind::Move | MutationKind::Rename
    ) {
        let target = request
            .explicit_target
            .as_ref()
            .and_then(|l| l.local_path())
            .or_else(|| {
                request
                    .destination
                    .as_ref()
                    .and_then(|l| l.local_path())
                    .map(|p| p.join(source.file_name().unwrap_or_default()))
            })?;
        command.arg(target);
    }
    Some(command)
}

/// Local filesystem paths only; GVFS/remote mounts are surfaced as fuse
/// paths containing a `gvfs` component and must never reach the helper.
fn is_local_path(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| component.as_os_str() == "gvfs")
}

pub fn is_permission_denied(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied
}

pub fn looks_like_permission_error(message: &str) -> bool {
    message == "operation.permission-denied" || message.to_lowercase().contains("permission denied")
}

/// A failed record is eligible for a privileged retry only when it is a
/// single, local, permission-denied operation. Multi-item batches and any
/// remote/GVfs source or destination are rejected so the helper never runs
/// against something outside the user's own filesystem.
pub fn is_eligible_for_retry(record: &OperationRecord) -> bool {
    let OperationState::Failed(message) = &record.state else {
        return false;
    };
    if !looks_like_permission_error(message) {
        return false;
    }
    if record.sources.len() != 1 {
        return false;
    }
    if !record
        .source_uris
        .iter()
        .chain(record.destination_uri.iter())
        .all(|uri| uri.starts_with("file:"))
    {
        return false;
    }
    if !record.sources.iter().all(|source| is_local_path(source)) {
        return false;
    }
    record.destination.as_deref().map_or(true, is_local_path)
}

fn verb_for(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Copy => "copy",
        OperationKind::Move => "move",
        OperationKind::Trash => "trash",
        OperationKind::Delete => "delete",
    }
}

/// Builds the `pkexec` invocation that re-runs this same binary in
/// `--elevated-op` mode for exactly the one validated source (and
/// destination, when the operation needs one). The helper never runs the
/// whole application elevated; it performs a single retried operation and
/// exits.
pub fn retry_command(record: &OperationRecord) -> Option<Command> {
    if !is_eligible_for_retry(record) {
        return None;
    }
    let source = record.sources.first()?;
    let executable = std::env::current_exe().ok()?;
    let mut command = Command::new("pkexec");
    command
        .arg(executable)
        .arg("--elevated-op")
        .arg(verb_for(record.kind))
        .arg(source);
    if matches!(record.kind, OperationKind::Copy | OperationKind::Move) {
        let destination = record.destination.as_ref()?;
        command.arg(destination.join(source.file_name()?));
    }
    Some(command)
}

/// Entry point for the `--elevated-op` helper invocation launched by
/// `pkexec`. Returns `None` when the arguments don't request the helper, so
/// the caller falls through to the normal GTK application startup.
pub fn run_from_args(args: &[String]) -> Option<i32> {
    if args.first().map(String::as_str) != Some("--elevated-op") {
        return None;
    }
    let verb = args.get(1)?;
    let source = args.get(2).map(Path::new)?;
    let destination = args.get(3).map(Path::new);
    Some(
        match ElevatedOperations::perform(verb, source, destination) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("Elevated operation failed: {error}");
                1
            }
        },
    )
}

pub struct ElevatedOperations;

impl ElevatedOperations {
    /// Performs the single validated local operation. Called only from the
    /// `--elevated-op` helper entry point, which already runs with the
    /// privileges `pkexec` granted for this one retry.
    pub fn perform(verb: &str, source: &Path, destination: Option<&Path>) -> io::Result<()> {
        if !is_local_path(source) || destination.is_some_and(|path| !is_local_path(path)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "elevated retry only supports local paths",
            ));
        }
        match verb {
            "copy" => {
                let destination = destination.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "copy requires a destination")
                })?;
                crate::core::operations::FileOperations::copy_to_target(source, destination)
            }
            "move" => {
                let destination = destination.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "move requires a destination")
                })?;
                crate::core::operations::FileOperations::move_to_target(source, destination)
            }
            "trash" => {
                crate::core::operations::FileOperations::move_to_trash(&[source.to_path_buf()])
            }
            "delete" => {
                crate::core::operations::FileOperations::delete_permanently(&[source.to_path_buf()])
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unknown elevated operation",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn failed_record(
        sources: Vec<PathBuf>,
        destination: Option<PathBuf>,
        message: &str,
    ) -> OperationRecord {
        OperationRecord {
            id: 1,
            kind: OperationKind::Copy,
            source_uris: Vec::new(),
            sources,
            destination_uri: None,
            destination,
            completed: 0,
            total: 1,
            state: OperationState::Failed(message.into()),
            finished_at: None,
        }
    }

    #[test]
    fn permission_denied_errors_are_detected() {
        let error = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        assert!(is_permission_denied(&error));
        assert!(!is_permission_denied(&io::Error::new(
            io::ErrorKind::NotFound,
            "missing"
        )));
    }

    #[test]
    fn a_single_local_permission_failure_is_eligible() {
        let record = failed_record(
            vec![PathBuf::from("/etc/hosts")],
            Some(PathBuf::from("/tmp")),
            "Permission denied (os error 13)",
        );
        assert!(is_eligible_for_retry(&record));
    }

    #[test]
    fn a_gvfs_source_is_never_eligible() {
        let record = failed_record(
            vec![PathBuf::from("/run/user/1000/gvfs/smb-share/report.txt")],
            None,
            "Permission denied",
        );
        assert!(!is_eligible_for_retry(&record));
    }

    #[test]
    fn a_gvfs_destination_is_never_eligible() {
        let record = failed_record(
            vec![PathBuf::from("/etc/hosts")],
            Some(PathBuf::from("/run/user/1000/gvfs/smb-share")),
            "Permission denied",
        );
        assert!(!is_eligible_for_retry(&record));
    }

    #[test]
    fn a_multi_item_batch_is_never_eligible() {
        let record = failed_record(
            vec![PathBuf::from("/etc/hosts"), PathBuf::from("/etc/passwd")],
            None,
            "Permission denied",
        );
        assert!(!is_eligible_for_retry(&record));
    }

    #[test]
    fn a_non_permission_failure_is_not_eligible() {
        let record = failed_record(
            vec![PathBuf::from("/etc/hosts")],
            None,
            "No such file or directory",
        );
        assert!(!is_eligible_for_retry(&record));
    }

    #[test]
    fn non_helper_arguments_fall_through_to_normal_startup() {
        assert_eq!(run_from_args(&[]), None);
        assert_eq!(run_from_args(&["--verbose".into()]), None);
    }

    #[test]
    fn a_helper_invocation_without_a_source_is_rejected() {
        assert_eq!(
            run_from_args(&["--elevated-op".into(), "trash".into()]),
            None
        );
    }

    #[test]
    fn a_successful_record_is_not_eligible() {
        let record = OperationRecord {
            id: 1,
            kind: OperationKind::Copy,
            source_uris: Vec::new(),
            sources: vec![PathBuf::from("/etc/hosts")],
            destination_uri: None,
            destination: None,
            completed: 1,
            total: 1,
            state: OperationState::Completed,
            finished_at: None,
        };
        assert!(!is_eligible_for_retry(&record));
    }
}
