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
    let content = GtkBox::new(Orientation::Vertical, 8);
    let toolbar = GtkBox::new(Orientation::Horizontal, 6);
    let back = Button::from_icon_name("go-previous-symbolic");
    let forward = Button::from_icon_name("go-next-symbolic");
    let up = Button::from_icon_name("go-up-symbolic");
    let new_folder = Button::from_icon_name("folder-new-symbolic");
    new_folder.set_tooltip_text(Some("New Folder"));
    let location_bar = LocationBar::new(&initial);
    for button in [&back, &forward, &up] {
        toolbar.append(button);
    }
    toolbar.append(&location_bar.widget);
    toolbar.append(&new_folder);
    content.append(&toolbar);
    let body = gtk::Paned::new(Orientation::Horizontal);
    body.set_vexpand(true);
    body.set_position(180);
    let shortcuts = GtkBox::new(Orientation::Vertical, 4);
    let shortcuts_scroll = ScrolledWindow::new();
    shortcuts_scroll.set_child(Some(&shortcuts));
    body.set_start_child(Some(&shortcuts_scroll));
    let list = ListBox::new();
    list.set_activate_on_single_click(false);
    list.set_selection_mode(gtk::SelectionMode::Single);
    let scroll = ScrolledWindow::new();
    scroll.set_child(Some(&list));
    body.set_end_child(Some(&scroll));
    content.append(&body);
    let destination = Label::new(None);
    destination.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    content.append(&destination);
    let select = Button::with_label(action_label);
    select.add_css_class("suggested-action");
    let cancel = Button::with_label("Cancel");
    let footer = GtkBox::new(Orientation::Horizontal, 8);
    footer.append(&cancel);
    footer.append(&select);
    toolbar_view.add_bottom_bar(&footer);
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
                let row_content = GtkBox::new(Orientation::Horizontal, 8);
                row_content.set_margin_top(6);
                row_content.set_margin_bottom(6);
                row_content.set_margin_start(8);
                row_content.set_margin_end(8);
                row_content.append(&gtk::Image::from_icon_name("folder-symbolic"));
                row_content.append(&Label::new(Some(&entry.display_name)));
                row.set_child(Some(&row_content));
                folders.borrow_mut().push(entry.location);
                list.append(&row);
            }
            location_bar.set_uri(snapshot.location.uri());
            destination.set_text(&format!("Destination: {}", snapshot.location.uri()));
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
            destination.set_text(&format!("Destination: {}", target.uri()));
        }
    });
    list.connect_row_activated({
        let picker = picker.clone();
        move |_, row| {
            picker.activate_entry(row.index() as usize);
        }
    });
    for section in sidebar_model.borrow().sections() {
        shortcuts.append(&Label::new(Some(&section.title)));
        for node in section.nodes {
            let button = Button::with_label(node.location.label());
            let location = node.location.location().clone();
            let picker = picker.clone();
            button.connect_clicked(move |_| picker.select_location(location.clone()));
            shortcuts.append(&button);
        }
    }
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
