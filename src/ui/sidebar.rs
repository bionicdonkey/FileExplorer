use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gio::prelude::*;
use gtk::prelude::*;
use gtk::{
    Box as GtkBox, Button, GestureClick, Image, Label, Orientation, Popover, ScrolledWindow,
    ToggleButton,
};

use crate::core::contracts::{DirectoryRequest, PageRequest, RequestId, RequestOutcome};
use crate::core::ports::DirectoryRepository;
use crate::core::sidebar_model::SidebarSection;
use crate::core::{Location, OperationManager};
use crate::core::{SidebarLocation, SidebarModel, SidebarNode};
use crate::infrastructure::gio_directory::GioDirectoryRepository;
use crate::ui::operations_page::active_summary;

#[derive(Clone, Debug)]
pub enum SidebarAction {
    OpenSection(String),
    Open(Location),
    OpenInNewTab(Location),
    OpenInNewWindow(Location),
    OpenInTerminal(PathBuf),
    Pin(Location),
    Unpin(Location),
    AddNetworkLocation,
    Unmount(Location),
    OpenOperations,
}

struct PendingFocus {
    key: String,
    occurrence: Option<Vec<String>>,
    token: u64,
    pending_requests: BTreeSet<RequestId>,
    fallback: GtkBox,
}

#[derive(Clone)]
pub struct Sidebar {
    pub widget: ScrolledWindow,
    content: GtkBox,
    model: Rc<RefCell<SidebarModel>>,
    repository: Rc<dyn DirectoryRepository>,
    on_action: Rc<dyn Fn(SidebarAction)>,
    active_path: Rc<RefCell<Option<Location>>>,
    active_section: Rc<RefCell<Option<String>>>,
    mount_parent: gtk::Window,
    operation_manager: Rc<RefCell<Option<OperationManager>>>,
    operation_summary: Rc<RefCell<Option<String>>>,
    pending_focus: Rc<RefCell<Option<PendingFocus>>>,
    next_focus_token: Rc<Cell<u64>>,
}

impl Sidebar {
    pub fn new<F: Fn(SidebarAction) + 'static>(
        model: Rc<RefCell<SidebarModel>>,
        mount_parent: &impl IsA<gtk::Window>,
        on_action: F,
    ) -> Self {
        let repository = Rc::new(GioDirectoryRepository::new(
            glib::MainContext::ref_thread_default(),
        ));
        let window: gtk::Window = mount_parent.clone().upcast();
        repository.set_mount_operation(move || gtk::MountOperation::new(Some(&window)).upcast());
        Self::new_with_repository(model, repository, mount_parent, on_action)
    }

    pub fn new_with_repository<F: Fn(SidebarAction) + 'static>(
        model: Rc<RefCell<SidebarModel>>,
        repository: Rc<dyn DirectoryRepository>,
        mount_parent: &impl IsA<gtk::Window>,
        on_action: F,
    ) -> Self {
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
            repository,
            on_action: Rc::new(on_action),
            active_path: Rc::new(RefCell::new(None)),
            active_section: Rc::new(RefCell::new(None)),
            mount_parent: mount_parent.clone().upcast(),
            operation_manager: Rc::new(RefCell::new(None)),
            operation_summary: Rc::new(RefCell::new(None)),
            pending_focus: Rc::new(RefCell::new(None)),
            next_focus_token: Rc::new(Cell::new(1)),
        };
        sidebar.refresh();

        let refresh_on_mount = sidebar.clone();
        gio::VolumeMonitor::get().connect_mount_added(move |_, mount| {
            crate::ui::accessibility::AccessibleStatus::announce_for(
                &refresh_on_mount.widget,
                &format!("Connected {}", mount.name()),
                crate::ui::accessibility::StatusPriority::Polite,
            );
            refresh_on_mount.refresh();
        });
        let refresh_on_unmount = sidebar.clone();
        gio::VolumeMonitor::get().connect_mount_removed(move |_, mount| {
            crate::ui::accessibility::AccessibleStatus::announce_for(
                &refresh_on_unmount.widget,
                &format!("Disconnected {}", mount.name()),
                crate::ui::accessibility::StatusPriority::Urgent,
            );
            refresh_on_unmount.refresh();
        });
        sidebar
    }

    pub fn model(&self) -> Rc<RefCell<SidebarModel>> {
        self.model.clone()
    }

    pub fn refresh(&self) {
        let focused_name = gtk::prelude::GtkWindowExt::focus(&self.mount_parent)
            .filter(|widget| widget.is_ancestor(&self.widget))
            .map(|widget| widget.widget_name());
        let focus_token = self.next_focus_token.get();
        self.next_focus_token
            .set(focus_token.wrapping_add(1).max(1));
        *self.pending_focus.borrow_mut() = focused_name.clone().map(|key| PendingFocus {
            occurrence: Self::focus_occurrence(&key),
            key: key.to_string(),
            token: focus_token,
            pending_requests: BTreeSet::new(),
            fallback: self.content.clone(),
        });
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        let sections = sidebar_sections(&self.model);
        for section in sections {
            let section_is_empty = section.nodes.is_empty();
            let rows = GtkBox::new(Orientation::Vertical, 0);
            rows.add_css_class("sidebar-tree");
            let device_rows = GtkBox::new(Orientation::Vertical, 0);
            let has_devices = section.title == "This PC" && section.nodes.len() > 1;
            for (index, node) in section.nodes.into_iter().enumerate() {
                if should_show_devices_heading(&section.title, index) {
                    let device_heading = GtkBox::new(Orientation::Horizontal, 0);
                    let devices_toggle = ToggleButton::new();
                    devices_toggle.set_widget_name("sidebar-expand-devices");
                    devices_toggle.add_css_class("flat");
                    devices_toggle.add_css_class("sidebar-section-toggle");
                    devices_toggle.add_css_class("sidebar-subsection-toggle");
                    let devices_expanded = self.model.borrow().preferences().devices_expanded();
                    devices_toggle.set_active(devices_expanded);
                    crate::ui::accessibility::name_expander(
                        &devices_toggle,
                        "Expand or collapse Devices",
                    );
                    devices_toggle.set_child(Some(&Image::from_icon_name(if devices_expanded {
                        "pan-down-symbolic"
                    } else {
                        "pan-end-symbolic"
                    })));
                    device_rows.set_visible(devices_expanded);
                    let device_rows_for_toggle = device_rows.clone();
                    let preferences = self.model.clone();
                    devices_toggle.connect_toggled(move |toggle| {
                        let expanded = toggle.is_active();
                        device_rows_for_toggle.set_visible(expanded);
                        toggle.set_child(Some(&Image::from_icon_name(if expanded {
                            "pan-down-symbolic"
                        } else {
                            "pan-end-symbolic"
                        })));
                        let mut model = preferences.borrow_mut();
                        model.preferences_mut().set_devices_expanded(expanded);
                        let _ = model.preferences().save();
                    });
                    device_heading.append(&devices_toggle);
                    // A collection of attached volumes, rather than one physical disk.
                    // Zorin ships this symbolic icon and GTK resolves it through the
                    // currently selected icon theme.
                    let devices_icon = Image::from_icon_name("drive-multidisk-symbolic");
                    devices_icon.set_pixel_size(18);
                    devices_icon.set_margin_start(6);
                    devices_icon.set_margin_end(8);
                    devices_icon.add_css_class("sidebar-location-icon");
                    device_heading.append(&devices_icon);
                    let devices = Label::new(Some("Devices"));
                    devices.add_css_class("sidebar-subheading");
                    devices.set_xalign(0.0);
                    device_heading.append(&devices);
                    rows.append(&device_heading);
                    rows.append(&device_rows);
                }
                if has_devices && index > 0 {
                    self.append_node(&device_rows, node, 1, &section.title, focus_token);
                } else {
                    self.append_node(&rows, node, 0, &section.title, focus_token);
                }
            }

            let heading_row = GtkBox::new(Orientation::Horizontal, 0);
            let toggle = ToggleButton::new();
            toggle.set_widget_name(&format!("sidebar-expand-section-{}", section.title));
            toggle.add_css_class("flat");
            toggle.add_css_class("sidebar-section-toggle");
            let section_title = section.title.to_owned();
            let expanded = self
                .model
                .borrow()
                .preferences()
                .section_expanded(&section_title);
            toggle.set_active(expanded);
            crate::ui::accessibility::name_expander(
                &toggle,
                &format!("Expand or collapse {}", section.title),
            );
            rows.set_visible(expanded);
            toggle.set_child(Some(&Image::from_icon_name(if expanded {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            })));
            let rows_for_toggle = rows.clone();
            let preferences = self.model.clone();
            toggle.connect_toggled(move |toggle| {
                let expanded = toggle.is_active();
                rows_for_toggle.set_visible(expanded);
                toggle.set_child(Some(&Image::from_icon_name(if expanded {
                    "pan-down-symbolic"
                } else {
                    "pan-end-symbolic"
                })));
                let mut model = preferences.borrow_mut();
                model
                    .preferences_mut()
                    .set_section_expanded(section_title.clone(), expanded);
                let _ = model.preferences().save();
            });
            heading_row.append(&toggle);
            let heading = Button::with_label(&section.title);
            heading.set_widget_name(&format!("sidebar-section-{}", section.title));
            heading.set_hexpand(true);
            heading.set_halign(gtk::Align::Fill);
            heading.add_css_class("flat");
            heading.add_css_class("heading");
            heading.add_css_class("sidebar-section-heading");
            if self.active_section.borrow().as_deref() == Some(section.title.as_str()) {
                heading.add_css_class("selected");
                heading
                    .update_property(&[gtk::accessible::Property::Description("Current section")]);
            }
            let action = self.on_action.clone();
            let section_title = section.title.clone();
            heading.connect_clicked(move |_| {
                action(SidebarAction::OpenSection(section_title.clone()))
            });
            if section.title == "Network & online" {
                heading.set_hexpand(true);
                let add = Button::from_icon_name("list-add-symbolic");
                add.set_widget_name("sidebar-add-network");
                add.add_css_class("flat");
                add.set_tooltip_text(Some("Add network location"));
                crate::ui::accessibility::name_control(&add, "Add network location");
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
        if let Some(manager) = self.operation_manager.borrow().as_ref() {
            if let Some(summary) = active_summary(&manager.records()) {
                let button = Button::with_label(&summary);
                button.set_widget_name("sidebar-operations");
                button.add_css_class("flat");
                button.add_css_class("sidebar-operation-summary");
                button.set_halign(gtk::Align::Fill);
                let action = self.on_action.clone();
                button.connect_clicked(move |_| action(SidebarAction::OpenOperations));
                self.content.append(&button);
            }
        }
        Self::restore_pending_focus(&self.pending_focus, focus_token);
    }

    fn focus_occurrence(key: &str) -> Option<Vec<String>> {
        ["sidebar-expand-", "sidebar-location-"]
            .iter()
            .find_map(|prefix| key.strip_prefix(prefix))
            .and_then(|occurrence| serde_json::from_str(occurrence).ok())
    }

    fn restore_focus(widget: &gtk::Widget, key: &str) -> bool {
        if widget.widget_name() == key && widget.is_visible() && widget.is_sensitive() {
            return widget.grab_focus();
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            if Self::restore_focus(&next, key) {
                return true;
            }
            child = next.next_sibling();
        }
        false
    }

    fn restore_pending_focus(pending_focus: &Rc<RefCell<Option<PendingFocus>>>, token: u64) {
        let Some((key, fallback, has_pending_requests)) = pending_focus
            .borrow()
            .as_ref()
            .filter(|pending| pending.token == token)
            .map(|pending| {
                (
                    pending.key.clone(),
                    pending.fallback.clone(),
                    !pending.pending_requests.is_empty(),
                )
            })
        else {
            return;
        };
        if Self::restore_focus(fallback.upcast_ref(), &key) {
            *pending_focus.borrow_mut() = None;
        } else if !has_pending_requests {
            fallback.child_focus(gtk::DirectionType::TabForward);
            *pending_focus.borrow_mut() = None;
        }
    }

    fn register_pending_focus_request(
        pending_focus: &Rc<RefCell<Option<PendingFocus>>>,
        token: u64,
        occurrence: &[String],
        request: RequestId,
    ) {
        let mut pending_focus = pending_focus.borrow_mut();
        let Some(pending) = pending_focus.as_mut() else {
            return;
        };
        if pending.token == token
            && pending.occurrence.as_ref().is_some_and(|target| {
                target.len() > occurrence.len() && target.starts_with(occurrence)
            })
        {
            pending.pending_requests.insert(request);
        }
    }

    fn complete_pending_focus_request(
        pending_focus: &Rc<RefCell<Option<PendingFocus>>>,
        token: u64,
        request: RequestId,
    ) {
        if let Some(pending) = pending_focus
            .borrow_mut()
            .as_mut()
            .filter(|pending| pending.token == token)
        {
            pending.pending_requests.remove(&request);
        }
    }

    fn cancel_pending_focus_request(
        pending_focus: &Rc<RefCell<Option<PendingFocus>>>,
        token: u64,
        request: RequestId,
    ) {
        Self::complete_pending_focus_request(pending_focus, token, request);
        Self::restore_pending_focus(pending_focus, token);
    }

    pub fn set_operation_manager(&self, manager: OperationManager) {
        *self.operation_manager.borrow_mut() = Some(manager.clone());
        let sidebar = self.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            let next = sidebar
                .operation_manager
                .borrow()
                .as_ref()
                .and_then(|manager| active_summary(&manager.records()));
            if operation_summary_changed(&sidebar.operation_summary.borrow(), &next) {
                *sidebar.operation_summary.borrow_mut() = next;
                sidebar.refresh();
            }
            glib::ControlFlow::Continue
        });
    }

    pub fn set_active_path(&self, path: &Path) {
        self.set_active_location(Location::from_local_path(path));
    }

    pub fn set_active_location(&self, location: Location) {
        *self.active_path.borrow_mut() = Some(location);
        *self.active_section.borrow_mut() = None;
        self.refresh();
    }

    pub fn clear_active_path(&self) {
        clear_navigation_selection(&self.active_path, &self.active_section);
        self.refresh();
    }

    pub fn set_active_section(&self, section: impl Into<String>) {
        *self.active_path.borrow_mut() = None;
        *self.active_section.borrow_mut() = Some(section.into());
        self.refresh();
    }

    #[cfg(test)]
    pub(crate) fn active_navigation_selection(&self) -> (Option<PathBuf>, Option<String>) {
        (
            self.active_path
                .borrow()
                .as_ref()
                .and_then(Location::local_path),
            self.active_section.borrow().clone(),
        )
    }

    pub fn toggle_quick_access(&self, path: PathBuf) {
        self.toggle_quick_access_location(Location::from_local_path(&path));
    }

    pub fn toggle_quick_access_location(&self, location: Location) {
        let mut model = self.model.borrow_mut();
        if model.preferences().locations().any(|pin| pin == &location) {
            model.preferences_mut().unpin_location(&location);
        } else {
            model.preferences_mut().pin_location(location);
        }
        let _ = model.preferences().save();
        drop(model);
        self.refresh();
    }

    fn append_node(
        &self,
        parent: &GtkBox,
        node: SidebarNode,
        depth: i32,
        section: &str,
        focus_token: u64,
    ) {
        Self::append_node_inner(
            parent,
            node,
            depth,
            vec![section.to_owned()],
            self.model.clone(),
            self.repository.clone(),
            self.on_action.clone(),
            self.active_path.clone(),
            self.mount_parent.clone(),
            self.pending_focus.clone(),
            focus_token,
        );
    }

    fn append_node_inner(
        parent: &GtkBox,
        node: SidebarNode,
        depth: i32,
        mut occurrence: Vec<String>,
        model: Rc<RefCell<SidebarModel>>,
        repository: Rc<dyn DirectoryRepository>,
        on_action: Rc<dyn Fn(SidebarAction)>,
        active_path: Rc<RefCell<Option<Location>>>,
        mount_parent: gtk::Window,
        pending_focus: Rc<RefCell<Option<PendingFocus>>>,
        focus_token: u64,
    ) {
        // A URI may occur in several sections or under several ancestors in
        // one section. Keep the whole ancestry, without position indices that
        // change when another sibling is inserted. JSON escapes delimiters in
        // section titles/URIs so distinct paths cannot alias one focus key.
        occurrence.push(node.location.uri());
        let focus_key = serde_json::to_string(&occurrence).expect("string paths serialize");
        let row = GtkBox::new(Orientation::Vertical, 0);
        let header = GtkBox::new(Orientation::Horizontal, 0);
        header.set_margin_start(depth * 16);
        header.add_css_class("sidebar-tree-row");

        let children = GtkBox::new(Orientation::Vertical, 0);
        children.set_visible(false);
        if node.expandable {
            let expander = ToggleButton::new();
            expander.set_widget_name(&format!("sidebar-expand-{focus_key}"));
            crate::ui::accessibility::name_expander(
                &expander,
                &format!("Expand or collapse {}", node.location.label()),
            );
            expander.add_css_class("flat");
            expander.add_css_class("sidebar-expander");
            let initially_expanded = model
                .borrow()
                .preferences()
                .location_expanded(&node.location);
            let chevron = Image::from_icon_name(if initially_expanded {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            });
            chevron.set_pixel_size(14);
            expander.set_child(Some(&chevron));
            let children_model = model.clone();
            let children_repository = repository.clone();
            let child_location = node.location.clone();
            let children_for_toggle = children.clone();
            let children_action = on_action.clone();
            let children_active_path = active_path.clone();
            let children_mount_parent = mount_parent.clone();
            let pending_request = Rc::new(RefCell::new(None::<(RequestId, gio::Cancellable)>));
            let unrealized_request = pending_request.clone();
            let focus_for_unrealize = pending_focus.clone();
            expander.connect_unrealize(move |_| {
                if let Some((request, cancellable)) = unrealized_request.borrow_mut().take() {
                    cancellable.cancel();
                    Self::cancel_pending_focus_request(&focus_for_unrealize, focus_token, request);
                }
            });
            let focus_for_toggle = pending_focus.clone();
            expander.connect_toggled(move |button| {
                let expanded = button.is_active();
                if !expanded {
                    if let Some((request, cancellable)) = pending_request.borrow_mut().take() {
                        cancellable.cancel();
                        Self::cancel_pending_focus_request(&focus_for_toggle, focus_token, request);
                    }
                }
                children_for_toggle.set_visible(expanded);
                let icon = if expanded {
                    "pan-down-symbolic"
                } else {
                    "pan-end-symbolic"
                };
                button.set_child(Some(&Image::from_icon_name(icon)));
                {
                    let mut sidebar_model = children_model.borrow_mut();
                    sidebar_model
                        .preferences_mut()
                        .set_location_expanded(&child_location, expanded);
                    let _ = sidebar_model.preferences().save();
                }
                if expanded && children_for_toggle.first_child().is_none() {
                    let cancellable = gio::Cancellable::new();
                    let request = DirectoryRequest {
                        id: RequestId::allocate(),
                        location: child_location.location().clone(),
                        page: PageRequest {
                            offset: 0,
                            limit: usize::MAX,
                        },
                        cancellable: cancellable.clone(),
                    };
                    *pending_request.borrow_mut() = Some((request.id, cancellable.clone()));
                    Self::register_pending_focus_request(
                        &focus_for_toggle,
                        focus_token,
                        &occurrence,
                        request.id,
                    );
                    let pending_result = pending_request.clone();
                    let result_button = button.clone();
                    let children_after_result = children_for_toggle.clone();
                    let model_after_result = children_model.clone();
                    let repository_after_result = children_repository.clone();
                    let action_after_result = children_action.clone();
                    let active_after_result = children_active_path.clone();
                    let parent_after_result = children_mount_parent.clone();
                    let occurrence_after_result = occurrence.clone();
                    let focus_after_result = focus_for_toggle.clone();
                    children_repository.list(
                        request,
                        Box::new(move |completion| {
                            let current = pending_result.borrow().as_ref().map(|(id, _)| *id);
                            if current != Some(completion.id)
                                || cancellable.is_cancelled()
                                || !result_button.is_active()
                            {
                                Self::cancel_pending_focus_request(
                                    &focus_after_result,
                                    focus_token,
                                    completion.id,
                                );
                                return;
                            }
                            pending_result.borrow_mut().take();
                            Self::complete_pending_focus_request(
                                &focus_after_result,
                                focus_token,
                                completion.id,
                            );
                            let RequestOutcome::Succeeded(snapshot) = completion.outcome else {
                                Self::restore_pending_focus(&focus_after_result, focus_token);
                                return;
                            };
                            Self::append_children(
                                &children_after_result,
                                SidebarModel::children_from_snapshot(&snapshot),
                                depth,
                                occurrence_after_result.clone(),
                                model_after_result.clone(),
                                repository_after_result.clone(),
                                action_after_result.clone(),
                                active_after_result.clone(),
                                parent_after_result.clone(),
                                focus_after_result.clone(),
                                focus_token,
                            );
                            Self::restore_pending_focus(&focus_after_result, focus_token);
                        }),
                    );
                }
            });
            if initially_expanded {
                expander.set_active(true);
            }
            header.append(&expander);
        } else {
            let spacer = GtkBox::new(Orientation::Horizontal, 0);
            spacer.set_size_request(32, -1);
            header.append(&spacer);
        }

        let button = Button::new();
        button.set_widget_name(&format!("sidebar-location-{focus_key}"));
        crate::ui::accessibility::name_control(&button, node.location.label());
        button.set_hexpand(true);
        button.set_halign(gtk::Align::Fill);
        button.add_css_class("flat");
        button.add_css_class("sidebar-location");
        if active_path.borrow().as_ref() == Some(node.location.location()) {
            button.add_css_class("selected");
            button.update_property(&[gtk::accessible::Property::Description("Current location")]);
        }
        let button_content = GtkBox::new(Orientation::Horizontal, 8);
        button_content.set_margin_top(4);
        button_content.set_margin_bottom(4);
        let icon = Image::from_icon_name(node.location.icon_name());
        icon.set_pixel_size(18);
        icon.add_css_class("sidebar-location-icon");
        let label = Label::new(Some(node.location.label()));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_hexpand(true);
        button_content.append(&icon);
        button_content.append(&label);
        button.set_child(Some(&button_content));
        let action = on_action.clone();
        let location = node.location.clone();
        button.connect_clicked(move |_| action(SidebarAction::Open(location.location().clone())));
        Self::add_context_menu(&button, &node.location, model, on_action);
        header.append(&button);
        row.append(&header);
        row.append(&children);
        parent.append(&row);
    }

    fn append_children(
        parent: &GtkBox,
        children: Vec<SidebarNode>,
        depth: i32,
        occurrence: Vec<String>,
        model: Rc<RefCell<SidebarModel>>,
        repository: Rc<dyn DirectoryRepository>,
        on_action: Rc<dyn Fn(SidebarAction)>,
        active_path: Rc<RefCell<Option<Location>>>,
        mount_parent: gtk::Window,
        pending_focus: Rc<RefCell<Option<PendingFocus>>>,
        focus_token: u64,
    ) {
        for child in children {
            Self::append_node_inner(
                parent,
                child,
                depth + 1,
                occurrence.clone(),
                model.clone(),
                repository.clone(),
                on_action.clone(),
                active_path.clone(),
                mount_parent.clone(),
                pending_focus.clone(),
                focus_token,
            );
        }
    }

    fn add_context_menu(
        button: &Button,
        location: &SidebarLocation,
        model: Rc<RefCell<SidebarModel>>,
        action: Rc<dyn Fn(SidebarAction)>,
    ) {
        let context_button = button.clone();
        let location = location.clone();
        let gesture = GestureClick::new();
        gesture.set_button(3);
        let popup = Rc::new(move |x: f64, y: f64| {
            let popover = Popover::new();
            popover.set_parent(&context_button);
            crate::ui::accessibility::restore_popover_focus(&popover, &context_button);
            popover.set_autohide(true);
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            let menu = GtkBox::new(Orientation::Vertical, 2);
            menu.set_margin_top(6);
            menu.set_margin_bottom(6);
            if location.local_path().is_none() {
                let open = Button::with_label("Open");
                let open_action = action.clone();
                let popover_for_open = popover.clone();
                let uri = location.location().clone();
                open.connect_clicked(move |_| {
                    popover_for_open.popdown();
                    open_action(SidebarAction::Open(uri.clone()));
                });
                menu.append(&open);
                for (label, command) in [
                    (
                        "Open in New Tab",
                        SidebarAction::OpenInNewTab(location.location().clone()),
                    ),
                    (
                        "Open in New Window",
                        SidebarAction::OpenInNewWindow(location.location().clone()),
                    ),
                ] {
                    let button = Button::with_label(label);
                    let action = action.clone();
                    let popover = popover.clone();
                    button.connect_clicked(move |_| {
                        popover.popdown();
                        action(command.clone());
                    });
                    menu.append(&button);
                }
                if model.borrow().mount_for_location(&location).is_some() {
                    let unmount = Button::with_label("Unmount");
                    let action = action.clone();
                    let popover_for_unmount = popover.clone();
                    let uri = location.location().clone();
                    unmount.connect_clicked(move |_| {
                        popover_for_unmount.popdown();
                        action(SidebarAction::Unmount(uri.clone()));
                    });
                    menu.append(&unmount);
                }
                if model
                    .borrow()
                    .preferences()
                    .remote_pins()
                    .iter()
                    .any(|pinned| pinned == &location.uri())
                {
                    let unpin = Button::with_label("Unpin from Network & online");
                    let action = action.clone();
                    let popover_for_unpin = popover.clone();
                    let uri = location.location().clone();
                    unpin.connect_clicked(move |_| {
                        popover_for_unpin.popdown();
                        action(SidebarAction::Unpin(uri.clone()));
                    });
                    menu.append(&unpin);
                }
                popover.set_child(Some(&menu));
                popover.popup();
                return;
            }
            let path = location
                .local_path()
                .expect("local location was checked")
                .to_path_buf();
            for (label, sidebar_action) in [
                (
                    "Open",
                    SidebarAction::Open(Location::from_local_path(&path)),
                ),
                (
                    "Open in New Tab",
                    SidebarAction::OpenInNewTab(Location::from_local_path(&path)),
                ),
                (
                    "Open in New Window",
                    SidebarAction::OpenInNewWindow(Location::from_local_path(&path)),
                ),
                (
                    "Open in Terminal",
                    SidebarAction::OpenInTerminal(path.clone()),
                ),
            ] {
                let item = Button::with_label(label);
                let action = action.clone();
                let popover = popover.clone();
                item.connect_clicked(move |_| {
                    popover.popdown();
                    action(sidebar_action.clone());
                });
                menu.append(&item);
            }
            let separator = gtk::Separator::new(Orientation::Horizontal);
            separator.set_margin_top(4);
            separator.set_margin_bottom(4);
            menu.append(&separator);
            if model.borrow().mount_for_location(&location).is_some() {
                let unmount = Button::with_label("Unmount");
                let action = action.clone();
                let popover_for_unmount = popover.clone();
                let uri = location.location().clone();
                unmount.connect_clicked(move |_| {
                    popover_for_unmount.popdown();
                    action(SidebarAction::Unmount(uri.clone()));
                });
                menu.append(&unmount);
            }
            let is_pinned = model.borrow().preferences().is_pinned(&path);
            let pin_item = Button::with_label(if is_pinned {
                "Unpin from Quick access"
            } else {
                "Pin to Quick access"
            });
            let action = action.clone();
            let popover_for_pin = popover.clone();
            let pin_action = if is_pinned {
                SidebarAction::Unpin(Location::from_local_path(&path))
            } else {
                SidebarAction::Pin(Location::from_local_path(&path))
            };
            pin_item.connect_clicked(move |_| {
                popover_for_pin.popdown();
                action(pin_action.clone());
            });
            menu.append(&pin_item);
            popover.set_child(Some(&menu));
            popover.popup();
        });
        let pointer_popup = popup.clone();
        gesture.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            pointer_popup(x, y);
        });
        button.add_controller(gesture);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if crate::ui::accessibility::context_menu_key(key, modifiers) {
                popup(0.0, 0.0);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        button.add_controller(keys);
    }
}

fn sidebar_sections(model: &Rc<RefCell<SidebarModel>>) -> Vec<SidebarSection> {
    model.borrow().sections()
}

fn clear_navigation_selection(
    active_path: &RefCell<Option<Location>>,
    active_section: &RefCell<Option<String>>,
) {
    *active_path.borrow_mut() = None;
    *active_section.borrow_mut() = None;
}

fn operation_summary_changed(previous: &Option<String>, next: &Option<String>) -> bool {
    previous != next
}

fn should_show_devices_heading(section_title: &str, node_index: usize) -> bool {
    section_title == "This PC" && node_index == 1
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gio::prelude::*;
    use gtk::prelude::*;

    use crate::core::{
        contracts::{Capability, CapabilitySnapshot, EntryKind, EntrySnapshot},
        Location, SidebarModel, SidebarPreferences,
    };
    use crate::infrastructure::mock_provider::{MockConfig, MockProvider};

    use super::should_show_devices_heading;

    #[test]
    fn devices_heading_is_nested_after_file_system_in_this_pc() {
        assert!(should_show_devices_heading("This PC", 1));
        assert!(!should_show_devices_heading("This PC", 0));
        assert!(!should_show_devices_heading("user", 1));
    }

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn sidebar_renders_quick_access_this_pc_and_network_sections() {
        gtk::init().unwrap();
        assert!(gtk::is_initialized());
    }

    #[test]
    fn operation_summary_only_requires_a_render_when_it_changes() {
        assert!(!super::operation_summary_changed(&None, &None));
        assert!(super::operation_summary_changed(
            &None,
            &Some("1 file operation in progress".into())
        ));
    }

    #[test]
    fn clearing_navigation_selection_also_clears_the_active_section() {
        let active_path = RefCell::new(Some(
            crate::core::Location::from_uri("file:///tmp/example").unwrap(),
        ));
        let active_section = RefCell::new(Some("Network & online".to_string()));

        super::clear_navigation_selection(&active_path, &active_section);

        assert!(active_path.borrow().is_none());
        assert!(active_section.borrow().is_none());
    }

    #[test]
    fn section_snapshot_releases_the_sidebar_model_before_rendering() {
        let model = Rc::new(RefCell::new(SidebarModel::new(
            SidebarPreferences::in_memory(),
            tempfile::tempdir().unwrap().path().to_path_buf(),
        )));

        let _sections = super::sidebar_sections(&model);

        assert!(model.try_borrow_mut().is_ok());
    }

    #[gtk::test]
    fn remote_expansion_loads_directory_children_from_the_repository() {
        let context = glib::MainContext::default();
        let remote = Location::from_uri("smb://fixture/share").unwrap();
        let child = Location::from_uri("smb://fixture/share/projects").unwrap();
        let provider = Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        provider.set_directory(
            remote.clone(),
            vec![directory_entry(child, "Projects")],
            list_capabilities(),
        );
        let sidebar = remote_sidebar(remote, provider.clone());

        expander_named(&sidebar.widget, "share").set_active(true);
        assert!(!contains_sidebar_label(&sidebar.widget, "Projects"));

        advance_provider(&context, &provider);

        assert!(contains_sidebar_label(&sidebar.widget, "Projects"));
    }

    #[gtk::test]
    fn collapsing_an_expansion_cancels_its_pending_repository_request() {
        let context = glib::MainContext::default();
        let remote = Location::from_uri("smb://fixture/share").unwrap();
        let provider = Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        provider.set_directory(
            remote.clone(),
            vec![directory_entry(
                Location::from_uri("smb://fixture/share/projects").unwrap(),
                "Projects",
            )],
            list_capabilities(),
        );
        let sidebar = remote_sidebar(remote, provider.clone());

        let expander = expander_named(&sidebar.widget, "share");
        expander.set_active(true);
        expander.set_active(false);
        advance_provider(&context, &provider);

        assert!(!contains_sidebar_label(&sidebar.widget, "Projects"));
    }

    #[gtk::test]
    fn stale_success_after_collapse_does_not_repopulate_the_sidebar_tree() {
        let context = glib::MainContext::default();
        let remote = Location::from_uri("smb://fixture/share").unwrap();
        let provider = Rc::new(MockProvider::new(context.clone(), MockConfig::default()));
        provider.set_list_results_after_cancellation(true);
        provider.set_directory(
            remote.clone(),
            vec![directory_entry(
                Location::from_uri("smb://fixture/share/projects").unwrap(),
                "Projects",
            )],
            list_capabilities(),
        );
        let sidebar = remote_sidebar(remote, provider.clone());

        let expander = expander_named(&sidebar.widget, "share");
        expander.set_active(true);
        expander.set_active(false);
        advance_provider(&context, &provider);

        assert!(
            !contains_sidebar_label(&sidebar.widget, "Projects"),
            "a provider may complete after cancellation, but that old result must be ignored"
        );
    }

    fn remote_sidebar(remote: Location, provider: Rc<MockProvider>) -> super::Sidebar {
        let mut preferences = SidebarPreferences::in_memory();
        preferences.pin_location(remote);
        preferences.set_section_expanded("Quick access", true);
        let model = Rc::new(RefCell::new(SidebarModel::new(
            preferences,
            tempfile::tempdir().unwrap().path().to_path_buf(),
        )));
        let window = gtk::Window::new();
        let sidebar = super::Sidebar::new_with_repository(model, provider, &window, |_| {});
        window.set_child(Some(&sidebar.widget));
        window.present();
        sidebar
    }

    fn list_capabilities() -> CapabilitySnapshot {
        CapabilitySnapshot {
            version: 1,
            supported: [Capability::List].into_iter().collect(),
            stable_order: true,
            ..Default::default()
        }
    }

    fn directory_entry(location: Location, display_name: &str) -> EntrySnapshot {
        EntrySnapshot {
            location,
            display_name: display_name.into(),
            kind: EntryKind::Directory,
            size: None,
            modified: None,
            content_type: None,
            capabilities: list_capabilities(),
        }
    }

    fn advance_provider(context: &glib::MainContext, provider: &MockProvider) {
        provider.advance(1);
        while context.pending() {
            context.iteration(false);
        }
    }

    fn expander_named(sidebar: &gtk::ScrolledWindow, name: &str) -> gtk::ToggleButton {
        sidebar_descendants(sidebar.upcast_ref())
            .into_iter()
            .find_map(|widget| {
                widget
                    .downcast::<gtk::ToggleButton>()
                    .ok()
                    .filter(|button| {
                        button.tooltip_text().as_deref()
                            == Some(&format!("Expand or collapse {name}"))
                    })
            })
            .unwrap_or_else(|| panic!("missing expander for {name}"))
    }

    fn contains_sidebar_label(sidebar: &gtk::ScrolledWindow, label: &str) -> bool {
        sidebar_descendants(sidebar.upcast_ref())
            .into_iter()
            .any(|widget| {
                widget
                    .downcast::<gtk::Label>()
                    .ok()
                    .is_some_and(|candidate| candidate.label() == label)
            })
    }

    fn sidebar_descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
        let mut descendants = vec![widget.clone()];
        let mut child = widget.first_child();
        while let Some(current) = child {
            descendants.extend(sidebar_descendants(&current));
            child = current.next_sibling();
        }
        descendants
    }
}
