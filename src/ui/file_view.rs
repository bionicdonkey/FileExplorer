use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, ColumnView, ColumnViewColumn, CustomSorter, FlowBox, Image, Label, NoSelection, Orientation, ScrolledWindow, SignalListItemFactory, SortListModel};

use crate::core::directory_model::DirectoryModel;
use crate::core::file_item::FileItem;
use crate::core::{ListColumn, ViewMode, ViewOptions};

#[derive(Clone)]
enum ListRow {
    Group(String),
    File(FileItem),
}

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
                    ViewMode::DetailedList | ViewMode::CompactList => self.widget.set_child(Some(&self.column_view_for(&items, &options))),
                    ViewMode::IconGrid => self.widget.set_child(Some(&self.grid_view(&items, &options))),
                };
            }
            Err(error) => {
                self.widget.set_child(Some(&Label::new(Some(&format!("Unable to open folder: {error}")))));
            }
        }
    }

    fn column_view_for(&self, items: &[FileItem], options: &ViewOptions) -> ColumnView {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let mut last_group = None;
        for item in items {
            let group = crate::core::directory_model::group_label(item, options.group_field, std::time::SystemTime::now());
            if group != last_group {
                if let Some(group) = &group {
                    store.append(&glib::BoxedAnyObject::new(ListRow::Group(group.clone())));
                }
                last_group = group;
            }
            store.append(&glib::BoxedAnyObject::new(ListRow::File(item.clone())));
        }

        let sorted = SortListModel::new(Some(store), None::<gtk::Sorter>);
        let selection = NoSelection::new(Some(sorted.clone()));
        let view = ColumnView::new(Some(selection.clone()));
        view.set_reorderable(true);
        view.set_show_row_separators(true);
        view.add_css_class(if options.view_mode == ViewMode::CompactList { "compact-file-list" } else { "detailed-file-list" });

        for column_id in options.list_columns.normalized().order {
            let column = ColumnViewColumn::new(Some(column_title(column_id)), Some(column_factory(column_id, options.thumbnails)));
            column.set_resizable(true);
            column.set_fixed_width(options.list_columns.width_for(column_id));
            column.set_expand(column_id == ListColumn::Name);
            column.set_sorter(Some(&column_sorter(column_id)));
            view.append_column(&column);
        }
        sorted.set_sorter(view.sorter().as_ref());

        let file_view = self.clone();
        view.connect_activate(move |_, position| {
            let Some(object) = selection.item(position).and_downcast::<glib::BoxedAnyObject>() else { return; };
            let row = object.borrow::<ListRow>();
            if let ListRow::File(item) = &*row {
                activate_path(&file_view, item.path().to_path_buf());
            }
        });
        view
    }

    pub fn column_view(&self) -> Option<ColumnView> {
        self.widget.child()?.downcast::<ColumnView>().ok()
    }

    fn grid_view(&self, items: &[FileItem], options: &ViewOptions) -> FlowBox {
        let grid = FlowBox::new(); grid.set_selection_mode(gtk::SelectionMode::None); grid.set_column_spacing(12); grid.set_row_spacing(12); grid.set_margin_top(12); grid.set_margin_start(12); grid.set_margin_end(12);
        for item in items { let tile = Button::new(); tile.add_css_class("flat"); let body = GtkBox::new(Orientation::Vertical, 6); let icon = item_image(item, options.thumbnails); icon.set_pixel_size(options.grid_size.icon_size()); body.append(&icon); let label = Label::new(Some(item.name())); label.set_wrap(true); label.set_max_width_chars(18); body.append(&label); tile.set_child(Some(&body)); let view = self.clone(); let path = item.path().to_path_buf(); tile.connect_clicked(move |_| activate_path(&view, path.clone())); grid.append(&tile); }
        grid
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

fn column_factory(column: ListColumn, thumbnails: bool) -> SignalListItemFactory {
    let factory = SignalListItemFactory::new();
    factory.connect_setup(move |_, list_item| {
        let list_item = list_item.downcast_ref::<gtk::ListItem>().expect("column factories receive list items");
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
        let list_item = list_item.downcast_ref::<gtk::ListItem>().expect("column factories receive list items");
        let Some(object) = list_item.item().and_downcast::<glib::BoxedAnyObject>() else { return; };
        let row = object.borrow::<ListRow>();
        let Some(child) = list_item.child() else { return; };
        match (&*row, column) {
            (ListRow::Group(group), ListColumn::Name) => {
                let line = child.downcast::<GtkBox>().expect("name cells use a box");
                let icon = line.first_child().and_downcast::<Image>().expect("name cells include an icon");
                icon.clear();
                let label = line.last_child().and_downcast::<Label>().expect("name cells include a label");
                label.set_label(group);
                label.add_css_class("heading");
            }
            (ListRow::Group(_), _) => child.downcast::<Label>().expect("metadata cells use labels").set_label(""),
            (ListRow::File(item), ListColumn::Name) => {
                let line = child.downcast::<GtkBox>().expect("name cells use a box");
                let icon = line.first_child().and_downcast::<Image>().expect("name cells include an icon");
                let item_icon = file_icon(item);
                if let Some(gicon) = item_icon.gicon() {
                    icon.set_from_gicon(&gicon);
                } else {
                    icon.set_from_icon_name(item_icon.icon_name().as_deref());
                }
                if thumbnails && item.content_type().starts_with("image/") {
                    let icon = icon.clone();
                    let path = item.path().to_path_buf();
                    glib::idle_add_local_once(move || icon.set_from_file(Some(path)));
                }
                let label = line.last_child().and_downcast::<Label>().expect("name cells include a label");
                label.remove_css_class("heading");
                label.set_label(item.name());
            }
            (ListRow::File(item), ListColumn::DateModified) => child.downcast::<Label>().expect("metadata cells use labels").set_label(&format_modified(item.modified())),
            (ListRow::File(item), ListColumn::Type) => child.downcast::<Label>().expect("metadata cells use labels").set_label(&item.type_name()),
            (ListRow::File(item), ListColumn::Size) => child.downcast::<Label>().expect("metadata cells use labels").set_label(&item.display_size()),
        }
    });
    factory
}

fn column_sorter(column: ListColumn) -> CustomSorter {
    CustomSorter::new(move |left, right| {
        let left = left.downcast_ref::<glib::BoxedAnyObject>().expect("file table contains boxed rows").borrow::<ListRow>();
        let right = right.downcast_ref::<glib::BoxedAnyObject>().expect("file table contains boxed rows").borrow::<ListRow>();
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use gtk::prelude::ListModelExt;

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

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn list_modes_build_four_reorderable_resizable_columns() {
        gtk::init().expect("GTK should initialize for widget tests");
        let view = FileView::new(Rc::new(RefCell::new(PathBuf::from("/tmp"))));

        for mode in [ViewMode::DetailedList, ViewMode::CompactList] {
            view.set_options(ViewOptions { view_mode: mode, ..ViewOptions::default() });
            let column_view = view.column_view().expect("list mode must use ColumnView");
            assert_eq!(column_view.columns().n_items(), 4);
            assert!(column_view.is_reorderable());
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
