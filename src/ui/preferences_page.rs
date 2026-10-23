use crate::core::application_preferences::default_new_tab_location;
use crate::core::{
    ApplicationPreferences, ConflictDefault, GridSize, GroupField, OperationManager, SearchHistory,
    SidebarModel, SortDirection, SortField, ViewMode, ViewOptions, ViewPreferencesStore,
};
use crate::ui::sidebar::Sidebar;
use adw::prelude::*;
use gtk::{
    Adjustment, Box as GtkBox, Button, CheckButton, Image, Label, Orientation, ScrolledWindow,
    SpinButton, Stack, StackSwitcher,
};
use std::cell::RefCell;
use std::rc::Rc;

pub fn category_names() -> [&'static str; 6] {
    [
        "General",
        "Views",
        "File operations",
        "Search & privacy",
        "Sidebar",
        "About",
    ]
}
fn conflict_default_labels() -> [&'static str; 4] {
    ["Ask", "Replace", "Keep Both", "Skip"]
}
fn view_default_groups() -> [&'static str; 6] {
    [
        "Layout",
        "Icon size",
        "Sort",
        "Sort direction",
        "Grouping",
        "Content",
    ]
}

pub fn build(
    preferences: Rc<RefCell<ApplicationPreferences>>,
    operations: OperationManager,
    views: Rc<RefCell<ViewPreferencesStore>>,
    search_history: Rc<RefCell<SearchHistory>>,
    sidebar_model: Rc<RefCell<SidebarModel>>,
    sidebar_holder: Rc<RefCell<Option<Sidebar>>>,
) -> ScrolledWindow {
    let content = GtkBox::new(Orientation::Vertical, 14);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(32);
    content.set_margin_end(32);
    let title = Label::new(Some("Preferences"));
    title.add_css_class("title-1");
    title.set_xalign(0.0);
    content.append(&title);
    let subtitle = Label::new(Some(
        "Settings are grouped here as their behaviour becomes available.",
    ));
    subtitle.add_css_class("dim-label");
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);
    content.append(&subtitle);
    let stack = Stack::new();
    stack.set_vexpand(true);
    stack.set_hexpand(true);
    let switcher = StackSwitcher::new();
    switcher.set_stack(Some(&stack));
    switcher.set_halign(gtk::Align::Start);
    content.append(&switcher);
    for category in category_names() {
        let page = match category {
            "File operations" => operations_page(preferences.clone(), operations.clone()),
            "General" => general_page(
                preferences.clone(),
                sidebar_model.clone(),
                operations.clone(),
            ),
            "Search & privacy" => search_privacy_page(preferences.clone(), search_history.clone()),
            "Sidebar" => sidebar_page(
                preferences.clone(),
                sidebar_model.clone(),
                sidebar_holder.clone(),
            ),
            "About" => about_page(),
            _ => views_page(views.clone()),
        };
        stack.add_titled(
            &page,
            Some(&category.to_lowercase().replace(' ', "-")),
            category,
        );
    }
    content.append(&stack);
    let scroll = ScrolledWindow::new();
    scroll.set_child(Some(&content));
    scroll
}

fn about_details() -> (&'static str, &'static str, &'static str) {
    (
        "Zorin Explorer",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_LICENSE"),
    )
}

fn about_page() -> GtkBox {
    let page = GtkBox::new(Orientation::Vertical, 12);
    page.set_margin_top(18);
    let details = about_details();

    let identity = GtkBox::new(Orientation::Horizontal, 14);
    let icon = Image::from_icon_name("com.zorin.Explorer");
    icon.set_pixel_size(64);
    icon.set_valign(gtk::Align::Center);
    icon.set_halign(gtk::Align::Start);
    identity.append(&icon);
    let text = GtkBox::new(Orientation::Vertical, 4);
    text.set_valign(gtk::Align::Center);
    let name = Label::new(Some(details.0));
    name.add_css_class("title-2");
    name.set_xalign(0.0);
    text.append(&name);
    let version = Label::new(Some(&format!("Version {}", details.1)));
    version.add_css_class("dim-label");
    version.set_xalign(0.0);
    text.append(&version);
    identity.append(&text);
    page.append(&identity);

    let description = Label::new(Some(
        "A native Explorer-style file manager for Linux, designed for Zorin OS and GNOME.",
    ));
    description.set_wrap(true);
    description.set_xalign(0.0);
    description.set_margin_top(8);
    page.append(&description);

    let details_heading = Label::new(Some("Product information"));
    details_heading.add_css_class("heading");
    details_heading.set_xalign(0.0);
    details_heading.set_margin_top(12);
    page.append(&details_heading);
    for text in [
        format!("Application ID: com.zorin.Explorer"),
        format!("License: {}", details.2),
        "Built with GTK 4 and libadwaita".into(),
    ] {
        let row = Label::new(Some(&text));
        row.set_xalign(0.0);
        row.set_selectable(true);
        page.append(&row);
    }
    page
}

fn general_page(
    preferences: Rc<RefCell<ApplicationPreferences>>,
    sidebar_model: Rc<RefCell<SidebarModel>>,
    operations: OperationManager,
) -> GtkBox {
    let page = GtkBox::new(Orientation::Vertical, 10);
    page.set_margin_top(18);
    let heading = Label::new(Some("General"));
    heading.add_css_class("title-3");
    heading.set_xalign(0.0);
    page.append(&heading);
    let close = CheckButton::with_label("Close Zorin Explorer when the last tab is closed");
    close.set_active(preferences.borrow().close_on_last_tab);
    let stored = preferences.clone();
    close.connect_toggled(move |button| {
        stored.borrow_mut().close_on_last_tab = button.is_active();
        let _ = stored.borrow().save();
    });
    page.append(&close);
    let restore = CheckButton::with_label("Restore previous tabs on launch");
    restore.set_active(preferences.borrow().restore_previous_tabs);
    let stored = preferences.clone();
    restore.connect_toggled(move |button| {
        stored.borrow_mut().restore_previous_tabs = button.is_active();
        let _ = stored.borrow().save();
        if !button.is_active() {
            let _ = stored.borrow().clear_session();
        }
    });
    page.append(&restore);

    let new_tab_heading = Label::new(Some("New tabs open to"));
    new_tab_heading.add_css_class("heading");
    new_tab_heading.set_xalign(0.0);
    new_tab_heading.set_margin_top(8);
    page.append(&new_tab_heading);
    let new_tab_row = GtkBox::new(Orientation::Horizontal, 8);
    let new_tab_label = Label::new(Some(&new_tab_location_display(
        &preferences.borrow().new_tab_location,
    )));
    new_tab_label.set_hexpand(true);
    new_tab_label.set_xalign(0.0);
    new_tab_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    new_tab_row.append(&new_tab_label);
    let browse = Button::with_label("Browse…");
    let browse_preferences = preferences.clone();
    let browse_label = new_tab_label.clone();
    let browse_sidebar_model = sidebar_model.clone();
    browse.connect_clicked(move |button| {
        choose_new_tab_location(
            button,
            browse_preferences.clone(),
            browse_label.clone(),
            browse_sidebar_model.clone(),
            operations.clone(),
        )
    });
    new_tab_row.append(&browse);
    let reset = Button::with_label("Reset to Home");
    let reset_preferences = preferences.clone();
    let reset_label = new_tab_label.clone();
    reset.connect_clicked(move |_| {
        let home = default_new_tab_location();
        reset_preferences.borrow_mut().new_tab_location = home.clone();
        let _ = reset_preferences.borrow().save();
        reset_label.set_text(&new_tab_location_display(&home));
    });
    new_tab_row.append(&reset);
    page.append(&new_tab_row);
    page
}

fn new_tab_location_display(uri: &str) -> String {
    gio::File::for_uri(uri)
        .path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| uri.to_owned())
}

fn choose_new_tab_location(
    button: &Button,
    preferences: Rc<RefCell<ApplicationPreferences>>,
    label: Label,
    sidebar_model: Rc<RefCell<SidebarModel>>,
    operations: OperationManager,
) {
    let Some(parent) = button.root().and_downcast::<gtk::Window>() else {
        return;
    };
    let initial = crate::core::Location::from_uri(&preferences.borrow().new_tab_location)
        .unwrap_or_else(|_| crate::core::Location::home());
    crate::ui::folder_picker::pick_folder(
        &parent,
        "New Tabs Open To",
        "Select",
        initial,
        sidebar_model,
        operations,
        move |path| {
            let uri = path.uri().to_owned();
            preferences.borrow_mut().new_tab_location = uri.clone();
            let _ = preferences.borrow().save();
            label.set_text(&new_tab_location_display(&uri));
        },
    );
}

fn search_privacy_page(
    preferences: Rc<RefCell<ApplicationPreferences>>,
    history: Rc<RefCell<SearchHistory>>,
) -> GtkBox {
    let page = GtkBox::new(Orientation::Vertical, 10);
    page.set_margin_top(18);
    let heading = Label::new(Some("Search & privacy"));
    heading.add_css_class("title-3");
    heading.set_xalign(0.0);
    page.append(&heading);
    let content_search =
        CheckButton::with_label("Search file contents when the desktop search index is available");
    content_search.set_active(preferences.borrow().content_search_enabled);
    let stored = preferences.clone();
    content_search.connect_toggled(move |button| {
        stored.borrow_mut().content_search_enabled = button.is_active();
        let _ = stored.borrow().save();
    });
    page.append(&content_search);
    let index_note = Label::new(Some("Content search requires a compatible desktop-index provider. If unavailable, search explains why; files are never scanned to substitute for an index."));
    index_note.set_wrap(true);
    index_note.set_xalign(0.0);
    page.append(&index_note);
    let record_history = CheckButton::with_label("Remember recent searches");
    record_history.set_active(preferences.borrow().record_search_history);
    let stored = preferences.clone();
    let record_history_effect = history.clone();
    record_history.connect_toggled(move |button| {
        stored.borrow_mut().record_search_history = button.is_active();
        let _ = stored.borrow().save();
        if !button.is_active() {
            record_history_effect.borrow_mut().clear();
            let _ = record_history_effect.borrow().save();
        }
    });
    page.append(&record_history);
    let retention_row = GtkBox::new(Orientation::Horizontal, 8);
    let label = Label::new(Some("Keep recent searches for (days)"));
    label.set_hexpand(true);
    label.set_xalign(0.0);
    retention_row.append(&label);
    let spin = SpinButton::new(
        Some(&Adjustment::new(
            preferences.borrow().search_history_retention_days as f64,
            1.0,
            365.0,
            1.0,
            7.0,
            0.0,
        )),
        1.0,
        0,
    );
    let stored = preferences.clone();
    let retention_history = history.clone();
    spin.connect_value_changed(move |spin| {
        let days = spin.value() as u32;
        stored.borrow_mut().search_history_retention_days = days;
        let _ = stored.borrow().save();
        retention_history.borrow_mut().prune(days);
        let _ = retention_history.borrow().save();
    });
    retention_row.append(&spin);
    page.append(&retention_row);
    let clear = Button::with_label("Clear search history");
    clear.set_halign(gtk::Align::Start);
    let clear_history = history.clone();
    clear.connect_clicked(move |_| {
        clear_history.borrow_mut().clear();
        let _ = clear_history.borrow().save();
    });
    page.append(&clear);
    page
}

fn sidebar_page(
    _preferences: Rc<RefCell<ApplicationPreferences>>,
    sidebar_model: Rc<RefCell<SidebarModel>>,
    sidebar_holder: Rc<RefCell<Option<Sidebar>>>,
) -> GtkBox {
    let page = GtkBox::new(Orientation::Vertical, 10);
    page.set_margin_top(18);
    let heading = Label::new(Some("Sidebar"));
    heading.add_css_class("title-3");
    heading.set_xalign(0.0);
    page.append(&heading);
    let description = Label::new(Some("Choose which sidebar section opens expanded by default for sections you haven't toggled yourself."));
    description.add_css_class("dim-label");
    description.set_wrap(true);
    description.set_xalign(0.0);
    page.append(&description);
    let section_titles = sidebar_model
        .borrow()
        .sections()
        .into_iter()
        .map(|section| section.title)
        .collect::<Vec<_>>();
    let current_default = sidebar_model
        .borrow()
        .preferences()
        .default_expanded_section()
        .to_owned();
    let mut first: Option<CheckButton> = None;
    for title in &section_titles {
        let control = CheckButton::with_label(title);
        control.set_active(*title == current_default);
        if let Some(first) = &first {
            control.set_group(Some(first));
        } else {
            first = Some(control.clone());
        }
        let model = sidebar_model.clone();
        let holder = sidebar_holder.clone();
        let title = title.clone();
        control.connect_toggled(move |control| {
            if !control.is_active() {
                return;
            }
            let mut model = model.borrow_mut();
            model
                .preferences_mut()
                .set_default_expanded_section(title.clone());
            let _ = model.preferences().save();
            drop(model);
            if let Some(sidebar) = holder.borrow().as_ref() {
                sidebar.refresh();
            }
        });
        page.append(&control);
    }
    let reset = Button::with_label("Reset sidebar expansion");
    reset.set_halign(gtk::Align::Start);
    reset.set_margin_top(12);
    let model = sidebar_model.clone();
    let holder = sidebar_holder.clone();
    reset.connect_clicked(move |_| {
        let mut model = model.borrow_mut();
        model.preferences_mut().clear_section_expansion();
        let _ = model.preferences().save();
        drop(model);
        if let Some(sidebar) = holder.borrow().as_ref() {
            sidebar.refresh();
        }
    });
    page.append(&reset);
    page
}

fn operations_page(
    preferences: Rc<RefCell<ApplicationPreferences>>,
    operations: OperationManager,
) -> GtkBox {
    let page = GtkBox::new(Orientation::Vertical, 10);
    page.set_margin_top(18);
    let heading = Label::new(Some("File operations"));
    heading.add_css_class("title-3");
    heading.set_xalign(0.0);
    page.append(&heading);
    let row = GtkBox::new(Orientation::Horizontal, 8);
    let label = Label::new(Some("Keep completed history for (days)"));
    label.set_hexpand(true);
    label.set_xalign(0.0);
    row.append(&label);
    let spin = SpinButton::new(
        Some(&Adjustment::new(
            preferences.borrow().operation_history_days as f64,
            1.0,
            365.0,
            1.0,
            7.0,
            0.0,
        )),
        1.0,
        0,
    );
    let stored = preferences.clone();
    let retention_manager = operations.clone();
    spin.connect_value_changed(move |spin| {
        let days = spin.value() as u32;
        stored.borrow_mut().operation_history_days = days;
        let _ = stored.borrow().save();
        retention_manager.set_retention_days(days);
    });
    row.append(&spin);
    page.append(&row);
    let clear = Button::with_label("Clear completed history");
    clear.set_halign(gtk::Align::Start);
    clear.connect_clicked(move |_| operations.clear_completed());
    page.append(&clear);
    let conflict_heading = Label::new(Some("When an item already exists"));
    conflict_heading.add_css_class("heading");
    conflict_heading.set_xalign(0.0);
    conflict_heading.set_margin_top(12);
    page.append(&conflict_heading);
    let conflict_description = Label::new(Some(
        "Choose the default action for copy and move conflicts.",
    ));
    conflict_description.add_css_class("dim-label");
    conflict_description.set_xalign(0.0);
    page.append(&conflict_description);
    let mut first: Option<CheckButton> = None;
    for (label, value) in conflict_default_labels().into_iter().zip([
        ConflictDefault::Ask,
        ConflictDefault::Replace,
        ConflictDefault::KeepBoth,
        ConflictDefault::Skip,
    ]) {
        let control = CheckButton::with_label(label);
        control.set_active(preferences.borrow().conflict_default == value);
        if let Some(first) = &first {
            control.set_group(Some(first));
        } else {
            first = Some(control.clone());
        }
        let stored = preferences.clone();
        control.connect_toggled(move |control| {
            if control.is_active() {
                stored.borrow_mut().conflict_default = value;
                let _ = stored.borrow().save();
            }
        });
        page.append(&control);
    }
    page
}

fn views_page(views: Rc<RefCell<ViewPreferencesStore>>) -> GtkBox {
    let page = GtkBox::new(Orientation::Vertical, 10);
    page.set_margin_top(18);
    let heading = Label::new(Some("Views"));
    heading.add_css_class("title-3");
    heading.set_xalign(0.0);
    page.append(&heading);
    let description = Label::new(Some("These defaults apply to folders without their own saved view. Use Reset folder view from the View menu to return a folder to these defaults."));
    description.add_css_class("dim-label");
    description.set_wrap(true);
    description.set_xalign(0.0);
    page.append(&description);
    let options = views.borrow().defaults();
    let groups = view_default_groups();
    append_view_radio(
        &page,
        groups[0],
        [
            ("Detailed List", ViewMode::DetailedList),
            ("Compact List", ViewMode::CompactList),
            ("Icon Grid", ViewMode::IconGrid),
            ("Hybrid", ViewMode::Hybrid),
        ],
        options.view_mode,
        views.clone(),
        |options, value| options.view_mode = value,
    );
    append_view_radio(
        &page,
        groups[1],
        [
            ("Small", GridSize::Small),
            ("Medium", GridSize::Medium),
            ("Large", GridSize::Large),
            ("Extra Large", GridSize::ExtraLarge),
        ],
        options.grid_size,
        views.clone(),
        |options, value| options.grid_size = value,
    );
    append_view_radio(
        &page,
        groups[2],
        [
            ("Name", SortField::Name),
            ("Date modified", SortField::DateModified),
            ("Type", SortField::Type),
            ("Size", SortField::Size),
        ],
        options.sort_field,
        views.clone(),
        |options, value| options.sort_field = value,
    );
    append_view_radio(
        &page,
        groups[3],
        [
            ("Ascending", SortDirection::Ascending),
            ("Descending", SortDirection::Descending),
        ],
        options.sort_direction,
        views.clone(),
        |options, value| options.sort_direction = value,
    );
    append_view_radio(
        &page,
        groups[4],
        [
            ("None", GroupField::None),
            ("Type", GroupField::Type),
            ("Size", GroupField::Size),
            ("Date modified", GroupField::DateModified),
        ],
        options.group_field,
        views.clone(),
        |options, value| options.group_field = value,
    );
    let content_heading = Label::new(Some(groups[5]));
    content_heading.add_css_class("heading");
    content_heading.set_xalign(0.0);
    content_heading.set_margin_top(8);
    page.append(&content_heading);
    append_view_toggle(
        &page,
        "Folders first",
        options.folders_first,
        views.clone(),
        |options, active| options.folders_first = active,
    );
    append_view_toggle(
        &page,
        "Show thumbnails",
        options.thumbnails,
        views.clone(),
        |options, active| options.thumbnails = active,
    );
    append_view_toggle(
        &page,
        "Show hidden files",
        options.show_hidden,
        views.clone(),
        |options, active| options.show_hidden = active,
    );
    let reset = Button::with_label("Reset all folder views");
    reset.add_css_class("destructive-action");
    reset.set_halign(gtk::Align::Start);
    reset.set_margin_top(16);
    let views = views.clone();
    reset.connect_clicked(move |button| confirm_clear_all_folder_views(button, views.clone()));
    page.append(&reset);
    page
}

fn append_view_radio<T: Copy + PartialEq + 'static, const N: usize>(
    page: &GtkBox,
    heading_text: &str,
    entries: [(&str, T); N],
    selected: T,
    views: Rc<RefCell<ViewPreferencesStore>>,
    update: impl Fn(&mut ViewOptions, T) + Copy + 'static,
) {
    let heading = Label::new(Some(heading_text));
    heading.add_css_class("heading");
    heading.set_xalign(0.0);
    heading.set_margin_top(8);
    page.append(&heading);
    let mut first: Option<CheckButton> = None;
    for (label, value) in entries {
        let control = CheckButton::with_label(label);
        control.set_active(value == selected);
        if let Some(first) = &first {
            control.set_group(Some(first));
        } else {
            first = Some(control.clone());
        }
        let views = views.clone();
        control.connect_toggled(move |control| {
            if control.is_active() {
                update_view_defaults(&views, |options| update(options, value));
            }
        });
        page.append(&control);
    }
}

fn append_view_toggle(
    page: &GtkBox,
    label: &str,
    active: bool,
    views: Rc<RefCell<ViewPreferencesStore>>,
    update: impl Fn(&mut ViewOptions, bool) + 'static,
) {
    let control = CheckButton::with_label(label);
    control.set_active(active);
    control.connect_toggled(move |control| {
        update_view_defaults(&views, |options| update(options, control.is_active()))
    });
    page.append(&control);
}

fn update_view_defaults(
    views: &Rc<RefCell<ViewPreferencesStore>>,
    update: impl FnOnce(&mut ViewOptions),
) {
    let mut views = views.borrow_mut();
    let mut options = views.defaults();
    update(&mut options);
    views.set_defaults(options);
    let _ = views.save();
}

fn confirm_clear_all_folder_views(button: &Button, views: Rc<RefCell<ViewPreferencesStore>>) {
    let parent = button.root().and_downcast::<gtk::Window>();
    let dialog = adw::MessageDialog::new(parent.as_ref(), Some("Reset all folder views?"), Some("This removes every folder-specific view setting. Your global view defaults will be kept."));
    dialog.add_responses(&[("reset", "Reset All"), ("cancel", "Cancel")]);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
    dialog.connect_response(None, move |_, response| {
        if response == "reset" {
            let mut views = views.borrow_mut();
            views.clear_all_folders();
            let _ = views.save();
        }
    });
    dialog.present();
}

#[cfg(test)]
mod tests {
    #[test]
    fn categories_cover_current_and_planned_preference_areas() {
        assert_eq!(
            super::category_names(),
            [
                "General",
                "Views",
                "File operations",
                "Search & privacy",
                "Sidebar",
                "About"
            ]
        );
    }

    #[test]
    fn about_details_use_the_package_metadata() {
        assert_eq!(
            super::about_details(),
            (
                "Zorin Explorer",
                env!("CARGO_PKG_VERSION"),
                env!("CARGO_PKG_LICENSE")
            )
        );
    }

    #[test]
    fn transfer_conflict_controls_expose_every_persisted_action() {
        assert_eq!(
            super::conflict_default_labels(),
            ["Ask", "Replace", "Keep Both", "Skip"]
        );
    }

    #[test]
    fn view_preferences_expose_all_default_groups() {
        assert_eq!(
            super::view_default_groups(),
            [
                "Layout",
                "Icon size",
                "Sort",
                "Sort direction",
                "Grouping",
                "Content"
            ]
        );
    }

    #[test]
    fn new_tab_location_displays_as_a_plain_filesystem_path() {
        assert_eq!(
            super::new_tab_location_display("file:///home/user/Documents"),
            "/home/user/Documents"
        );
    }

    #[test]
    fn a_non_file_new_tab_location_falls_back_to_the_raw_uri() {
        assert_eq!(
            super::new_tab_location_display("network:///"),
            "network:///"
        );
    }
}
