use std::path::PathBuf;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, GestureClick, Image, Label, ListBox, ListBoxRow, Orientation, Popover, ScrolledWindow};

use crate::core::location::Location;
use crate::ui::context_menu::ContextCommand;

pub struct Sidebar {
    pub widget: ScrolledWindow,
}

impl Sidebar {
    pub fn new<F: Fn(PathBuf) + Clone + 'static, C: Fn(PathBuf, ContextCommand) + Clone + 'static>(on_navigate: F, on_context: C) -> Self {
        let list = ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("navigation-sidebar");

        let home = Location::home().path().to_path_buf();
        let sections = [
            ("Home", "user-home-symbolic", home.clone()),
            ("Desktop", "user-desktop-symbolic", home.join("Desktop")),
            ("Documents", "folder-documents-symbolic", home.join("Documents")),
            ("Downloads", "folder-download-symbolic", home.join("Downloads")),
            ("Pictures", "folder-pictures-symbolic", home.join("Pictures")),
            ("File System", "drive-harddisk-symbolic", PathBuf::from("/")),
        ];
        for (label, icon_name, path) in sections {
            let row = ListBoxRow::new();
            let button = Button::new();
            button.set_halign(gtk::Align::Fill);
            button.add_css_class("flat");
            let button_content = GtkBox::new(Orientation::Horizontal, 10);
            button_content.set_margin_top(5);
            button_content.set_margin_bottom(5);
            let icon = Image::from_icon_name(icon_name);
            icon.set_pixel_size(18);
            let text = Label::new(Some(label));
            text.set_xalign(0.0);
            button_content.append(&icon);
            button_content.append(&text);
            button.set_child(Some(&button_content));
            let navigate = on_navigate.clone();
            let navigation_path = path.clone();
            button.connect_clicked(move |_| {
                navigate(navigation_path.clone());
            });
            let context = on_context.clone();
            let context_path = path.clone();
            let context_button = button.clone();
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
                for (label, command) in [("Open", ContextCommand::Open), ("Open in New Tab", ContextCommand::OpenInNewTab), ("Open in New Window", ContextCommand::OpenInNewWindow), ("Open in Terminal", ContextCommand::OpenInTerminal)] {
                    let item = Button::with_label(label);
                    let context = context.clone();
                    let path = context_path.clone();
                    let popover = popover.clone();
                    item.connect_clicked(move |_| { popover.popdown(); context(path.clone(), command); });
                    menu.append(&item);
                }
                popover.set_child(Some(&menu));
                popover.popup();
            });
            button.add_controller(gesture);
            row.set_child(Some(&button));
            list.append(&row);
        }

        let title = Label::new(Some("Quick access"));
        title.add_css_class("heading");
        title.set_xalign(0.0);
        let container = GtkBox::new(Orientation::Vertical, 8);
        container.set_margin_top(12);
        container.set_margin_start(8);
        container.set_margin_end(8);
        container.append(&title);
        container.append(&list);
        let scroll = ScrolledWindow::new();
        scroll.set_child(Some(&container));
        Self { widget: scroll }
    }
}
