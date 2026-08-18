use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use gtk::{Adjustment, Box as GtkBox, Button, CheckButton, Label, Orientation, ScrolledWindow, SpinButton, Stack, StackSwitcher};
use crate::core::{ApplicationPreferences, ConflictDefault, OperationManager};

pub fn category_names() -> [&'static str; 3] { ["General", "Views", "File operations"] }
fn conflict_default_labels() -> [&'static str; 4] { ["Ask", "Replace", "Keep Both", "Skip"] }

pub fn build(preferences: Rc<RefCell<ApplicationPreferences>>, operations: OperationManager) -> ScrolledWindow {
    let content = GtkBox::new(Orientation::Vertical, 14);
    content.set_margin_top(24); content.set_margin_bottom(24); content.set_margin_start(32); content.set_margin_end(32);
    let title = Label::new(Some("Preferences")); title.add_css_class("title-1"); title.set_xalign(0.0); content.append(&title);
    let subtitle = Label::new(Some("Settings are grouped here as their behaviour becomes available.")); subtitle.add_css_class("dim-label"); subtitle.set_xalign(0.0); subtitle.set_wrap(true); content.append(&subtitle);
    let stack = Stack::new(); stack.set_vexpand(true); stack.set_hexpand(true);
    let switcher = StackSwitcher::new(); switcher.set_stack(Some(&stack)); switcher.set_halign(gtk::Align::Start); content.append(&switcher);
    for category in category_names() { let page = if category == "File operations" { operations_page(preferences.clone(), operations.clone()) } else if category == "General" { general_page(preferences.clone()) } else { category_page(category) }; stack.add_titled(&page, Some(&category.to_lowercase().replace(' ', "-")), category); }
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

fn category_page(category: &str) -> GtkBox {
    let page = GtkBox::new(Orientation::Vertical, 8); page.set_margin_top(18);
    let heading = Label::new(Some(category)); heading.add_css_class("title-3"); heading.set_xalign(0.0); page.append(&heading);
    let description = match category {
        "General" => "Startup, tab restoration, and interaction preferences will appear here once those behaviours are implemented.",
        "Views" => "Folder-specific display options are available from the View menu. Global defaults will appear here when implemented.",
        _ => "Operation history retention and transfer defaults will appear here with their persisted operation-history implementation.",
    };
    let text = Label::new(Some(description)); text.set_xalign(0.0); text.set_wrap(true); text.add_css_class("dim-label"); page.append(&text); page
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
}
