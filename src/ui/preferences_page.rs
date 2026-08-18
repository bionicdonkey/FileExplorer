use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use gtk::{Adjustment, Box as GtkBox, Button, CheckButton, Label, Orientation, ScrolledWindow, SpinButton, Stack, StackSwitcher};
use crate::core::{ApplicationPreferences, ConflictDefault, GridSize, GroupField, OperationManager, SortDirection, SortField, ViewMode, ViewOptions, ViewPreferencesStore};

pub fn category_names() -> [&'static str; 3] { ["General", "Views", "File operations"] }
fn conflict_default_labels() -> [&'static str; 4] { ["Ask", "Replace", "Keep Both", "Skip"] }
fn view_default_groups() -> [&'static str; 6] { ["Layout", "Icon size", "Sort", "Sort direction", "Grouping", "Content"] }

pub fn build(preferences: Rc<RefCell<ApplicationPreferences>>, operations: OperationManager, views: Rc<RefCell<ViewPreferencesStore>>) -> ScrolledWindow {
    let content = GtkBox::new(Orientation::Vertical, 14);
    content.set_margin_top(24); content.set_margin_bottom(24); content.set_margin_start(32); content.set_margin_end(32);
    let title = Label::new(Some("Preferences")); title.add_css_class("title-1"); title.set_xalign(0.0); content.append(&title);
    let subtitle = Label::new(Some("Settings are grouped here as their behaviour becomes available.")); subtitle.add_css_class("dim-label"); subtitle.set_xalign(0.0); subtitle.set_wrap(true); content.append(&subtitle);
    let stack = Stack::new(); stack.set_vexpand(true); stack.set_hexpand(true);
    let switcher = StackSwitcher::new(); switcher.set_stack(Some(&stack)); switcher.set_halign(gtk::Align::Start); content.append(&switcher);
    for category in category_names() { let page = if category == "File operations" { operations_page(preferences.clone(), operations.clone()) } else if category == "General" { general_page(preferences.clone()) } else { views_page(views.clone()) }; stack.add_titled(&page, Some(&category.to_lowercase().replace(' ', "-")), category); }
    content.append(&stack);
    let scroll = ScrolledWindow::new(); scroll.set_child(Some(&content)); scroll
}

fn general_page(preferences: Rc<RefCell<ApplicationPreferences>>) -> GtkBox { let page = GtkBox::new(Orientation::Vertical, 10); page.set_margin_top(18); let heading = Label::new(Some("General")); heading.add_css_class("title-3"); heading.set_xalign(0.0); page.append(&heading); let close = CheckButton::with_label("Close Zorin Explorer when the last tab is closed"); close.set_active(preferences.borrow().close_on_last_tab); close.connect_toggled(move |button| { preferences.borrow_mut().close_on_last_tab = button.is_active(); let _ = preferences.borrow().save(); }); page.append(&close); page }

fn operations_page(preferences: Rc<RefCell<ApplicationPreferences>>, operations: OperationManager) -> GtkBox {
    let page = GtkBox::new(Orientation::Vertical, 10); page.set_margin_top(18);
    let heading = Label::new(Some("File operations")); heading.add_css_class("title-3"); heading.set_xalign(0.0); page.append(&heading);
    let row = GtkBox::new(Orientation::Horizontal, 8); let label = Label::new(Some("Keep completed history for (days)")); label.set_hexpand(true); label.set_xalign(0.0); row.append(&label);
    let spin = SpinButton::new(Some(&Adjustment::new(preferences.borrow().operation_history_days as f64, 1.0, 365.0, 1.0, 7.0, 0.0)), 1.0, 0); let stored = preferences.clone(); let retention_manager = operations.clone(); spin.connect_value_changed(move |spin| { let days = spin.value() as u32; stored.borrow_mut().operation_history_days = days; let _ = stored.borrow().save(); retention_manager.set_retention_days(days); }); row.append(&spin); page.append(&row);
    let clear = Button::with_label("Clear completed history"); clear.set_halign(gtk::Align::Start); clear.connect_clicked(move |_| operations.clear_completed()); page.append(&clear);
    let conflict_heading = Label::new(Some("When an item already exists")); conflict_heading.add_css_class("heading"); conflict_heading.set_xalign(0.0); conflict_heading.set_margin_top(12); page.append(&conflict_heading);
    let conflict_description = Label::new(Some("Choose the default action for copy and move conflicts.")); conflict_description.add_css_class("dim-label"); conflict_description.set_xalign(0.0); page.append(&conflict_description);
    let mut first: Option<CheckButton> = None;
    for (label, value) in conflict_default_labels().into_iter().zip([ConflictDefault::Ask, ConflictDefault::Replace, ConflictDefault::KeepBoth, ConflictDefault::Skip]) {
        let control = CheckButton::with_label(label);
        control.set_active(preferences.borrow().conflict_default == value);
        if let Some(first) = &first { control.set_group(Some(first)); } else { first = Some(control.clone()); }
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
    let heading = Label::new(Some("Views")); heading.add_css_class("title-3"); heading.set_xalign(0.0); page.append(&heading);
    let description = Label::new(Some("These defaults apply to folders without their own saved view. Use Reset folder view from the View menu to return a folder to these defaults.")); description.add_css_class("dim-label"); description.set_wrap(true); description.set_xalign(0.0); page.append(&description);
    let options = views.borrow().defaults();
    let groups = view_default_groups();
    append_view_radio(&page, groups[0], [("Detailed List", ViewMode::DetailedList), ("Compact List", ViewMode::CompactList), ("Icon Grid", ViewMode::IconGrid), ("Hybrid", ViewMode::Hybrid)], options.view_mode, views.clone(), |options, value| options.view_mode = value);
    append_view_radio(&page, groups[1], [("Small", GridSize::Small), ("Medium", GridSize::Medium), ("Large", GridSize::Large), ("Extra Large", GridSize::ExtraLarge)], options.grid_size, views.clone(), |options, value| options.grid_size = value);
    append_view_radio(&page, groups[2], [("Name", SortField::Name), ("Date modified", SortField::DateModified), ("Type", SortField::Type), ("Size", SortField::Size)], options.sort_field, views.clone(), |options, value| options.sort_field = value);
    append_view_radio(&page, groups[3], [("Ascending", SortDirection::Ascending), ("Descending", SortDirection::Descending)], options.sort_direction, views.clone(), |options, value| options.sort_direction = value);
    append_view_radio(&page, groups[4], [("None", GroupField::None), ("Type", GroupField::Type), ("Size", GroupField::Size), ("Date modified", GroupField::DateModified)], options.group_field, views.clone(), |options, value| options.group_field = value);
    let content_heading = Label::new(Some(groups[5])); content_heading.add_css_class("heading"); content_heading.set_xalign(0.0); content_heading.set_margin_top(8); page.append(&content_heading);
    append_view_toggle(&page, "Folders first", options.folders_first, views.clone(), |options, active| options.folders_first = active);
    append_view_toggle(&page, "Show thumbnails", options.thumbnails, views.clone(), |options, active| options.thumbnails = active);
    append_view_toggle(&page, "Show hidden files", options.show_hidden, views, |options, active| options.show_hidden = active);
    page
}

fn append_view_radio<T: Copy + PartialEq + 'static, const N: usize>(page: &GtkBox, heading_text: &str, entries: [(&str, T); N], selected: T, views: Rc<RefCell<ViewPreferencesStore>>, update: impl Fn(&mut ViewOptions, T) + Copy + 'static) {
    let heading = Label::new(Some(heading_text)); heading.add_css_class("heading"); heading.set_xalign(0.0); heading.set_margin_top(8); page.append(&heading);
    let mut first: Option<CheckButton> = None;
    for (label, value) in entries {
        let control = CheckButton::with_label(label); control.set_active(value == selected);
        if let Some(first) = &first { control.set_group(Some(first)); } else { first = Some(control.clone()); }
        let views = views.clone();
        control.connect_toggled(move |control| { if control.is_active() { update_view_defaults(&views, |options| update(options, value)); } });
        page.append(&control);
    }
}

fn append_view_toggle(page: &GtkBox, label: &str, active: bool, views: Rc<RefCell<ViewPreferencesStore>>, update: impl Fn(&mut ViewOptions, bool) + 'static) {
    let control = CheckButton::with_label(label); control.set_active(active);
    control.connect_toggled(move |control| update_view_defaults(&views, |options| update(options, control.is_active())));
    page.append(&control);
}

fn update_view_defaults(views: &Rc<RefCell<ViewPreferencesStore>>, update: impl FnOnce(&mut ViewOptions)) {
    let mut views = views.borrow_mut(); let mut options = views.defaults(); update(&mut options); views.set_defaults(options); let _ = views.save();
}

#[cfg(test)]
mod tests {
    #[test]
    fn categories_cover_current_and_planned_preference_areas() {
        assert_eq!(super::category_names(), ["General", "Views", "File operations"]);
    }

    #[test]
    fn transfer_conflict_controls_expose_every_persisted_action() {
        assert_eq!(super::conflict_default_labels(), ["Ask", "Replace", "Keep Both", "Skip"]);
    }

    #[test]
    fn view_preferences_expose_all_default_groups() {
        assert_eq!(super::view_default_groups(), ["Layout", "Icon size", "Sort", "Sort direction", "Grouping", "Content"]);
    }
}
