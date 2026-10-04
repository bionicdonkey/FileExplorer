use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::ui::accessibility::{name_control, AccessibleStatus, StatusPriority};
use adw::prelude::*;
use adw::{Application, ApplicationWindow, HeaderBar, TabPage, TabView, ToolbarView};
use gtk::{
    Align, Box as GtkBox, Button, CheckButton, Entry, Label, MenuButton, Orientation, Paned,
    Popover, SearchEntry, Separator,
};

use crate::application::commands::{
    route, ClipboardAction, ClipboardPayload, ClipboardState, Shortcut, TransferCompletion,
};
use crate::core::contracts::{ConflictDecision, MutationKind};
use crate::core::location::Location;
use crate::core::SearchHistory;
fn unmount_failure_message(error: &glib::Error) -> &'static str {
    crate::core::contracts::OperationError::from_gio(error, None, None).message_key
}

use crate::core::{ApplicationPreferences, ConflictDefault, OperationManager};
use crate::core::{
    GridSize, GroupField, SidebarModel, SidebarPreferences, SortDirection, SortField, ViewMode,
    ViewOptions, ViewPreferencesStore,
};
use crate::ui::context_menu::{ContextCommand, ContextTarget};
use crate::ui::file_view::FileView;
use crate::ui::header::{FileAction, HeaderControls, NavigationAction};
use crate::ui::operations_page;
use crate::ui::preferences_page;
use crate::ui::properties_dialog::PropertiesDialog;
use crate::ui::sidebar::{Sidebar, SidebarAction};
use crate::ui::tab_layout::{calculate_tab_layout, TabLayoutInput};
use crate::ui::tabs::TabStrip;

struct TabData {
    view: FileView,
    location: Rc<RefCell<Location>>,
    page: TabPage,
}

#[derive(Clone)]
struct FileClipboard {
    state: ClipboardState,
    source_view: FileView,
    provider: gtk::gdk::ContentProvider,
}

thread_local! {
    // Windows on the same display share the cut lease, including its source
    // highlight. The registry itself does not keep a clipboard owner alive.
    static FILE_CLIPBOARDS: RefCell<Vec<(gtk::gdk::Clipboard, std::rc::Weak<RefCell<Option<FileClipboard>>>)>> = RefCell::new(Vec::new());
}

fn application_clipboard(system: &gtk::gdk::Clipboard) -> Rc<RefCell<Option<FileClipboard>>> {
    FILE_CLIPBOARDS.with(|owners| {
        let mut owners = owners.borrow_mut();
        owners.retain(|(_, owner)| owner.strong_count() > 0);
        if let Some(owner) = owners
            .iter()
            .find_map(|(clipboard, owner)| (clipboard == system).then(|| owner.upgrade()).flatten())
        {
            return owner;
        }
        let owner = Rc::new(RefCell::new(None));
        watch_clipboard(system, &owner);
        owners.push((system.clone(), Rc::downgrade(&owner)));
        owner
    })
}

fn clipboard_payload(
    owner: &Rc<RefCell<Option<FileClipboard>>>,
    system: &gtk::gdk::Clipboard,
) -> Option<ClipboardPayload> {
    let mut state = owner.borrow_mut();
    if state
        .as_ref()
        .is_some_and(|c| system.content().as_ref() != Some(&c.provider))
    {
        if let Some(previous) = state.take() {
            previous.source_view.set_pending_cut_locations(vec![]);
        }
    }
    state.as_ref().and_then(|c| c.state.payload().cloned())
}

fn watch_clipboard(system: &gtk::gdk::Clipboard, owner: &Rc<RefCell<Option<FileClipboard>>>) {
    let owner = Rc::downgrade(owner);
    system.connect_changed(move |system| {
        let system = system.clone();
        let owner = owner.clone();
        // set_content emits synchronously, before the new lease is installed.
        glib::idle_add_local_once(move || {
            if let Some(owner) = owner.upgrade() {
                clipboard_payload(&owner, &system);
            }
        });
    });
}

#[derive(Clone)]
struct ClipboardLease {
    owner: Rc<RefCell<Option<FileClipboard>>>,
    generation: u64,
}

pub struct ExplorerWindow {
    window: ApplicationWindow,
}

impl ExplorerWindow {
    pub fn new(app: &Application) -> Self {
        Self::new_at(app, Location::home(), true)
    }

    fn new_at(app: &Application, initial: Location, restore_session: bool) -> Self {
        let window = ApplicationWindow::builder()
            .application(app)
            .title("Zorin Explorer")
            .default_width(1280)
            .default_height(800)
            .build();
        let application_preferences = Rc::new(RefCell::new(
            ApplicationPreferences::load_default().unwrap_or_default(),
        ));
        let operations = OperationManager::load_history(
            application_preferences.borrow().history_path(),
            application_preferences.borrow().operation_history_days,
        );
        let search_history = Rc::new(RefCell::new({
            let path = application_preferences.borrow().search_history_path();
            let mut history = path
                .map(SearchHistory::load_from)
                .transpose()
                .unwrap_or(None)
                .unwrap_or_else(SearchHistory::in_memory);
            let _ = history.enforce_policy(
                application_preferences.borrow().record_search_history,
                application_preferences
                    .borrow()
                    .search_history_retention_days,
            );
            history
        }));

        let header = HeaderBar::new();
        header.set_show_title(false);
        header.add_css_class("explorer-titlebar");
        let sidebar_button = Button::from_icon_name("sidebar-show-symbolic");
        sidebar_button.add_css_class("flat");
        sidebar_button.set_tooltip_text(Some("Show or hide sidebar"));
        header.pack_start(&sidebar_button);

        let tabs = TabStrip::new();
        tabs.bar.set_hexpand(false);
        let tab_host = GtkBox::new(Orientation::Horizontal, 0);
        tab_host.set_halign(gtk::Align::Start);
        tab_host.append(&tabs.bar);
        let new_tab = Button::from_icon_name("list-add-symbolic");
        new_tab.add_css_class("flat");
        new_tab.set_tooltip_text(Some("New tab (Ctrl+T)"));
        tab_host.append(&new_tab);
        header.pack_start(&tab_host);
        let preferences_button = Button::from_icon_name("emblem-system-symbolic");
        preferences_button.add_css_class("flat");
        preferences_button.set_tooltip_text(Some("Preferences"));
        let operations_button = Button::from_icon_name("view-list-symbolic");
        operations_button.add_css_class("flat");
        operations_button.set_tooltip_text(Some("File operations"));
        for (button, name) in [
            (&sidebar_button, "Show or hide sidebar"),
            (&new_tab, "New tab (Ctrl+T)"),
            (&preferences_button, "Preferences"),
            (&operations_button, "File operations"),
        ] {
            name_control(button, name);
        }
        let operations_tabs = tabs.view.clone();
        let operations_manager = operations.clone();
        operations_button.connect_clicked(move |_| {
            open_operations_tab(&operations_tabs, operations_manager.clone())
        });
        header.pack_end(&operations_button);
        header.pack_end(&preferences_button);

        let startup_paths =
            if restore_session && application_preferences.borrow().restore_previous_tabs {
                match application_preferences.borrow().load_session() {
                    Ok(session) => {
                        for notice in session.notices {
                            eprintln!("Saved tabs recovery notice: {notice:?}");
                        }
                        session
                            .locations
                            .into_iter()
                            .take(crate::core::application_preferences::MAX_SESSION_TABS)
                            .collect()
                    }
                    Err(error) => {
                        eprintln!("Saved tabs could not be recovered: {error}");
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
        let startup_paths = if startup_paths.is_empty() {
            vec![initial.clone()]
        } else {
            startup_paths
        };
        let first_location = Rc::new(RefCell::new(startup_paths[0].clone()));
        let header_controls = HeaderControls::new(first_location.clone());
        let tabs_data: Rc<RefCell<Vec<TabData>>> = Rc::new(RefCell::new(Vec::new()));
        let preferences = Rc::new(RefCell::new(
            ViewPreferencesStore::load_default()
                .unwrap_or_else(|_| ViewPreferencesStore::in_memory()),
        ));
        let clipboard =
            application_clipboard(&gtk::prelude::WidgetExt::display(&window).clipboard());
        for path in startup_paths {
            let location = Rc::new(RefCell::new(path.clone()));
            let view = FileView::from_location(path.clone());
            if let Some(path) = path.local_path() {
                view.set_options(preferences.borrow().options_for(&path, &[]));
            }
            let page = tabs.view.append(&view.widget);
            page.set_title(&path.display_name());
            tabs_data.borrow_mut().push(TabData {
                view,
                location,
                page,
            });
        }

        apply_tab_layout(&window, &tabs);
        let layout_window = window.clone();
        let layout_tabs = tabs.clone();
        tabs.view.connect_n_pages_notify(move |_| {
            apply_tab_layout(&layout_window, &layout_tabs);
        });

        let sidebar_model = Rc::new(RefCell::new(SidebarModel::new(
            SidebarPreferences::load_default().unwrap_or_else(|_| SidebarPreferences::in_memory()),
            Location::home().local_path().expect("home is local"),
        )));
        let sidebar_holder: Rc<RefCell<Option<Sidebar>>> = Rc::new(RefCell::new(None));
        let sidebar_context_data = tabs_data.clone();
        let sidebar_context_tabs = tabs.view.clone();
        let sidebar_context_header = header_controls.clone();
        let sidebar_context_preferences = preferences.clone();
        let sidebar_context_app = app.clone();
        let sidebar_context_clipboard = clipboard.clone();
        let sidebar_context_operations = operations.clone();
        let sidebar_window = window.clone();
        let sidebar_model_actions = sidebar_model.clone();
        let sidebar_holder_actions = sidebar_holder.clone();
        let sidebar = Sidebar::new(sidebar_model.clone(), &window, move |action| match action {
            SidebarAction::OpenSection(title) => {
                let section = sidebar_model_actions
                    .borrow()
                    .sections()
                    .into_iter()
                    .find(|section| section.title == title);
                let sidebar = sidebar_holder_actions.borrow().clone();
                let target = sidebar_target(
                    selected_index(&sidebar_context_tabs, &sidebar_context_data),
                    sidebar_context_data.borrow().len(),
                );
                if let (Some(index), Some(section), Some(sidebar)) = (target, section, sidebar) {
                    show_file_tab(&sidebar_context_tabs, &sidebar_context_data, index);
                    let tab = &sidebar_context_data.borrow()[index];
                    tab.view.show_section_overview(section.clone());
                    tab.page.set_title(&section.title);
                    sidebar_context_header.set_section(&section.title);
                    sidebar.set_active_section(section.title);
                }
            }
            SidebarAction::Open(path) => {
                let sidebar = sidebar_holder_actions.borrow().clone();
                let target = sidebar_target(
                    selected_index(&sidebar_context_tabs, &sidebar_context_data),
                    sidebar_context_data.borrow().len(),
                );
                if let (Some(index), Some(sidebar)) = (target, sidebar) {
                    show_file_tab(&sidebar_context_tabs, &sidebar_context_data, index);
                    navigate_tab(
                        index,
                        path,
                        &sidebar_context_data,
                        &sidebar_context_tabs,
                        &sidebar_context_header,
                        &sidebar_context_preferences,
                        &sidebar,
                        true,
                    );
                }
            }
            SidebarAction::OpenInNewTab(path) => {
                if let Some(sidebar) = sidebar_holder_actions.borrow().clone() {
                    add_tab_at(
                        path,
                        &sidebar_context_data,
                        &sidebar_context_tabs,
                        &sidebar_context_header,
                        &sidebar_context_preferences,
                        &sidebar_context_app,
                        &sidebar_context_clipboard,
                        &sidebar,
                        &sidebar_context_operations,
                    );
                }
            }
            SidebarAction::OpenInNewWindow(path) => {
                ExplorerWindow::new_at(&sidebar_context_app, path, false).present()
            }
            SidebarAction::OpenInTerminal(path) => open_terminal(&path),
            SidebarAction::Pin(path) => {
                let mut model = sidebar_model_actions.borrow_mut();
                model.preferences_mut().pin_location(path);
                let _ = model.preferences().save();
                drop(model);
                if let Some(sidebar) = sidebar_holder_actions.borrow().as_ref() {
                    sidebar.refresh();
                }
            }
            SidebarAction::Unpin(path) => {
                let mut model = sidebar_model_actions.borrow_mut();
                model.preferences_mut().unpin_location(&path);
                let _ = model.preferences().save();
                drop(model);
                if let Some(sidebar) = sidebar_holder_actions.borrow().as_ref() {
                    sidebar.refresh();
                }
            }
            SidebarAction::AddNetworkLocation => show_network_location_dialog(
                &sidebar_window,
                sidebar_model_actions.clone(),
                sidebar_holder_actions.clone(),
            ),
            SidebarAction::Unmount(uri) => {
                let mount = sidebar_model_actions.borrow().mount_for_uri(uri.uri());
                if let Some(mount) = mount {
                    let operation = gtk::MountOperation::new(Some(&sidebar_window));
                    let keep_operation = operation.clone();
                    let cancellable = gio::Cancellable::new();
                    let cancel_on_destroy = cancellable.clone();
                    let cancel_handler =
                        sidebar_window.connect_destroy(move |_| cancel_on_destroy.cancel());
                    let operation_window = sidebar_window.clone();
                    let sidebar = sidebar_holder_actions.clone();
                    mount.unmount_with_operation(
                        gio::MountUnmountFlags::NONE,
                        Some(&operation),
                        Some(&cancellable),
                        move |result| {
                            let _keep_operation_alive = &keep_operation;
                            match result {
                                Ok(()) => AccessibleStatus::announce_for(
                                    &operation_window,
                                    "Location unmounted",
                                    StatusPriority::Polite,
                                ),
                                Err(error) => AccessibleStatus::announce_for(
                                    &operation_window,
                                    unmount_failure_message(&error),
                                    StatusPriority::Urgent,
                                ),
                            }
                            operation_window.disconnect(cancel_handler);
                            if let Some(sidebar) = sidebar.borrow().as_ref() {
                                sidebar.refresh();
                            }
                        },
                    );
                }
            }
            SidebarAction::MountVolume(id) => {
                let volume = sidebar_model_actions.borrow().volume_for_id(&id);
                if let Some(volume) = volume {
                    // Parented to the window so an encrypted or password-protected
                    // device can prompt, rather than failing silently.
                    let operation = gtk::MountOperation::new(Some(&sidebar_window));
                    let keep_operation = operation.clone();
                    let cancellable = gio::Cancellable::new();
                    let cancel_on_destroy = cancellable.clone();
                    let cancel_handler =
                        sidebar_window.connect_destroy(move |_| cancel_on_destroy.cancel());
                    let operation_window = sidebar_window.clone();
                    let sidebar = sidebar_holder_actions.clone();
                    let mounted_volume = volume.clone();
                    let open_data = sidebar_context_data.clone();
                    let open_tabs = sidebar_context_tabs.clone();
                    let open_header = sidebar_context_header.clone();
                    let open_preferences = sidebar_context_preferences.clone();
                    volume.mount(
                        gio::MountMountFlags::NONE,
                        Some(&operation),
                        Some(&cancellable),
                        move |result| {
                            let _keep_operation_alive = &keep_operation;
                            match result {
                                Ok(()) => {
                                    AccessibleStatus::announce_for(
                                        &operation_window,
                                        &format!("Mounted {}", mounted_volume.name()),
                                        StatusPriority::Polite,
                                    );
                                    // Only now does the device have somewhere to
                                    // navigate to, so the open happens here rather
                                    // than when the row was activated.
                                    let landed = mounted_volume.get_mount().and_then(|mount| {
                                        Location::try_from_file(mount.root()).ok()
                                    });
                                    if let (Some(location), Some(index), Some(sidebar)) = (
                                        landed,
                                        selected_index(&open_tabs, &open_data),
                                        sidebar.borrow().clone(),
                                    ) {
                                        navigate_tab(
                                            index,
                                            location,
                                            &open_data,
                                            &open_tabs,
                                            &open_header,
                                            &open_preferences,
                                            &sidebar,
                                            true,
                                        );
                                    }
                                }
                                Err(error) => AccessibleStatus::announce_for(
                                    &operation_window,
                                    &format!(
                                        "Could not mount {}: {}",
                                        mounted_volume.name(),
                                        error.message()
                                    ),
                                    StatusPriority::Urgent,
                                ),
                            }
                            operation_window.disconnect(cancel_handler);
                            if let Some(sidebar) = sidebar.borrow().as_ref() {
                                sidebar.refresh();
                            }
                        },
                    );
                }
            }
            SidebarAction::OpenOperations => {
                open_operations_tab(&sidebar_context_tabs, sidebar_context_operations.clone())
            }
        });
        sidebar.set_active_location(
            tabs_data
                .borrow()
                .first()
                .map(|tab| tab.view.current_location())
                .unwrap_or(initial),
        );
        let mounted_tabs = Rc::downgrade(&tabs_data);
        // The sidebar's retained monitor, not a fresh `VolumeMonitor::get()`:
        // GIO finalizes the monitor when the last reference drops, so a handler
        // connected to a temporary never fires and tabs were never marked
        // unavailable when their mount went away.
        sidebar
            .volume_monitor()
            .connect_mount_removed(move |_, mount| {
                if let (Some(data), Ok(root)) = (
                    mounted_tabs.upgrade(),
                    Location::try_from_file(mount.root()),
                ) {
                    for tab in data.borrow().iter() {
                        tab.view.mark_unavailable(&root);
                    }
                }
            });
        sidebar.set_operation_manager(operations.clone());
        *sidebar_holder.borrow_mut() = Some(sidebar.clone());

        let content = Paned::new(Orientation::Horizontal);
        content.set_position(250);
        content.set_start_child(Some(&sidebar.widget));
        content.set_end_child(Some(&tabs.view));
        content.set_shrink_start_child(false);
        content.set_shrink_end_child(false);
        content.set_margin_end(content_right_inset());

        let search = SearchEntry::builder()
            .placeholder_text("Search this folder")
            .hexpand(true)
            .build();
        name_control(&search, "Search this folder (Ctrl+F)");
        let shortcut_search = search.clone();
        let status = Label::new(Some("Ready"));
        status.set_halign(Align::Start);
        status.add_css_class("dim-label");
        status.set_margin_start(8);
        status.set_margin_end(8);
        status.set_margin_bottom(6);

        let preferences_tabs = tabs.view.clone();
        let preferences_values = application_preferences.clone();
        let preferences_operations = operations.clone();
        let preferences_views = preferences.clone();
        let preferences_search_history = search_history.clone();
        let preferences_sidebar_model = sidebar_model.clone();
        let preferences_sidebar_holder = sidebar_holder.clone();
        preferences_button.connect_clicked(move |_| {
            open_preferences_tab(
                &preferences_tabs,
                preferences_values.clone(),
                preferences_operations.clone(),
                preferences_views.clone(),
                preferences_search_history.clone(),
                preferences_sidebar_model.clone(),
                preferences_sidebar_holder.clone(),
            )
        });

        let search_data = tabs_data.clone();
        let search_tabs = tabs.view.clone();
        let search_preferences = application_preferences.clone();
        let search_status = status.clone();
        // Cancel on every edit; the coordinator owns the only debounce timer.
        search.connect_changed(move |entry| {
            perform_search(
                &entry.text(),
                &search_data,
                &search_tabs,
                &search_preferences,
                &search_status,
            );
        });
        let activate_preferences = application_preferences.clone();
        let activate_history = search_history.clone();
        search.connect_activate(move |entry| {
            record_search_term(&entry.text(), &activate_preferences, &activate_history);
        });

        let body = GtkBox::new(Orientation::Vertical, 0);
        body.append(&header_controls.widget);
        body.append(&file_toolbar(
            search,
            view_controls(&tabs_data, &tabs.view, &preferences),
        ));
        body.append(&content);
        body.append(&status);
        let announcements = AccessibleStatus::new();
        announcements.widget.set_margin_start(8);
        announcements.widget.set_margin_end(8);
        announcements.watch_operations(operations.clone());
        body.append(&announcements.widget);
        let toolbar = ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&body));
        window.set_content(Some(&toolbar));
        let style = adw::StyleManager::default();
        if style.is_high_contrast() {
            window.add_css_class("high-contrast");
        }
        let style_window = window.downgrade();
        style.connect_high_contrast_notify(move |style| {
            if let Some(window) = style_window.upgrade() {
                if style.is_high_contrast() {
                    window.add_css_class("high-contrast");
                } else {
                    window.remove_css_class("high-contrast");
                }
            }
        });

        let collapsible_sidebar = sidebar.widget.clone();
        sidebar_button.update_state(&[gtk::accessible::State::Expanded(Some(
            sidebar.widget.is_visible(),
        ))]);
        sidebar_button.connect_clicked(move |button| {
            collapsible_sidebar.set_visible(!collapsible_sidebar.is_visible());
            button.update_state(&[gtk::accessible::State::Expanded(Some(
                collapsible_sidebar.is_visible(),
            ))]);
        });

        let startup_pages = tabs_data
            .borrow()
            .iter()
            .map(|tab| tab.page.clone())
            .collect::<Vec<_>>();
        for page in startup_pages {
            wire_tab(
                page,
                &tabs_data,
                &tabs.view,
                &header_controls,
                &preferences,
                app,
                &clipboard,
                &sidebar,
                &operations,
            );
        }

        let file_action_data = tabs_data.clone();
        let file_action_tabs = tabs.view.clone();
        header_controls.connect_file_action(move |action| {
            dispatch_active_file_command(
                context_command_for_file_action(action),
                &file_action_data,
                &file_action_tabs,
            );
        });

        let shortcut_data = tabs_data.clone();
        let shortcut_tabs = tabs.view.clone();
        let shortcut_preferences = preferences.clone();
        let shortcut_header = header_controls.clone();
        let shortcuts = gtk::EventControllerKey::new();
        let shortcut_new_tab = new_tab.downgrade();
        let shortcut_sidebar = sidebar.widget.clone();
        let shortcut_window = window.downgrade();
        let shortcut_navigation_sidebar = sidebar.clone();
        shortcuts.connect_key_pressed(move |_, key, _, modifiers| {
            let navigation = if key == gtk::gdk::Key::F5 {
                Some(NavigationAction::Refresh)
            } else if modifiers.contains(gtk::gdk::ModifierType::ALT_MASK) {
                match key {
                    gtk::gdk::Key::Left => Some(NavigationAction::Back),
                    gtk::gdk::Key::Right => Some(NavigationAction::Forward),
                    gtk::gdk::Key::Up => Some(NavigationAction::Up),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(action) = navigation {
                if let Some(index) = selected_index(&shortcut_tabs, &shortcut_data) {
                    handle_navigation_action(
                        index,
                        action,
                        &shortcut_data,
                        &shortcut_tabs,
                        &shortcut_header,
                        &shortcut_preferences,
                        &shortcut_navigation_sidebar,
                    );
                }
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::F2 {
                dispatch_active_file_command(
                    ContextCommand::Rename,
                    &shortcut_data,
                    &shortcut_tabs,
                );
                return glib::Propagation::Stop;
            }
            let control = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            if control {
                match key.to_unicode().map(|c| c.to_ascii_lowercase()) {
                    Some('f') => {
                        shortcut_search.grab_focus();
                        return glib::Propagation::Stop;
                    }
                    Some('t') => {
                        if let Some(button) = shortcut_new_tab.upgrade() {
                            button.emit_clicked();
                        }
                        return glib::Propagation::Stop;
                    }
                    Some('w') => {
                        if let Some(page) = shortcut_tabs.selected_page() {
                            shortcut_tabs.close_page(&page);
                        }
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
                if key == gtk::gdk::Key::Page_Down {
                    shortcut_tabs.select_next_page();
                    return glib::Propagation::Stop;
                }
                if key == gtk::gdk::Key::Page_Up {
                    shortcut_tabs.select_previous_page();
                    return glib::Propagation::Stop;
                }
            }
            if key == gtk::gdk::Key::F6 {
                let focused = shortcut_window
                    .upgrade()
                    .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w));
                let in_sidebar = focused
                    .as_ref()
                    .is_some_and(|w| w.is_ancestor(&shortcut_sidebar));
                if in_sidebar {
                    shortcut_tabs.child_focus(gtk::DirectionType::TabForward);
                } else if shortcut_sidebar.is_visible() {
                    shortcut_sidebar.child_focus(gtk::DirectionType::TabForward);
                } else {
                    shortcut_header.focus_location();
                }
                return glib::Propagation::Stop;
            }
            if key
                .to_unicode()
                .is_some_and(|key| key.eq_ignore_ascii_case(&'l'))
                && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
            {
                shortcut_header.focus_location();
                return glib::Propagation::Stop;
            }
            if key
                .to_unicode()
                .is_some_and(|key| key.eq_ignore_ascii_case(&'h'))
                && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
            {
                update_active_options(
                    &shortcut_data,
                    &shortcut_tabs,
                    &shortcut_preferences,
                    "View:Toggle hidden files",
                );
                return glib::Propagation::Stop;
            }
            let command = shortcut_command(
                key.to_unicode().map(|key| key.to_ascii_lowercase()),
                key == gtk::gdk::Key::Delete,
                modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK),
                modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK),
            );
            if let Some(command) = command {
                dispatch_active_file_command(command, &shortcut_data, &shortcut_tabs);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(shortcuts);

        let close_operations = operations.clone();
        window.connect_close_request(move |window| {
            if close_operations.active_count() == 0 {
                return glib::Propagation::Proceed;
            }
            show_close_operations_dialog(window, close_operations.clone());
            glib::Propagation::Stop
        });

        let new_data = tabs_data.clone();
        let new_tabs = tabs.view.clone();
        let new_header = header_controls.clone();
        let new_preferences = preferences.clone();
        let new_app = app.clone();
        let new_clipboard = clipboard.clone();
        let new_operations = operations.clone();
        let new_sidebar = sidebar.clone();
        let new_application_preferences = application_preferences.clone();
        new_tab.connect_clicked(move |_| {
            let path = new_tab_location(&new_application_preferences.borrow());
            let location = Rc::new(RefCell::new(path.clone()));
            let view = FileView::from_location(path.clone());
            if let Some(path) = path.local_path() {
                view.set_options(new_preferences.borrow().options_for(&path, &[]));
            }
            let page = new_tabs.append(&view.widget);
            page.set_title(&path.display_name());
            new_data.borrow_mut().push(TabData {
                view,
                location,
                page: page.clone(),
            });
            wire_tab(
                page.clone(),
                &new_data,
                &new_tabs,
                &new_header,
                &new_preferences,
                &new_app,
                &new_clipboard,
                &new_sidebar,
                &new_operations,
            );
            new_tabs.set_selected_page(&page);
        });

        let session_preferences = application_preferences.clone();
        let session_data = tabs_data.clone();
        window.connect_destroy(move |_| {
            for tab in session_data.borrow().iter() {
                tab.view.close();
            }
            let locations = session_data
                .borrow()
                .iter()
                .map(|tab| tab.view.current_location())
                .collect::<Vec<_>>();
            let _ = session_preferences.borrow().save_session(&locations);
        });

        let close_data = tabs_data.clone();
        let close_window = window.clone();
        let close_preferences = application_preferences.clone();
        tabs.view.connect_close_page(move |view, page| {
            let closing_file_tab = close_data.borrow().iter().any(|tab| tab.page == *page);
            if !closing_file_tab {
                view.close_page_finish(page, true);
                return true;
            }
            if should_protect_last_file_tab(true, close_data.borrow().len()) {
                if close_preferences.borrow().close_on_last_tab {
                    close_window.close();
                } else {
                    show_last_tab_dialog(&close_window);
                }
                return true;
            }
            if let Some(tab) = close_data.borrow().iter().find(|tab| tab.page == *page) {
                tab.view.close();
            }
            remove_matching(&close_data, |tab| tab.page == *page);
            view.close_page_finish(page, true);
            true
        });

        let tab_context_data = tabs_data.clone();
        let tab_context_view = tabs.view.clone();
        let tab_context_header = header_controls.clone();
        let tab_context_preferences = preferences.clone();
        let tab_context_app = app.clone();
        let tab_context_clipboard = clipboard.clone();
        let tab_context_operations = operations.clone();
        let tab_context_sidebar = sidebar.clone();
        tabs.set_context_handler(move |command| {
            let Some(page) = tab_context_view.selected_page() else {
                return;
            };
            match command {
                ContextCommand::CloseTab => tab_context_view.close_page(&page),
                ContextCommand::CloseOtherTabs => tab_context_view.close_other_pages(&page),
                ContextCommand::DuplicateTab => {
                    let path = tab_context_data
                        .borrow()
                        .iter()
                        .find(|tab| tab.page == page)
                        .map(|tab| tab.location.borrow().clone());
                    if let Some(path) = path {
                        add_tab_at(
                            path,
                            &tab_context_data,
                            &tab_context_view,
                            &tab_context_header,
                            &tab_context_preferences,
                            &tab_context_app,
                            &tab_context_clipboard,
                            &tab_context_sidebar,
                            &tab_context_operations,
                        );
                    }
                }
                _ => {}
            }
        });

        let select_data = tabs_data.clone();
        let select_header = header_controls.clone();
        let select_sidebar = sidebar.clone();
        tabs.view.connect_selected_page_notify(move |view| {
            if let Some(index) = selected_index(view, &select_data) {
                let tab = &select_data.borrow()[index];
                select_header.set_tab_content(&tab.view.tab_state().borrow().content);
                if let Some(title) = tab.view.section_title() {
                    select_sidebar.set_active_section(title);
                }
                select_header.set_file_action_availability(&tab.view.command_availability());
                if tab.view.section_title().is_none() {
                    select_sidebar.set_active_location(tab.view.current_location());
                }
            }
        });

        let navigation_data = tabs_data.clone();
        let navigation_tabs = tabs.view.clone();
        let navigation_header = header_controls.clone();
        let navigation_preferences = preferences.clone();
        let navigation_sidebar = sidebar.clone();
        header_controls.connect_navigation(move |action| {
            let Some(index) = selected_index(&navigation_tabs, &navigation_data) else {
                return;
            };
            handle_navigation_action(
                index,
                action,
                &navigation_data,
                &navigation_tabs,
                &navigation_header,
                &navigation_preferences,
                &navigation_sidebar,
            );
        });

        let breadcrumb_data = tabs_data.clone();
        let breadcrumb_tabs = tabs.view.clone();
        let breadcrumb_header = header_controls.clone();
        let breadcrumb_preferences = preferences.clone();
        let breadcrumb_sidebar = sidebar.clone();
        header_controls.set_navigation_handler(move |path| {
            if let Some(index) = selected_index(&breadcrumb_tabs, &breadcrumb_data) {
                navigate_tab(
                    index,
                    path,
                    &breadcrumb_data,
                    &breadcrumb_tabs,
                    &breadcrumb_header,
                    &breadcrumb_preferences,
                    &breadcrumb_sidebar,
                    true,
                );
            }
        });

        let entry_data = tabs_data.clone();
        let entry_tabs = tabs.view.clone();
        let entry_header = header_controls.clone();
        let entry_preferences = preferences.clone();
        let entry_sidebar = sidebar.clone();
        header_controls.connect_location_changed(move |path| {
            if let Some(index) = selected_index(&entry_tabs, &entry_data) {
                navigate_tab(
                    index,
                    path,
                    &entry_data,
                    &entry_tabs,
                    &entry_header,
                    &entry_preferences,
                    &entry_sidebar,
                    true,
                );
            }
        });

        Self { window }
    }
    /// The GTK toplevel used by application lifecycle coordination.
    pub fn window(&self) -> &ApplicationWindow {
        &self.window
    }

    pub fn present(&self) {
        self.window.present();
    }
}

fn show_last_tab_dialog(parent: &ApplicationWindow) {
    let dialog = adw::MessageDialog::new(
        Some(parent),
        Some("Close Zorin Explorer?"),
        Some("This is the last tab. Closing it will close Zorin Explorer."),
    );
    dialog.add_responses(&[("close", "Close Application"), ("cancel", "Cancel")]);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
    let window = parent.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "close" {
            window.close();
        }
    });
    dialog.present();
}

fn handle_navigation_action(
    index: usize,
    action: NavigationAction,
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    header: &HeaderControls,
    _preferences: &Rc<RefCell<ViewPreferencesStore>>,
    sidebar: &Sidebar,
) {
    let view = data.borrow()[index].view.clone();
    // A section remains visible until a directory load succeeds. Refresh must
    // replace that pending load before considering a section-only rebuild.
    if matches!(action, NavigationAction::Refresh) && view.has_pending_navigation() {
        view.refresh();
        return;
    }
    if let Some(section_title) = view.section_title() {
        match action {
            NavigationAction::Back => {
                if view.clear_section_overview() {
                    restore_underlying_location(index, data, tabs, header, sidebar);
                }
                return;
            }
            NavigationAction::Refresh => {
                if let Some(section) = sidebar
                    .model()
                    .borrow()
                    .sections()
                    .into_iter()
                    .find(|section| section.title == section_title)
                {
                    view.show_section_overview(section);
                }
                return;
            }
            NavigationAction::Forward | NavigationAction::Up => {}
        }
    }
    match action {
        NavigationAction::Back => view.go_back(),
        NavigationAction::Forward => view.go_forward(),
        NavigationAction::Up => {
            if let Some(parent) = view.current_location().parent() {
                view.navigate(parent);
            }
        }
        NavigationAction::Refresh => view.refresh(),
    }
}

fn new_tab_location(preferences: &ApplicationPreferences) -> Location {
    crate::core::location::LocationInput::parse(&preferences.new_tab_location)
        .unwrap_or_else(|_| Location::home())
}

fn wire_tab(
    page: TabPage,
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    header: &HeaderControls,
    preferences: &Rc<RefCell<ViewPreferencesStore>>,
    app: &Application,
    clipboard: &Rc<RefCell<Option<FileClipboard>>>,
    sidebar: &Sidebar,
    operations: &OperationManager,
) {
    let view = data
        .borrow()
        .iter()
        .find(|tab| tab.page == page)
        .map(|tab| tab.view.clone())
        .expect("a tab page should have corresponding tab data");
    let close_tabs = tabs.clone();
    let close_page = page.clone();
    view.set_close_handler(move || close_tabs.close_page(&close_page));
    let committed_data = Rc::downgrade(data);
    let committed_page = page.clone();
    let committed_tabs = tabs.clone();
    let committed_header = header.clone();
    let committed_sidebar = sidebar.clone();
    let committed_preferences = preferences.clone();
    view.set_location_committed_handler(move |location| {
        let Some(data) = committed_data.upgrade() else {
            return;
        };
        let view = data
            .borrow()
            .iter()
            .find(|tab| tab.page == committed_page)
            .map(|tab| tab.view.clone());
        let Some(view) = view else {
            return;
        };
        if let Some(tab) = data.borrow().iter().find(|tab| tab.page == committed_page) {
            *tab.location.borrow_mut() = location.clone();
        }
        if let Some(path) = location.local_path() {
            view.set_options(committed_preferences.borrow().options_for(&path, &[]));
        }
        crate::ui::tabs::set_location_title(&committed_page, &location);
        if committed_tabs
            .selected_page()
            .is_some_and(|selected| selected == committed_page)
        {
            if let Some(path) = location.local_path() {
                committed_header.set_location(&path);
                committed_sidebar.set_active_path(&path);
            } else {
                committed_header.set_uri(location.uri());
                committed_sidebar.set_active_location(location.clone());
            }
        }
    });
    let quick_access_model = sidebar.model();
    view.set_quick_access_predicate(move |path| {
        quick_access_model
            .borrow()
            .preferences()
            .locations()
            .any(|pin| pin == path)
    });
    let availability_tabs = tabs.clone();
    let availability_page = page.clone();
    let availability_header = header.clone();
    view.set_command_availability_handler(move |availability| {
        if availability_tabs
            .selected_page()
            .is_some_and(|selected| selected == availability_page)
        {
            availability_header.set_file_action_availability(&availability);
        }
    });
    let options_data = data.clone();
    let options_preferences = preferences.clone();
    let options_page = page.clone();
    view.set_options_changed_handler(move |options| {
        let path = options_data
            .borrow()
            .iter()
            .find(|tab| tab.page == options_page)
            .and_then(|tab| tab.view.current_location().local_path());
        if let Some(path) = path {
            persist_options(&mut options_preferences.borrow_mut(), &path, options);
        }
    });
    let navigation_data = data.clone();
    let navigation_tabs = tabs.clone();
    let navigation_header = header.clone();
    let navigation_preferences = preferences.clone();
    let navigation_sidebar = sidebar.clone();
    let navigation_page = page.clone();
    view.set_navigation_handler(move |path| {
        let index = {
            navigation_data
                .borrow()
                .iter()
                .position(|tab| tab.page == navigation_page)
        };
        if let Some(index) = index {
            navigate_tab(
                index,
                path,
                &navigation_data,
                &navigation_tabs,
                &navigation_header,
                &navigation_preferences,
                &navigation_sidebar,
                true,
            );
        }
    });

    let command_data = data.clone();
    let command_tabs = tabs.clone();
    let command_header = header.clone();
    let command_preferences = preferences.clone();
    let command_app = app.clone();
    let command_clipboard = clipboard.clone();
    let command_sidebar = sidebar.clone();
    let command_operations = operations.clone();
    let command_view = view.clone();
    view.set_application_handler(move |intent| {
        let command = intent.kind;
        let target = command_view
            .selected_target()
            .unwrap_or_else(|| ContextTarget::Background(intent.destination.clone()));
        let path = target_path(&target);
        let clipboard_payload = clipboard_payload(
            &command_clipboard,
            &command_view.widget.display().clipboard(),
        );
        match command {
            ContextCommand::OpenWith => {
                if let Some(application) = intent.application {
                    let files = intent
                        .selection
                        .iter()
                        .map(|l| gio::File::for_uri(l.uri()))
                        .collect::<Vec<_>>();
                    if let Err(error) = application.launch(&files, None::<&gio::AppLaunchContext>) {
                        show_operation_error(
                            &command_view,
                            crate::core::contracts::OperationError::from_gio(
                                &error,
                                intent.selection.first(),
                                None,
                            )
                            .message_key,
                        );
                    }
                }
            }
            ContextCommand::Open => {
                for item in command_view.selected_items() {
                    command_view.open_item(&item);
                }
            }
            ContextCommand::OpenInNewTab => {
                if let Some(path) = target_location(&target) {
                    add_tab_at(
                        path,
                        &command_data,
                        &command_tabs,
                        &command_header,
                        &command_preferences,
                        &command_app,
                        &command_clipboard,
                        &command_sidebar,
                        &command_operations,
                    );
                }
            }
            ContextCommand::OpenInNewWindow => {
                if let Some(path) = target_location(&target) {
                    ExplorerWindow::new_at(&command_app, path, false).present();
                }
            }
            ContextCommand::OpenInTerminal => {
                if let Some(path) = path {
                    open_terminal(&path);
                }
            }
            ContextCommand::Copy | ContextCommand::Cut => {
                if !intent.selection.is_empty() {
                    if let Some(previous) = command_clipboard.borrow_mut().take() {
                        previous.source_view.set_pending_cut_locations(vec![]);
                    }
                    let payload = ClipboardPayload {
                        action: if command == ContextCommand::Copy {
                            ClipboardAction::Copy
                        } else {
                            ClipboardAction::Cut
                        },
                        locations: intent.selection,
                    };
                    if payload.action == ClipboardAction::Cut {
                        command_view.set_pending_cut_locations(payload.locations.clone());
                    }
                    let files = gtk::gdk::FileList::from_array(
                        &payload
                            .locations
                            .iter()
                            .map(|l| gio::File::for_uri(l.uri()))
                            .collect::<Vec<_>>(),
                    );
                    let provider = gtk::gdk::ContentProvider::new_union(&[
                        gtk::gdk::ContentProvider::for_value(&files.to_value()),
                        gtk::gdk::ContentProvider::for_bytes(
                            "text/uri-list",
                            &glib::Bytes::from_owned(payload.uri_list().into_bytes()),
                        ),
                        gtk::gdk::ContentProvider::for_bytes(
                            "x-special/gnome-copied-files",
                            &glib::Bytes::from_owned(
                                format!(
                                    "{}\n{}",
                                    if payload.action == ClipboardAction::Cut {
                                        "cut"
                                    } else {
                                        "copy"
                                    },
                                    payload.uri_list()
                                )
                                .into_bytes(),
                            ),
                        ),
                    ]);
                    let _ = command_view
                        .widget
                        .display()
                        .clipboard()
                        .set_content(Some(&provider));
                    let mut state = ClipboardState::default();
                    state.replace(payload);
                    *command_clipboard.borrow_mut() = Some(FileClipboard {
                        state,
                        source_view: command_view.clone(),
                        provider,
                    });
                }
            }
            ContextCommand::CopyFilename => {
                copy_text(
                    &command_view,
                    &command_view
                        .selected_items()
                        .iter()
                        .map(|i| i.name().to_owned())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            ContextCommand::CopyFullPath => {
                copy_text(
                    &command_view,
                    &intent
                        .selection
                        .iter()
                        .map(Location::uri)
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            ContextCommand::CopyTo | ContextCommand::MoveTo => {
                if !intent.selection.is_empty() {
                    choose_destination(
                        &command_view,
                        intent.selection,
                        command == ContextCommand::MoveTo,
                        command_operations.clone(),
                        command_sidebar.model(),
                    );
                }
            }
            ContextCommand::Paste => {
                if let Some(action) = intent.transfer {
                    start_uri_transfer_with_completion(
                        &command_view,
                        ClipboardPayload {
                            action: if intent.source_deletes_on_success {
                                ClipboardAction::Copy
                            } else {
                                action
                            },
                            locations: intent.selection,
                        },
                        intent.destination,
                        command_operations.clone(),
                        None,
                        intent.completion,
                    );
                } else if let Some(payload) = clipboard_payload {
                    start_uri_transfer(
                        &command_view,
                        payload,
                        intent.destination,
                        command_operations.clone(),
                        Some(command_clipboard.clone()),
                    );
                } else {
                    let view = command_view.clone();
                    let operations = command_operations.clone();
                    read_uri_clipboard(command_view.widget.display().clipboard(), move |payload| {
                        if let Some(payload) = payload {
                            start_uri_transfer(
                                &view,
                                payload,
                                intent.destination,
                                operations,
                                None,
                            );
                        }
                    });
                }
            }
            ContextCommand::NewFolder => {
                let parent = intent.destination;
                let refresh = command_view.clone();
                let submit_operations = command_operations.clone();
                let refresh_operations = command_operations.clone();
                show_name_dialog(
                    &command_view,
                    "New Folder",
                    "Folder name",
                    "New Folder",
                    "Create",
                    move |name| {
                        let target = crate::application::commands::named_child(&parent, &name)?;
                        Ok(submit_operations.submit_locations(
                            MutationKind::Create,
                            vec![target],
                            None,
                            None,
                            None,
                            false,
                        ))
                    },
                    move |id| schedule_refresh(refresh.clone(), refresh_operations.clone(), id),
                );
            }
            ContextCommand::Rename => {
                if let Some(source) = intent.selection.first().cloned() {
                    let initial = command_view
                        .selected_items()
                        .first()
                        .map(|i| i.name().to_owned())
                        .unwrap_or_default();
                    let refresh = command_view.clone();
                    let submit_operations = command_operations.clone();
                    let refresh_operations = command_operations.clone();
                    show_name_dialog(
                        &command_view,
                        "Rename",
                        "New name",
                        &initial,
                        "Rename",
                        move |name| {
                            let parent = source.parent().ok_or_else(|| {
                                std::io::Error::new(
                                    std::io::ErrorKind::InvalidInput,
                                    "This location has no parent",
                                )
                            })?;
                            let target = crate::application::commands::named_child(&parent, &name)?;
                            Ok(submit_operations.submit_locations(
                                MutationKind::Rename,
                                vec![source.clone()],
                                Some(parent),
                                Some(target),
                                None,
                                false,
                            ))
                        },
                        move |id| schedule_refresh(refresh.clone(), refresh_operations.clone(), id),
                    );
                }
            }
            ContextCommand::MoveToTrash => {
                if !intent.selection.is_empty() {
                    let operation_id = command_operations.submit_locations(
                        MutationKind::Trash,
                        intent.selection,
                        None,
                        None,
                        None,
                        false,
                    );
                    schedule_refresh(
                        command_view.clone(),
                        command_operations.clone(),
                        operation_id,
                    );
                }
            }
            ContextCommand::DeletePermanently => {
                if !intent.selection.is_empty() {
                    confirm_permanent_delete(
                        &command_view,
                        intent.selection,
                        command_operations.clone(),
                    );
                }
            }
            ContextCommand::Properties => {
                if let Some(location) = intent.selection.first() {
                    let refresh = command_view.clone();
                    PropertiesDialog::present(
                        command_view.widget.root().and_downcast_ref::<gtk::Window>(),
                        location.clone(),
                        move || refresh.refresh(),
                    );
                }
            }
            ContextCommand::ToggleQuickAccess => {
                command_sidebar.toggle_quick_access_location(
                    intent
                        .selection
                        .first()
                        .unwrap_or(&intent.destination)
                        .clone(),
                );
            }
            ContextCommand::Refresh => command_view.refresh(),
            _ => {}
        }
    });
}

#[allow(deprecated)]
fn show_name_dialog<F, S>(
    view: &FileView,
    title: &str,
    prompt: &str,
    initial: &str,
    accept_label: &str,
    action: F,
    on_success: S,
) where
    F: Fn(String) -> std::io::Result<u64> + 'static,
    S: Fn(u64) + 'static,
{
    let dialog = gtk::Dialog::new();
    dialog.set_title(Some(title));
    dialog.set_modal(true);
    dialog.set_resizable(true);
    dialog.add_css_class("explorer-dialog");
    let titlebar = HeaderBar::new();
    titlebar.set_show_start_title_buttons(false);
    titlebar.set_show_end_title_buttons(false);
    titlebar.set_title_widget(Some(&Label::new(Some(title))));
    dialog.set_titlebar(Some(&titlebar));
    dialog.set_default_size(420, 180);
    if let Some(window) = view.widget.root().and_downcast::<gtk::Window>() {
        dialog.set_transient_for(Some(&window));
    }
    dialog.add_button(accept_label, gtk::ResponseType::Accept);
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    let content = dialog.content_area();
    content.set_spacing(8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&Label::new(Some(prompt)));
    let entry = Entry::new();
    entry.set_text(initial);
    entry.set_activates_default(true);
    dialog.set_default_response(gtk::ResponseType::Accept);
    content.append(&entry);
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            let name = entry.text().trim().to_owned();
            if !name.is_empty() {
                if let Ok(id) = action(name) {
                    on_success(id);
                }
            }
        }
        dialog.close();
    });
    dialog.present();
}

fn target_path(target: &ContextTarget) -> Option<PathBuf> {
    match target {
        ContextTarget::File(item) | ContextTarget::Folder(item) => item.local_path(),
        ContextTarget::SidebarLocation(location) | ContextTarget::Background(location) => {
            location.local_path()
        }
        ContextTarget::Tab => None,
    }
}

fn target_location(target: &ContextTarget) -> Option<Location> {
    match target {
        ContextTarget::File(item) | ContextTarget::Folder(item) => Some(item.location().clone()),
        ContextTarget::SidebarLocation(location) | ContextTarget::Background(location) => {
            Some(location.clone())
        }
        ContextTarget::Tab => None,
    }
}

fn context_command_for_file_action(action: FileAction) -> ContextCommand {
    match action {
        FileAction::Cut => ContextCommand::Cut,
        FileAction::Copy => ContextCommand::Copy,
        FileAction::Paste => ContextCommand::Paste,
        FileAction::MoveToTrash => ContextCommand::MoveToTrash,
    }
}

fn shortcut_command(
    key: Option<char>,
    delete: bool,
    control: bool,
    shift: bool,
) -> Option<ContextCommand> {
    if delete {
        return Some(if shift {
            route(Shortcut::DeletePermanently)
        } else {
            route(Shortcut::Trash)
        });
    }
    control
        .then(|| match key {
            Some('x') => Some(route(Shortcut::Cut)),
            Some('c') => Some(route(Shortcut::Copy)),
            Some('v') => Some(route(Shortcut::Paste)),
            _ => None,
        })
        .flatten()
}

fn dispatch_active_file_command(
    command: ContextCommand,
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
) {
    let Some(index) = selected_index(tabs, data) else {
        return;
    };
    let view = data.borrow()[index].view.clone();
    let target = if command == ContextCommand::Paste {
        Some(ContextTarget::Background(view.current_location()))
    } else {
        view.selected_target()
    };
    if let Some(target) = target {
        view.dispatch_context_command(target, command);
    }
}

fn add_tab_at(
    path: Location,
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    header: &HeaderControls,
    preferences: &Rc<RefCell<ViewPreferencesStore>>,
    app: &Application,
    clipboard: &Rc<RefCell<Option<FileClipboard>>>,
    sidebar: &Sidebar,
    operations: &OperationManager,
) {
    let location = Rc::new(RefCell::new(path.clone()));
    let view = FileView::from_location(path.clone());
    if let Some(path) = path.local_path() {
        view.set_options(preferences.borrow().options_for(&path, &[]));
    }
    let page = tabs.append(&view.widget);
    page.set_title(&path.display_name());
    data.borrow_mut().push(TabData {
        view,
        location,
        page: page.clone(),
    });
    wire_tab(
        page.clone(),
        data,
        tabs,
        header,
        preferences,
        app,
        clipboard,
        sidebar,
        operations,
    );
    tabs.set_selected_page(&page);
}

fn open_operations_tab(tabs: &TabView, manager: OperationManager) {
    if let Some(page) = tabs
        .pages()
        .iter::<TabPage>()
        .filter_map(Result::ok)
        .find(|page| page.title().as_str() == "Operations")
    {
        tabs.set_selected_page(&page);
        return;
    }
    let page = tabs.append(&operations_page::build(manager));
    page.set_title("Operations");
    tabs.set_selected_page(&page);
}

fn open_preferences_tab(
    tabs: &TabView,
    preferences: Rc<RefCell<ApplicationPreferences>>,
    operations: OperationManager,
    views: Rc<RefCell<ViewPreferencesStore>>,
    search_history: Rc<RefCell<SearchHistory>>,
    sidebar_model: Rc<RefCell<SidebarModel>>,
    sidebar_holder: Rc<RefCell<Option<Sidebar>>>,
) {
    if let Some(page) = tabs
        .pages()
        .iter::<TabPage>()
        .filter_map(Result::ok)
        .find(|page| page.title().as_str() == "Preferences")
    {
        tabs.set_selected_page(&page);
        return;
    }
    let page = tabs.append(&preferences_page::build(
        preferences,
        operations,
        views,
        search_history,
        sidebar_model,
        sidebar_holder,
    ));
    page.set_title("Preferences");
    tabs.set_selected_page(&page);
}

/// Runs a search against the active tab's folder and swaps its listing for
/// the results. An empty query returns the view to its normal folder listing.
fn perform_search(
    term: &str,
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    preferences: &Rc<RefCell<ApplicationPreferences>>,
    status: &Label,
) {
    let Some(index) = selected_index(tabs, data) else {
        return;
    };
    let view = {
        let data = data.borrow();
        let tab = &data[index];
        tab.view.clone()
    };
    if term.trim().is_empty() {
        view.clear_search_results();
        status.set_text("Ready");
        return;
    }
    let content_search = preferences.borrow().content_search_enabled;
    let started = view.search(
        term,
        if content_search {
            crate::core::contracts::SearchMode::Content
        } else {
            crate::core::contracts::SearchMode::Filename
        },
        true,
    );
    status.set_text(if started {
        "Search results and scope are shown in the active tab"
    } else {
        "Search is available when the folder has finished loading"
    });
}

/// Recorded only when a search is committed with Enter, not on every
/// keystroke, and only while the user has recent-search recording enabled.
fn record_search_term(
    term: &str,
    preferences: &Rc<RefCell<ApplicationPreferences>>,
    history: &Rc<RefCell<SearchHistory>>,
) {
    let preferences = preferences.borrow();
    let mut history = history.borrow_mut();
    let _ = history.record_submitted(
        term,
        preferences.record_search_history,
        preferences.search_history_retention_days,
    );
}

fn copy_text(view: &FileView, text: &str) {
    view.widget.display().clipboard().set_text(text);
}

fn open_terminal(path: &Path) {
    for terminal in ["kgx", "gnome-terminal", "x-terminal-emulator"] {
        if std::process::Command::new(terminal)
            .current_dir(path)
            .spawn()
            .is_ok()
        {
            break;
        }
    }
}

#[allow(deprecated)]
fn show_network_location_dialog(
    parent: &ApplicationWindow,
    model: Rc<RefCell<SidebarModel>>,
    sidebar: Rc<RefCell<Option<Sidebar>>>,
) {
    let dialog = gtk::Dialog::new();
    dialog.set_title(Some("Add Network Location"));
    dialog.set_modal(true);
    dialog.add_css_class("explorer-dialog");
    dialog.set_transient_for(Some(parent));
    dialog.add_button("Add", gtk::ResponseType::Accept);
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.set_default_response(gtk::ResponseType::Accept);
    let content = dialog.content_area();
    content.set_spacing(8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let prompt = Label::new(Some("Network address (for example smb://nas.local/share)"));
    prompt.set_xalign(0.0);
    content.append(&prompt);
    let entry = Entry::new();
    entry.set_placeholder_text(Some("smb://server/share"));
    entry.set_activates_default(true);
    content.append(&entry);
    let error = Label::new(None);
    error.add_css_class("error");
    error.set_xalign(0.0);
    error.set_wrap(true);
    content.append(&error);
    dialog.connect_response(move |dialog, response| {
        if response != gtk::ResponseType::Accept {
            dialog.close();
            return;
        }
        match model
            .borrow_mut()
            .preferences_mut()
            .pin_remote(entry.text().to_string())
        {
            Ok(()) => {
                let _ = model.borrow().preferences().save();
                if let Some(sidebar) = sidebar.borrow().as_ref() {
                    sidebar.refresh();
                }
                dialog.close();
            }
            Err(message) => error.set_text(&message),
        }
    });
    dialog.present();
}

fn read_uri_clipboard(
    clipboard: gtk::gdk::Clipboard,
    done: impl FnOnce(Option<ClipboardPayload>) + 'static,
) {
    glib::MainContext::default().spawn_local(async move {
        let Ok((stream, mime)) = clipboard
            .read_future(
                &["x-special/gnome-copied-files", "text/uri-list"],
                glib::Priority::DEFAULT,
            )
            .await
        else {
            done(None);
            return;
        };
        let mut bytes = Vec::new();
        loop {
            let Ok(chunk) = stream
                .read_bytes_future(8192, glib::Priority::DEFAULT)
                .await
            else {
                done(None);
                return;
            };
            if chunk.is_empty() {
                break;
            }
            bytes.extend_from_slice(chunk.as_ref());
            if bytes.len() > 1024 * 1024 {
                done(None);
                return;
            }
        }
        let payload = std::str::from_utf8(&bytes).ok().and_then(|text| {
            if mime == "x-special/gnome-copied-files" {
                ClipboardPayload::parse_gnome(text).ok()
            } else {
                ClipboardPayload::parse(ClipboardAction::Copy, text).ok()
            }
        });
        done(payload);
    });
}

fn choose_destination(
    view: &FileView,
    sources: Vec<Location>,
    move_source: bool,
    operations: OperationManager,
    sidebar_model: Rc<RefCell<SidebarModel>>,
) {
    let reload = view.clone();
    crate::ui::folder_picker::pick_folder(
        &view.widget,
        if move_source { "Move To" } else { "Copy To" },
        if move_source { "Move" } else { "Copy" },
        view.current_location(),
        sidebar_model,
        operations.clone(),
        move |destination| {
            start_uri_transfer(
                &reload,
                ClipboardPayload {
                    action: if move_source {
                        ClipboardAction::Cut
                    } else {
                        ClipboardAction::Copy
                    },
                    locations: sources.clone(),
                },
                destination,
                operations.clone(),
                None,
            )
        },
    );
}

/// Each item uses the same coordinator and conflict decisions. Continue only
/// after a terminal acknowledgement; a failure never drops unattempted items.
fn start_uri_transfer(
    view: &FileView,
    payload: ClipboardPayload,
    destination: Location,
    operations: OperationManager,
    clipboard: Option<Rc<RefCell<Option<FileClipboard>>>>,
) {
    start_uri_transfer_with_completion(view, payload, destination, operations, clipboard, None);
}

#[derive(Clone, Default)]
struct TransferBatch {
    completion: Option<TransferCompletion>,
    failed: Rc<std::cell::Cell<bool>>,
    conflict_default: ConflictDefault,
}
impl TransferBatch {
    fn finish(&self) {
        if let Some(done) = &self.completion {
            done.complete(!self.failed.get());
        }
    }
}

fn start_uri_transfer_with_completion(
    view: &FileView,
    payload: ClipboardPayload,
    destination: Location,
    operations: OperationManager,
    clipboard: Option<Rc<RefCell<Option<FileClipboard>>>>,
    completion: Option<TransferCompletion>,
) {
    let clipboard = clipboard.and_then(|owner| {
        let generation = owner.borrow().as_ref().map(|c| c.state.generation())?;
        Some(ClipboardLease { owner, generation })
    });
    transfer_next(
        view.clone(),
        payload,
        destination,
        operations,
        clipboard,
        0,
        None,
        TransferBatch {
            completion,
            conflict_default: ApplicationPreferences::load_default()
                .unwrap_or_default()
                .conflict_default,
            ..Default::default()
        },
    );
}

fn transfer_next(
    view: FileView,
    payload: ClipboardPayload,
    destination: Location,
    operations: OperationManager,
    clipboard: Option<ClipboardLease>,
    index: usize,
    decision: Option<ConflictDecision>,
    batch: TransferBatch,
) {
    let Some(source) = payload.locations.get(index).cloned() else {
        view.refresh();
        batch.failed.set(true);
        batch.finish();
        return;
    };
    let kind = if payload.action == ClipboardAction::Cut {
        MutationKind::Move
    } else {
        MutationKind::Copy
    };
    let id = operations.submit_locations(
        kind,
        if decision == Some(ConflictDecision::CancelRemaining) {
            payload.locations[index..].to_vec()
        } else {
            vec![source]
        },
        Some(destination.clone()),
        None,
        decision,
        false,
    );
    if let Some(state) = &clipboard {
        if let Some(current) = state.owner.borrow_mut().as_mut() {
            if current.state.generation() == state.generation {
                current.state.begin_paste(id);
            }
        }
    }
    glib::timeout_add_local(std::time::Duration::from_millis(40), move || {
        let Some(record) = operations.records().into_iter().find(|r| r.id == id) else {
            // History can be cleared between completion and this projection.
            // With no terminal evidence, never acknowledge an external MOVE.
            batch.failed.set(true);
            batch.finish();
            finish_cut(&clipboard, id, &view);
            return glib::ControlFlow::Break;
        };
        if matches!(
            record.state,
            crate::core::OperationState::Queued
                | crate::core::OperationState::Running
                | crate::core::OperationState::CancelRequested
        ) {
            return glib::ControlFlow::Continue;
        }
        let conflict = operations
            .summary(id)
            .and_then(|s| s.error_kind)
            .is_some_and(|kind| kind == "Conflict");
        if conflict && decision.is_none() {
            show_transfer_conflict(
                view.clone(),
                payload.clone(),
                destination.clone(),
                operations.clone(),
                clipboard.clone(),
                index,
                batch.clone(),
            );
            return glib::ControlFlow::Break;
        }
        if let crate::core::OperationState::Failed(error) = &record.state {
            show_operation_error(&view, error);
        }
        if record.state != crate::core::OperationState::Completed
            || decision == Some(ConflictDecision::Skip)
            || !operations
                .summary(id)
                .is_some_and(|s| !s.completed.is_empty() && s.remaining.is_empty())
        {
            batch.failed.set(true);
        }
        if record.state == crate::core::OperationState::Cancelled
            && decision != Some(ConflictDecision::CancelRemaining)
            && index + 1 < payload.locations.len()
        {
            transfer_next(
                view.clone(),
                payload.clone(),
                destination.clone(),
                operations.clone(),
                clipboard.clone(),
                index + 1,
                Some(ConflictDecision::CancelRemaining),
                batch.clone(),
            );
            return glib::ControlFlow::Break;
        }
        let last = index + 1 == payload.locations.len()
            || matches!(record.state, crate::core::OperationState::Cancelled);
        if last {
            finish_cut(&clipboard, id, &view);
            batch.finish();
        } else {
            transfer_next(
                view.clone(),
                payload.clone(),
                destination.clone(),
                operations.clone(),
                clipboard.clone(),
                index + 1,
                None,
                batch.clone(),
            );
        }
        view.refresh();
        glib::ControlFlow::Break
    });
}

fn finish_cut(clipboard: &Option<ClipboardLease>, id: u64, view: &FileView) {
    if let Some(state) = clipboard {
        let generation = state.generation;
        let mut state = state.owner.borrow_mut();
        if let Some(current) = state.as_mut() {
            if current.state.generation() != generation {
                return;
            }
            current.state.finish_paste(id, true);
            if current.state.payload().is_none() {
                current.source_view.set_pending_cut_locations(vec![]);
                current.source_view.refresh();
                if view.widget.display().clipboard().content().as_ref() == Some(&current.provider) {
                    let _ = view
                        .widget
                        .display()
                        .clipboard()
                        .set_content(None::<&gtk::gdk::ContentProvider>);
                }
                *state = None;
            }
        }
    }
}

#[allow(deprecated)]
fn show_transfer_conflict(
    view: FileView,
    payload: ClipboardPayload,
    destination: Location,
    operations: OperationManager,
    clipboard: Option<ClipboardLease>,
    index: usize,
    batch: TransferBatch,
) {
    let choice = match batch.conflict_default {
        ConflictDefault::Replace => Some(ConflictDecision::Replace),
        ConflictDefault::KeepBoth => Some(ConflictDecision::KeepBoth),
        ConflictDefault::Skip => Some(ConflictDecision::Skip),
        ConflictDefault::Ask => None,
    };
    if let Some(choice) = choice {
        transfer_next(
            view,
            payload,
            destination,
            operations,
            clipboard,
            index,
            Some(choice),
            batch,
        );
        return;
    }
    let parent = view.widget.root().and_downcast::<gtk::Window>();
    let dialog = adw::MessageDialog::new(
        parent.as_ref(),
        Some("An item with this name already exists"),
        Some("Choose how to handle this item. Remaining items will continue in order."),
    );
    dialog.add_responses(&[
        ("replace", "Replace"),
        ("keep", "Keep Both"),
        ("skip", "Skip"),
        ("cancel", "Cancel Remaining"),
    ]);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| {
        let decision = match response {
            "replace" => ConflictDecision::Replace,
            "keep" => ConflictDecision::KeepBoth,
            "skip" => ConflictDecision::Skip,
            _ => ConflictDecision::CancelRemaining,
        };
        view.widget.grab_focus();
        transfer_next(
            view.clone(),
            payload.clone(),
            destination.clone(),
            operations.clone(),
            clipboard.clone(),
            index,
            Some(decision),
            batch.clone(),
        );
    });
    dialog.present();
}

/// Waits for this specific operation to finish, not every operation in the
/// manager: gating on the global active count meant one stuck or
/// long-running operation anywhere silently blocked every other
/// operation's post-completion refresh, including on unrelated tabs.
fn schedule_refresh(view: FileView, operations: OperationManager, operation_id: u64) {
    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        let Some(record) = operations
            .records()
            .into_iter()
            .find(|record| record.id == operation_id)
        else {
            return glib::ControlFlow::Break;
        };
        if matches!(
            record.state,
            crate::core::OperationState::Queued
                | crate::core::OperationState::Running
                | crate::core::OperationState::CancelRequested
        ) {
            return glib::ControlFlow::Continue;
        }
        view.refresh();
        if let crate::core::OperationState::Failed(error) = record.state {
            show_operation_error(&view, &error);
        }
        glib::ControlFlow::Break
    });
}

fn show_operation_error(view: &FileView, error: &str) {
    AccessibleStatus::announce_for(&view.widget, error, StatusPriority::Urgent);
    let parent = view.widget.root().and_downcast::<gtk::Window>();
    let dialog = adw::MessageDialog::new(
        parent.as_ref(),
        Some("File operation could not be completed"),
        Some(error),
    );
    dialog.add_response("ok", "OK");
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("ok");
    dialog.present();
}

#[allow(deprecated)]
pub fn confirm_permanent_delete(
    view: &FileView,
    locations: Vec<Location>,
    operations: OperationManager,
) -> adw::MessageDialog {
    AccessibleStatus::announce_for(
        &view.widget,
        "Confirm permanent deletion. This cannot be undone. Cancel is the default.",
        StatusPriority::Urgent,
    );
    let parent = view.widget.root().and_downcast::<gtk::Window>();
    let dialog = adw::MessageDialog::new(
        parent.as_ref(),
        Some("Delete permanently?"),
        Some(&format!(
            "{} selected item(s) will be permanently deleted. This cannot be undone.",
            locations.len()
        )),
    );
    dialog.add_responses(&[("delete", "Delete Permanently"), ("cancel", "Cancel")]);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    let reload = view.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "delete" {
            let operation_id = operations.submit_locations(
                MutationKind::Delete,
                locations.clone(),
                None,
                None,
                None,
                true,
            );
            schedule_refresh(reload.clone(), operations.clone(), operation_id);
        }
        reload.widget.grab_focus();
    });
    dialog.present();
    dialog
}

fn show_close_operations_dialog(parent: &ApplicationWindow, operations: OperationManager) {
    let dialog = adw::MessageDialog::new(Some(parent), Some("File operations are still running"), Some(&format!("{} operation(s) are still running. Closing now will stop them and may leave partial files behind.", operations.active_count())));
    dialog.add_responses(&[("wait", "Keep Waiting"), ("close", "Cancel All & Close")]);
    dialog.set_default_response(Some("wait"));
    dialog.set_close_response("wait");
    dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
    let window = parent.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "close" {
            operations.cancel_all_active();
            if let Some(application) = window.application() {
                application.activate_action("quit", None);
            }
        }
    });
    dialog.present();
}

fn navigate_tab(
    index: usize,
    path: Location,
    data: &Rc<RefCell<Vec<TabData>>>,
    _tabs: &TabView,
    _header: &HeaderControls,
    _preferences: &Rc<RefCell<ViewPreferencesStore>>,
    _sidebar: &Sidebar,
    _record_history: bool,
) {
    if let Some(tab) = data.borrow().get(index) {
        tab.view.navigate(path);
    }
}

/// Reinstates the URI location hidden behind a virtual sidebar overview.
fn restore_underlying_location(
    index: usize,
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    header: &HeaderControls,
    sidebar: &Sidebar,
) {
    let data_ref = data.borrow();
    let Some(tab) = data_ref.get(index) else {
        return;
    };
    let location = tab.view.current_location();
    tab.page.set_title(&location.display_name());
    if tabs
        .selected_page()
        .as_ref()
        .is_some_and(|page| *page == tab.page)
    {
        header.set_uri(location.uri());
        sidebar.set_active_location(location);
    }
}

#[cfg(test)]
fn sidebar_path_for_tab(path: &Path, is_remote: bool) -> Option<PathBuf> {
    (!is_remote).then(|| path.to_path_buf())
}

/// Brings a file tab to the front, so a sidebar action taken while Preferences
/// or Operations is open shows its result rather than changing a hidden tab.
fn show_file_tab(tabs: &TabView, data: &Rc<RefCell<Vec<TabData>>>, index: usize) {
    let page = data.borrow().get(index).map(|tab| tab.page.clone());
    if let Some(page) = page {
        if tabs.selected_page().as_ref() != Some(&page) {
            tabs.set_selected_page(&page);
        }
    }
}

fn selected_index(tabs: &TabView, data: &Rc<RefCell<Vec<TabData>>>) -> Option<usize> {
    let page = tabs.selected_page()?;
    matching_index(&data.borrow(), |tab| tab.page == page)
}

/// Which file tab a sidebar action should act on.
///
/// Preferences and Operations are appended to the tab view but are not file
/// tabs, so nothing in `tabs_data` matches the selected page while one of them
/// is open. Every sidebar action was guarded on that lookup and so did nothing
/// at all, silently, whenever Preferences had focus. Falling back to the first
/// file tab is what clicking a location is asking for.
fn sidebar_target(selected: Option<usize>, file_tabs: usize) -> Option<usize> {
    match selected {
        Some(index) => Some(index),
        None if file_tabs > 0 => Some(0),
        None => None,
    }
}

fn should_protect_last_file_tab(closing_file_tab: bool, file_tab_count: usize) -> bool {
    closing_file_tab && file_tab_count <= 1
}

fn matching_index<T>(items: &[T], matches: impl Fn(&T) -> bool) -> Option<usize> {
    items.iter().position(matches)
}

fn remove_matching<T>(items: &RefCell<Vec<T>>, matches: impl Fn(&T) -> bool) -> bool {
    let index = { items.borrow().iter().position(matches) };
    if let Some(index) = index {
        items.borrow_mut().remove(index);
        true
    } else {
        false
    }
}

fn content_right_inset() -> i32 {
    8
}

fn apply_tab_layout(window: &ApplicationWindow, tabs: &TabStrip) {
    let window_width = window.width();
    let layout = calculate_tab_layout(TabLayoutInput::new(
        tabs.view.n_pages(),
        if window_width > 0 { window_width } else { 1280 },
    ));

    tabs.set_layout(layout);
}

fn view_controls(
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    preferences: &Rc<RefCell<ViewPreferencesStore>>,
) -> GtkBox {
    let controls = GtkBox::new(Orientation::Horizontal, 6);
    for label in ["Sort", "Group", "View"] {
        let options = selected_index(tabs, data)
            .map(|index| data.borrow()[index].view.options())
            .unwrap_or_default();
        let initial_body = view_menu_body(label, &options, data, tabs, preferences);
        let button = menu_button(label, &initial_body);
        let popover = button
            .popover()
            .expect("menu buttons always have a popover");
        let data = data.clone();
        let tabs = tabs.clone();
        let preferences = preferences.clone();
        let label = label.to_string();
        // A tab can change its folder while this toolbar remains alive. Refresh
        // immediately before the popover is shown so its checks mirror the
        // selected folder, rather than the folder that existed at construction.
        popover.connect_show(move |popover| {
            let options = selected_index(&tabs, &data)
                .map(|index| data.borrow()[index].view.options())
                .unwrap_or_default();
            popover.set_child(Some(&view_menu_body(
                &label,
                &options,
                &data,
                &tabs,
                &preferences,
            )));
        });
        controls.append(&button);
    }
    controls
}

fn menu_button(label: &str, child: &impl IsA<gtk::Widget>) -> MenuButton {
    let button = MenuButton::builder().label(label).build();
    let popover = Popover::new();
    popover.set_child(Some(child));
    button.set_popover(Some(&popover));
    button
}

fn view_menu_body(
    label: &str,
    options: &ViewOptions,
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    preferences: &Rc<RefCell<ViewPreferencesStore>>,
) -> GtkBox {
    let menu = GtkBox::new(Orientation::Vertical, 2);
    menu.set_margin_top(6);
    menu.set_margin_bottom(6);
    match label {
        "Sort" => {
            append_heading(&menu, "Sort by");
            append_radio_group(
                &menu,
                [
                    ("Name", options.sort_field == SortField::Name),
                    (
                        "Date modified",
                        options.sort_field == SortField::DateModified,
                    ),
                    ("Type", options.sort_field == SortField::Type),
                    ("Size", options.sort_field == SortField::Size),
                ],
                "Sort",
                data,
                tabs,
                preferences,
            );
            append_separator(&menu);
            append_heading(&menu, "Direction");
            append_radio_group(
                &menu,
                [
                    (
                        "Ascending",
                        options.sort_direction == SortDirection::Ascending,
                    ),
                    (
                        "Descending",
                        options.sort_direction == SortDirection::Descending,
                    ),
                ],
                "Sort",
                data,
                tabs,
                preferences,
            );
            append_separator(&menu);
            append_check(
                &menu,
                "Folders first",
                options.folders_first,
                options.view_mode != ViewMode::Hybrid,
                "Sort:Toggle folders first",
                data,
                tabs,
                preferences,
            );
        }
        "Group" => {
            append_heading(&menu, "Group by");
            append_radio_group(
                &menu,
                [
                    ("None", options.group_field == GroupField::None),
                    ("Type", options.group_field == GroupField::Type),
                    ("Size", options.group_field == GroupField::Size),
                    (
                        "Date modified",
                        options.group_field == GroupField::DateModified,
                    ),
                ],
                "Group",
                data,
                tabs,
                preferences,
            );
        }
        "View" => {
            append_heading(&menu, "Layout");
            append_radio_group(
                &menu,
                [
                    ("Detailed List", options.view_mode == ViewMode::DetailedList),
                    ("Compact List", options.view_mode == ViewMode::CompactList),
                    ("Icon Grid", options.view_mode == ViewMode::IconGrid),
                    ("Hybrid", options.view_mode == ViewMode::Hybrid),
                ],
                "View",
                data,
                tabs,
                preferences,
            );
            append_separator(&menu);
            append_heading(&menu, "Icon size");
            append_radio_group(
                &menu,
                [
                    ("Small icons", options.grid_size == GridSize::Small),
                    ("Medium icons", options.grid_size == GridSize::Medium),
                    ("Large icons", options.grid_size == GridSize::Large),
                    (
                        "Extra Large icons",
                        options.grid_size == GridSize::ExtraLarge,
                    ),
                ],
                "View",
                data,
                tabs,
                preferences,
            );
            append_separator(&menu);
            append_check(
                &menu,
                "Show thumbnails",
                options.thumbnails,
                true,
                "View:Toggle thumbnails",
                data,
                tabs,
                preferences,
            );
            append_check(
                &menu,
                "Show hidden files",
                options.show_hidden,
                true,
                "View:Toggle hidden files",
                data,
                tabs,
                preferences,
            );
            append_separator(&menu);
            let reset = Button::with_label("Reset folder view");
            reset.set_margin_start(8);
            reset.set_margin_end(8);
            let data = data.clone();
            let tabs = tabs.clone();
            let preferences = preferences.clone();
            reset.connect_clicked(move |_| reset_active_options(&data, &tabs, &preferences));
            menu.append(&reset);
        }
        _ => {}
    }
    menu
}

fn append_heading(menu: &GtkBox, text: &str) {
    let heading = Label::new(Some(text));
    heading.add_css_class("dim-label");
    heading.set_xalign(0.0);
    heading.set_margin_start(12);
    heading.set_margin_top(4);
    menu.append(&heading);
}

fn append_separator(menu: &GtkBox) {
    let separator = Separator::new(Orientation::Horizontal);
    separator.set_margin_top(4);
    separator.set_margin_bottom(4);
    menu.append(&separator);
}

fn append_radio_group<const N: usize>(
    menu: &GtkBox,
    entries: [(&str, bool); N],
    prefix: &str,
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    preferences: &Rc<RefCell<ViewPreferencesStore>>,
) {
    let mut first: Option<CheckButton> = None;
    for (entry, active) in entries {
        let control = CheckButton::with_label(entry);
        control.set_margin_start(8);
        control.set_active(active);
        if let Some(first) = &first {
            control.set_group(Some(first));
        } else {
            first = Some(control.clone());
        }
        let data = data.clone();
        let tabs = tabs.clone();
        let preferences = preferences.clone();
        let action = format!("{prefix}:{entry}");
        control.connect_toggled(move |control| {
            if control.is_active() {
                update_active_options(&data, &tabs, &preferences, &action);
            }
        });
        menu.append(&control);
    }
}

fn append_check(
    menu: &GtkBox,
    label: &str,
    active: bool,
    sensitive: bool,
    action: &str,
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    preferences: &Rc<RefCell<ViewPreferencesStore>>,
) {
    let control = CheckButton::with_label(label);
    control.set_margin_start(8);
    control.set_active(active);
    control.set_sensitive(sensitive);
    let data = data.clone();
    let tabs = tabs.clone();
    let preferences = preferences.clone();
    let action = action.to_string();
    control.connect_toggled(move |_| update_active_options(&data, &tabs, &preferences, &action));
    menu.append(&control);
}

fn file_toolbar(search: SearchEntry, controls: GtkBox) -> GtkBox {
    search.set_hexpand(true);
    let row = GtkBox::new(Orientation::Horizontal, 6);
    row.set_margin_start(8);
    row.set_margin_end(8);
    row.set_margin_bottom(6);
    row.append(&search);
    row.append(&controls);
    row
}

fn update_active_options(
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    preferences: &Rc<RefCell<ViewPreferencesStore>>,
    entry: &str,
) {
    let Some(index) = selected_index(tabs, data) else {
        return;
    };
    let mut data = data.borrow_mut();
    let tab = &mut data[index];
    let mut options = tab.view.options();
    match entry {
        "Sort:Name" => options.sort_field = SortField::Name,
        "Sort:Date modified" => options.sort_field = SortField::DateModified,
        "Sort:Type" => options.sort_field = SortField::Type,
        "Sort:Size" => options.sort_field = SortField::Size,
        "Sort:Ascending" => options.sort_direction = SortDirection::Ascending,
        "Sort:Descending" => options.sort_direction = SortDirection::Descending,
        "Sort:Toggle folders first" => options.folders_first = !options.folders_first,
        "Group:None" => options.group_field = GroupField::None,
        "Group:Type" => options.group_field = GroupField::Type,
        "Group:Size" => options.group_field = GroupField::Size,
        "Group:Date modified" => options.group_field = GroupField::DateModified,
        "View:Detailed List" => options.view_mode = ViewMode::DetailedList,
        "View:Compact List" => options.view_mode = ViewMode::CompactList,
        "View:Icon Grid" => options.view_mode = ViewMode::IconGrid,
        "View:Hybrid" => options.view_mode = ViewMode::Hybrid,
        "View:Small icons" => options.grid_size = GridSize::Small,
        "View:Medium icons" => options.grid_size = GridSize::Medium,
        "View:Large icons" => options.grid_size = GridSize::Large,
        "View:Extra Large icons" => options.grid_size = GridSize::ExtraLarge,
        "View:Toggle thumbnails" => options.thumbnails = !options.thumbnails,
        "View:Toggle hidden files" => options.show_hidden = !options.show_hidden,
        _ => return,
    }
    let path = tab.view.current_location().local_path();
    tab.view.set_options(options.clone());
    if let Some(path) = path {
        persist_options(&mut preferences.borrow_mut(), &path, options);
    }
}

fn reset_active_options(
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    preferences: &Rc<RefCell<ViewPreferencesStore>>,
) {
    let Some(index) = selected_index(tabs, data) else {
        return;
    };
    let Some(path) = data.borrow()[index].view.current_location().local_path() else {
        return;
    };
    let options = {
        let mut preferences = preferences.borrow_mut();
        preferences.clear(&path);
        let options = preferences.options_for(&path, &[]);
        let _ = preferences.save();
        options
    };
    data.borrow()[index].view.set_options(options);
}

fn persist_options(store: &mut ViewPreferencesStore, path: &Path, options: ViewOptions) {
    store.set(path, options);
    let _ = store.save();
}

#[cfg(all(test, target_os = "linux"))]
#[path = "dnd_tests.rs"]
mod dnd_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn unmount_failure_uses_a_safe_operation_error_message_key() {
        let raw = glib::Error::new(
            gio::IOErrorEnum::NotConnected,
            "SMB provider failed at smb://server/private/payroll.csv",
        );

        assert_eq!(
            super::unmount_failure_message(&raw),
            "operation.mount-unavailable"
        );
    }

    #[gtk::test]
    fn slice3_windows_share_cut_lease_and_paste_the_new_owner() {
        use super::*;
        let fixture = tempfile::tempdir().unwrap();
        let views = [
            FileView::new(Location::from_local_path(fixture.path())),
            FileView::new(Location::from_local_path(fixture.path())),
        ];
        let windows = [gtk::Window::new(), gtk::Window::new()];
        for i in 0..2 {
            windows[i].set_child(Some(&views[i].widget));
        }
        let system = views[0].widget.display().clipboard();
        let first = application_clipboard(&system);
        let second = application_clipboard(&views[1].widget.display().clipboard());
        assert!(Rc::ptr_eq(&first, &second));
        let publish = |owner: &Rc<RefCell<Option<FileClipboard>>>, view: &FileView, name: &str| {
            let payload = ClipboardPayload {
                action: ClipboardAction::Cut,
                locations: vec![Location::from_local_path(&fixture.path().join(name))],
            };
            let provider = gtk::gdk::ContentProvider::for_bytes(
                "text/uri-list",
                &glib::Bytes::from_owned(payload.uri_list().into_bytes()),
            );
            system.set_content(Some(&provider)).unwrap();
            let mut state = ClipboardState::default();
            state.replace(payload.clone());
            state.begin_paste(42);
            let generation = state.generation();
            *owner.borrow_mut() = Some(FileClipboard {
                state,
                source_view: view.clone(),
                provider,
            });
            (
                payload,
                Some(ClipboardLease {
                    owner: owner.clone(),
                    generation,
                }),
            )
        };
        let (_, old_cut) = publish(&first, &views[0], "A");
        let (b, new_cut) = publish(&second, &views[1], "B");
        assert_eq!(clipboard_payload(&first, &system), Some(b.clone()));
        finish_cut(&old_cut, 42, &views[0]);
        assert_eq!(clipboard_payload(&second, &system), Some(b));
        finish_cut(&new_cut, 42, &views[0]);
        assert!(second.borrow().is_none());
        assert!(system.content().is_none());
        for window in windows {
            window.close();
        }
        for view in views {
            view.close();
        }
    }

    #[gtk::test]
    fn slice3_two_windows_follow_system_provider_and_old_cut_cannot_clear_replacement() {
        use super::*;
        // Previous tests relinquish the X11 selection asynchronously. Drain
        // their SelectionClear before claiming a new owner in this fixture.
        gtk::gdk::Display::default().unwrap().sync();
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        let fixture = tempfile::tempdir().unwrap();
        let first = FileView::new(Location::from_local_path(fixture.path()));
        let second = FileView::new(Location::from_local_path(fixture.path()));
        let windows = [gtk::Window::new(), gtk::Window::new()];
        windows[0].set_child(Some(&first.widget));
        windows[1].set_child(Some(&second.widget));
        let system = first.widget.display().clipboard();
        let publish = |view: &FileView, name: &str, action| {
            let payload = ClipboardPayload {
                action,
                locations: vec![Location::from_local_path(&fixture.path().join(name))],
            };
            let provider = gtk::gdk::ContentProvider::new_union(&[
                gtk::gdk::ContentProvider::for_value(
                    &gtk::gdk::FileList::from_array(&[gio::File::for_uri(
                        payload.locations[0].uri(),
                    )])
                    .to_value(),
                ),
                gtk::gdk::ContentProvider::for_bytes(
                    "text/uri-list",
                    &glib::Bytes::from_owned(payload.uri_list().into_bytes()),
                ),
                gtk::gdk::ContentProvider::for_bytes(
                    "x-special/gnome-copied-files",
                    &glib::Bytes::from_owned(
                        format!(
                            "{}\n{}",
                            if action == ClipboardAction::Cut {
                                "cut"
                            } else {
                                "copy"
                            },
                            payload.uri_list()
                        )
                        .into_bytes(),
                    ),
                ),
            ]);
            system.set_content(Some(&provider)).unwrap();
            let mut state = ClipboardState::default();
            state.replace(payload.clone());
            state.begin_paste(42);
            let owner = Rc::new(RefCell::new(Some(FileClipboard {
                state,
                source_view: view.clone(),
                provider,
            })));
            watch_clipboard(&system, &owner);
            (owner, payload)
        };
        for finish_before_paste in [false, true] {
            let (owner_a, _) = publish(&first, "A", ClipboardAction::Cut);
            let lease = Some(ClipboardLease {
                generation: owner_a.borrow().as_ref().unwrap().state.generation(),
                owner: owner_a.clone(),
            });
            let (owner_b, b) = publish(&second, "B", ClipboardAction::Copy);
            let replacement = system.content().unwrap();
            if finish_before_paste {
                finish_cut(&lease, 42, &first);
            }
            assert!(
                clipboard_payload(&owner_a, &system).is_none(),
                "window 1 must not paste its stale A cache"
            );
            let read = Rc::new(RefCell::new(None));
            let output = read.clone();
            read_uri_clipboard(system.clone(), move |value| {
                *output.borrow_mut() = Some(value)
            });
            let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while read.borrow().is_none() {
                glib::MainContext::default().iteration(false);
                assert!(std::time::Instant::now() < end, "clipboard read stalled: finish_first={finish_before_paste}, local={}, formats={:?}, owner_matches={}", system.is_local(), system.formats(), system.content().as_ref() == Some(&replacement));
            }
            assert_eq!(read.borrow_mut().take().unwrap(), Some(b.clone()));
            finish_cut(&lease, 42, &first);
            assert_eq!(system.content(), Some(replacement));
            assert_eq!(clipboard_payload(&owner_b, &system), Some(b));
        }
        system
            .set_content(None::<&gtk::gdk::ContentProvider>)
            .unwrap();
        for window in windows {
            window.close();
        }
        first.close();
        second.close();
    }

    #[gtk::test]
    fn slice3_drop_acknowledgement_waits_for_every_terminal_item() {
        use super::*;
        for (decision, expected) in [
            (None, true),
            (Some(ConflictDecision::Skip), false),
            (Some(ConflictDecision::CancelRemaining), false),
        ] {
            let fixture = tempfile::tempdir().unwrap();
            let destination = fixture.path().join("destination");
            std::fs::create_dir(&destination).unwrap();
            let sources: Vec<_> = ["a", "b"]
                .into_iter()
                .map(|name| {
                    let path = fixture.path().join(name);
                    std::fs::write(&path, name).unwrap();
                    Location::from_local_path(&path)
                })
                .collect();
            let view = FileView::new(Location::from_local_path(&destination));
            let manager = OperationManager::default();
            let result = Rc::new(RefCell::new(None));
            let output = result.clone();
            transfer_next(
                view.clone(),
                ClipboardPayload {
                    action: ClipboardAction::Copy,
                    locations: sources.clone(),
                },
                Location::from_local_path(&destination),
                manager.clone(),
                None,
                0,
                decision,
                TransferBatch {
                    completion: Some(TransferCompletion::new(move |success| {
                        *output.borrow_mut() = Some(success)
                    })),
                    ..Default::default()
                },
            );
            assert!(
                result.borrow().is_none(),
                "enqueue is not an acknowledgement"
            );
            let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while result.borrow().is_none() {
                glib::MainContext::default().iteration(false);
                assert!(std::time::Instant::now() < end);
            }
            assert_eq!(*result.borrow(), Some(expected));
            assert_eq!(manager.active_count(), 0);
            for source in sources {
                assert!(
                    source.local_path().unwrap().exists(),
                    "external MOVE source remains until protocol acknowledgement"
                );
            }
            view.close();
        }
    }

    #[gtk::test]
    fn slice3_system_clipboard_round_trip_validates_uri_transport_and_cut_action() {
        use super::*;
        let clipboard = gtk::gdk::Display::default().unwrap().clipboard();
        for (text, accepted) in [
            ("cut\nsmb://host/share/a\nfile:///tmp/b\n", true),
            ("copy\nsmb://user:secret@host/share/a\n", false),
        ] {
            let provider = gtk::gdk::ContentProvider::for_bytes(
                "x-special/gnome-copied-files",
                &glib::Bytes::from_owned(text.as_bytes().to_vec()),
            );
            clipboard.set_content(Some(&provider)).unwrap();
            let result = Rc::new(RefCell::new(None));
            let output = result.clone();
            read_uri_clipboard(clipboard.clone(), move |payload| {
                *output.borrow_mut() = Some(payload)
            });
            let context = glib::MainContext::default();
            let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while result.borrow().is_none() {
                context.iteration(false);
                assert!(std::time::Instant::now() < end);
            }
            let value = result.borrow_mut().take().unwrap();
            assert_eq!(value.is_some(), accepted);
            if let Some(payload) = value {
                assert_eq!(payload.action, ClipboardAction::Cut);
                assert_eq!(payload.locations.len(), 2);
            }
        }
        clipboard
            .set_content(None::<&gtk::gdk::ContentProvider>)
            .unwrap();
    }

    #[gtk::test]
    fn slice3_cancel_remaining_records_every_unattempted_uri() {
        use super::*;
        let fixture = tempfile::tempdir().unwrap();
        let view = FileView::new(Location::from_local_path(fixture.path()));
        let sources: Vec<_> = ["a", "b", "c"]
            .into_iter()
            .map(|name| Location::from_local_path(&fixture.path().join(name)))
            .collect();
        let manager = OperationManager::default();
        transfer_next(
            view.clone(),
            ClipboardPayload {
                action: ClipboardAction::Copy,
                locations: sources.clone(),
            },
            Location::from_local_path(fixture.path()),
            manager.clone(),
            None,
            0,
            Some(ConflictDecision::CancelRemaining),
            TransferBatch::default(),
        );
        let context = glib::MainContext::default();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while manager.active_count() > 0 {
            context.iteration(false);
            assert!(std::time::Instant::now() < end);
        }
        let record = manager.records().remove(0);
        assert_eq!(
            manager.summary(record.id).unwrap().remaining,
            sources
                .iter()
                .map(|l| l.uri().to_owned())
                .collect::<Vec<_>>()
        );
        view.close();
    }

    #[gtk::test]
    fn slice3_cut_transfer_clears_after_completion_and_retains_a_replacement_cut() {
        use super::*;
        let context = glib::MainContext::default();
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let destination = fixture.path().join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        for name in ["a", "b"] {
            std::fs::write(source.join(name), "data").unwrap();
        }
        let view = FileView::new(Location::from_local_path(&source));
        let payload = ClipboardPayload {
            action: ClipboardAction::Cut,
            locations: ["a", "b"]
                .into_iter()
                .map(|name| Location::from_local_path(&source.join(name)))
                .collect(),
        };
        let manager = OperationManager::default();
        let mut state = ClipboardState::default();
        state.replace(payload.clone());
        let clipboard = Rc::new(RefCell::new(Some(FileClipboard {
            state,
            source_view: view.clone(),
            provider: gtk::gdk::ContentProvider::for_value(&"fixture".to_value()),
        })));
        start_uri_transfer(
            &view,
            payload.clone(),
            Location::from_local_path(&destination),
            manager.clone(),
            Some(clipboard.clone()),
        );
        assert!(clipboard
            .borrow()
            .as_ref()
            .unwrap()
            .state
            .payload()
            .is_some());
        // Re-cutting the same URIs is a new clipboard generation. An old batch
        // must not consume it as it advances from the first to the second item.
        clipboard
            .borrow_mut()
            .as_mut()
            .unwrap()
            .state
            .replace(payload.clone());
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while manager.records().len() < 2 || manager.active_count() > 0 {
            context.iteration(false);
            assert!(std::time::Instant::now() < end);
        }
        let settle = std::time::Instant::now() + std::time::Duration::from_millis(100);
        while std::time::Instant::now() < settle {
            context.iteration(false);
        }
        assert_eq!(
            clipboard.borrow().as_ref().and_then(|c| c.state.payload()),
            Some(&payload)
        );
        assert!(destination.join("a").exists() && destination.join("b").exists());
        let moved = ClipboardPayload {
            action: ClipboardAction::Cut,
            locations: ["a", "b"]
                .into_iter()
                .map(|name| Location::from_local_path(&destination.join(name)))
                .collect(),
        };
        clipboard
            .borrow_mut()
            .as_mut()
            .unwrap()
            .state
            .replace(moved.clone());
        start_uri_transfer(
            &view,
            moved,
            Location::from_local_path(&source),
            manager.clone(),
            Some(clipboard.clone()),
        );
        assert!(clipboard.borrow().is_some());
        while clipboard.borrow().is_some() {
            context.iteration(false);
            assert!(std::time::Instant::now() < end);
        }
        assert!(source.join("a").exists() && source.join("b").exists());
        view.close();
    }

    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use gtk::prelude::{OrientableExt, PopoverExt, WidgetExt};
    use gtk::{Box as GtkBox, Label, Orientation, SearchEntry, Window};

    use super::{
        content_right_inset, file_toolbar, handle_navigation_action, matching_index, menu_button,
        persist_options, remove_matching, shortcut_command, should_protect_last_file_tab,
        sidebar_path_for_tab, TabData,
    };
    use crate::core::sidebar_model::SidebarSection;
    use crate::core::{
        Location, SidebarModel, SidebarPreferences, SortField, ViewOptions, ViewPreferencesStore,
    };
    use crate::ui::context_menu::ContextCommand;
    use crate::ui::file_view::FileView;
    use crate::ui::header::{HeaderControls, NavigationAction};
    use crate::ui::sidebar::Sidebar;

    fn commit_remote_location(view: &FileView) {
        use crate::core::{contracts::*, Location, ResolvedLocation};
        let location = Location::from_uri("smb://server/share").unwrap();
        let state = view.tab_state();
        let request = state
            .borrow_mut()
            .begin_navigation(location.clone())
            .unwrap();
        assert!(state.borrow_mut().apply_directory_result(
            request.generation,
            RequestCompletion {
                id: request.request.id,
                outcome: RequestOutcome::Succeeded(DirectorySnapshot {
                    location: ResolvedLocation {
                        requested: location.clone(),
                        resolved: location
                    },
                    entries: vec![],
                    capabilities: Default::default(),
                    next_page: None,
                }),
            }
        ));
    }

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn back_from_a_remote_section_restores_the_header_tab_and_sidebar() {
        gtk::init().expect("GTK should initialize for widget tests");
        let path = PathBuf::from("/tmp");
        let location = Rc::new(RefCell::new(path.clone()));
        let view = FileView::new(Location::from_local_path(&location.borrow()));
        commit_remote_location(&view);
        view.show_section_overview(SidebarSection {
            title: "Network & online".into(),
            nodes: vec![],
            mountables: vec![],
        });
        let tabs = adw::TabView::new();
        let page = tabs.append(&view.widget);
        tabs.set_selected_page(&page);
        page.set_title("Network & online");
        let data = Rc::new(RefCell::new(vec![TabData {
            view: view.clone(),
            location: Rc::new(RefCell::new(view.current_location())),
            page: page.clone(),
        }]));
        let header = HeaderControls::new(Rc::new(RefCell::new(
            crate::core::Location::from_uri("file:///tmp").unwrap(),
        )));
        header.set_section("Network & online");
        let window = Window::new();
        let sidebar = Sidebar::new(
            Rc::new(RefCell::new(SidebarModel::new(
                SidebarPreferences::in_memory(),
                PathBuf::from("/tmp"),
            ))),
            &window,
            |_| {},
        );
        sidebar.set_active_section("Network & online");

        handle_navigation_action(
            0,
            NavigationAction::Back,
            &data,
            &tabs,
            &header,
            &Rc::new(RefCell::new(ViewPreferencesStore::in_memory())),
            &sidebar,
        );

        assert_eq!(view.section_title(), None);
        assert_eq!(view.uri().as_deref(), Some("smb://server/share"));
        assert_eq!(page.title(), "smb://server/…");
        assert_eq!(header.displayed_location_text(), "smb://server/share");
        assert_eq!(sidebar.active_navigation_selection(), (None, None));
    }

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn refresh_section_rereads_the_sidebar_model_without_losing_remote_state() {
        gtk::init().expect("GTK should initialize for widget tests");
        let path = PathBuf::from("/tmp");
        let location = Rc::new(RefCell::new(path.clone()));
        let view = FileView::new(Location::from_local_path(&location.borrow()));
        commit_remote_location(&view);
        let model = Rc::new(RefCell::new(SidebarModel::new(
            SidebarPreferences::in_memory(),
            path.clone(),
        )));
        let initial_section = model
            .borrow()
            .sections()
            .into_iter()
            .find(|section| section.title == "Network & online")
            .unwrap();
        let initial_count = initial_section.nodes.len();
        view.show_section_overview(initial_section);
        model
            .borrow_mut()
            .preferences_mut()
            .pin_remote("smb://server/updated".into())
            .unwrap();
        let tabs = adw::TabView::new();
        let page = tabs.append(&view.widget);
        tabs.set_selected_page(&page);
        let data = Rc::new(RefCell::new(vec![TabData {
            view: view.clone(),
            location: Rc::new(RefCell::new(view.current_location())),
            page,
        }]));
        let header = HeaderControls::new(Rc::new(RefCell::new(
            crate::core::Location::from_uri("file:///tmp").unwrap(),
        )));
        let window = Window::new();
        let sidebar = Sidebar::new(model, &window, |_| {});

        handle_navigation_action(
            0,
            NavigationAction::Refresh,
            &data,
            &tabs,
            &header,
            &Rc::new(RefCell::new(ViewPreferencesStore::in_memory())),
            &sidebar,
        );

        assert_eq!(view.uri().as_deref(), Some("smb://server/share"));
        assert_eq!(view.section_title().as_deref(), Some("Network & online"));
        assert_eq!(view.section_overview_node_count(), initial_count + 1);

        view.set_uri("smb://server/pending".into());
        let generation = view.tab_state().borrow().navigation_generation;
        handle_navigation_action(
            0,
            NavigationAction::Refresh,
            &data,
            &tabs,
            &header,
            &Rc::new(RefCell::new(ViewPreferencesStore::in_memory())),
            &sidebar,
        );
        assert!(view.tab_state().borrow().navigation_generation > generation,
            "toolbar Refresh must replace the pending navigation, not cancel it by rebuilding the section");
        assert!(matches!(&view.tab_state().borrow().status,
            crate::core::navigation::NavigationStatus::Loading(location) if location.uri() == "smb://server/pending"));
        assert_eq!(view.section_title().as_deref(), Some("Network & online"));
    }

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn refresh_remote_location_reloads_it_without_a_section_view() {
        gtk::init().expect("GTK should initialize for widget tests");
        let path = PathBuf::from("/tmp");
        let location = Rc::new(RefCell::new(path.clone()));
        let view = FileView::new(Location::from_local_path(&location.borrow()));
        commit_remote_location(&view);
        view.show_search_results(vec![]);
        let tabs = adw::TabView::new();
        let page = tabs.append(&view.widget);
        tabs.set_selected_page(&page);
        let data = Rc::new(RefCell::new(vec![TabData {
            view: view.clone(),
            location: Rc::new(RefCell::new(view.current_location())),
            page,
        }]));
        let header = HeaderControls::new(Rc::new(RefCell::new(
            crate::core::Location::from_uri("file:///tmp").unwrap(),
        )));
        let window = Window::new();
        let sidebar = Sidebar::new(
            Rc::new(RefCell::new(SidebarModel::new(
                SidebarPreferences::in_memory(),
                PathBuf::from("/tmp"),
            ))),
            &window,
            |_| {},
        );

        handle_navigation_action(
            0,
            NavigationAction::Refresh,
            &data,
            &tabs,
            &header,
            &Rc::new(RefCell::new(ViewPreferencesStore::in_memory())),
            &sidebar,
        );

        assert_eq!(view.section_title(), None);
        assert_eq!(view.uri().as_deref(), Some("smb://server/share"));
        assert!(
            view.is_showing_search_results(),
            "pending refresh preserves current content"
        );
        commit_remote_location(&view);
        assert!(!view.is_showing_search_results());
    }

    #[test]
    fn remove_matching_releases_the_read_borrow_before_removing_the_middle_item() {
        let items = RefCell::new(vec!["first", "middle", "last"]);

        assert!(remove_matching(&items, |item| *item == "middle"));
        assert_eq!(items.into_inner(), vec!["first", "last"]);
    }

    #[test]
    fn matching_index_tracks_a_tab_after_a_preceding_tab_is_removed() {
        let mut pages = vec!["first", "middle", "last"];
        pages.remove(0);

        assert_eq!(matching_index(&pages, |page| *page == "last"), Some(1));
    }

    #[test]
    fn save_active_options_updates_the_selected_folder() {
        let path = PathBuf::from("/tmp/selected-folder");
        let mut store = ViewPreferencesStore::in_memory();

        persist_options(
            &mut store,
            &path,
            ViewOptions {
                sort_field: SortField::Size,
                ..ViewOptions::default()
            },
        );

        assert_eq!(store.options_for(&path, &[]).sort_field, SortField::Size);
    }

    #[test]
    fn content_right_inset_matches_toolbar() {
        assert_eq!(content_right_inset(), 8);
    }

    #[test]
    fn sidebar_selection_follows_the_selected_tabs_local_location() {
        let documents = PathBuf::from("/home/user/Documents");

        assert_eq!(sidebar_path_for_tab(&documents, false), Some(documents));
        assert_eq!(
            sidebar_path_for_tab(PathBuf::from("/unused").as_path(), true),
            None
        );
    }

    #[test]
    fn common_file_operation_shortcuts_map_to_context_commands() {
        assert_eq!(
            shortcut_command(Some('x'), false, true, false),
            Some(ContextCommand::Cut)
        );
        assert_eq!(
            shortcut_command(Some('c'), false, true, false),
            Some(ContextCommand::Copy)
        );
        assert_eq!(
            shortcut_command(Some('v'), false, true, false),
            Some(ContextCommand::Paste)
        );
        assert_eq!(
            shortcut_command(None, true, false, false),
            Some(ContextCommand::MoveToTrash)
        );
        assert_eq!(
            shortcut_command(None, true, false, true),
            Some(ContextCommand::DeletePermanently)
        );
    }

    #[test]
    fn file_operation_shortcuts_require_the_control_modifier_except_delete() {
        assert_eq!(shortcut_command(Some('x'), false, false, false), None);
        assert_eq!(shortcut_command(Some('v'), false, false, false), None);
        assert_eq!(
            shortcut_command(None, true, true, false),
            Some(ContextCommand::MoveToTrash)
        );
    }

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn file_toolbar_places_search_and_view_controls_in_one_row() {
        gtk::init().expect("GTK should initialize for widget tests");

        let row = file_toolbar(SearchEntry::new(), GtkBox::new(Orientation::Horizontal, 6));

        assert_eq!(row.orientation(), Orientation::Horizontal);
        assert!(row.first_child().is_some());
        assert!(row.last_child().is_some());
    }

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn stateful_menu_button_has_visible_initial_content_and_is_sensitive() {
        gtk::init().expect("GTK should initialize for widget tests");

        let button = menu_button("Sort", &Label::new(Some("Sort by")));

        assert!(button.popover().is_some());
        assert!(button
            .popover()
            .and_then(|popover| popover.child())
            .is_some());
        assert!(button.is_sensitive());
    }

    #[test]
    fn utility_tabs_never_trigger_the_last_file_tab_close_rule() {
        assert!(!should_protect_last_file_tab(false, 1));
        assert!(should_protect_last_file_tab(true, 1));
        assert!(!should_protect_last_file_tab(true, 2));
    }

    #[test]
    fn sidebar_actions_fall_back_to_a_file_tab() {
        // Preferences and Operations are in the tab view but not in `tabs_data`,
        // so nothing matches the selected page while one is open. Without a
        // fallback every sidebar action silently did nothing.
        assert_eq!(
            super::sidebar_target(Some(2), 4),
            Some(2),
            "a selected file tab is used as-is"
        );
        assert_eq!(
            super::sidebar_target(None, 3),
            Some(0),
            "Preferences selected falls back to a file tab"
        );
        assert_eq!(
            super::sidebar_target(None, 0),
            None,
            "with no file tabs there is nothing to act on"
        );
    }
}
