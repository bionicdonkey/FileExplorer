use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, Label, Orientation, ProgressBar, ScrolledWindow};

use crate::core::file_item::format_size;
use crate::core::{OperationKind, OperationManager, OperationRecord, OperationState};

pub fn active_summary(records: &[OperationRecord]) -> Option<String> {
    let active = records
        .iter()
        .filter(|record| {
            matches!(
                record.state,
                OperationState::Queued | OperationState::Running | OperationState::CancelRequested
            )
        })
        .count();
    (active > 0).then(|| {
        format!(
            "{active} file operation{} in progress",
            if active == 1 { "" } else { "s" }
        )
    })
}

/// Per-record widgets, created once and updated in place on every poll.
/// Rebuilding the whole row (as this used to do) destroyed and recreated
/// the Cancel button ~10x/second, which visibly flashed its hover state.
struct RowWidgets {
    container: GtkBox,
    line: Label,
    progress: ProgressBar,
    cancel: Button,
    retry: Button,
}

#[derive(Default)]
struct RowsState {
    widgets: HashMap<u64, RowWidgets>,
    empty_label: Option<Label>,
}

pub fn build(manager: OperationManager) -> ScrolledWindow {
    let content = GtkBox::new(Orientation::Vertical, 10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(24);
    content.set_margin_end(24);
    let title = Label::new(Some("File operations"));
    title.add_css_class("title-2");
    title.set_xalign(0.0);
    content.append(&title);
    let rows = GtkBox::new(Orientation::Vertical, 8);
    content.append(&rows);
    let clear = Button::with_label("Clear completed");
    clear.set_halign(gtk::Align::End);
    let clear_manager = manager.clone();
    clear.connect_clicked(move |_| clear_manager.clear_completed());
    content.append(&clear);
    let state = Rc::new(RefCell::new(RowsState::default()));
    refresh_rows(&rows, &manager, &state);
    let periodic_rows = rows.clone();
    let periodic_manager = manager.clone();
    let periodic_state = state.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        refresh_rows(&periodic_rows, &periodic_manager, &periodic_state);
        glib::ControlFlow::Continue
    });
    let scroll = ScrolledWindow::new();
    scroll.set_child(Some(&content));
    scroll
}

fn refresh_rows(rows: &GtkBox, manager: &OperationManager, state: &Rc<RefCell<RowsState>>) {
    let records = manager.records();
    let mut state = state.borrow_mut();

    if records.is_empty() {
        for (_, widgets) in state.widgets.drain() {
            rows.remove(&widgets.container);
        }
        if state.empty_label.is_none() {
            let empty = Label::new(Some("No recent file operations."));
            empty.add_css_class("dim-label");
            empty.set_xalign(0.0);
            rows.append(&empty);
            state.empty_label = Some(empty);
        }
        return;
    }
    if let Some(label) = state.empty_label.take() {
        rows.remove(&label);
    }

    let current_ids: std::collections::HashSet<u64> =
        records.iter().map(|record| record.id).collect();
    let stale: Vec<u64> = state
        .widgets
        .keys()
        .copied()
        .filter(|id| !current_ids.contains(id))
        .collect();
    for id in stale {
        if let Some(widgets) = state.widgets.remove(&id) {
            rows.remove(&widgets.container);
        }
    }

    // Newest first; a brand-new record is prepended so existing rows never
    // need to be reordered (records never change position once created).
    for record in records.into_iter().rev() {
        if let Some(widgets) = state.widgets.get(&record.id) {
            update_row(widgets, &record, manager);
        } else {
            let widgets = build_row(manager, record.id);
            update_row(&widgets, &record, manager);
            rows.prepend(&widgets.container);
            state.widgets.insert(record.id, widgets);
        }
    }
}

fn build_row(manager: &OperationManager, id: u64) -> RowWidgets {
    let container = GtkBox::new(Orientation::Vertical, 4);
    container.add_css_class("operation-row");
    let line = Label::new(None);
    line.set_xalign(0.0);
    line.set_wrap(true);
    container.append(&line);
    let progress = ProgressBar::new();
    progress.set_show_text(true);
    container.append(&progress);
    let cancel = Button::with_label("Cancel");
    crate::ui::accessibility::name_control(&cancel, "Cancel operation");
    cancel.set_halign(gtk::Align::End);
    let cancel_manager = manager.clone();
    cancel.connect_clicked(move |_| cancel_manager.cancel(id));
    container.append(&cancel);
    let retry = Button::with_label("Authenticate & Retry");
    retry.set_halign(gtk::Align::End);
    retry.set_tooltip_text(Some(
        "Retries this single item with administrator privileges",
    ));
    let retry_manager = manager.clone();
    retry.connect_clicked(move |_| {
        let _ = retry_manager.retry_elevated(id);
    });
    container.append(&retry);
    RowWidgets {
        container,
        line,
        progress,
        cancel,
        retry,
    }
}

fn update_row(widgets: &RowWidgets, record: &OperationRecord, manager: &OperationManager) {
    let item = record
        .source_uris
        .first()
        .and_then(|uri| crate::core::Location::from_uri(uri).ok())
        .map(|location| location.display_name())
        .or_else(|| {
            record
                .sources
                .first()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "item".into());
    let action = match record.kind {
        OperationKind::Copy => "Copying",
        OperationKind::Move => "Moving",
        OperationKind::Create => "Creating folder",
        OperationKind::Rename => "Renaming",
        OperationKind::Trash => "Moving to Trash",
        OperationKind::Delete => "Deleting",
    };
    let state_text = match &record.state {
        OperationState::Queued => "Queued".into(),
        OperationState::CancelRequested => "Cancellation requested".into(),
        // Copy tracks bytes (so a single large file still shows motion);
        // other kinds are quick per-item operations, shown as a count.
        OperationState::Running if record.kind == OperationKind::Copy => format!(
            "{} of {}",
            format_size(record.completed as u64),
            format_size(record.total as u64)
        ),
        OperationState::Running => format!("{} of {} item(s)", record.completed, record.total),
        OperationState::Completed => "Completed".into(),
        OperationState::Cancelled => "Cancelled".into(),
        OperationState::Failed(error) => format!("Failed: {error}"),
    };
    let outcome = manager
        .summary(record.id)
        .map(|summary| match summary.effect.as_str() {
            "Partial" => format!(
                " — {} item(s) completed; remaining items were not completed",
                summary.completed.len()
            ),
            "Unknown" => " — changes may have occurred; inspect the affected locations".into(),
            _ => String::new(),
        })
        .unwrap_or_default();
    widgets
        .line
        .set_text(&format!("{action} {item} — {state_text}{outcome}"));

    let active = matches!(
        record.state,
        OperationState::Queued | OperationState::Running | OperationState::CancelRequested
    );
    widgets.progress.set_visible(active);
    widgets.cancel.set_visible(active);
    widgets
        .cancel
        .set_sensitive(record.state != OperationState::CancelRequested);
    widgets
        .cancel
        .update_property(&[gtk::accessible::Property::Description(&format!(
            "{action} {item}. {}",
            if record.state == OperationState::CancelRequested {
                "Cancellation requested; waiting for the current item"
            } else {
                "Stop this operation; completed items are retained"
            }
        ))]);
    widgets.progress.update_property(&[
        gtk::accessible::Property::Label("Operation progress"),
        gtk::accessible::Property::ValueText(&state_text),
    ]);
    if active {
        let fraction = if record.total == 0 {
            0.0
        } else {
            record.completed as f64 / record.total as f64
        };
        widgets.progress.set_fraction(fraction);
        widgets
            .progress
            .set_text(Some(&format!("{:.0}%", fraction * 100.0)));
    }

    widgets
        .retry
        .set_visible(manager.can_retry_elevated(record.id));
}

#[cfg(test)]
mod tests {
    #[test]
    fn active_summary_is_hidden_without_active_records() {
        assert_eq!(super::active_summary(&[]), None);
    }
}
