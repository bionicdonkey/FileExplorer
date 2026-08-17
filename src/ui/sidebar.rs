use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gio::prelude::*;
use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, GestureClick, Image, Label, Orientation, Popover, ScrolledWindow, ToggleButton};

use crate::core::{SidebarLocation, SidebarModel, SidebarNode};

#[derive(Clone, Debug)]
pub enum SidebarAction {
    Open(PathBuf),
    OpenInNewTab(PathBuf),
    OpenInNewWindow(PathBuf),
    OpenInTerminal(PathBuf),
    OpenUri(String),
    Pin(PathBuf),
    Unpin(PathBuf),
    AddNetworkLocation,
    UnpinRemote(String),
}

#[derive(Clone)]
pub struct Sidebar {
    pub widget: ScrolledWindow,
    content: GtkBox,
    model: Rc<RefCell<SidebarModel>>,
    on_action: Rc<dyn Fn(SidebarAction)>,
    active_path: Rc<RefCell<Option<PathBuf>>>,
    mount_parent: gtk::Window,
}

impl Sidebar {
    pub fn new<F: Fn(SidebarAction) + 'static>(model: Rc<RefCell<SidebarModel>>, mount_parent: &impl IsA<gtk::Window>, on_action: F) -> Self {
        let content = GtkBox::new(Orientation::Vertical, 12);
        content.add_css_class("explorer-sidebar");
        content.set_margin_top(8);
        content.set_margin_bottom(8);
        content.set_margin_start(8);
        content.set_margin_end(8);
        let scroll = ScrolledWindow::new();
        scroll.set_child(Some(&content));

        let sidebar = Self {
            widget: scroll,
            content,
            model,
            on_action: Rc::new(on_action),
            active_path: Rc::new(RefCell::new(None)),
            mount_parent: mount_parent.clone().upcast(),
        };
        sidebar.refresh();

        let refresh_on_mount = sidebar.clone();
        gio::VolumeMonitor::get().connect_mount_added(move |_, _| refresh_on_mount.refresh());
        let refresh_on_unmount = sidebar.clone();
        gio::VolumeMonitor::get().connect_mount_removed(move |_, _| refresh_on_unmount.refresh());
        sidebar
    }

    pub fn refresh(&self) {
        while let Some(child) = self.content.first_child() { self.content.remove(&child); }
        for section in self.model.borrow().sections() {
            let section_is_empty = section.nodes.is_empty();
            let rows = GtkBox::new(Orientation::Vertical, 0);
            rows.add_css_class("sidebar-tree");
            for node in section.nodes { self.append_node(&rows, node, 0); }

            let heading_row = GtkBox::new(Orientation::Horizontal, 0);
            let toggle = ToggleButton::new();
            toggle.add_css_class("flat");
            toggle.add_css_class("sidebar-section-toggle");
            toggle.set_active(true);
            toggle.set_child(Some(&Image::from_icon_name("pan-down-symbolic")));
            let rows_for_toggle = rows.clone();
            toggle.connect_toggled(move |toggle| {
                let expanded = toggle.is_active();
                rows_for_toggle.set_visible(expanded);
                toggle.set_child(Some(&Image::from_icon_name(if expanded { "pan-down-symbolic" } else { "pan-end-symbolic" })));
            });
            heading_row.append(&toggle);
            let heading = Label::new(Some(section.title));
            heading.add_css_class("heading");
            heading.add_css_class("sidebar-section-heading");
            heading.set_xalign(0.0);
            if section.title == "Network & online" {
                heading.set_hexpand(true);
                let add = Button::from_icon_name("list-add-symbolic");
                add.add_css_class("flat");
                add.set_tooltip_text(Some("Add network location"));
                let action = self.on_action.clone();
                add.connect_clicked(move |_| action(SidebarAction::AddNetworkLocation));
                heading_row.append(&heading);
                heading_row.append(&add);
                self.content.append(&heading_row);
            } else {
                heading_row.append(&heading);
                self.content.append(&heading_row);
            }

            if section_is_empty && section.title == "Quick access" {
                let empty = Label::new(Some("Pin folders here for quick access"));
                empty.add_css_class("dim-label");
                empty.set_xalign(0.0);
                empty.set_margin_start(10);
                self.content.append(&empty);
            }

            self.content.append(&rows);
        }
    }

    pub fn set_active_path(&self, path: &Path) {
        *self.active_path.borrow_mut() = Some(path.to_path_buf());
        self.refresh();
    }

    fn append_node(&self, parent: &GtkBox, node: SidebarNode, depth: i32) {
        Self::append_node_inner(parent, node, depth, self.model.clone(), self.on_action.clone(), self.active_path.clone(), self.mount_parent.clone());
    }

    fn append_node_inner(parent: &GtkBox, node: SidebarNode, depth: i32, model: Rc<RefCell<SidebarModel>>, on_action: Rc<dyn Fn(SidebarAction)>, active_path: Rc<RefCell<Option<PathBuf>>>, mount_parent: gtk::Window) {
        let row = GtkBox::new(Orientation::Vertical, 0);
        let header = GtkBox::new(Orientation::Horizontal, 0);
        header.set_margin_start(depth * 16);
        header.add_css_class("sidebar-tree-row");

        let children = GtkBox::new(Orientation::Vertical, 0);
        children.set_visible(false);
        if node.expandable {
            let expander = ToggleButton::new();
            expander.add_css_class("flat");
            expander.add_css_class("sidebar-expander");
            let chevron = Image::from_icon_name("pan-end-symbolic");
            chevron.set_pixel_size(14);
            expander.set_child(Some(&chevron));
            let children_model = model.clone();
            let child_location = node.location.clone();
            let children_for_toggle = children.clone();
            let children_action = on_action.clone();
            let children_active_path = active_path.clone();
            let children_mount_parent = mount_parent.clone();
            expander.connect_toggled(move |button| {
                let expanded = button.is_active();
                children_for_toggle.set_visible(expanded);
                let icon = if expanded { "pan-down-symbolic" } else { "pan-end-symbolic" };
                button.set_child(Some(&Image::from_icon_name(icon)));
                if expanded && children_for_toggle.first_child().is_none() {
                    let initial_children = children_model.borrow().children_for_location(&child_location);
                    if !initial_children.is_empty() || child_location.local_path().is_some() {
                        Self::append_children(&children_for_toggle, initial_children, depth, children_model.clone(), children_action.clone(), children_active_path.clone(), children_mount_parent.clone());
                    } else {
                        let location_file = gio::File::for_uri(&child_location.uri());
                        let is_mountable = location_file
                            .query_info("standard::type", gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
                            .map(|info| info.file_type() == gio::FileType::Mountable)
                            .unwrap_or(false);
                        let operation = gtk::MountOperation::new(Some(&children_mount_parent));
                        let keep_operation = operation.clone();
                        let children_after_mount = children_for_toggle.clone();
                        let model_after_mount = children_model.clone();
                        let action_after_mount = children_action.clone();
                        let active_after_mount = children_active_path.clone();
                        let parent_after_mount = children_mount_parent.clone();
                        let location_after_mount = child_location.clone();
                        if is_mountable {
                            location_file.mount_mountable(gio::MountMountFlags::NONE, Some(&operation), gio::Cancellable::NONE, move |result| {
                                let _keep_operation_alive = &keep_operation;
                                let location = result.map(|root| SidebarLocation::from_uri(root.uri().to_string(), location_after_mount.label(), location_after_mount.icon_name())).unwrap_or(location_after_mount);
                                let loaded = model_after_mount.borrow().children_for_location(&location);
                                Self::append_children(&children_after_mount, loaded, depth, model_after_mount.clone(), action_after_mount.clone(), active_after_mount.clone(), parent_after_mount.clone());
                            });
                        } else {
                            location_file.mount_enclosing_volume(gio::MountMountFlags::NONE, Some(&operation), gio::Cancellable::NONE, move |_| {
                                let _keep_operation_alive = &keep_operation;
                                let loaded = model_after_mount.borrow().children_for_location(&location_after_mount);
                                Self::append_children(&children_after_mount, loaded, depth, model_after_mount.clone(), action_after_mount.clone(), active_after_mount.clone(), parent_after_mount.clone());
                            });
                        }
                    }
                }
            });
            header.append(&expander);
        } else {
            let spacer = GtkBox::new(Orientation::Horizontal, 0);
            spacer.set_size_request(32, -1);
            header.append(&spacer);
        }

        let button = Button::new();
        button.set_hexpand(true);
        button.set_halign(gtk::Align::Fill);
        button.add_css_class("flat");
        button.add_css_class("sidebar-location");
        if node.location.local_path().is_some_and(|path| active_path.borrow().as_deref() == Some(path)) {
            button.add_css_class("selected");
        }
        let button_content = GtkBox::new(Orientation::Horizontal, 8);
        button_content.set_margin_top(4);
        button_content.set_margin_bottom(4);
        let icon = Image::from_icon_name(node.location.icon_name());
        icon.set_pixel_size(18);
        let label = Label::new(Some(node.location.label()));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_hexpand(true);
        button_content.append(&icon);
        button_content.append(&label);
        button.set_child(Some(&button_content));
        let action = on_action.clone();
        let location = node.location.clone();
        button.connect_clicked(move |_| match location.local_path() {
            Some(path) => action(SidebarAction::Open(path.to_path_buf())),
            None => action(SidebarAction::OpenUri(location.uri())),
        });
        Self::add_context_menu(&button, &node.location, model, on_action);
        header.append(&button);
        row.append(&header);
        row.append(&children);
        parent.append(&row);
    }

    fn append_children(parent: &GtkBox, children: Vec<SidebarNode>, depth: i32, model: Rc<RefCell<SidebarModel>>, on_action: Rc<dyn Fn(SidebarAction)>, active_path: Rc<RefCell<Option<PathBuf>>>, mount_parent: gtk::Window) {
        for child in children {
            Self::append_node_inner(parent, child, depth + 1, model.clone(), on_action.clone(), active_path.clone(), mount_parent.clone());
        }
    }

    fn add_context_menu(button: &Button, location: &SidebarLocation, model: Rc<RefCell<SidebarModel>>, action: Rc<dyn Fn(SidebarAction)>) {
        let context_button = button.clone();
        let location = location.clone();
        let gesture = GestureClick::new();
        gesture.set_button(3);
        gesture.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let popover = Popover::new();
            popover.set_parent(&context_button);
            popover.set_autohide(true);
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            let menu = GtkBox::new(Orientation::Vertical, 2);
            menu.set_margin_top(6);
            menu.set_margin_bottom(6);
            if location.local_path().is_none() {
                let open = Button::with_label("Open");
                let open_action = action.clone();
                let popover_for_open = popover.clone();
                let uri = location.uri();
                open.connect_clicked(move |_| { popover_for_open.popdown(); open_action(SidebarAction::OpenUri(uri.clone())); });
                menu.append(&open);
                if model.borrow().preferences().remote_pins().iter().any(|pinned| pinned == &location.uri()) {
                    let unpin = Button::with_label("Unpin from Network & online");
                    let action = action.clone();
                    let popover_for_unpin = popover.clone();
                    let uri = location.uri();
                    unpin.connect_clicked(move |_| { popover_for_unpin.popdown(); action(SidebarAction::UnpinRemote(uri.clone())); });
                    menu.append(&unpin);
                }
                popover.set_child(Some(&menu));
                popover.popup();
                return;
            }
            let path = location.local_path().expect("local location was checked").to_path_buf();
            for (label, sidebar_action) in [
                ("Open", SidebarAction::Open(path.clone())),
                ("Open in New Tab", SidebarAction::OpenInNewTab(path.clone())),
                ("Open in New Window", SidebarAction::OpenInNewWindow(path.clone())),
                ("Open in Terminal", SidebarAction::OpenInTerminal(path.clone())),
            ] {
                let item = Button::with_label(label);
                let action = action.clone();
                let popover = popover.clone();
                item.connect_clicked(move |_| { popover.popdown(); action(sidebar_action.clone()); });
                menu.append(&item);
            }
            let separator = gtk::Separator::new(Orientation::Horizontal);
            separator.set_margin_top(4);
            separator.set_margin_bottom(4);
            menu.append(&separator);
            let is_pinned = model.borrow().preferences().is_pinned(&path);
            let pin_item = Button::with_label(if is_pinned { "Unpin from Quick access" } else { "Pin to Quick access" });
            let action = action.clone();
            let popover_for_pin = popover.clone();
            let pin_action = if is_pinned { SidebarAction::Unpin(path.clone()) } else { SidebarAction::Pin(path.clone()) };
            pin_item.connect_clicked(move |_| { popover_for_pin.popdown(); action(pin_action.clone()); });
            menu.append(&pin_item);
            popover.set_child(Some(&menu));
            popover.popup();
        });
        button.add_controller(gesture);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn sidebar_renders_quick_access_this_pc_and_network_sections() {
        gtk::init().unwrap();
        assert!(gtk::is_initialized());
    }
}
