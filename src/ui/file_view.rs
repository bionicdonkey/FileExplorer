use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gio::prelude::*;
use glib::prelude::ToValue;
use gtk::prelude::*;
use gtk::{
    Box as GtkBox, Button, ColumnView, ColumnViewColumn, CustomSorter, FlowBox, GestureClick,
    Image, Label, Orientation, Popover, ScrolledWindow, Separator, SignalListItemFactory,
    SingleSelection, SortListModel,
};

use crate::application::navigation::NavigationCoordinator;
use crate::core::directory_model::sort_items;
use crate::core::file_item::FileItem;
use crate::core::navigation::{NavigationStatus, TabContent, TabState};
use crate::core::sidebar_model::SidebarSection;
use crate::core::{
    GroupField, ListColumn, ListColumnLayout, Location, SidebarLocation, SidebarNode,
    SortDirection, SortField, ViewMode, ViewOptions,
};
use crate::infrastructure::gio_directory::GioDirectoryRepository;
use crate::ui::context_menu::{
    configure_command_button, delete_command, item_commands, open_with_apps, CommandAvailability,
    ContextCommand, ContextTarget,
};

#[derive(Clone)]
enum ListRow {
    Group(String),
    File(FileItem),
}

fn filesystem_space(path: &Path) -> Option<(u64, u64)> {
    gio::File::for_path(path)
        .query_filesystem_info("filesystem::size,filesystem::free", gio::Cancellable::NONE)
        .ok()
        .map(|info| {
            (
                info.attribute_uint64("filesystem::size"),
                info.attribute_uint64("filesystem::free"),
            )
        })
        .filter(|(total, _)| *total > 0)
}

/// Produces the one availability snapshot used by the toolbar while retaining
/// the displayed location as the Paste destination even when an item is
/// selected.
fn command_availability_for_target(
    selected_target: Option<&ContextTarget>,
    destination: &Location,
) -> CommandAvailability {
    let selection = match selected_target {
        Some(ContextTarget::File(item) | ContextTarget::Folder(item)) => vec![item.clone()],
        _ => Vec::new(),
    };
    CommandAvailability::for_selection(&selection, Some(destination))
}

#[derive(Clone)]
pub struct FileView {
    pub widget: ScrolledWindow,
    navigation: NavigationCoordinator,
    search: crate::application::search::SearchCoordinator,
    on_location_committed: Rc<RefCell<Option<Box<dyn Fn(Location)>>>>,
    on_navigate: Rc<RefCell<Option<Box<dyn Fn(Location)>>>>,
    on_close: Rc<RefCell<Option<Box<dyn Fn()>>>>,
    on_context_command: Rc<RefCell<Option<Box<dyn Fn(ContextTarget, ContextCommand)>>>>,
    on_options_changed: Rc<RefCell<Option<Box<dyn Fn(ViewOptions)>>>>,
    options: Rc<RefCell<ViewOptions>>,
    selected_target: Rc<RefCell<Option<ContextTarget>>>,
    on_command_availability_changed: Rc<RefCell<Option<Box<dyn Fn(CommandAvailability)>>>>,
    pending_cut: Rc<RefCell<Option<PathBuf>>>,
    is_quick_access_location: Rc<RefCell<Option<Rc<dyn Fn(&Location) -> bool>>>>,
}

impl FileView {
    pub fn new(location: Location) -> Self {
        Self::from_location(location)
    }

    pub fn from_location(initial: Location) -> Self {
        let repository = Rc::new(GioDirectoryRepository::new(
            glib::MainContext::ref_thread_default(),
        ));
        let view = Self::new_with_repository(initial, repository.clone());
        let widget = view.widget.downgrade();
        repository.set_mount_operation(move || {
            let parent = widget
                .upgrade()
                .and_then(|widget| widget.root())
                .and_downcast::<gtk::Window>();
            gtk::MountOperation::new(parent.as_ref()).upcast()
        });
        view
    }

    pub fn new_with_repository(
        current_location: Location,
        repository: Rc<dyn crate::core::ports::DirectoryRepository>,
    ) -> Self {
        Self::new_with_repositories(
            current_location,
            repository,
            Rc::new(
                crate::infrastructure::tracker_search::TrackerSearchRepository::new(
                    glib::MainContext::ref_thread_default(),
                ),
            ),
        )
    }

    pub fn new_with_repositories(
        current_location: Location,
        repository: Rc<dyn crate::core::ports::DirectoryRepository>,
        search_repository: Rc<dyn crate::core::ports::SearchRepository>,
    ) -> Self {
        let scroll = ScrolledWindow::new();
        scroll.set_vexpand(true);
        let on_navigate = Rc::new(RefCell::new(None));
        let state = Rc::new(RefCell::new(TabState::new(current_location)));
        let view = Self {
            widget: scroll,
            search: crate::application::search::SearchCoordinator::new(
                state.clone(),
                search_repository,
            ),
            navigation: NavigationCoordinator::new(state, repository),
            on_location_committed: Rc::new(RefCell::new(None)),
            on_navigate,
            on_close: Rc::new(RefCell::new(None)),
            on_context_command: Rc::new(RefCell::new(None)),
            on_options_changed: Rc::new(RefCell::new(None)),
            options: Rc::new(RefCell::new(ViewOptions::default())),
            selected_target: Rc::new(RefCell::new(None)),
            on_command_availability_changed: Rc::new(RefCell::new(None)),
            pending_cut: Rc::new(RefCell::new(None)),
            is_quick_access_location: Rc::new(RefCell::new(None)),
        };
        let background_view = view.clone();
        let background = GestureClick::new();
        background.set_button(3);
        let background_parent = view.widget.clone();
        background.connect_pressed(move |gesture, _, x, y| {
            if background_view.section_title().is_none() {
                show_item_context_menu(
                    &background_parent,
                    x,
                    y,
                    ContextTarget::Background(background_view.current_location()),
                    &background_view,
                    gesture
                        .current_event_state()
                        .contains(gtk::gdk::ModifierType::SHIFT_MASK),
                )
            }
        });
        view.widget.add_controller(background);
        view.refresh();
        view
    }

    pub fn tab_state(&self) -> Rc<RefCell<TabState>> {
        self.navigation.state.clone()
    }

    pub fn set_location(&self, path: PathBuf) {
        self.navigate(Location::from_local_path(&path));
    }

    pub fn set_uri(&self, uri: String) {
        if let Ok(location) = Location::from_uri(&uri) {
            self.navigate(location);
        }
    }

    pub fn navigate(&self, location: Location) {
        self.clear_search_results();
        let view = self.clone();
        self.navigation.navigate(location, move |accepted| {
            if accepted {
                view.navigation_finished();
            }
        });
        self.show_loading();
    }

    pub fn go_back(&self) {
        self.clear_search_results();
        let view = self.clone();
        if self.navigation.back(move |accepted| {
            if accepted {
                view.navigation_finished();
            }
        }) {
            self.show_loading();
        }
    }

    pub fn go_forward(&self) {
        self.clear_search_results();
        let view = self.clone();
        if self.navigation.forward(move |accepted| {
            if accepted {
                view.navigation_finished();
            }
        }) {
            self.show_loading();
        }
    }

    pub fn close(&self) {
        self.search.clear();
        self.navigation.close_tab();
    }

    pub fn set_close_handler(&self, callback: impl Fn() + 'static) {
        *self.on_close.borrow_mut() = Some(Box::new(callback));
    }

    pub fn mark_unavailable(&self, root: &Location) {
        if self.navigation.state.borrow_mut().mark_unavailable(root) {
            if self.current_location().is_within(root) {
                self.search.clear();
            }
            if !self.navigation.state.borrow().has_available_directory() {
                *self.selected_target.borrow_mut() = None;
            }
            self.notify_command_availability_changed();
            self.reload();
        }
    }

    pub fn set_location_committed_handler(&self, callback: impl Fn(Location) + 'static) {
        *self.on_location_committed.borrow_mut() = Some(Box::new(callback));
    }

    fn show_loading(&self) {
        if self.navigation.state.borrow().directory.is_none() && self.section_title().is_none() {
            self.widget
                .set_child(Some(&Label::new(Some("Loading folder…"))));
        }
    }

    fn navigation_finished(&self) {
        let state = self.navigation.state.borrow();
        let location = state.location.clone();
        let succeeded = matches!(state.status, NavigationStatus::Ready);
        drop(state);
        if succeeded {
            let state = self.navigation.state.borrow();
            *self.selected_target.borrow_mut() = state.directory.as_ref().and_then(|snapshot| {
                snapshot
                    .entries
                    .iter()
                    .find(|entry| {
                        state
                            .selection_keys
                            .iter()
                            .any(|key| key == entry.location.uri())
                    })
                    .map(|entry| file_target(&FileItem::from_snapshot(entry)))
            });
            drop(state);
            self.notify_command_availability_changed();
            if let Some(callback) = self.on_location_committed.borrow().as_ref() {
                callback(location);
            }
        }
        self.reload();
    }

    fn show_error(&self) {
        let status = self.navigation.state.borrow().status.clone();
        if let NavigationStatus::Failed { error, .. } = status {
            // Retain the successful listing and attach a non-modal retry message.
            let content = self.widget.child();
            self.widget.set_child(gtk::Widget::NONE);
            let box_ = GtkBox::new(Orientation::Vertical, 8);
            let banner = GtkBox::new(Orientation::Horizontal, 8);
            banner.append(&Label::new(Some(error.message_key)));
            let retry = Button::with_label("Retry");
            let view = self.clone();
            retry.connect_clicked(move |_| {
                let view_done = view.clone();
                view.navigation.retry(move |accepted| {
                    if accepted {
                        view_done.navigation_finished();
                    }
                });
                view.show_loading();
            });
            banner.append(&retry);
            box_.append(&banner);
            if let Some(content) = content {
                box_.append(&content);
            }
            self.widget.set_child(Some(&box_));
        }
    }

    pub fn set_navigation_handler<F: Fn(Location) + 'static>(&self, callback: F) {
        *self.on_navigate.borrow_mut() = Some(Box::new(callback));
    }

    pub fn set_context_handler<F: Fn(ContextTarget, ContextCommand) + 'static>(&self, callback: F) {
        *self.on_context_command.borrow_mut() = Some(Box::new(callback));
    }

    pub fn set_quick_access_predicate<F: Fn(&Location) -> bool + 'static>(&self, predicate: F) {
        *self.is_quick_access_location.borrow_mut() = Some(Rc::new(predicate));
    }

    fn is_quick_access_location(&self, path: &Location) -> bool {
        self.is_quick_access_location
            .borrow()
            .as_ref()
            .is_some_and(|predicate| predicate(path))
    }

    pub fn selected_target(&self) -> Option<ContextTarget> {
        self.selected_target.borrow().clone()
    }

    pub fn current_location(&self) -> Location {
        self.navigation.state.borrow().location.clone()
    }

    pub fn command_availability(&self) -> CommandAvailability {
        if !self.navigation.state.borrow().has_available_directory() {
            let mut availability = CommandAvailability::for_selection(&[], None);
            availability.disabled_reason = Some("This location is unavailable");
            return availability;
        }
        let selected_target = self.selected_target();
        command_availability_for_target(selected_target.as_ref(), &self.current_location())
    }

    pub fn set_command_availability_handler<F: Fn(CommandAvailability) + 'static>(
        &self,
        callback: F,
    ) {
        *self.on_command_availability_changed.borrow_mut() = Some(Box::new(callback));
        self.notify_command_availability_changed();
    }

    fn notify_command_availability_changed(&self) {
        self.navigation.state.borrow_mut().selection_keys =
            match self.selected_target.borrow().as_ref() {
                Some(ContextTarget::File(item) | ContextTarget::Folder(item)) => {
                    vec![item.identity_key().to_owned()]
                }
                _ => vec![],
            };
        if let Some(callback) = self.on_command_availability_changed.borrow().as_ref() {
            callback(self.command_availability());
        }
    }

    pub fn dispatch_context_command(&self, target: ContextTarget, command: ContextCommand) {
        if !self.navigation.state.borrow().has_available_directory() {
            return;
        }
        if !CommandAvailability::for_target(&target).allows(command) {
            return;
        }
        if let Some(callback) = self.on_context_command.borrow().as_ref() {
            callback(target, command);
        }
    }

    pub fn set_pending_cut(&self, path: Option<PathBuf>) {
        *self.pending_cut.borrow_mut() = path;
        self.reload();
    }

    pub fn set_options_changed_handler<F: Fn(ViewOptions) + 'static>(&self, callback: F) {
        *self.on_options_changed.borrow_mut() = Some(Box::new(callback));
    }

    pub fn set_options(&self, options: ViewOptions) {
        *self.options.borrow_mut() = options;
        self.reload();
    }

    pub fn options(&self) -> ViewOptions {
        self.options.borrow().clone()
    }

    pub fn uri(&self) -> Option<String> {
        let location = self.current_location();
        location
            .local_path()
            .is_none()
            .then(|| location.uri().to_owned())
    }

    /// Replaces the current listing with search results, which may include
    /// items outside the folder currently open in this view.
    pub fn show_search_results(&self, items: Vec<FileItem>) {
        let mut state = self.navigation.state.borrow_mut();
        state.cancel_pending();
        state.content = TabContent::Search {
            underlying: state.location.clone(),
            items,
        };
        drop(state);
        self.reload();
    }

    pub fn clear_search_results(&self) {
        let was_active = self.search.clear();
        if !was_active && !self.is_showing_search_results() {
            return;
        }
        if self.is_showing_search_results() {
            self.navigation.state.borrow_mut().restore_directory();
        }
        self.navigation_finished();
    }

    pub fn search(
        &self,
        term: &str,
        mode: crate::core::contracts::SearchMode,
        debounce: bool,
    ) -> bool {
        if term.trim().is_empty() {
            self.clear_search_results();
            return true;
        }
        let scope = crate::core::contracts::SearchScope::local_fallback(self.current_location());
        let view = self.clone();
        let done = move |_| view.search_finished();
        if debounce {
            self.search.submit_debounced(term, mode, scope, done)
        } else {
            self.search.submit(term, mode, scope, done)
        }
    }

    fn search_finished(&self) {
        *self.selected_target.borrow_mut() = None;
        self.notify_command_availability_changed();
        self.reload();
    }

    fn show_search_status(&self) {
        use crate::core::contracts::SearchOutcome;
        use adw::prelude::{MessageDialogExt, MessageDialogExtManual};
        if !self.is_showing_search_results() {
            return;
        }
        let Some(outcome) = self.search.outcome() else {
            return;
        };
        let content = self.widget.child();
        self.widget.set_child(gtk::Widget::NONE);
        let layout = GtkBox::new(Orientation::Vertical, 8);
        let status = Label::new(Some(&outcome.status_text()));
        status.set_wrap(true);
        status.set_xalign(0.0);
        layout.append(&status);
        if matches!(outcome, SearchOutcome::Incomplete { .. }) {
            let retry = Button::with_label("Retry search");
            let view = self.clone();
            retry.connect_clicked(move |_| {
                let updated = view.clone();
                view.search.retry(move |_| updated.search_finished());
            });
            layout.append(&retry);
        }
        if let SearchOutcome::Completed { declared_scope, .. } = outcome {
            let expand = Button::with_label("Expand search scope…");
            let view = self.clone();
            expand.connect_clicked(move |_| {
                let Some(parent) = view.widget.root().and_downcast::<gtk::Window>() else {
                    return;
                };
                let scope = crate::core::contracts::SearchScope {
                    max_depth: declared_scope.max_depth.saturating_add(1),
                    max_results: declared_scope.max_results.saturating_mul(2),
                    timeout: declared_scope.timeout.saturating_mul(2),
                    ..declared_scope.clone()
                };
                let dialog = adw::MessageDialog::new(
                    Some(&parent),
                    Some("Expand search scope"),
                    Some(&scope.annotation()),
                );
                dialog.add_responses(&[("cancel", "Cancel"), ("expand", "Search expanded scope")]);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                let view = view.clone();
                dialog.connect_response(None, move |dialog, response| {
                    if response == "expand" {
                        let updated = view.clone();
                        view.search
                            .expand_scope(scope.clone(), move |_| updated.search_finished());
                    }
                    dialog.close();
                });
                dialog.present();
            });
            layout.append(&expand);
        }
        if let Some(content) = content {
            layout.append(&content);
        }
        self.widget.set_child(Some(&layout));
    }

    pub fn refresh(&self) {
        self.clear_search_results();
        if !self.has_pending_navigation() && self.section_title().is_some() {
            self.reload();
            return;
        }
        let view = self.clone();
        self.navigation.refresh(move |accepted| {
            if accepted {
                view.navigation_finished();
            }
        });
        self.show_loading();
    }

    pub fn has_pending_navigation(&self) -> bool {
        self.navigation.state.borrow().has_pending_navigation()
    }

    pub fn is_showing_search_results(&self) -> bool {
        matches!(
            self.navigation.state.borrow().content,
            TabContent::Search { .. }
        )
    }

    pub fn show_section_overview(&self, section: SidebarSection) {
        self.search.clear();
        *self.selected_target.borrow_mut() = None;
        self.navigation.state.borrow_mut().show_section(section);
        self.notify_command_availability_changed();
        self.reload();
    }

    pub fn clear_section_overview(&self) -> bool {
        if self.section_title().is_none() {
            return false;
        }
        self.navigation.state.borrow_mut().restore_directory();
        if self.navigation.state.borrow().directory.is_some() {
            self.reload();
        } else {
            self.refresh();
        }
        true
    }

    pub fn section_title(&self) -> Option<String> {
        match &self.navigation.state.borrow().content {
            TabContent::SectionOverview { section, .. } => Some(section.title.clone()),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn section_overview_node_count(&self) -> usize {
        match &self.navigation.state.borrow().content {
            TabContent::SectionOverview { section, .. } => section.nodes.len(),
            _ => 0,
        }
    }

    /// Repaints owned metadata; never enumerates or queries the filesystem.
    fn reload(&self) {
        let state = self.navigation.state.borrow().clone();
        let options = self.options();
        let mut items = match state.content {
            TabContent::Unavailable(location)
                if !matches!(state.status, NavigationStatus::Loading(_)) =>
            {
                let content = GtkBox::new(Orientation::Vertical, 12);
                content.append(&Label::new(Some("This location is unavailable")));
                content.append(&Label::new(Some(&location.display_name())));
                let reconnect = Button::with_label("Reconnect");
                let view = self.clone();
                reconnect.connect_clicked(move |_| view.refresh());
                content.append(&reconnect);
                let close = Button::with_label("Close");
                let handler = self.on_close.clone();
                close.connect_clicked(move |_| {
                    if let Some(callback) = handler.borrow().as_ref() {
                        callback();
                    }
                });
                content.append(&close);
                self.widget.set_child(Some(&content));
                return;
            }
            TabContent::SectionOverview { section, .. } => {
                self.render_section_overview(&section);
                self.show_error();
                return;
            }
            TabContent::Search { items, .. } => items,
            _ => match state.directory {
                Some(snapshot) => snapshot
                    .entries
                    .iter()
                    .filter(|entry| options.show_hidden || !entry.display_name.starts_with('.'))
                    .map(FileItem::from_snapshot)
                    .collect(),
                None => {
                    self.widget.set_child(Some(&Label::new(Some(
                        if matches!(state.status, NavigationStatus::Loading(_)) {
                            "Loading folder…"
                        } else {
                            "No folder loaded"
                        },
                    ))));
                    self.show_error();
                    return;
                }
            },
        };
        sort_items(&mut items, &options);
        self.render(items, &options);
        self.show_search_status();
        self.show_error();
    }

    fn render_section_overview(&self, section: &SidebarSection) {
        let page = GtkBox::new(Orientation::Vertical, 18);
        page.add_css_class("section-overview");
        page.set_margin_top(24);
        page.set_margin_bottom(24);
        page.set_margin_start(28);
        page.set_margin_end(28);

        let title = Label::new(Some(&section.title));
        title.add_css_class("title-1");
        title.set_xalign(0.0);
        page.append(&title);

        if section.title == "This PC" {
            let description = Label::new(Some("Storage and locations on this computer"));
            description.add_css_class("dim-label");
            description.set_xalign(0.0);
            page.append(&description);
        }

        let entries_heading = Label::new(Some(if section.title == "This PC" {
            "Drives"
        } else {
            "Locations"
        }));
        entries_heading.add_css_class("heading");
        entries_heading.set_xalign(0.0);
        page.append(&entries_heading);

        let entries = GtkBox::new(Orientation::Vertical, 6);
        entries.add_css_class("section-overview-entries");
        if section.nodes.is_empty() {
            let empty = Label::new(Some("No locations are available here yet."));
            empty.add_css_class("dim-label");
            empty.set_xalign(0.0);
            entries.append(&empty);
        } else {
            for node in &section.nodes {
                entries.append(&self.section_entry(node, section.title == "This PC"));
            }
        }
        page.append(&entries);

        self.widget.set_child(Some(&page));
    }

    fn section_entry(&self, node: &SidebarNode, show_storage: bool) -> Button {
        let button = Button::new();
        button.add_css_class("flat");
        button.add_css_class("section-overview-entry");
        button.set_halign(gtk::Align::Fill);
        let content = GtkBox::new(Orientation::Horizontal, 12);
        content.set_margin_top(10);
        content.set_margin_bottom(10);
        content.set_margin_start(12);
        content.set_margin_end(12);
        let icon = Image::from_icon_name(node.location.icon_name());
        icon.set_pixel_size(24);
        content.append(&icon);

        let text = GtkBox::new(Orientation::Vertical, 3);
        text.set_hexpand(true);
        let label = Label::new(Some(node.location.label()));
        label.set_xalign(0.0);
        text.append(&label);
        if show_storage {
            if let Some(path) = node.location.local_path() {
                if let Some((total, free)) = filesystem_space(path) {
                    let detail = Label::new(Some(&format!(
                        "{} free of {}",
                        crate::core::file_item::format_size(free),
                        crate::core::file_item::format_size(total)
                    )));
                    detail.add_css_class("dim-label");
                    detail.set_xalign(0.0);
                    text.append(&detail);
                    let progress = gtk::ProgressBar::new();
                    progress
                        .set_fraction((1.0 - free as f64 / total.max(1) as f64).clamp(0.0, 1.0));
                    progress.set_show_text(false);
                    text.append(&progress);
                }
            }
        }
        content.append(&text);
        button.set_child(Some(&content));
        let view = self.clone();
        let location = node.location.clone();
        button.connect_clicked(move |_| view.open_sidebar_location(&location));
        button
    }

    fn open_sidebar_location(&self, location: &SidebarLocation) {
        if let Ok(location) = Location::from_uri(&location.uri()) {
            if let Some(callback) = self.on_navigate.borrow().as_ref() {
                callback(location);
            } else {
                self.navigate(location);
            }
        }
    }

    fn render(&self, items: Vec<FileItem>, options: &ViewOptions) {
        if items.is_empty() {
            let message = if self.is_showing_search_results() {
                match self.search.outcome() {
                    Some(crate::core::contracts::SearchOutcome::Incomplete { .. }) => {
                        "No results were accepted before search stopped"
                    }
                    Some(crate::core::contracts::SearchOutcome::Unsupported { .. }) => {
                        "Search is unavailable for this location or mode"
                    }
                    _ => "No files matched in the declared search scope",
                }
            } else {
                "This folder is empty"
            };
            self.widget.set_child(Some(&Label::new(Some(message))));
            return;
        }
        match options.view_mode {
            ViewMode::DetailedList | ViewMode::CompactList => self
                .widget
                .set_child(Some(&self.column_view_for(&items, options))),
            ViewMode::IconGrid => self
                .widget
                .set_child(Some(&self.grid_view(&items, options))),
            ViewMode::Hybrid => self
                .widget
                .set_child(Some(&self.hybrid_view(&items, options))),
        };
    }

    fn column_view_for(&self, items: &[FileItem], options: &ViewOptions) -> ColumnView {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let mut last_group = None;
        for item in items {
            let group = crate::core::directory_model::group_label(
                item,
                options.group_field,
                std::time::SystemTime::now(),
            );
            if group != last_group {
                if let Some(group) = &group {
                    store.append(&glib::BoxedAnyObject::new(ListRow::Group(group.clone())));
                }
                last_group = group;
            }
            store.append(&glib::BoxedAnyObject::new(ListRow::File(item.clone())));
        }

        // Group headings are part of this model. Applying a second GTK sort would
        // move the file rows independently and detach them from their headings.
        let native_row_sorting = uses_native_row_sorting(options.group_field);
        let sorted = native_row_sorting
            .then(|| SortListModel::new(Some(store.clone()), None::<gtk::Sorter>));
        let selection = match &sorted {
            Some(sorted) => SingleSelection::new(Some(sorted.clone())),
            None => SingleSelection::new(Some(store)),
        };
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let selected_key = self
            .navigation
            .state
            .borrow()
            .selection_keys
            .first()
            .cloned();
        let selected_position = (0..selection.n_items()).find(|position| {
            selection
                .item(*position)
                .and_downcast::<glib::BoxedAnyObject>()
                .and_then(|object| file_target_from_row(&object.borrow::<ListRow>()))
                .is_some_and(|target| match target {
                    ContextTarget::File(item) | ContextTarget::Folder(item) => {
                        selected_key.as_deref() == Some(item.identity_key())
                    }
                    _ => false,
                })
        });
        selection.set_selected(selected_position.unwrap_or(gtk::INVALID_LIST_POSITION));
        let selected_target = self.selected_target.clone();
        let availability_view = self.clone();
        selection.connect_selection_changed(move |model, _, _| {
            *selected_target.borrow_mut() = model
                .selected_item()
                .and_downcast::<glib::BoxedAnyObject>()
                .and_then(|object| file_target_from_row(&object.borrow::<ListRow>()));
            availability_view.notify_command_availability_changed();
        });
        let view = ColumnView::new(Some(selection.clone()));
        view.set_reorderable(true);
        view.set_show_row_separators(true);
        view.add_css_class(if options.view_mode == ViewMode::CompactList {
            "compact-file-list"
        } else {
            "detailed-file-list"
        });

        for column_id in options.list_columns.normalized().order {
            let column = ColumnViewColumn::new(
                Some(column_title(column_id)),
                Some(column_factory(column_id, options.thumbnails, self.clone())),
            );
            column.set_resizable(true);
            column.set_fixed_width(options.list_columns.width_for(column_id));
            column.set_expand(column_id == ListColumn::Name);
            column.set_sorter(Some(&column_sorter(column_id)));
            let options_state = self.options.clone();
            let changed = self.on_options_changed.clone();
            let layout_view = view.clone();
            column.connect_fixed_width_notify(move |_| {
                publish_column_layout(&layout_view, &options_state, &changed)
            });
            view.append_column(&column);
        }
        if let Some(sorted) = &sorted {
            sorted.set_sorter(view.sorter().as_ref());
        }

        let options_state = self.options.clone();
        let changed = self.on_options_changed.clone();
        let layout_view = view.clone();
        view.columns().connect_items_changed(move |_, _, _, _| {
            publish_column_layout(&layout_view, &options_state, &changed)
        });

        let options_state = self.options.clone();
        let changed = self.on_options_changed.clone();
        let file_view = self.clone();
        let column_sorter = view
            .sorter()
            .and_downcast::<gtk::ColumnViewSorter>()
            .expect("column view uses a column sorter");
        column_sorter.connect_primary_sort_column_notify(move |sorter| {
            let Some(column) = sorter.primary_sort_column() else {
                return;
            };
            let Some(field) = column.title().as_deref().and_then(sort_field_for_title) else {
                return;
            };
            let mut options = options_state.borrow().clone();
            options.sort_field = field;
            options.sort_direction = match sorter.primary_sort_order() {
                gtk::SortType::Descending => SortDirection::Descending,
                _ => SortDirection::Ascending,
            };
            *options_state.borrow_mut() = options.clone();
            if let Some(callback) = changed.borrow().as_ref() {
                callback(options);
            }
            if !uses_native_row_sorting(options_state.borrow().group_field) {
                let file_view = file_view.clone();
                glib::idle_add_local_once(move || file_view.reload());
            }
        });

        let file_view = self.clone();
        view.connect_activate(move |_, position| {
            let Some(object) = selection
                .item(position)
                .and_downcast::<glib::BoxedAnyObject>()
            else {
                return;
            };
            let row = object.borrow::<ListRow>();
            if let ListRow::File(item) = &*row {
                activate_item(&file_view, item.clone());
            }
        });
        view
    }

    pub fn column_view(&self) -> Option<ColumnView> {
        self.widget.child()?.downcast::<ColumnView>().ok()
    }

    fn grid_view(&self, items: &[FileItem], options: &ViewOptions) -> FlowBox {
        let selected_key = self
            .navigation
            .state
            .borrow()
            .selection_keys
            .first()
            .cloned();
        let layout = options.grid_size.grid_layout();
        let grid = FlowBox::new();
        grid.set_selection_mode(gtk::SelectionMode::Single);
        grid.set_activate_on_single_click(false);
        grid.set_valign(gtk::Align::Start);
        grid.set_column_spacing(layout.gap.try_into().expect("grid gaps are positive"));
        grid.set_row_spacing(layout.gap.try_into().expect("grid gaps are positive"));
        grid.set_margin_top(12);
        grid.set_margin_start(12);
        grid.set_margin_end(12);
        let activation_items = items.to_vec();
        let activation_view = self.clone();
        grid.connect_child_activated(move |_, child| {
            if let Some(item) = activation_items.get(child.index() as usize) {
                activate_item(&activation_view, item.clone());
            }
        });
        let grid_selection_items = items.to_vec();
        let selected_target = self.selected_target.clone();
        let availability_view = self.clone();
        grid.connect_selected_children_changed(move |grid| {
            *selected_target.borrow_mut() = grid
                .selected_children()
                .first()
                .and_then(|child| grid_selection_items.get(child.index() as usize))
                .map(file_target);
            availability_view.notify_command_availability_changed();
        });
        for item in items {
            let tile = GtkBox::new(Orientation::Vertical, layout.gap / 2);
            tile.add_css_class("icon-grid-tile");
            if self
                .pending_cut
                .borrow()
                .as_ref()
                .is_some_and(|path| item.local_path().as_deref() == Some(path))
            {
                tile.add_css_class("cut-pending");
            }
            tile.set_size_request(layout.tile_width, -1);
            tile.set_halign(gtk::Align::Center);
            tile.set_valign(gtk::Align::Start);
            tile.set_vexpand(false);
            tile.set_tooltip_text(Some(item.name()));
            tile.set_margin_top(layout.vertical_padding);
            tile.set_margin_bottom(layout.vertical_padding);
            let icon = item_image(item, options.thumbnails);
            icon.set_pixel_size(options.grid_size.icon_size());
            tile.append(&icon);
            tile.append(&grid_tile_label(item.name(), layout.label_width_chars));
            attach_drag_source(&tile, item.clone());
            let target = file_target(item);
            let view = self.clone();
            let grid_context = grid.clone();
            let menu_tile = tile.clone();
            let selected_target = self.selected_target.clone();
            let gesture = GestureClick::new();
            gesture.set_button(3);
            gesture.connect_pressed(move |gesture, _, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                *selected_target.borrow_mut() = Some(target.clone());
                view.notify_command_availability_changed();
                if let Some(child) = menu_tile.parent().and_downcast::<gtk::FlowBoxChild>() {
                    grid_context.select_child(&child);
                }
                show_item_context_menu(
                    &menu_tile,
                    x,
                    y,
                    target.clone(),
                    &view,
                    gesture
                        .current_event_state()
                        .contains(gtk::gdk::ModifierType::SHIFT_MASK),
                )
            });
            tile.add_controller(gesture);
            grid.append(&tile);
            if selected_key.as_deref() == Some(item.identity_key()) {
                if let Some(child) = tile.parent().and_downcast::<gtk::FlowBoxChild>() {
                    grid.select_child(&child);
                }
            }
        }
        grid
    }

    fn hybrid_view(&self, items: &[FileItem], options: &ViewOptions) -> GtkBox {
        let root = GtkBox::new(Orientation::Vertical, 8);
        root.set_margin_start(12);
        root.set_margin_top(12);
        let folders: Vec<_> = items.iter().filter(|item| item.is_dir()).cloned().collect();
        let files: Vec<_> = items
            .iter()
            .filter(|item| !item.is_dir())
            .cloned()
            .collect();
        if !folders.is_empty() {
            let heading = Label::new(Some("Folders"));
            heading.add_css_class("heading");
            heading.set_xalign(0.0);
            root.append(&heading);
            root.append(&self.grid_view(&folders, options));
        }
        if !files.is_empty() {
            let heading = Label::new(Some("Files"));
            heading.add_css_class("heading");
            heading.set_xalign(0.0);
            heading.set_margin_top(12);
            root.append(&heading);
            let file_options = ViewOptions {
                view_mode: ViewMode::DetailedList,
                folders_first: false,
                ..options.clone()
            };
            root.append(&self.column_view_for(&files, &file_options));
        }
        root
    }
}

/// Group headings and the entries underneath them must be treated as one ordered
/// section.  DirectoryModel has already sorted those sections, so only ungrouped
/// lists may be reordered by GTK's live column sorter.
fn uses_native_row_sorting(group_field: GroupField) -> bool {
    group_field == GroupField::None
}

fn grid_tile_label(name: &str, width_chars: i32) -> Label {
    let label = Label::new(Some(name));
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_lines(2);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_width_chars(width_chars);
    label.set_max_width_chars(width_chars);
    label.set_justify(gtk::Justification::Center);
    label.set_xalign(0.5);
    label
}

fn show_item_context_menu(
    parent: &impl IsA<gtk::Widget>,
    x: f64,
    y: f64,
    target: ContextTarget,
    view: &FileView,
    shift_held: bool,
) {
    let popover = Popover::new();
    popover.set_parent(parent);
    popover.set_autohide(true);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    let menu = GtkBox::new(Orientation::Vertical, 2);
    menu.set_margin_top(6);
    menu.set_margin_bottom(6);
    let availability = CommandAvailability::for_target(&target);
    for command in item_commands(&target) {
        if command == ContextCommand::OpenWith {
            menu.append(&open_with_submenu(&target, &availability));
            continue;
        }
        if command == ContextCommand::Copy {
            menu.append(&command_submenu(
                "Copy ›",
                &target,
                view,
                &[
                    ContextCommand::Copy,
                    ContextCommand::CopyFilename,
                    ContextCommand::CopyFullPath,
                    ContextCommand::CopyTo,
                ],
                popover.clone(),
                &availability,
            ));
            continue;
        }
        if command == ContextCommand::Cut {
            menu.append(&command_submenu(
                "Move ›",
                &target,
                view,
                &[ContextCommand::Cut, ContextCommand::MoveTo],
                popover.clone(),
                &availability,
            ));
            continue;
        }
        if matches!(
            command,
            ContextCommand::CopyFilename
                | ContextCommand::CopyFullPath
                | ContextCommand::CopyTo
                | ContextCommand::MoveTo
        ) {
            continue;
        }
        if command == ContextCommand::MoveToTrash {
            menu.append(&dynamic_delete_button(
                target.clone(),
                view.clone(),
                popover.clone(),
                shift_held,
                &availability,
            ));
        } else {
            menu.append(&command_button(
                context_command_label_for(command, &target, view),
                target.clone(),
                view.clone(),
                command,
                Some(popover.clone()),
                &availability,
            ));
        }
    }
    popover.set_child(Some(&menu));
    popover.popup();
}

fn dynamic_delete_button(
    target: ContextTarget,
    view: FileView,
    popover: Popover,
    shift_held: bool,
    availability: &CommandAvailability,
) -> Button {
    let shifted = Rc::new(RefCell::new(shift_held));
    let button = Button::with_label(context_command_label(delete_command(shift_held)));
    set_command_enabled(&button, availability, delete_command(shift_held));
    let update_label = {
        let button = button.clone();
        let shifted = shifted.clone();
        let availability = availability.clone();
        move |is_shifted| {
            *shifted.borrow_mut() = is_shifted;
            let command = delete_command(is_shifted);
            button.set_label(context_command_label(command));
            set_command_enabled(&button, &availability, command);
        }
    };
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gtk::gdk::Key::Shift_L
            || key == gtk::gdk::Key::Shift_R
            || modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK)
        {
            update_label(true);
        }
        glib::Propagation::Proceed
    });
    let button_on_release = button.clone();
    let shifted_on_release = shifted.clone();
    let availability_on_release = availability.clone();
    keys.connect_key_released(move |_, key, _, modifiers| {
        if key == gtk::gdk::Key::Shift_L
            || key == gtk::gdk::Key::Shift_R
            || !modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK)
        {
            *shifted_on_release.borrow_mut() = false;
            button_on_release.set_label(context_command_label(delete_command(false)));
            set_command_enabled(
                &button_on_release,
                &availability_on_release,
                delete_command(false),
            );
        }
    });
    popover.add_controller(keys);
    button.connect_clicked(move |_| {
        popover.popdown();
        let command = delete_command(*shifted.borrow());
        view.dispatch_context_command(target.clone(), command);
    });
    button
}

fn command_submenu(
    label: &str,
    target: &ContextTarget,
    view: &FileView,
    commands: &[ContextCommand],
    root: Popover,
    availability: &CommandAvailability,
) -> Button {
    let button = Button::with_label(label);
    let enabled = commands
        .iter()
        .copied()
        .any(|command| availability.allows(command));
    button.set_sensitive(enabled);
    if !enabled {
        if let Some(reason) = commands
            .iter()
            .copied()
            .find_map(|command| availability.disabled_reason_for(command))
        {
            button.set_tooltip_text(Some(reason));
            button.update_property(&[gtk::accessible::Property::Description(reason)]);
        }
    }
    let submenu_parent = button.clone();
    let target = target.clone();
    let view = view.clone();
    let commands = commands.to_vec();
    let availability = availability.clone();
    button.connect_clicked(move |_| {
        let menu = GtkBox::new(Orientation::Vertical, 2);
        menu.set_margin_top(6);
        menu.set_margin_bottom(6);
        for command in &commands {
            menu.append(&command_button(
                context_command_label(*command),
                target.clone(),
                view.clone(),
                *command,
                Some(root.clone()),
                &availability,
            ));
        }
        show_submenu(&submenu_parent, &menu);
    });
    button
}

fn open_with_submenu(target: &ContextTarget, availability: &CommandAvailability) -> Button {
    let button = Button::with_label("Open With ›");
    set_command_enabled(&button, availability, ContextCommand::OpenWith);
    let submenu_parent = button.clone();
    let target = target.clone();
    button.connect_clicked(move |_| {
        let (uri, content_type) = match &target {
            ContextTarget::File(item) => (
                item.location().uri().to_owned(),
                item.content_type().to_owned(),
            ),
            _ => return,
        };
        let menu = GtkBox::new(Orientation::Vertical, 2);
        menu.set_margin_top(6);
        menu.set_margin_bottom(6);
        for app in open_with_apps(&content_type) {
            let app_button = Button::with_label(&app.display_name());
            let uri = uri.clone();
            app_button.connect_clicked(move |_| {
                let _ = app.launch(&[gio::File::for_uri(&uri)], None::<&gio::AppLaunchContext>);
            });
            menu.append(&app_button);
        }
        menu.append(&Separator::new(Orientation::Horizontal));
        let other = Button::with_label("Other Application…");
        let parent = submenu_parent.clone();
        other.connect_clicked(move |_| {
            show_application_chooser(&parent, uri.clone(), content_type.clone())
        });
        menu.append(&other);
        show_submenu(&submenu_parent, &menu);
    });
    button
}

fn show_submenu(parent: &Button, menu: &GtkBox) {
    let submenu = Popover::new();
    submenu.set_parent(parent);
    submenu.set_position(gtk::PositionType::Right);
    submenu.set_autohide(true);
    submenu.set_child(Some(menu));
    submenu.popup();
}

#[allow(deprecated)]
fn show_application_chooser(parent: &Button, uri: String, content_type: String) {
    let window = parent.root().and_downcast::<gtk::Window>();
    let dialog = gtk::AppChooserDialog::for_content_type(
        window.as_ref(),
        gtk::DialogFlags::MODAL,
        &content_type,
    );
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Ok {
            if let Some(app) = dialog.app_info() {
                let _ = app.launch(&[gio::File::for_uri(&uri)], None::<&gio::AppLaunchContext>);
            }
        }
        dialog.close();
    });
    dialog.present();
}

fn command_button(
    label: &str,
    target: ContextTarget,
    view: FileView,
    command: ContextCommand,
    parent: Option<Popover>,
    availability: &CommandAvailability,
) -> Button {
    let button = Button::with_label(label);
    set_command_enabled(&button, availability, command);
    button.connect_clicked(move |_| {
        if let Some(popover) = &parent {
            popover.popdown();
        }
        if command == ContextCommand::Open {
            if let ContextTarget::Folder(item) | ContextTarget::File(item) = &target {
                activate_item(&view, item.clone());
                return;
            }
        }
        view.dispatch_context_command(target.clone(), command);
    });
    button
}

fn set_command_enabled(
    button: &Button,
    availability: &CommandAvailability,
    command: ContextCommand,
) {
    configure_command_button(button, availability, command);
}

fn context_command_label(command: ContextCommand) -> &'static str {
    match command {
        ContextCommand::Open => "Open",
        ContextCommand::OpenInNewTab => "Open in New Tab",
        ContextCommand::OpenInNewWindow => "Open in New Window",
        ContextCommand::OpenInTerminal => "Open in Terminal",
        ContextCommand::Cut => "Cut",
        ContextCommand::Copy => "Copy",
        ContextCommand::CopyFilename => "Copy Filename",
        ContextCommand::CopyFullPath => "Copy Full Path",
        ContextCommand::CopyTo => "Copy To…",
        ContextCommand::MoveTo => "Move To…",
        ContextCommand::Rename => "Rename",
        ContextCommand::MoveToTrash => "Move to Trash",
        ContextCommand::DeletePermanently => "Delete Permanently…",
        ContextCommand::Properties => "Properties",
        ContextCommand::NewFolder => "New Folder",
        ContextCommand::Paste => "Paste",
        ContextCommand::Refresh => "Refresh",
        ContextCommand::CloseTab => "Close Tab",
        ContextCommand::CloseOtherTabs => "Close Other Tabs",
        ContextCommand::DuplicateTab => "Duplicate Tab",
        ContextCommand::ToggleQuickAccess => "Pin to Quick access",
        ContextCommand::OpenWith => "Open With",
    }
}

fn context_command_label_for(
    command: ContextCommand,
    target: &ContextTarget,
    view: &FileView,
) -> &'static str {
    if command == ContextCommand::ToggleQuickAccess
        && target_location(target).is_some_and(|location| view.is_quick_access_location(&location))
    {
        "Unpin from Quick access"
    } else {
        context_command_label(command)
    }
}

fn column_title(column: ListColumn) -> &'static str {
    match column {
        ListColumn::Name => "Name",
        ListColumn::DateModified => "Date modified",
        ListColumn::Type => "Type",
        ListColumn::Size => "Size",
    }
}

fn sort_field_for_title(title: &str) -> Option<SortField> {
    match title {
        "Name" => Some(SortField::Name),
        "Date modified" => Some(SortField::DateModified),
        "Type" => Some(SortField::Type),
        "Size" => Some(SortField::Size),
        _ => None,
    }
}

fn list_column_for_title(title: &str) -> Option<ListColumn> {
    match title {
        "Name" => Some(ListColumn::Name),
        "Date modified" => Some(ListColumn::DateModified),
        "Type" => Some(ListColumn::Type),
        "Size" => Some(ListColumn::Size),
        _ => None,
    }
}

fn publish_column_layout(
    view: &ColumnView,
    options_state: &Rc<RefCell<ViewOptions>>,
    changed: &Rc<RefCell<Option<Box<dyn Fn(ViewOptions)>>>>,
) {
    let columns = view.columns();
    let mut order = Vec::new();
    let mut widths = std::collections::BTreeMap::new();
    for index in 0..columns.n_items() {
        let Some(column) = columns.item(index).and_downcast::<ColumnViewColumn>() else {
            continue;
        };
        let Some(id) = column.title().as_deref().and_then(list_column_for_title) else {
            continue;
        };
        order.push(id);
        widths.insert(id, column.fixed_width());
    }
    let layout = ListColumnLayout { order, widths }.normalized();
    if options_state.borrow().list_columns == layout {
        return;
    }
    let mut options = options_state.borrow().clone();
    options.list_columns = layout;
    *options_state.borrow_mut() = options.clone();
    if let Some(callback) = changed.borrow().as_ref() {
        callback(options);
    }
}

fn column_factory(column: ListColumn, thumbnails: bool, view: FileView) -> SignalListItemFactory {
    let factory = SignalListItemFactory::new();
    factory.connect_setup(move |_, list_item| {
        let list_item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("column factories receive list items");
        if column == ListColumn::Name {
            let line = GtkBox::new(Orientation::Horizontal, 8);
            line.set_margin_start(10);
            line.set_margin_end(10);
            line.set_margin_top(6);
            line.set_margin_bottom(6);
            let icon = Image::new();
            icon.set_pixel_size(20);
            let label = Label::new(None);
            label.set_xalign(0.0);
            line.append(&icon);
            line.append(&label);
            let context_item = list_item.clone();
            let context_view = view.clone();
            let gesture = GestureClick::new();
            gesture.set_button(3);
            gesture.connect_pressed(move |gesture, _, x, y| {
                let Some(object) = context_item.item().and_downcast::<glib::BoxedAnyObject>()
                else {
                    return;
                };
                let row = object.borrow::<ListRow>();
                let ListRow::File(item) = &*row else {
                    return;
                };
                let target = file_target(item);
                let Some(parent) = context_item.child() else {
                    return;
                };
                gesture.set_state(gtk::EventSequenceState::Claimed);
                *context_view.selected_target.borrow_mut() = Some(target.clone());
                context_view.notify_command_availability_changed();
                show_item_context_menu(
                    &parent,
                    x,
                    y,
                    target,
                    &context_view,
                    gesture
                        .current_event_state()
                        .contains(gtk::gdk::ModifierType::SHIFT_MASK),
                );
            });
            line.add_controller(gesture);
            let drag_item = list_item.clone();
            let drag_source = gtk::DragSource::new();
            drag_source.set_actions(gtk::gdk::DragAction::COPY);
            drag_source.connect_prepare(move |_, _, _| {
                let object = drag_item.item().and_downcast::<glib::BoxedAnyObject>()?;
                let row = object.borrow::<ListRow>();
                let ListRow::File(item) = &*row else {
                    return None;
                };
                drag_content_provider(item)
            });
            line.add_controller(drag_source);
            list_item.set_child(Some(&line));
        } else {
            let label = Label::new(None);
            label.set_xalign(1.0);
            label.add_css_class("dim-label");
            label.set_margin_start(8);
            label.set_margin_end(10);
            list_item.set_child(Some(&label));
        }
    });
    factory.connect_bind(move |_, list_item| {
        let list_item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("column factories receive list items");
        let Some(object) = list_item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let row = object.borrow::<ListRow>();
        let Some(child) = list_item.child() else {
            return;
        };
        match (&*row, column) {
            (ListRow::Group(group), ListColumn::Name) => {
                let line = child.downcast::<GtkBox>().expect("name cells use a box");
                let icon = line
                    .first_child()
                    .and_downcast::<Image>()
                    .expect("name cells include an icon");
                icon.clear();
                let label = line
                    .last_child()
                    .and_downcast::<Label>()
                    .expect("name cells include a label");
                label.set_label(group);
                label.add_css_class("heading");
            }
            (ListRow::Group(_), _) => child
                .downcast::<Label>()
                .expect("metadata cells use labels")
                .set_label(""),
            (ListRow::File(item), ListColumn::Name) => {
                let line = child.downcast::<GtkBox>().expect("name cells use a box");
                let icon = line
                    .first_child()
                    .and_downcast::<Image>()
                    .expect("name cells include an icon");
                set_item_image(&icon, item, thumbnails);
                let label = line
                    .last_child()
                    .and_downcast::<Label>()
                    .expect("name cells include a label");
                label.remove_css_class("heading");
                label.set_label(item.name());
            }
            (ListRow::File(item), ListColumn::DateModified) => child
                .downcast::<Label>()
                .expect("metadata cells use labels")
                .set_label(&format_modified(item.modified())),
            (ListRow::File(item), ListColumn::Type) => child
                .downcast::<Label>()
                .expect("metadata cells use labels")
                .set_label(&item.type_name()),
            (ListRow::File(item), ListColumn::Size) => child
                .downcast::<Label>()
                .expect("metadata cells use labels")
                .set_label(&item.display_size()),
        }
    });
    factory
}

fn column_sorter(column: ListColumn) -> CustomSorter {
    CustomSorter::new(move |left, right| {
        let left = left
            .downcast_ref::<glib::BoxedAnyObject>()
            .expect("file table contains boxed rows")
            .borrow::<ListRow>();
        let right = right
            .downcast_ref::<glib::BoxedAnyObject>()
            .expect("file table contains boxed rows")
            .borrow::<ListRow>();
        match (&*left, &*right) {
            (ListRow::Group(_), ListRow::Group(_)) => gtk::Ordering::Equal,
            (ListRow::Group(_), _) => gtk::Ordering::Smaller,
            (_, ListRow::Group(_)) => gtk::Ordering::Larger,
            (ListRow::File(left), ListRow::File(right)) => compare_items(left, right, column),
        }
    })
}

fn compare_items(left: &FileItem, right: &FileItem, column: ListColumn) -> gtk::Ordering {
    let order = match column {
        ListColumn::Name => left.name().cmp(right.name()),
        ListColumn::DateModified => left.modified().cmp(&right.modified()),
        ListColumn::Type => left.type_name().cmp(&right.type_name()),
        ListColumn::Size => left.size().cmp(&right.size()),
    };
    match order {
        std::cmp::Ordering::Less => gtk::Ordering::Smaller,
        std::cmp::Ordering::Equal => gtk::Ordering::Equal,
        std::cmp::Ordering::Greater => gtk::Ordering::Larger,
    }
}

fn file_target(item: &FileItem) -> ContextTarget {
    if item.is_dir() {
        ContextTarget::Folder(item.clone())
    } else {
        ContextTarget::File(item.clone())
    }
}

fn file_target_from_row(row: &ListRow) -> Option<ContextTarget> {
    match row {
        ListRow::File(item) => Some(file_target(item)),
        ListRow::Group(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use gio::prelude::FileExt;
    use gtk::prelude::{ListModelExt, WidgetExt};

    use super::{
        cached_thumbnail_file_for, command_availability_for_target, drag_uri_for, grid_tile_label,
        local_image_thumbnail_file, thumbnail_cache_homes_from, thumbnail_cache_paths,
        uses_native_row_sorting, FileView,
    };
    use crate::core::navigation::{TabContent, TabState};
    use crate::core::sidebar_model::SidebarSection;
    use crate::core::{FileItem, GroupField, Location, ViewMode, ViewOptions};
    use crate::ui::context_menu::{CommandAvailability, ContextCommand, ContextTarget};

    #[test]
    fn section_overview_preserves_and_restores_a_remote_location() {
        let mut state = TabState::new(Location::from_uri("smb://server/share").unwrap());
        state.show_section(SidebarSection {
            title: "This PC".into(),
            nodes: Vec::new(),
        });

        assert_eq!(state.location.uri(), "smb://server/share");
        state.restore_directory();
        assert_eq!(state.location.uri(), "smb://server/share");
        assert!(matches!(state.content, TabContent::Unavailable(_)));
    }

    #[test]
    fn refreshing_a_section_overview_keeps_its_remote_backing_location() {
        let mut state = TabState::new(Location::from_uri("smb://server/share").unwrap());
        state.show_section(SidebarSection {
            title: "Network & online".into(),
            nodes: Vec::new(),
        });
        state.show_section(SidebarSection {
            title: "Network & online".into(),
            nodes: Vec::new(),
        });

        assert_eq!(state.location.uri(), "smb://server/share");
        assert!(
            matches!(&state.content, TabContent::SectionOverview { section, .. } if section.title == "Network & online")
        );
    }

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn remote_current_location_has_no_local_compatibility_path() {
        gtk::init().unwrap();
        let local = PathBuf::from("/tmp");
        let view = FileView::new(Location::from_local_path(&local));

        view.set_uri("smb://host/share".into());

        assert_eq!(view.current_location(), Location::from_local_path(&local));
        let state = view.tab_state();
        let remote = Location::from_uri("smb://host/share").unwrap();
        let request = state.borrow_mut().begin_navigation(remote.clone()).unwrap();
        state.borrow_mut().apply_directory_result(
            request.generation,
            crate::core::contracts::RequestCompletion {
                id: request.request.id,
                outcome: crate::core::contracts::RequestOutcome::Succeeded(
                    crate::core::contracts::DirectorySnapshot {
                        location: crate::core::ResolvedLocation {
                            requested: remote.clone(),
                            resolved: remote,
                        },
                        entries: vec![],
                        capabilities: Default::default(),
                        next_page: None,
                    },
                ),
            },
        );
        assert_eq!(view.current_location().uri(), "smb://host/share");
        assert!(view.current_location().local_path().is_none());
        assert_eq!(view.command_availability().can_paste, false);
    }

    #[test]
    fn selected_local_item_keeps_toolbar_and_shortcut_paste_available() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("selected.txt");
        fs::write(&path, "contents").unwrap();
        let selected = FileItem::from_path(&path).unwrap();
        let destination = Location::from_local_path(directory.path());

        let toolbar =
            command_availability_for_target(Some(&ContextTarget::File(selected)), &destination);
        let shortcut = CommandAvailability::for_target(&ContextTarget::Background(destination));

        assert!(toolbar.allows(ContextCommand::Paste));
        assert!(shortcut.allows(ContextCommand::Paste));
    }

    #[test]
    fn grouped_lists_do_not_apply_a_second_native_row_sort() {
        assert!(uses_native_row_sorting(GroupField::None));
        assert!(!uses_native_row_sorting(GroupField::DateModified));
        assert!(!uses_native_row_sorting(GroupField::Type));
        assert!(!uses_native_row_sorting(GroupField::Size));
    }

    #[test]
    fn thumbnail_uses_the_gnome_cache_before_the_original_image() {
        let directory = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let image_path = directory.path().join("photo.svg");
        fs::write(&image_path, br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#).unwrap();
        let item = crate::core::file_item::FileItem::from_path(&image_path).unwrap();
        let cached = thumbnail_cache_paths(cache.path(), &gio::File::for_path(&image_path).uri())
            .pop()
            .unwrap();
        fs::create_dir_all(cached.parent().unwrap()).unwrap();
        fs::write(&cached, b"cached thumbnail").unwrap();

        assert_eq!(cached_thumbnail_file_for(&item, cache.path()), Some(cached));
    }

    #[test]
    fn thumbnail_cache_candidates_prefer_large_before_normal() {
        let cache = tempfile::tempdir().unwrap();
        let uri = "file:///home/tester/Downloads/photo.jpeg";
        let paths = thumbnail_cache_paths(cache.path(), uri);

        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].parent().unwrap().file_name().unwrap(), "large");
        assert_eq!(paths[1].parent().unwrap().file_name().unwrap(), "normal");
    }

    #[test]
    fn thumbnail_falls_back_to_the_original_local_image() {
        let directory = tempfile::tempdir().unwrap();
        let image_path = directory.path().join("photo.svg");
        fs::write(&image_path, br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#).unwrap();
        let item = crate::core::file_item::FileItem::from_path(&image_path).unwrap();

        assert_eq!(local_image_thumbnail_file(&item), Some(image_path));
    }

    #[test]
    fn thumbnail_lookup_checks_the_shared_home_cache_before_the_sandbox_cache() {
        let home = PathBuf::from("/home/tester");
        let sandbox_cache = PathBuf::from("/home/tester/.var/app/com.zorin.Explorer/cache");

        assert_eq!(
            thumbnail_cache_homes_from(Some(&home), Some(&sandbox_cache)),
            vec![home.join(".cache"), sandbox_cache]
        );
    }

    #[test]
    fn drag_export_uses_a_file_uri_for_local_items() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.txt");
        fs::write(&path, "contents").unwrap();
        let item = crate::core::file_item::FileItem::from_path(&path).unwrap();

        assert_eq!(
            drag_uri_for(&item),
            Some(gio::File::for_path(path).uri().to_string())
        );
    }

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn grid_tile_label_reserves_two_wrapped_ellipsized_lines() {
        gtk::init().expect("GTK should initialize for widget tests");

        let label = grid_tile_label("A deliberately long folder name", 26);

        assert!(label.wraps());
        assert_eq!(label.lines(), 2);
        assert_eq!(label.ellipsize(), gtk::pango::EllipsizeMode::End);
        assert_eq!(label.max_width_chars(), 26);
    }

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn setting_icon_grid_replaces_the_detail_list_renderer() {
        gtk::init().expect("GTK should initialize for widget tests");
        let view = FileView::new(Location::from_uri("file:///tmp").unwrap());

        view.set_options(ViewOptions {
            view_mode: ViewMode::IconGrid,
            ..ViewOptions::default()
        });

        assert_eq!(view.options().view_mode, ViewMode::IconGrid);
    }

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn icon_grid_requires_double_click_to_activate_an_item() {
        gtk::init().expect("GTK should initialize for widget tests");
        let view = FileView::new(Location::from_uri("file:///tmp").unwrap());
        let grid = view.grid_view(&[], &ViewOptions::default());

        assert!(!grid.activates_on_single_click());
    }

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn list_modes_build_four_reorderable_resizable_columns() {
        gtk::init().expect("GTK should initialize for widget tests");
        let view = FileView::new(Location::from_uri("file:///tmp").unwrap());

        for mode in [ViewMode::DetailedList, ViewMode::CompactList] {
            view.set_options(ViewOptions {
                view_mode: mode,
                ..ViewOptions::default()
            });
            let column_view = view.column_view().expect("list mode must use ColumnView");
            assert_eq!(column_view.columns().n_items(), 4);
            assert!(column_view.is_reorderable());
        }
    }

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn file_scroller_relies_on_the_window_for_its_right_inset() {
        gtk::init().expect("GTK should initialize for widget tests");
        let view = FileView::new(Location::from_uri("file:///tmp").unwrap());

        assert_eq!(view.widget.margin_end(), 0);
    }
}

fn file_icon(item: &FileItem) -> Image {
    if item.is_dir() {
        let icon_name = match item.name() {
            "Desktop" => "user-desktop",
            "Documents" => "folder-documents",
            "Downloads" => "folder-download",
            "Music" => "folder-music",
            "Pictures" => "folder-pictures",
            "Videos" => "folder-videos",
            _ => "folder",
        };
        return Image::from_icon_name(icon_name);
    }

    let icon = gio::content_type_get_icon(item.content_type());
    Image::from_gicon(&icon)
}

fn item_image(item: &FileItem, thumbnails: bool) -> Image {
    let image = file_icon(item);
    apply_thumbnail(&image, item, thumbnails);
    image
}

fn set_item_image(image: &Image, item: &FileItem, thumbnails: bool) {
    let fallback = file_icon(item);
    if let Some(gicon) = fallback.gicon() {
        image.set_from_gicon(&gicon);
    } else {
        image.set_from_icon_name(fallback.icon_name().as_deref());
    }
    apply_thumbnail(image, item, thumbnails);
}

fn apply_thumbnail(image: &Image, item: &FileItem, thumbnails: bool) {
    if !thumbnails {
        return;
    }
    let path = thumbnail_cache_homes()
        .into_iter()
        .find_map(|cache_home| cached_thumbnail_file_for(item, &cache_home))
        .or_else(|| local_image_thumbnail_file(item));
    let Some(path) = path else {
        return;
    };
    if let Ok(texture) = gtk::gdk::Texture::from_filename(path) {
        image.set_from_paintable(Some(&texture));
    }
}

fn cached_thumbnail_file_for(item: &FileItem, cache_home: &Path) -> Option<PathBuf> {
    let uri = thumbnail_uri(item)?;
    thumbnail_cache_paths(cache_home, &uri)
        .into_iter()
        .find(|cached| cached.is_file())
}

fn local_image_thumbnail_file(item: &FileItem) -> Option<PathBuf> {
    item.local_path()
        .filter(|path| item.content_type().starts_with("image/") && path.is_file())
}

fn thumbnail_uri(item: &FileItem) -> Option<String> {
    Some(item.location().uri().to_owned())
}

fn thumbnail_cache_homes() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg_cache = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from);
    thumbnail_cache_homes_from(home.as_deref(), xdg_cache.as_deref())
}

fn thumbnail_cache_homes_from(home: Option<&Path>, xdg_cache: Option<&Path>) -> Vec<PathBuf> {
    let mut homes = Vec::new();
    if let Some(home) = home {
        homes.push(home.join(".cache"));
    }
    if let Some(xdg_cache) = xdg_cache {
        let cache = xdg_cache.to_path_buf();
        if !homes.contains(&cache) {
            homes.push(cache);
        }
    }
    homes
}

fn thumbnail_cache_paths(cache_home: &Path, uri: &str) -> Vec<PathBuf> {
    let mut checksum =
        glib::Checksum::new(glib::ChecksumType::Md5).expect("MD5 is available in GLib");
    checksum.update(uri.as_bytes());
    let filename = format!("{}.png", checksum.string().expect("checksum has a value"));
    ["large", "normal"]
        .into_iter()
        .map(|size| cache_home.join("thumbnails").join(size).join(&filename))
        .collect()
}

fn attach_drag_source(widget: &impl IsA<gtk::Widget>, item: FileItem) {
    let drag_source = gtk::DragSource::new();
    drag_source.set_actions(gtk::gdk::DragAction::COPY);
    drag_source.connect_prepare(move |_, _, _| drag_content_provider(&item));
    widget.add_controller(drag_source);
}

fn drag_content_provider(item: &FileItem) -> Option<gtk::gdk::ContentProvider> {
    let file = gio::File::for_uri(&drag_uri_for(item)?);
    let files = gtk::gdk::FileList::from_array(&[file]);
    Some(gtk::gdk::ContentProvider::for_value(&files.to_value()))
}

fn drag_uri_for(item: &FileItem) -> Option<String> {
    Some(item.location().uri().to_owned())
}

fn format_modified(modified: std::time::SystemTime) -> String {
    let age = std::time::SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default()
        .as_secs();
    if age < 86_400 {
        "Today".into()
    } else if age < 172_800 {
        "Yesterday".into()
    } else {
        format!("{} days ago", age / 86_400)
    }
}

fn activate_item(view: &FileView, item: FileItem) {
    let uri = item.location().uri();
    if item.is_dir() {
        if let Some(callback) = view.on_navigate.borrow().as_ref() {
            callback(item.location().clone());
        } else {
            view.navigate(item.location().clone());
        }
    } else {
        let _ = gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>);
    }
}

fn target_location(target: &ContextTarget) -> Option<Location> {
    match target {
        ContextTarget::File(item) | ContextTarget::Folder(item) => Some(item.location().clone()),
        ContextTarget::Background(location) | ContextTarget::SidebarLocation(location) => {
            Some(location.clone())
        }
        ContextTarget::Tab => None,
    }
}
