use gtk::prelude::*;
use std::{cell::RefCell, rc::Rc};
use zorin_explorer::{
    core::Location,
    ui::{header::HeaderControls, tabs::TabStrip},
};

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut result = vec![widget.clone()];
    let mut child = widget.first_child();
    while let Some(next) = child {
        result.extend(descendants(&next));
        child = next.next_sibling();
    }
    result
}

fn sidebar_section_rows(sidebar: &gtk::ScrolledWindow, title: &str) -> gtk::Widget {
    descendants(sidebar.upcast_ref())
        .into_iter()
        .find(|w| {
            w.clone().downcast::<gtk::Button>().ok().is_some_and(|b| {
                b.has_css_class("sidebar-section-heading") && b.label().as_deref() == Some(title)
            })
        })
        .unwrap()
        .parent()
        .unwrap()
        .next_sibling()
        .unwrap()
}

#[gtk::test]
fn slice4_sidebar_duplicate_home_restores_the_user_section_occurrence() {
    use zorin_explorer::{
        core::sidebar_model::{SidebarModel, SidebarPreferences},
        ui::sidebar::Sidebar,
    };
    let directory = tempfile::tempdir().unwrap();
    let mut preferences = SidebarPreferences::in_memory();
    preferences.pin_location(Location::from_local_path(directory.path()));
    let model = Rc::new(RefCell::new(SidebarModel::new(
        preferences,
        directory.path().into(),
    )));
    let user_section = model.borrow().sections()[1].title.clone();
    model
        .borrow_mut()
        .preferences_mut()
        .set_section_expanded(user_section.clone(), true);
    model
        .borrow_mut()
        .preferences_mut()
        .set_section_expanded("Quick access", true);
    let window = gtk::Window::new();
    let sidebar = Sidebar::new(model, &window, |_| {});
    window.set_child(Some(&sidebar.widget));
    window.present();
    let rows = sidebar_section_rows(&sidebar.widget, &user_section);
    let home = descendants(&rows)
        .into_iter()
        .find(|w| {
            w.has_css_class("sidebar-location") && w.tooltip_text().as_deref() == Some("Home")
        })
        .unwrap();
    home.grab_focus();
    sidebar.refresh();
    let focused = gtk::prelude::GtkWindowExt::focus(&window).unwrap();
    let user_rows = sidebar_section_rows(&sidebar.widget, &user_section);
    assert!(
        focused.is_ancestor(&user_rows),
        "Home must stay in the user section, not jump to Quick access"
    );
    assert_eq!(focused.tooltip_text().as_deref(), Some("Home"));
    window.close();
}

#[gtk::test]
fn slice4_sidebar_duplicate_tree_expander_restores_its_ancestor_occurrence() {
    use zorin_explorer::{
        core::sidebar_model::{SidebarModel, SidebarPreferences},
        ui::sidebar::Sidebar,
    };
    let directory = tempfile::tempdir().unwrap();
    let child = directory.path().join("Child");
    std::fs::create_dir_all(child.join("Grandchild")).unwrap();
    let mut preferences = SidebarPreferences::in_memory();
    // Put the direct pin before the same URI nested beneath the Home pin.
    preferences.pin_location(Location::from_local_path(&child));
    preferences.pin_location(Location::from_local_path(directory.path()));
    preferences.set_section_expanded("Quick access", true);
    let model = Rc::new(RefCell::new(SidebarModel::new(
        preferences,
        directory.path().into(),
    )));
    let window = gtk::Window::new();
    let sidebar = Sidebar::new(model, &window, |_| {});
    window.set_child(Some(&sidebar.widget));
    window.present();
    let rows = sidebar_section_rows(&sidebar.widget, "Quick access");
    let home_row = rows.last_child().unwrap();
    let home_expander = home_row
        .first_child()
        .unwrap()
        .first_child()
        .unwrap()
        .downcast::<gtk::ToggleButton>()
        .unwrap();
    home_expander.set_active(true);
    let nested = descendants(&home_row)
        .into_iter()
        .find_map(|w| {
            w.downcast::<gtk::ToggleButton>()
                .ok()
                .filter(|b| b.tooltip_text().as_deref() == Some("Expand or collapse Child"))
        })
        .unwrap();
    nested.set_active(true);
    nested.grab_focus();
    sidebar.refresh();
    let refreshed_rows = sidebar_section_rows(&sidebar.widget, "Quick access");
    let refreshed_home = refreshed_rows.last_child().unwrap();
    let focused = gtk::prelude::GtkWindowExt::focus(&window).unwrap();
    assert!(
        focused.is_ancestor(&refreshed_home),
        "nested expander must not jump to the direct pin for the same URI"
    );
    assert_eq!(
        focused.tooltip_text().as_deref(),
        Some("Expand or collapse Child")
    );
    assert!(focused.downcast::<gtk::ToggleButton>().unwrap().is_active());
    window.close();
}

#[gtk::test]
fn slice4_list_cut_description_is_cleared_on_replacement_and_unbind() {
    use glib::translate::ToGlibPtr;
    use zorin_explorer::{
        core::{GroupField, ViewMode, ViewOptions},
        ui::file_view::FileView,
    };
    let context = glib::MainContext::default();
    let directory = tempfile::tempdir().unwrap();
    let cut = directory.path().join("a-cut.txt");
    std::fs::write(&cut, "cut").unwrap();
    std::fs::write(directory.path().join("b-plain.txt"), "plain").unwrap();
    for mode in [ViewMode::DetailedList, ViewMode::CompactList] {
        let view = FileView::new(Location::from_local_path(directory.path()));
        view.set_options(ViewOptions {
            view_mode: mode,
            group_field: GroupField::Type,
            ..Default::default()
        });
        view.set_pending_cut_locations(vec![Location::from_local_path(&cut)]);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while view.column_view().is_none() {
            context.iteration(false);
            assert!(std::time::Instant::now() < end);
        }
        let column = view.column_view().unwrap();
        let selection = column
            .model()
            .unwrap()
            .downcast::<gtk::MultiSelection>()
            .unwrap();
        let store = selection
            .model()
            .unwrap()
            .downcast::<gio::ListStore>()
            .unwrap();
        assert_eq!(store.n_items(), 3, "one real group heading and two files");
        let heading = store.item(0).unwrap();
        let cut_row = store.item(1).unwrap();
        let plain_row = store.item(2).unwrap();
        store.remove_all();
        let bound = Rc::new(RefCell::new(Vec::<gtk::Widget>::new()));
        let unbound = Rc::new(RefCell::new(Vec::new()));
        for index in 0..column.columns().n_items() {
            let factory = column
                .columns()
                .item(index)
                .unwrap()
                .downcast::<gtk::ColumnViewColumn>()
                .unwrap()
                .factory()
                .unwrap()
                .downcast::<gtk::SignalListItemFactory>()
                .unwrap();
            let bound = bound.clone();
            factory.connect_bind(move |_, item| {
                bound.borrow_mut().push(
                    item.downcast_ref::<gtk::ListItem>()
                        .unwrap()
                        .child()
                        .unwrap(),
                );
            });
            let unbound = unbound.clone();
            factory.connect_unbind(move |_, item| {
                let cell = item
                    .downcast_ref::<gtk::ListItem>()
                    .unwrap()
                    .child()
                    .unwrap();
                unbound.borrow_mut().push(gtk::test_accessible_has_property(
                    &cell,
                    gtk::AccessibleProperty::Description,
                ));
            });
        }
        store.append(&cut_row);
        let window = gtk::Window::new();
        window.set_default_size(900, 300);
        window.set_child(Some(&view.widget));
        window.present();
        while bound.borrow().len() < column.columns().n_items() as usize {
            context.iteration(false);
            assert!(std::time::Instant::now() < end);
        }
        let cells = bound.borrow().clone();
        for cell in &cells {
            assert!(
                gtk::test_accessible_has_property(cell, gtk::AccessibleProperty::Description),
                "{mode:?}: every cut list cell must expose its cut state"
            );
            let expected = std::ffi::CString::new("Cut; ready to move").unwrap();
            // GTK's public test API returns NULL when the actual accessible
            // string matches; this checks the AT property, not the tooltip.
            let mismatch = unsafe {
                gtk::ffi::gtk_test_accessible_check_property(
                    cell.upcast_ref::<gtk::Accessible>().to_glib_none().0,
                    gtk::ffi::GTK_ACCESSIBLE_PROPERTY_DESCRIPTION,
                    expected.as_ptr(),
                )
            };
            let failure: Option<glib::GString> =
                unsafe { glib::translate::from_glib_full(mismatch) };
            assert!(failure.is_none(), "{failure:?}");
        }
        for replacement in [&plain_row, &cut_row, &heading] {
            bound.borrow_mut().clear();
            store.splice(0, 1, &[replacement.clone()]);
            while bound.borrow().len() < cells.len() {
                context.iteration(false);
                assert!(std::time::Instant::now() < end);
            }
            assert!(
                unbound
                    .borrow()
                    .iter()
                    .all(|has_description| !has_description),
                "the real unbind callback must clear the cut description"
            );
            let is_cut = replacement == &cut_row;
            for cell in bound.borrow().iter() {
                assert_eq!(
                    gtk::test_accessible_has_property(cell, gtk::AccessibleProperty::Description),
                    is_cut,
                    "{mode:?}: normal/group cells must not retain a cut description"
                );
                assert_eq!(cell.has_css_class("cut-pending"), is_cut);
            }
        }
        assert!(
            !unbound.borrow().is_empty(),
            "the fixture must observe real GTK unbinds"
        );
        view.close();
        window.close();
    }
}

#[gtk::test]
fn slice4_icon_controls_have_explicit_accessible_names() {
    let header = HeaderControls::new(Rc::new(RefCell::new(Location::home())));
    let tabs = TabStrip::new();
    for widget in descendants(header.widget.upcast_ref())
        .into_iter()
        .chain([tabs.previous_button.upcast(), tabs.next_button.upcast()])
    {
        if let Ok(button) = widget.downcast::<gtk::Button>() {
            if button.icon_name().is_some() {
                assert!(
                    gtk::test_accessible_has_property(&button, gtk::AccessibleProperty::Label),
                    "Missing explicit accessible name: {:?}",
                    button.tooltip_text()
                );
                assert!(button.tooltip_text().is_some());
            }
        }
    }
}

#[gtk::test]
fn slice4_location_escape_restores_breadcrumbs_without_navigation() {
    use zorin_explorer::ui::header::LocationBar;
    let bar = LocationBar::new(&Location::home());
    let window = gtk::Window::new();
    window.set_child(Some(&bar.widget));
    window.present();
    bar.focus_location();
    let entry = descendants(bar.widget.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Entry>().ok())
        .unwrap();
    entry.set_text("unfinished");
    let controllers = entry.observe_controllers();
    let handled = (0..controllers.n_items())
        .filter_map(|i| {
            controllers
                .item(i)
                .and_downcast::<gtk::EventControllerKey>()
        })
        .any(|key| {
            key.emit_by_name::<bool>(
                "key-pressed",
                &[
                    &gtk::gdk::Key::Escape,
                    &0u32,
                    &gtk::gdk::ModifierType::empty(),
                ],
            )
        });
    assert!(handled, "Escape must cancel location editing");
    assert_eq!(
        bar.widget.visible_child_name().as_deref(),
        Some("breadcrumbs")
    );
    assert_eq!(entry.text().as_str(), Location::home().uri());
    window.close();
}

#[gtk::test]
fn slice4_committed_navigation_clears_a_stale_location_validation_error() {
    use zorin_explorer::ui::header::LocationBar;
    let bar = LocationBar::new(&Location::home());
    bar.connect_location_changed(|_| {});
    let entry = descendants(bar.widget.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Entry>().ok())
        .unwrap();
    entry.set_text("relative-input");
    entry.emit_activate();
    assert!(entry.has_css_class("error"));
    assert!(gtk::test_accessible_has_state(
        &entry,
        gtk::AccessibleState::Invalid
    ));
    bar.set_uri(Location::home().uri());
    assert!(!entry.has_css_class("error"));
    assert!(!gtk::test_accessible_has_state(
        &entry,
        gtk::AccessibleState::Invalid
    ));
}

#[gtk::test]
fn slice4_status_priorities_coalesce_repeated_updates() {
    use zorin_explorer::ui::accessibility::{AccessibleStatus, StatusPriority};
    let status = AccessibleStatus::new();
    let labels: Vec<_> = descendants(status.widget.upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .collect();
    let normal = labels
        .iter()
        .find(|l| l.accessible_role() == gtk::AccessibleRole::Status)
        .unwrap();
    let urgent = labels
        .iter()
        .find(|l| l.accessible_role() == gtk::AccessibleRole::Alert)
        .unwrap();
    let updates = Rc::new(std::cell::Cell::new(0));
    let count = updates.clone();
    normal.connect_label_notify(move |_| count.set(count.get() + 1));
    status.announce("Copy started", StatusPriority::Polite);
    status.announce("Copy started", StatusPriority::Polite);
    assert_eq!(updates.get(), 1, "polling must not repeat announcements");
    status.announce("Copy failed", StatusPriority::Urgent);
    assert_eq!(urgent.text(), "Copy failed");
    assert!(
        normal.text().is_empty(),
        "stale status must not mask errors"
    );
}

#[gtk::test]
fn slice4_keyboard_context_menu_keeps_selection_and_returns_focus() {
    use zorin_explorer::{
        core::{GroupField, ViewOptions},
        ui::file_view::FileView,
    };
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("keyboard.txt"), "fixture").unwrap();
    let view = FileView::new(Location::from_local_path(directory.path()));
    view.set_options(ViewOptions {
        view_mode: zorin_explorer::core::ViewMode::DetailedList,
        group_field: GroupField::None,
        ..Default::default()
    });
    let window = gtk::Window::new();
    window.set_child(Some(&view.widget));
    window.present();
    let context = glib::MainContext::default();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while view.column_view().is_none() {
        context.iteration(false);
        assert!(std::time::Instant::now() < end);
    }
    let column = view.column_view().unwrap();
    column.model().unwrap().select_item(0, true);
    column.grab_focus();
    let focused = gtk::prelude::GtkWindowExt::focus(&window).unwrap();
    let controllers = view.widget.observe_controllers();
    let handled = (0..controllers.n_items())
        .filter_map(|i| {
            controllers
                .item(i)
                .and_downcast::<gtk::EventControllerKey>()
        })
        .any(|key| {
            key.emit_by_name::<bool>(
                "key-pressed",
                &[
                    &gtk::gdk::Key::F10,
                    &0u32,
                    &gtk::gdk::ModifierType::SHIFT_MASK,
                ],
            )
        });
    assert!(handled, "Shift+F10 must open the selected file's menu");
    let popup = descendants(view.widget.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Popover>().ok())
        .unwrap();
    assert!(popup.is_visible());
    assert_eq!(view.selected_items()[0].name(), "keyboard.txt");
    popup.popdown();
    while context.pending() {
        context.iteration(false);
    }
    assert_eq!(gtk::prelude::GtkWindowExt::focus(&window), Some(focused));
    view.close();
    window.close();
}

#[gtk::test]
fn slice4_sidebar_expanders_expose_state_and_refresh_keeps_keyboard_focus() {
    use zorin_explorer::{
        core::sidebar_model::{SidebarModel, SidebarPreferences},
        ui::sidebar::Sidebar,
    };
    let directory = tempfile::tempdir().unwrap();
    let window = gtk::Window::new();
    let model = Rc::new(RefCell::new(SidebarModel::new(
        SidebarPreferences::in_memory(),
        directory.path().into(),
    )));
    let sidebar = Sidebar::new(model, &window, |_| {});
    window.set_child(Some(&sidebar.widget));
    window.present();
    let toggle = descendants(sidebar.widget.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::ToggleButton>().ok())
        .unwrap();
    assert!(gtk::test_accessible_has_property(
        &toggle,
        gtk::AccessibleProperty::Label
    ));
    assert!(gtk::test_accessible_has_state(
        &toggle,
        gtk::AccessibleState::Expanded
    ));
    toggle.grab_focus();
    let name = toggle.tooltip_text();
    sidebar.refresh();
    let focused = gtk::prelude::GtkWindowExt::focus(&window).expect("refresh must retain focus");
    assert_eq!(focused.tooltip_text(), name);
    assert!(focused.is_ancestor(&sidebar.widget));
    window.close();
}

#[gtk::test]
fn slice4_operation_terminal_status_announces_once_and_cancel_is_named() {
    use zorin_explorer::{
        core::{contracts::MutationKind, OperationManager},
        ui::{accessibility::AccessibleStatus, operations_page},
    };
    let directory = tempfile::tempdir().unwrap();
    let manager = OperationManager::default();
    let status = AccessibleStatus::new();
    let window = gtk::Window::new();
    window.set_child(Some(&status.widget));
    window.present();
    status.watch_operations(manager.clone());
    manager.submit_locations(
        MutationKind::Create,
        vec![Location::from_local_path(&directory.path().join("created"))],
        None,
        None,
        None,
        false,
    );
    let normal = descendants(status.widget.upcast_ref())
        .into_iter()
        .find_map(|w| {
            w.downcast::<gtk::Label>()
                .ok()
                .filter(|l| l.accessible_role() == gtk::AccessibleRole::Status)
        })
        .unwrap();
    let context = glib::MainContext::default();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !normal.text().contains("completed") {
        context.iteration(false);
        assert!(std::time::Instant::now() < end);
    }
    assert!(directory.path().join("created").is_dir());
    let notifications = Rc::new(std::cell::Cell::new(0));
    let count = notifications.clone();
    normal.connect_label_notify(move |_| count.set(count.get() + 1));
    let end = std::time::Instant::now() + std::time::Duration::from_millis(450);
    while std::time::Instant::now() < end {
        context.iteration(false);
    }
    assert_eq!(
        notifications.get(),
        0,
        "terminal history must not be re-announced on polls"
    );
    let center = operations_page::build(manager);
    let cancel = descendants(center.upcast_ref())
        .into_iter()
        .find_map(|w| {
            w.downcast::<gtk::Button>()
                .ok()
                .filter(|b| b.label().as_deref() == Some("Cancel"))
        })
        .unwrap();
    assert!(gtk::test_accessible_has_property(
        &cancel,
        gtk::AccessibleProperty::Label
    ));
    assert_eq!(cancel.tooltip_text().as_deref(), Some("Cancel operation"));
    window.close();
}

#[gtk::test]
fn slice4_opening_a_folder_keeps_keyboard_focus_in_the_new_listing() {
    use zorin_explorer::{
        core::{GroupField, ViewMode, ViewOptions},
        ui::file_view::FileView,
    };
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("inside.txt"), "fixture").unwrap();
    let view = FileView::new(Location::from_local_path(directory.path()));
    view.set_options(ViewOptions {
        view_mode: ViewMode::DetailedList,
        group_field: GroupField::None,
        ..Default::default()
    });
    let window = gtk::Window::new();
    window.set_child(Some(&view.widget));
    window.present();
    let context = glib::MainContext::default();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while view.column_view().is_none() {
        context.iteration(false);
        assert!(std::time::Instant::now() < end);
    }
    let column = view.column_view().unwrap();
    column.model().unwrap().select_item(0, true);
    column.grab_focus();
    column.emit_by_name::<()>("activate", &[&0u32]);
    while view.current_location() != Location::from_local_path(&folder) {
        context.iteration(false);
        assert!(std::time::Instant::now() < end);
    }
    let focused = gtk::prelude::GtkWindowExt::focus(&window).unwrap();
    assert!(
        focused.is_ancestor(&view.widget),
        "navigation must not retain a detached row's focus"
    );
    view.close();
    window.close();
}
