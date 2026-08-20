use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, Entry, GestureClick, Image, Label, Orientation, Separator, Stack};

use crate::core::location::Location;

#[derive(Clone, Copy)]
pub enum NavigationAction {
    Back,
    Forward,
    Up,
    Refresh,
}

#[derive(Clone, Copy)]
pub enum FileAction {
    Cut,
    Copy,
    Paste,
    MoveToTrash,
}

/// A clickable breadcrumb trail over a `Stack` that swaps to a plain `Entry`
/// on double-click, so typing or pasting a path works the same way anywhere
/// this bar is used. Shared by the main window's header and the in-app
/// folder picker so a fix to one applies to both instead of drifting apart.
#[derive(Clone)]
pub struct LocationBar {
    pub widget: Stack,
    entry: Entry,
    breadcrumbs: GtkBox,
    stack: Stack,
    on_navigate: Rc<RefCell<Option<Box<dyn Fn(PathBuf)>>>>,
}

impl LocationBar {
    pub fn new(initial: &std::path::Path) -> Self {
        let entry = Entry::builder().hexpand(true).build();
        entry.set_tooltip_text(Some("Location (Ctrl+L)"));
        entry.add_css_class("monospace");
        entry.add_css_class("location-entry");
        entry.set_text(&initial.display().to_string());
        let breadcrumbs = GtkBox::new(Orientation::Horizontal, 2);
        breadcrumbs.set_hexpand(true);
        breadcrumbs.add_css_class("location-bar");
        let stack = Stack::new();
        stack.set_hexpand(true);
        stack.add_named(&breadcrumbs, Some("breadcrumbs"));
        stack.add_named(&entry, Some("entry"));
        stack.set_visible_child_name("breadcrumbs");

        let click = GestureClick::new();
        click.set_button(1);
        let edit_stack = stack.clone();
        let edit_entry = entry.clone();
        click.connect_released(move |_, presses, _, _| {
            if presses == 2 {
                edit_stack.set_visible_child_name("entry");
                edit_entry.grab_focus();
                edit_entry.select_region(0, -1);
            }
        });
        breadcrumbs.add_controller(click);

        let on_navigate = Rc::new(RefCell::new(None));
        let bar = Self { widget: stack.clone(), entry, breadcrumbs, stack, on_navigate };
        bar.render_breadcrumbs(initial);
        bar
    }

    pub fn connect_location_changed<F: Fn(PathBuf) + 'static>(&self, callback: F) {
        let stack = self.stack.clone();
        self.entry.connect_activate(move |entry| {
            callback(PathBuf::from(entry.text().as_str()));
            stack.set_visible_child_name("breadcrumbs");
        });
    }

    pub fn set_navigation_handler<F: Fn(PathBuf) + 'static>(&self, callback: F) {
        *self.on_navigate.borrow_mut() = Some(Box::new(callback));
        self.render_breadcrumbs_from_entry();
    }

    pub fn set_location(&self, path: &std::path::Path) {
        self.entry.set_text(&path.display().to_string());
        self.render_breadcrumbs(path);
        self.stack.set_visible_child_name("breadcrumbs");
    }

    pub fn set_uri(&self, uri: &str) {
        self.entry.set_text(uri);
        self.render_uri_breadcrumbs(uri);
        self.stack.set_visible_child_name("breadcrumbs");
    }

    fn render_breadcrumbs_from_entry(&self) {
        let path = PathBuf::from(self.entry.text().as_str());
        self.render_breadcrumbs(&path);
    }

    fn render_breadcrumbs(&self, path: &std::path::Path) {
        while let Some(child) = self.breadcrumbs.first_child() {
            self.breadcrumbs.remove(&child);
        }
        let home = Location::home().path().to_path_buf();
        let under_home = path.starts_with(&home);
        let mut accumulated = if under_home { home.clone() } else { PathBuf::from("/") };
        let base = Button::new();
        base.add_css_class("flat");
        base.add_css_class("breadcrumb-segment");
        if under_home {
            let content = GtkBox::new(Orientation::Horizontal, 6);
            let icon = Image::from_icon_name("user-home-symbolic");
            icon.set_pixel_size(16);
            content.append(&icon);
            content.append(&Label::new(Some("Home")));
            base.set_child(Some(&content));
        } else {
            base.set_label("/");
        }
        let remaining = if under_home { path.strip_prefix(&home).unwrap_or(path) } else { path };
        let components = remaining
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(name) => Some(name.to_os_string()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let base_is_current = components.is_empty();
        if base_is_current {
            base.add_css_class("breadcrumb-current");
        }
        self.connect_breadcrumb(&base, accumulated.clone(), base_is_current);
        self.breadcrumbs.append(&base);

        for (index, component) in components.iter().enumerate() {
            let separator = Label::new(Some("/"));
            separator.add_css_class("breadcrumb-separator");
            self.breadcrumbs.append(&separator);
            accumulated.push(component);
            let button = Button::with_label(&component.to_string_lossy());
            button.add_css_class("flat");
            button.add_css_class("breadcrumb-segment");
            let is_current = index + 1 == components.len();
            if is_current {
                button.add_css_class("breadcrumb-current");
            }
            self.connect_breadcrumb(&button, accumulated.clone(), is_current);
            self.breadcrumbs.append(&button);
        }
        let trailing = Label::new(None);
        trailing.set_hexpand(true);
        self.breadcrumbs.append(&trailing);
    }

    fn render_uri_breadcrumbs(&self, uri: &str) {
        while let Some(child) = self.breadcrumbs.first_child() {
            self.breadcrumbs.remove(&child);
        }

        let (base, remaining) = match uri.split_once("://") {
            Some((scheme, rest)) => (format!("{scheme}://"), rest),
            None => (String::new(), uri),
        };
        let segments = remaining.split('/').filter(|segment| !segment.is_empty()).collect::<Vec<_>>();
        let root = Button::with_label(if base.is_empty() { "/" } else { &base });
        root.add_css_class("flat");
        root.add_css_class("breadcrumb-segment");
        if segments.is_empty() { root.add_css_class("breadcrumb-current"); }
        self.breadcrumbs.append(&root);

        for (index, segment) in segments.iter().enumerate() {
            let separator = Label::new(Some("/"));
            separator.add_css_class("breadcrumb-separator");
            self.breadcrumbs.append(&separator);
            let button = Button::with_label(segment);
            button.add_css_class("flat");
            button.add_css_class("breadcrumb-segment");
            if index + 1 == segments.len() { button.add_css_class("breadcrumb-current"); }
            self.breadcrumbs.append(&button);
        }
        let trailing = Label::new(None);
        trailing.set_hexpand(true);
        self.breadcrumbs.append(&trailing);
    }

    fn connect_breadcrumb(&self, button: &Button, path: PathBuf, edit_after: bool) {
        let handler = self.on_navigate.clone();
        let stack = self.stack.clone();
        let entry = self.entry.clone();
        button.connect_clicked(move |_| {
            if let Some(callback) = handler.borrow().as_ref() {
                callback(path.clone());
            }
            if edit_after {
                stack.set_visible_child_name("entry");
                entry.grab_focus();
                entry.select_region(0, -1);
            }
        });
    }
}

#[derive(Clone)]
pub struct HeaderControls {
    pub widget: GtkBox,
    location_bar: LocationBar,
    back: Button,
    forward: Button,
    up: Button,
    refresh: Button,
    on_file_action: Rc<RefCell<Option<Box<dyn Fn(FileAction)>>>>,
}

impl HeaderControls {
    pub fn new(location: Rc<RefCell<PathBuf>>) -> Self {
        let widget = GtkBox::new(Orientation::Horizontal, 6);
        widget.set_margin_top(6);
        widget.set_margin_bottom(6);
        widget.set_margin_start(8);
        widget.set_margin_end(8);

        let back = Button::from_icon_name("go-previous-symbolic");
        back.set_tooltip_text(Some("Go back"));
        let forward = Button::from_icon_name("go-next-symbolic");
        forward.set_tooltip_text(Some("Go forward"));
        let up = Button::from_icon_name("go-up-symbolic");
        up.set_tooltip_text(Some("Go up"));
        let refresh = Button::from_icon_name("view-refresh-symbolic");
        refresh.set_tooltip_text(Some("Refresh"));
        let navigation = GtkBox::new(Orientation::Horizontal, 0);
        navigation.add_css_class("linked");
        for button in [&back, &forward, &up, &refresh] {
            button.add_css_class("flat");
            navigation.append(button);
        }
        widget.append(&navigation);
        widget.append(&Separator::new(Orientation::Vertical));

        let location_bar = LocationBar::new(&location.borrow());
        widget.append(&location_bar.widget);

        let on_file_action: Rc<RefCell<Option<Box<dyn Fn(FileAction)>>>> = Rc::new(RefCell::new(None));
        let actions = GtkBox::new(Orientation::Horizontal, 0);
        actions.add_css_class("linked");
        for (action, label, icon) in [
            (FileAction::Cut, "Cut", "edit-cut-symbolic"),
            (FileAction::Copy, "Copy", "edit-copy-symbolic"),
            (FileAction::Paste, "Paste", "edit-paste-symbolic"),
            (FileAction::MoveToTrash, "Move to Trash", "user-trash-symbolic"),
        ] {
            let button = Button::from_icon_name(icon);
            button.set_tooltip_text(Some(label));
            button.set_icon_name(icon);
            button.add_css_class("flat");
            let callback = on_file_action.clone();
            button.connect_clicked(move |_| {
                if let Some(callback) = callback.borrow().as_ref() {
                    callback(action);
                }
            });
            actions.append(&button);
        }
        widget.append(&actions);

        Self { widget, location_bar, back, forward, up, refresh, on_file_action }
    }

    pub fn connect_location_changed<F: Fn(PathBuf) + 'static>(&self, callback: F) {
        self.location_bar.connect_location_changed(callback);
    }

    pub fn set_navigation_handler<F: Fn(PathBuf) + 'static>(&self, callback: F) {
        self.location_bar.set_navigation_handler(callback);
    }

    pub fn connect_navigation<F: Fn(NavigationAction) + 'static>(&self, callback: F) {
        let callback = Rc::new(callback);
        for (button, action) in [
            (&self.back, NavigationAction::Back),
            (&self.forward, NavigationAction::Forward),
            (&self.up, NavigationAction::Up),
            (&self.refresh, NavigationAction::Refresh),
        ] {
            let callback = callback.clone();
            button.connect_clicked(move |_| callback(action));
        }
    }

    pub fn connect_file_action<F: Fn(FileAction) + 'static>(&self, callback: F) {
        *self.on_file_action.borrow_mut() = Some(Box::new(callback));
    }

    pub fn set_location(&self, path: &std::path::Path) {
        self.location_bar.set_location(path);
    }

    pub fn set_uri(&self, uri: &str) {
        self.location_bar.set_uri(uri);
    }
}
