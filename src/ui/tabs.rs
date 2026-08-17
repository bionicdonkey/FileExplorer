use adw::{TabBar, TabView};
use gtk::prelude::*;

use crate::ui::tab_layout::TabLayout;

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
        bar.set_expand_tabs(true);
        bar.add_css_class("tab-strip");
        Self { bar, view }
    }

    pub fn set_layout(&self, layout: TabLayout) {
        self.bar.set_width_request(layout.tab_bar_width);
        self.bar.set_tooltip_text(if layout.overflowing {
            Some("Scroll to view more tabs")
        } else {
            None
        });
    }
}
