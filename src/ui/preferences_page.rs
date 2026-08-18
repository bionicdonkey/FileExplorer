use gtk::prelude::*;
use gtk::{Box as GtkBox, Label, Orientation, ScrolledWindow, Stack, StackSwitcher};

pub fn category_names() -> [&'static str; 3] { ["General", "Views", "File operations"] }

pub fn build() -> ScrolledWindow {
    let content = GtkBox::new(Orientation::Vertical, 14);
    content.set_margin_top(24); content.set_margin_bottom(24); content.set_margin_start(32); content.set_margin_end(32);
    let title = Label::new(Some("Preferences")); title.add_css_class("title-1"); title.set_xalign(0.0); content.append(&title);
    let subtitle = Label::new(Some("Settings are grouped here as their behaviour becomes available.")); subtitle.add_css_class("dim-label"); subtitle.set_xalign(0.0); subtitle.set_wrap(true); content.append(&subtitle);
    let stack = Stack::new(); stack.set_vexpand(true); stack.set_hexpand(true);
    let switcher = StackSwitcher::new(); switcher.set_stack(Some(&stack)); switcher.set_halign(gtk::Align::Start); content.append(&switcher);
    for category in category_names() { stack.add_titled(&category_page(category), Some(&category.to_lowercase().replace(' ', "-")), category); }
    content.append(&stack);
    let scroll = ScrolledWindow::new(); scroll.set_child(Some(&content)); scroll
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
}
