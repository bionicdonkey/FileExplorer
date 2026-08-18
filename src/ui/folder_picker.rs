use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use adw::HeaderBar;
use gtk::{Box as GtkBox, Button, Entry, Image, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow, SelectionMode, Separator};

use crate::core::location::Location;
use crate::core::navigation::NavigationHistory;
use crate::core::operations::FileOperations;

/// An in-process folder picker built from this app's own widgets, so
/// choosing a destination never hands off to an external file manager.
#[allow(deprecated)]
pub fn pick_folder<F: Fn(PathBuf) + 'static>(parent: &impl IsA<gtk::Window>, title: &str, action_label: &str, initial: PathBuf, on_selected: F) {
    let dialog = gtk::Dialog::new();
    dialog.set_transient_for(Some(parent));
    dialog.set_modal(true);
    dialog.set_resizable(true);
    dialog.add_css_class("explorer-dialog");
    dialog.set_default_size(720, 480);
    let titlebar = HeaderBar::new();
    titlebar.set_show_start_title_buttons(false);
    titlebar.set_show_end_title_buttons(false);
    titlebar.set_title_widget(Some(&Label::new(Some(title))));
    dialog.set_titlebar(Some(&titlebar));
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    let select_button = dialog.add_button(action_label, gtk::ResponseType::Accept);
    select_button.add_css_class("suggested-action");
    dialog.set_default_response(gtk::ResponseType::Accept);

    let start = if initial.is_dir() { initial } else { Location::home().path().to_path_buf() };
    let location = Rc::new(RefCell::new(start.clone()));
    let selected: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let history = Rc::new(RefCell::new(NavigationHistory::new()));
    history.borrow_mut().push(start);
    let listed_folders: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));

    let content = dialog.content_area();
    content.set_orientation(Orientation::Vertical);

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

    let shortcuts = ListBox::new();
    shortcuts.set_selection_mode(SelectionMode::None);
    shortcuts.add_css_class("navigation-sidebar");
    let shortcuts_scroll = ScrolledWindow::new();
    shortcuts_scroll.set_child(Some(&shortcuts));
    shortcuts_scroll.set_width_request(180);
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
        move |_, row| { if let Some(path) = listed_folders.borrow().get(row.index() as usize).cloned() { navigate(path, true); } }
    });

    for (label, path) in shortcuts_for(&Location::home().path().to_path_buf()) {
        let row = shortcut_row(&label, &path);
        let gesture = gtk::GestureClick::new();
        let navigate = navigate.clone();
        gesture.connect_released(move |_, _, _, _| navigate(path.clone(), true));
        row.add_controller(gesture);
        shortcuts.append(&row);
    }

    back.connect_clicked({
        let history = history.clone();
        let navigate = navigate.clone();
        move |_| { if let Some(path) = history.borrow_mut().back().map(Path::to_path_buf) { navigate(path, false); } }
    });
    forward.connect_clicked({
        let history = history.clone();
        let navigate = navigate.clone();
        move |_| { if let Some(path) = history.borrow_mut().forward().map(Path::to_path_buf) { navigate(path, false); } }
    });
    up.connect_clicked({
        let location = location.clone();
        let navigate = navigate.clone();
        move |_| { if let Some(parent) = location.borrow().parent().map(Path::to_path_buf) { navigate(parent, true); } }
    });
    path_entry.connect_activate({
        let navigate = navigate.clone();
        move |entry| navigate(PathBuf::from(entry.text().as_str()), true)
    });
    new_folder.connect_clicked({
        let location = location.clone();
        let navigate = navigate.clone();
        move |_| {
            if let Ok(path) = FileOperations::create_folder(&location.borrow(), "New Folder") { navigate(path, true); }
        }
    });

    navigate(location.borrow().clone(), false);

    dialog.connect_response({
        let location = location.clone();
        let selected = selected.clone();
        move |dialog, response| {
            if response == gtk::ResponseType::Accept {
                let chosen = selected.borrow().clone().unwrap_or_else(|| location.borrow().clone());
                on_selected(chosen);
            }
            dialog.destroy();
        }
    });
    dialog.present();
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

fn shortcut_row(label: &str, path: &Path) -> ListBoxRow {
    let row = ListBoxRow::new();
    row.set_selectable(false);
    let content = GtkBox::new(Orientation::Horizontal, 8);
    content.set_margin_top(6); content.set_margin_bottom(6); content.set_margin_start(8); content.set_margin_end(8);
    let icon = Image::from_icon_name(standard_folder_icon(path, label)); icon.set_pixel_size(18);
    let text = Label::new(Some(label)); text.set_xalign(0.0); text.set_hexpand(true); text.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&icon);
    content.append(&text);
    row.set_child(Some(&content));
    row
}

pub fn shortcuts_for(home: &Path) -> Vec<(String, PathBuf)> {
    let mut shortcuts = vec![("Home".to_owned(), home.to_path_buf())];
    for name in ["Desktop", "Documents", "Downloads", "Pictures"] {
        let path = home.join(name);
        if path.exists() { shortcuts.push((name.to_owned(), path)); }
    }
    shortcuts.push(("This PC".to_owned(), PathBuf::from("/")));
    shortcuts
}

fn standard_folder_icon(path: &Path, label: &str) -> &'static str {
    if label == "This PC" { return "drive-harddisk-symbolic"; }
    match path.file_name().and_then(|name| name.to_str()) {
        Some("Desktop") => "user-desktop-symbolic",
        Some("Documents") => "folder-documents-symbolic",
        Some("Downloads") => "folder-download-symbolic",
        Some("Pictures") => "folder-pictures-symbolic",
        _ if label == "Home" => "user-home-symbolic",
        _ => "folder-symbolic",
    }
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
    fn shortcuts_always_include_home_and_this_pc() {
        let shortcuts = shortcuts_for(Path::new("/tmp/does-not-exist"));
        assert_eq!(shortcuts.first(), Some(&("Home".to_owned(), PathBuf::from("/tmp/does-not-exist"))));
        assert_eq!(shortcuts.last(), Some(&("This PC".to_owned(), PathBuf::from("/"))));
    }

    #[test]
    fn shortcuts_only_include_folders_that_exist() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("Desktop")).unwrap();

        let shortcuts = shortcuts_for(temporary.path());

        assert!(shortcuts.iter().any(|(label, _)| label == "Desktop"));
        assert!(!shortcuts.iter().any(|(label, _)| label == "Documents"));
    }

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
