use std::path::PathBuf;

use gtk::prelude::*;
use gtk::{Box as GtkBox, CheckButton, Entry, Grid, Label, Orientation, Stack, StackSwitcher};

use crate::core::{PermissionBits, PropertyDetails};

pub struct PropertiesDialog;

impl PropertiesDialog {
    #[allow(deprecated)]
    pub fn present<F: Fn() + 'static>(parent: Option<&gtk::Window>, path: PathBuf, on_saved: F) {
        let Ok(details) = PropertyDetails::load(&path) else { return; };
        let dialog = gtk::Dialog::new();
        dialog.set_title(Some(&format!("Properties — {}", details.name)));
        dialog.set_modal(true);
        dialog.set_default_size(520, 480);
        dialog.set_transient_for(parent);
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        dialog.add_button("Apply", gtk::ResponseType::Apply);
        dialog.add_button("OK", gtk::ResponseType::Ok);
        let content = dialog.content_area();
        content.set_margin_top(12); content.set_margin_bottom(12); content.set_margin_start(16); content.set_margin_end(16);
        let stack = Stack::new();
        let switcher = StackSwitcher::new(); switcher.set_stack(Some(&stack));
        content.append(&switcher);
        let name = Entry::new(); name.set_text(&details.name);
        stack.add_titled(&general_page(&details, &name), Some("general"), "General");
        let checks = permission_page(details.permissions);
        stack.add_titled(&checks.0, Some("permissions"), "Permissions");
        stack.add_titled(&sharing_page(details.is_dir), Some("sharing"), "Sharing");
        content.append(&stack);
        dialog.connect_response(move |dialog, response| {
            if matches!(response, gtk::ResponseType::Apply | gtk::ResponseType::Ok) {
                let new_name = name.text().trim().to_owned();
                let new_path = if new_name.is_empty() { path.clone() } else { path.parent().unwrap_or(&path).join(new_name) };
                let renamed = if new_path != path { std::fs::rename(&path, &new_path).is_ok() } else { true };
                if renamed {
                    #[cfg(unix)] {
                        use std::os::unix::fs::PermissionsExt;
                        if let Ok(metadata) = std::fs::metadata(&new_path) {
                            let bits = PermissionBits { owner_read: checks.1[0].is_active(), owner_write: checks.1[1].is_active(), owner_execute: checks.1[2].is_active(), group_read: checks.1[3].is_active(), group_write: checks.1[4].is_active(), group_execute: checks.1[5].is_active(), other_read: checks.1[6].is_active(), other_write: checks.1[7].is_active(), other_execute: checks.1[8].is_active() };
                            let _ = std::fs::set_permissions(&new_path, PermissionsExt::from_mode(bits.to_mode()));
                            let _ = metadata;
                        }
                    }
                    on_saved();
                }
                if response == gtk::ResponseType::Ok { dialog.close(); }
            } else { dialog.close(); }
        });
        dialog.present();
    }
}

fn general_page(details: &PropertyDetails, name: &Entry) -> GtkBox {
    let page = GtkBox::new(Orientation::Vertical, 12);
    page.set_margin_top(16);
    let heading = Label::new(Some(if details.is_dir { "📁" } else { "📄" })); heading.set_css_classes(&["title-1"]); heading.set_halign(gtk::Align::Center); page.append(&heading);
    let grid = Grid::new(); grid.set_column_spacing(18); grid.set_row_spacing(9);
    for (row, (label, value)) in [("Name", name.text().to_string()), ("Type", if details.is_dir { "Folder".into() } else { gio::content_type_get_description(&details.content_type).to_string() }), ("Location", details.path.parent().unwrap_or(&details.path).display().to_string()), ("Size", format!("{} bytes", details.size)), ("Modified", format_time(details.modified)), ("Accessed", format_time(details.accessed)), ("Created", format_time(details.created))].into_iter().enumerate() {
        let key = Label::new(Some(label)); key.set_xalign(0.0); key.add_css_class("dim-label"); grid.attach(&key, 0, row as i32, 1, 1);
        if label == "Name" { grid.attach(name, 1, row as i32, 1, 1); } else { let value = Label::new(Some(&value)); value.set_xalign(0.0); value.set_selectable(true); grid.attach(&value, 1, row as i32, 1, 1); }
    }
    page.append(&grid); page
}

fn permission_page(bits: PermissionBits) -> (GtkBox, Vec<CheckButton>) {
    let page = GtkBox::new(Orientation::Vertical, 10); page.set_margin_top(16);
    page.append(&Label::new(Some("Access permissions")));
    let grid = Grid::new(); grid.set_column_spacing(12); grid.set_row_spacing(8);
    for (column, title) in ["Read", "Write", "Execute"].into_iter().enumerate() { grid.attach(&Label::new(Some(title)), (column + 1) as i32, 0, 1, 1); }
    let values = [bits.owner_read,bits.owner_write,bits.owner_execute,bits.group_read,bits.group_write,bits.group_execute,bits.other_read,bits.other_write,bits.other_execute];
    let mut checks = Vec::new();
    for (row, title) in ["Owner", "Group", "Others"].into_iter().enumerate() { grid.attach(&Label::new(Some(title)), 0, (row + 1) as i32, 1, 1); for column in 0..3 { let check = CheckButton::new(); check.set_active(values[row * 3 + column]); grid.attach(&check, (column + 1) as i32, (row + 1) as i32, 1, 1); checks.push(check); } }
    page.append(&grid); (page, checks)
}

fn sharing_page(is_folder: bool) -> GtkBox { let page = GtkBox::new(Orientation::Vertical, 12); page.set_margin_top(16); page.append(&Label::new(Some(if is_folder { "Folder sharing will be available when Samba user shares are configured on this system." } else { "Sharing is available for folders only." }))); page }
fn format_time(time: Option<std::time::SystemTime>) -> String { time.and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok()).map(|value| format!("{} seconds since Unix epoch", value.as_secs())).unwrap_or_else(|| "Unavailable".into()) }
