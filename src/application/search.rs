//! Search owns a temporary overlay, never navigation history or persisted terms.
use crate::core::{
    contracts::*,
    navigation::{TabContent, TabState},
    ports::SearchRepository,
    FileItem,
};
use gio::prelude::*;
use std::{cell::RefCell, collections::HashSet, rc::Rc, time::Duration};

#[derive(Clone)]
pub struct SearchCoordinator {
    tab: Rc<RefCell<TabState>>,
    repository: Rc<dyn SearchRepository>,
    state: Rc<RefCell<SearchState>>,
}

#[derive(Default)]
struct SearchState {
    request: Option<SearchRequest>,
    pending: bool,
    generation: u64,
    saved: Option<(u64, DirectorySnapshot, Vec<String>)>,
    outcome: Option<SearchOutcome>,
}

impl SearchCoordinator {
    pub fn new(tab: Rc<RefCell<TabState>>, repository: Rc<dyn SearchRepository>) -> Self {
        Self {
            tab,
            repository,
            state: Rc::new(RefCell::new(SearchState::default())),
        }
    }

    pub fn outcome(&self) -> Option<SearchOutcome> {
        self.state.borrow().outcome.clone()
    }

    fn prepare(&self, query: &str, mode: SearchMode, scope: SearchScope) -> Option<SearchRequest> {
        let tab = self.tab.borrow();
        if !tab.has_available_directory() || tab.has_pending_navigation() {
            return None;
        }
        let mut state = self.state.borrow_mut();
        if let Some(previous) = &state.request {
            previous.cancellable.cancel();
        }
        if state.saved.is_none() {
            state.saved = tab.directory.clone().map(|directory| {
                (
                    tab.navigation_generation,
                    directory,
                    tab.selection_keys.clone(),
                )
            });
        }
        state.generation = tab.navigation_generation;
        let request = SearchRequest {
            id: RequestId::allocate(),
            scope,
            query: query.trim().into(),
            mode,
            cancellable: gio::Cancellable::new(),
        };
        state.request = Some(request.clone());
        state.pending = true;
        Some(request)
    }

    pub fn submit(
        &self,
        query: &str,
        mode: SearchMode,
        scope: SearchScope,
        changed: impl FnOnce(SearchOutcome) + 'static,
    ) -> bool {
        let Some(request) = self.prepare(query, mode, scope) else {
            return false;
        };
        self.dispatch(request, changed);
        true
    }

    /// Invalidate the previous query immediately, including during the debounce.
    pub fn submit_debounced(
        &self,
        query: &str,
        mode: SearchMode,
        scope: SearchScope,
        changed: impl FnOnce(SearchOutcome) + 'static,
    ) -> bool {
        let Some(request) = self.prepare(query, mode, scope) else {
            return false;
        };
        let search = self.clone();
        glib::MainContext::ref_thread_default().spawn_local(async move {
            glib::timeout_future(Duration::from_millis(180)).await;
            if !request.cancellable.is_cancelled() {
                search.dispatch(request, changed);
            }
        });
        true
    }

    fn dispatch(&self, request: SearchRequest, changed: impl FnOnce(SearchOutcome) + 'static) {
        let state = Rc::downgrade(&self.state);
        let tab = Rc::downgrade(&self.tab);
        self.repository.search(
            request,
            Box::new(move |completion| {
                let (Some(state), Some(tab)) = (state.upgrade(), tab.upgrade()) else {
                    return;
                };
                let mut state = state.borrow_mut();
                let mut tab = tab.borrow_mut();
                let Some(request) = &state.request else {
                    return;
                };
                if !state.pending
                    || request.id != completion.id
                    || request.cancellable.is_cancelled()
                    || state.generation != tab.navigation_generation
                    || !tab.has_available_directory()
                {
                    return;
                }
                let declared_scope = match &completion.outcome {
                    SearchOutcome::Completed { declared_scope, .. }
                    | SearchOutcome::Incomplete { declared_scope, .. }
                    | SearchOutcome::Unsupported { declared_scope, .. } => declared_scope,
                };
                let mut outcome = if declared_scope == &request.scope {
                    completion.outcome
                } else {
                    SearchOutcome::Incomplete {
                        matches: vec![],
                        declared_scope: request.scope.clone(),
                        reason: SearchIncompleteReason::MalformedPage,
                        retry: RetryClass::AfterRefresh,
                        error: None,
                    }
                };
                let mut items = vec![];
                if let SearchOutcome::Completed { matches, .. }
                | SearchOutcome::Incomplete { matches, .. } = &mut outcome
                {
                    let mut accepted = HashSet::new();
                    matches.retain(|entry| accepted.insert(entry.location.uri().to_owned()));
                    items = matches.iter().map(FileItem::from_snapshot).collect();
                }
                tab.content = TabContent::Search {
                    underlying: tab.location.clone(),
                    items,
                };
                tab.selection_keys.clear();
                state.pending = false;
                state.outcome = Some(outcome.clone());
                drop(tab);
                drop(state);
                changed(outcome);
            }),
        );
    }

    pub fn retry(&self, changed: impl FnOnce(SearchOutcome) + 'static) -> bool {
        let request = self.state.borrow().request.clone();
        let Some(request) = request else {
            return false;
        };
        self.submit(&request.query, request.mode, request.scope, changed)
    }

    pub fn expand_scope(
        &self,
        scope: SearchScope,
        changed: impl FnOnce(SearchOutcome) + 'static,
    ) -> bool {
        let request = self.state.borrow().request.clone();
        let Some(request) = request else {
            return false;
        };
        self.submit(&request.query, request.mode, scope, changed)
    }

    pub fn clear(&self) -> bool {
        let mut state = self.state.borrow_mut();
        let was_active = state.request.is_some();
        if let Some(request) = state.request.take() {
            request.cancellable.cancel();
        }
        state.pending = false;
        state.outcome = None;
        if let Some((generation, directory, selection)) = state.saved.take() {
            let mut tab = self.tab.borrow_mut();
            if generation == tab.navigation_generation && tab.has_available_directory() {
                tab.directory = Some(directory);
                tab.selection_keys = selection;
                tab.restore_directory();
            }
        }
        was_active
    }
}
