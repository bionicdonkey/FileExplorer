use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gio::prelude::*;
use glib::prelude::ToValue;
use gtk::prelude::*;
use gtk::{
    Box as GtkBox, Button, ColumnView, ColumnViewColumn, CustomFilter, CustomSorter,
    FilterListModel, FlowBox, FlowBoxChild, GestureClick, Image, Label, MultiSelection,
    Orientation, Popover, ScrolledWindow, Separator, SignalListItemFactory, SortListModel,
};

use crate::application::commands::{
    ApplicationCommand, ClipboardAction, ClipboardPayload, DragProposal, TransferCompletion,
};
use crate::application::navigation::NavigationCoordinator;
use crate::core::contracts::EntryKind;
use crate::core::directory_model::sort_items;
use crate::core::file_item::FileItem;
use crate::core::navigation::{NavigationStatus, TabContent, TabState};
use crate::core::sidebar_model::SidebarSection;
use crate::core::{
    GroupField, ListColumn, ListColumnLayout, Location, SidebarLocation, SidebarNode,
    SortDirection, SortField, ViewMode, ViewOptions,
};
use crate::infrastructure::gio_directory::GioDirectoryRepository;
use crate::ui::accessibility::{AccessibleStatus, StatusPriority};
use crate::ui::context_menu::{
    configure_command_button, delete_command, item_commands, open_with_apps, CommandAvailability,
    ContextCommand, ContextTarget,
};

#[derive(Clone)]
enum ListRow {
    Group(String),
    File(FileItem),
}

#[derive(Clone)]
enum VisibleMetadataCacheEntry {
    Pending,
    Resolved(crate::core::contracts::VisibleMetadata),
}

struct StreamAppendWaiter {
    generation: u64,
    model: SortListModel,
    handler: glib::SignalHandlerId,
}

struct MetadataSortWaiter {
    generation: u64,
    model: SortListModel,
    handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    active: Rc<std::cell::Cell<bool>>,
}

struct FlowBoxThumbnailWaiter {
    image: glib::WeakRef<Image>,
    item: FileItem,
    thumbnails: bool,
}

/// The only state retained by a visible-metadata completion. Callbacks keep a
/// weak reference to this target, rather than retaining the whole FileView
/// after its tab has closed.
struct VisibleMetadataTarget {
    navigation: NavigationCoordinator,
    model_store: Rc<RefCell<Option<gio::ListStore>>>,
    folder_store: Rc<RefCell<Option<gio::ListStore>>>,
    model_row_index: Rc<RefCell<std::collections::HashMap<Location, u32>>>,
    folder_row_index: Rc<RefCell<std::collections::HashMap<Location, u32>>>,
    stream_append_model: Rc<RefCell<Option<SortListModel>>>,
    metadata_sort_waiter: Rc<RefCell<Option<MetadataSortWaiter>>>,
    pending: Rc<
        RefCell<
            crate::application::performance::VisibleCache<crate::core::contracts::VisibleMetadata>,
        >,
    >,
    generation: Rc<std::cell::Cell<Option<u64>>>,
    cache: Rc<RefCell<crate::application::performance::VisibleCache<VisibleMetadataCacheEntry>>>,
    requests: Rc<
        RefCell<
            std::collections::HashMap<
                crate::core::contracts::RequestId,
                (gio::Cancellable, Vec<Location>),
            >,
        >,
    >,
    flowbox_thumbnail_waiters:
        RefCell<std::collections::HashMap<Location, Vec<FlowBoxThumbnailWaiter>>>,
}

impl VisibleMetadataTarget {
    fn cancel_requests(&self) {
        let mut cancelled_locations = std::collections::HashSet::new();
        for (_, (cancellable, locations)) in self.requests.borrow_mut().drain() {
            cancellable.cancel();
            cancelled_locations.extend(locations);
        }
        if cancelled_locations.is_empty() {
            return;
        }
        self.flowbox_thumbnail_waiters
            .borrow_mut()
            .retain(|location, _| !cancelled_locations.contains(location));

        let retained = self
            .cache
            .borrow()
            .entries()
            .into_iter()
            .filter(|(location, entry)| {
                !cancelled_locations.contains(location)
                    || !matches!(entry, VisibleMetadataCacheEntry::Pending)
            })
            .collect::<Vec<_>>();
        let mut cache = self.cache.borrow_mut();
        cache.invalidate();
        for (location, entry) in retained {
            cache.insert(location, entry);
        }
    }

    fn cancel_metadata_sort_waiter(&self) {
        if let Some(waiter) = self.metadata_sort_waiter.borrow_mut().take() {
            waiter.active.set(false);
            if let Some(handler) = waiter.handler.borrow_mut().take() {
                waiter.model.disconnect(handler);
            }
        }
    }

    fn clear_metadata_sort_waiter(&self, active: &Rc<std::cell::Cell<bool>>) {
        let mut waiter = self.metadata_sort_waiter.borrow_mut();
        if waiter
            .as_ref()
            .is_some_and(|waiter| Rc::ptr_eq(&waiter.active, active))
        {
            waiter.take();
        }
    }

    fn metadata_sort_is_current(&self, generation: u64, model: &SortListModel) -> bool {
        let state = self.navigation.state.borrow();
        if state.navigation_generation != generation
            || matches!(state.status, NavigationStatus::Closed)
        {
            return false;
        }
        drop(state);
        self.stream_append_model
            .borrow()
            .as_ref()
            .is_some_and(|current| current == model)
    }

    fn wait_for_metadata_sort_settlement(self: &Rc<Self>) {
        let generation = self.navigation.state.borrow().navigation_generation;
        let Some(model) = self
            .stream_append_model
            .borrow()
            .as_ref()
            .filter(|model| model.is_incremental() && model.pending() > 0)
            .cloned()
        else {
            return;
        };
        if self
            .metadata_sort_waiter
            .borrow()
            .as_ref()
            .is_some_and(|waiter| waiter.generation == generation && waiter.model == model)
        {
            return;
        }
        self.cancel_metadata_sort_waiter();
        let active = Rc::new(std::cell::Cell::new(true));
        let active_for_notify = active.clone();
        let handler = Rc::new(RefCell::new(None));
        let handler_for_notify = handler.clone();
        let target = Rc::downgrade(self);
        let target_for_notify = target.clone();
        let id = model.connect_pending_notify(move |settled_model| {
            if settled_model.pending() > 0 || !active_for_notify.replace(false) {
                return;
            }
            let Some(id) = handler_for_notify.borrow_mut().take() else {
                return;
            };
            let Some(target) = target_for_notify.upgrade() else {
                return;
            };
            target.clear_metadata_sort_waiter(&active_for_notify);
            let model = settled_model.clone();
            let target = Rc::downgrade(&target);
            glib::idle_add_local_once(move || {
                model.disconnect(id);
                let Some(target) = target.upgrade() else {
                    return;
                };
                if target.metadata_sort_is_current(generation, &model) {
                    target.flush();
                }
            });
        });
        *handler.borrow_mut() = Some(id);
        *self.metadata_sort_waiter.borrow_mut() = Some(MetadataSortWaiter {
            generation,
            model,
            handler,
            active,
        });
    }

    fn clear_cache(&self) {
        self.cancel_metadata_sort_waiter();
        self.cache.borrow_mut().invalidate();
        self.pending.borrow_mut().invalidate();
        self.flowbox_thumbnail_waiters.borrow_mut().clear();
        self.generation.set(None);
    }

    fn request_flowbox_thumbnail_metadata(
        self: &Rc<Self>,
        image: &Image,
        item: &FileItem,
        thumbnails: bool,
    ) {
        let generation = self.prepare_generation();
        let cached = self.cache.borrow_mut().get(item.location());
        if let Some(VisibleMetadataCacheEntry::Resolved(metadata)) = cached.as_ref() {
            apply_resolved_item(image, &item.with_visible_metadata(metadata), thumbnails);
            return;
        }

        self.flowbox_thumbnail_waiters
            .borrow_mut()
            .entry(item.location().clone())
            .or_default()
            .push(FlowBoxThumbnailWaiter {
                image: image.downgrade(),
                item: item.clone(),
                thumbnails,
            });
        if matches!(cached, Some(VisibleMetadataCacheEntry::Pending)) {
            return;
        }

        self.cache
            .borrow_mut()
            .insert(item.location().clone(), VisibleMetadataCacheEntry::Pending);
        let request = crate::core::contracts::VisibleMetadataRequest {
            id: crate::core::contracts::RequestId::allocate(),
            locations: vec![item.location().clone()],
            cancellable: gio::Cancellable::new(),
        };
        self.register(&request);
        let request_id = request.id;
        let target = Rc::downgrade(self);
        self.navigation.inspect_visible_metadata(
            request,
            Box::new(move |result| {
                if let Some(target) = target.upgrade() {
                    target.complete(generation, request_id, result);
                }
            }),
        );
    }

    fn apply_flowbox_thumbnail_waiters(
        &self,
        metadata: &[crate::core::contracts::VisibleMetadata],
    ) {
        let metadata_by_location: std::collections::HashMap<_, _> = metadata
            .iter()
            .map(|entry| (entry.location.clone(), entry))
            .collect();
        let waiters = {
            let mut waiters = self.flowbox_thumbnail_waiters.borrow_mut();
            metadata_by_location
                .keys()
                .filter_map(|location| waiters.remove(location))
                .flatten()
                .collect::<Vec<_>>()
        };
        for waiter in waiters {
            let Some(metadata) = metadata_by_location.get(waiter.item.location()) else {
                continue;
            };
            let Some(image) = waiter.image.upgrade() else {
                continue;
            };
            apply_resolved_item(
                &image,
                &waiter.item.with_visible_metadata(metadata),
                waiter.thumbnails,
            );
        }
    }

    fn discard_missing_successful_metadata(
        &self,
        requested: &[Location],
        metadata: &[crate::core::contracts::VisibleMetadata],
    ) {
        let returned = metadata
            .iter()
            .map(|entry| entry.location.clone())
            .collect::<std::collections::HashSet<_>>();
        let missing = requested
            .iter()
            .filter(|location| !returned.contains(*location))
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        if missing.is_empty() {
            return;
        }

        self.flowbox_thumbnail_waiters
            .borrow_mut()
            .retain(|location, _| !missing.contains(location));
        let mut cache = self.cache.borrow_mut();
        let retained_cache = cache
            .entries()
            .into_iter()
            .filter(|(location, entry)| {
                !missing.contains(location)
                    || matches!(entry, VisibleMetadataCacheEntry::Resolved(_))
            })
            .collect::<Vec<_>>();
        cache.invalidate();
        for (location, entry) in retained_cache {
            cache.insert(location, entry);
        }
        drop(cache);
        let mut pending = self.pending.borrow_mut();
        let retained_pending = pending
            .entries()
            .into_iter()
            .filter(|(location, _)| !missing.contains(location))
            .collect::<Vec<_>>();
        pending.invalidate();
        for (location, entry) in retained_pending {
            pending.insert(location, entry);
        }
    }

    fn register(&self, request: &crate::core::contracts::VisibleMetadataRequest) {
        self.requests.borrow_mut().insert(
            request.id,
            (request.cancellable.clone(), request.locations.clone()),
        );
    }

    fn complete(
        self: &Rc<Self>,
        generation: u64,
        request_id: crate::core::contracts::RequestId,
        result: crate::core::contracts::RequestCompletion<
            Vec<crate::core::contracts::VisibleMetadata>,
        >,
    ) {
        let Some((_, requested)) = self.requests.borrow_mut().remove(&request_id) else {
            return;
        };
        if result.id != request_id
            || self.navigation.state.borrow().navigation_generation != generation
            || matches!(
                self.navigation.state.borrow().status,
                NavigationStatus::Closed
            )
        {
            return;
        }
        if let crate::core::contracts::RequestOutcome::Succeeded(metadata) = result.outcome {
            self.discard_missing_successful_metadata(&requested, &metadata);
            self.apply(metadata.clone());
            self.apply_flowbox_thumbnail_waiters(&metadata);
        }
    }

    fn prepare_generation(&self) -> u64 {
        let generation = self.navigation.state.borrow().navigation_generation;
        if self.generation.get() != Some(generation) {
            self.cache.borrow_mut().invalidate();
            self.pending.borrow_mut().invalidate();
            self.generation.set(Some(generation));
        }
        generation
    }

    fn apply(self: &Rc<Self>, metadata: Vec<crate::core::contracts::VisibleMetadata>) {
        self.prepare_generation();
        if metadata.is_empty() {
            return;
        }
        let metadata_by_location: std::collections::HashMap<_, _> = metadata
            .iter()
            .cloned()
            .map(|entry| (entry.location.clone(), entry))
            .collect();
        {
            let mut cache = self.cache.borrow_mut();
            for entry in metadata {
                cache.insert(
                    entry.location.clone(),
                    VisibleMetadataCacheEntry::Resolved(entry),
                );
            }
        }
        self.queue(metadata_by_location.into_values());
        self.flush();
    }

    fn queue(&self, metadata: impl IntoIterator<Item = crate::core::contracts::VisibleMetadata>) {
        let mut cache = self.cache.borrow_mut();
        let mut pending = self.pending.borrow_mut();
        for entry in metadata {
            if matches!(
                cache.get(&entry.location),
                Some(VisibleMetadataCacheEntry::Resolved(_))
            ) {
                pending.insert(entry.location.clone(), entry);
            }
        }
    }

    fn flush(self: &Rc<Self>) {
        // An in-flight page stream needs GTK's incremental sorter to reach a
        // settled pending state before the next backing batch is appended.
        // Splicing a resolved metadata row here restarts that sorter, so retain
        // the value in the bounded generation cache. A generation-scoped
        // sorter-settlement continuation drains it once GTK has settled.
        if self.navigation.state.borrow().has_pending_navigation()
            || self
                .stream_append_model
                .borrow()
                .as_ref()
                .is_some_and(|model| model.is_incremental() && model.pending() > 0)
        {
            self.wait_for_metadata_sort_settlement();
            return;
        }
        let mut metadata_by_location: std::collections::HashMap<_, _> = {
            let mut cache = self.cache.borrow_mut();
            self.pending
                .borrow()
                .entries()
                .into_iter()
                .filter(|(location, _)| {
                    matches!(
                        cache.get(location),
                        Some(VisibleMetadataCacheEntry::Resolved(_))
                    )
                })
                .collect()
        };
        if metadata_by_location.is_empty() {
            self.pending.borrow_mut().invalidate();
            return;
        }
        let mut applied = self.apply_to_store(
            self.model_store.borrow().as_ref(),
            &self.model_row_index.borrow(),
            &metadata_by_location,
        );
        applied.extend(self.apply_to_store(
            self.folder_store.borrow().as_ref(),
            &self.folder_row_index.borrow(),
            &metadata_by_location,
        ));
        metadata_by_location.retain(|location, _| !applied.contains(location));
        let mut pending = self.pending.borrow_mut();
        pending.invalidate();
        for entry in metadata_by_location.into_values() {
            pending.insert(entry.location.clone(), entry);
        }
        drop(pending);
        self.wait_for_metadata_sort_settlement();
    }

    fn apply_to_store(
        &self,
        store: Option<&gio::ListStore>,
        row_index: &std::collections::HashMap<Location, u32>,
        metadata: &std::collections::HashMap<Location, crate::core::contracts::VisibleMetadata>,
    ) -> std::collections::HashSet<Location> {
        let Some(store) = store else {
            return Default::default();
        };
        let mut applied = std::collections::HashSet::new();
        let mut updates = Vec::new();
        for (location, entry) in metadata {
            let Some(&index) = row_index.get(location) else {
                continue;
            };
            let Some(object) = store.item(index).and_downcast::<glib::BoxedAnyObject>() else {
                continue;
            };
            let ListRow::File(item) = &*object.borrow::<ListRow>() else {
                continue;
            };
            if item.location() != location {
                continue;
            }
            updates.push((
                index,
                glib::BoxedAnyObject::new(ListRow::File(item.with_visible_metadata(entry))),
            ));
            applied.insert(location.clone());
        }
        splice_updated_rows(store, updates);
        applied
    }
}

/// Runs a completion action exactly once after an incremental sort settles.
/// Disconnect and mutate selection from an idle turn so neither operation
/// reenters GTK's pending-notify emission.
fn connect_once_when_sort_settled(model: &SortListModel, action: impl Fn() + 'static) {
    let handler = Rc::new(RefCell::new(None));
    let handler_for_notify = handler.clone();
    let action: Rc<dyn Fn()> = Rc::new(action);
    let action_for_notify = action.clone();
    let completed = Rc::new(std::cell::Cell::new(false));
    let completed_for_notify = completed.clone();
    let id = model.connect_pending_notify(move |model| {
        if model.pending() > 0 || completed_for_notify.replace(true) {
            return;
        }
        let Some(id) = handler_for_notify.borrow_mut().take() else {
            return;
        };
        let model = model.clone();
        let action = action_for_notify.clone();
        glib::idle_add_local_once(move || {
            model.disconnect(id);
            action();
        });
    });
    if completed.get() {
        model.disconnect(id);
        let action = action.clone();
        glib::idle_add_local_once(move || action());
    } else {
        *handler.borrow_mut() = Some(id);
    }
}

fn append_model_page(
    store: &gio::ListStore,
    items: Vec<FileItem>,
    group_field: GroupField,
    show_hidden: bool,
) -> Vec<(Location, u32)> {
    let mut groups = std::collections::HashSet::new();
    if group_field != GroupField::None {
        for index in 0..store.n_items() {
            if let Some(object) = store.item(index).and_downcast::<glib::BoxedAnyObject>() {
                if let ListRow::Group(group) = &*object.borrow::<ListRow>() {
                    groups.insert(group.clone());
                }
            }
        }
    }
    let first_index = store.n_items();
    let mut rows = Vec::with_capacity(items.len());
    let mut file_indexes = Vec::with_capacity(items.len());
    for item in items {
        if let Some(group) = crate::core::directory_model::group_label(
            &item,
            group_field,
            std::time::SystemTime::now(),
        ) {
            if (show_hidden || !item.name().starts_with('.')) && groups.insert(group.clone()) {
                rows.push(glib::BoxedAnyObject::new(ListRow::Group(group)));
            }
        }
        file_indexes.push((item.location().clone(), first_index + rows.len() as u32));
        rows.push(glib::BoxedAnyObject::new(ListRow::File(item)));
    }
    store.extend_from_slice(&rows);
    file_indexes
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

/// How soon a monitor-driven refresh may follow the previous one, and how far
/// that delay grows while a directory keeps changing.
const MONITOR_REFRESH_BASE_MS: u64 = 100;
const MONITOR_REFRESH_MAX_MS: u64 = 10_000;

/// Produces the one availability snapshot used by the toolbar while retaining
/// the displayed location as the Paste destination even when an item is
/// selected.
#[cfg(test)]
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

/// Replaces rows of `store` in place, one splice per cluster of nearby rows
/// instead of one per row. Every splice makes the sort and filter models above
/// the store reprocess, which on a large directory costs milliseconds each; the
/// rows a refresh touches are mostly neighbours, so a few splices cover them.
/// Rows between two updates are re-inserted unchanged.
fn splice_updated_rows(store: &gio::ListStore, mut updates: Vec<(u32, glib::BoxedAnyObject)>) {
    const MAX_GAP: u32 = 8;
    updates.sort_unstable_by_key(|(index, _)| *index);
    let mut clusters: Vec<Vec<(u32, glib::BoxedAnyObject)>> = Vec::new();
    for update in updates {
        match clusters.last_mut() {
            Some(cluster)
                if cluster
                    .last()
                    .is_some_and(|(last, _)| update.0 - *last <= MAX_GAP + 1) =>
            {
                cluster.push(update)
            }
            _ => clusters.push(vec![update]),
        }
    }
    for cluster in clusters.into_iter().rev() {
        let start = cluster[0].0;
        let end = cluster[cluster.len() - 1].0;
        let mut updated: std::collections::HashMap<u32, glib::BoxedAnyObject> =
            cluster.into_iter().collect();
        let mut rows = Vec::with_capacity((end - start + 1) as usize);
        for index in start..=end {
            match updated.remove(&index) {
                Some(row) => rows.push(row),
                None => match store.item(index).and_downcast::<glib::BoxedAnyObject>() {
                    Some(row) => rows.push(row),
                    None => return,
                },
            }
        }
        store.splice(start, end - start + 1, &rows);
    }
}

/// How many separate row changes an in-place refresh applies before a rebuild
/// is cheaper, before allowing for the size of the directory.
const MAX_IN_PLACE_SPLICES: usize = 8;

/// A rendered list pane: the widget on screen and the backing store a refresh
/// can splice in place instead of rebuilding the widget.
#[derive(Clone)]
struct ListPane {
    store: gio::ListStore,
    recycled: bool,
}

/// A rendered `FlowBox` pane, with the live item vector its closures index into
/// and the options its tiles were built with.
/// The grid is held weakly: its closures reach back into the view, so a strong
/// handle here would keep the whole view alive forever.
#[derive(Clone)]
struct FlowPane {
    grid: glib::WeakRef<FlowBox>,
    items: Rc<RefCell<Vec<FileItem>>>,
    options: ViewOptions,
}

#[derive(Clone)]
enum Pane {
    List(ListPane),
    Flow(FlowPane),
}

#[derive(Clone)]
enum RenderedPanes {
    Single(Pane),
    Hybrid(Option<Pane>, Option<Pane>),
}

/// What is on screen now: enough to decide whether a refresh can update the
/// existing widgets in place rather than tearing them down.
#[derive(Clone)]
struct RenderedView {
    location: Location,
    options: ViewOptions,
    search: bool,
    panes: RenderedPanes,
}

/// What the window's status bar shows about the focused tab: whether its
/// listing is still streaming, and how many folders and files the directory
/// it is showing holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewStatus {
    pub loading: bool,
    pub directory: bool,
    pub folders: usize,
    pub files: usize,
}

#[derive(Clone)]
pub struct FileView {
    pub widget: ScrolledWindow,
    navigation: NavigationCoordinator,
    search: crate::application::search::SearchCoordinator,
    on_location_committed: Rc<RefCell<Option<Box<dyn Fn(Location)>>>>,
    on_navigate: Rc<RefCell<Option<Box<dyn Fn(Location)>>>>,
    on_close: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    on_context_command: Rc<RefCell<Option<Box<dyn Fn(ContextTarget, ContextCommand)>>>>,
    on_application_command: Rc<RefCell<Option<Box<dyn Fn(ApplicationCommand)>>>>,
    selection: Rc<RefCell<Vec<FileItem>>>,
    on_options_changed: Rc<RefCell<Option<Box<dyn Fn(ViewOptions)>>>>,
    options: Rc<RefCell<ViewOptions>>,
    selected_target: Rc<RefCell<Option<ContextTarget>>>,
    on_command_availability_changed: Rc<RefCell<Option<Box<dyn Fn(CommandAvailability)>>>>,
    on_status: Rc<RefCell<Option<Box<dyn Fn(ViewStatus)>>>>,
    rendered_view: Rc<RefCell<Option<RenderedView>>>,
    /// Set when a monitor event invalidates cached thumbnails and content
    /// types: rows an in-place refresh keeps must re-request theirs, since
    /// only freshly bound widgets ask on their own.
    metadata_stale: Rc<std::cell::Cell<bool>>,
    pending_cut: Rc<RefCell<Vec<Location>>>,
    is_quick_access_location: Rc<RefCell<Option<Rc<dyn Fn(&Location) -> bool>>>>,
    model_store: Rc<RefCell<Option<gio::ListStore>>>,
    folder_store: Rc<RefCell<Option<gio::ListStore>>>,
    model_row_index: Rc<RefCell<std::collections::HashMap<Location, u32>>>,
    folder_row_index: Rc<RefCell<std::collections::HashMap<Location, u32>>>,
    rendered_generation: Rc<std::cell::Cell<Option<u64>>>,
    /// The list on screen is a plain store built in one go, not a paged
    /// stream: its rows take metadata, but the stream machinery must not
    /// treat it as a partially appended model.
    plain_list: Rc<std::cell::Cell<bool>>,
    rendered_count: Rc<std::cell::Cell<usize>>,
    stream_append_waiter: Rc<RefCell<Option<StreamAppendWaiter>>>,
    stream_append_model: Rc<RefCell<Option<SortListModel>>>,
    directory_monitor: Rc<RefCell<Option<(Location, gio::FileMonitor)>>>,
    pending_visible_metadata: Rc<
        RefCell<
            crate::application::performance::VisibleCache<crate::core::contracts::VisibleMetadata>,
        >,
    >,
    visible_metadata_generation: Rc<std::cell::Cell<Option<u64>>>,
    /// The listing carries no size, date or type, so sorting by one of them
    /// needs those fetched for every entry. Holds the navigation generation
    /// that has already been asked, so an entry the provider cannot answer for
    /// is not requested again on every repaint.
    sort_metadata_generation: Rc<std::cell::Cell<Option<u64>>>,
    /// Characters typed to find an item by name, and when the last arrived.
    type_ahead: Rc<RefCell<(String, std::time::Instant)>>,
    monitor_refresh_pending: Rc<std::cell::Cell<bool>>,
    /// Delay before the next monitor-driven refresh, doubled per consecutive
    /// refresh so a directory that never stops changing cannot keep the view
    /// cycling forever; a quiet interval resets it.
    monitor_backoff_ms: Rc<std::cell::Cell<u64>>,
    last_monitor_change: Rc<std::cell::Cell<std::time::Instant>>,
    controllers: Rc<RefCell<Vec<gtk::EventController>>>,
    stream_selection: Rc<RefCell<Option<Vec<String>>>>,
    /// Identity keys of the selection when a plain left click was pressed, so
    /// the release can tell "clicked what was already selected" apart from
    /// "clicked something new".
    selection_click_keys: Rc<RefCell<Option<Vec<String>>>>,
    /// Set while a selection change is programmatic rather than a click, so the
    /// pane it came from does not write an emptied selection back.
    syncing_selection: Rc<std::cell::Cell<bool>>,
    visible_metadata:
        Rc<RefCell<crate::application::performance::VisibleCache<VisibleMetadataCacheEntry>>>,
    metadata_target: Rc<VisibleMetadataTarget>,
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
        scroll.update_property(&[gtk::accessible::Property::Label("Files")]);
        let on_navigate = Rc::new(RefCell::new(None));
        let state = Rc::new(RefCell::new(TabState::new(current_location)));
        let navigation = NavigationCoordinator::new(state.clone(), repository);
        let model_store: Rc<RefCell<Option<gio::ListStore>>> = Default::default();
        let folder_store: Rc<RefCell<Option<gio::ListStore>>> = Default::default();
        let model_row_index: Rc<RefCell<std::collections::HashMap<Location, u32>>> =
            Default::default();
        let folder_row_index: Rc<RefCell<std::collections::HashMap<Location, u32>>> =
            Default::default();
        let pending_visible_metadata = Rc::new(RefCell::new(
            crate::application::performance::VisibleCache::new(Default::default()),
        ));
        let visible_metadata_generation: Rc<std::cell::Cell<Option<u64>>> = Default::default();
        let visible_metadata = Rc::new(RefCell::new(
            crate::application::performance::VisibleCache::new(Default::default()),
        ));
        let stream_append_model: Rc<RefCell<Option<SortListModel>>> = Default::default();
        let metadata_target = Rc::new(VisibleMetadataTarget {
            navigation: navigation.clone(),
            model_store: model_store.clone(),
            folder_store: folder_store.clone(),
            model_row_index: model_row_index.clone(),
            folder_row_index: folder_row_index.clone(),
            stream_append_model: stream_append_model.clone(),
            metadata_sort_waiter: Default::default(),
            pending: pending_visible_metadata.clone(),
            generation: visible_metadata_generation.clone(),
            cache: visible_metadata.clone(),
            requests: Default::default(),
            flowbox_thumbnail_waiters: Default::default(),
        });
        let view = Self {
            widget: scroll,
            search: crate::application::search::SearchCoordinator::new(
                state.clone(),
                search_repository,
            ),
            navigation,
            on_location_committed: Rc::new(RefCell::new(None)),
            on_navigate,
            on_close: Rc::new(RefCell::new(None)),
            on_context_command: Rc::new(RefCell::new(None)),
            on_application_command: Rc::new(RefCell::new(None)),
            selection: Rc::new(RefCell::new(Vec::new())),
            on_options_changed: Rc::new(RefCell::new(None)),
            options: Rc::new(RefCell::new(ViewOptions::default())),
            selected_target: Rc::new(RefCell::new(None)),
            on_command_availability_changed: Rc::new(RefCell::new(None)),
            pending_cut: Rc::new(RefCell::new(Vec::new())),
            is_quick_access_location: Rc::new(RefCell::new(None)),
            model_store,
            folder_store,
            model_row_index,
            folder_row_index,
            rendered_generation: Default::default(),
            plain_list: Default::default(),
            rendered_count: Default::default(),
            stream_append_waiter: Default::default(),
            stream_append_model,
            directory_monitor: Default::default(),
            pending_visible_metadata,
            visible_metadata_generation,
            sort_metadata_generation: Default::default(),
            type_ahead: Rc::new(RefCell::new((String::new(), std::time::Instant::now()))),
            monitor_refresh_pending: Default::default(),
            monitor_backoff_ms: Rc::new(std::cell::Cell::new(MONITOR_REFRESH_BASE_MS)),
            last_monitor_change: Rc::new(std::cell::Cell::new(std::time::Instant::now())),
            controllers: Default::default(),
            stream_selection: Default::default(),
            selection_click_keys: Default::default(),
            syncing_selection: Default::default(),
            on_status: Default::default(),
            rendered_view: Default::default(),
            metadata_stale: Default::default(),
            visible_metadata,
            metadata_target,
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
        view.widget.add_controller(background.clone());
        view.controllers.borrow_mut().push(background.upcast());
        // Neither a FlowBox in multiple-selection mode nor a list row turns a
        // click on an already-selected item into a deselection, and neither
        // clears the selection when the click lands on empty space, so both are
        // decided here. Capture phase sees the press and the release before the
        // row or tile applies its own selection in the bubble phase.
        let pressed_view = view.clone();
        let pressed_root = view.widget.clone();
        let deselect = GestureClick::new();
        deselect.set_button(1);
        deselect.set_propagation_phase(gtk::PropagationPhase::Capture);
        deselect.connect_pressed(move |gesture, _, x, y| {
            let modifiers = gesture.current_event_state();
            let modifying = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                || modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let pressed = if modifying || click_is_on_a_control_or_header(&pressed_root, x, y) {
                None
            } else {
                Some(selection_keys_of(&pressed_view.selected_items()))
            };
            *pressed_view.selection_click_keys.borrow_mut() = pressed;
        });
        let released_view = view.clone();
        deselect.connect_released(move |_, _, _, _| {
            let Some(pressed) = released_view.selection_click_keys.borrow_mut().take() else {
                return;
            };
            let view = released_view.clone();
            // The pane applies its own selection while this release is still
            // propagating, so the verdict waits for the main loop.
            glib::idle_add_local_once(move || {
                let released = selection_keys_of(&view.selected_items());
                if click_leaves_selection_unchanged(&pressed, &released) {
                    view.clear_selection();
                }
            });
        });
        view.widget.add_controller(deselect.clone());
        view.controllers.borrow_mut().push(deselect.upcast());
        let keyboard_view = view.clone();
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if keyboard_view.handle_type_ahead_key(key, modifiers) {
                return glib::Propagation::Stop;
            }
            if !crate::ui::accessibility::context_menu_key(key, modifiers)
                || keyboard_view.section_title().is_some()
            {
                return glib::Propagation::Proceed;
            }
            let target = keyboard_view
                .selected_target()
                .unwrap_or_else(|| ContextTarget::Background(keyboard_view.current_location()));
            show_item_context_menu(
                &keyboard_view.widget,
                12.0,
                12.0,
                target,
                &keyboard_view,
                false,
            );
            glib::Propagation::Stop
        });
        view.widget.add_controller(keys.clone());
        view.controllers.borrow_mut().push(keys.upcast());
        let drop_view = view.clone();
        let drop_controller = attach_async_drop(&view.widget, view.clone(), move || {
            drop_view
                .command_availability()
                .can_paste
                .then(|| drop_view.current_location())
        });
        view.controllers.borrow_mut().push(drop_controller.upcast());
        view.refresh();
        view
    }

    /// Typing a name selects the first matching item and scrolls to it. A
    /// pause of a second starts a new search; Backspace shortens the current
    /// one and Escape abandons it. Returns whether the key was used.
    pub fn handle_type_ahead_key(
        &self,
        key: gtk::gdk::Key,
        modifiers: gtk::gdk::ModifierType,
    ) -> bool {
        const RESET_AFTER: std::time::Duration = std::time::Duration::from_secs(1);
        if self.section_title().is_some()
            || modifiers.intersects(
                gtk::gdk::ModifierType::CONTROL_MASK
                    | gtk::gdk::ModifierType::ALT_MASK
                    | gtk::gdk::ModifierType::SUPER_MASK
                    | gtk::gdk::ModifierType::META_MASK,
            )
        {
            return false;
        }
        let editing = self
            .widget
            .root()
            .and_downcast::<gtk::Window>()
            .and_then(|window| gtk::prelude::GtkWindowExt::focus(&window))
            .is_some_and(|focused| {
                focused.is::<gtk::Editable>()
                    || focused.ancestor(gtk::Text::static_type()).is_some()
            });
        if editing {
            return false;
        }
        let query = {
            let mut typed = self.type_ahead.borrow_mut();
            if typed.1.elapsed() > RESET_AFTER {
                typed.0.clear();
            }
            let active = !typed.0.is_empty();
            if key == gtk::gdk::Key::Escape && active {
                typed.0.clear();
                return true;
            }
            if key == gtk::gdk::Key::BackSpace && active {
                typed.0.pop();
            } else {
                let Some(character) = key.to_unicode().filter(|c| !c.is_control()) else {
                    return false;
                };
                if character.is_whitespace() && !active {
                    return false;
                }
                typed.0.push(character);
            }
            typed.1 = std::time::Instant::now();
            typed.0.to_lowercase()
        };
        if !query.is_empty() {
            self.select_by_name(&query);
        }
        true
    }

    /// Selects the first item, in display order, whose name starts with the
    /// query, or failing that contains it.
    fn select_by_name(&self, query: &str) {
        enum Pane {
            Column(ColumnView),
            Grid(gtk::GridView),
            Flow(FlowBox),
        }
        fn collect(widget: &gtk::Widget, panes: &mut Vec<Pane>) {
            if let Some(view) = widget.downcast_ref::<ColumnView>() {
                panes.push(Pane::Column(view.clone()));
                return;
            }
            if let Some(view) = widget.downcast_ref::<gtk::GridView>() {
                panes.push(Pane::Grid(view.clone()));
                return;
            }
            if let Some(grid) = widget.downcast_ref::<FlowBox>() {
                panes.push(Pane::Flow(grid.clone()));
                return;
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                collect(&next, panes);
                child = next.next_sibling();
            }
        }
        fn model_names(model: Option<gtk::SelectionModel>) -> Vec<Option<String>> {
            let Some(model) = model else {
                return Vec::new();
            };
            (0..model.n_items())
                .map(|index| {
                    let object = model.item(index).and_downcast::<glib::BoxedAnyObject>()?;
                    let name = match &*object.borrow::<ListRow>() {
                        ListRow::File(item) => Some(item.name().to_lowercase()),
                        ListRow::Group(_) => None,
                    };
                    name
                })
                .collect()
        }
        let mut panes = Vec::new();
        collect(self.widget.upcast_ref(), &mut panes);
        let flow_items = |grid: &FlowBox| -> Vec<Option<String>> {
            let rendered = self.rendered_view.borrow();
            let Some(rendered) = rendered.as_ref() else {
                return Vec::new();
            };
            let candidates = match &rendered.panes {
                RenderedPanes::Single(pane) => vec![Some(pane)],
                RenderedPanes::Hybrid(folders, files) => vec![folders.as_ref(), files.as_ref()],
            };
            candidates
                .into_iter()
                .flatten()
                .find_map(|pane| match pane {
                    self::Pane::Flow(pane) if pane.grid.upgrade().as_ref() == Some(grid) => Some(
                        pane.items
                            .borrow()
                            .iter()
                            .map(|item| Some(item.name().to_lowercase()))
                            .collect(),
                    ),
                    _ => None,
                })
                .unwrap_or_default()
        };
        let names: Vec<Vec<Option<String>>> = panes
            .iter()
            .map(|pane| match pane {
                Pane::Column(view) => model_names(view.model()),
                Pane::Grid(view) => model_names(view.model()),
                Pane::Flow(grid) => flow_items(grid),
            })
            .collect();
        let find = |matches: &dyn Fn(&str) -> bool| {
            names.iter().enumerate().find_map(|(pane, names)| {
                names
                    .iter()
                    .position(|name| name.as_deref().is_some_and(matches))
                    .map(|index| (pane, index as u32))
            })
        };
        let Some((pane, index)) =
            find(&|name| name.starts_with(query)).or_else(|| find(&|name| name.contains(query)))
        else {
            return;
        };
        let flags = gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT;
        match &panes[pane] {
            Pane::Column(view) => view.scroll_to(index, None, flags, None),
            Pane::Grid(view) => view.scroll_to(index, flags, None),
            Pane::Flow(grid) => {
                if let Some(child) = grid.child_at_index(index as i32) {
                    grid.unselect_all();
                    grid.select_child(&child);
                    child.grab_focus();
                }
            }
        }
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
        // An explicit request is a fresh render; only refreshes of the folder
        // already shown may splice into the widgets on screen.
        *self.rendered_view.borrow_mut() = None;
        self.clear_search_results();
        self.cancel_stream_append_waiter();
        self.metadata_target.cancel_metadata_sort_waiter();
        self.metadata_target.cancel_requests();
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
        if !self.navigation.state.borrow().history.can_go_back() {
            return;
        }
        self.cancel_stream_append_waiter();
        self.metadata_target.cancel_metadata_sort_waiter();
        self.metadata_target.cancel_requests();
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
        if !self.navigation.state.borrow().history.can_go_forward() {
            return;
        }
        self.cancel_stream_append_waiter();
        self.metadata_target.cancel_metadata_sort_waiter();
        self.metadata_target.cancel_requests();
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
        self.cancel_stream_append_waiter();
        self.metadata_target.cancel_metadata_sort_waiter();
        self.metadata_target.cancel_requests();
        self.metadata_target.clear_cache();
        self.navigation.close_tab();
        if let Some((_, monitor)) = self.directory_monitor.borrow_mut().take() {
            monitor.cancel();
        }
        self.model_store.borrow_mut().take();
        self.folder_store.borrow_mut().take();
        self.model_row_index.borrow_mut().clear();
        self.folder_row_index.borrow_mut().clear();
        self.widget.set_child(gtk::Widget::NONE);
        for controller in self.controllers.borrow_mut().drain(..) {
            self.widget.remove_controller(&controller);
        }
        self.on_navigate.borrow_mut().take();
        self.on_location_committed.borrow_mut().take();
        self.on_close.borrow_mut().take();
        self.on_context_command.borrow_mut().take();
        self.on_application_command.borrow_mut().take();
        self.on_options_changed.borrow_mut().take();
        self.on_command_availability_changed.borrow_mut().take();
    }

    pub fn set_close_handler(&self, callback: impl Fn() + 'static) {
        *self.on_close.borrow_mut() = Some(Rc::new(callback));
    }

    pub fn mark_unavailable(&self, root: &Location) {
        if self.navigation.state.borrow_mut().mark_unavailable(root) {
            if self.current_location().is_within(root) {
                self.search.clear();
            }
            if !self.navigation.state.borrow().has_available_directory() {
                *self.selected_target.borrow_mut() = None;
                self.selection.borrow_mut().clear();
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
        self.publish_status();
    }

    fn navigation_finished(&self) {
        self.watch_committed_directory();
        // A stream extends the existing model in bounded pages. Replacing the
        // widget here would repeatedly reset scrolling, focus and cell caches.
        if self.append_directory_page() {
            self.complete_directory_stream();
            self.ensure_sort_metadata();
            return;
        }
        let state = self.navigation.state.borrow();
        let location = state.location.clone();
        let succeeded = matches!(state.status, NavigationStatus::Ready);
        drop(state);
        if succeeded {
            AccessibleStatus::announce_for(
                &self.widget,
                &format!("Opened {}", location.display_name()),
                StatusPriority::Polite,
            );
            let state = self.navigation.state.borrow();
            let stream_keys = state
                .has_pending_navigation()
                .then(|| state.selection_keys.clone());
            let restored = state
                .directory
                .as_ref()
                .map(|snapshot| {
                    snapshot
                        .entries
                        .iter()
                        .filter(|entry| {
                            state
                                .selection_keys
                                .iter()
                                .any(|key| key == entry.location.uri())
                        })
                        .map(FileItem::from_snapshot)
                        .collect()
                })
                .unwrap_or_default();
            drop(state);
            self.select_items(restored);
            if let Some(keys) = stream_keys {
                self.navigation.state.borrow_mut().selection_keys = keys.clone();
                *self.stream_selection.borrow_mut() = Some(keys);
            }
            if let Some(callback) = self.on_location_committed.borrow().as_ref() {
                callback(location);
            }
        }
        self.reload();
    }

    fn request_visible_metadata(&self, item: &FileItem) {
        let generation = self.metadata_target.prepare_generation();
        let mut cache = self.visible_metadata.borrow_mut();
        if cache.get(item.location()).is_some() {
            return;
        }
        cache.insert(item.location().clone(), VisibleMetadataCacheEntry::Pending);
        drop(cache);
        let request = crate::core::contracts::VisibleMetadataRequest {
            id: crate::core::contracts::RequestId::allocate(),
            locations: vec![item.location().clone()],
            cancellable: gio::Cancellable::new(),
        };
        self.metadata_target.register(&request);
        let request_id = request.id;
        let target = Rc::downgrade(&self.metadata_target);
        self.navigation.inspect_visible_metadata(
            request,
            Box::new(move |result| {
                if let Some(target) = target.upgrade() {
                    target.complete(generation, request_id, result);
                }
            }),
        );
    }

    /// The compact FlowBox grid owns concrete image widgets instead of a
    /// ListModel row. Its rendered file tiles are therefore the visible-range
    /// boundary for metadata needed to promote a deferred thumbnail.
    fn request_flowbox_thumbnail_metadata(&self, image: &Image, item: &FileItem, thumbnails: bool) {
        // The listing carries no content type, so the icon needs this answer
        // whether or not thumbnails are enabled.
        if item.is_dir() || item.content_type_resolved() {
            return;
        }
        self.metadata_target
            .request_flowbox_thumbnail_metadata(image, item, thumbnails);
    }

    #[cfg(test)]
    fn apply_visible_metadata(&self, metadata: Vec<crate::core::contracts::VisibleMetadata>) {
        self.prepare_visible_metadata_generation();
        if metadata.is_empty() {
            return;
        }
        let metadata_by_location: std::collections::HashMap<_, _> = metadata
            .iter()
            .cloned()
            .map(|entry| (entry.location.clone(), entry))
            .collect();
        {
            let mut cache = self.visible_metadata.borrow_mut();
            for entry in metadata {
                cache.insert(
                    entry.location.clone(),
                    VisibleMetadataCacheEntry::Resolved(entry),
                );
            }
        }
        let mut applied = self.apply_visible_metadata_to_store(
            self.model_store.borrow().as_ref(),
            &self.model_row_index.borrow(),
            &metadata_by_location,
        );
        applied.extend(self.apply_visible_metadata_to_store(
            self.folder_store.borrow().as_ref(),
            &self.folder_row_index.borrow(),
            &metadata_by_location,
        ));
        self.queue_visible_metadata(
            metadata_by_location
                .into_iter()
                .filter_map(|(location, entry)| (!applied.contains(&location)).then_some(entry)),
        );
        self.flush_visible_metadata();
    }

    #[cfg(test)]
    fn complete_visible_metadata(
        &self,
        generation: u64,
        _requested: Location,
        metadata: Vec<crate::core::contracts::VisibleMetadata>,
    ) {
        if self.navigation.state.borrow().navigation_generation != generation {
            return;
        }
        self.prepare_visible_metadata_generation();
        self.apply_visible_metadata(metadata);
    }

    fn prepare_visible_metadata_generation(&self) -> u64 {
        let generation = self.navigation.state.borrow().navigation_generation;
        if self.visible_metadata_generation.get() != Some(generation) {
            self.visible_metadata.borrow_mut().invalidate();
            self.pending_visible_metadata.borrow_mut().invalidate();
            self.visible_metadata_generation.set(Some(generation));
        }
        generation
    }

    #[cfg(test)]
    fn resolved_visible_metadata_count(&self) -> usize {
        self.visible_metadata
            .borrow()
            .entries()
            .iter()
            .filter(|(_, entry)| matches!(entry, VisibleMetadataCacheEntry::Resolved(_)))
            .count()
    }

    fn queue_visible_metadata(
        &self,
        metadata: impl IntoIterator<Item = crate::core::contracts::VisibleMetadata>,
    ) {
        let mut cache = self.visible_metadata.borrow_mut();
        let mut pending = self.pending_visible_metadata.borrow_mut();
        for entry in metadata {
            if matches!(
                cache.get(&entry.location),
                Some(VisibleMetadataCacheEntry::Resolved(_))
            ) {
                pending.insert(entry.location.clone(), entry);
            }
        }
    }

    fn reapply_cached_visible_metadata(&self) {
        self.prepare_visible_metadata_generation();
        let cached = self.visible_metadata.borrow().entries();
        self.queue_visible_metadata(cached.into_iter().filter_map(|(_, entry)| match entry {
            VisibleMetadataCacheEntry::Resolved(metadata) => Some(metadata),
            VisibleMetadataCacheEntry::Pending => None,
        }));
        self.flush_visible_metadata();
    }

    /// Metadata for an indexed backing row is safe to apply immediately. Rows
    /// from later virtual pages stay in a generation-scoped bounded cache until
    /// their URI index exists.
    fn flush_visible_metadata(&self) {
        let mut metadata_by_location: std::collections::HashMap<_, _> = {
            let mut cache = self.visible_metadata.borrow_mut();
            self.pending_visible_metadata
                .borrow()
                .entries()
                .into_iter()
                .filter(|(location, _)| {
                    matches!(
                        cache.get(location),
                        Some(VisibleMetadataCacheEntry::Resolved(_))
                    )
                })
                .collect()
        };
        if metadata_by_location.is_empty() {
            self.pending_visible_metadata.borrow_mut().invalidate();
            return;
        }
        let mut applied = self.apply_visible_metadata_to_store(
            self.model_store.borrow().as_ref(),
            &self.model_row_index.borrow(),
            &metadata_by_location,
        );
        applied.extend(self.apply_visible_metadata_to_store(
            self.folder_store.borrow().as_ref(),
            &self.folder_row_index.borrow(),
            &metadata_by_location,
        ));
        metadata_by_location.retain(|location, _| !applied.contains(location));
        let mut pending = self.pending_visible_metadata.borrow_mut();
        pending.invalidate();
        for entry in metadata_by_location.into_values() {
            pending.insert(entry.location.clone(), entry);
        }
        drop(pending);
        self.metadata_target.wait_for_metadata_sort_settlement();
    }

    /// Replaces only backing rows named by the response. The source-list
    /// replacement emits GTK's normal items-changed signal, keeping its
    /// incremental sort/filter wrappers in sync without scanning the store.
    fn apply_visible_metadata_to_store(
        &self,
        store: Option<&gio::ListStore>,
        row_index: &std::collections::HashMap<Location, u32>,
        metadata: &std::collections::HashMap<Location, crate::core::contracts::VisibleMetadata>,
    ) -> std::collections::HashSet<Location> {
        let Some(store) = store else {
            return Default::default();
        };
        let mut applied = std::collections::HashSet::new();
        let mut updates = Vec::new();
        for (location, entry) in metadata {
            let Some(&index) = row_index.get(location) else {
                continue;
            };
            let Some(object) = store.item(index).and_downcast::<glib::BoxedAnyObject>() else {
                continue;
            };
            let ListRow::File(item) = &*object.borrow::<ListRow>() else {
                continue;
            };
            // A view may have reloaded after the asynchronous query started.
            // Keep a stale index from ever changing a different URI.
            if item.location() != location {
                continue;
            }
            updates.push((
                index,
                glib::BoxedAnyObject::new(ListRow::File(item.with_visible_metadata(entry))),
            ));
            applied.insert(location.clone());
        }
        splice_updated_rows(store, updates);
        applied
    }

    fn watch_committed_directory(&self) {
        let state = self.navigation.state.borrow();
        if !matches!(state.status, NavigationStatus::Ready) {
            return;
        }
        let location = state.location.clone();
        drop(state);
        if self
            .directory_monitor
            .borrow()
            .as_ref()
            .is_some_and(|(current, _)| *current == location)
        {
            return;
        }
        if let Some((_, monitor)) = self.directory_monitor.borrow_mut().take() {
            monitor.cancel();
        }
        let view = self.clone();
        if let Some(monitor) = self.navigation.watch(&location, move || {
            view.arm_monitor_refresh();
        }) {
            *self.directory_monitor.borrow_mut() = Some((location, monitor));
        }
        // A different directory has its own churn; start reacting promptly again.
        self.monitor_backoff_ms.set(MONITOR_REFRESH_BASE_MS);
    }

    /// Coalesces directory-monitor churn into one debounced refresh. A busy
    /// directory such as `/tmp` changes continuously; starting a new listing
    /// while the previous one is still streaming thousands of entries rebuilds
    /// the view over and over and never lets it settle, so a refresh requested
    /// during a listing is held back and re-armed once that listing completes.
    fn arm_monitor_refresh(&self) {
        let now = std::time::Instant::now();
        let quiet_for = now.duration_since(self.last_monitor_change.get());
        self.last_monitor_change.set(now);
        if quiet_for >= std::time::Duration::from_millis(self.monitor_backoff_ms.get()) {
            // The directory stayed quiet for a whole cycle: whatever churns it
            // has stopped, so react promptly again.
            self.monitor_backoff_ms.set(MONITOR_REFRESH_BASE_MS);
        }
        if self.monitor_refresh_pending.replace(true) {
            return;
        }
        invalidate_thumbnails();
        self.visible_metadata.borrow_mut().invalidate();
        self.visible_metadata_generation.set(None);
        self.metadata_stale.set(true);
        self.schedule_monitor_refresh();
    }

    fn schedule_monitor_refresh(&self) {
        let delay = std::time::Duration::from_millis(self.monitor_backoff_ms.get());
        let view = self.clone();
        glib::timeout_add_local_once(delay, move || {
            view.run_monitor_refresh();
        });
    }

    fn run_monitor_refresh(&self) {
        self.monitor_refresh_pending.set(false);
        if matches!(
            self.navigation.state.borrow().status,
            NavigationStatus::Closed
        ) || self.is_showing_search_results()
        {
            return;
        }
        if self.has_pending_navigation() {
            // The listing in flight already predates these changes; refresh
            // once it completes instead of starting a competing one.
            self.monitor_refresh_pending.set(true);
            return;
        }
        self.monitor_backoff_ms
            .set((self.monitor_backoff_ms.get() * 2).min(MONITOR_REFRESH_MAX_MS));
        self.refresh();
    }

    fn show_error(&self) {
        let status = self.navigation.state.borrow().status.clone();
        if let NavigationStatus::Failed { error, .. } = status {
            AccessibleStatus::announce_for(&self.widget, error.message_key, StatusPriority::Urgent);
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
        self.publish_status();
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

    pub fn selected_items(&self) -> Vec<FileItem> {
        self.selection.borrow().clone()
    }

    /// Drops every pane's visual selection, then the logical one. This is what
    /// a click on empty space, or on the item that was already selected, means.
    fn clear_selection(&self) {
        clear_pane_selection(self.widget.upcast_ref::<gtk::Widget>());
        self.select_items(Vec::new());
    }

    /// Hybrid view renders two panes that own their selections independently.
    /// Selecting in one clears the other, so the highlight matches the logical
    /// selection, which only ever names the pane that was clicked last.
    fn link_hybrid_panes(&self, folders: &gtk::Widget, files: &gtk::Widget) {
        self.clear_other_pane_on_selection(folders, files);
        self.clear_other_pane_on_selection(files, folders);
    }

    fn clear_other_pane_on_selection(&self, source: &gtk::Widget, other: &gtk::Widget) {
        let syncing = self.syncing_selection.clone();
        let other = other.downgrade();
        connect_pane_selection_changed(source, move |selected| {
            if syncing.get() || selected == 0 {
                return;
            }
            let Some(other) = other.upgrade() else {
                return;
            };
            syncing.set(true);
            clear_pane_selection(&other);
            syncing.set(false);
        });
    }

    fn select_items(&self, items: Vec<FileItem>) {
        if self.syncing_selection.get() {
            // Clearing the other hybrid pane, not a click in this one.
            return;
        }
        *self.selected_target.borrow_mut() = items.first().map(file_target);
        *self.selection.borrow_mut() = items;
        self.notify_command_availability_changed();
        if self.stream_selection.borrow().is_some() && self.has_pending_navigation() {
            *self.stream_selection.borrow_mut() =
                Some(self.navigation.state.borrow().selection_keys.clone());
        }
    }

    fn restore_stream_selection(&self, keys: Vec<String>) {
        if keys.is_empty() {
            return;
        }
        fn models(widget: &gtk::Widget, result: &mut Vec<gtk::SelectionModel>) {
            if let Some(view) = widget.downcast_ref::<ColumnView>() {
                if let Some(model) = view.model() {
                    result.push(model);
                }
                return;
            }
            if let Some(view) = widget.downcast_ref::<gtk::GridView>() {
                if let Some(model) = view.model() {
                    result.push(model);
                }
                return;
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                models(&next, result);
                child = next.next_sibling();
            }
        }
        let mut selection_models = Vec::new();
        models(self.widget.upcast_ref(), &mut selection_models);
        let keys: std::collections::HashSet<_> = keys.into_iter().collect();
        let view = self.clone();
        let generation = self.navigation.state.borrow().navigation_generation;
        glib::idle_add_local(move || {
            if view.navigation.state.borrow().navigation_generation != generation
                || matches!(
                    view.navigation.state.borrow().status,
                    NavigationStatus::Closed
                )
            {
                return glib::ControlFlow::Break;
            }
            if selection_models.iter().any(|model| {
                model
                    .downcast_ref::<MultiSelection>()
                    .and_then(|model| model.model())
                    .and_downcast::<SortListModel>()
                    .is_some_and(|model| model.pending() > 0)
            }) {
                return glib::ControlFlow::Continue;
            }
            let mut restored = Vec::new();
            for model in &selection_models {
                let selected = gtk::Bitset::new_empty();
                for index in 0..model.n_items() {
                    if let Some(object) = model.item(index).and_downcast::<glib::BoxedAnyObject>() {
                        if let ListRow::File(item) = &*object.borrow::<ListRow>() {
                            if keys.contains(item.identity_key()) {
                                selected.add(index);
                                restored.push(item.clone());
                            }
                        }
                    }
                }
                model.set_selection(&selected, &gtk::Bitset::new_range(0, model.n_items()));
            }
            view.select_items(restored);
            glib::ControlFlow::Break
        });
    }

    fn select_context_item(&self, item: &FileItem) {
        if !self
            .selection
            .borrow()
            .iter()
            .any(|i| i.location() == item.location())
        {
            if let Some(model) = self.column_view().and_then(|view| view.model()) {
                for index in 0..model.n_items() {
                    let matches = model.item(index).and_downcast::<glib::BoxedAnyObject>().is_some_and(|o| matches!(&*o.borrow::<ListRow>(), ListRow::File(row) if row.location() == item.location()));
                    if matches {
                        model.select_item(index, true);
                        return;
                    }
                }
            }
            self.select_items(vec![item.clone()]);
        }
    }

    pub fn set_status_handler(&self, callback: impl Fn(ViewStatus) + 'static) {
        *self.on_status.borrow_mut() = Some(Box::new(callback));
        self.publish_status();
    }

    /// Re-sends the status snapshot. The window also asks for one when the
    /// selected tab changes, since each tab owns its own listing.
    pub fn publish_status(&self) {
        let status = self.current_status();
        if let Some(callback) = self.on_status.borrow().as_ref() {
            callback(status);
        }
    }

    fn current_status(&self) -> ViewStatus {
        let state = self.navigation.state.borrow();
        let loading = matches!(state.status, NavigationStatus::Loading(_));
        let directory =
            matches!(state.content, TabContent::Directory(_)) && !self.is_showing_search_results();
        // Counted as displayed: a hidden entry the view is filtering out must
        // not appear in the totals beside it.
        let show_hidden = self.options.borrow().show_hidden;
        let (folders, files) = if directory {
            state
                .directory
                .as_ref()
                .map(|snapshot| {
                    snapshot
                        .entries
                        .iter()
                        .filter(|entry| show_hidden || !entry.display_name.starts_with('.'))
                        .fold((0, 0), |(folders, files), entry| {
                            if matches!(entry.kind, EntryKind::Directory) {
                                (folders + 1, files)
                            } else {
                                (folders, files + 1)
                            }
                        })
                })
                .unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        ViewStatus {
            loading,
            directory,
            folders,
            files,
        }
    }

    pub fn set_application_handler(&self, callback: impl Fn(ApplicationCommand) + 'static) {
        *self.on_application_command.borrow_mut() = Some(Box::new(callback));
    }

    pub fn drag_payload(&self, action: ClipboardAction) -> Option<ClipboardPayload> {
        let availability = self.command_availability();
        if !(match action {
            ClipboardAction::Copy => availability.can_copy,
            ClipboardAction::Cut => availability.can_cut,
        }) {
            return None;
        }
        Some(ClipboardPayload {
            action,
            locations: self
                .selected_items()
                .iter()
                .map(|i| i.location().clone())
                .collect(),
        })
    }

    pub fn dispatch_drop(&self, text: &str, action: ClipboardAction) -> bool {
        if !self.command_availability().can_paste {
            return false;
        }
        self.dispatch_transfer(text, action, self.current_location(), false, None)
    }

    fn dispatch_transfer(
        &self,
        text: &str,
        action: ClipboardAction,
        destination: Location,
        source_deletes_on_success: bool,
        completion: Option<TransferCompletion>,
    ) -> bool {
        let Ok(payload) = ClipboardPayload::parse(action, text) else {
            return false;
        };
        let mut command = DragProposal {
            sources: payload.locations,
            destination,
            action,
        }
        .command();
        command.completion = completion;
        command.source_deletes_on_success = source_deletes_on_success;
        if let Some(callback) = self.on_application_command.borrow().as_ref() {
            callback(command);
            true
        } else {
            false
        }
    }

    pub fn dispatch_drop_on(&self, folder: &FileItem, text: &str, action: ClipboardAction) -> bool {
        if !self.navigation.state.borrow().has_available_directory()
            || !folder.is_dir()
            || !folder
                .capabilities()
                .supports(crate::core::contracts::Capability::Create)
        {
            return false;
        }
        let Ok(payload) = ClipboardPayload::parse(action, text) else {
            return false;
        };
        if let Some(callback) = self.on_application_command.borrow().as_ref() {
            callback(
                DragProposal {
                    sources: payload.locations,
                    destination: folder.location().clone(),
                    action,
                }
                .command(),
            );
            true
        } else {
            false
        }
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
        CommandAvailability::for_capabilities(
            &self.selected_items(),
            self.navigation
                .state
                .borrow()
                .directory
                .as_ref()
                .map(|d| &d.capabilities),
        )
    }

    pub fn set_command_availability_handler<F: Fn(CommandAvailability) + 'static>(
        &self,
        callback: F,
    ) {
        *self.on_command_availability_changed.borrow_mut() = Some(Box::new(callback));
        self.notify_command_availability_changed();
    }

    fn notify_command_availability_changed(&self) {
        self.navigation.state.borrow_mut().selection_keys = self
            .selection
            .borrow()
            .iter()
            .map(|i| i.identity_key().to_owned())
            .collect();
        if let Some(callback) = self.on_command_availability_changed.borrow().as_ref() {
            callback(self.command_availability());
        }
    }

    pub fn dispatch_context_command(&self, target: ContextTarget, command: ContextCommand) {
        if !self.navigation.state.borrow().has_available_directory() {
            return;
        }
        if !self.command_availability().allows(command) {
            return;
        }
        if let Some(callback) = self.on_application_command.borrow().as_ref() {
            callback(ApplicationCommand::route(
                command,
                if matches!(target, ContextTarget::Background(_)) {
                    Vec::new()
                } else {
                    self.selected_items()
                        .iter()
                        .map(|i| i.location().clone())
                        .collect()
                },
                self.current_location(),
            ));
            return;
        }
        if let Some(callback) = self.on_context_command.borrow().as_ref() {
            callback(target, command);
        }
    }

    pub fn set_pending_cut(&self, path: Option<PathBuf>) {
        self.set_pending_cut_locations(path.iter().map(|p| Location::from_local_path(p)).collect());
    }

    pub fn dispatch_open_with(&self, application: gio::AppInfo) {
        if !self.command_availability().allows(ContextCommand::OpenWith) {
            return;
        }
        let mut command = ApplicationCommand::route(
            ContextCommand::OpenWith,
            self.selected_items()
                .iter()
                .map(|i| i.location().clone())
                .collect(),
            self.current_location(),
        );
        command.application = Some(application);
        if let Some(callback) = self.on_application_command.borrow().as_ref() {
            callback(command);
        }
    }

    pub fn set_pending_cut_locations(&self, locations: Vec<Location>) {
        *self.pending_cut.borrow_mut() = locations;
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
        self.selection.borrow_mut().clear();
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
        AccessibleStatus::announce_for(
            &self.widget,
            &outcome.status_text(),
            StatusPriority::Polite,
        );
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
        invalidate_thumbnails();
        self.metadata_stale.set(true);
        self.clear_search_results();
        self.cancel_stream_append_waiter();
        self.metadata_target.cancel_metadata_sort_waiter();
        self.metadata_target.cancel_requests();
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
        self.selection.borrow_mut().clear();
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
        self.cancel_stream_append_waiter();
        self.metadata_target.cancel_metadata_sort_waiter();
        self.metadata_target.cancel_requests();
        self.stream_append_model.borrow_mut().take();
        self.model_store.borrow_mut().take();
        self.plain_list.set(false);
        self.prepare_visible_metadata_generation();
        self.folder_store.borrow_mut().take();
        self.model_row_index.borrow_mut().clear();
        self.folder_row_index.borrow_mut().clear();
        self.rendered_generation.set(None);
        let owned_focus = self
            .widget
            .root()
            .and_downcast::<gtk::Window>()
            .and_then(|window| gtk::prelude::GtkWindowExt::focus(&window))
            .is_some_and(|focused| {
                focused == self.widget.clone().upcast::<gtk::Widget>()
                    || focused.is_ancestor(&self.widget)
            });
        self.reload_content();
        {
            let state = self.navigation.state.borrow();
            if matches!(state.content, TabContent::Directory(_))
                && self.model_store.borrow().is_some()
                && !self.plain_list.get()
            {
                self.rendered_generation
                    .set(Some(state.navigation_generation));
                self.rendered_count.set(
                    state
                        .directory
                        .as_ref()
                        .map_or(0, |snapshot| snapshot.entries.len()),
                );
            }
        }
        // The remaining calls re-enter GTK signal handlers (child_focus emits
        // selected-children-changed), which borrow this state mutably.
        self.reapply_cached_visible_metadata();
        if owned_focus {
            self.widget.child_focus(gtk::DirectionType::TabForward);
        }
        self.publish_status();
        self.ensure_sort_metadata();
    }

    /// Size, date and type are not part of a listing, so a sort on one of them
    /// has nothing to order by until they are fetched. Asks for every entry
    /// once the listing is complete, records the answers in the snapshot, and
    /// repaints.
    fn ensure_sort_metadata(&self) {
        if self.options().sort_field == SortField::Name {
            return;
        }
        let (generation, locations) = {
            let state = self.navigation.state.borrow();
            if state.has_pending_navigation()
                || !matches!(state.status, NavigationStatus::Ready)
                || !matches!(state.content, TabContent::Directory(_))
                || self.sort_metadata_generation.get() == Some(state.navigation_generation)
            {
                return;
            }
            let Some(snapshot) = &state.directory else {
                return;
            };
            let locations: Vec<Location> = snapshot
                .entries
                .iter()
                .filter(|entry| {
                    entry.size.is_none() && entry.modified.is_none() && entry.content_type.is_none()
                })
                .map(|entry| entry.location.clone())
                .collect();
            (state.navigation_generation, locations)
        };
        if locations.is_empty() {
            return;
        }
        self.sort_metadata_generation.set(Some(generation));
        let request = crate::core::contracts::VisibleMetadataRequest {
            id: crate::core::contracts::RequestId::allocate(),
            locations,
            cancellable: gio::Cancellable::new(),
        };
        let request_id = request.id;
        let view = self.clone();
        self.navigation.inspect_visible_metadata(
            request,
            Box::new(move |result| {
                let crate::core::contracts::RequestOutcome::Succeeded(metadata) = result.outcome
                else {
                    return;
                };
                if result.id != request_id {
                    return;
                }
                {
                    let mut state = view.navigation.state.borrow_mut();
                    if state.navigation_generation != generation
                        || matches!(state.status, NavigationStatus::Closed)
                    {
                        return;
                    }
                    let Some(snapshot) = &mut state.directory else {
                        return;
                    };
                    let mut by_location: std::collections::HashMap<_, _> = metadata
                        .into_iter()
                        .map(|entry| (entry.location.clone(), entry))
                        .collect();
                    for entry in &mut snapshot.entries {
                        if let Some(found) = by_location.remove(&entry.location) {
                            entry.size = found.size;
                            entry.modified = found.modified;
                            entry.content_type = found.content_type;
                        }
                    }
                }
                // The rows on screen are in the old order and an in-place
                // refresh would keep them there.
                *view.rendered_view.borrow_mut() = None;
                view.reload();
            }),
        );
    }

    fn reload_content(&self) {
        let state = self.navigation.state.borrow().clone();
        let options = self.options();
        let mut items = match state.content {
            TabContent::Unavailable(location)
                if !matches!(state.status, NavigationStatus::Loading(_)) =>
            {
                let content = GtkBox::new(Orientation::Vertical, 12);
                AccessibleStatus::announce_for(
                    &self.widget,
                    "This location is unavailable. Reconnect or close the tab.",
                    StatusPriority::Urgent,
                );
                content.append(&Label::new(Some("This location is unavailable")));
                content.append(&Label::new(Some(&location.display_name())));
                let reconnect = Button::with_label("Reconnect");
                let view = self.clone();
                reconnect.connect_clicked(move |_| view.refresh());
                content.append(&reconnect);
                let close = Button::with_label("Close");
                let handler = self.on_close.clone();
                close.connect_clicked(move |_| {
                    // Cloned into a binding first: a scrutinee temporary would
                    // keep the guard alive through the body, and the callback
                    // re-enters `FileView::close`, which clears this handler.
                    let callback = handler.borrow().clone();
                    if let Some(callback) = callback {
                        callback();
                    }
                });
                content.append(&close);
                self.widget.set_child(Some(&content));
                *self.rendered_view.borrow_mut() = None;
                return;
            }
            TabContent::SectionOverview { section, .. } => {
                self.render_section_overview(&section);
                *self.rendered_view.borrow_mut() = None;
                self.show_error();
                return;
            }
            TabContent::Search { items, .. } => items,
            _ => match state.directory {
                Some(snapshot) => snapshot
                    .entries
                    .iter()
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
        if items.len() <= crate::application::performance::PAGE_SIZE {
            sort_items(&mut items, &options);
        }
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
            *self.rendered_view.borrow_mut() = None;
            return;
        }
        let location = self.navigation.state.borrow().location.clone();
        let search = self.is_showing_search_results();
        if let Some(rendered) = self.rendered_view.borrow().clone() {
            if rendered.location == location
                && rendered.options == *options
                && rendered.search == search
                && self.apply_in_place(&rendered.panes, &items, options)
            {
                // The widgets on screen absorbed the new listing; rebuilding
                // them would flash an empty view and drop scroll and focus.
                self.metadata_stale.set(false);
                let plain = |pane: &Pane| matches!(pane, Pane::List(list) if !list.recycled);
                self.plain_list.set(match &rendered.panes {
                    RenderedPanes::Single(pane) => plain(pane),
                    RenderedPanes::Hybrid(_, files) => files.as_ref().is_some_and(plain),
                });
                return;
            }
        }
        // Fresh widgets request their own thumbnails and content types.
        self.metadata_stale.set(false);
        let recycled = self.use_recycled_model(items.len());
        match options.view_mode {
            ViewMode::DetailedList | ViewMode::CompactList => {
                let view = self.column_view_for(&items, options);
                self.remember_rendered(RenderedView {
                    location,
                    options: options.clone(),
                    search,
                    panes: RenderedPanes::Single(self.list_pane(recycled)),
                });
                self.widget.set_child(Some(&view));
            }
            ViewMode::IconGrid if recycled => {
                let grid = self.recycled_grid(&items, options, true, true);
                self.remember_rendered(RenderedView {
                    location,
                    options: options.clone(),
                    search,
                    panes: RenderedPanes::Single(self.list_pane(true)),
                });
                self.widget.set_child(Some(&grid));
            }
            ViewMode::IconGrid => {
                let (grid, pane) = self.grid_view(&items, options);
                self.remember_rendered(RenderedView {
                    location,
                    options: options.clone(),
                    search,
                    panes: RenderedPanes::Single(Pane::Flow(pane)),
                });
                self.widget.set_child(Some(&grid));
            }
            ViewMode::Hybrid => {
                let (root, panes) = self.hybrid_view(&items, options);
                self.remember_rendered(RenderedView {
                    location,
                    options: options.clone(),
                    search,
                    panes,
                });
                self.widget.set_child(Some(&root));
            }
        };
    }

    fn remember_rendered(&self, rendered: RenderedView) {
        *self.rendered_view.borrow_mut() = Some(rendered);
    }

    fn list_pane(&self, recycled: bool) -> Pane {
        let store = self
            .model_store
            .borrow()
            .clone()
            .expect("a list render stores its backing model");
        Pane::List(ListPane { store, recycled })
    }

    /// Splices a fresh listing into the widgets already on screen. Returns
    /// false when the change is structural -- grouped rows, a mode flip, a
    /// reordered grid -- and the caller must rebuild instead.
    fn apply_in_place(
        &self,
        panes: &RenderedPanes,
        items: &[FileItem],
        options: &ViewOptions,
    ) -> bool {
        match panes {
            RenderedPanes::Single(Pane::List(pane)) => {
                pane.recycled == self.use_recycled_model(items.len())
                    && self.diff_list(pane, items, options, false)
            }
            RenderedPanes::Single(Pane::Flow(pane)) => {
                !self.use_recycled_model(items.len()) && self.diff_flow(pane, items)
            }
            RenderedPanes::Hybrid(folders, files) => {
                let folder_items: Vec<_> =
                    items.iter().filter(|item| item.is_dir()).cloned().collect();
                let file_items: Vec<_> = items
                    .iter()
                    .filter(|item| !item.is_dir())
                    .cloned()
                    .collect();
                let recycled = self.use_recycled_model(items.len());
                let folders_ok = match (folders, folder_items.is_empty()) {
                    (None, true) => true,
                    (Some(Pane::Flow(pane)), false) => {
                        !recycled && self.diff_flow(pane, &folder_items)
                    }
                    (Some(Pane::List(pane)), false) => {
                        pane.recycled == recycled
                            && self.diff_list(pane, &folder_items, options, true)
                    }
                    _ => false,
                };
                let file_options = ViewOptions {
                    view_mode: ViewMode::DetailedList,
                    folders_first: false,
                    ..options.clone()
                };
                let files_ok = match (files, file_items.is_empty()) {
                    (None, true) => true,
                    (Some(Pane::List(pane)), false) => {
                        pane.recycled == self.use_recycled_model(file_items.len())
                            && self.diff_list(pane, &file_items, &file_options, false)
                    }
                    _ => false,
                };
                folders_ok && files_ok
            }
        }
    }

    fn diff_list(
        &self,
        pane: &ListPane,
        items: &[FileItem],
        options: &ViewOptions,
        folder_pane: bool,
    ) -> bool {
        if options.group_field != GroupField::None {
            return false;
        }
        let store = &pane.store;
        let mut current: std::collections::HashMap<String, (u32, FileItem)> =
            std::collections::HashMap::new();
        for index in 0..store.n_items() {
            let Some(object) = store.item(index).and_downcast::<glib::BoxedAnyObject>() else {
                return false;
            };
            let row = object.borrow::<ListRow>();
            match &*row {
                ListRow::File(item) => {
                    current.insert(item.identity_key().to_owned(), (index, item.clone()));
                }
                ListRow::Group(_) => return false,
            }
        }
        let new_keys: std::collections::HashSet<&str> =
            items.iter().map(|item| item.identity_key()).collect();
        if new_keys.len() != items.len() {
            return false;
        }
        let mut removals: Vec<u32> = current
            .values()
            .filter(|(_, item)| !new_keys.contains(item.identity_key()))
            .map(|(index, _)| *index)
            .collect();
        removals.sort_unstable();
        // Contiguous removals are one splice. Every separate splice makes the
        // sort and filter models above the store reprocess, which on a large
        // directory costs far more than rebuilding once.
        let mut runs: Vec<(u32, u32)> = Vec::new();
        for index in removals {
            match runs.last_mut() {
                Some((start, len)) if *start + *len == index => *len += 1,
                _ => runs.push((index, 1)),
            }
        }
        let changed: Vec<&FileItem> = items
            .iter()
            .filter(|item| {
                current.get(item.identity_key()).is_some_and(|(_, old)| {
                    old.size() != item.size()
                        || old.modified() != item.modified()
                        || old.name() != item.name()
                })
            })
            .collect();
        let appended: Vec<glib::BoxedAnyObject> = items
            .iter()
            .filter(|item| !current.contains_key(item.identity_key()))
            .map(|item| glib::BoxedAnyObject::new(ListRow::File((*item).clone())))
            .collect();
        let splices = runs.len() + changed.len() + usize::from(!appended.is_empty());
        if splices > MAX_IN_PLACE_SPLICES + store.n_items() as usize / 64 {
            return false;
        }
        for (start, len) in runs.into_iter().rev() {
            store.splice(start, len, &[] as &[glib::BoxedAnyObject]);
        }
        if !changed.is_empty() {
            let mut positions: std::collections::HashMap<String, u32> =
                std::collections::HashMap::new();
            for index in 0..store.n_items() {
                if let Some(object) = store.item(index).and_downcast::<glib::BoxedAnyObject>() {
                    if let ListRow::File(item) = &*object.borrow::<ListRow>() {
                        positions.insert(item.identity_key().to_owned(), index);
                    }
                }
            }
            let updates = changed
                .into_iter()
                .filter_map(|item| {
                    positions.get(item.identity_key()).map(|&index| {
                        (
                            index,
                            glib::BoxedAnyObject::new(ListRow::File(item.clone())),
                        )
                    })
                })
                .collect();
            splice_updated_rows(store, updates);
        }
        if !appended.is_empty() {
            store.splice(store.n_items(), 0, &appended);
        }
        // The backing store is whole again; repoint the stream and metadata
        // bookkeeping at it, wherever the pane lives.
        let mut row_index = if folder_pane {
            *self.folder_store.borrow_mut() = Some(store.clone());
            self.folder_row_index.borrow_mut()
        } else {
            *self.model_store.borrow_mut() = Some(store.clone());
            self.model_row_index.borrow_mut()
        };
        row_index.clear();
        for index in 0..store.n_items() {
            if let Some(object) = store.item(index).and_downcast::<glib::BoxedAnyObject>() {
                let row = object.borrow::<ListRow>();
                if let ListRow::File(item) = &*row {
                    row_index.insert(item.location().clone(), index);
                }
            }
        }
        self.rendered_count.set(items.len());
        true
    }

    fn diff_flow(&self, pane: &FlowPane, items: &[FileItem]) -> bool {
        // Children come and go while the position-indexed vector is stale, so
        // the grid's own selection signals would publish the wrong items.
        // Hold them back and publish once from the settled vector.
        let owned_keys: std::collections::HashSet<String> = pane
            .items
            .borrow()
            .iter()
            .map(|item| item.identity_key().to_owned())
            .collect();
        let owned_selection = {
            let selection = self.selection.borrow();
            !selection.is_empty()
                && selection
                    .iter()
                    .all(|item| owned_keys.contains(item.identity_key()))
        };
        let was_syncing = self.syncing_selection.replace(true);
        let applied = self.splice_flow(pane, items);
        self.syncing_selection.set(was_syncing);
        if applied && !was_syncing {
            if let Some(grid) = pane.grid.upgrade() {
                let selected = grid
                    .selected_children()
                    .iter()
                    .filter_map(|child| pane.items.borrow().get(child.index() as usize).cloned())
                    .collect::<Vec<_>>();
                // A sibling hybrid pane may hold the selection; leave it be.
                if owned_selection || !selected.is_empty() {
                    self.select_items(selected);
                }
            }
        }
        applied
    }

    fn splice_flow(&self, pane: &FlowPane, items: &[FileItem]) -> bool {
        let Some(grid) = pane.grid.upgrade() else {
            return false;
        };
        let old_items = pane.items.borrow().clone();
        let children: Vec<FlowBoxChild> = {
            let mut collected = Vec::new();
            let mut index = 0;
            while let Some(child) = grid.child_at_index(index) {
                collected.push(child);
                index += 1;
            }
            collected
        };
        if children.len() != old_items.len() {
            return false;
        }
        let new_keys: std::collections::HashSet<&str> =
            items.iter().map(|item| item.identity_key()).collect();
        if new_keys.len() != items.len() {
            return false;
        }
        for (index, child) in children.iter().enumerate().rev() {
            if !new_keys.contains(old_items[index].identity_key()) {
                grid.remove(child);
            }
        }
        let mut live: Vec<FileItem> = old_items
            .into_iter()
            .filter(|item| new_keys.contains(item.identity_key()))
            .collect();
        for (position, item) in items.iter().enumerate() {
            match live.get(position) {
                Some(existing) if existing.identity_key() == item.identity_key() => {
                    let Some(child) = grid.child_at_index(position as i32) else {
                        return false;
                    };
                    if self.metadata_stale.get()
                        || existing.size() != item.size()
                        || existing.modified() != item.modified()
                        || existing.name() != item.name()
                    {
                        child.set_child(Some(&self.grid_tile_widget(item, &pane.options)));
                    }
                }
                Some(_) => return false, // reordered: rebuild rather than shuffle
                None => {
                    let tile = self.grid_tile_widget(item, &pane.options);
                    grid.insert(&tile, position as i32);
                    if let Some(child) = tile.parent().and_downcast::<FlowBoxChild>() {
                        child.update_property(&[gtk::accessible::Property::Label(item.name())]);
                    }
                    live.insert(position, item.clone());
                }
            }
        }
        while grid.child_at_index(items.len() as i32).is_some() {
            if let Some(child) = grid.child_at_index(items.len() as i32) {
                grid.remove(&child);
            }
        }
        *pane.items.borrow_mut() = items.to_vec();
        true
    }

    fn column_view_for(&self, items: &[FileItem], options: &ViewOptions) -> ColumnView {
        if self.use_recycled_model(items.len()) {
            return self.large_column_view(items, options, true, true);
        }
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        *self.model_store.borrow_mut() = Some(store.clone());
        self.plain_list.set(true);
        let mut row_index = self.model_row_index.borrow_mut();
        row_index.clear();
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
            row_index.insert(item.location().clone(), store.n_items());
            store.append(&glib::BoxedAnyObject::new(ListRow::File(item.clone())));
        }
        drop(row_index);

        // Group headings are part of this model. Applying a second GTK sort would
        // move the file rows independently and detach them from their headings.
        let native_row_sorting = uses_native_row_sorting(options.group_field);
        let sorted = native_row_sorting
            .then(|| SortListModel::new(Some(store.clone()), None::<gtk::Sorter>));
        let selection = match &sorted {
            Some(sorted) => MultiSelection::new(Some(sorted.clone())),
            None => MultiSelection::new(Some(store)),
        };
        let selected_keys = self.navigation.state.borrow().selection_keys.clone();
        for position in 0..selection.n_items() {
            let selected = selection
                .item(position)
                .and_downcast::<glib::BoxedAnyObject>()
                .and_then(|object| file_target_from_row(&object.borrow::<ListRow>()))
                .is_some_and(|target| match target {
                    ContextTarget::File(item) | ContextTarget::Folder(item) => {
                        selected_keys.iter().any(|key| key == item.identity_key())
                    }
                    _ => false,
                });
            if selected {
                selection.select_item(position, false);
            }
        }
        let availability_view = self.clone();
        selection.connect_selection_changed(move |model, _, _| {
            let items = (0..model.n_items())
                .filter(|i| model.is_selected(*i))
                .filter_map(|i| model.item(i).and_downcast::<glib::BoxedAnyObject>())
                .filter_map(|object| match &*object.borrow::<ListRow>() {
                    ListRow::File(item) => Some(item.clone()),
                    _ => None,
                })
                .collect();
            availability_view.select_items(items);
        });
        let view = ColumnView::new(Some(selection.clone()));
        view.update_property(&[
            gtk::accessible::Property::Label("Files"),
            gtk::accessible::Property::MultiSelectable(true),
        ]);
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
            let layout_view = view.downgrade();
            column.connect_fixed_width_notify(move |_| {
                if let Some(view) = layout_view.upgrade() {
                    publish_column_layout(&view, &options_state, &changed);
                }
            });
            view.append_column(&column);
        }
        if let Some(sorted) = &sorted {
            sorted.set_sorter(view.sorter().as_ref());
        }

        let options_state = self.options.clone();
        let changed = self.on_options_changed.clone();
        let layout_view = view.downgrade();
        view.columns().connect_items_changed(move |_, _, _, _| {
            if let Some(view) = layout_view.upgrade() {
                publish_column_layout(&view, &options_state, &changed);
            }
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
            } else {
                let file_view = file_view.clone();
                glib::idle_add_local_once(move || file_view.ensure_sort_metadata());
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
        fn find(widget: &gtk::Widget) -> Option<ColumnView> {
            if let Some(view) = widget.downcast_ref::<ColumnView>() {
                return Some(view.clone());
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                if let Some(view) = find(&next) {
                    return Some(view);
                }
                child = next.next_sibling();
            }
            None
        }
        find(self.widget.upcast_ref())
    }

    fn paged_selection(
        &self,
        items: &[FileItem],
        options: &ViewOptions,
        incremental_sort: bool,
        primary_stream_sort: bool,
    ) -> MultiSelection {
        use crate::application::performance::PagedSnapshot;
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        *self.model_store.borrow_mut() = Some(store.clone());
        self.plain_list.set(false);
        self.model_row_index.borrow_mut().clear();
        let mut pages = PagedSnapshot::new(items.to_vec());
        if let Some(page) = pages.next_page() {
            self.model_row_index.borrow_mut().extend(append_model_page(
                &store,
                page,
                options.group_field,
                options.show_hidden,
            ));
        }
        let weak = store.downgrade();
        let group_field = options.group_field;
        let show_hidden = options.show_hidden;
        let row_index = self.model_row_index.clone();
        let filter_options = options.clone();
        let options = options.clone();
        let filter = CustomFilter::new(move |object| {
            let object = object.downcast_ref::<glib::BoxedAnyObject>().unwrap();
            match &*object.borrow::<ListRow>() {
                ListRow::Group(_) => true,
                ListRow::File(item) => filter_options.show_hidden || !item.name().starts_with('.'),
            }
        });
        let sorter = CustomSorter::new(move |a, b| {
            let a = a
                .downcast_ref::<glib::BoxedAnyObject>()
                .unwrap()
                .borrow::<ListRow>();
            let b = b
                .downcast_ref::<glib::BoxedAnyObject>()
                .unwrap()
                .borrow::<ListRow>();
            let group = if options.group_field == GroupField::None {
                std::cmp::Ordering::Equal
            } else {
                let now = std::time::SystemTime::now();
                let label = |row: &ListRow| match row {
                    ListRow::Group(group) => Some(group.clone()),
                    ListRow::File(item) => {
                        crate::core::directory_model::group_label(item, options.group_field, now)
                    }
                };
                label(&a).cmp(&label(&b))
            };
            if group != std::cmp::Ordering::Equal {
                return group.into();
            }
            let (a, b) = match (&*a, &*b) {
                (ListRow::Group(_), ListRow::Group(_)) => return gtk::Ordering::Equal,
                (ListRow::Group(_), _) => return gtk::Ordering::Smaller,
                (_, ListRow::Group(_)) => return gtk::Ordering::Larger,
                (ListRow::File(a), ListRow::File(b)) => (a, b),
            };
            let folders = if options.folders_first {
                b.is_dir().cmp(&a.is_dir())
            } else {
                std::cmp::Ordering::Equal
            };
            let order = match options.sort_field {
                SortField::Name => a.name().to_lowercase().cmp(&b.name().to_lowercase()),
                SortField::Size => a.size().cmp(&b.size()),
                SortField::Type => a.content_type().cmp(b.content_type()),
                SortField::DateModified => a.modified().cmp(&b.modified()),
            }
            .then_with(|| a.name().cmp(b.name()));
            let order = if options.sort_direction == SortDirection::Descending {
                order.reverse()
            } else {
                order
            };
            folders.then(order).into()
        });
        let filtered = FilterListModel::new(Some(store), Some(filter));
        let sorted = SortListModel::new(Some(filtered), Some(sorter));
        sorted.set_incremental(incremental_sort);
        if primary_stream_sort {
            *self.stream_append_model.borrow_mut() = Some(sorted.clone());
        }
        let selection = MultiSelection::new(Some(sorted.clone()));
        let populating = Rc::new(std::cell::Cell::new(false));
        let population = populating.clone();
        let weak_selection = selection.downgrade();
        let keys = Rc::new(RefCell::new(
            self.navigation
                .state
                .borrow()
                .selection_keys
                .iter()
                .cloned()
                .collect::<std::collections::HashSet<_>>(),
        ));
        let selection_keys = keys.clone();
        let completion_view = self.clone();
        let completion_population = population.clone();
        let completion_selection = weak_selection.clone();
        let completion_keys = keys.clone();
        glib::idle_add_local_full(glib::Priority::LOW, move || {
            let Some(store) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if let Some(page) = pages.next_page() {
                row_index.borrow_mut().extend(append_model_page(
                    &store,
                    page,
                    group_field,
                    show_hidden,
                ));
                completion_view.flush_visible_metadata();
                return glib::ControlFlow::Continue;
            }
            if sorted.pending() > 0 {
                let population = completion_population.clone();
                let weak_selection = completion_selection.clone();
                let keys = completion_keys.clone();
                let view = completion_view.clone();
                connect_once_when_sort_settled(&sorted, move || {
                    // The one-shot handler has disconnected before this can
                    // change selection, so a metadata-triggered sort cannot
                    // reenter this initial-restoration path.
                    population.set(true);
                    if let Some(selection) = weak_selection.upgrade() {
                        for position in 0..selection.n_items() {
                            if selection.item(position).and_downcast::<glib::BoxedAnyObject>().is_some_and(|object| matches!(&*object.borrow::<ListRow>(), ListRow::File(item) if keys.borrow().contains(item.identity_key()))) {
                                selection.select_item(position, false);
                            }
                        }
                    }
                    population.set(false);
                    view.flush_visible_metadata();
                });
                return glib::ControlFlow::Break;
            }
            population.set(true);
            if let Some(selection) = weak_selection.upgrade() {
                for position in 0..selection.n_items() {
                    if selection.item(position).and_downcast::<glib::BoxedAnyObject>().is_some_and(|object| matches!(&*object.borrow::<ListRow>(), ListRow::File(item) if keys.borrow().contains(item.identity_key()))) {
                        selection.select_item(position, false);
                    }
                }
            }
            population.set(false);
            glib::ControlFlow::Break
        });
        let view = self.clone();
        selection.connect_selection_changed(move |model, _, _| {
            if populating.get() {
                return;
            }
            let selected = model.selection();
            let items = gtk::BitsetIter::init_first(&selected)
                .map(|(iter, first)| {
                    std::iter::once(first)
                        .chain(iter)
                        .filter_map(|i| model.item(i).and_downcast::<glib::BoxedAnyObject>())
                        .filter_map(|o| match &*o.borrow::<ListRow>() {
                            ListRow::File(item) => Some(item.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            view.select_items(items);
            *selection_keys.borrow_mut() = view
                .navigation
                .state
                .borrow()
                .selection_keys
                .iter()
                .cloned()
                .collect();
        });
        selection
    }

    fn use_recycled_model(&self, count: usize) -> bool {
        count >= crate::application::performance::PAGE_SIZE
            || self
                .navigation
                .state
                .borrow()
                .directory
                .as_ref()
                .is_some_and(|snapshot| snapshot.next_page.is_some())
    }

    fn incremental_sort_model(&self) -> Option<SortListModel> {
        self.stream_append_model.borrow().clone()
    }

    fn pending_incremental_sort_model(&self) -> Option<SortListModel> {
        self.incremental_sort_model()
            .filter(|model| model.is_incremental() && model.pending() > 0)
    }

    fn contains_incremental_sort_model(&self, expected: &SortListModel) -> bool {
        self.incremental_sort_model()
            .is_some_and(|model| model == *expected)
    }

    /// `rendered_count` tracks directory snapshot consumption for stream append;
    /// the URI indexes are the authoritative count of rows actually backed by
    /// a ListStore and therefore eligible for metadata replacement.
    fn has_unrendered_backing_rows(&self) -> bool {
        let state = self.navigation.state.borrow();
        self.rendered_generation.get() == Some(state.navigation_generation)
            && state.directory.as_ref().is_some_and(|snapshot| {
                snapshot.entries.len()
                    > self.model_row_index.borrow().len() + self.folder_row_index.borrow().len()
            })
    }

    fn stream_append_is_current(&self, generation: u64, model: &SortListModel) -> bool {
        let state = self.navigation.state.borrow();
        if self.rendered_generation.get() != Some(generation)
            || state.navigation_generation != generation
            || matches!(state.status, NavigationStatus::Closed)
        {
            return false;
        }
        drop(state);
        self.contains_incremental_sort_model(model)
    }

    fn cancel_stream_append_waiter(&self) {
        if let Some(waiter) = self.stream_append_waiter.borrow_mut().take() {
            waiter.model.disconnect(waiter.handler);
        }
    }

    fn wait_for_incremental_sort(&self, generation: u64, model: &SortListModel) {
        if self
            .stream_append_waiter
            .borrow()
            .as_ref()
            .is_some_and(|waiter| waiter.generation == generation && waiter.model == *model)
        {
            return;
        }
        self.cancel_stream_append_waiter();
        let view = self.clone();
        let handler = model.connect_pending_notify(move |model| {
            if model.pending() > 0 || !view.stream_append_is_current(generation, model) {
                return;
            }
            view.cancel_stream_append_waiter();
            view.resume_directory_page_append(generation, model);
        });
        *self.stream_append_waiter.borrow_mut() = Some(StreamAppendWaiter {
            generation,
            model: model.clone(),
            handler,
        });
    }

    fn resume_directory_page_append(&self, generation: u64, model: &SortListModel) {
        let view = self.clone();
        let model = model.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1), move || {
            if view.stream_append_is_current(generation, &model) {
                view.navigation_finished_after_sort_completion();
            }
        });
    }

    fn navigation_finished_after_sort_completion(&self) {
        if self.append_directory_page() {
            self.complete_directory_stream();
        }
    }

    fn complete_directory_stream(&self) {
        if self.has_pending_navigation() || self.has_unrendered_backing_rows() {
            return;
        }
        if let Some(model) = self.pending_incremental_sort_model() {
            let generation = self.navigation.state.borrow().navigation_generation;
            self.wait_for_incremental_sort(generation, &model);
            return;
        }
        if let Some(keys) = self.stream_selection.borrow_mut().take() {
            self.restore_stream_selection(keys);
        }
        self.metadata_target.flush();
        self.flush_visible_metadata();
        if self.monitor_refresh_pending.get() {
            // Changes arrived while this listing was streaming; one follow-up
            // refresh, not one per change.
            self.schedule_monitor_refresh();
        }
    }

    fn schedule_directory_page_append(&self, generation: u64, model: &SortListModel) {
        let view = self.clone();
        let model = model.clone();
        glib::idle_add_local_once(move || {
            if !view.stream_append_is_current(generation, &model) {
                return;
            }
            if let Some(pending) = view.pending_incremental_sort_model() {
                view.wait_for_incremental_sort(generation, &pending);
                return;
            }
            view.navigation_finished();
        });
    }

    fn append_directory_page(&self) -> bool {
        let state = self.navigation.state.borrow();
        if self.rendered_generation.get() != Some(state.navigation_generation)
            || !matches!(state.content, TabContent::Directory(_))
            || !matches!(state.status, NavigationStatus::Ready)
        {
            return false;
        }
        let Some(snapshot) = &state.directory else {
            return false;
        };
        let store = self.model_store.borrow().clone();
        let Some(store) = store else {
            return false;
        };
        let generation = state.navigation_generation;
        if let Some(model) = self.pending_incremental_sort_model() {
            drop(state);
            self.wait_for_incremental_sort(generation, &model);
            return true;
        }
        let options = self.options();
        let start = self.rendered_count.get();
        let end =
            (start + crate::application::performance::MODEL_BATCH_SIZE).min(snapshot.entries.len());
        let additions: Vec<_> = snapshot.entries[start..end]
            .iter()
            .map(FileItem::from_snapshot)
            .collect();
        self.rendered_count.set(end);
        let more_entries = end < snapshot.entries.len();
        drop(state);
        let folders = self.folder_store.borrow().clone();
        let (folders_to_add, files_to_add): (Vec<_>, Vec<_>) = additions
            .into_iter()
            .partition(|item| folders.is_some() && item.is_dir());
        self.model_row_index.borrow_mut().extend(append_model_page(
            &store,
            files_to_add,
            options.group_field,
            options.show_hidden,
        ));
        if let Some(folders) = folders {
            self.folder_row_index.borrow_mut().extend(append_model_page(
                &folders,
                folders_to_add,
                options.group_field,
                options.show_hidden,
            ));
        }
        if more_entries {
            if let Some(model) = self.incremental_sort_model() {
                self.schedule_directory_page_append(generation, &model);
            }
        }
        true
    }

    fn recycled_grid(
        &self,
        items: &[FileItem],
        options: &ViewOptions,
        incremental_sort: bool,
        primary_stream_sort: bool,
    ) -> gtk::GridView {
        let selection = self.paged_selection(items, options, incremental_sort, primary_stream_sort);
        let grid = gtk::GridView::new(
            Some(selection.clone()),
            Some(column_factory_layout(
                ListColumn::Name,
                options.thumbnails,
                self.clone(),
                true,
            )),
        );
        grid.set_single_click_activate(false);
        grid.set_min_columns(1);
        grid.set_max_columns(12);
        grid.update_property(&[
            gtk::accessible::Property::Label("Files"),
            gtk::accessible::Property::MultiSelectable(true),
        ]);
        let view = self.clone();
        grid.connect_activate(move |_, position| {
            if let Some(object) = selection
                .item(position)
                .and_downcast::<glib::BoxedAnyObject>()
            {
                if let ListRow::File(item) = &*object.borrow::<ListRow>() {
                    activate_item(&view, item.clone());
                }
            }
        });
        grid
    }

    fn large_column_view(
        &self,
        items: &[FileItem],
        options: &ViewOptions,
        incremental_sort: bool,
        primary_stream_sort: bool,
    ) -> ColumnView {
        let selection = self.paged_selection(items, options, incremental_sort, primary_stream_sort);
        let column_view = ColumnView::new(Some(selection.clone()));
        column_view.set_reorderable(true);
        column_view.set_show_row_separators(true);
        column_view.update_property(&[
            gtk::accessible::Property::Label("Files"),
            gtk::accessible::Property::MultiSelectable(true),
        ]);
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
            let layout_view = column_view.downgrade();
            column.connect_fixed_width_notify(move |_| {
                if let Some(view) = layout_view.upgrade() {
                    publish_column_layout(&view, &options_state, &changed);
                }
            });
            column_view.append_column(&column);
            if sort_field_for_title(column_title(column_id)) == Some(options.sort_field) {
                column_view.sort_by_column(
                    Some(&column),
                    if options.sort_direction == SortDirection::Ascending {
                        gtk::SortType::Ascending
                    } else {
                        gtk::SortType::Descending
                    },
                );
            }
        }
        // Header changes persist the new options and rebuild an incremental
        // model; the selected ordering always comes from the shared options.
        self.connect_column_sorting(&column_view);
        let view = self.clone();
        column_view.connect_activate(move |_, position| {
            if let Some(object) = selection
                .item(position)
                .and_downcast::<glib::BoxedAnyObject>()
            {
                if let ListRow::File(item) = &*object.borrow::<ListRow>() {
                    activate_item(&view, item.clone());
                }
            }
        });
        column_view
    }

    fn connect_column_sorting(&self, column_view: &ColumnView) {
        let view = self.clone();
        let sorter = column_view
            .sorter()
            .and_downcast::<gtk::ColumnViewSorter>()
            .unwrap();
        sorter.connect_changed(move |sorter, _| {
            let Some(column) = sorter.primary_sort_column() else {
                return;
            };
            let Some(field) = column.title().as_deref().and_then(sort_field_for_title) else {
                return;
            };
            let mut options = view.options();
            options.sort_field = field;
            options.sort_direction = if sorter.primary_sort_order() == gtk::SortType::Descending {
                SortDirection::Descending
            } else {
                SortDirection::Ascending
            };
            *view.options.borrow_mut() = options.clone();
            if let Some(changed) = view.on_options_changed.borrow().as_ref() {
                changed(options);
            }
            let view = view.clone();
            glib::idle_add_local_once(move || view.reload());
        });
        let options = self.options.clone();
        let changed = self.on_options_changed.clone();
        let weak = column_view.downgrade();
        column_view
            .columns()
            .connect_items_changed(move |_, _, _, _| {
                if let Some(view) = weak.upgrade() {
                    publish_column_layout(&view, &options, &changed);
                }
            });
    }

    pub fn open_item(&self, item: &FileItem) {
        match crate::core::activation_for(item) {
            crate::core::Activation::Browse(location) => {
                if let Some(callback) = self.on_navigate.borrow().as_ref() {
                    callback(location);
                } else {
                    self.navigate(location);
                }
            }
            crate::core::Activation::LaunchExternally => {
                gio::AppInfo::launch_default_for_uri_async(
                    item.location().uri(),
                    None::<&gio::AppLaunchContext>,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        }
    }

    /// One icon-grid tile. It reaches its grid through its own parents, so
    /// tiles built later by an in-place refresh behave like the originals.
    fn grid_tile_widget(&self, item: &FileItem, options: &ViewOptions) -> GtkBox {
        let layout = options.grid_size.grid_layout();
        let tile = GtkBox::new(Orientation::Vertical, layout.gap / 2);
        tile.add_css_class("icon-grid-tile");
        if self.pending_cut.borrow().contains(item.location()) {
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
        self.request_flowbox_thumbnail_metadata(&icon, item, options.thumbnails);
        tile.append(&icon);
        tile.append(&grid_tile_label(item.name(), layout.label_width_chars));
        attach_drag_source(&tile, item.clone(), self.clone());
        let target = file_target(item);
        let view = self.clone();
        let menu_tile = tile.downgrade();
        let gesture = GestureClick::new();
        gesture.set_button(3);
        gesture.connect_pressed(move |gesture, _, x, y| {
            let Some(menu_tile) = menu_tile.upgrade() else {
                return;
            };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(child) = menu_tile.parent().and_downcast::<FlowBoxChild>() {
                if let Some(grid) = child.parent().and_downcast::<FlowBox>() {
                    if !child.is_selected() {
                        grid.unselect_all();
                        grid.select_child(&child);
                    }
                }
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
        tile
    }

    fn grid_view(&self, items: &[FileItem], options: &ViewOptions) -> (FlowBox, FlowPane) {
        let selected_keys = self.navigation.state.borrow().selection_keys.clone();
        let layout = options.grid_size.grid_layout();
        let grid = FlowBox::new();
        grid.update_property(&[
            gtk::accessible::Property::Label("Files"),
            gtk::accessible::Property::MultiSelectable(true),
        ]);
        grid.set_selection_mode(gtk::SelectionMode::Multiple);
        grid.set_activate_on_single_click(false);
        grid.set_valign(gtk::Align::Start);
        grid.set_column_spacing(layout.gap.try_into().expect("grid gaps are positive"));
        grid.set_row_spacing(layout.gap.try_into().expect("grid gaps are positive"));
        grid.set_margin_top(12);
        grid.set_margin_start(12);
        grid.set_margin_end(12);
        // The closures below index children by position, so they read a live
        // vector that an in-place refresh keeps in step with the grid.
        let grid_items: Rc<RefCell<Vec<FileItem>>> = Rc::new(RefCell::new(items.to_vec()));
        let activation_items = grid_items.clone();
        let activation_view = self.clone();
        grid.connect_child_activated(move |_, child| {
            if let Some(item) = activation_items.borrow().get(child.index() as usize) {
                activate_item(&activation_view, item.clone());
            }
        });
        let grid_selection_items = grid_items.clone();
        let availability_view = self.clone();
        grid.connect_selected_children_changed(move |grid| {
            let items = grid
                .selected_children()
                .iter()
                .filter_map(|child| {
                    grid_selection_items
                        .borrow()
                        .get(child.index() as usize)
                        .cloned()
                })
                .collect();
            availability_view.select_items(items);
        });
        for item in items {
            let tile = self.grid_tile_widget(item, options);
            grid.append(&tile);
            if let Some(child) = tile.parent().and_downcast::<gtk::FlowBoxChild>() {
                child.update_property(&[gtk::accessible::Property::Label(item.name())]);
                if self.pending_cut.borrow().contains(item.location()) {
                    child.update_property(&[gtk::accessible::Property::Description(
                        "Cut; ready to move",
                    )]);
                }
            }
            if selected_keys.iter().any(|key| key == item.identity_key()) {
                if let Some(child) = tile.parent().and_downcast::<gtk::FlowBoxChild>() {
                    grid.select_child(&child);
                }
            }
        }
        // GTK initially focuses a tile when this renderer is mapped and may
        // replace a restored multi-selection with that tile. Restore the URI
        // set after mapping, without changing the user's later selections.
        let map_items = grid_items.clone();
        let map_view = self.clone();
        let first_map = std::cell::Cell::new(true);
        grid.connect_map(move |grid| {
            let keys = if first_map.replace(false) {
                selected_keys.clone()
            } else {
                map_view.navigation.state.borrow().selection_keys.clone()
            };
            if keys.is_empty() {
                return;
            }
            let weak = grid.downgrade();
            let items = map_items.clone();
            glib::idle_add_local_once(move || {
                if let Some(grid) = weak.upgrade().filter(|grid| grid.is_mapped()) {
                    grid.unselect_all();
                    for (index, key) in items.borrow().iter().enumerate() {
                        if keys.iter().any(|stored| stored == key.identity_key()) {
                            if let Some(child) = grid.child_at_index(index as i32) {
                                grid.select_child(&child);
                            }
                        }
                    }
                }
            });
        });
        let pane = FlowPane {
            grid: grid.downgrade(),
            items: grid_items,
            options: options.clone(),
        };
        (grid, pane)
    }

    fn hybrid_view(&self, items: &[FileItem], options: &ViewOptions) -> (GtkBox, RenderedPanes) {
        let root = GtkBox::new(Orientation::Vertical, 8);
        root.set_margin_start(12);
        root.set_margin_top(12);
        let folders: Vec<_> = items.iter().filter(|item| item.is_dir()).cloned().collect();
        let files: Vec<_> = items
            .iter()
            .filter(|item| !item.is_dir())
            .cloned()
            .collect();
        if self.use_recycled_model(items.len()) {
            let folders_view = self.recycled_grid(&folders, options, false, false);
            *self.folder_store.borrow_mut() = self.model_store.borrow_mut().take();
            *self.folder_row_index.borrow_mut() =
                std::mem::take(&mut *self.model_row_index.borrow_mut());
            let folders_scroll = ScrolledWindow::builder()
                .min_content_height(120)
                .max_content_height(240)
                .propagate_natural_height(true)
                .child(&folders_view)
                .build();
            root.append(&Label::new(Some("Folders")));
            root.append(&folders_scroll);
            let files_view = self.large_column_view(&files, options, true, true);
            let files_scroll = ScrolledWindow::builder()
                .vexpand(true)
                .min_content_height(200)
                .child(&files_view)
                .build();
            root.append(&Label::new(Some("Files")));
            root.append(&files_scroll);
            self.link_hybrid_panes(
                folders_view.upcast_ref::<gtk::Widget>(),
                files_view.upcast_ref::<gtk::Widget>(),
            );
            let folders_pane = self.folder_store.borrow().clone().map(|store| {
                Pane::List(ListPane {
                    store,
                    recycled: true,
                })
            });
            let files_pane = self.model_store.borrow().clone().map(|store| {
                Pane::List(ListPane {
                    store,
                    recycled: true,
                })
            });
            // The outer viewport must allocate these two bounded scrollers,
            // rather than measuring an unbounded list's natural height.
            root.set_vexpand(true);
            return (root, RenderedPanes::Hybrid(folders_pane, files_pane));
        }
        let mut folder_pane = None;
        let mut file_pane = None;
        let mut folder_widget = None;
        let mut file_widget = None;
        if !folders.is_empty() {
            let heading = Label::new(Some("Folders"));
            heading.add_css_class("heading");
            heading.set_xalign(0.0);
            root.append(&heading);
            let (grid, pane) = self.grid_view(&folders, options);
            folder_pane = Some(Pane::Flow(pane));
            folder_widget = Some(grid.clone().upcast::<gtk::Widget>());
            root.append(&grid);
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
            let list = self.column_view_for(&files, &file_options);
            file_pane = Some(self.list_pane(self.use_recycled_model(files.len())));
            file_widget = Some(list.clone().upcast::<gtk::Widget>());
            root.append(&list);
        }
        if let (Some(folders), Some(files)) = (folder_widget, file_widget) {
            self.link_hybrid_panes(&folders, &files);
        }
        (root, RenderedPanes::Hybrid(folder_pane, file_pane))
    }
}

fn selection_keys_of(items: &[FileItem]) -> Vec<String> {
    items
        .iter()
        .map(|item| item.identity_key().to_owned())
        .collect()
}

/// Whether a plain click should leave nothing selected.
///
/// `GtkFlowBox` in multiple-selection mode clears every child and then re-selects
/// the one clicked, and a list row selects exclusively, so neither turns a click
/// on an already-selected item into a deselection; and neither clears the
/// selection when the click lands on empty space. Both leave the selection
/// exactly as it was, which is the user asking for no selection at all.
fn click_leaves_selection_unchanged(pressed: &[String], released: &[String]) -> bool {
    if pressed.is_empty() {
        return false;
    }
    fn key_set(selection: &[String]) -> std::collections::HashSet<&str> {
        selection.iter().map(String::as_str).collect()
    }
    key_set(pressed) == key_set(released)
}

fn selection_model_of(widget: &gtk::Widget) -> Option<gtk::SelectionModel> {
    if let Some(view) = widget.downcast_ref::<ColumnView>() {
        return view.model();
    }
    if let Some(view) = widget.downcast_ref::<gtk::GridView>() {
        return view.model();
    }
    if let Some(view) = widget.downcast_ref::<gtk::ListView>() {
        return view.model();
    }
    None
}

/// Drops the selection a pane is showing, descending into a container that is
/// not itself a pane. Each pane reports the empty selection through its own
/// handler, which is what clears the logical selection.
fn clear_pane_selection(widget: &gtk::Widget) {
    if let Some(grid) = widget.downcast_ref::<FlowBox>() {
        grid.unselect_all();
        return;
    }
    if let Some(model) = selection_model_of(widget) {
        model.unselect_all();
        return;
    }
    let mut child = widget.first_child();
    while let Some(next) = child {
        clear_pane_selection(&next);
        child = next.next_sibling();
    }
}

fn connect_pane_selection_changed(widget: &gtk::Widget, callback: impl Fn(usize) + 'static) {
    let callback = Rc::new(callback);
    if let Some(grid) = widget.downcast_ref::<FlowBox>() {
        let selected = callback.clone();
        grid.connect_selected_children_changed(move |grid| {
            selected(grid.selected_children().len());
        });
        return;
    }
    if let Some(model) = selection_model_of(widget) {
        let selected = callback;
        model.connect_selection_changed(move |model, _, _| {
            selected(model.selection().size() as usize);
        });
    }
}

/// Whether a click landed on something the user is operating rather than on
/// empty space: a control, or a column header they are sorting by. Neither
/// should cost them their selection. `GtkColumnViewTitle` is private to GTK, so
/// the header is recognised structurally -- reaching the `ColumnView` without
/// passing through the `ListView` holding its rows means the click was above
/// them.
fn click_is_on_a_control_or_header(root: &ScrolledWindow, x: f64, y: f64) -> bool {
    let Some(picked) = root.pick(x, y, gtk::PickFlags::DEFAULT) else {
        return false;
    };
    let mut widget = Some(picked);
    while let Some(current) = widget {
        if current.downcast_ref::<gtk::Scrollbar>().is_some()
            || current.downcast_ref::<Button>().is_some()
            || current.downcast_ref::<gtk::Entry>().is_some()
            || current.downcast_ref::<gtk::Switch>().is_some()
        {
            return true;
        }
        if current.downcast_ref::<ColumnView>().is_some() {
            return true;
        }
        if current.downcast_ref::<FlowBox>().is_some() || selection_model_of(&current).is_some() {
            return false;
        }
        widget = current.parent();
    }
    false
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
    crate::ui::accessibility::restore_popover_focus(&popover, parent);
    popover.set_autohide(true);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    let menu = GtkBox::new(Orientation::Vertical, 2);
    menu.set_margin_top(6);
    menu.set_margin_bottom(6);
    let availability = view.command_availability();
    for command in item_commands(&target) {
        if command == ContextCommand::OpenWith {
            menu.append(&open_with_submenu(&target, &availability, view));
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

fn open_with_submenu(
    target: &ContextTarget,
    availability: &CommandAvailability,
    view: &FileView,
) -> Button {
    let button = Button::with_label("Open With ›");
    set_command_enabled(&button, availability, ContextCommand::OpenWith);
    let submenu_parent = button.clone();
    let target = target.clone();
    let view = view.clone();
    button.connect_clicked(move |_| {
        let (content_type, location) = match &target {
            ContextTarget::File(item) => (item.effective_content_type(), item.location().clone()),
            _ => return,
        };
        let menu = GtkBox::new(Orientation::Vertical, 2);
        menu.set_margin_top(6);
        menu.set_margin_bottom(6);
        for app in open_with_apps(&content_type) {
            let app_button = Button::with_label(&app.display_name());
            let view = view.clone();
            app_button.connect_clicked(move |_| {
                view.dispatch_open_with(app.clone());
            });
            menu.append(&app_button);
        }
        menu.append(&Separator::new(Orientation::Horizontal));
        let other = Button::with_label("Other Application…");
        let parent = submenu_parent.clone();
        let view = view.clone();
        other.connect_clicked(move |_| {
            show_application_chooser(&parent, content_type.clone(), &location, view.clone())
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

/// Inside a Flatpak sandbox GIO sees only the sandbox's own applications, so a
/// chooser built from it lists almost nothing. The desktop portal shows the
/// host's chooser instead.
fn running_in_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

#[allow(deprecated)]
fn show_application_chooser(
    parent: &Button,
    content_type: String,
    location: &Location,
    view: FileView,
) {
    let window = parent.root().and_downcast::<gtk::Window>();
    if running_in_flatpak() {
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_uri(location.uri())));
        launcher.set_always_ask(true);
        launcher.launch(window.as_ref(), gio::Cancellable::NONE, |result| {
            if let Err(error) = result {
                if !error.matches(gtk::DialogError::Dismissed) {
                    eprintln!("Open With failed: {error}");
                }
            }
        });
        return;
    }
    let dialog = gtk::AppChooserDialog::for_content_type(
        window.as_ref(),
        gtk::DialogFlags::MODAL,
        &content_type,
    );
    if let Some(chooser) = dialog.widget().downcast_ref::<gtk::AppChooserWidget>() {
        chooser.set_show_other(true);
    }
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Ok {
            if let Some(app) = dialog.app_info() {
                view.dispatch_open_with(app);
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
        ContextCommand::Extract => "Extract",
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
    column_factory_layout(column, thumbnails, view, false)
}

fn column_factory_layout(
    column: ListColumn,
    thumbnails: bool,
    view: FileView,
    grid_cell: bool,
) -> SignalListItemFactory {
    let factory = SignalListItemFactory::new();
    let bind_view = view.clone();
    let grid_options = view.options();
    factory.connect_setup(move |_, list_item| {
        let list_item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("column factories receive list items");
        if column == ListColumn::Name {
            let line = GtkBox::new(Orientation::Horizontal, 8);
            if grid_cell {
                line.set_orientation(Orientation::Vertical);
                line.set_size_request(grid_options.grid_size.grid_layout().tile_width, -1);
                line.add_css_class("icon-grid-tile");
            }
            line.set_margin_start(10);
            line.set_margin_end(10);
            line.set_margin_top(6);
            line.set_margin_bottom(6);
            let icon = Image::new();
            icon.set_pixel_size(20);
            let label = Label::new(None);
            if grid_cell {
                label.set_wrap(true);
                label.set_lines(2);
                label.set_max_width_chars(grid_options.grid_size.grid_layout().label_width_chars);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                icon.set_pixel_size(grid_options.grid_size.icon_size());
            }
            label.set_xalign(0.0);
            line.append(&icon);
            line.append(&label);
            let context_item = list_item.downgrade();
            let context_view = view.clone();
            let gesture = GestureClick::new();
            gesture.set_button(3);
            gesture.connect_pressed(move |gesture, _, x, y| {
                let Some(context_item) = context_item.upgrade() else {
                    return;
                };
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
                context_view.select_context_item(item);
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
            let touch_item = list_item.downgrade();
            let drop_item = list_item.downgrade();
            attach_folder_drop(&line, view.clone(), move || {
                let object = drop_item
                    .upgrade()?
                    .item()
                    .and_downcast::<glib::BoxedAnyObject>()?;
                let row = object.borrow::<ListRow>();
                match &*row {
                    ListRow::File(item) => Some(item.clone()),
                    _ => None,
                }
            });
            attach_touch_menu(&line, view.clone(), move || {
                let object = touch_item
                    .upgrade()?
                    .item()
                    .and_downcast::<glib::BoxedAnyObject>()?;
                let row = object.borrow::<ListRow>();
                match &*row {
                    ListRow::File(item) => Some(item.clone()),
                    _ => None,
                }
            });
            let drag_item = list_item.downgrade();
            let drag_view = view.clone();
            let drag_source = gtk::DragSource::new();
            drag_source.set_actions(gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE);
            drag_source.connect_prepare(move |source, _, _| {
                let object = drag_item
                    .upgrade()?
                    .item()
                    .and_downcast::<glib::BoxedAnyObject>()?;
                let row = object.borrow::<ListRow>();
                let ListRow::File(item) = &*row else {
                    return None;
                };
                drag_view.select_context_item(item);
                source.set_actions(drag_actions(&drag_view));
                drag_selection_provider(&drag_view)
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
        if let ListRow::File(item) = &*row {
            // GTK binds only live/recycled viewport cells, so this is the
            // production visible-range boundary for advisory metadata work.
            bind_view.request_visible_metadata(item);
        }
        list_item.set_selectable(matches!(&*row, ListRow::File(_)));
        if matches!(&*row, ListRow::File(item) if bind_view.pending_cut.borrow().contains(item.location())) {
            child.add_css_class("cut-pending");
            child.update_property(&[gtk::accessible::Property::Description("Cut; ready to move")]);
        } else {
            // GTK reuses cells for different files and group headings. Clear
            // both projections so a previous cut row cannot label the next.
            child.remove_css_class("cut-pending");
            child.reset_property(gtk::AccessibleProperty::Description);
        }
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
                // Rebinding destroys the old image, so an asynchronous decode
                // cannot paint a former file's thumbnail onto a reused cell.
                let size = icon.pixel_size();
                line.remove(&icon);
                let icon = item_image(item, thumbnails);
                icon.set_pixel_size(size);
                line.prepend(&icon);
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
    factory.connect_unbind(|_, list_item| {
        if let Some(child) = list_item
            .downcast_ref::<gtk::ListItem>()
            .and_then(|item| item.child())
        {
            child.remove_css_class("cut-pending");
            child.reset_property(gtk::AccessibleProperty::Description);
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
    use gtk::ColumnView;

    use super::{
        cached_thumbnail_file_for, click_is_on_a_control_or_header,
        click_leaves_selection_unchanged, command_availability_for_target, drag_uri_for,
        grid_tile_label, local_image_thumbnail_file, thumbnail_cache_homes_from,
        thumbnail_cache_paths, uses_native_row_sorting, FileView,
    };
    use crate::core::navigation::{TabContent, TabState};
    use crate::core::sidebar_model::SidebarSection;
    use crate::core::{
        FileItem, GroupField, Location, SortDirection, SortField, ViewMode, ViewOptions,
    };
    use crate::ui::context_menu::{CommandAvailability, ContextCommand, ContextTarget};

    #[gtk::test]
    fn slice4_reused_list_cells_clear_cut_descriptions_for_files_and_groups() {
        use super::{column_factory, ListRow};
        use crate::core::ListColumn;
        use gtk::prelude::*;
        let directory = tempfile::tempdir().unwrap();
        let cut_path = directory.path().join("cut.txt");
        let plain_path = directory.path().join("plain.txt");
        fs::write(&cut_path, "cut").unwrap();
        fs::write(&plain_path, "plain").unwrap();
        let cut = FileItem::from_path(&cut_path).unwrap();
        let plain = FileItem::from_path(&plain_path).unwrap();
        let view = FileView::new(Location::from_local_path(directory.path()));
        view.set_pending_cut_locations(vec![cut.location().clone()]);
        for column in [
            ListColumn::Name,
            ListColumn::Type,
            ListColumn::Size,
            ListColumn::DateModified,
        ] {
            let row = glib::BoxedAnyObject::new(ListRow::File(cut.clone()));
            let store = gio::ListStore::new::<glib::BoxedAnyObject>();
            store.append(&row);
            let factory = column_factory(column, false, view.clone());
            let bound = std::rc::Rc::new(std::cell::RefCell::new(None));
            let capture = bound.clone();
            factory.connect_bind(move |_, item| {
                *capture.borrow_mut() = Some(item.downcast_ref::<gtk::ListItem>().unwrap().clone());
            });
            let list = gtk::ListView::new(
                Some(gtk::NoSelection::new(Some(store))),
                Some(factory.clone()),
            );
            let window = gtk::Window::new();
            window.set_child(Some(&list));
            window.present();
            let context = glib::MainContext::default();
            let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while bound.borrow().is_none() {
                context.iteration(false);
                assert!(std::time::Instant::now() < end);
            }
            let item = bound.borrow().clone().unwrap();
            let cell = item.child().unwrap();
            assert!(gtk::test_accessible_has_property(
                &cell,
                gtk::AccessibleProperty::Description
            ));
            // Drive the production bind handler with the same native ListItem
            // and cell. Model splicing alone may allocate new cells instead of
            // reusing them on a particular GTK version, hiding stale state.
            for replacement in [
                ListRow::File(plain.clone()),
                ListRow::Group("Documents".into()),
            ] {
                row.replace(replacement);
                factory.emit_by_name::<()>("bind", &[&item]);
                assert_eq!(item.child().as_ref(), Some(&cell));
                assert!(!gtk::test_accessible_has_property(
                    &cell,
                    gtk::AccessibleProperty::Description
                ));
                assert!(!cell.has_css_class("cut-pending"));
                row.replace(ListRow::File(cut.clone()));
                factory.emit_by_name::<()>("bind", &[&item]);
                assert!(gtk::test_accessible_has_property(
                    &cell,
                    gtk::AccessibleProperty::Description
                ));
            }
            window.close();
        }
        view.close();
    }

    #[gtk::test]
    fn slice5_rebound_virtual_cells_request_each_new_uri_once() {
        use super::{column_factory, ListRow};
        use crate::core::contracts::{Capability, CapabilitySnapshot, EntryKind, EntrySnapshot};
        use crate::core::ListColumn;
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};
        use gtk::prelude::*;

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/share").unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        let snapshot = |name: &str| EntrySnapshot {
            location: Location::from_uri(&format!("{}/{}", root.uri(), name)).unwrap(),
            display_name: name.into(),
            kind: EntryKind::File,
            size: Some(7),
            modified: None,
            content_type: Some("text/plain".into()),
            capabilities: CapabilitySnapshot {
                supported: [Capability::List].into_iter().collect(),
                ..Default::default()
            },
        };
        let first = FileItem::from_snapshot(&snapshot("first.txt"));
        let second = FileItem::from_snapshot(&snapshot("second.txt"));
        let view = FileView::new_with_repository(root.clone(), provider.clone());
        let row = glib::BoxedAnyObject::new(ListRow::File(first.clone()));
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        store.append(&row);
        let factory = column_factory(ListColumn::Name, false, view.clone());
        let bound = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = bound.clone();
        factory.connect_bind(move |_, item| {
            *capture.borrow_mut() = Some(item.downcast_ref::<gtk::ListItem>().unwrap().clone());
        });
        let list = gtk::ListView::new(
            Some(gtk::NoSelection::new(Some(store))),
            Some(factory.clone()),
        );
        let window = gtk::Window::new();
        window.set_child(Some(&list));
        window.present();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while bound.borrow().is_none() {
            context.iteration(false);
            assert!(std::time::Instant::now() < deadline);
        }
        let bound = bound.borrow().clone().unwrap();
        assert_eq!(
            provider.visible_metadata_requests(),
            vec![first.location().clone()]
        );

        row.replace(ListRow::File(second.clone()));
        factory.emit_by_name::<()>("bind", &[&bound]);
        row.replace(ListRow::File(first.clone()));
        factory.emit_by_name::<()>("bind", &[&bound]);
        let requests = provider.visible_metadata_requests();
        assert_eq!(
            requests.len(),
            2,
            "rebinding a cached URI must not repeat I/O"
        );
        assert!(requests.contains(first.location()));
        assert!(requests.contains(second.location()));
        window.close();
        view.close();
    }

    #[gtk::test]
    fn slice5_visible_metadata_updates_file_and_folder_backing_rows() {
        use super::ListRow;
        use crate::core::contracts::VisibleMetadata;
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};
        use glib::prelude::Cast;

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/share").unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context, MockConfig::default()));
        let view = FileView::new_with_repository(root, provider);
        let directory = tempfile::tempdir().unwrap();
        let folder_path = directory.path().join("folder");
        let file_path = directory.path().join("file.txt");
        std::fs::create_dir(&folder_path).unwrap();
        std::fs::write(&file_path, b"fixture").unwrap();
        let folder = FileItem::from_path(&folder_path).unwrap();
        let file = FileItem::from_path(&file_path).unwrap();
        let folders = gio::ListStore::new::<glib::BoxedAnyObject>();
        let files = gio::ListStore::new::<glib::BoxedAnyObject>();
        folders.append(&glib::BoxedAnyObject::new(ListRow::File(folder.clone())));
        files.append(&glib::BoxedAnyObject::new(ListRow::File(file.clone())));
        *view.folder_store.borrow_mut() = Some(folders.clone());
        *view.model_store.borrow_mut() = Some(files.clone());
        view.folder_row_index
            .borrow_mut()
            .insert(folder.location().clone(), 0);
        view.model_row_index
            .borrow_mut()
            .insert(file.location().clone(), 0);

        view.apply_visible_metadata(vec![
            VisibleMetadata {
                location: folder.location().clone(),
                size: Some(111),
                modified: None,
                content_type: None,
            },
            VisibleMetadata {
                location: file.location().clone(),
                size: Some(222),
                modified: None,
                content_type: None,
            },
        ]);

        let size_at = |store: &gio::ListStore| {
            let object = store
                .item(0)
                .unwrap()
                .downcast::<glib::BoxedAnyObject>()
                .unwrap();
            let ListRow::File(item) = &*object.borrow::<ListRow>() else {
                panic!("fixture row must be a file");
            };
            item.size()
        };
        assert_eq!(
            size_at(&folders),
            111,
            "folder cells receive visible metadata"
        );
        assert_eq!(size_at(&files), 222, "file cells receive visible metadata");
        view.close();
    }

    #[gtk::test]
    fn slice5_visible_metadata_updates_indexed_rows_before_later_virtual_pages() {
        use super::ListRow;
        use crate::application::performance::CachePolicy;
        use crate::core::contracts::{
            Capability, CapabilitySnapshot, DirectorySnapshot, EntryKind, EntrySnapshot,
            VisibleMetadata,
        };
        use crate::core::navigation::{NavigationStatus, TabContent};
        use crate::core::ResolvedLocation;
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};
        use glib::prelude::Cast;

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/metadata-stream").unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context, MockConfig::default()));
        let view = FileView::new_with_repository(root.clone(), provider);
        let entries: Vec<_> = (0..=CachePolicy::default().max_entries)
            .map(|index| EntrySnapshot {
                location: Location::from_uri(&format!("{}/item-{index:03}", root.uri())).unwrap(),
                display_name: format!("item-{index:03}"),
                kind: EntryKind::File,
                size: None,
                modified: None,
                content_type: None,
                capabilities: CapabilitySnapshot {
                    supported: [Capability::List].into_iter().collect(),
                    ..Default::default()
                },
            })
            .collect();
        let first = FileItem::from_snapshot(&entries[0]);
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        store.append(&glib::BoxedAnyObject::new(ListRow::File(first.clone())));
        *view.model_store.borrow_mut() = Some(store.clone());
        view.model_row_index
            .borrow_mut()
            .insert(first.location().clone(), 0);
        {
            let mut state = view.navigation.state.borrow_mut();
            let generation = state.navigation_generation;
            state.content = TabContent::Directory(root.clone());
            state.status = NavigationStatus::Ready;
            state.directory = Some(DirectorySnapshot {
                location: ResolvedLocation {
                    requested: root.clone(),
                    resolved: root.clone(),
                },
                entries: entries.clone(),
                capabilities: CapabilitySnapshot {
                    supported: [Capability::List].into_iter().collect(),
                    ..Default::default()
                },
                next_page: Some(crate::core::contracts::PageRequest {
                    offset: 1,
                    limit: 1,
                }),
            });
            view.rendered_generation.set(Some(generation));
            view.rendered_count.set(1);
        }
        assert!(view.has_pending_navigation());
        assert!(view.has_unrendered_backing_rows());

        view.apply_visible_metadata(
            entries
                .iter()
                .enumerate()
                .map(|(index, entry)| VisibleMetadata {
                    location: entry.location.clone(),
                    size: Some(index as u64),
                    modified: None,
                    content_type: Some("application/x-streamed".into()),
                })
                .collect(),
        );

        let object = store
            .item(0)
            .unwrap()
            .downcast::<glib::BoxedAnyObject>()
            .unwrap();
        let ListRow::File(item) = &*object.borrow::<ListRow>() else {
            panic!("fixture row must be a file");
        };
        assert_eq!(item.content_type(), "application/x-streamed");
        assert!(
            view.pending_visible_metadata.borrow().len() <= CachePolicy::default().max_entries,
            "metadata for later virtual pages must remain bounded"
        );
        view.close();
    }

    #[gtk::test]
    fn slice5_final_stream_append_drains_second_metadata_completion_after_next_sort_settle() {
        use super::{append_model_page, ListRow};
        use crate::core::contracts::{
            Capability, CapabilitySnapshot, DirectorySnapshot, EntryKind, EntrySnapshot,
            SearchCompletion, SearchMode, SearchOutcome, SearchRequest, VisibleMetadata,
        };
        use crate::core::navigation::{NavigationStatus, TabContent};
        use crate::core::ports::{SearchCallback, SearchRepository};
        use crate::core::ResolvedLocation;
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};
        use gio::prelude::CancellableExt;
        use glib::prelude::Cast;
        use std::cell::RefCell;
        use std::rc::Rc;

        #[derive(Default)]
        struct SearchDouble(RefCell<Vec<(SearchRequest, SearchCallback)>>);

        impl SearchRepository for SearchDouble {
            fn search(&self, request: SearchRequest, done: SearchCallback) {
                self.0.borrow_mut().push((request, done));
            }
        }

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/final-sort-metadata").unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        let search = Rc::new(SearchDouble::default());
        let view = FileView::new_with_repositories(root.clone(), provider.clone(), search.clone());
        provider.advance(1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while view.has_pending_navigation() {
            assert!(std::time::Instant::now() < deadline);
            context.iteration(false);
        }
        let entries: Vec<_> = (0..2_048)
            .map(|index| EntrySnapshot {
                location: Location::from_uri(&format!("{}/{index:05}", root.uri())).unwrap(),
                display_name: format!("{index:05}"),
                kind: EntryKind::File,
                size: None,
                modified: None,
                content_type: None,
                capabilities: CapabilitySnapshot {
                    supported: [Capability::List].into_iter().collect(),
                    ..Default::default()
                },
            })
            .collect();
        let target = entries.last().unwrap().location.clone();
        let second_target = entries[entries.len() - 2].location.clone();
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let sorter = gtk::CustomSorter::new(|left, right| {
            let left = left
                .downcast_ref::<glib::BoxedAnyObject>()
                .unwrap()
                .borrow::<ListRow>();
            let right = right
                .downcast_ref::<glib::BoxedAnyObject>()
                .unwrap()
                .borrow::<ListRow>();
            let (ListRow::File(left), ListRow::File(right)) = (&*left, &*right) else {
                return gtk::Ordering::Equal;
            };
            left.name().cmp(right.name()).into()
        });
        let sorted = gtk::SortListModel::new(Some(store.clone()), Some(sorter));
        sorted.set_incremental(true);
        *view.model_store.borrow_mut() = Some(store.clone());
        view.model_row_index.borrow_mut().extend(append_model_page(
            &store,
            entries[..crate::application::performance::MODEL_BATCH_SIZE]
                .iter()
                .map(FileItem::from_snapshot)
                .collect(),
            GroupField::None,
            true,
        ));
        *view.stream_append_model.borrow_mut() = Some(sorted.clone());
        {
            let mut state = view.navigation.state.borrow_mut();
            let generation = state.navigation_generation;
            state.content = TabContent::Directory(root.clone());
            state.status = NavigationStatus::Ready;
            state.directory = Some(DirectorySnapshot {
                location: ResolvedLocation {
                    requested: root.clone(),
                    resolved: root.clone(),
                },
                entries,
                capabilities: CapabilitySnapshot {
                    supported: [Capability::List].into_iter().collect(),
                    ..Default::default()
                },
                next_page: None,
            });
            view.rendered_generation.set(Some(generation));
            view.rendered_count
                .set(crate::application::performance::MODEL_BATCH_SIZE);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while sorted.pending() > 0 {
            assert!(std::time::Instant::now() < deadline);
            context.iteration(false);
        }
        view.apply_visible_metadata(vec![VisibleMetadata {
            location: target.clone(),
            size: Some(2_048),
            modified: None,
            content_type: Some("application/x-final-sort".into()),
        }]);
        assert_eq!(view.pending_visible_metadata.borrow().len(), 1);

        view.navigation_finished();

        assert!(
            sorted.pending() > 0,
            "the final append must make the incremental sorter pending"
        );
        assert!(
            !view.has_pending_navigation(),
            "the final append must run after backend navigation completes"
        );
        assert_eq!(view.model_row_index.borrow().len(), 2_048);
        assert!(
            !view.has_unrendered_backing_rows(),
            "the final append must back every directory row"
        );
        assert_eq!(
            view.pending_visible_metadata.borrow().len(),
            1,
            "final-page metadata must wait for the post-append sorter-settled notification"
        );
        assert!(
            view.stream_append_waiter.borrow().is_some(),
            "the final append must subscribe to the sorter-settled notification"
        );
        let pending_object = store
            .item(2_047)
            .unwrap()
            .downcast::<glib::BoxedAnyObject>()
            .unwrap();
        let ListRow::File(pending_item) = &*pending_object.borrow::<ListRow>() else {
            panic!("final backing row must be a file");
        };
        assert_eq!(pending_item.content_type(), "application/octet-stream");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let first_object = store
                .item(2_047)
                .unwrap()
                .downcast::<glib::BoxedAnyObject>()
                .unwrap();
            let ListRow::File(first_item) = &*first_object.borrow::<ListRow>() else {
                panic!("first metadata backing row must remain a file");
            };
            if sorted.pending() > 0 && first_item.content_type() == "application/x-final-sort" {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            context.iteration(false);
        }
        view.metadata_target.apply(vec![VisibleMetadata {
            location: second_target.clone(),
            size: Some(2_047),
            modified: None,
            content_type: Some("application/x-second-final-sort".into()),
        }]);
        assert_eq!(
            view.pending_visible_metadata.borrow().len(),
            1,
            "a completion during the post-final-flush sort must remain deferred"
        );
        assert!(
            !view.tab_state().borrow().history.can_go_back()
                && !view.tab_state().borrow().history.can_go_forward(),
            "the initial location has neither history direction available"
        );
        assert!(
            view.metadata_target.metadata_sort_waiter.borrow().is_some(),
            "the deferred metadata completion must retain its sort waiter"
        );
        assert!(view.search("report", SearchMode::Filename, false));
        let (request, done) = search.0.borrow_mut().remove(0);
        view.go_back();
        assert!(
            request.cancellable.is_cancelled(),
            "back at the history boundary must still clear an active search"
        );
        assert!(
            view.metadata_target.metadata_sort_waiter.borrow().is_some(),
            "clearing search at the history boundary must not cancel the metadata waiter"
        );
        done(SearchCompletion {
            id: request.id,
            outcome: SearchOutcome::Completed {
                matches: vec![],
                declared_scope: request.scope,
                scope_limited: false,
                limits: vec![],
            },
        });
        assert!(
            !view.is_showing_search_results(),
            "a cleared search callback must not restore its results"
        );
        view.go_forward();

        while sorted.pending() > 0 || view.pending_visible_metadata.borrow().len() > 0 {
            assert!(std::time::Instant::now() < deadline);
            context.iteration(false);
        }
        let settled_object = store
            .item(2_047)
            .unwrap()
            .downcast::<glib::BoxedAnyObject>()
            .unwrap();
        let ListRow::File(settled_item) = &*settled_object.borrow::<ListRow>() else {
            panic!("final backing row must remain a file");
        };
        assert_eq!(settled_item.content_type(), "application/x-final-sort");
        let second_settled_object = store
            .item(2_046)
            .unwrap()
            .downcast::<glib::BoxedAnyObject>()
            .unwrap();
        let ListRow::File(second_settled_item) = &*second_settled_object.borrow::<ListRow>() else {
            panic!("second metadata backing row must remain a file");
        };
        assert_eq!(
            second_settled_item.content_type(),
            "application/x-second-final-sort",
            "the next sorter settlement must drain metadata received during the post-final flush"
        );
        view.close();
    }

    #[gtk::test]
    fn slice5_cached_visible_metadata_survives_a_model_reload() {
        use super::ListRow;
        use crate::core::contracts::{
            Capability, CapabilitySnapshot, EntryKind, EntrySnapshot, VisibleMetadata,
        };
        use crate::core::{SortDirection, SortField, ViewMode, ViewOptions};
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};
        use glib::prelude::Cast;

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/reload-metadata").unwrap();
        // This row is deliberately outside the first virtual page. Reload
        // starts with only the first 128 backing rows, so cached metadata must
        // remain pending until this later page is appended.
        let location = Location::from_uri(&format!("{}/zz-report.txt", root.uri())).unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        provider.set_directory(
            root.clone(),
            (0..129)
                .map(|index| EntrySnapshot {
                    location: if index == 128 {
                        location.clone()
                    } else {
                        Location::from_uri(&format!("{}/item-{index:03}", root.uri())).unwrap()
                    },
                    display_name: if index == 128 {
                        "zz-report.txt".into()
                    } else {
                        format!("item-{index:03}")
                    },
                    kind: EntryKind::File,
                    size: Some(index),
                    modified: None,
                    content_type: Some("text/plain".into()),
                    capabilities: CapabilitySnapshot {
                        supported: [Capability::List].into_iter().collect(),
                        ..Default::default()
                    },
                })
                .collect(),
            CapabilitySnapshot {
                stable_order: true,
                supported: [Capability::List].into_iter().collect(),
                ..Default::default()
            },
        );
        let view = FileView::new_with_repository(root.clone(), provider.clone());
        provider.advance(1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while view.column_view().is_none() || context.pending() {
            assert!(std::time::Instant::now() < deadline);
            context.iteration(false);
        }

        view.apply_visible_metadata(vec![VisibleMetadata {
            location: location.clone(),
            size: Some(1),
            modified: None,
            content_type: Some("application/x-reloaded".into()),
        }]);
        view.set_options(ViewOptions {
            view_mode: ViewMode::DetailedList,
            sort_field: SortField::Name,
            sort_direction: SortDirection::Descending,
            ..Default::default()
        });
        while context.pending() {
            assert!(std::time::Instant::now() < deadline);
            context.iteration(false);
        }

        let model = view.column_view().unwrap().model().unwrap();
        let item = (0..model.n_items())
            .find_map(|index| {
                let item = model.item(index)?.downcast::<glib::BoxedAnyObject>().ok()?;
                let matches = matches!(
                    &*item.borrow::<ListRow>(),
                    ListRow::File(item) if item.location() == &location
                );
                matches.then_some(item)
            })
            .expect("reloaded model must retain the metadata target");
        let ListRow::File(item) = &*item.borrow::<ListRow>() else {
            panic!("fixture row must remain a file");
        };
        assert_eq!(item.content_type(), "application/x-reloaded");
        view.close();
    }

    #[gtk::test]
    fn slice5_resolved_visible_metadata_is_bounded_and_evicted_rows_retry() {
        use crate::application::performance::CachePolicy;
        use crate::core::contracts::{EntryKind, EntrySnapshot, VisibleMetadata};
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/bounded-metadata").unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context, MockConfig::default()));
        let view = FileView::new_with_repository(root.clone(), provider.clone());
        let locations: Vec<_> = (0..=CachePolicy::default().max_entries)
            .map(|index| Location::from_uri(&format!("{}/item-{index:03}", root.uri())).unwrap())
            .collect();
        view.apply_visible_metadata(
            locations
                .iter()
                .map(|location| VisibleMetadata {
                    location: location.clone(),
                    size: Some(1),
                    modified: None,
                    content_type: Some("application/x-bounded".into()),
                })
                .collect(),
        );

        assert!(
            view.resolved_visible_metadata_count() <= CachePolicy::default().max_entries,
            "resolution bookkeeping must be bounded with the visible metadata cache"
        );

        let first = FileItem::from_snapshot(&EntrySnapshot {
            location: locations[0].clone(),
            display_name: "item-000".into(),
            kind: EntryKind::File,
            size: None,
            modified: None,
            content_type: None,
            capabilities: Default::default(),
        });
        view.request_visible_metadata(&first);
        assert_eq!(
            provider.visible_metadata_requests(),
            vec![locations[0].clone()]
        );
        view.close();
    }

    #[gtk::test]
    fn slice5_empty_visible_metadata_completion_retries_the_requested_location() {
        use crate::core::contracts::{EntryKind, EntrySnapshot};
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/empty-metadata").unwrap();
        let target = Location::from_uri(&format!("{}/missing.txt", root.uri())).unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        provider.set_directory(root.clone(), Vec::new(), Default::default());
        let view = FileView::new_with_repository(root, provider.clone());
        provider.advance(1);
        while context.pending() {
            context.iteration(false);
        }
        let item = FileItem::from_snapshot(&EntrySnapshot {
            location: target.clone(),
            display_name: "missing.txt".into(),
            kind: EntryKind::File,
            size: None,
            modified: None,
            content_type: None,
            capabilities: Default::default(),
        });

        let image = gtk::Image::new();
        view.request_flowbox_thumbnail_metadata(&image, &item, true);
        provider.advance(1);
        while context.pending() {
            context.iteration(false);
        }

        assert!(view.visible_metadata.borrow_mut().get(&target).is_none());
        assert_eq!(view.pending_visible_metadata.borrow().len(), 0);
        assert!(view
            .metadata_target
            .flowbox_thumbnail_waiters
            .borrow()
            .get(&target)
            .is_none());

        let rebound_image = gtk::Image::new();
        view.request_flowbox_thumbnail_metadata(&rebound_image, &item, true);
        assert_eq!(
            provider.visible_metadata_requests(),
            vec![target.clone(), target],
            "a successful response that omits a requested location must permit a retry"
        );
        view.close();
    }

    #[gtk::test]
    fn slice5_pending_rebind_does_not_apply_deferred_metadata_from_an_evicted_resolution() {
        use super::{ListRow, VisibleMetadataCacheEntry};
        use crate::application::performance::CachePolicy;
        use crate::core::contracts::{EntryKind, EntrySnapshot, VisibleMetadata};
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};
        use glib::prelude::Cast;

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/deferred-metadata-rebind").unwrap();
        let target = Location::from_uri(&format!("{}/later-page-item", root.uri())).unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context, MockConfig::default()));
        let view = FileView::new_with_repository(root.clone(), provider);
        let target_item = FileItem::from_snapshot(&EntrySnapshot {
            location: target.clone(),
            display_name: "later-page-item".into(),
            kind: EntryKind::File,
            size: None,
            modified: None,
            content_type: None,
            capabilities: Default::default(),
        });

        view.apply_visible_metadata(vec![VisibleMetadata {
            location: target.clone(),
            size: Some(1),
            modified: None,
            content_type: Some("application/x-stale-deferred".into()),
        }]);
        assert_eq!(view.pending_visible_metadata.borrow().len(), 1);

        {
            let mut cache = view.visible_metadata.borrow_mut();
            for index in 0..CachePolicy::default().max_entries {
                let location =
                    Location::from_uri(&format!("{}/filler-{index:03}", root.uri())).unwrap();
                cache.insert(location, VisibleMetadataCacheEntry::Pending);
            }
            assert!(
                cache.get(&target).is_none(),
                "the old resolution is evicted"
            );
        }

        view.request_visible_metadata(&target_item);
        assert!(matches!(
            view.visible_metadata.borrow_mut().get(&target),
            Some(VisibleMetadataCacheEntry::Pending)
        ));

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        store.append(&glib::BoxedAnyObject::new(ListRow::File(target_item)));
        *view.model_store.borrow_mut() = Some(store.clone());
        view.model_row_index.borrow_mut().insert(target.clone(), 0);
        view.flush_visible_metadata();

        let object = store
            .item(0)
            .unwrap()
            .downcast::<glib::BoxedAnyObject>()
            .unwrap();
        let ListRow::File(item) = &*object.borrow::<ListRow>() else {
            panic!("fixture row must be a file");
        };
        assert_eq!(
            item.content_type(),
            "application/octet-stream",
            "a pending rebind must not apply stale deferred metadata"
        );

        let generation = view.navigation.state.borrow().navigation_generation;
        view.complete_visible_metadata(
            generation,
            target.clone(),
            vec![VisibleMetadata {
                location: target,
                size: Some(2),
                modified: None,
                content_type: Some("application/x-fresh".into()),
            }],
        );
        let object = store
            .item(0)
            .unwrap()
            .downcast::<glib::BoxedAnyObject>()
            .unwrap();
        let ListRow::File(item) = &*object.borrow::<ListRow>() else {
            panic!("fixture row must be a file");
        };
        assert_eq!(item.content_type(), "application/x-fresh");
        view.close();
    }

    #[gtk::test]
    fn slice5_initial_sort_completion_disconnects_before_later_settles() {
        use super::connect_once_when_sort_settled;
        use gtk::prelude::*;

        let context = glib::MainContext::default();
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let sorter = gtk::CustomSorter::new(|left, right| {
            let left = left
                .downcast_ref::<glib::BoxedAnyObject>()
                .unwrap()
                .borrow::<u32>();
            let right = right
                .downcast_ref::<glib::BoxedAnyObject>()
                .unwrap()
                .borrow::<u32>();
            left.cmp(&right).into()
        });
        let sorted = gtk::SortListModel::new(Some(store.clone()), Some(sorter));
        sorted.set_incremental(true);
        store.extend_from_slice(
            &(0..2_048)
                .map(|value| glib::BoxedAnyObject::new(value as u32))
                .collect::<Vec<_>>(),
        );
        let completions = std::rc::Rc::new(std::cell::Cell::new(0));
        let recorded = completions.clone();
        connect_once_when_sort_settled(&sorted, move || {
            recorded.set(recorded.get() + 1);
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while completions.get() == 0 {
            assert!(std::time::Instant::now() < deadline);
            context.iteration(false);
        }
        assert_eq!(completions.get(), 1);

        store.append(&glib::BoxedAnyObject::new(2_049u32));
        while sorted.pending() > 0 || context.pending() {
            assert!(std::time::Instant::now() < deadline);
            context.iteration(false);
        }
        assert_eq!(
            completions.get(),
            1,
            "a later metadata-style sort settle must not repeat the O(n) initial restoration"
        );
    }

    #[gtk::test]
    fn slice5_stale_visible_metadata_reply_is_rejected_after_navigation_changes() {
        use crate::core::contracts::VisibleMetadata;
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};

        let context = glib::MainContext::default();
        let first = Location::from_uri("smb://fixture/first").unwrap();
        let second = Location::from_uri("smb://fixture/second").unwrap();
        let item = Location::from_uri(&format!("{}/report.txt", first.uri())).unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context, MockConfig::default()));
        let view = FileView::new_with_repository(first, provider);
        let stale_generation = view.navigation.state.borrow().navigation_generation;

        view.navigate(second);
        view.complete_visible_metadata(
            stale_generation,
            item.clone(),
            vec![VisibleMetadata {
                location: item.clone(),
                size: Some(99),
                modified: None,
                content_type: Some("application/x-stale".into()),
            }],
        );

        assert!(view.visible_metadata.borrow_mut().get(&item).is_none());
        assert!(view.pending_visible_metadata.borrow().len() == 0);
        view.close();
    }

    #[gtk::test]
    fn slice5_reload_reissues_cancelled_visible_metadata_and_applies_the_fresh_result() {
        use super::VisibleMetadataCacheEntry;
        use crate::core::contracts::{EntryKind, EntrySnapshot};
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};
        use gio::prelude::CancellableExt;

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/reload-inflight-metadata").unwrap();
        let target = Location::from_uri(&format!("{}/report.txt", root.uri())).unwrap();
        let entry = EntrySnapshot {
            location: target.clone(),
            display_name: "report.txt".into(),
            kind: EntryKind::File,
            size: Some(99),
            modified: None,
            content_type: Some("text/plain".into()),
            capabilities: Default::default(),
        };
        let provider = std::rc::Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        provider.set_directory(root.clone(), vec![entry.clone()], Default::default());
        let view = FileView::new_with_repository(root, provider.clone());
        provider.advance(1);
        while context.pending() {
            context.iteration(false);
        }
        let item = FileItem::from_snapshot(&entry);

        view.request_visible_metadata(&item);
        let cancellations = provider.visible_metadata_request_cancellables();
        view.reload();
        assert!(cancellations[0].is_cancelled());

        view.request_visible_metadata(&item);
        assert_eq!(
            provider.visible_metadata_requests(),
            vec![target.clone(), target.clone()],
            "a same-generation reload must reissue cancelled visible metadata"
        );
        provider.advance(1);
        while context.pending() {
            context.iteration(false);
        }

        assert!(matches!(
            view.visible_metadata.borrow_mut().get(&target),
            Some(VisibleMetadataCacheEntry::Resolved(metadata))
                if metadata.size == Some(99)
                    && metadata.content_type.as_deref() == Some("text/plain")
        ));
        view.close();
    }

    #[gtk::test]
    fn slice5_navigation_cancels_visible_metadata_and_rejects_its_delayed_reply() {
        use crate::core::contracts::{EntryKind, EntrySnapshot};
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};
        use gio::prelude::CancellableExt;

        let context = glib::MainContext::default();
        let first = Location::from_uri("smb://fixture/navigation-metadata-first").unwrap();
        let second = Location::from_uri("smb://fixture/navigation-metadata-second").unwrap();
        let target = Location::from_uri(&format!("{}/report.txt", first.uri())).unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        let entry = EntrySnapshot {
            location: target.clone(),
            display_name: "report.txt".into(),
            kind: EntryKind::File,
            size: Some(99),
            modified: None,
            content_type: Some("text/plain".into()),
            capabilities: Default::default(),
        };
        provider.set_directory(first.clone(), vec![entry.clone()], Default::default());
        provider.set_visible_metadata_results_after_cancellation(true);
        let view = FileView::new_with_repository(first, provider.clone());
        let item = FileItem::from_snapshot(&entry);

        view.request_visible_metadata(&item);
        let cancellations = provider.visible_metadata_request_cancellables();
        view.navigate(second);
        assert!(cancellations[0].is_cancelled());
        assert_eq!(
            provider.active_visible_metadata_requests(),
            1,
            "cancellation is intent; the provider retains native request ownership until its delayed completion"
        );
        provider.advance(1);
        while context.pending() {
            context.iteration(false);
        }

        assert!(view.visible_metadata.borrow_mut().get(&target).is_none());
        assert_eq!(view.pending_visible_metadata.borrow().len(), 0);
        assert_eq!(
            provider.active_visible_metadata_requests(),
            0,
            "the cancelled native request must release after its terminal reply"
        );
        view.close();
    }

    #[gtk::test]
    fn slice5_closing_a_view_cancels_visible_metadata_and_ignores_its_delayed_reply() {
        use crate::core::contracts::{EntryKind, EntrySnapshot};
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};
        use gio::prelude::CancellableExt;

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/close-metadata").unwrap();
        let target = Location::from_uri(&format!("{}/report.txt", root.uri())).unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        let view = FileView::new_with_repository(root, provider.clone());
        let item = FileItem::from_snapshot(&EntrySnapshot {
            location: target.clone(),
            display_name: "report.txt".into(),
            kind: EntryKind::File,
            size: None,
            modified: None,
            content_type: None,
            capabilities: Default::default(),
        });

        view.request_visible_metadata(&item);
        let cancellations = provider.visible_metadata_request_cancellables();
        assert_eq!(cancellations.len(), 1);
        assert!(!cancellations[0].is_cancelled());

        view.close();
        assert!(cancellations[0].is_cancelled());
        provider.advance(1);
        while context.pending() {
            context.iteration(false);
        }

        assert!(view.visible_metadata.borrow_mut().get(&target).is_none());
        assert_eq!(view.pending_visible_metadata.borrow().len(), 0);
    }

    #[test]
    fn section_overview_preserves_and_restores_a_remote_location() {
        let mut state = TabState::new(Location::from_uri("smb://server/share").unwrap());
        state.show_section(SidebarSection {
            title: "This PC".into(),
            nodes: Vec::new(),
            mountables: vec![],
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
            mountables: vec![],
        });
        state.show_section(SidebarSection {
            title: "Network & online".into(),
            nodes: Vec::new(),
            mountables: vec![],
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

    #[gtk::test]
    fn thumbnail_decodes_after_delayed_visible_content_type_metadata() {
        use crate::core::contracts::{
            CapabilitySnapshot, EntryKind, EntrySnapshot, VisibleMetadata,
        };
        use glib::object::CastNone;
        use gtk::gdk::prelude::TextureExt;
        use std::time::{Duration, Instant};

        super::invalidate_thumbnails();
        let directory = tempfile::tempdir().unwrap();
        let image_path = directory.path().join("delayed.png");
        let pixbuf =
            gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, 16, 16)
                .unwrap();
        pixbuf.fill(0x00ff00ff);
        pixbuf.savev(&image_path, "png", &[]).unwrap();
        let unresolved = FileItem::from_snapshot(&EntrySnapshot {
            location: Location::from_local_path(&image_path),
            display_name: "delayed.png".into(),
            kind: EntryKind::File,
            size: None,
            modified: None,
            content_type: None,
            capabilities: CapabilitySnapshot::default(),
        });
        let image = gtk::Image::new();
        super::apply_thumbnail(&image, &unresolved, true);

        assert!(!super::THUMBNAILS.with(|state| {
            let mut state = state.borrow_mut();
            state.running
                || !state.queue.is_empty()
                || state.cache.get(unresolved.location()).is_some()
        }));

        let resolved = unresolved.with_visible_metadata(&VisibleMetadata {
            location: unresolved.location().clone(),
            size: None,
            modified: None,
            content_type: Some("image/png".into()),
        });
        let promoted_image = gtk::Image::new();
        super::apply_thumbnail(&promoted_image, &resolved, true);
        let context = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(3);
        while promoted_image
            .paintable()
            .and_downcast::<gtk::gdk::Texture>()
            .is_none()
        {
            assert!(
                Instant::now() < deadline,
                "resolved image did not receive a thumbnail"
            );
            context.iteration(false);
        }
        assert_eq!(
            promoted_image
                .paintable()
                .and_downcast::<gtk::gdk::Texture>()
                .map(|texture| (texture.width(), texture.height())),
            Some((256, 256))
        );
        super::invalidate_thumbnails();
    }

    #[gtk::test]
    fn opening_an_archive_navigates_into_it_instead_of_leaving_the_window() {
        // The view used to hand everything that was not a directory to the
        // desktop, which for a zip is another file manager.
        let directory = tempfile::tempdir().unwrap();
        let archive_path = directory.path().join("Trip.zip");
        fs::write(&archive_path, b"PK\x05\x06").unwrap();
        let archive = FileItem::from_path(&archive_path).unwrap();
        let note_path = directory.path().join("note.txt");
        fs::write(&note_path, "plain").unwrap();
        let note = FileItem::from_path(&note_path).unwrap();

        let view = FileView::new(Location::from_local_path(directory.path()));
        let navigated: std::rc::Rc<std::cell::RefCell<Vec<Location>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let record = navigated.clone();
        view.set_navigation_handler(move |location| record.borrow_mut().push(location));

        view.open_item(&archive);
        let inside = navigated.borrow().last().cloned();
        let inside = inside.expect("opening an archive must navigate");
        assert!(inside.is_inside_archive(), "got {}", inside.uri());
        assert_eq!(inside.display_name(), "Trip.zip");

        // Everything else still belongs to the application that owns it, and
        // leaving the window is not a navigation.
        view.open_item(&note);
        assert_eq!(navigated.borrow().len(), 1);
    }

    #[gtk::test]
    fn flowbox_late_successful_metadata_after_reload_does_not_paint_a_stale_tile() {
        use crate::core::contracts::{CapabilitySnapshot, EntryKind, EntrySnapshot};
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};
        use gio::prelude::CancellableExt;
        use glib::object::CastNone;
        use std::time::{Duration, Instant};

        super::invalidate_thumbnails();
        let directory = tempfile::tempdir().unwrap();
        let image_path = directory.path().join("delayed-flowbox.png");
        let pixbuf =
            gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, 16, 16)
                .unwrap();
        pixbuf.fill(0x00ff00ff);
        pixbuf.savev(&image_path, "png", &[]).unwrap();

        let root = Location::from_uri("smb://fixture/flowbox-thumbnail-rebind").unwrap();
        let location = Location::from_local_path(&image_path);
        let entry = EntrySnapshot {
            location: location.clone(),
            display_name: "delayed-flowbox.png".into(),
            kind: EntryKind::File,
            size: None,
            modified: None,
            content_type: Some("image/png".into()),
            capabilities: CapabilitySnapshot::default(),
        };
        let context = glib::MainContext::default();
        let provider = std::rc::Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        provider.set_directory(root.clone(), vec![entry], CapabilitySnapshot::default());
        provider.set_visible_metadata_results_after_cancellation(true);
        let view = FileView::new_with_repository(root, provider.clone());
        provider.advance(1);
        while context.pending() {
            context.iteration(false);
        }
        provider.advance(1);
        while context.pending() {
            context.iteration(false);
        }
        let unresolved = FileItem::from_snapshot(&EntrySnapshot {
            location: location.clone(),
            display_name: "delayed-flowbox.png".into(),
            kind: EntryKind::File,
            size: None,
            modified: None,
            content_type: None,
            capabilities: CapabilitySnapshot::default(),
        });
        let stale_image = gtk::Image::new();
        view.request_flowbox_thumbnail_metadata(&stale_image, &unresolved, true);
        let cancellations = provider.visible_metadata_request_cancellables();
        assert_eq!(cancellations.len(), 1);

        view.reload();
        assert!(cancellations[0].is_cancelled());
        assert!(
            view.metadata_target
                .flowbox_thumbnail_waiters
                .borrow()
                .is_empty(),
            "reload must release stale FlowBox thumbnail waiters"
        );
        assert!(
            view.metadata_target.requests.borrow().is_empty(),
            "reload must release the stale visible-metadata request target"
        );

        provider.advance(1);
        while context.pending() {
            context.iteration(false);
        }
        assert!(
            stale_image
                .paintable()
                .and_downcast::<gtk::gdk::Texture>()
                .is_none(),
            "a late successful callback for a cancelled FlowBox tile must not paint it"
        );
        assert!(view.visible_metadata.borrow_mut().get(&location).is_none());
        assert_eq!(view.pending_visible_metadata.borrow().len(), 0);
        assert_eq!(provider.active_visible_metadata_requests(), 0);
        assert!(view.metadata_target.requests.borrow().is_empty());
        assert!(view
            .metadata_target
            .flowbox_thumbnail_waiters
            .borrow()
            .is_empty());

        let rebound_image = gtk::Image::new();
        view.request_flowbox_thumbnail_metadata(&rebound_image, &unresolved, true);
        assert_eq!(
            provider.visible_metadata_requests(),
            vec![location.clone(), location.clone()],
            "reload must cancel the stale FlowBox request and permit a fresh binding"
        );
        provider.advance(1);
        while context.pending() {
            context.iteration(false);
        }
        assert!(matches!(
            view.visible_metadata.borrow_mut().get(&location),
            Some(super::VisibleMetadataCacheEntry::Resolved(metadata))
                if metadata.content_type.as_deref() == Some("image/png")
        ));
        assert!(view.metadata_target.requests.borrow().is_empty());
        assert!(
            view.metadata_target
                .flowbox_thumbnail_waiters
                .borrow()
                .is_empty(),
            "the current FlowBox completion must release its waiter"
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while rebound_image
            .paintable()
            .and_downcast::<gtk::gdk::Texture>()
            .is_none()
        {
            assert!(
                Instant::now() < deadline,
                "the fresh FlowBox metadata callback did not promote the thumbnail"
            );
            context.iteration(false);
        }
        assert!(
            stale_image
                .paintable()
                .and_downcast::<gtk::gdk::Texture>()
                .is_none(),
            "a late successful FlowBox callback must not paint its detached tile"
        );
        view.close();
        super::invalidate_thumbnails();
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
        let (grid, _pane) = view.grid_view(&[], &ViewOptions::default());

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

    /// `reload` restores keyboard focus, and GTK emits `selected-children-changed`
    /// synchronously while doing so. The handler writes the selection back into
    /// the navigation state, so a state guard left alive across the focus restore
    /// aborts the process with "RefCell already borrowed".
    #[gtk::test]
    fn reload_restoring_focus_does_not_reborrow_navigation_state() {
        use gtk::prelude::*;

        fn find_flow_box(widget: &gtk::Widget) -> Option<gtk::FlowBox> {
            if let Some(grid) = widget.downcast_ref::<gtk::FlowBox>() {
                return Some(grid.clone());
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                if let Some(grid) = find_flow_box(&next) {
                    return Some(grid);
                }
                child = next.next_sibling();
            }
            None
        }

        let directory = tempfile::tempdir().unwrap();
        for index in 0..3 {
            fs::write(directory.path().join(format!("item-{index}.txt")), "item").unwrap();
        }
        let view = FileView::new(Location::from_local_path(directory.path()));
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let loaded = view
                .navigation
                .state
                .borrow()
                .directory
                .as_ref()
                .is_some_and(|snapshot| !snapshot.entries.is_empty());
            if loaded {
                break;
            }
            context.iteration(false);
            assert!(
                std::time::Instant::now() < deadline,
                "the directory never finished loading"
            );
        }
        view.set_options(ViewOptions {
            view_mode: ViewMode::IconGrid,
            ..ViewOptions::default()
        });

        let window = gtk::Window::new();
        window.set_child(Some(&view.widget));
        window.present();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !view.widget.is_mapped() {
            context.iteration(false);
            assert!(
                std::time::Instant::now() < deadline,
                "the view never mapped"
            );
        }

        let grid = find_flow_box(view.widget.upcast_ref::<gtk::Widget>())
            .expect("icon grid renders a FlowBox");
        let first = grid.child_at_index(0).expect("the grid renders tiles");
        first.grab_focus();
        grid.select_child(&first);
        let focused = gtk::prelude::GtkWindowExt::focus(&window)
            .expect("the window keeps focus inside the presented view");
        assert!(
            focused == view.widget.clone().upcast::<gtk::Widget>()
                || focused.is_ancestor(&view.widget),
            "reload only restores focus that already sits inside the view"
        );

        // Any option change repaints through `reload`, the path that crashed
        // when the navigation state guard outlived the focus restore.
        view.set_options(ViewOptions {
            view_mode: ViewMode::IconGrid,
            show_hidden: true,
            ..ViewOptions::default()
        });
        for _ in 0..10 {
            context.iteration(false);
        }

        assert!(view.options().show_hidden, "the reload applied the options");
        assert!(
            find_flow_box(view.widget.upcast_ref::<gtk::Widget>()).is_some(),
            "the reload repainted the icon grid"
        );
        assert_eq!(
            view.navigation.state.borrow().selection_keys.len(),
            1,
            "the focus restore kept the selection the grid reported"
        );
        view.close();
    }
    /// The unavailable-location banner's Close button runs the close handler,
    /// which in a window closes the tab page; that handler re-enters
    /// `FileView::close`, and `close` clears the handler cell. Invoking the
    /// callback under a guard on that cell aborted the process with
    /// "RefCell already borrowed".
    #[gtk::test]
    fn unavailable_banner_close_survives_the_handler_reentering_close() {
        use crate::core::navigation::{NavigationStatus, TabContent};
        use gtk::prelude::*;

        fn find_button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if button.label().as_deref() == Some(label) {
                    return Some(button.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                if let Some(button) = find_button(&next, label) {
                    return Some(button);
                }
                child = next.next_sibling();
            }
            None
        }

        let directory = tempfile::tempdir().unwrap();
        let root = Location::from_local_path(directory.path());
        let view = FileView::new(root.clone());
        {
            let mut state = view.navigation.state.borrow_mut();
            state.content = TabContent::Unavailable(root.clone());
            state.status = NavigationStatus::Idle;
        }
        view.reload();
        let close = find_button(view.widget.upcast_ref::<gtk::Widget>(), "Close")
            .expect("the unavailable banner offers Close");

        let closed = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = closed.clone();
        let handler_view = view.clone();
        view.set_close_handler(move || {
            flag.set(true);
            handler_view.close();
        });

        close.emit_clicked();

        assert!(closed.get(), "the banner ran the close handler");
        assert!(
            view.on_close.borrow().is_none(),
            "close() finished clearing the handler the callback was invoked from"
        );
    }

    /// Shared helpers for the selection tests: a view backed by a real
    /// directory, and the panes a view mode renders.
    fn loaded_view(directory: &std::path::Path, view_mode: ViewMode) -> FileView {
        let view = FileView::new(Location::from_local_path(directory));
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let loaded = view
                .navigation
                .state
                .borrow()
                .directory
                .as_ref()
                .is_some_and(|snapshot| !snapshot.entries.is_empty());
            if loaded {
                break;
            }
            context.iteration(false);
            assert!(
                std::time::Instant::now() < deadline,
                "the directory never finished loading"
            );
        }
        view.set_options(ViewOptions {
            view_mode,
            ..ViewOptions::default()
        });
        view
    }

    fn populate(directory: &std::path::Path, folders: usize, files: usize) {
        for index in 0..folders {
            fs::create_dir(directory.join(format!("folder-{index}"))).unwrap();
        }
        for index in 0..files {
            fs::write(directory.join(format!("file-{index}.txt")), "file").unwrap();
        }
    }

    fn find_flow_box_in(widget: &gtk::Widget) -> Option<gtk::FlowBox> {
        use glib::prelude::Cast;
        if let Some(grid) = widget.downcast_ref::<gtk::FlowBox>() {
            return Some(grid.clone());
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            if let Some(grid) = find_flow_box_in(&next) {
                return Some(grid);
            }
            child = next.next_sibling();
        }
        None
    }

    fn find_column_view_in(widget: &gtk::Widget) -> Option<ColumnView> {
        use glib::prelude::Cast;
        if let Some(list) = widget.downcast_ref::<ColumnView>() {
            return Some(list.clone());
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            if let Some(list) = find_column_view_in(&next) {
                return Some(list);
            }
            child = next.next_sibling();
        }
        None
    }

    /// A plain click on the item that is already selected, and a plain click on
    /// empty space, look the same to GTK: both leave the selection exactly as it
    /// was. That is the signal to drop it.
    #[test]
    fn a_click_that_leaves_the_selection_alone_asks_for_no_selection() {
        let one = vec!["file:///tmp/a".to_owned()];
        assert!(click_leaves_selection_unchanged(&one, &one));

        let two = vec!["file:///tmp/a".to_owned(), "file:///tmp/b".to_owned()];
        let reordered = vec!["file:///tmp/b".to_owned(), "file:///tmp/a".to_owned()];
        assert!(
            click_leaves_selection_unchanged(&two, &reordered),
            "selection order is not a change"
        );

        let other = vec!["file:///tmp/b".to_owned()];
        assert!(
            !click_leaves_selection_unchanged(&one, &other),
            "clicking another item moved the selection"
        );
        assert!(
            !click_leaves_selection_unchanged(&two, &one),
            "narrowing a multi-selection is GTK's doing, not a request to deselect"
        );
        assert!(
            !click_leaves_selection_unchanged(&[], &[]),
            "with nothing selected there is nothing to clear"
        );
    }

    #[gtk::test]
    fn clearing_the_selection_releases_the_grid_highlight() {
        use gtk::prelude::*;

        let directory = tempfile::tempdir().unwrap();
        populate(directory.path(), 0, 3);
        let view = loaded_view(directory.path(), ViewMode::IconGrid);
        let grid = find_flow_box_in(view.widget.upcast_ref::<gtk::Widget>())
            .expect("icon grid renders a FlowBox");
        let tile = grid.child_at_index(1).expect("the grid renders tiles");
        grid.select_child(&tile);
        assert_eq!(view.selected_items().len(), 1);

        view.clear_selection();

        assert!(view.selected_items().is_empty());
        assert!(
            grid.selected_children().is_empty(),
            "the tile is no longer highlighted"
        );
        assert!(
            view.navigation.state.borrow().selection_keys.is_empty(),
            "a reload has no selection left to restore"
        );
        view.close();
    }

    fn listed_file_names(view: &FileView) -> Vec<String> {
        use gtk::prelude::*;
        let list = find_column_view_in(view.widget.upcast_ref::<gtk::Widget>())
            .expect("the view renders a file list");
        let model = list.model().expect("the file list has a model");
        (0..model.n_items())
            .filter_map(|position| model.item(position).and_downcast::<glib::BoxedAnyObject>())
            .filter_map(|object| match &*object.borrow::<super::ListRow>() {
                super::ListRow::File(item) => Some(item.name().to_owned()),
                super::ListRow::Group(_) => None,
            })
            .collect()
    }

    /// A listing carries no size, date or type, so sorting on one of them used
    /// to fall back to name order and appear to do nothing.
    #[gtk::test]
    fn sorting_hybrid_files_by_size_orders_them_by_size() {
        for files in [5usize, 200] {
            let directory = tempfile::tempdir().unwrap();
            for index in 0..files {
                // Sizes run opposite to names, so a name-order fallback is visible.
                fs::write(
                    directory.path().join(format!("file-{index:03}.txt")),
                    "x".repeat(files - index),
                )
                .unwrap();
            }
            let view = loaded_view(directory.path(), ViewMode::Hybrid);
            let expected: Vec<String> = (0..files)
                .rev()
                .map(|index| format!("file-{index:03}.txt"))
                .collect();

            let mut options = view.options();
            options.sort_field = SortField::Size;
            options.sort_direction = SortDirection::Ascending;
            view.set_options(options.clone());
            let context = glib::MainContext::default();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while listed_file_names(&view).first() != expected.first() {
                context.iteration(false);
                assert!(
                    std::time::Instant::now() < deadline,
                    "{files} files never sorted by size: {:?}",
                    &listed_file_names(&view)[..3]
                );
            }
            assert_eq!(listed_file_names(&view), expected);

            options.sort_direction = SortDirection::Descending;
            view.set_options(options);
            while context.iteration(false) {}
            let mut reversed = expected;
            reversed.reverse();
            assert_eq!(listed_file_names(&view), reversed);
            view.close();
        }
    }

    /// Typing a name finds it in whichever pane holds it, and a pause starts a
    /// new search.
    #[gtk::test]
    fn typing_a_name_selects_the_first_matching_item() {
        use gtk::gdk::{Key, ModifierType};

        for mode in [ViewMode::DetailedList, ViewMode::IconGrid, ViewMode::Hybrid] {
            let directory = tempfile::tempdir().unwrap();
            for name in ["apple.txt", "banana.txt", "blueberry.txt", "cherry.txt"] {
                fs::write(directory.path().join(name), "x").unwrap();
            }
            fs::create_dir(directory.path().join("books")).unwrap();
            let view = loaded_view(directory.path(), mode);
            let none = ModifierType::empty();
            let selected = |view: &FileView| -> Vec<String> {
                view.selected_items()
                    .iter()
                    .map(|item| item.name().to_owned())
                    .collect()
            };

            assert!(view.handle_type_ahead_key(Key::c, none));
            assert_eq!(selected(&view), ["cherry.txt"], "{mode:?}");

            assert!(view.handle_type_ahead_key(Key::BackSpace, none));
            assert!(view.handle_type_ahead_key(Key::b, none));
            assert!(view.handle_type_ahead_key(Key::l, none));
            assert_eq!(selected(&view), ["blueberry.txt"], "{mode:?}");

            view.type_ahead.borrow_mut().1 -= std::time::Duration::from_secs(2);
            assert!(view.handle_type_ahead_key(Key::b, none));
            assert!(view.handle_type_ahead_key(Key::o, none));
            assert_eq!(selected(&view), ["books"], "{mode:?}");

            assert!(
                !view.handle_type_ahead_key(Key::a, ModifierType::CONTROL_MASK),
                "shortcuts are not searches"
            );
            view.close();
        }
    }

    /// Hybrid view renders the folders in a FlowBox and the files in a
    /// ColumnView, each owning a selection of its own. Selecting in one pane
    /// used to leave the other pane highlighted, so a folder and a file both
    /// looked selected while the logical selection named only the file.
    #[gtk::test]
    fn selecting_in_one_hybrid_pane_clears_the_other() {
        use gtk::prelude::*;

        let directory = tempfile::tempdir().unwrap();
        populate(directory.path(), 1, 1);
        let view = loaded_view(directory.path(), ViewMode::Hybrid);
        let root = view.widget.upcast_ref::<gtk::Widget>();
        let grid = find_flow_box_in(root).expect("hybrid renders a folder grid");
        let list = find_column_view_in(root).expect("hybrid renders a file list");
        let files = list.model().expect("the file pane has a selection model");
        let folder = grid
            .child_at_index(0)
            .expect("the folder pane renders the folder");

        grid.select_child(&folder);
        assert_eq!(view.selected_items().len(), 1);
        assert!(
            view.selected_items()[0].is_dir(),
            "the folder pane owns the selection"
        );

        files.select_item(0, true);
        assert_eq!(
            view.selected_items().len(),
            1,
            "the file replaced the folder"
        );
        assert!(!view.selected_items()[0].is_dir());
        assert!(
            grid.selected_children().is_empty(),
            "the folder pane gave up its highlight"
        );

        grid.select_child(&folder);
        assert_eq!(view.selected_items().len(), 1);
        assert!(view.selected_items()[0].is_dir());
        assert_eq!(
            files.selection().size(),
            0,
            "the file pane gave up its highlight"
        );
        view.close();
    }

    /// Sorting by a column is not a click on empty space, so it must not cost
    /// the user their selection. `GtkColumnViewTitle` is private to GTK, so the
    /// header is recognised by where it sits rather than by its type.
    #[gtk::test]
    fn a_click_on_a_column_header_is_not_a_click_on_empty_space() {
        use gtk::prelude::*;

        let directory = tempfile::tempdir().unwrap();
        populate(directory.path(), 0, 3);
        let view = loaded_view(directory.path(), ViewMode::DetailedList);
        let window = gtk::Window::new();
        window.set_default_size(640, 480);
        window.set_child(Some(&view.widget));
        window.present();
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // Hit testing needs real allocations, so wait for the list to be laid
        // out rather than merely mapped.
        let list = loop {
            context.iteration(false);
            let laid_out =
                find_column_view_in(view.widget.upcast_ref::<gtk::Widget>()).filter(|list| {
                    list.compute_bounds(&view.widget)
                        .is_some_and(|bounds| bounds.height() > 60.0)
                });
            if let Some(list) = laid_out {
                break list;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the list was never laid out"
            );
        };
        let bounds = list
            .compute_bounds(&view.widget)
            .expect("the list is inside the scrolled window");
        let x = f64::from(bounds.x()) + 4.0;
        let header = f64::from(bounds.y()) + 2.0;
        let rows = f64::from(bounds.y()) + f64::from(bounds.height()) / 2.0;

        assert!(
            click_is_on_a_control_or_header(&view.widget, x, header),
            "the column headers sit above the rows"
        );
        assert!(
            !click_is_on_a_control_or_header(&view.widget, x, rows),
            "the rows and the empty space below them are the click target"
        );
        view.close();
    }
    /// A directory that keeps changing must not start a second listing on top
    /// of the one still streaming. `/tmp`-scale directories change continuously,
    /// and refreshing over an in-flight listing rebuilds the view forever
    /// instead of ever rendering it.
    #[gtk::test]
    fn monitor_changes_wait_for_an_in_flight_listing() {
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/share").unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        let view = FileView::new_with_repository(root.clone(), provider.clone());
        // The constructor starts the first listing and the mock completes
        // nothing until it is advanced, so a listing is in flight.
        assert!(
            view.has_pending_navigation(),
            "the initial listing must still be in flight"
        );

        view.arm_monitor_refresh();
        view.run_monitor_refresh();
        assert!(
            view.monitor_refresh_pending.get(),
            "a change during a listing is held back, not refreshed over it"
        );

        provider.advance(1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while view.has_pending_navigation() {
            context.iteration(false);
            assert!(
                std::time::Instant::now() < deadline,
                "the listing never completed"
            );
        }
        assert!(
            view.monitor_refresh_pending.get(),
            "completing a listing must re-arm the held-back refresh"
        );

        view.run_monitor_refresh();
        assert!(!view.monitor_refresh_pending.get());
        assert!(
            view.has_pending_navigation(),
            "the held-back refresh started exactly one listing"
        );
        view.close();
    }

    /// A directory that never stops changing must not refresh at a constant
    /// cadence forever: each monitor-driven refresh doubles the wait for the
    /// next, and a quiet interval restores the prompt one.
    #[gtk::test]
    fn consecutive_monitor_refreshes_back_off_and_quiet_resets() {
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/share").unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        let view = FileView::new_with_repository(root.clone(), provider.clone());
        provider.advance(1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while view.has_pending_navigation() {
            context.iteration(false);
            assert!(std::time::Instant::now() < deadline);
        }

        for expected in [200u64, 400] {
            view.run_monitor_refresh();
            assert_eq!(
                view.monitor_backoff_ms.get(),
                expected,
                "each executed refresh doubles the wait for the next"
            );
            provider.advance(1);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while view.has_pending_navigation() {
                context.iteration(false);
                assert!(std::time::Instant::now() < deadline);
            }
        }

        view.last_monitor_change
            .set(std::time::Instant::now() - std::time::Duration::from_secs(1));
        view.arm_monitor_refresh();
        assert_eq!(
            view.monitor_backoff_ms.get(),
            100,
            "a quiet interval restores the prompt reaction"
        );
        view.close();
    }

    /// The status bar snapshot must say "streaming" while the listing is in
    /// flight and carry the folder/file split of the directory once it lands.
    #[gtk::test]
    fn status_reports_loading_then_the_directory_counts() {
        use crate::core::contracts::{Capability, CapabilitySnapshot, EntryKind, EntrySnapshot};
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/share").unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        let entry = |name: &str, kind: EntryKind| EntrySnapshot {
            location: Location::from_uri(&format!("{}/{}", root.uri(), name)).unwrap(),
            display_name: name.into(),
            kind,
            size: None,
            modified: None,
            content_type: None,
            capabilities: CapabilitySnapshot {
                supported: [Capability::List].into_iter().collect(),
                ..Default::default()
            },
        };
        provider.set_directory(
            root.clone(),
            vec![
                entry("docs", EntryKind::Directory),
                entry("pics", EntryKind::Directory),
                entry("a.txt", EntryKind::File),
                entry("b.txt", EntryKind::File),
                entry("c.txt", EntryKind::File),
            ],
            CapabilitySnapshot {
                version: 1,
                supported: [Capability::List].into_iter().collect(),
                stable_order: true,
                ..Default::default()
            },
        );
        let view = FileView::new_with_repository(root.clone(), provider.clone());
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = seen.clone();
        view.set_status_handler(move |status| captured.borrow_mut().push(status));
        assert!(
            seen.borrow().first().is_some_and(|status| status.loading),
            "the first snapshot says the listing is streaming"
        );

        provider.advance(1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while seen.borrow().last().is_some_and(|status| status.loading) {
            context.iteration(false);
            assert!(
                std::time::Instant::now() < deadline,
                "the listing never landed"
            );
        }
        let last = *seen.borrow().last().expect("a snapshot arrived");
        assert!(!last.loading);
        assert!(last.directory);
        assert_eq!((last.folders, last.files), (2, 3));
        view.close();
    }

    /// A refresh of a directory whose listing changed must update the widgets
    /// already on screen rather than replacing them: rebuilding flashes an
    /// empty view and drops scroll position, focus and selection, which on a
    /// busy directory reads as the folder reloading itself over and over.
    #[gtk::test]
    fn a_changed_listing_splices_into_the_view_already_on_screen() {
        use crate::core::contracts::{Capability, CapabilitySnapshot, EntryKind, EntrySnapshot};
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/share").unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        let entry = |name: &str| EntrySnapshot {
            location: Location::from_uri(&format!("{}/{}", root.uri(), name)).unwrap(),
            display_name: name.into(),
            kind: EntryKind::File,
            size: Some(4),
            modified: None,
            content_type: None,
            capabilities: CapabilitySnapshot {
                supported: [Capability::List].into_iter().collect(),
                ..Default::default()
            },
        };
        let caps = CapabilitySnapshot {
            version: 1,
            supported: [Capability::List].into_iter().collect(),
            stable_order: true,
            ..Default::default()
        };
        provider.set_directory(
            root.clone(),
            vec![entry("a.txt"), entry("b.txt")],
            caps.clone(),
        );
        let view = FileView::new_with_repository(root.clone(), provider.clone());
        let settle = || {
            provider.advance(1);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while view.has_pending_navigation() {
                context.iteration(false);
                assert!(std::time::Instant::now() < deadline);
            }
        };
        settle();
        let rendered = view.widget.child().expect("the directory is rendered");

        view.refresh();
        settle();
        assert_eq!(
            view.widget.child(),
            Some(rendered.clone()),
            "an unchanged listing must not rebuild the view"
        );

        provider.set_directory(
            root.clone(),
            vec![entry("a.txt"), entry("b.txt"), entry("c.txt")],
            caps,
        );
        view.refresh();
        settle();
        assert_eq!(
            view.widget.child(),
            Some(rendered),
            "a listing that gained a file splices into the same view"
        );
        assert_eq!(view.selected_items().len(), 0);
        view.close();
    }

    /// The icon grid indexes its children by position, so a splice that drops
    /// and adds tiles must keep the selection on the same files, not on
    /// whatever now sits at the old index.
    #[gtk::test]
    fn a_changed_grid_keeps_its_tiles_and_its_selection() {
        use crate::core::contracts::{Capability, CapabilitySnapshot, EntryKind, EntrySnapshot};
        use crate::infrastructure::mock_provider::{MockConfig, MockProvider};

        let context = glib::MainContext::default();
        let root = Location::from_uri("smb://fixture/share").unwrap();
        let provider = std::rc::Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        let entry = |name: &str| EntrySnapshot {
            location: Location::from_uri(&format!("{}/{}", root.uri(), name)).unwrap(),
            display_name: name.into(),
            kind: EntryKind::File,
            size: Some(4),
            modified: None,
            content_type: None,
            capabilities: CapabilitySnapshot {
                supported: [Capability::List].into_iter().collect(),
                ..Default::default()
            },
        };
        let caps = CapabilitySnapshot {
            version: 1,
            supported: [Capability::List].into_iter().collect(),
            stable_order: true,
            ..Default::default()
        };
        provider.set_directory(
            root.clone(),
            vec![entry("a.txt"), entry("b.txt"), entry("c.txt")],
            caps.clone(),
        );
        let view = FileView::new_with_repository(root.clone(), provider.clone());
        view.set_options(ViewOptions {
            view_mode: ViewMode::IconGrid,
            ..ViewOptions::default()
        });
        let settle = || {
            provider.advance(1);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while view.has_pending_navigation() {
                context.iteration(false);
                assert!(std::time::Instant::now() < deadline);
            }
        };
        settle();
        let grid = find_flow_box_in(&view.widget.child().expect("rendered"))
            .expect("icon grid renders a FlowBox");
        let kept = grid.child_at_index(2).expect("c.txt has a tile");
        grid.select_child(&kept);
        assert_eq!(view.selected_items()[0].name(), "c.txt");

        // a.txt goes and d.txt arrives: c.txt moves from index 2 to 1.
        provider.set_directory(
            root.clone(),
            vec![entry("b.txt"), entry("c.txt"), entry("d.txt")],
            caps,
        );
        view.refresh();
        settle();

        let after =
            find_flow_box_in(&view.widget.child().expect("rendered")).expect("still a grid");
        assert_eq!(after, grid, "the grid itself is not rebuilt");
        assert_eq!(after.child_at_index(1).as_ref(), Some(&kept));
        assert!(after.child_at_index(3).is_none());
        let selected = view.selected_items();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name(), "c.txt");
        view.close();
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

/// A tile built from an unresolved listing row shows the generic fallback
/// icon; once its content type arrives, show the icon that type maps to, then
/// promote to a thumbnail where those are on.
fn apply_resolved_item(image: &Image, item: &FileItem, thumbnails: bool) {
    if item.content_type_resolved() && !item.is_dir() {
        image.set_from_gicon(&gio::content_type_get_icon(item.content_type()));
    }
    apply_thumbnail(image, item, thumbnails);
}

fn apply_thumbnail(image: &Image, item: &FileItem, thumbnails: bool) {
    // The initial streaming listing deliberately omits content types. Do not
    // convert that temporary fallback into a URI-scoped negative cache entry:
    // binding will run again after visible metadata promotes the row.
    if !thumbnails || !item.content_type_resolved() {
        return;
    }
    if let Some(cached) = THUMBNAILS.with(|state| state.borrow_mut().cache.get(item.location())) {
        if let Some(texture) = cached {
            image.set_from_paintable(Some(&texture));
        }
        return;
    }
    let start = THUMBNAILS.with(|state| {
        let mut state = state.borrow_mut();
        // Only bound GTK rows enter this queue; remove rows recycled since the
        // request and cap both queued requests and retained decoded textures.
        state
            .queue
            .retain(|(image, _, _)| image.upgrade().is_some());
        if state.queue.len() >= 256 {
            state.queue.pop_front();
        }
        let epoch = state.epoch;
        state
            .queue
            .push_back((image.downgrade(), item.clone(), epoch));
        if state.running {
            false
        } else {
            state.running = true;
            true
        }
    });
    if start {
        glib::MainContext::ref_thread_default().spawn_local(async move {
            loop {
                let request = THUMBNAILS.with(|state| state.borrow_mut().queue.pop_front());
                let Some((image, item, epoch)) = request else {
                    THUMBNAILS.with(|state| state.borrow_mut().running = false);
                    break;
                };
                if image.upgrade().is_none() {
                    continue;
                }
                let location = item.location().clone();
                let pixels = gio::spawn_blocking(move || {
                    let path = thumbnail_cache_homes()
                        .into_iter()
                        .find_map(|home| cached_thumbnail_file_for(&item, &home))
                        .or_else(|| local_image_thumbnail_file(&item))?;
                    let pixbuf =
                        gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(path, 256, 256, true).ok()?;
                    Some((
                        pixbuf.read_pixel_bytes(),
                        pixbuf.width(),
                        pixbuf.height(),
                        pixbuf.rowstride(),
                        pixbuf.has_alpha(),
                    ))
                })
                .await
                .ok()
                .flatten();
                let current = THUMBNAILS.with(|state| state.borrow().epoch == epoch);
                if !current {
                    continue;
                }
                let texture: Option<gtk::gdk::Texture> =
                    pixels.map(|(bytes, width, height, stride, alpha)| {
                        gtk::gdk::MemoryTexture::new(
                            width,
                            height,
                            if alpha {
                                gtk::gdk::MemoryFormat::R8g8b8a8
                            } else {
                                gtk::gdk::MemoryFormat::R8g8b8
                            },
                            &bytes,
                            stride as usize,
                        )
                        .upcast()
                    });
                THUMBNAILS.with(|state| state.borrow_mut().cache.insert(location, texture.clone()));
                if let (Some(image), Some(texture)) = (image.upgrade(), texture) {
                    image.set_from_paintable(Some(&texture));
                }
            }
        });
    }
}

struct ThumbnailState {
    cache: crate::application::performance::VisibleCache<Option<gtk::gdk::Texture>>,
    queue: std::collections::VecDeque<(glib::WeakRef<Image>, FileItem, u64)>,
    epoch: u64,
    running: bool,
}
thread_local! {
    static THUMBNAILS: RefCell<ThumbnailState> = RefCell::new(ThumbnailState {
        cache: crate::application::performance::VisibleCache::new(Default::default()),
        queue: Default::default(), epoch: 0, running: false,
    });
}
fn invalidate_thumbnails() {
    THUMBNAILS.with(|state| {
        let mut state = state.borrow_mut();
        state.epoch = state.epoch.wrapping_add(1);
        state.cache.invalidate();
        state.queue.clear();
    });
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

fn attach_drag_source(widget: &impl IsA<gtk::Widget>, item: FileItem, view: FileView) {
    let drop_item = item.clone();
    attach_folder_drop(widget, view.clone(), move || Some(drop_item.clone()));
    let touch_item = item.clone();
    attach_touch_menu(widget, view.clone(), move || Some(touch_item.clone()));
    let drag_source = gtk::DragSource::new();
    drag_source.set_actions(gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE);
    drag_source.connect_prepare(move |source, _, _| {
        view.select_context_item(&item);
        source.set_actions(drag_actions(&view));
        drag_selection_provider(&view)
    });
    widget.add_controller(drag_source);
}

fn attach_folder_drop(
    widget: &impl IsA<gtk::Widget>,
    view: FileView,
    item: impl Fn() -> Option<FileItem> + 'static,
) {
    let target_view = view.clone();
    attach_async_drop(widget, view, move || {
        let folder = item()?;
        (target_view
            .navigation
            .state
            .borrow()
            .has_available_directory()
            && folder.is_dir()
            && folder
                .capabilities()
                .supports(crate::core::contracts::Capability::Create))
        .then(|| folder.location().clone())
    });
}

fn drop_action(
    actions: gtk::gdk::DragAction,
    modifiers: gtk::gdk::ModifierType,
) -> gtk::gdk::DragAction {
    use gtk::gdk::{DragAction as A, ModifierType as M};
    if modifiers.contains(M::SHIFT_MASK) && actions.contains(A::MOVE) {
        A::MOVE
    } else if actions.contains(A::COPY) {
        A::COPY
    } else if actions.contains(A::MOVE) {
        A::MOVE
    } else {
        A::empty()
    }
}

fn attach_async_drop(
    widget: &impl IsA<gtk::Widget>,
    view: FileView,
    destination: impl Fn() -> Option<Location> + 'static,
) -> gtk::DropTargetAsync {
    use gtk::gdk::{ContentFormats, DragAction};
    let target = gtk::DropTargetAsync::new(
        Some(ContentFormats::new(&["text/uri-list"])),
        DragAction::COPY | DragAction::MOVE,
    );
    let destination = Rc::new(destination);
    let negotiated = Rc::new(RefCell::new(None::<(gtk::gdk::Drop, DragAction)>));
    let leaving = negotiated.clone();
    target.connect_drag_leave(move |_, drop| {
        if leaving
            .borrow()
            .as_ref()
            .is_some_and(|(previous, _)| previous == drop)
        {
            leaving.borrow_mut().take();
        }
    });
    for enter in [true, false] {
        let destination = destination.clone();
        let negotiated = negotiated.clone();
        let negotiate =
            move |target: &gtk::DropTargetAsync, drop: &gtk::gdk::Drop, _: f64, _: f64| {
                let action = if destination().is_some() {
                    drop_action(drop.actions(), target.current_event_state())
                } else {
                    DragAction::empty()
                };
                *negotiated.borrow_mut() = Some((drop.clone(), action));
                action
            };
        if enter {
            target.connect_drag_enter(negotiate);
        } else {
            target.connect_drag_motion(negotiate);
        }
    }
    target.connect_drop(move |_, drop, _, _| {
        let Some(destination) = destination() else {
            return false;
        };
        let action = negotiated
            .borrow()
            .as_ref()
            .filter(|(previous, _)| previous == drop)
            .map(|(_, action)| *action)
            .unwrap_or_else(|| drop_action(drop.actions(), gtk::gdk::ModifierType::empty()));
        if action.is_empty() || !drop.actions().contains(action) {
            return false;
        }
        let source_deletes = action == DragAction::MOVE && drop.drag().is_none();
        let drop = drop.clone();
        let finish = drop.clone();
        let completion = TransferCompletion::new(move |success| {
            // Older GTK X11 sources emit dnd-finished even for a failed drop,
            // deriving delete_data from the last selected action. Withdraw
            // MOVE before rejecting so they cannot delete untransferred data.
            if !success {
                finish.status(DragAction::empty(), DragAction::empty());
            }
            finish.finish(if success { action } else { DragAction::empty() });
        });
        let view = view.clone();
        glib::MainContext::default().spawn_local(async move {
            // Read the wire format, including cross-process sources that have no
            // GdkDrag object. URI validation and size limits precede dispatch.
            let Ok((stream, _)) = drop
                .read_future(&["text/uri-list"], glib::Priority::DEFAULT)
                .await
            else {
                return;
            };
            let mut bytes = Vec::new();
            loop {
                let Ok(chunk) = stream
                    .read_bytes_future(8192, glib::Priority::DEFAULT)
                    .await
                else {
                    return;
                };
                if chunk.is_empty() {
                    break;
                }
                bytes.extend_from_slice(chunk.as_ref());
                if bytes.len() > 1024 * 1024 {
                    return;
                }
            }
            let Ok(text) = std::str::from_utf8(&bytes) else {
                return;
            };
            view.dispatch_transfer(
                text,
                if action == DragAction::MOVE {
                    ClipboardAction::Cut
                } else {
                    ClipboardAction::Copy
                },
                destination,
                source_deletes,
                Some(completion),
            );
        });
        // Acceptance of responsibility is not completion; only the callback
        // above acknowledges GTK after the whole coordinator batch terminates.
        true
    });
    widget.add_controller(target.clone());
    target
}

fn attach_touch_menu(
    widget: &impl IsA<gtk::Widget>,
    view: FileView,
    item: impl Fn() -> Option<FileItem> + 'static,
) {
    let gesture = gtk::GestureLongPress::new();
    gesture.set_touch_only(true);
    let parent = widget.clone().upcast::<gtk::Widget>().downgrade();
    gesture.connect_pressed(move |_, x, y| {
        if let (Some(parent), Some(item)) = (parent.upgrade(), item()) {
            view.select_context_item(&item);
            show_item_context_menu(&parent, x, y, file_target(&item), &view, false);
        }
    });
    widget.add_controller(gesture);
}

fn drag_selection_provider(view: &FileView) -> Option<gtk::gdk::ContentProvider> {
    let payload = view
        .drag_payload(ClipboardAction::Copy)
        .or_else(|| view.drag_payload(ClipboardAction::Cut))?;
    let files = gtk::gdk::FileList::from_array(
        &payload
            .locations
            .iter()
            .map(|l| gio::File::for_uri(l.uri()))
            .collect::<Vec<_>>(),
    );
    Some(gtk::gdk::ContentProvider::for_value(&files.to_value()))
}

fn drag_actions(view: &FileView) -> gtk::gdk::DragAction {
    let availability = view.command_availability();
    let mut actions = gtk::gdk::DragAction::empty();
    if availability.can_copy {
        actions |= gtk::gdk::DragAction::COPY;
    }
    if availability.can_cut {
        actions |= gtk::gdk::DragAction::MOVE;
    }
    actions
}

#[cfg(test)]
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
    view.select_items(vec![item.clone()]);
    if view.on_application_command.borrow().is_some() {
        view.dispatch_context_command(file_target(&item), ContextCommand::Open);
    } else {
        view.open_item(&item);
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
