use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, FlowBox, Image, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow};

use crate::core::directory_model::DirectoryModel;
use crate::core::file_item::FileItem;
use crate::core::{ViewMode, ViewOptions};

#[derive(Clone)]
pub struct FileView {
    pub widget: ScrolledWindow,
    location: Rc<RefCell<PathBuf>>,
    on_navigate: Rc<RefCell<Option<Box<dyn Fn(PathBuf)>>>>,
    options: Rc<RefCell<ViewOptions>>,
}

impl FileView {
    pub fn new(location: Rc<RefCell<PathBuf>>) -> Self {
        let scroll = ScrolledWindow::new();
        scroll.set_vexpand(true);
        scroll.set_margin_end(12);
        let on_navigate = Rc::new(RefCell::new(None));
        let view = Self { widget: scroll, location, on_navigate, options: Rc::new(RefCell::new(ViewOptions::default())) };
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

    pub fn set_options(&self, options: ViewOptions) {
        *self.options.borrow_mut() = options;
        self.reload();
    }

    pub fn options(&self) -> ViewOptions { self.options.borrow().clone() }

    fn reload(&self) {
        let options = self.options();
        match DirectoryModel::load_with_options(&self.location.borrow(), &options) {
            Ok(items) if items.is_empty() => {
                self.widget.set_child(Some(&Label::new(Some("This folder is empty"))));
            }
            Ok(items) => {
                match options.view_mode {
                    ViewMode::DetailedList => self.widget.set_child(Some(&self.list_view(&items, &options, true))),
                    ViewMode::CompactList => self.widget.set_child(Some(&self.list_view(&items, &options, false))),
                    ViewMode::IconGrid => self.widget.set_child(Some(&self.grid_view(&items, &options))),
                };
            }
            Err(error) => {
                self.widget.set_child(Some(&Label::new(Some(&format!("Unable to open folder: {error}")))));
            }
        }
    }

    fn list_view(&self, items: &[FileItem], options: &ViewOptions, detailed: bool) -> ListBox {
        let list = ListBox::new(); list.set_selection_mode(gtk::SelectionMode::Single); list.set_show_separators(true); list.add_css_class("boxed-list");
        let mut last_group = None;
        for item in items {
            let group = crate::core::directory_model::group_label(item, options.group_field, std::time::SystemTime::now());
            if group != last_group { if let Some(group) = &group { let row = ListBoxRow::new(); row.set_selectable(false); row.set_activatable(false); let label = Label::new(Some(group)); label.add_css_class("heading"); label.set_margin_top(12); label.set_margin_start(12); label.set_xalign(0.0); row.set_child(Some(&label)); list.append(&row); } last_group = group; }
            let row = ListBoxRow::new(); row.set_activatable(true); row.set_selectable(true);
            let view = self.clone(); let path = item.path().to_path_buf(); row.connect_activate(move |_| activate_path(&view, path.clone()));
            let line = GtkBox::new(Orientation::Horizontal, 12); line.set_margin_top(8); line.set_margin_bottom(8); line.set_margin_start(12); line.set_margin_end(12);
            let icon = item_image(item, options.thumbnails); icon.set_pixel_size(20); line.append(&icon);
            let name = Label::new(Some(item.name())); name.set_hexpand(true); name.set_xalign(0.0); line.append(&name);
            if detailed { for value in [format_modified(item.modified()), item.type_name(), item.display_size()] { let label = Label::new(Some(&value)); label.add_css_class("dim-label"); label.set_xalign(1.0); line.append(&label); } } else { let kind = Label::new(Some(&item.type_name())); kind.add_css_class("dim-label"); line.append(&kind); }
            row.set_child(Some(&line)); list.append(&row);
        }
        list
    }

    fn grid_view(&self, items: &[FileItem], options: &ViewOptions) -> FlowBox {
        let grid = FlowBox::new(); grid.set_selection_mode(gtk::SelectionMode::None); grid.set_column_spacing(12); grid.set_row_spacing(12); grid.set_margin_top(12); grid.set_margin_start(12); grid.set_margin_end(12);
        for item in items { let tile = Button::new(); tile.add_css_class("flat"); let body = GtkBox::new(Orientation::Vertical, 6); let icon = item_image(item, options.thumbnails); icon.set_pixel_size(options.grid_size.icon_size()); body.append(&icon); let label = Label::new(Some(item.name())); label.set_wrap(true); label.set_max_width_chars(18); body.append(&label); tile.set_child(Some(&body)); let view = self.clone(); let path = item.path().to_path_buf(); tile.connect_clicked(move |_| activate_path(&view, path.clone())); grid.append(&tile); }
        grid
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use super::FileView;
    use crate::core::{ViewMode, ViewOptions};

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn setting_icon_grid_replaces_the_detail_list_renderer() {
        gtk::init().expect("GTK should initialize for widget tests");
        let view = FileView::new(Rc::new(RefCell::new(PathBuf::from("/tmp"))));

        view.set_options(ViewOptions { view_mode: ViewMode::IconGrid, ..ViewOptions::default() });

        assert_eq!(view.options().view_mode, ViewMode::IconGrid);
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

fn item_image(item: &FileItem, thumbnails: bool) -> Image {
    let image = file_icon(item);
    if thumbnails && item.content_type().starts_with("image/") { let image = image.clone(); let path = item.path().to_path_buf(); glib::idle_add_local_once(move || image.set_from_file(Some(path))); }
    image
}

fn format_modified(modified: std::time::SystemTime) -> String {
    let age = std::time::SystemTime::now().duration_since(modified).unwrap_or_default().as_secs();
    if age < 86_400 { "Today".into() } else if age < 172_800 { "Yesterday".into() } else { format!("{} days ago", age / 86_400) }
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
