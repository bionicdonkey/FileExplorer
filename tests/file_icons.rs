//! A listing carries no content types, so every row starts on the generic
//! fallback icon and is promoted when its metadata arrives. That promotion
//! must reach small folders and the icon grid, not only large paged lists.
use gtk::prelude::*;
use lorin_explorer::{
    core::{Location, ViewMode, ViewOptions},
    ui::file_view::FileView,
};

fn icon_names(widget: &gtk::Widget, out: &mut Vec<String>) {
    if let Some(image) = widget.downcast_ref::<gtk::Image>() {
        if let Some(icon) = image.gicon() {
            out.push(icon.to_string().map(|s| s.to_string()).unwrap_or_default());
        }
    }
    let mut child = widget.first_child();
    while let Some(next) = child {
        icon_names(&next, out);
        child = next.next_sibling();
    }
}

fn icons_settle_to_their_types(mode: ViewMode, thumbnails: bool) {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "notes.txt",
        "photo.png",
        "paper.pdf",
        "run.sh",
        "bundle.zip",
    ] {
        std::fs::write(dir.path().join(name), "x").unwrap();
    }
    let view = FileView::from_location(Location::from_local_path(dir.path()));
    view.set_options(ViewOptions {
        view_mode: mode,
        thumbnails,
        ..Default::default()
    });
    let window = gtk::Window::builder()
        .default_width(900)
        .default_height(600)
        .child(&view.widget)
        .build();
    window.present();
    let context = glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let wanted = [
        "text-plain",
        "image-png",
        "application-pdf",
        "application-zip",
    ];
    loop {
        context.iteration(false);
        let mut names = Vec::new();
        icon_names(view.widget.upcast_ref(), &mut names);
        let has = |needle: &str| names.iter().any(|name| name.contains(needle));
        if wanted.iter().all(|needle| has(needle)) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{mode:?} icons never matched their file types: {names:?}"
        );
    }
    view.close();
    window.close();
}

#[gtk::test]
fn small_list_shows_type_icons() {
    icons_settle_to_their_types(ViewMode::DetailedList, true);
}

#[gtk::test]
fn small_icon_grid_shows_type_icons() {
    icons_settle_to_their_types(ViewMode::IconGrid, true);
}

#[gtk::test]
fn small_icon_grid_shows_type_icons_with_thumbnails_off() {
    icons_settle_to_their_types(ViewMode::IconGrid, false);
}
