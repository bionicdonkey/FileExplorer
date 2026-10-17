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
            !record.hidden
                && matches!(
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

fn is_active(record: &OperationRecord) -> bool {
    matches!(
        record.state,
        OperationState::Queued | OperationState::Running | OperationState::CancelRequested
    )
}

fn action_label(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Copy => "Copying",
        OperationKind::Move => "Moving",
        OperationKind::Create => "Creating folder",
        OperationKind::Rename => "Renaming",
        OperationKind::Trash => "Moving to Trash",
        OperationKind::Delete => "Deleting",
        OperationKind::Compress => "Compressing",
        OperationKind::Extract => "Extracting",
    }
}

fn item_name(record: &OperationRecord) -> String {
    record
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
        .unwrap_or_else(|| "item".into())
}

/// What the window footer shows while work is running: a description and how
/// far along it is, or `None` for the fraction when any of it cannot be
/// measured yet. `None` overall means nothing is running.
pub fn footer_progress(records: &[OperationRecord]) -> Option<(String, Option<f64>)> {
    let active: Vec<_> = records
        .iter()
        .filter(|record| !record.hidden && is_active(record))
        .collect();
    let [first, rest @ ..] = active.as_slice() else {
        return None;
    };
    let label = if rest.is_empty() {
        format!("{} {}", action_label(first.display_kind()), item_name(first))
    } else {
        format!("{} operations", active.len())
    };
    let fractions: Vec<f64> = active
        .iter()
        .filter(|record| record.total > 0)
        .map(|record| (record.completed as f64 / record.total as f64).clamp(0.0, 1.0))
        .collect();
    let fraction = (fractions.len() == active.len())
        .then(|| fractions.iter().sum::<f64>() / fractions.len() as f64);
    Some((label, fraction))
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

/// Which records a list shows, and what it says when there are none.
#[derive(Clone, Copy)]
struct Listing {
    wanted: fn(&OperationRecord) -> bool,
    empty_text: &'static str,
}

const ALL_OPERATIONS: Listing = Listing {
    wanted: |record| !record.hidden,
    empty_text: "No recent file operations.",
};

const RUNNING_OPERATIONS: Listing = Listing {
    wanted: |record| !record.hidden && is_active(record),
    empty_text: "Nothing is running.",
};

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
    refresh_rows(&rows, &manager, &state, ALL_OPERATIONS);
    let periodic_rows = rows.clone();
    let periodic_manager = manager.clone();
    let periodic_state = state.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        refresh_rows(
            &periodic_rows,
            &periodic_manager,
            &periodic_state,
            ALL_OPERATIONS,
        );
        glib::ControlFlow::Continue
    });
    let scroll = ScrolledWindow::new();
    scroll.set_child(Some(&content));
    scroll
}

/// The running operations, for the footer's expanding list: the same rows as
/// the Operations tab, limited to what is in progress, and idle while hidden.
pub fn build_running_list(
    manager: OperationManager,
    on_show_all: impl Fn() + 'static,
) -> GtkBox {
    let content = GtkBox::new(Orientation::Vertical, 10);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.set_size_request(360, -1);
    let rows = GtkBox::new(Orientation::Vertical, 8);
    content.append(&rows);
    let show_all = Button::with_label("Show all operations");
    show_all.set_halign(gtk::Align::End);
    show_all.connect_clicked(move |_| on_show_all());
    content.append(&show_all);
    let state = Rc::new(RefCell::new(RowsState::default()));
    let weak_rows = rows.downgrade();
    let weak_content = content.downgrade();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let (Some(rows), Some(content)) = (weak_rows.upgrade(), weak_content.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        if content.is_mapped() {
            refresh_rows(&rows, &manager, &state, RUNNING_OPERATIONS);
        }
        glib::ControlFlow::Continue
    });
    content
}

fn refresh_rows(
    rows: &GtkBox,
    manager: &OperationManager,
    state: &Rc<RefCell<RowsState>>,
    listing: Listing,
) {
    let records: Vec<_> = manager
        .records()
        .into_iter()
        .filter(|record| (listing.wanted)(record))
        .collect();
    let mut state = state.borrow_mut();

    if records.is_empty() {
        for (_, widgets) in state.widgets.drain() {
            rows.remove(&widgets.container);
        }
        if state.empty_label.is_none() {
            let empty = Label::new(Some(listing.empty_text));
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
    let item = item_name(record);
    let kind = record.display_kind();
    let action = action_label(kind);
    // Work with no measured size yet, such as an archive still being opened.
    let preparing = record.total == 0
        && matches!(kind, OperationKind::Compress | OperationKind::Extract)
        && record.state == OperationState::Running;
    let counts_bytes = matches!(
        kind,
        OperationKind::Copy | OperationKind::Compress | OperationKind::Extract
    );
    let state_text = match &record.state {
        OperationState::Queued => "Queued".into(),
        OperationState::CancelRequested => "Cancellation requested".into(),
        // Copying and archiving track bytes (so a single large file still
        // shows motion); other kinds are quick per-item operations, shown as a count.
        OperationState::Running if preparing => "Preparing…".into(),
        OperationState::Running if counts_bytes => format!(
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

    let active = is_active(record);
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
    if preparing {
        widgets.progress.pulse();
        widgets.progress.set_text(None);
    } else if active {
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
    use crate::core::{OperationKind, OperationRecord, OperationState};

    fn record(kind: OperationKind, completed: usize, total: usize) -> OperationRecord {
        OperationRecord {
            id: 1,
            kind,
            source_uris: vec!["file:///tmp/Trip".into()],
            sources: vec!["/tmp/Trip".into()],
            destination_uri: None,
            destination: None,
            completed,
            total,
            state: OperationState::Running,
            finished_at: None,
            hidden: false,
            shown_as: None,
        }
    }

    #[test]
    fn active_summary_is_hidden_without_active_records() {
        assert_eq!(super::active_summary(&[]), None);
    }

    #[test]
    fn the_footer_names_one_operation_and_counts_several() {
        assert_eq!(super::footer_progress(&[]), None);
        assert_eq!(
            super::footer_progress(&[record(OperationKind::Compress, 25, 100)]),
            Some(("Compressing Trip".to_owned(), Some(0.25)))
        );
        let (label, fraction) = super::footer_progress(&[
            record(OperationKind::Copy, 50, 100),
            record(OperationKind::Extract, 100, 100),
        ])
        .unwrap();
        assert_eq!(label, "2 operations");
        assert_eq!(fraction, Some(0.75));
    }

    #[test]
    fn the_footer_has_no_fraction_while_any_work_is_unmeasured() {
        let (_, fraction) = super::footer_progress(&[
            record(OperationKind::Compress, 0, 0),
            record(OperationKind::Copy, 50, 100),
        ])
        .unwrap();
        assert_eq!(fraction, None);
    }

    #[test]
    fn a_stand_in_record_never_reaches_the_footer() {
        let mut stand_in = record(OperationKind::Extract, 0, 0);
        stand_in.hidden = true;
        assert_eq!(super::footer_progress(&[stand_in]), None);
    }
}
