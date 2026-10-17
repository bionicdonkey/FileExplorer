use crate::application::navigation::NavigationCoordinator;
use crate::core::{
    contracts::{Capability, EntryKind, EntrySnapshot, MutationKind},
    navigation::{NavigationStatus, TabState},
    Location, OperationManager, SidebarModel,
};
use crate::infrastructure::gio_directory::GioDirectoryRepository;
use crate::ui::header::LocationBar;
use adw::prelude::*;
use gtk::{Box as GtkBox, Button, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow};
use std::{cell::RefCell, rc::Rc};

/// A destination picker sharing the URI navigation and cancellation contract.
#[derive(Clone)]
pub struct FolderPicker {
    navigation: NavigationCoordinator,
    render: Rc<dyn Fn()>,
}
impl FolderPicker {
    pub fn new(
        initial: Location,
        repository: Rc<dyn crate::core::ports::DirectoryRepository>,
    ) -> Self {
        Self {
            navigation: NavigationCoordinator::new(
                Rc::new(RefCell::new(TabState::new(initial))),
                repository,
            ),
            render: Rc::new(|| {}),
        }
    }
    pub fn current_location(&self) -> Location {
        self.navigation.state.borrow().location.clone()
    }
    pub fn entries(&self) -> Vec<EntrySnapshot> {
        navigable_entries(&self.navigation.state.borrow())
    }
    pub fn activate_entry(&self, index: usize) {
        if let Some(entry) = self.entries().get(index) {
            self.select_location(entry.location.clone());
        }
    }
    pub fn select_location(&self, location: Location) {
        let render = self.render.clone();
        self.navigation.navigate(location, move |accepted| {
            if accepted {
                render();
            }
        });
        (self.render)();
    }
}

fn navigable_entries(state: &TabState) -> Vec<EntrySnapshot> {
    let mut entries = state
        .directory
        .as_ref()
        .map(|snapshot| {
            snapshot
                .entries
                .iter()
                .filter(|entry| {
                    matches!(entry.kind, EntryKind::Directory | EntryKind::Shortcut)
                        && !entry.display_name.starts_with('.')
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    entries.sort_by(|left, right| left.display_name.cmp(&right.display_name));
    entries
}

/// Where the files are headed, as the user thinks of it: a path under the home
/// folder starts with `~`, and anything that is not local is shown as its URI.
fn destination_text(location: &Location) -> String {
    let shown = match location.local_path() {
        Some(path) => match std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .and_then(|home| path.strip_prefix(&home).ok().map(|rest| rest.to_owned()))
        {
            Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
            Some(rest) => format!("~/{}", rest.display()),
            None => path.display().to_string(),
        },
        None => location.uri().to_owned(),
    };
    format!("Destination: {shown}")
}

fn flat_icon_button(icon: &str, tooltip: &str) -> Button {
    let button = Button::from_icon_name(icon);
    button.add_css_class("flat");
    button.set_tooltip_text(Some(tooltip));
    crate::ui::accessibility::name_control(&button, tooltip);
    button
}

fn placeholder_row(text: &str) -> ListBoxRow {
    let label = Label::new(Some(text));
    label.add_css_class("dim-label");
    label.set_margin_top(18);
    label.set_margin_bottom(18);
    let row = ListBoxRow::new();
    row.set_child(Some(&label));
    row.set_activatable(false);
    row.set_selectable(false);
    row
}

pub fn pick_folder<F: Fn(Location) + 'static>(
    parent: &impl IsA<gtk::Widget>,
    title: &str,
    action_label: &str,
    initial: Location,
    sidebar_model: Rc<RefCell<SidebarModel>>,
    operations: OperationManager,
    on_selected: F,
) -> FolderPicker {
    let dialog = adw::Dialog::new();
    dialog.set_title(title);
    dialog.set_content_width(760);
    dialog.set_content_height(520);
    let toolbar_view = adw::ToolbarView::new();
    let titlebar = adw::HeaderBar::new();
    titlebar.set_title_widget(Some(&Label::new(Some(title))));
    toolbar_view.add_top_bar(&titlebar);
    let content = GtkBox::new(Orientation::Vertical, 0);
    let toolbar = GtkBox::new(Orientation::Horizontal, 6);
    toolbar.set_margin_start(12);
    toolbar.set_margin_end(12);
    toolbar.set_margin_top(8);
    toolbar.set_margin_bottom(8);
    let back = flat_icon_button("go-previous-symbolic", "Back");
    let forward = flat_icon_button("go-next-symbolic", "Forward");
    let up = flat_icon_button("go-up-symbolic", "Up");
    let new_folder = flat_icon_button("folder-new-symbolic", "New Folder");
    let location_bar = LocationBar::new(&initial);
    location_bar.widget.set_hexpand(true);
    for button in [&back, &forward, &up] {
        toolbar.append(button);
    }
    toolbar.append(&location_bar.widget);
    toolbar.append(&new_folder);
    content.append(&toolbar);
    content.append(&gtk::Separator::new(Orientation::Horizontal));
    let body = gtk::Paned::new(Orientation::Horizontal);
    body.set_vexpand(true);
    body.set_position(210);
    body.set_resize_start_child(false);
    body.set_shrink_start_child(false);
    // Places on the left, like the main window's sidebar.
    let shortcuts = ListBox::new();
    shortcuts.add_css_class("navigation-sidebar");
    shortcuts.set_selection_mode(gtk::SelectionMode::None);
    shortcuts.set_activate_on_single_click(true);
    let shortcuts_scroll = ScrolledWindow::new();
    shortcuts_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    shortcuts_scroll.set_child(Some(&shortcuts));
    body.set_start_child(Some(&shortcuts_scroll));
    let list = ListBox::new();
    list.add_css_class("navigation-sidebar");
    list.set_activate_on_single_click(false);
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.update_property(&[gtk::accessible::Property::Label("Folders")]);
    let scroll = ScrolledWindow::new();
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroll.set_child(Some(&list));
    body.set_end_child(Some(&scroll));
    content.append(&body);
    content.append(&gtk::Separator::new(Orientation::Horizontal));
    // One row: where the files are headed, then the choices.
    let destination = Label::new(None);
    destination.add_css_class("picker-destination");
    destination.set_hexpand(true);
    destination.set_xalign(0.0);
    destination.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let select = Button::with_label(action_label);
    select.add_css_class("suggested-action");
    let cancel = Button::with_label("Cancel");
    let footer = GtkBox::new(Orientation::Horizontal, 8);
    footer.set_margin_start(12);
    footer.set_margin_end(12);
    footer.set_margin_top(10);
    footer.set_margin_bottom(10);
    footer.append(&destination);
    footer.append(&cancel);
    footer.append(&select);
    content.append(&footer);
    toolbar_view.set_content(Some(&content));
    dialog.set_child(Some(&toolbar_view));

    let repository = Rc::new(GioDirectoryRepository::new(
        glib::MainContext::ref_thread_default(),
    ));
    let mount_parent = parent.clone().upcast::<gtk::Widget>().downgrade();
    repository.set_mount_operation(move || {
        let parent = mount_parent
            .upgrade()
            .and_then(|widget| widget.root())
            .and_downcast::<gtk::Window>();
        gtk::MountOperation::new(parent.as_ref()).upcast()
    });
    let mut picker = FolderPicker::new(initial.clone(), repository);
    let state = picker.navigation.state.clone();
    let folders = Rc::new(RefCell::new(Vec::<Location>::new()));
    let selected = Rc::new(RefCell::new(None::<Location>));
    let render: Rc<dyn Fn()> = {
        let state = state.clone();
        let list = list.clone();
        let folders = folders.clone();
        let selected = selected.clone();
        let destination = destination.clone();
        let select = select.clone();
        let back = back.clone();
        let forward = forward.clone();
        let up = up.clone();
        let location_bar = location_bar.clone();
        let new_folder = new_folder.clone();
        Rc::new(move || {
            let snapshot = state.borrow().clone();
            select.set_sensitive(matches!(snapshot.status, NavigationStatus::Ready));
            new_folder.set_sensitive(
                matches!(snapshot.status, NavigationStatus::Ready)
                    && snapshot
                        .directory
                        .as_ref()
                        .is_some_and(|d| d.capabilities.supports(Capability::Create)),
            );
            back.set_sensitive(snapshot.history.can_go_back());
            forward.set_sensitive(snapshot.history.can_go_forward());
            up.set_sensitive(snapshot.location.parent().is_some());
            match snapshot.status {
                NavigationStatus::Loading(_) => {
                    destination.set_text("Loading folder…");
                    return;
                }
                NavigationStatus::Failed { .. } => {
                    destination
                        .set_text("This location could not be opened. Enter a location to retry.");
                    return;
                }
                _ => {}
            }
            while let Some(row) = list.row_at_index(0) {
                list.remove(&row);
            }
            *selected.borrow_mut() = None;
            folders.borrow_mut().clear();
            for entry in navigable_entries(&snapshot) {
                let row = ListBoxRow::new();
                row.add_css_class("picker-folder-row");
                let row_content = GtkBox::new(Orientation::Horizontal, 10);
                row_content.set_margin_top(6);
                row_content.set_margin_bottom(6);
                row_content.set_margin_start(6);
                row_content.set_margin_end(6);
                row_content.append(&gtk::Image::from_icon_name("folder"));
                let name = Label::new(Some(&entry.display_name));
                name.set_xalign(0.0);
                name.set_hexpand(true);
                name.set_ellipsize(gtk::pango::EllipsizeMode::End);
                row_content.append(&name);
                row.set_child(Some(&row_content));
                folders.borrow_mut().push(entry.location);
                list.append(&row);
            }
            if folders.borrow().is_empty() {
                list.append(&placeholder_row("No subfolders"));
            }
            location_bar.set_uri(snapshot.location.uri());
            destination.set_text(&destination_text(&snapshot.location));
        })
    };
    picker.render = render;
    location_bar.set_navigation_handler({
        let picker = picker.clone();
        move |location| picker.select_location(location)
    });
    location_bar.connect_location_changed({
        let picker = picker.clone();
        move |location| picker.select_location(location)
    });
    list.connect_row_selected({
        let selected = selected.clone();
        let folders = folders.clone();
        let state = state.clone();
        move |_, row| {
            *selected.borrow_mut() =
                row.and_then(|row| folders.borrow().get(row.index() as usize).cloned());
            let target = selected
                .borrow()
                .clone()
                .unwrap_or_else(|| state.borrow().location.clone());
            destination.set_text(&destination_text(&target));
        }
    });
    list.connect_row_activated({
        let picker = picker.clone();
        move |_, row| {
            picker.activate_entry(row.index() as usize);
        }
    });
    let places = Rc::new(RefCell::new(Vec::<Option<Location>>::new()));
    for (index, section) in sidebar_model.borrow().sections().into_iter().enumerate() {
        let heading = Label::new(Some(&section.title));
        heading.add_css_class("heading");
        heading.add_css_class("dim-label");
        heading.set_xalign(0.0);
        heading.set_margin_start(6);
        heading.set_margin_end(6);
        heading.set_margin_top(if index == 0 { 8 } else { 14 });
        heading.set_margin_bottom(4);
        let heading_row = ListBoxRow::new();
        heading_row.set_child(Some(&heading));
        heading_row.set_activatable(false);
        heading_row.set_selectable(false);
        shortcuts.append(&heading_row);
        places.borrow_mut().push(None);
        for node in section.nodes {
            let line = GtkBox::new(Orientation::Horizontal, 10);
            line.set_margin_top(4);
            line.set_margin_bottom(4);
            line.set_margin_start(6);
            line.set_margin_end(6);
            line.append(&gtk::Image::from_icon_name(node.location.icon_name()));
            let label = Label::new(Some(node.location.label()));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            line.append(&label);
            let row = ListBoxRow::new();
            row.add_css_class("picker-folder-row");
            row.set_child(Some(&line));
            shortcuts.append(&row);
            places
                .borrow_mut()
                .push(Some(node.location.location().clone()));
        }
    }
    shortcuts.connect_row_activated({
        let picker = picker.clone();
        move |_, row| {
            let location = places.borrow().get(row.index() as usize).cloned().flatten();
            if let Some(location) = location {
                picker.select_location(location);
            }
        }
    });
    for (button, backwards) in [(&back, true), (&forward, false)] {
        let picker = picker.clone();
        button.connect_clicked(move |_| {
            let render = picker.render.clone();
            if backwards {
                picker.navigation.back(move |accepted| {
                    if accepted {
                        render();
                    }
                });
            } else {
                picker.navigation.forward(move |accepted| {
                    if accepted {
                        render();
                    }
                });
            }
            (picker.render)();
        });
    }
    up.connect_clicked({
        let picker = picker.clone();
        let state = state.clone();
        move |_| {
            let parent = state.borrow().location.parent();
            if let Some(parent) = parent {
                picker.select_location(parent);
            }
        }
    });
    new_folder.connect_clicked({
        let picker = picker.clone();
        let state = state.clone();
        move |_| {
            let parent = state.borrow().location.clone();
            if !state
                .borrow()
                .directory
                .as_ref()
                .is_some_and(|d| d.capabilities.supports(Capability::Create))
            {
                return;
            }
            if let Ok(target) = crate::application::commands::named_child(&parent, "New Folder") {
                let id = operations.submit_locations(
                    MutationKind::Create,
                    vec![target.clone()],
                    None,
                    None,
                    None,
                    false,
                );
                let operations = operations.clone();
                let picker = picker.clone();
                let state = state.clone();
                glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                    if matches!(state.borrow().status, NavigationStatus::Closed) {
                        return glib::ControlFlow::Break;
                    }
                    let Some(record) = operations
                        .records()
                        .into_iter()
                        .find(|record| record.id == id)
                    else {
                        return glib::ControlFlow::Break;
                    };
                    if record.finished_at.is_none() {
                        return glib::ControlFlow::Continue;
                    }
                    if record.state == crate::core::OperationState::Completed {
                        picker.select_location(target.clone());
                    }
                    glib::ControlFlow::Break
                });
            }
        }
    });
    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });
    select.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            if !matches!(state.borrow().status, NavigationStatus::Ready) {
                return;
            }
            let location = selected
                .borrow()
                .clone()
                .unwrap_or_else(|| state.borrow().location.clone());
            on_selected(location);
            dialog.close();
        }
    });
    dialog.connect_closed({
        let picker = picker.clone();
        move |_| picker.navigation.close_tab()
    });
    picker.select_location(initial);
    dialog.present(parent);
    picker
}
