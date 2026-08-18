use std::time::Duration;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, Label, Orientation, ProgressBar, ScrolledWindow};

use crate::core::elevated_operations::is_eligible_for_retry;
use crate::core::{OperationManager, OperationRecord, OperationState};

pub fn active_summary(records: &[OperationRecord]) -> Option<String> {
    let active = records.iter().filter(|record| matches!(record.state, OperationState::Queued | OperationState::Running)).count();
    (active > 0).then(|| format!("{active} file operation{} in progress", if active == 1 { "" } else { "s" }))
}

pub fn build(manager: OperationManager) -> ScrolledWindow {
    let content = GtkBox::new(Orientation::Vertical, 10);
    content.set_margin_top(18); content.set_margin_bottom(18); content.set_margin_start(24); content.set_margin_end(24);
    let title = Label::new(Some("File operations")); title.add_css_class("title-2"); title.set_xalign(0.0); content.append(&title);
    let rows = GtkBox::new(Orientation::Vertical, 8); content.append(&rows);
    let clear = Button::with_label("Clear completed"); clear.set_halign(gtk::Align::End); let clear_manager = manager.clone(); clear.connect_clicked(move |_| clear_manager.clear_completed()); content.append(&clear);
    refresh_rows(&rows, &manager);
    let periodic_rows = rows.clone(); let periodic_manager = manager.clone();
    glib::timeout_add_local(Duration::from_millis(250), move || { refresh_rows(&periodic_rows, &periodic_manager); glib::ControlFlow::Continue });
    let scroll = ScrolledWindow::new(); scroll.set_child(Some(&content)); scroll
}

fn refresh_rows(rows: &GtkBox, manager: &OperationManager) {
    while let Some(child) = rows.first_child() { rows.remove(&child); }
    let records = manager.records();
    if records.is_empty() { let empty = Label::new(Some("No recent file operations.")); empty.add_css_class("dim-label"); empty.set_xalign(0.0); rows.append(&empty); return; }
    for record in records.into_iter().rev() {
        let row = GtkBox::new(Orientation::Vertical, 4); row.add_css_class("operation-row");
        let item = record.sources.first().and_then(|path| path.file_name()).and_then(|name| name.to_str()).unwrap_or("item");
        let action = match record.kind { crate::core::OperationKind::Copy => "Copying", crate::core::OperationKind::Move => "Moving", crate::core::OperationKind::Trash => "Moving to Trash", crate::core::OperationKind::Delete => "Deleting" };
        let state = match &record.state { OperationState::Queued => "Queued".into(), OperationState::Running => format!("{} of {} item(s)", record.completed, record.total), OperationState::Completed => "Completed".into(), OperationState::Cancelled => "Cancelled".into(), OperationState::Failed(error) => format!("Failed: {error}") };
        let line = Label::new(Some(&format!("{action} {item} — {state}"))); line.set_xalign(0.0); line.set_wrap(true); row.append(&line);
        if matches!(record.state, OperationState::Queued | OperationState::Running) { let progress = ProgressBar::new(); progress.set_fraction(if record.total == 0 { 0.0 } else { record.completed as f64 / record.total as f64 }); row.append(&progress); let cancel = Button::with_label("Cancel"); cancel.set_halign(gtk::Align::End); let cancel_manager = manager.clone(); let id = record.id; cancel.connect_clicked(move |_| cancel_manager.cancel(id)); row.append(&cancel); }
        if is_eligible_for_retry(&record) {
            let retry = Button::with_label("Authenticate & Retry"); retry.set_halign(gtk::Align::End); retry.set_tooltip_text(Some("Retries this single item with administrator privileges"));
            let retry_manager = manager.clone(); let id = record.id;
            retry.connect_clicked(move |_| { let _ = retry_manager.retry_elevated(id); });
            row.append(&retry);
        }
        rows.append(&row);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn active_summary_is_hidden_without_active_records() { assert_eq!(super::active_summary(&[]), None); }
}
