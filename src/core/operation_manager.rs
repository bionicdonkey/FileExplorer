use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::elevated_operations;
use crate::core::operations::FileOperations;
use crate::core::{Location, RecoverableStateNotice};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct StoredOperationHistoryV2 {
    schema_version: u32,
    records: Vec<StoredOperationRecordV2>,
}
#[derive(Serialize, Deserialize)]
struct StoredOperationRecordV2 {
    id: u64,
    kind: OperationKind,
    sources: Vec<String>,
    destination: Option<String>,
    completed: usize,
    total: usize,
    state: OperationState,
    finished_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationKind {
    Copy,
    Move,
    Trash,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationState {
    Queued,
    Running,
    Completed,
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationRecord {
    pub id: u64,
    pub kind: OperationKind,
    pub sources: Vec<PathBuf>,
    pub destination: Option<PathBuf>,
    pub completed: usize,
    pub total: usize,
    pub state: OperationState,
    #[serde(default)]
    pub finished_at: Option<i64>,
}

#[derive(Clone)]
pub struct OperationManager {
    records: Arc<Mutex<Vec<OperationRecord>>>,
    next_id: Arc<AtomicU64>,
    cancelled: Arc<Mutex<Vec<(u64, Arc<AtomicBool>)>>>,
    history_path: Option<Arc<PathBuf>>,
    retention_days: Arc<AtomicU64>,
    recoverable_notices: Arc<Mutex<Vec<RecoverableStateNotice>>>,
}

impl Default for OperationManager {
    fn default() -> Self {
        Self {
            records: Arc::new(Mutex::new(Vec::new())),
            next_id: Arc::new(AtomicU64::new(0)),
            cancelled: Arc::new(Mutex::new(Vec::new())),
            history_path: None,
            retention_days: Arc::new(AtomicU64::new(7)),
            recoverable_notices: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl OperationManager {
    pub fn load_history(path: Option<PathBuf>, retention_days: u32) -> Self {
        let manager = Self {
            history_path: path.clone().map(Arc::new),
            retention_days: Arc::new(AtomicU64::new(retention_days as u64)),
            ..Self::default()
        };
        if let Some(path) = path {
            if let Ok(bytes) = fs::read(path) {
                let (records, notices) = decode_history(&bytes);
                *manager.records.lock().unwrap() = records;
                *manager.recoverable_notices.lock().unwrap() = notices;
            }
        }
        // next_id otherwise always restarts at 0, so the first operation of
        // every session would be id 1 -- colliding with whatever record
        // already has id 1 in the loaded history. update() finds the first
        // matching id, so every state write for the new operation would
        // silently land on the old, already-finished record instead: it
        // never appears to start, Cancel affects the wrong row, and the new
        // operation never gets a row of its own in the Operations page.
        let highest_loaded_id = manager
            .records
            .lock()
            .unwrap()
            .iter()
            .map(|record| record.id)
            .max()
            .unwrap_or(0);
        manager.next_id.store(highest_loaded_id, Ordering::Relaxed);
        manager.prune_history();
        manager
    }
    pub fn set_retention_days(&self, days: u32) {
        self.retention_days.store(days as u64, Ordering::Relaxed);
        self.prune_history();
    }
    pub fn submit(
        &self,
        kind: OperationKind,
        sources: Vec<PathBuf>,
        destination: Option<PathBuf>,
    ) -> u64 {
        self.submit_inner(kind, sources, destination, None)
    }

    pub fn submit_to_target(&self, kind: OperationKind, source: PathBuf, target: PathBuf) -> u64 {
        self.submit_inner(
            kind,
            vec![source],
            target.parent().map(Path::to_path_buf),
            Some(target),
        )
    }

    fn submit_inner(
        &self,
        kind: OperationKind,
        sources: Vec<PathBuf>,
        destination: Option<PathBuf>,
        explicit_target: Option<PathBuf>,
    ) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let cancel = Arc::new(AtomicBool::new(false));
        // A placeholder; count_bytes() walks the whole tree and must not run
        // on the caller's thread (the GTK main loop), or submitting a large
        // folder copy freezes the app before the record even appears queued.
        let total = sources.len().max(1);
        self.records.lock().unwrap().push(OperationRecord {
            id,
            kind,
            sources: sources.clone(),
            destination: destination.clone(),
            completed: 0,
            total,
            state: OperationState::Queued,
            finished_at: None,
        });
        self.cancelled.lock().unwrap().push((id, cancel.clone()));
        let records = self.records.clone();
        let manager = self.clone();
        std::thread::spawn(move || {
            update(&records, id, |record| {
                record.state = OperationState::Running
            });
            if kind == OperationKind::Copy {
                // Progress is tracked in bytes, not file count: a single huge
                // file is one "file" but the transfer still needs to show
                // motion throughout, not just jump from 0% to 100%.
                let counted_total = (FileOperations::count_bytes(&sources) as usize).max(1);
                update(&records, id, |record| record.total = counted_total);
                let progress_records = records.clone();
                let mut completed = 0usize;
                let mut on_progress = move |bytes: u64| {
                    completed += bytes as usize;
                    update(&progress_records, id, |record| record.completed = completed);
                };
                let result = match &explicit_target {
                    Some(target) => sources.first().map_or(Ok(()), |source| {
                        FileOperations::copy_to_target_with_progress(
                            source,
                            target,
                            &cancel,
                            &mut on_progress,
                        )
                    }),
                    None => FileOperations::copy_with_progress(
                        &sources,
                        destination.as_deref().expect("copy requires destination"),
                        &cancel,
                        &mut on_progress,
                    ),
                };
                finish(&records, &manager, id, result);
                return;
            }
            for (index, source) in sources.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    update(&records, id, |record| {
                        record.state = OperationState::Cancelled;
                        record.finished_at = Some(now());
                    });
                    manager.persist_history();
                    return;
                }
                let result = match kind {
                    OperationKind::Move => match &explicit_target {
                        Some(target) => FileOperations::move_to_target(source, target),
                        None => FileOperations::move_to(
                            &[source.clone()],
                            destination.as_deref().expect("move requires destination"),
                        ),
                    },
                    OperationKind::Trash => FileOperations::move_to_trash(&[source.clone()]),
                    OperationKind::Delete => FileOperations::delete_permanently(&[source.clone()]),
                    OperationKind::Copy => unreachable!("copy is handled above"),
                };
                match result {
                    Ok(()) => update(&records, id, |record| record.completed = index + 1),
                    Err(error) => {
                        update(&records, id, |record| {
                            record.state = OperationState::Failed(error.to_string());
                            record.finished_at = Some(now());
                        });
                        manager.persist_history();
                        return;
                    }
                }
            }
            update(&records, id, |record| {
                record.state = OperationState::Completed;
                record.finished_at = Some(now());
            });
            manager.persist_history();
        });
        id
    }

    /// Re-attempts a single failed, local, permission-denied record through
    /// the polkit-backed helper. Ineligible records (batches, remote/GVfs
    /// paths, non-permission failures) are rejected before anything runs.
    pub fn retry_elevated(&self, id: u64) -> Result<(), String> {
        let record = self
            .records
            .lock()
            .unwrap()
            .iter()
            .find(|record| record.id == id)
            .cloned()
            .ok_or_else(|| "operation record not found".to_string())?;
        let mut command = elevated_operations::retry_command(&record).ok_or_else(|| {
            "this operation cannot be retried with elevated privileges".to_string()
        })?;
        update(&self.records, id, |record| {
            record.state = OperationState::Running
        });
        let records = self.records.clone();
        let manager = self.clone();
        std::thread::spawn(move || {
            let outcome = command.status();
            match outcome {
                Ok(status) if status.success() => update(&records, id, |record| {
                    record.state = OperationState::Completed;
                    record.finished_at = Some(now());
                }),
                Ok(status) => update(&records, id, |record| {
                    record.state =
                        OperationState::Failed(format!("Elevated retry exited with {status}"));
                    record.finished_at = Some(now());
                }),
                Err(error) => update(&records, id, |record| {
                    record.state = OperationState::Failed(error.to_string());
                    record.finished_at = Some(now());
                }),
            }
            manager.persist_history();
        });
        Ok(())
    }

    pub fn records(&self) -> Vec<OperationRecord> {
        self.records.lock().unwrap().clone()
    }
    pub fn recoverable_notices(&self) -> Vec<RecoverableStateNotice> {
        self.recoverable_notices.lock().unwrap().clone()
    }
    pub fn active_count(&self) -> usize {
        self.records()
            .iter()
            .filter(|record| {
                matches!(
                    record.state,
                    OperationState::Queued | OperationState::Running
                )
            })
            .count()
    }
    /// Signals the operation's own thread to stop *and* immediately marks
    /// the record Cancelled, rather than waiting for that thread to notice
    /// the flag. If the thread is still running, its own completion write
    /// will simply overwrite this again; if it never got scheduled (or died
    /// without reporting), the record no longer sits stuck forever with a
    /// Cancel button that visibly does nothing.
    pub fn cancel(&self, id: u64) {
        if let Some((_, flag)) = self
            .cancelled
            .lock()
            .unwrap()
            .iter()
            .find(|(known, _)| *known == id)
        {
            flag.store(true, Ordering::Relaxed);
        }
        update(&self.records, id, |record| {
            if matches!(
                record.state,
                OperationState::Queued | OperationState::Running
            ) {
                record.state = OperationState::Cancelled;
                record.finished_at = Some(now());
            }
        });
        self.persist_history();
    }
    /// Signals every currently active record to stop. Cancellation still
    /// happens on each operation's own background thread, so callers that
    /// need to close the window immediately should not wait on this.
    pub fn cancel_all_active(&self) {
        for (_, flag) in self.cancelled.lock().unwrap().iter() {
            flag.store(true, Ordering::Relaxed);
        }
    }
    pub fn clear_completed(&self) {
        self.records.lock().unwrap().retain(|record| {
            matches!(
                record.state,
                OperationState::Queued | OperationState::Running
            )
        });
        self.persist_history();
    }
    fn prune_history(&self) {
        let cutoff = now() - self.retention_days.load(Ordering::Relaxed) as i64 * 86_400;
        self.records.lock().unwrap().retain(|record| {
            matches!(
                record.state,
                OperationState::Queued | OperationState::Running
            ) || record
                .finished_at
                .is_some_and(|finished| finished >= cutoff)
        });
        self.persist_history();
    }
    fn persist_history(&self) {
        let Some(path) = &self.history_path else {
            return;
        };
        let records: Vec<_> = self
            .records()
            .into_iter()
            .filter(|record| {
                !matches!(
                    record.state,
                    OperationState::Queued | OperationState::Running
                )
            })
            .collect();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&StoredOperationHistoryV2 {
            schema_version: 2,
            records: records.iter().map(stored_record).collect(),
        }) {
            let _ = fs::write(path.as_ref(), bytes);
        }
    }
}

fn stored_record(record: &OperationRecord) -> StoredOperationRecordV2 {
    StoredOperationRecordV2 {
        id: record.id,
        kind: record.kind,
        sources: record
            .sources
            .iter()
            .map(|path| Location::from_local_path(path).uri().to_owned())
            .collect(),
        destination: record
            .destination
            .as_ref()
            .map(|path| Location::from_local_path(path).uri().to_owned()),
        completed: record.completed,
        total: record.total,
        state: record.state.clone(),
        finished_at: record.finished_at,
    }
}

fn decode_history(bytes: &[u8]) -> (Vec<OperationRecord>, Vec<RecoverableStateNotice>) {
    if let Ok(records) = serde_json::from_slice::<Vec<OperationRecord>>(bytes) {
        return (records, Vec::new());
    }
    let Ok(stored) = serde_json::from_slice::<StoredOperationHistoryV2>(bytes) else {
        return (Vec::new(), vec![RecoverableStateNotice::MalformedRecord]);
    };
    if stored.schema_version != 2 {
        return (Vec::new(), vec![RecoverableStateNotice::UnsupportedSchema]);
    }
    let mut records = Vec::new();
    let mut notices = Vec::new();
    for record in stored.records {
        let sources = record
            .sources
            .iter()
            .filter_map(|uri| {
                Location::from_uri(uri)
                    .ok()
                    .and_then(|location| location.local_path())
            })
            .collect::<Vec<_>>();
        let destination = record
            .destination
            .as_deref()
            .and_then(|uri| Location::from_uri(uri).ok())
            .and_then(|location| location.local_path());
        if sources.is_empty() || (record.destination.is_some() && destination.is_none()) {
            notices.push(RecoverableStateNotice::MalformedRecord);
            continue;
        }
        records.push(OperationRecord {
            id: record.id,
            kind: record.kind,
            sources,
            destination,
            completed: record.completed,
            total: record.total,
            state: record.state,
            finished_at: record.finished_at,
        });
    }
    (records, notices)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn update(
    records: &Arc<Mutex<Vec<OperationRecord>>>,
    id: u64,
    change: impl FnOnce(&mut OperationRecord),
) {
    if let Some(record) = records
        .lock()
        .unwrap()
        .iter_mut()
        .find(|record| record.id == id)
    {
        change(record);
    }
}

fn finish(
    records: &Arc<Mutex<Vec<OperationRecord>>>,
    manager: &OperationManager,
    id: u64,
    result: io::Result<()>,
) {
    let state = match result {
        Ok(()) => OperationState::Completed,
        Err(error) if error.kind() == io::ErrorKind::Interrupted => OperationState::Cancelled,
        Err(error) => OperationState::Failed(error.to_string()),
    };
    update(records, id, |record| {
        record.state = state;
        record.finished_at = Some(now());
    });
    manager.persist_history();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn active_count_only_includes_running_operations() {
        let manager = OperationManager::default();
        assert_eq!(manager.active_count(), 0);
    }

    #[test]
    fn a_new_operation_never_collides_with_an_id_from_loaded_history() {
        // Regression guard: next_id always restarted at 0, so the first
        // operation of a fresh session got id 1 -- the same id almost every
        // previous session's first operation also used. update() finds the
        // *first* record with a matching id, so every write for the new
        // operation silently landed on the old, already-finished one:
        // it never appeared to start, Cancel hit the wrong row, and it
        // never got a row of its own in the Operations page.
        let directory = tempfile::tempdir().unwrap();
        let history_path = directory.path().join("history.json");
        let previous_session = vec![OperationRecord {
            id: 1,
            kind: OperationKind::Copy,
            sources: vec![PathBuf::from("/tmp/old.zip")],
            destination: Some(PathBuf::from("/tmp")),
            completed: 10,
            total: 10,
            state: OperationState::Completed,
            finished_at: Some(now()),
        }];
        std::fs::write(
            &history_path,
            serde_json::to_vec(&previous_session).unwrap(),
        )
        .unwrap();

        let manager = OperationManager::load_history(Some(history_path), 7);
        let new_id = manager.submit(
            OperationKind::Copy,
            vec![PathBuf::from("/tmp/new.zip")],
            Some(PathBuf::from("/tmp")),
        );

        assert_ne!(
            new_id, 1,
            "new operation reused an id already present in loaded history"
        );
        let record = wait_for_finished(&manager, new_id);
        assert_eq!(record.sources, vec![PathBuf::from("/tmp/new.zip")]);
        // The old record must be untouched by the new operation's updates.
        let old_record = manager
            .records()
            .into_iter()
            .find(|record| record.id == 1)
            .unwrap();
        assert_eq!(old_record.sources, vec![PathBuf::from("/tmp/old.zip")]);
        assert_eq!(old_record.completed, 10);
    }

    #[test]
    fn cancel_immediately_marks_the_record_cancelled_even_if_no_thread_is_listening() {
        // Regression guard: cancel() used to only flip an atomic flag for the
        // operation's own thread to notice. If that thread never got
        // scheduled (or died before finishing), Cancel visibly did nothing
        // and the record stayed Queued/Running forever.
        let manager = OperationManager::default();
        manager.records.lock().unwrap().push(OperationRecord {
            id: 1,
            kind: OperationKind::Copy,
            sources: vec![PathBuf::from("/tmp/big.bin")],
            destination: Some(PathBuf::from("/tmp/dest")),
            completed: 0,
            total: 100,
            state: OperationState::Queued,
            finished_at: None,
        });

        manager.cancel(1);

        let record = manager
            .records()
            .into_iter()
            .find(|record| record.id == 1)
            .unwrap();
        assert_eq!(record.state, OperationState::Cancelled);
        assert!(record.finished_at.is_some());
        assert_eq!(manager.active_count(), 0);
    }

    #[test]
    fn retrying_an_unknown_record_is_rejected() {
        let manager = OperationManager::default();
        assert!(manager.retry_elevated(404).is_err());
    }

    #[test]
    fn retrying_a_non_permission_failure_is_rejected() {
        let manager = OperationManager::default();
        manager.records.lock().unwrap().push(OperationRecord {
            id: 1,
            kind: OperationKind::Delete,
            sources: vec![PathBuf::from("/tmp/report.txt")],
            destination: None,
            completed: 0,
            total: 1,
            state: OperationState::Failed("No such file or directory".into()),
            finished_at: None,
        });
        assert!(manager.retry_elevated(1).is_err());
    }

    #[test]
    fn history_writes_v2_canonical_uris_and_migrates_legacy_paths() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        let legacy = vec![OperationRecord {
            id: 3,
            kind: OperationKind::Copy,
            sources: vec![PathBuf::from("/tmp/source")],
            destination: Some(PathBuf::from("/tmp/destination")),
            completed: 1,
            total: 1,
            state: OperationState::Completed,
            finished_at: Some(now()),
        }];
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let manager = OperationManager::load_history(Some(path.clone()), 7);
        assert_eq!(
            manager.records()[0].sources,
            vec![PathBuf::from("/tmp/source")]
        );
        manager.persist_history();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["schema_version"], 2);
        assert_eq!(
            saved["records"][0]["sources"],
            serde_json::json!(["file:///tmp/source"])
        );
    }

    #[test]
    fn cancel_all_active_signals_every_tracked_operation() {
        let manager = OperationManager::default();
        let flag_one = Arc::new(AtomicBool::new(false));
        let flag_two = Arc::new(AtomicBool::new(false));
        manager
            .cancelled
            .lock()
            .unwrap()
            .push((1, flag_one.clone()));
        manager
            .cancelled
            .lock()
            .unwrap()
            .push((2, flag_two.clone()));

        manager.cancel_all_active();

        assert!(flag_one.load(Ordering::Relaxed));
        assert!(flag_two.load(Ordering::Relaxed));
    }

    #[test]
    fn copying_many_files_is_observable_mid_transfer_through_records() {
        // Regression guard for "the progress bar doesn't update": poll
        // records() the way the Operations page does and confirm we can
        // observe completed strictly between 0 and total at least once,
        // not just an initial 0 followed immediately by "Completed".
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        for index in 0..2000 {
            fs::write(source.join(format!("file-{index}.txt")), "x").unwrap();
        }
        let manager = OperationManager::default();

        let id = manager.submit(OperationKind::Copy, vec![source], Some(destination));
        let mut saw_mid_transfer_progress = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if let Some(record) = manager.records().into_iter().find(|record| record.id == id) {
                if record.completed > 0 && record.completed < record.total {
                    saw_mid_transfer_progress = true;
                }
                if record.finished_at.is_some() {
                    break;
                }
            }
        }

        assert!(
            saw_mid_transfer_progress,
            "never observed an intermediate completed/total value while the copy was running"
        );
    }

    #[test]
    fn copying_a_folder_reports_byte_progress_not_per_top_level_source() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("a.txt"), "12").unwrap();
        fs::write(source.join("b.txt"), "345").unwrap();
        let manager = OperationManager::default();

        let id = manager.submit(OperationKind::Copy, vec![source], Some(destination));
        let record = wait_for_finished(&manager, id);

        assert_eq!(
            record.total, 5,
            "total should be bytes copied (2 + 3), not the single top-level source or file count"
        );
        assert_eq!(record.completed, 5);
        assert_eq!(record.state, OperationState::Completed);
    }

    #[test]
    fn copying_a_single_large_file_reports_byte_progress_not_a_single_frozen_unit() {
        // Regression guard for the reported bug: a single big file used to
        // be "1 of 1" the whole transfer (no visible progress until done).
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("big.bin");
        let destination = directory.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, vec![0u8; 5 * (1 << 20)]).unwrap();
        let manager = OperationManager::default();

        let id = manager.submit(OperationKind::Copy, vec![source], Some(destination));

        let placeholder_units_total = 1; // sources.len().max(1), the pre-count placeholder
        let mut saw_mid_transfer_progress = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if let Some(record) = manager.records().into_iter().find(|record| record.id == id) {
                if record.total > placeholder_units_total
                    && record.completed > 0
                    && record.completed < record.total
                {
                    saw_mid_transfer_progress = true;
                }
                if record.finished_at.is_some() {
                    break;
                }
            }
        }

        assert!(
            saw_mid_transfer_progress,
            "never observed byte-level progress mid-transfer for a single large file"
        );
    }

    #[test]
    fn submitting_a_copy_does_not_block_on_counting_the_source_tree() {
        // count_bytes() must run on the background thread, not the caller's:
        // otherwise submitting a large folder copy blocks the GTK main loop
        // (reported as the app "freezing" / a transfer that never starts).
        // A few hundred files take long enough to count that a regression
        // here would make this test visibly slow, without making a passing
        // run flaky the way asserting on the placeholder value would.
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        for index in 0..500 {
            fs::write(source.join(format!("file-{index}.txt")), "x").unwrap();
        }
        let manager = OperationManager::default();

        let started = std::time::Instant::now();
        let id = manager.submit(OperationKind::Copy, vec![source], Some(destination));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "submit() should return immediately, not after walking the source tree"
        );

        wait_for_finished(&manager, id);
    }

    fn wait_for_finished(manager: &OperationManager, id: u64) -> OperationRecord {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(record) = manager.records().into_iter().find(|record| record.id == id) {
                if record.finished_at.is_some() {
                    return record;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "operation did not finish in time"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
