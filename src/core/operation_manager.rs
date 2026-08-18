use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::core::operations::FileOperations;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind { Copy, Move, Trash, Delete }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationState { Queued, Running, Completed, Failed(String), Cancelled }

#[derive(Clone, Debug)]
pub struct OperationRecord {
    pub id: u64,
    pub kind: OperationKind,
    pub sources: Vec<PathBuf>,
    pub destination: Option<PathBuf>,
    pub completed: usize,
    pub total: usize,
    pub state: OperationState,
}

#[derive(Clone, Default)]
pub struct OperationManager {
    records: Arc<Mutex<Vec<OperationRecord>>>,
    next_id: Arc<AtomicU64>,
    cancelled: Arc<Mutex<Vec<(u64, Arc<AtomicBool>)>>>,
}

impl OperationManager {
    pub fn submit(&self, kind: OperationKind, sources: Vec<PathBuf>, destination: Option<PathBuf>) -> u64 {
        self.submit_inner(kind, sources, destination, None)
    }

    pub fn submit_to_target(&self, kind: OperationKind, source: PathBuf, target: PathBuf) -> u64 {
        self.submit_inner(kind, vec![source], target.parent().map(Path::to_path_buf), Some(target))
    }

    fn submit_inner(&self, kind: OperationKind, sources: Vec<PathBuf>, destination: Option<PathBuf>, explicit_target: Option<PathBuf>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let total = sources.len();
        self.records.lock().unwrap().push(OperationRecord { id, kind, sources: sources.clone(), destination: destination.clone(), completed: 0, total, state: OperationState::Queued });
        self.cancelled.lock().unwrap().push((id, cancel.clone()));
        let records = self.records.clone();
        std::thread::spawn(move || {
            update(&records, id, |record| record.state = OperationState::Running);
            for (index, source) in sources.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    update(&records, id, |record| record.state = OperationState::Cancelled);
                    return;
                }
                let result = match kind {
                    OperationKind::Copy => match &explicit_target { Some(target) => FileOperations::copy_to_target(source, target), None => FileOperations::copy(&[source.clone()], destination.as_deref().expect("copy requires destination")) },
                    OperationKind::Move => match &explicit_target { Some(target) => FileOperations::move_to_target(source, target), None => FileOperations::move_to(&[source.clone()], destination.as_deref().expect("move requires destination")) },
                    OperationKind::Trash => FileOperations::move_to_trash(&[source.clone()]),
                    OperationKind::Delete => FileOperations::delete_permanently(&[source.clone()]),
                };
                match result {
                    Ok(()) => update(&records, id, |record| record.completed = index + 1),
                    Err(error) => { update(&records, id, |record| record.state = OperationState::Failed(error.to_string())); return; }
                }
            }
            update(&records, id, |record| record.state = OperationState::Completed);
        });
        id
    }

    pub fn records(&self) -> Vec<OperationRecord> { self.records.lock().unwrap().clone() }
    pub fn active_count(&self) -> usize { self.records().iter().filter(|record| matches!(record.state, OperationState::Queued | OperationState::Running)).count() }
    pub fn cancel(&self, id: u64) { if let Some((_, flag)) = self.cancelled.lock().unwrap().iter().find(|(known, _)| *known == id) { flag.store(true, Ordering::Relaxed); } }
    pub fn clear_completed(&self) { self.records.lock().unwrap().retain(|record| matches!(record.state, OperationState::Queued | OperationState::Running)); }
}

fn update(records: &Arc<Mutex<Vec<OperationRecord>>>, id: u64, change: impl FnOnce(&mut OperationRecord)) {
    if let Some(record) = records.lock().unwrap().iter_mut().find(|record| record.id == id) { change(record); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn active_count_only_includes_running_operations() {
        let manager = OperationManager::default();
        assert_eq!(manager.active_count(), 0);
    }
}
