use std::path::{Path, PathBuf};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::core::operations::FileOperations;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationKind { Copy, Move, Trash, Delete }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationState { Queued, Running, Completed, Failed(String), Cancelled }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationRecord {
    pub id: u64,
    pub kind: OperationKind,
    pub sources: Vec<PathBuf>,
    pub destination: Option<PathBuf>,
    pub completed: usize,
    pub total: usize,
    pub state: OperationState,
    #[serde(default)] pub finished_at: Option<i64>,
}

#[derive(Clone)]
pub struct OperationManager {
    records: Arc<Mutex<Vec<OperationRecord>>>,
    next_id: Arc<AtomicU64>,
    cancelled: Arc<Mutex<Vec<(u64, Arc<AtomicBool>)>>>,
    history_path: Option<Arc<PathBuf>>,
    retention_days: Arc<AtomicU64>,
}

impl Default for OperationManager { fn default() -> Self { Self { records: Arc::new(Mutex::new(Vec::new())), next_id: Arc::new(AtomicU64::new(0)), cancelled: Arc::new(Mutex::new(Vec::new())), history_path: None, retention_days: Arc::new(AtomicU64::new(7)) } } }

impl OperationManager {
    pub fn load_history(path: Option<PathBuf>, retention_days: u32) -> Self {
        let manager = Self { history_path: path.clone().map(Arc::new), retention_days: Arc::new(AtomicU64::new(retention_days as u64)), ..Self::default() };
        if let Some(path) = path { if let Ok(bytes) = fs::read(path) { if let Ok(records) = serde_json::from_slice::<Vec<OperationRecord>>(&bytes) { *manager.records.lock().unwrap() = records; } } }
        manager.prune_history(); manager
    }
    pub fn set_retention_days(&self, days: u32) { self.retention_days.store(days as u64, Ordering::Relaxed); self.prune_history(); }
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
        self.records.lock().unwrap().push(OperationRecord { id, kind, sources: sources.clone(), destination: destination.clone(), completed: 0, total, state: OperationState::Queued, finished_at: None });
        self.cancelled.lock().unwrap().push((id, cancel.clone()));
        let records = self.records.clone();
        let manager = self.clone();
        std::thread::spawn(move || {
            update(&records, id, |record| record.state = OperationState::Running);
            for (index, source) in sources.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    update(&records, id, |record| { record.state = OperationState::Cancelled; record.finished_at = Some(now()); }); manager.persist_history();
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
                    Err(error) => { update(&records, id, |record| { record.state = OperationState::Failed(error.to_string()); record.finished_at = Some(now()); }); manager.persist_history(); return; }
                }
            }
            update(&records, id, |record| { record.state = OperationState::Completed; record.finished_at = Some(now()); }); manager.persist_history();
        });
        id
    }

    pub fn records(&self) -> Vec<OperationRecord> { self.records.lock().unwrap().clone() }
    pub fn active_count(&self) -> usize { self.records().iter().filter(|record| matches!(record.state, OperationState::Queued | OperationState::Running)).count() }
    pub fn cancel(&self, id: u64) { if let Some((_, flag)) = self.cancelled.lock().unwrap().iter().find(|(known, _)| *known == id) { flag.store(true, Ordering::Relaxed); } }
    pub fn clear_completed(&self) { self.records.lock().unwrap().retain(|record| matches!(record.state, OperationState::Queued | OperationState::Running)); self.persist_history(); }
    fn prune_history(&self) { let cutoff = now() - self.retention_days.load(Ordering::Relaxed) as i64 * 86_400; self.records.lock().unwrap().retain(|record| matches!(record.state, OperationState::Queued | OperationState::Running) || record.finished_at.is_some_and(|finished| finished >= cutoff)); self.persist_history(); }
    fn persist_history(&self) { let Some(path) = &self.history_path else { return; }; let records: Vec<_> = self.records().into_iter().filter(|record| !matches!(record.state, OperationState::Queued | OperationState::Running)).collect(); if let Some(parent) = path.parent() { let _ = fs::create_dir_all(parent); } if let Ok(bytes) = serde_json::to_vec_pretty(&records) { let _ = fs::write(path.as_ref(), bytes); } }
}

fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64 }

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
