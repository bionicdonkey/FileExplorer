use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use adw::{Application, ApplicationWindow, HeaderBar, TabPage, TabView, ToolbarView};
use gtk::{Align, Box as GtkBox, Button, Label, MenuButton, Orientation, Paned, Popover, SearchEntry};

use crate::core::location::Location;
use crate::core::navigation::NavigationHistory;
use crate::core::{GridSize, GroupField, SortDirection, SortField, ViewMode, ViewPreferencesStore};
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
        let preferences = Rc::new(RefCell::new(ViewPreferencesStore::load_default().unwrap_or_else(|_| ViewPreferencesStore::in_memory())));
        let first_view = FileView::new(initial_location.clone());
        first_view.set_options(preferences.borrow().options_for(&initial, &[]));
        let first_page = tabs.view.append(&first_view.widget);
        first_page.set_title(&tab_title(&initial));
        tabs_data.borrow_mut().push(TabData {
            view: first_view,
            location: initial_location,
            history: history_with(initial),
            page: first_page.clone(),
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
        let sidebar_preferences = preferences.clone();
        let sidebar = Sidebar::new(move |path| {
            if let Some(index) = selected_index(&sidebar_tabs, &sidebar_data) {
                navigate_tab(index, path, &sidebar_data, &sidebar_tabs, &sidebar_header, &sidebar_preferences, true);
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
        body.append(&view_controls(&tabs_data, &tabs.view, &preferences));
        body.append(&search);
        body.append(&content);
        body.append(&status);
        let toolbar = ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&body));
        window.set_content(Some(&toolbar));

        wire_tab(first_page.clone(), &tabs_data, &tabs.view, &header_controls, &preferences);

        let new_data = tabs_data.clone();
        let new_tabs = tabs.view.clone();
        let new_header = header_controls.clone();
        let new_preferences = preferences.clone();
        new_tab.connect_clicked(move |_| {
            let path = Location::home().path().to_path_buf();
            let location = Rc::new(RefCell::new(path.clone()));
            let view = FileView::new(location.clone());
            view.set_options(new_preferences.borrow().options_for(&path, &[]));
            let page = new_tabs.append(&view.widget);
            page.set_title(&tab_title(&path));
            new_data.borrow_mut().push(TabData { view, location, history: history_with(path), page: page.clone() });
            wire_tab(page.clone(), &new_data, &new_tabs, &new_header, &new_preferences);
            new_tabs.set_selected_page(&page);
        });

        let close_data = tabs_data.clone();
        tabs.view.connect_close_page(move |view, page| {
            if close_data.borrow().len() <= 1 {
                return false;
            }
            remove_matching(&close_data, |tab| tab.page == *page);
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
        let navigation_preferences = preferences.clone();
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
                navigate_tab(index, path, &navigation_data, &navigation_tabs, &navigation_header, &navigation_preferences, !matches!(action, NavigationAction::Back | NavigationAction::Forward | NavigationAction::Refresh));
            }
        });

        let breadcrumb_data = tabs_data.clone();
        let breadcrumb_tabs = tabs.view.clone();
        let breadcrumb_header = header_controls.clone();
        let breadcrumb_preferences = preferences.clone();
        header_controls.set_navigation_handler(move |path| {
            if let Some(index) = selected_index(&breadcrumb_tabs, &breadcrumb_data) {
                navigate_tab(index, path, &breadcrumb_data, &breadcrumb_tabs, &breadcrumb_header, &breadcrumb_preferences, true);
            }
        });

        let entry_data = tabs_data.clone();
        let entry_tabs = tabs.view.clone();
        let entry_header = header_controls.clone();
        let entry_preferences = preferences.clone();
        header_controls.connect_location_changed(move |path| {
            if let Some(index) = selected_index(&entry_tabs, &entry_data) {
                navigate_tab(index, path, &entry_data, &entry_tabs, &entry_header, &entry_preferences, true);
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

fn wire_tab(page: TabPage, data: &Rc<RefCell<Vec<TabData>>>, tabs: &TabView, header: &HeaderControls, preferences: &Rc<RefCell<ViewPreferencesStore>>) {
    let view = data.borrow().iter().find(|tab| tab.page == page).map(|tab| tab.view.clone())
        .expect("a tab page should have corresponding tab data");
    let data = data.clone();
    let tabs = tabs.clone();
    let header = header.clone();
    let preferences = preferences.clone();
    view.set_navigation_handler(move |path| {
        let index = { data.borrow().iter().position(|tab| tab.page == page) };
        if let Some(index) = index {
            navigate_tab(index, path, &data, &tabs, &header, &preferences, true);
        }
    });
}

fn navigate_tab(index: usize, path: PathBuf, data: &Rc<RefCell<Vec<TabData>>>, tabs: &TabView, header: &HeaderControls, preferences: &Rc<RefCell<ViewPreferencesStore>>, record_history: bool) {
    if !path.is_dir() || index >= data.borrow().len() { return; }
    let mut data_ref = data.borrow_mut();
    let tab = &mut data_ref[index];
    if record_history { tab.history.borrow_mut().push(path.clone()); }
    *tab.location.borrow_mut() = path.clone();
    tab.view.set_location(path.clone());
    tab.view.set_options(preferences.borrow().options_for(&path, &[]));
    tab.page.set_title(&tab_title(&path));
    if tabs.selected_page().as_ref().is_some_and(|page| *page == tab.page) {
        header.set_location(&path);
    }
}

fn selected_index(tabs: &TabView, data: &Rc<RefCell<Vec<TabData>>>) -> Option<usize> {
    let page = tabs.selected_page()?;
    matching_index(&data.borrow(), |tab| tab.page == page)
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

fn view_controls(data: &Rc<RefCell<Vec<TabData>>>, tabs: &TabView, preferences: &Rc<RefCell<ViewPreferencesStore>>) -> GtkBox {
    let controls = GtkBox::new(Orientation::Horizontal, 6);
    controls.set_margin_start(8); controls.set_margin_end(8); controls.set_margin_bottom(6);
    for (label, entries) in [
        ("Sort", vec!["Name", "Date modified", "Type", "Size", "Ascending", "Descending", "Toggle folders first"]),
        ("Group", vec!["None", "Type", "Size", "Date modified"]),
        ("View", vec!["Detailed List", "Compact List", "Icon Grid", "Small icons", "Medium icons", "Large icons", "Extra Large icons", "Toggle thumbnails"]),
    ] {
        let button = MenuButton::builder().label(label).build();
        let popover = Popover::new(); let menu = GtkBox::new(Orientation::Vertical, 2); menu.set_margin_top(6); menu.set_margin_bottom(6);
        for entry in entries { let action = Button::with_label(entry); action.add_css_class("flat"); let data = data.clone(); let tabs = tabs.clone(); let preferences = preferences.clone(); let entry = format!("{label}:{entry}");
            action.connect_clicked(move |_| update_active_options(&data, &tabs, &preferences, &entry)); menu.append(&action); }
        popover.set_child(Some(&menu)); button.set_popover(Some(&popover)); controls.append(&button);
    }
    controls
}

fn update_active_options(data: &Rc<RefCell<Vec<TabData>>>, tabs: &TabView, preferences: &Rc<RefCell<ViewPreferencesStore>>, entry: &str) {
    let Some(index) = selected_index(tabs, data) else { return; };
    let mut data = data.borrow_mut(); let tab = &mut data[index]; let mut options = tab.view.options();
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
        "View:Small icons" => { options.view_mode = ViewMode::IconGrid; options.grid_size = GridSize::Small; }
        "View:Medium icons" => { options.view_mode = ViewMode::IconGrid; options.grid_size = GridSize::Medium; }
        "View:Large icons" => { options.view_mode = ViewMode::IconGrid; options.grid_size = GridSize::Large; }
        "View:Extra Large icons" => { options.view_mode = ViewMode::IconGrid; options.grid_size = GridSize::ExtraLarge; }
        "View:Toggle thumbnails" => options.thumbnails = !options.thumbnails,
        _ => return,
    }
    let path = tab.location.borrow().clone(); tab.view.set_options(options.clone());
    preferences.borrow_mut().set(&path, options); let _ = preferences.borrow().save();
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::{matching_index, remove_matching};

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
}
