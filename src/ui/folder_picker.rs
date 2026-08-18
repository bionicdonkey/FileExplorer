use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use adw::{HeaderBar, ToolbarView};
use gtk::{Box as GtkBox, Button, Entry, Image, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow, SelectionMode, Separator};

use crate::core::location::Location;
use crate::core::navigation::NavigationHistory;
use crate::core::operations::FileOperations;
use crate::core::{SidebarLocation, SidebarModel};

/// An in-process folder picker built from this app's own widgets, so
/// choosing a destination never hands off to an external file manager. The
/// shortcuts pane mirrors the real sidebar (same sections, pins, devices,
/// and network locations); remote/GVfs entries are shown for consistency
/// but disabled, since copy/move only supports local destinations.
///
/// Built on `adw::Dialog` + `adw::ToolbarView` (the same composition the
/// main window uses), not the deprecated `gtk::Dialog` action-area, so the
/// chrome (header bar, rounded corners, bottom action bar) matches the rest
/// of the app instead of looking like an unstyled GTK3-era dialog.
pub fn pick_folder<F: Fn(PathBuf) + 'static>(parent: &impl IsA<gtk::Widget>, title: &str, action_label: &str, initial: PathBuf, sidebar_model: Rc<RefCell<SidebarModel>>, on_selected: F) {
    let dialog = adw::Dialog::new();
    dialog.set_title(title);
    dialog.set_content_width(760);
    dialog.set_content_height(520);

    let toolbar_view = ToolbarView::new();
    let titlebar = HeaderBar::new();
    titlebar.set_show_start_title_buttons(false);
    titlebar.set_show_end_title_buttons(false);
    titlebar.set_title_widget(Some(&Label::new(Some(title))));
    toolbar_view.add_top_bar(&titlebar);

    let start = if initial.is_dir() { initial } else { Location::home().path().to_path_buf() };
    let location = Rc::new(RefCell::new(start.clone()));
    let selected: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let history = Rc::new(RefCell::new(NavigationHistory::new()));
    history.borrow_mut().push(start);
    let listed_folders: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));

    let content = GtkBox::new(Orientation::Vertical, 0);

    let toolbar = GtkBox::new(Orientation::Horizontal, 6);
    toolbar.set_margin_top(8); toolbar.set_margin_bottom(8); toolbar.set_margin_start(8); toolbar.set_margin_end(8);
    let back = Button::from_icon_name("go-previous-symbolic"); back.add_css_class("flat"); back.set_tooltip_text(Some("Go back"));
    let forward = Button::from_icon_name("go-next-symbolic"); forward.add_css_class("flat"); forward.set_tooltip_text(Some("Go forward"));
    let up = Button::from_icon_name("go-up-symbolic"); up.add_css_class("flat"); up.set_tooltip_text(Some("Go up"));
    let path_entry = Entry::new(); path_entry.set_hexpand(true);
    let new_folder = Button::from_icon_name("folder-new-symbolic"); new_folder.add_css_class("flat"); new_folder.set_tooltip_text(Some("New Folder"));
    for button in [&back, &forward, &up] { toolbar.append(button); }
    toolbar.append(&path_entry);
    toolbar.append(&new_folder);
    content.append(&toolbar);
    content.append(&Separator::new(Orientation::Horizontal));

    let body = GtkBox::new(Orientation::Horizontal, 0);
    body.set_vexpand(true);

    let shortcuts = GtkBox::new(Orientation::Vertical, 2);
    shortcuts.add_css_class("explorer-sidebar");
    shortcuts.set_margin_top(4); shortcuts.set_margin_bottom(4); shortcuts.set_margin_start(4); shortcuts.set_margin_end(4);
    let shortcuts_scroll = ScrolledWindow::new();
    shortcuts_scroll.set_child(Some(&shortcuts));
    shortcuts_scroll.set_width_request(200);
    shortcuts_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    body.append(&shortcuts_scroll);
    body.append(&Separator::new(Orientation::Vertical));

    let folder_list = ListBox::new();
    folder_list.set_selection_mode(SelectionMode::Single);
    folder_list.set_activate_on_single_click(false);
    let folder_scroll = ScrolledWindow::new();
    folder_scroll.set_child(Some(&folder_list));
    folder_scroll.set_hexpand(true);
    body.append(&folder_scroll);
    content.append(&body);

    let navigate: Rc<dyn Fn(PathBuf, bool)> = {
        let location = location.clone();
        let history = history.clone();
        let selected = selected.clone();
        let path_entry = path_entry.clone();
        let back = back.clone();
        let forward = forward.clone();
        let up = up.clone();
        let folder_list = folder_list.clone();
        let listed_folders = listed_folders.clone();
        Rc::new(move |path: PathBuf, record_history: bool| {
            if !path.is_dir() { return; }
            *location.borrow_mut() = path.clone();
            *selected.borrow_mut() = None;
            if record_history { history.borrow_mut().push(path.clone()); }
            path_entry.set_text(&path.display().to_string());
            back.set_sensitive(history.borrow().can_go_back());
            forward.set_sensitive(history.borrow().can_go_forward());
            up.set_sensitive(path.parent().is_some());
            while let Some(row) = folder_list.row_at_index(0) { folder_list.remove(&row); }
            let mut folders = listed_folders.borrow_mut();
            *folders = subfolders(&path);
            for folder in folders.iter() { folder_list.append(&folder_row(folder)); }
        })
    };

    // Connected once: row creation/removal happens inside `navigate`, but the
    // ListBox itself persists, so these must not be reconnected on refresh.
    folder_list.connect_row_selected({
        let selected = selected.clone();
        let listed_folders = listed_folders.clone();
        move |_, row| { *selected.borrow_mut() = row.map(|row| row.index()).and_then(|index| listed_folders.borrow().get(index as usize).cloned()); }
    });
    folder_list.connect_row_activated({
        let listed_folders = listed_folders.clone();
        let navigate = navigate.clone();
        // `if let Some(x) = refcell.borrow()... { }` keeps the borrow alive for
        // the whole body (a temporary-lifetime-extension gotcha), so `navigate`
        // below would re-borrow `listed_folders` while this one is still held.
        // Bind the owned value first so the borrow ends before `navigate` runs.
        move |_, row| {
            let path = listed_folders.borrow().get(row.index() as usize).cloned();
            if let Some(path) = path { navigate(path, true); }
        }
    });

    for section in sidebar_model.borrow().sections() {
        if section.nodes.is_empty() { continue; }
        let heading = Label::new(Some(&section.title));
        heading.add_css_class("heading");
        heading.add_css_class("dim-label");
        heading.set_xalign(0.0);
        heading.set_margin_top(10);
        heading.set_margin_start(6);
        shortcuts.append(&heading);
        for node in section.nodes {
            shortcuts.append(&shortcut_button(&node.location, &navigate));
        }
    }

    // Same temporary-lifetime-extension pitfall as above: bind the owned
    // path first so the RefCell borrow ends before `navigate` re-borrows it.
    back.connect_clicked({
        let history = history.clone();
        let navigate = navigate.clone();
        move |_| {
            let path = history.borrow_mut().back().map(Path::to_path_buf);
            if let Some(path) = path { navigate(path, false); }
        }
    });
    forward.connect_clicked({
        let history = history.clone();
        let navigate = navigate.clone();
        move |_| {
            let path = history.borrow_mut().forward().map(Path::to_path_buf);
            if let Some(path) = path { navigate(path, false); }
        }
    });
    up.connect_clicked({
        let location = location.clone();
        let navigate = navigate.clone();
        move |_| {
            let parent = location.borrow().parent().map(Path::to_path_buf);
            if let Some(parent) = parent { navigate(parent, true); }
        }
    });
    path_entry.connect_activate({
        let navigate = navigate.clone();
        move |entry| navigate(PathBuf::from(entry.text().as_str()), true)
    });
    new_folder.connect_clicked({
        let location = location.clone();
        let navigate = navigate.clone();
        move |_| {
            let created = FileOperations::create_folder(&location.borrow(), "New Folder").ok();
            if let Some(path) = created { navigate(path, true); }
        }
    });

    // Same pitfall: `location.borrow()` must not still be a live temporary
    // when `navigate` tries to re-borrow `location`, so bind it first.
    let start_path = location.borrow().clone();
    navigate(start_path, false);

    toolbar_view.set_content(Some(&content));

    toolbar_view.set_bottom_bar_style(adw::ToolbarStyle::RaisedBorder);
    let actions = GtkBox::new(Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    actions.set_margin_top(8); actions.set_margin_bottom(8); actions.set_margin_start(12); actions.set_margin_end(12);
    let cancel_button = Button::with_label("Cancel");
    let select_button = Button::with_label(action_label);
    select_button.add_css_class("suggested-action");
    actions.append(&cancel_button);
    actions.append(&select_button);
    toolbar_view.add_bottom_bar(&actions);

    dialog.set_child(Some(&toolbar_view));

    cancel_button.connect_clicked({
        let dialog = dialog.clone();
        move |_| { dialog.close(); }
    });
    select_button.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            let chosen = selected.borrow().clone().unwrap_or_else(|| location.borrow().clone());
            on_selected(chosen);
            dialog.close();
        }
    });

    dialog.present(parent);
}

fn folder_row(path: &Path) -> ListBoxRow {
    let row = ListBoxRow::new();
    let content = GtkBox::new(Orientation::Horizontal, 8);
    content.set_margin_top(6); content.set_margin_bottom(6); content.set_margin_start(8); content.set_margin_end(8);
    let icon = Image::from_icon_name("folder-symbolic"); icon.set_pixel_size(18);
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("").to_owned();
    let label = Label::new(Some(&name)); label.set_xalign(0.0); label.set_hexpand(true); label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&icon);
    content.append(&label);
    row.set_child(Some(&content));
    row
}

/// A single sidebar row. Local locations navigate the picker; remote/GVfs
/// locations are shown (for parity with the real sidebar) but disabled,
/// since this app's copy/move pipeline only supports local destinations.
fn shortcut_button(location: &SidebarLocation, navigate: &Rc<dyn Fn(PathBuf, bool)>) -> Button {
    let button = Button::new();
    button.add_css_class("flat");
    button.add_css_class("sidebar-location");
    let content = GtkBox::new(Orientation::Horizontal, 8);
    content.set_margin_top(4); content.set_margin_bottom(4); content.set_margin_start(4); content.set_margin_end(4);
    let icon = Image::from_icon_name(location.icon_name()); icon.set_pixel_size(18); icon.add_css_class("sidebar-location-icon");
    let label = Label::new(Some(location.label())); label.set_xalign(0.0); label.set_hexpand(true); label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&icon);
    content.append(&label);
    button.set_child(Some(&content));
    match location.local_path() {
        Some(path) => {
            let path = path.to_path_buf();
            let navigate = navigate.clone();
            button.connect_clicked(move |_| navigate(path.clone(), true));
        }
        None => {
            button.set_sensitive(false);
            button.set_tooltip_text(Some("Remote locations aren't supported as a destination yet"));
        }
    }
    button
}

fn subfolders(path: &Path) -> Vec<PathBuf> {
    let mut folders = std::fs::read_dir(path).map(|entries| {
        entries.flatten()
            .filter(|entry| entry.path().is_dir())
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| entry.path())
            .collect::<Vec<_>>()
    }).unwrap_or_default();
    folders.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    folders
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subfolders_are_sorted_and_exclude_hidden_and_files() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("b")).unwrap();
        std::fs::create_dir(temporary.path().join("a")).unwrap();
        std::fs::create_dir(temporary.path().join(".hidden")).unwrap();
        std::fs::write(temporary.path().join("file.txt"), "x").unwrap();

        let found = subfolders(temporary.path());

        assert_eq!(found, vec![temporary.path().join("a"), temporary.path().join("b")]);
    }
}
