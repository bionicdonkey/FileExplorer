use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use adw::{Application, ApplicationWindow, HeaderBar, TabPage, TabView, ToolbarView};
use gtk::{
    Align, Box as GtkBox, Button, CheckButton, Entry, Label, MenuButton, Orientation, Paned,
    Popover, SearchEntry, Separator,
};

use crate::core::file_item::FileItem;
use crate::core::location::Location;
use crate::core::navigation::NavigationHistory;
use crate::core::operations::FileOperations;
use crate::core::{ApplicationPreferences, ConflictDefault, OperationKind, OperationManager};
use crate::core::{
    GridSize, GroupField, SidebarModel, SidebarPreferences, SortDirection, SortField, ViewMode,
    ViewOptions, ViewPreferencesStore,
};
use crate::core::{SearchBackend, SearchHistory, SearchQuery, SearchService};
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
    location: Rc<RefCell<PathBuf>>,
    history: Rc<RefCell<NavigationHistory>>,
    page: TabPage,
}

#[derive(Clone, Copy)]
enum ClipboardOperation {
    Copy,
    Move,
}

#[derive(Clone)]
struct FileClipboard {
    path: PathBuf,
    operation: ClipboardOperation,
    source_view: FileView,
}

pub struct ExplorerWindow {
    window: ApplicationWindow,
}

impl ExplorerWindow {
    pub fn new(app: &Application) -> Self {
        Self::new_at(
            app,
            Location::home().local_path().expect("home is local"),
            true,
        )
    }

    fn new_at(app: &Application, initial: PathBuf, restore_session: bool) -> Self {
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
            history.prune(
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
                            .filter_map(|location| location.local_path())
                            .filter(|path| path.is_dir())
                            .collect::<Vec<_>>()
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
        let clipboard: Rc<RefCell<Option<FileClipboard>>> = Rc::new(RefCell::new(None));
        for path in startup_paths {
            let location = Rc::new(RefCell::new(path.clone()));
            let view = FileView::new(location.clone());
            view.set_options(preferences.borrow().options_for(&path, &[]));
            let page = tabs.view.append(&view.widget);
            page.set_title(&tab_title(&path));
            tabs_data.borrow_mut().push(TabData {
                view,
                location,
                history: history_with(path),
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
                if let (Some(index), Some(section), Some(sidebar)) = (
                    selected_index(&sidebar_context_tabs, &sidebar_context_data),
                    section,
                    sidebar,
                ) {
                    let tab = &sidebar_context_data.borrow()[index];
                    tab.view.show_section_overview(section.clone());
                    tab.page.set_title(&section.title);
                    sidebar_context_header.set_section(&section.title);
                    sidebar.set_active_section(section.title);
                }
            }
            SidebarAction::Open(path) => {
                let sidebar = sidebar_holder_actions.borrow().clone();
                if let (Some(index), Some(sidebar)) = (
                    selected_index(&sidebar_context_tabs, &sidebar_context_data),
                    sidebar,
                ) {
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
            SidebarAction::OpenUri(uri) => {
                let sidebar = sidebar_holder_actions.borrow().clone();
                if let (Some(index), Some(sidebar)) = (
                    selected_index(&sidebar_context_tabs, &sidebar_context_data),
                    sidebar,
                ) {
                    navigate_uri_tab(
                        index,
                        uri,
                        &sidebar_context_data,
                        &sidebar_context_tabs,
                        &sidebar_context_header,
                        &sidebar,
                    );
                }
            }
            SidebarAction::Pin(path) => {
                let mut model = sidebar_model_actions.borrow_mut();
                model.preferences_mut().pin(path);
                let _ = model.preferences().save();
                drop(model);
                if let Some(sidebar) = sidebar_holder_actions.borrow().as_ref() {
                    sidebar.refresh();
                }
            }
            SidebarAction::Unpin(path) => {
                let mut model = sidebar_model_actions.borrow_mut();
                model.preferences_mut().unpin(&path);
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
            SidebarAction::UnpinRemote(uri) => {
                let mut model = sidebar_model_actions.borrow_mut();
                model.preferences_mut().unpin_remote(&uri);
                let _ = model.preferences().save();
                drop(model);
                if let Some(sidebar) = sidebar_holder_actions.borrow().as_ref() {
                    sidebar.refresh();
                }
            }
            SidebarAction::Unmount(uri) => {
                let mount = sidebar_model_actions.borrow().mount_for_uri(&uri);
                if let Some(mount) = mount {
                    let operation = gtk::MountOperation::new(Some(&sidebar_window));
                    let keep_operation = operation.clone();
                    let sidebar = sidebar_holder_actions.clone();
                    mount.unmount_with_operation(
                        gio::MountUnmountFlags::NONE,
                        Some(&operation),
                        gio::Cancellable::NONE,
                        move |_| {
                            let _keep_operation_alive = &keep_operation;
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
        sidebar.set_active_path(&initial);
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
        search.connect_search_changed(move |entry| {
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
        let toolbar = ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&body));
        window.set_content(Some(&toolbar));

        let collapsible_sidebar = sidebar.widget.clone();
        sidebar_button.connect_clicked(move |_| {
            collapsible_sidebar.set_visible(!collapsible_sidebar.is_visible());
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
        let shortcuts = gtk::EventControllerKey::new();
        shortcuts.connect_key_pressed(move |_, key, _, modifiers| {
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
            let view = FileView::new(location.clone());
            view.set_options(new_preferences.borrow().options_for(&path, &[]));
            let page = new_tabs.append(&view.widget);
            page.set_title(&tab_title(&path));
            new_data.borrow_mut().push(TabData {
                view,
                location,
                history: history_with(path),
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
            let locations = session_data
                .borrow()
                .iter()
                .filter(|tab| tab.view.uri().is_none())
                .map(|tab| {
                    gio::File::for_path(&*tab.location.borrow())
                        .uri()
                        .to_string()
                })
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
                if let Some(title) = tab.view.section_title() {
                    select_header.set_section(&title);
                    select_sidebar.set_active_section(title);
                } else if let Some(uri) = tab.view.uri() {
                    select_header.set_uri(&uri);
                } else {
                    select_header.set_location(&tab.location.borrow());
                }
                if tab.view.section_title().is_none() {
                    sync_sidebar_for_tab(
                        &select_sidebar,
                        &tab.location.borrow(),
                        tab.view.uri().is_some(),
                    );
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

fn history_with(path: PathBuf) -> Rc<RefCell<NavigationHistory>> {
    let history = Rc::new(RefCell::new(NavigationHistory::new()));
    history.borrow_mut().push(path);
    history
}

fn handle_navigation_action(
    index: usize,
    action: NavigationAction,
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    header: &HeaderControls,
    preferences: &Rc<RefCell<ViewPreferencesStore>>,
    sidebar: &Sidebar,
) {
    let view = data.borrow()[index].view.clone();
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
    if matches!(action, NavigationAction::Refresh) {
        if let Some(uri) = view.uri() {
            view.set_uri(uri);
            return;
        }
    }
    let next = {
        let data = data.borrow_mut();
        match action {
            NavigationAction::Back => data[index]
                .history
                .borrow_mut()
                .back()
                .map(ToOwned::to_owned),
            NavigationAction::Forward => data[index]
                .history
                .borrow_mut()
                .forward()
                .map(ToOwned::to_owned),
            NavigationAction::Up => data[index]
                .location
                .borrow()
                .parent()
                .map(ToOwned::to_owned),
            NavigationAction::Refresh => Some(data[index].location.borrow().clone()),
        }
    };
    if let Some(path) = next {
        navigate_tab(
            index,
            path,
            data,
            tabs,
            header,
            preferences,
            sidebar,
            !matches!(
                action,
                NavigationAction::Back | NavigationAction::Forward | NavigationAction::Refresh
            ),
        );
    }
}

fn new_tab_location(preferences: &ApplicationPreferences) -> PathBuf {
    gio::File::for_uri(&preferences.new_tab_location)
        .path()
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| Location::home().local_path().expect("home is local"))
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
    let quick_access_model = sidebar.model();
    view.set_quick_access_predicate(move |path| {
        quick_access_model.borrow().preferences().is_pinned(path)
    });
    let options_data = data.clone();
    let options_preferences = preferences.clone();
    let options_page = page.clone();
    view.set_options_changed_handler(move |options| {
        let path = options_data
            .borrow()
            .iter()
            .find(|tab| tab.page == options_page)
            .map(|tab| tab.location.borrow().clone());
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

    let uri_navigation_data = data.clone();
    let uri_navigation_tabs = tabs.clone();
    let uri_navigation_header = header.clone();
    let uri_navigation_page = page.clone();
    let uri_navigation_sidebar = sidebar.clone();
    view.set_uri_navigation_handler(move |uri| {
        let index = {
            uri_navigation_data
                .borrow()
                .iter()
                .position(|tab| tab.page == uri_navigation_page)
        };
        if let Some(index) = index {
            navigate_uri_tab(
                index,
                uri,
                &uri_navigation_data,
                &uri_navigation_tabs,
                &uri_navigation_header,
                &uri_navigation_sidebar,
            );
        }
    });

    let command_data = data.clone();
    let command_tabs = tabs.clone();
    let command_header = header.clone();
    let command_preferences = preferences.clone();
    let command_page = page.clone();
    let command_app = app.clone();
    let command_clipboard = clipboard.clone();
    let command_sidebar = sidebar.clone();
    let command_operations = operations.clone();
    let command_view = view.clone();
    view.set_context_handler(move |target, command| {
        let path = target_path(&target);
        match command {
            ContextCommand::OpenInNewTab => {
                if let Some(path) = path {
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
                if let Some(path) = path {
                    ExplorerWindow::new_at(&command_app, path, false).present();
                }
            }
            ContextCommand::OpenInTerminal => {
                if let Some(path) = path {
                    open_terminal(&path);
                }
            }
            ContextCommand::Copy | ContextCommand::Cut => {
                if let Some(path) = path {
                    if command == ContextCommand::Cut {
                        command_view.set_pending_cut(Some(path.clone()));
                    }
                    *command_clipboard.borrow_mut() = Some(FileClipboard {
                        path,
                        operation: if command == ContextCommand::Copy {
                            ClipboardOperation::Copy
                        } else {
                            ClipboardOperation::Move
                        },
                        source_view: command_view.clone(),
                    });
                }
            }
            ContextCommand::CopyFilename => {
                if let Some(path) = path {
                    copy_text(
                        &command_view,
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default(),
                    );
                }
            }
            ContextCommand::CopyFullPath => {
                if let Some(path) = path {
                    copy_text(&command_view, &path.display().to_string());
                }
            }
            ContextCommand::CopyTo | ContextCommand::MoveTo => {
                if let Some(path) = path {
                    choose_destination(
                        &command_view,
                        path,
                        command == ContextCommand::MoveTo,
                        command_operations.clone(),
                        command_sidebar.model(),
                    );
                }
            }
            ContextCommand::Paste => {
                let destination = command_data
                    .borrow()
                    .iter()
                    .find(|tab| tab.page == command_page)
                    .map(|tab| tab.location.borrow().clone());
                if let (Some(clipboard), Some(destination)) =
                    (command_clipboard.borrow().clone(), destination)
                {
                    let kind = match clipboard.operation {
                        ClipboardOperation::Copy => OperationKind::Copy,
                        ClipboardOperation::Move => OperationKind::Move,
                    };
                    let cut_source = matches!(clipboard.operation, ClipboardOperation::Move)
                        .then_some(clipboard.source_view);
                    start_transfer(
                        &command_view,
                        clipboard.path,
                        destination,
                        kind,
                        command_operations.clone(),
                        cut_source,
                    );
                }
            }
            ContextCommand::NewFolder => {
                if let Some(parent) = path {
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
                            FileOperations::create_folder(&submit_operations, &parent, &name)
                        },
                        move |id| schedule_refresh(refresh.clone(), refresh_operations.clone(), id),
                    );
                }
            }
            ContextCommand::Rename => {
                if let Some(path) = path {
                    let initial = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_owned();
                    let refresh = command_view.clone();
                    let submit_operations = command_operations.clone();
                    let refresh_operations = command_operations.clone();
                    show_name_dialog(
                        &command_view,
                        "Rename",
                        "New name",
                        &initial,
                        "Rename",
                        move |name| FileOperations::rename(&submit_operations, &path, &name),
                        move |id| schedule_refresh(refresh.clone(), refresh_operations.clone(), id),
                    );
                }
            }
            ContextCommand::MoveToTrash => {
                if let Some(path) = path {
                    let operation_id =
                        command_operations.submit(OperationKind::Trash, vec![path], None);
                    schedule_refresh(
                        command_view.clone(),
                        command_operations.clone(),
                        operation_id,
                    );
                }
            }
            ContextCommand::DeletePermanently => {
                if let Some(path) = path {
                    confirm_permanent_delete(&command_view, path, command_operations.clone());
                }
            }
            ContextCommand::Properties => {
                if let Some(path) = path {
                    let refresh = command_view.clone();
                    PropertiesDialog::present(
                        command_view.widget.root().and_downcast_ref::<gtk::Window>(),
                        path,
                        move || refresh.set_location(refresh.location()),
                    );
                }
            }
            ContextCommand::ToggleQuickAccess => {
                if let Some(path) = path {
                    command_sidebar.toggle_quick_access(path);
                }
            }
            ContextCommand::Refresh => command_view.set_location(command_view.location()),
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

#[allow(dead_code, deprecated)]
fn show_properties_dialog(view: &FileView, path: &Path) {
    let dialog = gtk::Dialog::new();
    dialog.set_title(Some("Properties"));
    dialog.set_modal(true);
    if let Some(window) = view.widget.root().and_downcast::<gtk::Window>() {
        dialog.set_transient_for(Some(&window));
    }
    dialog.add_button("Close", gtk::ResponseType::Close);
    let content = dialog.content_area();
    content.set_spacing(6);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);
    let metadata = std::fs::metadata(path).ok();
    let kind = metadata
        .as_ref()
        .map(|meta| if meta.is_dir() { "Folder" } else { "File" })
        .unwrap_or("Unavailable");
    let size = metadata.as_ref().map(|meta| meta.len()).unwrap_or_default();
    for text in [
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("/")
            .to_owned(),
        format!("Type: {kind}"),
        format!("Location: {}", path.parent().unwrap_or(path).display()),
        format!("Size: {} bytes", size),
    ] {
        let label = Label::new(Some(&text));
        label.set_xalign(0.0);
        label.set_selectable(true);
        content.append(&label);
    }
    dialog.connect_response(|dialog, _| dialog.close());
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
            ContextCommand::DeletePermanently
        } else {
            ContextCommand::MoveToTrash
        });
    }
    control
        .then(|| match key {
            Some('x') => Some(ContextCommand::Cut),
            Some('c') => Some(ContextCommand::Copy),
            Some('v') => Some(ContextCommand::Paste),
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
        Some(ContextTarget::Background(Location::from_local_path(
            &view.location(),
        )))
    } else {
        view.selected_target()
    };
    if let Some(target) = target {
        view.dispatch_context_command(target, command);
    }
}

fn add_tab_at(
    path: PathBuf,
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
    let view = FileView::new(location.clone());
    view.set_options(preferences.borrow().options_for(&path, &[]));
    let page = tabs.append(&view.widget);
    page.set_title(&tab_title(&path));
    data.borrow_mut().push(TabData {
        view,
        location,
        history: history_with(path),
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
    let (view, folder, is_remote) = {
        let data = data.borrow();
        let tab = &data[index];
        let snapshot = (
            tab.view.clone(),
            tab.location.borrow().clone(),
            tab.view.uri().is_some(),
        );
        snapshot
    };
    if term.trim().is_empty() {
        view.clear_search_results();
        status.set_text("Ready");
        return;
    }
    if is_remote {
        status.set_text("Search isn't available for remote locations");
        return;
    }
    let content_search = preferences.borrow().content_search_enabled;
    let query = SearchQuery::new(term, content_search);
    match SearchService::search(&query, &folder) {
        Ok((backend, paths)) => {
            let items: Vec<FileItem> = paths
                .iter()
                .filter_map(|path| FileItem::from_path(path).ok())
                .collect();
            status.set_text(&search_status_text(backend, items.len()));
            view.show_search_results(items);
        }
        Err(error) => status.set_text(&format!("Search failed: {error}")),
    }
}

fn search_status_text(backend: SearchBackend, count: usize) -> String {
    let scope = match backend {
        SearchBackend::Tracker => "the desktop search index",
        SearchBackend::FolderFallback => "this folder only (desktop search index unavailable)",
    };
    format!(
        "Found {count} item{} searching {scope}",
        if count == 1 { "" } else { "s" }
    )
}

/// Recorded only when a search is committed with Enter, not on every
/// keystroke, and only while the user has recent-search recording enabled.
fn record_search_term(
    term: &str,
    preferences: &Rc<RefCell<ApplicationPreferences>>,
    history: &Rc<RefCell<SearchHistory>>,
) {
    if term.trim().is_empty() || !preferences.borrow().record_search_history {
        return;
    }
    let mut history = history.borrow_mut();
    history.record(term);
    let _ = history.save();
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

fn choose_destination(
    view: &FileView,
    source: PathBuf,
    move_source: bool,
    operations: OperationManager,
    sidebar_model: Rc<RefCell<SidebarModel>>,
) {
    let Some(parent) = view.widget.root().and_downcast::<gtk::Window>() else {
        return;
    };
    let reload = view.clone();
    let title = if move_source { "Move To" } else { "Copy To" };
    let action_label = if move_source { "Move" } else { "Copy" };
    crate::ui::folder_picker::pick_folder(
        &parent,
        title,
        action_label,
        view.location(),
        sidebar_model,
        operations.clone(),
        move |destination| {
            start_transfer(
                &reload,
                source.clone(),
                destination,
                if move_source {
                    OperationKind::Move
                } else {
                    OperationKind::Copy
                },
                operations.clone(),
                None,
            );
        },
    );
}

#[allow(deprecated)]
fn start_transfer(
    view: &FileView,
    source: PathBuf,
    destination: PathBuf,
    kind: OperationKind,
    operations: OperationManager,
    cut_source: Option<FileView>,
) {
    let Some(name) = source.file_name().map(|name| name.to_owned()) else {
        return;
    };
    let target = destination.join(&name);
    let refresh_view = view.clone();
    let submit = move |target: Option<PathBuf>, replace: bool| {
        let operation_id = match target {
            Some(target) => operations.submit_to_target(kind, source.clone(), target),
            None if replace => {
                operations.submit_replacing(kind, vec![source.clone()], destination.clone())
            }
            None => operations.submit(kind, vec![source.clone()], Some(destination.clone())),
        };
        if kind == OperationKind::Move {
            if let Some(cut_source) = &cut_source {
                cut_source.set_pending_cut(None);
            }
        }
        schedule_refresh(refresh_view.clone(), operations.clone(), operation_id);
    };
    if !target.exists() {
        submit(None, false);
        return;
    }
    match ApplicationPreferences::load_default()
        .unwrap_or_default()
        .conflict_default
    {
        ConflictDefault::Replace => {
            submit(None, true);
            return;
        }
        ConflictDefault::KeepBoth => {
            submit(Some(FileOperations::keep_both_destination(&target)), false);
            return;
        }
        ConflictDefault::Skip => return,
        ConflictDefault::Ask => {}
    }
    let parent = view.widget.root().and_downcast::<gtk::Window>();
    let dialog = adw::MessageDialog::new(
        parent.as_ref(),
        Some("An item with this name already exists"),
        Some(&format!(
            "“{}” already exists in this folder. Choose how to continue.",
            name.to_string_lossy()
        )),
    );
    dialog.add_responses(&[
        ("replace", "Replace"),
        ("keep", "Keep Both"),
        ("skip", "Skip"),
        ("cancel", "Cancel"),
    ]);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| match response {
        "replace" => submit(None, true),
        "keep" => submit(Some(FileOperations::keep_both_destination(&target)), false),
        _ => {}
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
        view.set_location(view.location());
        if let crate::core::OperationState::Failed(error) = record.state {
            show_operation_error(&view, &error);
        }
        glib::ControlFlow::Break
    });
}

fn show_operation_error(view: &FileView, error: &str) {
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
fn confirm_permanent_delete(view: &FileView, path: PathBuf, operations: OperationManager) {
    let parent = view.widget.root().and_downcast::<gtk::Window>();
    let dialog = adw::MessageDialog::new(
        parent.as_ref(),
        Some("Delete permanently?"),
        Some(&format!(
            "“{}” will be permanently deleted. This cannot be undone.",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("This item")
        )),
    );
    dialog.add_responses(&[("delete", "Delete Permanently"), ("cancel", "Cancel")]);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    let reload = view.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "delete" {
            let operation_id = operations.submit_confirmed_delete(vec![path.clone()]);
            schedule_refresh(reload.clone(), operations.clone(), operation_id);
        }
    });
    dialog.present();
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
            window.destroy();
        }
    });
    dialog.present();
}

fn navigate_tab(
    index: usize,
    path: PathBuf,
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    header: &HeaderControls,
    preferences: &Rc<RefCell<ViewPreferencesStore>>,
    sidebar: &Sidebar,
    record_history: bool,
) {
    if !path.is_dir() || index >= data.borrow().len() {
        return;
    }
    let mut data_ref = data.borrow_mut();
    let tab = &mut data_ref[index];
    if record_history {
        tab.history.borrow_mut().push(path.clone());
    }
    *tab.location.borrow_mut() = path.clone();
    tab.view.set_location(path.clone());
    tab.view
        .set_options(preferences.borrow().options_for(&path, &[]));
    tab.page.set_title(&tab_title(&path));
    if tabs
        .selected_page()
        .as_ref()
        .is_some_and(|page| *page == tab.page)
    {
        header.set_location(&path);
        sidebar.set_active_path(&path);
    }
}

fn navigate_uri_tab(
    index: usize,
    uri: String,
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    header: &HeaderControls,
    sidebar: &Sidebar,
) {
    let mut data_ref = data.borrow_mut();
    let Some(tab) = data_ref.get_mut(index) else {
        return;
    };
    tab.view.set_uri(uri.clone());
    tab.page.set_title(&uri_title(&uri));
    if tabs
        .selected_page()
        .as_ref()
        .is_some_and(|page| *page == tab.page)
    {
        header.set_uri(&uri);
        sidebar.clear_active_path();
    }
}

/// Reinstates the location hidden behind a virtual sidebar overview. A remote
/// location has no local history entry, so it must be restored from FileView's
/// retained URI rather than the tab's last local path.
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
    if let Some(uri) = tab.view.uri() {
        tab.page.set_title(&uri_title(&uri));
        if tabs
            .selected_page()
            .as_ref()
            .is_some_and(|page| *page == tab.page)
        {
            header.set_uri(&uri);
            sidebar.clear_active_path();
        }
    } else {
        let path = tab.location.borrow();
        tab.page.set_title(&tab_title(&path));
        if tabs
            .selected_page()
            .as_ref()
            .is_some_and(|page| *page == tab.page)
        {
            header.set_location(&path);
            sidebar.set_active_path(&path);
        }
    }
}

fn sidebar_path_for_tab(path: &Path, is_remote: bool) -> Option<PathBuf> {
    (!is_remote).then(|| path.to_path_buf())
}

fn sync_sidebar_for_tab(sidebar: &Sidebar, path: &Path, is_remote: bool) {
    if let Some(path) = sidebar_path_for_tab(path, is_remote) {
        sidebar.set_active_path(&path);
    } else {
        sidebar.clear_active_path();
    }
}

fn selected_index(tabs: &TabView, data: &Rc<RefCell<Vec<TabData>>>) -> Option<usize> {
    let page = tabs.selected_page()?;
    matching_index(&data.borrow(), |tab| tab.page == page)
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

fn tab_title(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "/".into())
}

fn uri_title(uri: &str) -> String {
    uri.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(uri)
        .to_owned()
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
    let path = tab.location.borrow().clone();
    tab.view.set_options(options.clone());
    persist_options(&mut preferences.borrow_mut(), &path, options);
}

fn reset_active_options(
    data: &Rc<RefCell<Vec<TabData>>>,
    tabs: &TabView,
    preferences: &Rc<RefCell<ViewPreferencesStore>>,
) {
    let Some(index) = selected_index(tabs, data) else {
        return;
    };
    let path = data.borrow()[index].location.borrow().clone();
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use gtk::prelude::{OrientableExt, PopoverExt, WidgetExt};
    use gtk::{Box as GtkBox, Label, Orientation, SearchEntry, Window};

    use super::{
        content_right_inset, file_toolbar, handle_navigation_action, history_with, matching_index,
        menu_button, persist_options, remove_matching, shortcut_command,
        should_protect_last_file_tab, sidebar_path_for_tab, TabData,
    };
    use crate::core::sidebar_model::SidebarSection;
    use crate::core::{
        SidebarModel, SidebarPreferences, SortField, ViewOptions, ViewPreferencesStore,
    };
    use crate::ui::context_menu::ContextCommand;
    use crate::ui::file_view::FileView;
    use crate::ui::header::{HeaderControls, NavigationAction};
    use crate::ui::sidebar::Sidebar;

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn back_from_a_remote_section_restores_the_header_tab_and_sidebar() {
        gtk::init().expect("GTK should initialize for widget tests");
        let path = PathBuf::from("/tmp");
        let location = Rc::new(RefCell::new(path.clone()));
        let view = FileView::new(location.clone());
        view.set_uri("smb://server/share".into());
        view.show_section_overview(SidebarSection {
            title: "Network & online".into(),
            nodes: vec![],
        });
        let tabs = adw::TabView::new();
        let page = tabs.append(&view.widget);
        tabs.set_selected_page(&page);
        page.set_title("Network & online");
        let data = Rc::new(RefCell::new(vec![TabData {
            view: view.clone(),
            location,
            history: history_with(path),
            page: page.clone(),
        }]));
        let header = HeaderControls::new(Rc::new(RefCell::new(PathBuf::from("/tmp"))));
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
        assert_eq!(page.title(), "share");
        assert_eq!(header.displayed_location_text(), "smb://server/share");
        assert_eq!(sidebar.active_navigation_selection(), (None, None));
    }

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn refresh_section_rereads_the_sidebar_model_without_losing_remote_state() {
        gtk::init().expect("GTK should initialize for widget tests");
        let path = PathBuf::from("/tmp");
        let location = Rc::new(RefCell::new(path.clone()));
        let view = FileView::new(location.clone());
        view.set_uri("smb://server/share".into());
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
            location,
            history: history_with(path),
            page,
        }]));
        let header = HeaderControls::new(Rc::new(RefCell::new(PathBuf::from("/tmp"))));
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
    }

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn refresh_remote_location_reloads_it_without_a_section_view() {
        gtk::init().expect("GTK should initialize for widget tests");
        let path = PathBuf::from("/tmp");
        let location = Rc::new(RefCell::new(path.clone()));
        let view = FileView::new(location.clone());
        view.set_uri("smb://server/share".into());
        view.show_search_results(vec![]);
        let tabs = adw::TabView::new();
        let page = tabs.append(&view.widget);
        tabs.set_selected_page(&page);
        let data = Rc::new(RefCell::new(vec![TabData {
            view: view.clone(),
            location,
            history: history_with(path),
            page,
        }]));
        let header = HeaderControls::new(Rc::new(RefCell::new(PathBuf::from("/tmp"))));
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
}
