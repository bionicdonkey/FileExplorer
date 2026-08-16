use std::path::PathBuf;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, Image, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow};

use crate::core::location::Location;

pub struct Sidebar {
    pub widget: ScrolledWindow,
}

impl Sidebar {
    pub fn new<F: Fn(PathBuf) + Clone + 'static>(on_navigate: F) -> Self {
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
            button.connect_clicked(move |_| {
                navigate(path.clone());
            });
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
