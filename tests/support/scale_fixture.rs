use gio::prelude::*;
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};
use lorin_explorer::core::{contracts::*, ports::*, Location, ResolvedLocation};

pub fn entries(root: &Location, count: usize) -> Vec<EntrySnapshot> {
    (0..count)
        .map(|n| EntrySnapshot {
            location: Location::from_uri(&format!("{}/file-{n:05}", root.uri())).unwrap(),
            display_name: format!("file-{n:05}"),
            kind: if n % 20 == 0 {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            size: Some(n as u64),
            modified: None,
            content_type: Some("text/plain".into()),
            capabilities: CapabilitySnapshot {
                supported: [Capability::List, Capability::Copy, Capability::Open]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        })
        .collect()
}

/// A network-free backend that starts responding after one event-loop turn and
/// emits one page per millisecond. Calling the full-list API is a test failure.
pub struct StreamingRepository {
    entries: Rc<Vec<EntrySnapshot>>,
    pub response_started: Rc<Cell<Option<Instant>>>,
    pub delivered: Rc<Cell<usize>>,
}
impl StreamingRepository {
    pub fn new(entries: Vec<EntrySnapshot>) -> Self {
        Self {
            entries: Rc::new(entries),
            response_started: Default::default(),
            delivered: Default::default(),
        }
    }
}
impl DirectoryRepository for StreamingRepository {
    fn list(&self, _: DirectoryRequest, _: DirectoryCallback) {
        panic!("streaming workflow must not request a complete remote enumeration");
    }
    fn list_pages(&self, request: DirectoryRequest, mut sink: DirectoryPageSink) {
        let entries = self.entries.clone();
        let response_started = self.response_started.clone();
        let delivered = self.delivered.clone();
        response_started.set(None);
        delivered.set(0);
        glib::MainContext::ref_thread_default().spawn_local(async move {
            let mut offset = 0;
            loop {
                glib::timeout_future(Duration::from_millis(1)).await;
                if request.cancellable.is_cancelled() {
                    sink(RequestCompletion {
                        id: request.id,
                        outcome: RequestOutcome::Cancelled,
                    });
                    break;
                }
                response_started.set(response_started.get().or_else(|| Some(Instant::now())));
                let end = (offset + 128).min(entries.len());
                let next = (end < entries.len()).then_some(PageRequest {
                    offset: end,
                    limit: 128,
                });
                let page = DirectorySnapshot {
                    location: ResolvedLocation {
                        requested: request.location.clone(),
                        resolved: request.location.clone(),
                    },
                    entries: entries[offset..end].to_vec(),
                    next_page: next,
                    capabilities: CapabilitySnapshot {
                        supported: [Capability::List].into_iter().collect(),
                        ..Default::default()
                    },
                };
                delivered.set(end);
                sink(RequestCompletion {
                    id: request.id,
                    outcome: RequestOutcome::Succeeded(page),
                });
                if next.is_none() {
                    break;
                }
                offset = end;
            }
        });
    }
}

pub fn environment() -> lorin_explorer::application::performance::BenchmarkEnvironment {
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .unwrap_or_default()
        .lines()
        .find(|line| line.starts_with("model name"))
        .unwrap_or("CPU unknown")
        .to_owned();
    let ram = std::fs::read_to_string("/proc/meminfo")
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or("RAM unknown")
        .to_owned();
    let os = std::fs::read_to_string("/etc/os-release")
        .unwrap_or_default()
        .lines()
        .find(|line| line.starts_with("PRETTY_NAME="))
        .unwrap_or("OS unknown")
        .to_owned();
    lorin_explorer::application::performance::BenchmarkEnvironment { cpu, cores: std::thread::available_parallelism().map_or(1, usize::from), ram, os, gtk: format!("{}.{}.{}", gtk::major_version(), gtk::minor_version(), gtk::micro_version()), renderer: std::env::var("GSK_RENDERER").unwrap_or_else(|_| "GTK default".into()), fixture: "10,000 entries; 128 per page; 50 tabs; 100 records".into(), cache_state: "cold: new app model and empty thumbnail cache; warm: same view after one completed listing; kernel filesystem cache retained".into() }
}
