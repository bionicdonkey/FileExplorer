use crate::core::{contracts::OperationError, file_item::format_size, Location};
use gtk::prelude::*;

pub struct PropertiesDialog;

impl PropertiesDialog {
    /// Metadata is queried asynchronously; absent provider fields stay unavailable.
    #[allow(deprecated)]
    pub fn present<F: Fn() + 'static>(
        parent: Option<&gtk::Window>,
        location: Location,
        on_closed: F,
    ) -> gtk::Dialog {
        let dialog = gtk::Dialog::new();
        dialog.set_title(Some("Properties"));
        dialog.set_transient_for(parent);
        dialog.set_modal(true);
        dialog.set_default_size(520, 380);
        dialog.add_button("Close", gtk::ResponseType::Close);
        let content = dialog.content_area();
        content.set_spacing(10);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);
        let status = gtk::Label::new(Some("Loading properties…"));
        let announcements = crate::ui::accessibility::AccessibleStatus::new();
        content.append(&announcements.widget);
        status.set_wrap(true);
        status.set_selectable(true);
        content.append(&status);
        let grid = gtk::Grid::new();
        grid.set_column_spacing(20);
        grid.set_row_spacing(8);
        let fields = [
            "Name",
            "Type",
            "Size",
            "Modified",
            "Created",
            "Owner",
            "Group",
            "Permissions",
        ];
        let mut values = Vec::new();
        for (row, field) in fields.into_iter().enumerate() {
            let label = gtk::Label::new(Some(field));
            label.set_xalign(0.0);
            let value = gtk::Label::new(Some("Unavailable"));
            value.set_xalign(0.0);
            value.set_selectable(true);
            value.set_wrap(true);
            value.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            value.set_max_width_chars(40);
            value.update_relation(&[gtk::accessible::Relation::LabelledBy(&[label.upcast_ref()])]);
            grid.attach(&label, 0, row as i32, 1, 1);
            grid.attach(&value, 1, row as i32, 1, 1);
            values.push(value);
        }
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_vexpand(true);
        scroll.set_child(Some(&grid));
        content.append(&scroll);
        let cancel = gio::Cancellable::new();
        let cancel_close = cancel.clone();
        dialog.connect_response(move |dialog, _| {
            cancel_close.cancel();
            on_closed();
            dialog.close();
        });
        let cancel_destroy = cancel.clone();
        dialog.connect_destroy(move |_| cancel_destroy.cancel());
        let weak = dialog.downgrade();
        let cancel_query = cancel.clone();
        gio::File::for_uri(location.uri()).query_info_async(
            "standard::display-name,standard::content-type,standard::size,time::modified,time::created,owner::user,owner::group,unix::mode",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, glib::Priority::DEFAULT, Some(&cancel),
            move |result| {
                if weak.upgrade().is_none() || cancel_query.is_cancelled() { return; }
                match result {
                    Err(error) => {
                        let message = format!("Properties unavailable: {}", OperationError::from_gio(&error, Some(&location), None).message_key);
                        status.set_text(&message);
                        announcements.announce(&message, crate::ui::accessibility::StatusPriority::Urgent);
                    },
                    Ok(info) => {
                        status.set_text("Properties");
                        announcements.announce("Properties loaded", crate::ui::accessibility::StatusPriority::Polite);
                        for (index, attribute) in ["standard::display-name", "standard::content-type", "standard::size", "time::modified", "time::created", "owner::user", "owner::group", "unix::mode"].iter().enumerate() {
                            if !info.has_attribute(attribute) { continue; }
                            let text = match index {
                                0 => info.display_name().to_string(),
                                1 => info.content_type().map(|t| gio::content_type_get_description(&t).to_string()).unwrap_or_else(|| "Unavailable".into()),
                                2 => format_size(info.size().max(0) as u64),
                                3 | 4 => glib::DateTime::from_unix_local(info.attribute_uint64(attribute) as i64).ok().and_then(|date| date.format("%Y-%m-%d %H:%M").ok()).map(|s| s.to_string()).unwrap_or_else(|| "Unavailable".into()),
                                7 => format!("{:o}", info.attribute_uint32(attribute) & 0o7777),
                                _ => info.attribute_string(attribute).map(|s| s.to_string()).unwrap_or_else(|| "Unavailable".into()),
                            };
                            values[index].set_text(&text);
                        }
                    }
                }
            },
        );
        dialog.present();
        dialog
    }
}
