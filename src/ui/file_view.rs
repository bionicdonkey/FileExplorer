use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Image, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow};

use crate::core::directory_model::DirectoryModel;
use crate::core::file_item::FileItem;

#[derive(Clone)]
pub struct FileView {
    pub widget: ScrolledWindow,
    list: ListBox,
    location: Rc<RefCell<PathBuf>>,
    paths: Rc<RefCell<Vec<PathBuf>>>,
    on_navigate: Rc<RefCell<Option<Box<dyn Fn(PathBuf)>>>>,
}

impl FileView {
    pub fn new(location: Rc<RefCell<PathBuf>>) -> Self {
        let list = ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_show_separators(true);
        list.add_css_class("boxed-list");
        let scroll = ScrolledWindow::new();
        scroll.set_vexpand(true);
        scroll.set_margin_end(12);
        scroll.set_child(Some(&list));
        let paths = Rc::new(RefCell::new(Vec::new()));
        let on_navigate = Rc::new(RefCell::new(None));
        let view = Self { widget: scroll, list, location, paths, on_navigate };
        let activation_paths = view.paths.clone();
        let activation_view = view.clone();
        view.list.connect_row_activated(move |_, row| {
            let Some(path) = activation_paths.borrow().get(row.index() as usize).cloned() else {
                return;
            };
            activate_path(&activation_view, path);
        });
        view.reload();
        view
    }

    pub fn set_location(&self, path: PathBuf) {
        *self.location.borrow_mut() = path;
        self.reload();
    }

    pub fn set_navigation_handler<F: Fn(PathBuf) + 'static>(&self, callback: F) {
        *self.on_navigate.borrow_mut() = Some(Box::new(callback));
    }

    fn reload(&self) {
        self.paths.borrow_mut().clear();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        match DirectoryModel::load(&self.location.borrow()) {
            Ok(items) if items.is_empty() => {
                let row = ListBoxRow::new();
                row.set_child(Some(&Label::new(Some("This folder is empty"))));
                self.list.append(&row);
            }
            Ok(items) => {
                for item in items {
                    self.paths.borrow_mut().push(item.path().to_path_buf());
                    let row = ListBoxRow::new();
                    row.set_activatable(true);
                    row.set_selectable(true);
                    let row_view = self.clone();
                    let row_path = item.path().to_path_buf();
                    row.connect_activate(move |_| activate_path(&row_view, row_path.clone()));
                    let line = GtkBox::new(Orientation::Horizontal, 12);
                    line.set_margin_top(8);
                    line.set_margin_bottom(8);
                    line.set_margin_start(12);
                    line.set_margin_end(12);
                    let icon = file_icon(&item);
                    icon.set_pixel_size(20);
                    icon.set_valign(gtk::Align::Center);
                    let name = Label::new(Some(item.name()));
                    name.set_hexpand(true);
                    name.set_xalign(0.0);
                    let kind = Label::new(Some(if item.is_dir() { "Folder" } else { "File" }));
                    kind.add_css_class("dim-label");
                    kind.set_xalign(1.0);
                    line.append(&icon);
                    line.append(&name);
                    line.append(&kind);
                    row.set_child(Some(&line));
                    self.list.append(&row);
                }
            }
            Err(error) => {
                let row = ListBoxRow::new();
                row.set_child(Some(&Label::new(Some(&format!("Unable to open folder: {error}")))));
                self.list.append(&row);
            }
        }
    }
}

fn file_icon(item: &FileItem) -> Image {
    if item.is_dir() {
        let icon_name = match item.name() {
            "Desktop" => "user-desktop-symbolic",
            "Documents" => "folder-documents-symbolic",
            "Downloads" => "folder-download-symbolic",
            "Music" => "folder-music-symbolic",
            "Pictures" => "folder-pictures-symbolic",
            "Videos" => "folder-videos-symbolic",
            _ => "folder-symbolic",
        };
        return Image::from_icon_name(icon_name);
    }

    let (content_type, _) = gio::content_type_guess(Some(item.path()), &[]);
    let icon = gio::content_type_get_icon(&content_type);
    Image::from_gicon(&icon)
}

fn activate_path(view: &FileView, path: PathBuf) {
    if path.is_dir() {
        if let Some(callback) = view.on_navigate.borrow().as_ref() {
            callback(path);
        } else {
            view.set_location(path);
        }
    } else {
        open_with_default_application(&path);
    }
}

fn open_with_default_application(path: &std::path::Path) {
    let uri = gio::File::for_path(path).uri();
    if gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>).is_err() {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}
