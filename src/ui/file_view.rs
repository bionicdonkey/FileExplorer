use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, ColumnView, ColumnViewColumn, CustomSorter, FlowBox, GestureClick, Image, Label, NoSelection, Orientation, Popover, ScrolledWindow, Separator, SignalListItemFactory, SortListModel};

use crate::core::directory_model::DirectoryModel;
use crate::core::file_item::FileItem;
use crate::core::{GroupField, ListColumn, ListColumnLayout, SortDirection, SortField, ViewMode, ViewOptions};
use crate::ui::context_menu::{item_commands, open_with_apps, ContextCommand, ContextTarget};

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
    on_context_command: Rc<RefCell<Option<Box<dyn Fn(ContextTarget, ContextCommand)>>>>,
    on_options_changed: Rc<RefCell<Option<Box<dyn Fn(ViewOptions)>>>>,
    options: Rc<RefCell<ViewOptions>>,
}

impl FileView {
    pub fn new(location: Rc<RefCell<PathBuf>>) -> Self {
        let scroll = ScrolledWindow::new();
        scroll.set_vexpand(true);
        let on_navigate = Rc::new(RefCell::new(None));
        let view = Self { widget: scroll, location, on_navigate, on_context_command: Rc::new(RefCell::new(None)), on_options_changed: Rc::new(RefCell::new(None)), options: Rc::new(RefCell::new(ViewOptions::default())) };
        let background_view = view.clone();
        let background = GestureClick::new();
        background.set_button(3);
        let background_parent = view.widget.clone();
        background.connect_pressed(move |_, _, x, y| show_item_context_menu(&background_parent, x, y, ContextTarget::Background(background_view.location()), &background_view));
        view.widget.add_controller(background);
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

    pub fn set_context_handler<F: Fn(ContextTarget, ContextCommand) + 'static>(&self, callback: F) {
        *self.on_context_command.borrow_mut() = Some(Box::new(callback));
    }

    pub fn set_options_changed_handler<F: Fn(ViewOptions) + 'static>(&self, callback: F) {
        *self.on_options_changed.borrow_mut() = Some(Box::new(callback));
    }

    pub fn set_options(&self, options: ViewOptions) {
        *self.options.borrow_mut() = options;
        self.reload();
    }

    pub fn options(&self) -> ViewOptions { self.options.borrow().clone() }

    pub fn location(&self) -> PathBuf { self.location.borrow().clone() }

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
                    ViewMode::Hybrid => self.widget.set_child(Some(&self.hybrid_view(&items, &options))),
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

        // Group headings are part of this model. Applying a second GTK sort would
        // move the file rows independently and detach them from their headings.
        let native_row_sorting = uses_native_row_sorting(options.group_field);
        let sorted = native_row_sorting.then(|| SortListModel::new(Some(store.clone()), None::<gtk::Sorter>));
        let selection = match &sorted {
            Some(sorted) => NoSelection::new(Some(sorted.clone())),
            None => NoSelection::new(Some(store)),
        };
        let view = ColumnView::new(Some(selection.clone()));
        view.set_reorderable(true);
        view.set_show_row_separators(true);
        view.add_css_class(if options.view_mode == ViewMode::CompactList { "compact-file-list" } else { "detailed-file-list" });

        for column_id in options.list_columns.normalized().order {
            let column = ColumnViewColumn::new(Some(column_title(column_id)), Some(column_factory(column_id, options.thumbnails, self.clone())));
            column.set_resizable(true);
            column.set_fixed_width(options.list_columns.width_for(column_id));
            column.set_expand(column_id == ListColumn::Name);
            column.set_sorter(Some(&column_sorter(column_id)));
            let options_state = self.options.clone();
            let changed = self.on_options_changed.clone();
            let layout_view = view.clone();
            column.connect_fixed_width_notify(move |_| publish_column_layout(&layout_view, &options_state, &changed));
            view.append_column(&column);
        }
        if let Some(sorted) = &sorted {
            sorted.set_sorter(view.sorter().as_ref());
        }

        let options_state = self.options.clone();
        let changed = self.on_options_changed.clone();
        let layout_view = view.clone();
        view.columns().connect_items_changed(move |_, _, _, _| publish_column_layout(&layout_view, &options_state, &changed));

        let options_state = self.options.clone();
        let changed = self.on_options_changed.clone();
        let file_view = self.clone();
        let column_sorter = view.sorter().and_downcast::<gtk::ColumnViewSorter>().expect("column view uses a column sorter");
        column_sorter.connect_primary_sort_column_notify(move |sorter| {
            let Some(column) = sorter.primary_sort_column() else { return; };
            let Some(field) = column.title().as_deref().and_then(sort_field_for_title) else { return; };
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
            }
        });

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
        let layout = options.grid_size.grid_layout();
        let grid = FlowBox::new(); grid.set_selection_mode(gtk::SelectionMode::Single); grid.set_valign(gtk::Align::Start); grid.set_column_spacing(layout.gap.try_into().expect("grid gaps are positive")); grid.set_row_spacing(layout.gap.try_into().expect("grid gaps are positive")); grid.set_margin_top(12); grid.set_margin_start(12); grid.set_margin_end(12);
        let activation_items = items.to_vec();
        let activation_view = self.clone();
        grid.connect_child_activated(move |_, child| { if let Some(item) = activation_items.get(child.index() as usize) { activate_path(&activation_view, item.path().to_path_buf()); } });
        for item in items { let tile = GtkBox::new(Orientation::Vertical, layout.gap / 2); tile.add_css_class("icon-grid-tile"); tile.set_size_request(layout.tile_width, -1); tile.set_halign(gtk::Align::Center); tile.set_valign(gtk::Align::Start); tile.set_vexpand(false); tile.set_tooltip_text(Some(item.name())); tile.set_margin_top(layout.vertical_padding); tile.set_margin_bottom(layout.vertical_padding); let icon = item_image(item, options.thumbnails); icon.set_pixel_size(options.grid_size.icon_size()); tile.append(&icon); tile.append(&grid_tile_label(item.name(), layout.label_width_chars)); let target = if item.is_dir() { ContextTarget::Folder(item.path().to_path_buf()) } else { ContextTarget::File { path: item.path().to_path_buf(), content_type: item.content_type().to_string() } }; let view = self.clone(); let grid_context = grid.clone(); let menu_tile = tile.clone(); let gesture = GestureClick::new(); gesture.set_button(3); gesture.connect_pressed(move |gesture, _, x, y| { gesture.set_state(gtk::EventSequenceState::Claimed); if let Some(child) = menu_tile.parent().and_downcast::<gtk::FlowBoxChild>() { grid_context.select_child(&child); } show_item_context_menu(&menu_tile, x, y, target.clone(), &view) }); tile.add_controller(gesture); grid.append(&tile); }
        grid
    }

    fn hybrid_view(&self, items: &[FileItem], options: &ViewOptions) -> GtkBox {
        let root = GtkBox::new(Orientation::Vertical, 8);
        root.set_margin_start(12);
        root.set_margin_top(12);
        let folders: Vec<_> = items.iter().filter(|item| item.is_dir()).cloned().collect();
        let files: Vec<_> = items.iter().filter(|item| !item.is_dir()).cloned().collect();
        if !folders.is_empty() {
            let heading = Label::new(Some("Folders")); heading.add_css_class("heading"); heading.set_xalign(0.0); root.append(&heading);
            root.append(&self.grid_view(&folders, options));
        }
        if !files.is_empty() {
            let heading = Label::new(Some("Files")); heading.add_css_class("heading"); heading.set_xalign(0.0); heading.set_margin_top(12); root.append(&heading);
            let file_options = ViewOptions { view_mode: ViewMode::DetailedList, folders_first: false, ..options.clone() };
            root.append(&self.column_view_for(&files, &file_options));
        }
        root
    }
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

fn show_item_context_menu(parent: &impl IsA<gtk::Widget>, x: f64, y: f64, target: ContextTarget, view: &FileView) {
    let popover = Popover::new();
    popover.set_parent(parent);
    popover.set_autohide(true);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    let menu = GtkBox::new(Orientation::Vertical, 2);
    menu.set_margin_top(6); menu.set_margin_bottom(6);
    for command in item_commands(&target) {
        if command == ContextCommand::OpenWith {
            menu.append(&open_with_submenu(&target));
            continue;
        }
        if command == ContextCommand::Copy {
            menu.append(&command_submenu("Copy ›", &target, view, &[ContextCommand::Copy, ContextCommand::CopyFilename, ContextCommand::CopyFullPath, ContextCommand::CopyTo]));
            continue;
        }
        if command == ContextCommand::Cut {
            menu.append(&command_submenu("Move ›", &target, view, &[ContextCommand::Cut, ContextCommand::MoveTo]));
            continue;
        }
        if matches!(command, ContextCommand::CopyFilename | ContextCommand::CopyFullPath | ContextCommand::CopyTo | ContextCommand::MoveTo) {
            continue;
        }
        menu.append(&command_button(context_command_label(command), target.clone(), view.clone(), command, Some(popover.clone())));
    }
    popover.set_child(Some(&menu));
    popover.popup();
}

fn command_submenu(label: &str, target: &ContextTarget, view: &FileView, commands: &[ContextCommand]) -> Button {
    let button = Button::with_label(label);
    let submenu_parent = button.clone();
    let target = target.clone();
    let view = view.clone();
    let commands = commands.to_vec();
    button.connect_clicked(move |_| {
        let menu = GtkBox::new(Orientation::Vertical, 2);
        menu.set_margin_top(6); menu.set_margin_bottom(6);
        for command in &commands {
            menu.append(&command_button(context_command_label(*command), target.clone(), view.clone(), *command, None));
        }
        show_submenu(&submenu_parent, &menu);
    });
    button
}

fn open_with_submenu(target: &ContextTarget) -> Button {
    let button = Button::with_label("Open With ›");
    let submenu_parent = button.clone();
    let target = target.clone();
    button.connect_clicked(move |_| {
        let (path, content_type) = match &target {
            ContextTarget::File { path, content_type } => (path.clone(), content_type.clone()),
            _ => return,
        };
        let menu = GtkBox::new(Orientation::Vertical, 2);
        menu.set_margin_top(6); menu.set_margin_bottom(6);
        for app in open_with_apps(&content_type) {
            let app_button = Button::with_label(&app.display_name());
            let path = path.clone();
            app_button.connect_clicked(move |_| { let _ = app.launch(&[gio::File::for_path(&path)], None::<&gio::AppLaunchContext>); });
            menu.append(&app_button);
        }
        menu.append(&Separator::new(Orientation::Horizontal));
        let other = Button::with_label("Other Application…");
        let parent = submenu_parent.clone();
        other.connect_clicked(move |_| show_application_chooser(&parent, path.clone(), content_type.clone()));
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

#[allow(deprecated)]
fn show_application_chooser(parent: &Button, path: PathBuf, content_type: String) {
    let window = parent.root().and_downcast::<gtk::Window>();
    let dialog = gtk::AppChooserDialog::for_content_type(window.as_ref(), gtk::DialogFlags::MODAL, &content_type);
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Ok {
            if let Some(app) = dialog.app_info() {
                let _ = app.launch(&[gio::File::for_path(&path)], None::<&gio::AppLaunchContext>);
            }
        }
        dialog.close();
    });
    dialog.present();
}

fn command_button(label: &str, target: ContextTarget, view: FileView, command: ContextCommand, parent: Option<Popover>) -> Button {
    let button = Button::with_label(label);
    button.connect_clicked(move |_| {
        if let Some(popover) = &parent { popover.popdown(); }
        if command == ContextCommand::Open {
            if let ContextTarget::Folder(path) | ContextTarget::File { path, .. } = &target {
                activate_path(&view, path.clone());
                return;
            }
        }
        if let Some(callback) = view.on_context_command.borrow().as_ref() {
            callback(target.clone(), command);
        }
    });
    button
}

fn context_command_label(command: ContextCommand) -> &'static str {
    match command { ContextCommand::Open => "Open", ContextCommand::OpenInNewTab => "Open in New Tab", ContextCommand::OpenInNewWindow => "Open in New Window", ContextCommand::OpenInTerminal => "Open in Terminal", ContextCommand::Cut => "Cut", ContextCommand::Copy => "Copy", ContextCommand::CopyFilename => "Copy Filename", ContextCommand::CopyFullPath => "Copy Full Path", ContextCommand::CopyTo => "Copy To…", ContextCommand::MoveTo => "Move To…", ContextCommand::Rename => "Rename", ContextCommand::MoveToTrash => "Move to Trash", ContextCommand::Properties => "Properties", ContextCommand::NewFolder => "New Folder", ContextCommand::Paste => "Paste", ContextCommand::Refresh => "Refresh", ContextCommand::CloseTab => "Close Tab", ContextCommand::CloseOtherTabs => "Close Other Tabs", ContextCommand::DuplicateTab => "Duplicate Tab", ContextCommand::OpenWith => "Open With" }
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

fn publish_column_layout(view: &ColumnView, options_state: &Rc<RefCell<ViewOptions>>, changed: &Rc<RefCell<Option<Box<dyn Fn(ViewOptions)>>>>) {
    let columns = view.columns();
    let mut order = Vec::new();
    let mut widths = std::collections::BTreeMap::new();
    for index in 0..columns.n_items() {
        let Some(column) = columns.item(index).and_downcast::<ColumnViewColumn>() else { continue; };
        let Some(id) = column.title().as_deref().and_then(list_column_for_title) else { continue; };
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
            let context_item = list_item.clone();
            let context_view = view.clone();
            let gesture = GestureClick::new();
            gesture.set_button(3);
            gesture.connect_pressed(move |gesture, _, x, y| {
                let Some(object) = context_item.item().and_downcast::<glib::BoxedAnyObject>() else { return; };
                let row = object.borrow::<ListRow>();
                let ListRow::File(item) = &*row else { return; };
                let target = if item.is_dir() { ContextTarget::Folder(item.path().to_path_buf()) } else { ContextTarget::File { path: item.path().to_path_buf(), content_type: item.content_type().to_string() } };
                let Some(parent) = context_item.child() else { return; };
                gesture.set_state(gtk::EventSequenceState::Claimed);
                show_item_context_menu(&parent, x, y, target, &context_view);
            });
            line.add_controller(gesture);
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

    use gtk::prelude::{ListModelExt, WidgetExt};

    use super::{grid_tile_label, uses_native_row_sorting, FileView};
    use crate::core::{GroupField, ViewMode, ViewOptions};

    #[test]
    fn grouped_lists_do_not_apply_a_second_native_row_sort() {
        assert!(uses_native_row_sorting(GroupField::None));
        assert!(!uses_native_row_sorting(GroupField::DateModified));
        assert!(!uses_native_row_sorting(GroupField::Type));
        assert!(!uses_native_row_sorting(GroupField::Size));
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

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn file_scroller_relies_on_the_window_for_its_right_inset() {
        gtk::init().expect("GTK should initialize for widget tests");
        let view = FileView::new(Rc::new(RefCell::new(PathBuf::from("/tmp"))));

        assert_eq!(view.widget.margin_end(), 0);
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
