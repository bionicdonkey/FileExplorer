//! Main-context navigation ownership; the repository owns all filesystem work.
use crate::core::{
    contracts::*,
    navigation::{NavigationRequest, TabState},
    ports::DirectoryRepository,
    Location,
};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
pub struct NavigationCoordinator {
    pub state: Rc<RefCell<TabState>>,
    repository: Rc<dyn DirectoryRepository>,
}
impl NavigationCoordinator {
    pub fn new(state: Rc<RefCell<TabState>>, repository: Rc<dyn DirectoryRepository>) -> Self {
        Self { state, repository }
    }
    pub fn navigate(&self, location: Location, changed: impl FnMut(bool) + 'static) -> bool {
        let request = self.state.borrow_mut().begin_navigation(location);
        self.dispatch(request, changed)
    }
    pub fn refresh(&self, changed: impl FnMut(bool) + 'static) -> bool {
        let request = self.state.borrow_mut().begin_refresh();
        self.dispatch(request, changed)
    }
    pub fn back(&self, changed: impl FnMut(bool) + 'static) -> bool {
        let request = self.state.borrow_mut().begin_back();
        self.dispatch(request, changed)
    }
    pub fn forward(&self, changed: impl FnMut(bool) + 'static) -> bool {
        let request = self.state.borrow_mut().begin_forward();
        self.dispatch(request, changed)
    }
    pub fn close_tab(&self) {
        self.state.borrow_mut().close();
    }
    pub fn watch(
        &self,
        location: &Location,
        changed: impl Fn() + 'static,
    ) -> Option<gio::FileMonitor> {
        self.repository.watch(location, Box::new(changed))
    }
    pub fn inspect_visible_metadata(
        &self,
        request: VisibleMetadataRequest,
        done: crate::core::ports::VisibleMetadataCallback,
    ) {
        self.repository.inspect_visible_metadata(request, done);
    }
    pub fn retry(&self, changed: impl FnMut(bool) + 'static) -> bool {
        let request = self.state.borrow_mut().begin_retry();
        self.dispatch(request, changed)
    }
    pub fn apply_directory_result(
        &self,
        generation: u64,
        result: RequestCompletion<DirectorySnapshot>,
    ) -> bool {
        self.state
            .borrow_mut()
            .apply_directory_result(generation, result)
    }
    fn dispatch(
        &self,
        request: Option<NavigationRequest>,
        mut changed: impl FnMut(bool) + 'static,
    ) -> bool {
        let Some(request) = request else {
            return false;
        };
        let state = Rc::downgrade(&self.state);
        self.repository.list_pages(
            request.request,
            Box::new(move |result| {
                let accepted = state.upgrade().is_some_and(|state| {
                    state
                        .borrow_mut()
                        .apply_directory_page(request.generation, result)
                });
                changed(accepted);
            }),
        );
        true
    }
}
