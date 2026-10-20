use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Entry, Image, Label, ListBox, ListBoxRow, Orientation, Popover};
use gtk::{ScrolledWindow, SelectionMode};

/// Enough rows to choose from without turning a huge folder into a second view.
const MAX_SUGGESTIONS: usize = 50;
/// Reading a directory is cheap, but a ten-thousand-entry folder should not be
/// walked on every keystroke just to fill a popover.
const MAX_ENUMERATED: usize = 4096;
const BATCH_SIZE: i32 = 256;

#[derive(Clone)]
struct Suggestion {
    name: String,
    folded: String,
    text: String,
}

/// The folder whose children can complete an entry's text, and the partial
/// name to match inside it.
struct CompletionTarget {
    directory: gio::File,
    directory_text: String,
    prefix: String,
    prefix_folded: String,
    uri_style: bool,
}

impl CompletionTarget {
    fn parse(input: &str) -> Option<Self> {
        if input.is_empty() || input.chars().any(char::is_control) {
            return None;
        }
        let (directory_text, prefix, uri_style) = if let Some(scheme_end) = input.find("://") {
            let authority_start = scheme_end + 3;
            // Without a path there is nothing below the authority to complete.
            input[authority_start..].find('/')?;
            let separator = input.rfind('/')?;
            let prefix = &input[separator + 1..];
            // The entry holds a URI when a remote location or a tab supplied
            // one, so the typed prefix may be percent-encoded.
            let prefix = glib::Uri::unescape_string(prefix, None)
                .map(|decoded| decoded.to_string())
                .unwrap_or_else(|| prefix.to_owned());
            (&input[..=separator], prefix, true)
        } else {
            // LocationInput only accepts absolute paths or URIs, so a relative
            // spelling cannot be completed without inventing a base.
            if !input.starts_with('/') {
                return None;
            }
            let separator = input.rfind('/')?;
            (
                &input[..=separator],
                input[separator + 1..].to_owned(),
                false,
            )
        };
        if prefix.contains(['?', '#']) {
            return None;
        }
        let directory = if uri_style {
            gio::File::for_uri(directory_text)
        } else {
            gio::File::for_path(directory_text)
        };
        Some(Self {
            directory,
            directory_text: directory_text.to_owned(),
            prefix_folded: prefix.to_lowercase(),
            prefix,
            uri_style,
        })
    }

    fn matches(&self, name: &str) -> bool {
        // Hiding dotfiles keeps the common case quiet; asking for one still
        // finds it, the way a shell does.
        if name.starts_with('.') && !self.prefix.starts_with('.') {
            return false;
        }
        name.to_lowercase().starts_with(&self.prefix_folded)
    }

    fn suggestion(&self, name: &str) -> Suggestion {
        let segment = if self.uri_style {
            glib::Uri::escape_string(name, Some("/"), false).to_string()
        } else {
            name.to_owned()
        };
        Suggestion {
            name: name.to_owned(),
            folded: name.to_lowercase(),
            text: format!("{}{segment}/", self.directory_text),
        }
    }
}

/// Folder suggestions for a location entry.
///
/// GTK 4's `GtkEntryCompletion` is deprecated, so this builds the same
/// interaction from a popover and a list: typing lists matching folders,
/// Up/Down choose one, Enter opens it, Tab inserts it, and Escape closes the
/// list before the entry's own Escape handler cancels editing.
pub struct LocationCompletion {
    entry: Entry,
    popover: Popover,
    list: ListBox,
    suggestions: RefCell<Vec<Suggestion>>,
    generation: Cell<u64>,
    pending: RefCell<Option<gio::Cancellable>>,
    quiet: Cell<bool>,
    selecting: Cell<bool>,
}

impl LocationCompletion {
    pub fn new(entry: &Entry) -> Rc<Self> {
        let list = ListBox::new();
        list.set_selection_mode(SelectionMode::Single);
        list.set_activate_on_single_click(true);
        list.set_can_focus(false);
        list.set_focusable(false);
        list.update_property(&[gtk::accessible::Property::Label("Folder suggestions")]);

        let scroll = ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_min_content_width(320);
        scroll.set_max_content_height(320);
        scroll.set_propagate_natural_height(true);
        scroll.set_child(Some(&list));

        let popover = Popover::new();
        popover.set_parent(entry);
        popover.set_position(gtk::PositionType::Bottom);
        popover.set_has_arrow(false);
        // Autohide popovers take a keyboard grab, which would pull focus and
        // keystrokes away from the entry. Closing is driven by focus loss,
        // Escape, and committing a suggestion instead.
        popover.set_autohide(false);
        popover.set_can_focus(false);
        popover.set_focusable(false);
        popover.add_css_class("location-completion");
        popover.set_child(Some(&scroll));

        let completion = Rc::new(Self {
            entry: entry.clone(),
            popover,
            list,
            suggestions: RefCell::new(Vec::new()),
            generation: Cell::new(0),
            pending: RefCell::new(None),
            quiet: Cell::new(false),
            selecting: Cell::new(false),
        });

        connect_changed(&completion);
        connect_keys(&completion);
        connect_focus(&completion);
        connect_rows(&completion);
        connect_selected(&completion);
        connect_closed(&completion);
        completion
    }

    /// Replaces the entry text without asking for completions. Used when a tab
    /// or breadcrumb moves the location, where a popup would be noise.
    pub fn set_text(&self, text: &str) {
        self.dismiss();
        self.quiet.set(true);
        self.entry.set_text(text);
        self.quiet.set(false);
        self.entry.set_position(-1);
    }

    /// Cancels in-flight work and hides suggestions, leaving the text alone.
    pub fn dismiss(&self) {
        self.cancel_pending();
        self.generation.set(self.generation.get().wrapping_add(1));
        self.popover.popdown();
        self.clear();
    }

    fn clear(&self) {
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        self.suggestions.borrow_mut().clear();
    }

    fn cancel_pending(&self) {
        if let Some(pending) = self.pending.borrow_mut().take() {
            pending.cancel();
        }
    }

    /// Asks the folder holding the partial name for its matching children.
    fn request(self: &Rc<Self>) {
        self.cancel_pending();
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let Some(target) = CompletionTarget::parse(self.entry.text().as_str()) else {
            self.popover.popdown();
            self.clear();
            return;
        };

        let cancellable = gio::Cancellable::new();
        *self.pending.borrow_mut() = Some(cancellable.clone());
        let completion = Rc::downgrade(self);
        let cancel = cancellable.clone();
        // The call borrows its own handle; the callback owns the one that is
        // checked and forwarded to later batches.
        let async_cancellable = cancellable.clone();
        target.directory.clone().enumerate_children_async(
            "standard::name,standard::display-name,standard::type",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
            Some(&async_cancellable),
            move |result| {
                let Some(completion) = completion.upgrade() else {
                    return;
                };
                if cancel.is_cancelled() || completion.generation.get() != generation {
                    return;
                }
                let Ok(enumerator) = result else {
                    completion.popover.popdown();
                    completion.clear();
                    return;
                };
                completion.collect(enumerator, target, generation, cancellable, 0, Vec::new());
            },
        );
    }

    fn collect(
        self: &Rc<Self>,
        enumerator: gio::FileEnumerator,
        target: CompletionTarget,
        generation: u64,
        cancellable: gio::Cancellable,
        scanned: usize,
        found: Vec<Suggestion>,
    ) {
        let completion = Rc::downgrade(self);
        let async_cancellable = cancellable.clone();
        enumerator.clone().next_files_async(
            BATCH_SIZE,
            glib::Priority::DEFAULT,
            Some(&async_cancellable),
            move |result| {
                let Some(completion) = completion.upgrade() else {
                    return;
                };
                if cancellable.is_cancelled() || completion.generation.get() != generation {
                    return;
                }
                let batch = match result {
                    Ok(batch) => batch,
                    Err(_) => {
                        completion.show(target, found);
                        return;
                    }
                };
                if batch.is_empty() {
                    completion.show(target, found);
                    return;
                }

                let mut scanned = scanned;
                let mut found = found;
                for info in batch {
                    scanned += 1;
                    if scanned > MAX_ENUMERATED {
                        break;
                    }
                    if info.file_type() != gio::FileType::Directory {
                        continue;
                    }
                    let Some(name) = info.name().to_str().map(ToOwned::to_owned) else {
                        continue;
                    };
                    if name.chars().any(char::is_control) {
                        continue;
                    }
                    if !target.matches(&name) {
                        continue;
                    }
                    found.push(target.suggestion(&name));
                    if found.len() >= MAX_SUGGESTIONS {
                        break;
                    }
                }

                if scanned >= MAX_ENUMERATED || found.len() >= MAX_SUGGESTIONS {
                    completion.show(target, found);
                } else {
                    completion.collect(enumerator, target, generation, cancellable, scanned, found);
                }
            },
        );
    }

    fn show(&self, target: CompletionTarget, mut found: Vec<Suggestion>) {
        if found.is_empty() || !self.entry.is_mapped() {
            self.popover.popdown();
            self.clear();
            return;
        }
        let prefix = &target.prefix;
        found.sort_by(|left, right| {
            let exact = |suggestion: &Suggestion| suggestion.name.starts_with(prefix);
            exact(right)
                .cmp(&exact(left))
                .then_with(|| left.folded.cmp(&right.folded))
                .then_with(|| left.name.cmp(&right.name))
        });
        found.truncate(MAX_SUGGESTIONS);

        self.clear();
        for suggestion in &found {
            self.list.append(&suggestion_row(suggestion));
        }
        *self.suggestions.borrow_mut() = found;

        if self.entry.width() > 0 {
            self.popover.set_size_request(self.entry.width(), -1);
        }
        self.popover.popup();
        self.select(0);
    }

    fn select(&self, index: usize) {
        let Some(row) = self.list.row_at_index(index as i32) else {
            return;
        };
        // Programmatic selection must not be mistaken for the user clicking a
        // suggestion.
        self.selecting.set(true);
        self.list.select_row(Some(&row));
        self.selecting.set(false);
    }

    fn accept(&self, index: usize, activate: bool) {
        let Some(suggestion) = self.suggestions.borrow().get(index).cloned() else {
            return;
        };
        self.set_text(&suggestion.text);
        if activate {
            self.entry.emit_activate();
        } else {
            self.entry.set_position(-1);
        }
    }

    fn step(&self, delta: isize) {
        let count = self.suggestions.borrow().len();
        if count == 0 {
            return;
        }
        let current = self
            .list
            .selected_row()
            .map_or(0, |row| row.index().max(0) as usize);
        let next = if delta < 0 {
            current.saturating_sub(1)
        } else {
            (current + 1).min(count - 1)
        };
        self.select(next);
    }
}

fn suggestion_row(suggestion: &Suggestion) -> ListBoxRow {
    let content = GtkBox::new(Orientation::Horizontal, 8);
    content.set_margin_top(6);
    content.set_margin_bottom(6);
    content.set_margin_start(8);
    content.set_margin_end(8);
    let icon = Image::from_icon_name("folder-symbolic");
    icon.set_pixel_size(16);
    content.append(&icon);
    let label = Label::new(Some(&suggestion.name));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&label);

    let row = ListBoxRow::new();
    row.set_child(Some(&content));
    row.set_focusable(false);
    row.set_tooltip_text(Some(&suggestion.text));
    row.update_property(&[gtk::accessible::Property::Label(&suggestion.name)]);
    row
}

fn connect_changed(completion: &Rc<LocationCompletion>) {
    let this = Rc::downgrade(completion);
    completion.entry.connect_changed(move |_| {
        if let Some(completion) = this.upgrade() {
            if !completion.quiet.get() {
                completion.request();
            }
        }
    });
}

fn connect_keys(completion: &Rc<LocationCompletion>) {
    let keys = gtk::EventControllerKey::new();
    // Capture phase, so these keys are handled before the entry's own Escape
    // controller and before Tab moves focus.
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let this = Rc::downgrade(completion);
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let Some(completion) = this.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if !completion.popover.is_visible() {
            return glib::Propagation::Proceed;
        }
        let plain = !modifiers.intersects(
            gtk::gdk::ModifierType::CONTROL_MASK
                | gtk::gdk::ModifierType::ALT_MASK
                | gtk::gdk::ModifierType::SUPER_MASK,
        );
        match key {
            gtk::gdk::Key::Escape => {
                completion.dismiss();
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Down if plain => {
                completion.step(1);
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Up if plain => {
                completion.step(-1);
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter if plain => {
                match completion.list.selected_row() {
                    Some(row) => {
                        completion.accept(row.index().max(0) as usize, true);
                        glib::Propagation::Stop
                    }
                    None => glib::Propagation::Proceed,
                }
            }
            gtk::gdk::Key::Tab if plain => match completion.list.selected_row() {
                Some(row) => {
                    completion.accept(row.index().max(0) as usize, false);
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            },
            _ => glib::Propagation::Proceed,
        }
    });
    completion.entry.add_controller(keys);
}

fn connect_focus(completion: &Rc<LocationCompletion>) {
    let focus = gtk::EventControllerFocus::new();
    let this = Rc::downgrade(completion);
    focus.connect_leave(move |_| {
        let Some(completion) = this.upgrade() else {
            return;
        };
        // Defer so a click on a suggestion has a chance to activate it first.
        let this = Rc::downgrade(&completion);
        glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
            if let Some(completion) = this.upgrade() {
                if !completion.entry.has_focus() {
                    completion.dismiss();
                }
            }
        });
    });
    completion.entry.add_controller(focus);
}

fn connect_rows(completion: &Rc<LocationCompletion>) {
    let this = Rc::downgrade(completion);
    completion.list.connect_row_activated(move |_, row| {
        if let Some(completion) = this.upgrade() {
            completion.accept(row.index().max(0) as usize, true);
        }
    });
}

fn connect_selected(completion: &Rc<LocationCompletion>) {
    let this = Rc::downgrade(completion);
    completion.list.connect_row_selected(move |_, row| {
        let Some(completion) = this.upgrade() else {
            return;
        };
        // A pointer selection completes immediately, so a slow click cannot
        // lose its row to a focus change. Arrow keys select programmatically
        // and are handled by the entry's key controller instead.
        if completion.selecting.get() {
            return;
        }
        if let Some(row) = row {
            completion.accept(row.index().max(0) as usize, true);
        }
    });
}

fn connect_closed(completion: &Rc<LocationCompletion>) {
    let this = Rc::downgrade(completion);
    completion.popover.connect_closed(move |_| {
        if let Some(completion) = this.upgrade() {
            completion.clear();
        }
    });
}

impl Drop for LocationCompletion {
    fn drop(&mut self) {
        self.cancel_pending();
        self.popover.unparent();
    }
}

#[cfg(test)]
mod tests {
    use super::CompletionTarget;
    use gio::prelude::*;

    #[test]
    fn targets_split_a_local_path_into_folder_and_prefix() {
        let target = CompletionTarget::parse("/home/user/Doc").unwrap();
        assert_eq!(target.directory_text, "/home/user/");
        assert_eq!(target.prefix, "Doc");
        assert!(!target.uri_style);
        assert_eq!(
            target.directory.path().unwrap(),
            std::path::Path::new("/home/user")
        );

        let target = CompletionTarget::parse("/home/user/").unwrap();
        assert_eq!(target.directory_text, "/home/user/");
        assert_eq!(target.prefix, "");
    }

    #[test]
    fn targets_keep_a_uri_spelling_intact() {
        let target = CompletionTarget::parse("file:///home/user/Doc").unwrap();
        assert_eq!(target.directory_text, "file:///home/user/");
        assert_eq!(target.prefix, "Doc");
        assert!(target.uri_style);
        assert_eq!(target.directory.uri(), "file:///home/user");

        let target = CompletionTarget::parse("smb://host/share/Do").unwrap();
        assert_eq!(target.directory_text, "smb://host/share/");
        assert_eq!(target.prefix, "Do");
        assert!(target.uri_style);
    }

    #[test]
    fn unsupported_text_has_no_target() {
        for input in [
            "",
            "relative/path",
            "smb://host",
            "file:///tmp/na?me",
            "/tmp/na\nme",
        ] {
            assert!(
                CompletionTarget::parse(input).is_none(),
                "{input:?} must not produce completions"
            );
        }
        // A trailing slash asks for everything in that folder.
        assert!(CompletionTarget::parse("smb://host/").is_some());
        assert!(CompletionTarget::parse("/").is_some());
    }

    #[test]
    fn matching_is_case_insensitive_and_hides_dotfiles() {
        let target = CompletionTarget::parse("/tmp/doc").unwrap();
        assert!(target.matches("Documents"));
        assert!(target.matches("documents"));
        assert!(!target.matches(".documents"));

        let hidden = CompletionTarget::parse("/tmp/.doc").unwrap();
        assert!(hidden.matches(".documents"));

        let encoded = CompletionTarget::parse("file:///tmp/My%20Doc").unwrap();
        assert_eq!(encoded.prefix, "My Doc");
        assert!(encoded.matches("My Documents"));
    }

    #[test]
    fn suggestions_append_a_separator_and_escape_uri_segments() {
        let target = CompletionTarget::parse("/home/user/Doc").unwrap();
        assert_eq!(target.suggestion("Documents").text, "/home/user/Documents/");

        let target = CompletionTarget::parse("file:///home/user/Doc").unwrap();
        assert_eq!(
            target.suggestion("My Documents").text,
            "file:///home/user/My%20Documents/"
        );
    }

    #[gtk::test]
    fn typing_lists_matching_folders_and_enter_accepts_one() {
        use gtk::prelude::*;
        use std::time::{Duration, Instant};

        let root = tempfile::tempdir().unwrap();
        for name in ["Documents", "Downloads", "Pictures"] {
            std::fs::create_dir(root.path().join(name)).unwrap();
        }

        let entry = gtk::Entry::new();
        let window = gtk::Window::new();
        window.set_child(Some(&entry));
        window.present();
        let completion = super::LocationCompletion::new(&entry);

        let context = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !entry.is_mapped() && Instant::now() < deadline {
            context.iteration(false);
        }
        assert!(
            entry.is_mapped(),
            "the entry must be mapped to show a popover"
        );

        entry.grab_focus();
        entry.set_text(&format!("{}/Do", root.path().display()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !completion.popover.is_visible() && Instant::now() < deadline {
            context.iteration(false);
        }
        assert!(
            completion.popover.is_visible(),
            "typing a folder prefix must open the suggestion popover"
        );
        let names = completion
            .suggestions
            .borrow()
            .iter()
            .map(|suggestion| suggestion.name.clone())
            .collect::<Vec<_>>();
        assert_eq!(names, ["Documents", "Downloads"]);

        // The entry must keep focus while the popover is open: typing narrows
        // the list, and a focus-driven dismissal would close it immediately.
        let settle = Instant::now() + Duration::from_millis(400);
        while Instant::now() < settle {
            context.iteration(false);
        }
        assert!(
            completion.popover.is_visible(),
            "suggestions must stay open while the entry keeps focus"
        );

        // Accepting with Enter fills the entry and asks the location handler to
        // act on it, exactly as a click on the row would.
        let controllers = entry.observe_controllers();
        let keys = (0..controllers.n_items())
            .filter_map(|index| {
                controllers
                    .item(index)
                    .and_downcast::<gtk::EventControllerKey>()
            })
            .next()
            .expect("the completion installs a key controller");
        keys.emit_by_name::<bool>(
            "key-pressed",
            &[
                &gtk::gdk::Key::Return,
                &0u32,
                &gtk::gdk::ModifierType::empty(),
            ],
        );
        assert_eq!(
            entry.text().as_str(),
            format!("{}/Documents/", root.path().display())
        );
        window.close();
    }
}
