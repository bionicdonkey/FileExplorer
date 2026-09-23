//! Bounded chunks for main-context model work and URI-keyed visible-row caches.
use crate::core::{FileItem, Location, ViewOptions};
use std::collections::VecDeque;
use std::time::Duration;

pub const PAGE_SIZE: usize = 128;

/// A model population cursor. Filtering is performed only on the next chunk;
/// GTK's incremental sorter merges those chunks without blocking input.
pub struct PagedSnapshot {
    items: std::vec::IntoIter<FileItem>,
    show_hidden: bool,
}
impl PagedSnapshot {
    pub fn new(items: Vec<FileItem>, options: &ViewOptions) -> Self {
        Self {
            items: items.into_iter(),
            show_hidden: options.show_hidden,
        }
    }
    pub fn next_page(&mut self) -> Option<Vec<FileItem>> {
        if self.items.len() == 0 {
            return None;
        }
        Some(
            self.items
                .by_ref()
                .take(PAGE_SIZE)
                .filter(|item| self.show_hidden || !item.name().starts_with('.'))
                .collect(),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisibleRange {
    pub start: usize,
    pub end: usize,
}
impl VisibleRange {
    pub fn clamped(self, count: usize) -> std::ops::Range<usize> {
        self.start.min(count)..self.end.max(self.start).min(count)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CachePolicy {
    pub max_entries: usize,
}
impl Default for CachePolicy {
    fn default() -> Self {
        Self { max_entries: 256 }
    }
}

pub struct VisibleCache<T> {
    policy: CachePolicy,
    entries: VecDeque<(Location, T)>,
}
impl<T: Clone> VisibleCache<T> {
    pub fn new(policy: CachePolicy) -> Self {
        Self {
            policy,
            entries: VecDeque::new(),
        }
    }
    pub fn get(&mut self, location: &Location) -> Option<T> {
        let index = self.entries.iter().position(|(key, _)| key == location)?;
        let entry = self.entries.remove(index)?;
        let value = entry.1.clone();
        self.entries.push_back(entry);
        Some(value)
    }
    pub fn insert(&mut self, location: Location, value: T) {
        self.entries.retain(|(key, _)| key != &location);
        self.entries.push_back((location, value));
        while self.entries.len() > self.policy.max_entries {
            self.entries.pop_front();
        }
    }
    pub fn invalidate(&mut self) {
        self.entries.clear();
    }
}

#[derive(Clone, Debug)]
pub struct BenchmarkEnvironment {
    pub cpu: String,
    pub cores: usize,
    pub ram: String,
    pub os: String,
    pub gtk: String,
    pub renderer: String,
    pub fixture: String,
    pub cache_state: String,
}

pub fn assert_p95(label: &str, runs: &[Duration], budget: Duration) -> Duration {
    assert!(
        runs.len() >= 30,
        "{label}: collect at least 30 measured runs after warm-up"
    );
    let mut sorted = runs.to_vec();
    sorted.sort_unstable();
    let p95 = sorted[(sorted.len() * 95).div_ceil(100) - 1];
    println!("{label}: n={}, p95={p95:?}, budget={budget:?}", runs.len());
    assert!(p95 <= budget, "{label}: p95 {p95:?} exceeds {budget:?}");
    p95
}
