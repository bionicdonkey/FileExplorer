use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use adw::{Application, ApplicationWindow, HeaderBar, TabPage, TabView, ToolbarView};
use gtk::{Align, Box as GtkBox, Button, Label, Orientation, Paned, SearchEntry};

use crate::core::location::Location;
use crate::core::navigation::NavigationHistory;
use crate::ui::file_view::FileView;
use crate::ui::header::{HeaderControls, NavigationAction};
use crate::ui::sidebar::Sidebar;
use crate::ui::tab_layout::{calculate_tab_layout, TabLayoutInput};
use crate::ui::tabs::TabStrip;

struct TabData {
    view: FileView,
    location: Rc<RefCell<PathBuf>>,
    history: Rc<RefCell<NavigationHistory>>,
    page: TabPage,
}

pub struct ExplorerWindow {
    window: ApplicationWindow,
}

impl ExplorerWindow {
    pub fn new(app: &Application) -> Self {
        let initial = Location::home().path().to_path_buf();
        let window = ApplicationWindow::builder()
            .application(app)
            .title("Zorin Explorer")
            .default_width(1280)
            .default_height(800)
            .build();

        let header = HeaderBar::new();
        header.set_show_title(false);
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

        let initial_location = Rc::new(RefCell::new(initial.clone()));
        let header_controls = HeaderControls::new(initial_location.clone());
        let tabs_data: Rc<RefCell<Vec<TabData>>> = Rc::new(RefCell::new(Vec::new()));
        let first_view = FileView::new(initial_location.clone());
        let first_page = tabs.view.append(&first_view.widget);
        first_page.set_title(&tab_title(&initial));
        tabs_data.borrow_mut().push(TabData {
            view: first_view,
            location: initial_location,
            history: history_with(initial),
            page: first_page,
        });

        apply_tab_layout(&window, &tabs);
        let layout_window = window.clone();
        let layout_tabs = tabs.clone();
        tabs.view.connect_n_pages_notify(move |_| {
            apply_tab_layout(&layout_window, &layout_tabs);
        });

        let sidebar_data = tabs_data.clone();
        let sidebar_tabs = tabs.view.clone();
        let sidebar_header = header_controls.clone();
        let sidebar = Sidebar::new(move |path| {
            if let Some(index) = selected_index(&sidebar_tabs, &sidebar_data) {
                navigate_tab(index, path, &sidebar_data, &sidebar_tabs, &sidebar_header, true);
            }
        });

        let content = Paned::new(Orientation::Horizontal);
        content.set_position(250);
        content.set_start_child(Some(&sidebar.widget));
        content.set_end_child(Some(&tabs.view));
        content.set_shrink_start_child(false);
        content.set_shrink_end_child(false);
        content.set_margin_end(12);

        let search = SearchEntry::builder().placeholder_text("Search this folder").hexpand(true).build();
        search.set_margin_start(8);
        search.set_margin_end(8);
        search.set_margin_bottom(6);
        let status = Label::new(Some("Ready"));
        status.set_halign(Align::Start);
        status.add_css_class("dim-label");
        status.set_margin_start(8);
        status.set_margin_end(8);
        status.set_margin_bottom(6);

        let body = GtkBox::new(Orientation::Vertical, 0);
        body.append(&header_controls.widget);
        body.append(&search);
        body.append(&content);
        body.append(&status);
        let toolbar = ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&body));
        window.set_content(Some(&toolbar));

        wire_tab(0, &tabs_data, &tabs.view, &header_controls);

        let new_data = tabs_data.clone();
        let new_tabs = tabs.view.clone();
        let new_header = header_controls.clone();
        new_tab.connect_clicked(move |_| {
            let path = Location::home().path().to_path_buf();
            let location = Rc::new(RefCell::new(path.clone()));
            let view = FileView::new(location.clone());
            let page = new_tabs.append(&view.widget);
            page.set_title(&tab_title(&path));
            let index = new_data.borrow().len();
            new_data.borrow_mut().push(TabData { view, location, history: history_with(path), page: page.clone() });
            wire_tab(index, &new_data, &new_tabs, &new_header);
            new_tabs.set_selected_page(&page);
        });

        let close_data = tabs_data.clone();
        tabs.view.connect_close_page(move |view, page| {
            if close_data.borrow().len() <= 1 {
                return false;
            }
            if let Some(index) = close_data.borrow().iter().position(|tab| tab.page == *page) {
                close_data.borrow_mut().remove(index);
            }
            view.close_page_finish(page, true);
            true
        });

        let select_data = tabs_data.clone();
        let select_header = header_controls.clone();
        tabs.view.connect_selected_page_notify(move |view| {
            if let Some(index) = selected_index(view, &select_data) {
                let path = select_data.borrow()[index].location.borrow().clone();
                select_header.set_location(&path);
            }
        });

        let navigation_data = tabs_data.clone();
        let navigation_tabs = tabs.view.clone();
        let navigation_header = header_controls.clone();
        header_controls.connect_navigation(move |action| {
            let Some(index) = selected_index(&navigation_tabs, &navigation_data) else { return; };
            let next = {
                let data = navigation_data.borrow_mut();
                match action {
                    NavigationAction::Back => data[index].history.borrow_mut().back().map(ToOwned::to_owned),
                    NavigationAction::Forward => data[index].history.borrow_mut().forward().map(ToOwned::to_owned),
                    NavigationAction::Up => data[index].location.borrow().parent().map(ToOwned::to_owned),
                    NavigationAction::Refresh => Some(data[index].location.borrow().clone()),
                }
            };
            if let Some(path) = next {
                navigate_tab(index, path, &navigation_data, &navigation_tabs, &navigation_header, !matches!(action, NavigationAction::Back | NavigationAction::Forward | NavigationAction::Refresh));
            }
        });

        let breadcrumb_data = tabs_data.clone();
        let breadcrumb_tabs = tabs.view.clone();
        let breadcrumb_header = header_controls.clone();
        header_controls.set_navigation_handler(move |path| {
            if let Some(index) = selected_index(&breadcrumb_tabs, &breadcrumb_data) {
                navigate_tab(index, path, &breadcrumb_data, &breadcrumb_tabs, &breadcrumb_header, true);
            }
        });

        let entry_data = tabs_data.clone();
        let entry_tabs = tabs.view.clone();
        let entry_header = header_controls.clone();
        header_controls.connect_location_changed(move |path| {
            if let Some(index) = selected_index(&entry_tabs, &entry_data) {
                navigate_tab(index, path, &entry_data, &entry_tabs, &entry_header, true);
            }
        });

        Self { window }
    }

    pub fn present(&self) {
        self.window.present();
    }
}

fn history_with(path: PathBuf) -> Rc<RefCell<NavigationHistory>> {
    let history = Rc::new(RefCell::new(NavigationHistory::new()));
    history.borrow_mut().push(path);
    history
}

fn wire_tab(index: usize, data: &Rc<RefCell<Vec<TabData>>>, tabs: &TabView, header: &HeaderControls) {
    let view = data.borrow()[index].view.clone();
    let data = data.clone();
    let tabs = tabs.clone();
    let header = header.clone();
    view.set_navigation_handler(move |path| navigate_tab(index, path, &data, &tabs, &header, true));
}

fn navigate_tab(index: usize, path: PathBuf, data: &Rc<RefCell<Vec<TabData>>>, tabs: &TabView, header: &HeaderControls, record_history: bool) {
    if !path.is_dir() || index >= data.borrow().len() { return; }
    let mut data_ref = data.borrow_mut();
    let tab = &mut data_ref[index];
    if record_history { tab.history.borrow_mut().push(path.clone()); }
    *tab.location.borrow_mut() = path.clone();
    tab.view.set_location(path.clone());
    tab.page.set_title(&tab_title(&path));
    if tabs.selected_page().as_ref().is_some_and(|page| *page == tab.page) {
        header.set_location(&path);
    }
}

fn selected_index(tabs: &TabView, data: &Rc<RefCell<Vec<TabData>>>) -> Option<usize> {
    let page = tabs.selected_page()?;
    data.borrow().iter().position(|tab| tab.page == page)
}

fn tab_title(path: &std::path::Path) -> String {
    path.file_name().and_then(|name| name.to_str()).map(ToOwned::to_owned).unwrap_or_else(|| "/".into())
}

fn apply_tab_layout(window: &ApplicationWindow, tabs: &TabStrip) {
    let window_width = window.width();
    let layout = calculate_tab_layout(TabLayoutInput::new(
        tabs.view.n_pages(),
        if window_width > 0 { window_width } else { 1280 },
    ));

    tabs.set_layout(layout);
}
