use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{
    Box as GtkBox, Button, Entry, GestureClick, Image, Label, Orientation, Separator, Stack,
};

use crate::core::location::{Location, LocationInput};
use crate::ui::context_menu::{configure_command_button, CommandAvailability, ContextCommand};

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
    on_navigate: Rc<RefCell<Option<Box<dyn Fn(Location)>>>>,
    committed_uri: Rc<RefCell<String>>,
}

impl LocationBar {
    pub fn new(initial: &Location) -> Self {
        let entry = Entry::builder().hexpand(true).build();
        entry.set_tooltip_text(Some("Location (Ctrl+L)"));
        entry.update_property(&[
            gtk::accessible::Property::Label("Location"),
            gtk::accessible::Property::KeyShortcuts("Control+l"),
        ]);
        entry.add_css_class("monospace");
        entry.add_css_class("location-entry");
        entry.set_text(initial.uri());
        let breadcrumbs = GtkBox::new(Orientation::Horizontal, 2);
        breadcrumbs.set_hexpand(true);
        breadcrumbs.add_css_class("location-bar");
        let stack = Stack::new();
        stack.set_hexpand(true);
        // Long paths scroll without increasing the window's minimum width.
        let breadcrumb_scroll = gtk::ScrolledWindow::new();
        breadcrumb_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
        breadcrumb_scroll.set_propagate_natural_width(false);
        breadcrumb_scroll.set_min_content_width(40);
        breadcrumb_scroll.set_child(Some(&breadcrumbs));
        stack.set_hhomogeneous(false);
        stack.set_transition_type(gtk::StackTransitionType::None);
        stack.add_named(&breadcrumb_scroll, Some("breadcrumbs"));
        stack.add_named(&entry, Some("entry"));
        stack.set_visible_child_name("breadcrumbs");
        let committed_uri = Rc::new(RefCell::new(initial.uri().to_owned()));
        let cancel_uri = committed_uri.clone();
        let cancel_stack = stack.downgrade();
        let cancel_entry = entry.downgrade();
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key != gtk::gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            if let (Some(stack), Some(entry)) = (cancel_stack.upgrade(), cancel_entry.upgrade()) {
                entry.set_text(&cancel_uri.borrow());
                entry.remove_css_class("error");
                entry.reset_state(gtk::AccessibleState::Invalid);
                entry.reset_property(gtk::AccessibleProperty::Description);
                entry.set_tooltip_text(Some("Location (Ctrl+L)"));
                stack.set_visible_child_name("breadcrumbs");
                stack.child_focus(gtk::DirectionType::TabForward);
            }
            glib::Propagation::Stop
        });
        entry.add_controller(keys);

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
        let bar = Self {
            widget: stack.clone(),
            entry,
            breadcrumbs,
            stack,
            on_navigate,
            committed_uri,
        };
        bar.set_uri(initial.uri());
        bar
    }

    pub fn connect_location_changed<F: Fn(Location) + 'static>(&self, callback: F) {
        let stack = self.stack.clone();
        self.entry.connect_activate(move |entry| {
            match LocationInput::parse(entry.text().as_str()) {
                Ok(location) => {
                    entry.remove_css_class("error");
                    entry.reset_state(gtk::AccessibleState::Invalid);
                    entry.reset_property(gtk::AccessibleProperty::Description);
                    entry.set_tooltip_text(Some("Location (Ctrl+L)"));
                    callback(location);
                    stack.set_visible_child_name("breadcrumbs");
                }
                Err(_) => {
                    entry.update_state(&[gtk::accessible::State::Invalid(
                        gtk::AccessibleInvalidState::True,
                    )]);
                    entry.update_property(&[gtk::accessible::Property::Description(
                        "Enter an absolute path or a URI without credentials",
                    )]);
                    crate::ui::accessibility::AccessibleStatus::announce_for(
                        entry,
                        "Enter an absolute path or a URI without credentials",
                        crate::ui::accessibility::StatusPriority::Urgent,
                    );
                    entry.add_css_class("error");
                    entry.set_tooltip_text(Some(
                        "Enter an absolute path or a URI without credentials",
                    ));
                }
            }
        });
    }

    pub fn set_navigation_handler<F: Fn(Location) + 'static>(&self, callback: F) {
        *self.on_navigate.borrow_mut() = Some(Box::new(callback));
        self.render_breadcrumbs_from_entry();
    }

    pub fn set_location(&self, path: &std::path::Path) {
        self.clear_validation_error();
        *self.committed_uri.borrow_mut() = Location::from_local_path(path).uri().to_owned();
        self.entry.set_text(&path.display().to_string());
        self.render_breadcrumbs(path);
        self.stack.set_visible_child_name("breadcrumbs");
    }

    pub fn set_uri(&self, uri: &str) {
        self.clear_validation_error();
        *self.committed_uri.borrow_mut() = uri.to_owned();
        self.entry.set_text(uri);
        self.render_uri_breadcrumbs(uri);
        self.stack.set_visible_child_name("breadcrumbs");
    }

    fn clear_validation_error(&self) {
        self.entry.remove_css_class("error");
        self.entry.reset_state(gtk::AccessibleState::Invalid);
        self.entry
            .reset_property(gtk::AccessibleProperty::Description);
        self.entry.set_tooltip_text(Some("Location (Ctrl+L)"));
    }

    pub fn focus_location(&self) {
        self.stack.set_visible_child_name("entry");
        self.entry.grab_focus();
        self.entry.select_region(0, -1);
    }

    pub fn set_section(&self, title: &str) {
        while let Some(child) = self.breadcrumbs.first_child() {
            self.breadcrumbs.remove(&child);
        }
        let label = Label::new(Some(title));
        label.add_css_class("breadcrumb-current");
        label.set_xalign(0.0);
        self.breadcrumbs.append(&label);
        let trailing = Label::new(None);
        trailing.set_hexpand(true);
        self.breadcrumbs.append(&trailing);
        self.stack.set_visible_child_name("breadcrumbs");
    }

    fn render_breadcrumbs_from_entry(&self) {
        if let Ok(location) = LocationInput::parse(self.entry.text().as_str()) {
            self.set_uri(location.uri());
        }
    }

    fn render_breadcrumbs(&self, path: &std::path::Path) {
        while let Some(child) = self.breadcrumbs.first_child() {
            self.breadcrumbs.remove(&child);
        }
        let home = Location::home().local_path().expect("home is local");
        let under_home = path.starts_with(&home);
        let mut accumulated = if under_home {
            home.clone()
        } else {
            PathBuf::from("/")
        };
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
        let remaining = if under_home {
            path.strip_prefix(&home).unwrap_or(path)
        } else {
            path
        };
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
        if let Ok(location) = Location::from_uri(uri) {
            if let Some(path) = location.local_path() {
                self.render_breadcrumbs(&path);
                return;
            }
        }
        while let Some(child) = self.breadcrumbs.first_child() {
            self.breadcrumbs.remove(&child);
        }

        let Ok(location) = Location::from_uri(uri) else {
            return;
        };
        // Provider parent handles preserve escaping and provider-specific roots.
        // Show compact labels immediately, then update from asynchronous metadata.
        let mut chain = vec![location];
        while chain.len() < 8 {
            let Some(parent) = chain.last().and_then(Location::parent) else {
                break;
            };
            if chain.contains(&parent) {
                break;
            }
            chain.push(parent);
        }
        chain.reverse();
        for (index, location) in chain.iter().enumerate() {
            if index > 0 {
                self.breadcrumbs.append(&Label::new(Some("/")));
            }
            let button = Button::with_label(&location.display_name());
            button.add_css_class("flat");
            button.add_css_class("breadcrumb-segment");
            let current = index + 1 == chain.len();
            if current {
                button.add_css_class("breadcrumb-current");
            }
            self.connect_location_breadcrumb(&button, location.clone(), current);
            self.breadcrumbs.append(&button);
            let weak = button.downgrade();
            gio::File::for_uri(location.uri()).query_info_async(
                "standard::display-name",
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
                gio::Cancellable::NONE,
                move |result| {
                    if let (Some(button), Ok(info)) = (weak.upgrade(), result) {
                        if !info.display_name().is_empty() {
                            button.set_label(&info.display_name());
                            button.update_property(&[gtk::accessible::Property::Label(
                                &info.display_name(),
                            )]);
                        }
                    }
                },
            );
        }
        let trailing = Label::new(None);
        trailing.set_hexpand(true);
        self.breadcrumbs.append(&trailing);
    }

    fn connect_breadcrumb(&self, button: &Button, path: PathBuf, edit_after: bool) {
        self.connect_location_breadcrumb(button, Location::from_local_path(&path), edit_after);
    }

    fn connect_location_breadcrumb(&self, button: &Button, location: Location, edit_after: bool) {
        button.update_property(&[
            gtk::accessible::Property::Label(button.label().as_deref().unwrap_or("Home")),
            gtk::accessible::Property::Description(if edit_after {
                "Current location. Activate to edit location"
            } else {
                "Open parent location"
            }),
        ]);
        button.set_tooltip_text(Some(&location.redacted()));
        let handler = self.on_navigate.clone();
        let stack = self.stack.clone();
        let entry = self.entry.clone();
        button.connect_clicked(move |_| {
            if let Some(callback) = handler.borrow().as_ref() {
                callback(location.clone());
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
    cut: Button,
    copy: Button,
    paste: Button,
    trash: Button,
    on_file_action: Rc<RefCell<Option<Box<dyn Fn(FileAction)>>>>,
}

impl HeaderControls {
    /// Switching tabs always restores the edit URI before an overview label.
    pub fn set_tab_content(&self, content: &crate::core::navigation::TabContent) {
        use crate::core::navigation::TabContent;
        let location = match content {
            TabContent::Directory(location) | TabContent::Unavailable(location) => location,
            TabContent::SectionOverview { underlying, .. }
            | TabContent::Search { underlying, .. } => underlying,
        };
        self.set_uri(location.uri());
        if let TabContent::SectionOverview { section, .. } = content {
            self.set_section(&section.title);
        }
    }

    pub fn new(location: Rc<RefCell<Location>>) -> Self {
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
            crate::ui::accessibility::name_control(
                button,
                button.tooltip_text().as_deref().unwrap(),
            );
            button.add_css_class("flat");
            navigation.append(button);
        }
        widget.append(&navigation);
        widget.append(&Separator::new(Orientation::Vertical));

        let location_bar = LocationBar::new(&location.borrow());
        widget.append(&location_bar.widget);

        let on_file_action: Rc<RefCell<Option<Box<dyn Fn(FileAction)>>>> =
            Rc::new(RefCell::new(None));
        let actions = GtkBox::new(Orientation::Horizontal, 0);
        actions.add_css_class("linked");
        let mut action_buttons = Vec::new();
        for (action, label, icon) in [
            (FileAction::Cut, "Cut", "edit-cut-symbolic"),
            (FileAction::Copy, "Copy", "edit-copy-symbolic"),
            (FileAction::Paste, "Paste", "edit-paste-symbolic"),
            (
                FileAction::MoveToTrash,
                "Move to Trash",
                "user-trash-symbolic",
            ),
        ] {
            let button = Button::from_icon_name(icon);
            button.set_tooltip_text(Some(label));
            button.set_icon_name(icon);
            button.update_property(&[gtk::accessible::Property::Label(label)]);
            button.add_css_class("flat");
            let callback = on_file_action.clone();
            button.connect_clicked(move |_| {
                if let Some(callback) = callback.borrow().as_ref() {
                    callback(action);
                }
            });
            action_buttons.push(button.clone());
            actions.append(&button);
        }
        widget.append(&actions);

        Self {
            widget,
            location_bar,
            back,
            forward,
            up,
            refresh,
            cut: action_buttons.remove(0),
            copy: action_buttons.remove(0),
            paste: action_buttons.remove(0),
            trash: action_buttons.remove(0),
            on_file_action,
        }
    }

    pub fn connect_location_changed<F: Fn(Location) + 'static>(&self, callback: F) {
        self.location_bar.connect_location_changed(callback);
    }

    pub fn focus_location(&self) {
        self.location_bar.focus_location();
    }

    pub fn set_navigation_handler<F: Fn(Location) + 'static>(&self, callback: F) {
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

    pub fn set_file_action_availability(&self, availability: &CommandAvailability) {
        for (button, action, label) in [
            (&self.cut, ContextCommand::Cut, "Cut"),
            (&self.copy, ContextCommand::Copy, "Copy"),
            (&self.paste, ContextCommand::Paste, "Paste"),
            (&self.trash, ContextCommand::MoveToTrash, "Move to Trash"),
        ] {
            configure_command_button(button, availability, action);
            if availability.allows(action) {
                button.set_tooltip_text(Some(label));
            }
        }
    }

    pub fn set_location(&self, path: &std::path::Path) {
        self.location_bar.set_location(path);
    }

    pub fn set_uri(&self, uri: &str) {
        self.location_bar.set_uri(uri);
    }

    pub fn set_section(&self, title: &str) {
        self.location_bar.set_section(title);
    }

    #[cfg(test)]
    pub(crate) fn displayed_location_text(&self) -> String {
        self.location_bar.entry.text().to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gtk::prelude::*;

    use super::HeaderControls;
    use crate::core::FileItem;
    use crate::ui::context_menu::CommandAvailability;

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn toolbar_actions_follow_the_shared_availability_query() {
        gtk::init().unwrap();
        let header = HeaderControls::new(Rc::new(RefCell::new(
            crate::core::Location::from_uri("file:///tmp").unwrap(),
        )));
        let parent = gio::File::for_uri("smb://host/share");
        let info = gio::FileInfo::new();
        info.set_name("report.txt");
        info.set_display_name("report.txt");
        let remote = FileItem::from_gio(&parent, &info);

        header.set_file_action_availability(&CommandAvailability::for_selection(&[remote], None));

        assert!(!header.cut.is_sensitive());
        assert_eq!(
            header.trash.tooltip_text().as_deref(),
            Some("This location does not support Trash")
        );
    }
}
