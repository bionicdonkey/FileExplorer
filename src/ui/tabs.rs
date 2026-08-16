use adw::{TabBar, TabView};
use gtk::prelude::*;

const TAB_WIDTH: i32 = 176;
const MAX_VISIBLE_TABS: i32 = 4;

fn tab_bar_width(page_count: i32) -> i32 {
    page_count.clamp(1, MAX_VISIBLE_TABS) * TAB_WIDTH
}

#[derive(Clone)]
pub struct TabStrip {
    pub bar: TabBar,
    pub view: TabView,
}

impl TabStrip {
    pub fn new() -> Self {
        let view = TabView::new();
        let bar = TabBar::new();
        bar.set_view(Some(&view));
        bar.set_autohide(false);
        bar.set_expand_tabs(false);
        bar.add_css_class("tab-strip");
        bar.set_width_request(tab_bar_width(view.n_pages()));

        let resizing_bar = bar.clone();
        view.connect_n_pages_notify(move |view| {
            resizing_bar.set_width_request(tab_bar_width(view.n_pages()));
        });

        Self { bar, view }
    }
}

#[cfg(test)]
mod tests {
    use super::tab_bar_width;

    #[test]
    fn tab_bar_width_tracks_visible_tabs_and_is_capped() {
        assert_eq!(tab_bar_width(0), 176);
        assert_eq!(tab_bar_width(1), 176);
        assert_eq!(tab_bar_width(2), 352);
        assert_eq!(tab_bar_width(4), 704);
        assert_eq!(tab_bar_width(5), 704);
    }
}
