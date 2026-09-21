//! Native GTK status/alert roles keep the GTK 4.12 baseline. Continuous byte
//! progress is exposed as a value, never pushed into the live status region.
use gtk::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusPriority {
    Polite,
    Urgent,
}

#[derive(Clone)]
pub struct AccessibleStatus {
    pub widget: gtk::Box,
    normal: gtk::Label,
    urgent: gtk::Label,
}

impl Default for AccessibleStatus {
    fn default() -> Self {
        Self::new()
    }
}

impl AccessibleStatus {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_widget_name("explorer-accessible-status");
        let normal = gtk::Label::builder()
            .accessible_role(gtk::AccessibleRole::Status)
            .build();
        let urgent = gtk::Label::builder()
            .accessible_role(gtk::AccessibleRole::Alert)
            .build();
        for label in [&normal, &urgent] {
            label.set_wrap(true);
            label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            label.set_xalign(0.0);
            widget.append(label);
        }
        Self {
            widget,
            normal,
            urgent,
        }
    }

    pub fn announce(&self, message: &str, priority: StatusPriority) {
        let (active, inactive) = match priority {
            StatusPriority::Polite => (&self.normal, &self.urgent),
            StatusPriority::Urgent => (&self.urgent, &self.normal),
        };
        if active.text() == message && inactive.text().is_empty() {
            return;
        }
        inactive.set_text("");
        active.set_text(message);
    }

    /// Find the stable window status without making tabs own their window.
    pub fn announce_for(widget: &impl IsA<gtk::Widget>, message: &str, priority: StatusPriority) {
        fn find(widget: &gtk::Widget) -> Option<AccessibleStatus> {
            if widget.widget_name() == "explorer-accessible-status" {
                return Some(AccessibleStatus {
                    widget: widget.clone().downcast().ok()?,
                    normal: widget.first_child()?.downcast().ok()?,
                    urgent: widget.last_child()?.downcast().ok()?,
                });
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                if let Some(status) = find(&next) {
                    return Some(status);
                }
                child = next.next_sibling();
            }
            None
        }
        if let Some(status) = widget.root().and_then(|root| find(root.upcast_ref())) {
            status.announce(message, priority);
        }
    }

    pub fn watch_operations(&self, manager: crate::core::OperationManager) {
        use crate::core::OperationState;
        // Existing terminal history is not a new event when a window opens.
        let mut previous: std::collections::HashMap<_, _> = manager
            .records()
            .into_iter()
            .filter(|r| r.finished_at.is_some())
            .map(|r| (r.id, r.state))
            .collect();
        let weak = self.widget.downgrade();
        glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
            let Some(widget) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let records = manager.records();
            let mut messages = Vec::new();
            let mut priority = StatusPriority::Polite;
            previous.retain(|id, _| records.iter().any(|r| r.id == *id));
            for record in records {
                let was = previous.insert(record.id, record.state.clone());
                if was.as_ref() == Some(&record.state) {
                    continue;
                }
                let text = match &record.state {
                    OperationState::Queued | OperationState::Running if was.is_none() => {
                        "started".to_owned()
                    }
                    OperationState::Completed if was.is_none() => {
                        "started and completed".to_owned()
                    }
                    OperationState::Completed => "completed".to_owned(),
                    OperationState::Cancelled => "cancelled".to_owned(),
                    OperationState::Failed(error) => {
                        priority = StatusPriority::Urgent;
                        format!("failed: {error}")
                    }
                    _ => continue,
                };
                let outcome = manager
                    .summary(record.id)
                    .map(|s| match s.effect.as_str() {
                        "Partial" => "; some items completed",
                        "Unknown" => "; changes may have occurred. Inspect affected locations",
                        _ => "",
                    })
                    .unwrap_or("");
                messages.push(format!(
                    "{:?} operation {} {text}{outcome}",
                    record.kind, record.id
                ));
            }
            if !messages.is_empty() {
                Self::announce_for(&widget, &messages.join(". "), priority);
            }
            glib::ControlFlow::Continue
        });
    }
}

pub fn name_control(widget: &impl IsA<gtk::Widget>, name: &str) {
    widget.set_tooltip_text(Some(name));
    widget
        .as_ref()
        .update_property(&[gtk::accessible::Property::Label(name)]);
}

pub fn name_expander(button: &gtk::ToggleButton, name: &str) {
    name_control(button, name);
    button.update_state(&[gtk::accessible::State::Expanded(Some(button.is_active()))]);
    button.connect_toggled(|button| {
        button.update_state(&[gtk::accessible::State::Expanded(Some(button.is_active()))]);
    });
}

/// GTK bubbles these keys from the focused descendant to its owning surface.
pub fn context_menu_key(key: gtk::gdk::Key, modifiers: gtk::gdk::ModifierType) -> bool {
    key == gtk::gdk::Key::Menu
        || (key == gtk::gdk::Key::F10 && modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK))
}

pub fn restore_popover_focus(popover: &gtk::Popover, parent: &impl IsA<gtk::Widget>) {
    let focused = parent
        .root()
        .and_downcast::<gtk::Window>()
        .and_then(|window| gtk::prelude::GtkWindowExt::focus(&window))
        .map(|widget| widget.downgrade());
    popover.connect_closed(move |popover| {
        if let Some(widget) = focused.as_ref().and_then(|weak| weak.upgrade()) {
            widget.grab_focus();
        }
        popover.unparent();
    });
}
