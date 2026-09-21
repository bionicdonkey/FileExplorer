use adw::{TabBar, TabView};
use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, GestureClick, Orientation, Popover};

use crate::ui::context_menu::ContextCommand;

use crate::ui::tab_layout::TabLayout;

/// Update a directory tab only when its canonical location has committed.
pub fn set_location_title(page: &adw::TabPage, location: &crate::core::Location) {
    page.set_title(&location.display_name());
    page.set_tooltip(&location.redacted());
}

#[derive(Clone)]
pub struct TabStrip {
    pub bar: TabBar,
    pub view: TabView,
    pub previous_button: Button,
    pub next_button: Button,
}

impl TabStrip {
    pub fn new() -> Self {
        let view = TabView::new();
        let bar = TabBar::new();
        bar.set_view(Some(&view));
        bar.set_autohide(false);
        bar.set_expand_tabs(true);
        bar.add_css_class("tab-strip");

        let previous_button = overflow_button("go-previous-symbolic", "Previous tab");
        let next_button = overflow_button("go-next-symbolic", "Next tab");
        bar.set_start_action_widget(Some(&previous_button));
        bar.set_end_action_widget(Some(&next_button));

        let previous_view = view.clone();
        previous_button.connect_clicked(move |_| {
            previous_view.select_previous_page();
        });

        let next_view = view.clone();
        next_button.connect_clicked(move |_| {
            next_view.select_next_page();
        });

        Self {
            bar,
            view,
            previous_button,
            next_button,
        }
    }

    pub fn set_layout(&self, layout: TabLayout) {
        self.bar.set_width_request(layout.tab_bar_width);
        self.previous_button.set_visible(layout.overflowing);
        self.next_button.set_visible(layout.overflowing);
        self.bar.set_tooltip_text(if layout.overflowing {
            Some("Scroll to view more tabs")
        } else {
            None
        });
    }

    pub fn set_context_handler<F: Fn(ContextCommand) + 'static>(&self, callback: F) {
        let handler = std::rc::Rc::new(callback);
        let gesture = GestureClick::new();
        gesture.set_button(3);
        let bar = self.bar.clone();
        let popup = std::rc::Rc::new(move |x: f64, y: f64| {
            let popover = Popover::new();
            popover.set_parent(&bar);
            crate::ui::accessibility::restore_popover_focus(&popover, &bar);
            popover.set_autohide(true);
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            let menu = GtkBox::new(Orientation::Vertical, 2);
            menu.set_margin_top(6);
            menu.set_margin_bottom(6);
            for (label, command) in [
                ("Close Tab", ContextCommand::CloseTab),
                ("Close Other Tabs", ContextCommand::CloseOtherTabs),
                ("Duplicate Tab", ContextCommand::DuplicateTab),
            ] {
                let button = Button::with_label(label);
                let handler = handler.clone();
                let popover = popover.clone();
                button.connect_clicked(move |_| {
                    popover.popdown();
                    handler(command);
                });
                menu.append(&button);
            }
            popover.set_child(Some(&menu));
            popover.popup();
        });
        let pointer_popup = popup.clone();
        gesture.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            pointer_popup(x, y);
        });
        self.bar.add_controller(gesture);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if crate::ui::accessibility::context_menu_key(key, modifiers) {
                popup(0.0, 0.0);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        self.bar.add_controller(keys);
    }
}

fn overflow_button(icon_name: &str, tooltip: &str) -> Button {
    let button = Button::from_icon_name(icon_name);
    button.add_css_class("flat");
    button.set_tooltip_text(Some(tooltip));
    crate::ui::accessibility::name_control(&button, tooltip);
    button.set_visible(false);
    button
}

#[cfg(test)]
mod tests {
    use super::TabStrip;
    use crate::ui::tab_layout::{calculate_tab_layout, TabLayoutInput};
    use gtk::prelude::*;

    #[test]
    #[ignore = "requires a GTK desktop session"]
    fn tab_strip_updates_overflow_navigation_controls_from_layout() {
        gtk::init().expect("GTK should initialize for widget tests");
        let tabs = TabStrip::new();

        assert!(!tabs.previous_button.is_visible());
        assert!(!tabs.next_button.is_visible());
        assert_eq!(
            tabs.previous_button.tooltip_text().as_deref(),
            Some("Previous tab")
        );
        assert_eq!(tabs.next_button.tooltip_text().as_deref(), Some("Next tab"));
        let overflow = calculate_tab_layout(TabLayoutInput::new(11, 1280));
        let fitting = calculate_tab_layout(TabLayoutInput::new(1, 1280));

        tabs.set_layout(overflow);
        assert!(tabs.previous_button.is_visible());
        assert!(tabs.next_button.is_visible());

        tabs.set_layout(fitting);
        assert!(!tabs.previous_button.is_visible());
        assert!(!tabs.next_button.is_visible());
    }
}
