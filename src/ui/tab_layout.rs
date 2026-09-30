pub const PREFERRED_TAB_WIDTH: i32 = 176;
pub const MIN_TAB_WIDTH: i32 = 110;

// Includes the sidebar toggle, two overflow-arrow slots, the new-tab button,
// padding, and native window controls. Reserving it keeps the tab request
// below the existing title-bar allocation at the overflow threshold.
const FIXED_TITLEBAR_WIDTH: i32 = 272;
const TAB_BAR_EDGE_ALLOWANCE: i32 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabLayoutInput {
    pub page_count: i32,
    pub window_width: i32,
}

impl TabLayoutInput {
    pub fn new(page_count: i32, window_width: i32) -> Self {
        Self {
            page_count,
            window_width,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabLayout {
    pub window_width: i32,
    pub tab_bar_width: i32,
    pub tab_width: i32,
    pub overflowing: bool,
}

pub fn calculate_tab_layout(input: TabLayoutInput) -> TabLayout {
    let page_count = input.page_count.max(1);
    let window_width = input.window_width.max(MIN_TAB_WIDTH);

    let available_tab_space = (window_width - FIXED_TITLEBAR_WIDTH).max(MIN_TAB_WIDTH);
    let tab_content_space = (available_tab_space - TAB_BAR_EDGE_ALLOWANCE).max(MIN_TAB_WIDTH);
    let unconstrained_tab_width = tab_content_space / page_count;
    let overflowing = unconstrained_tab_width < MIN_TAB_WIDTH;
    let tab_width = unconstrained_tab_width.clamp(MIN_TAB_WIDTH, PREFERRED_TAB_WIDTH);
    let tab_bar_width = if overflowing {
        available_tab_space
    } else {
        (page_count * tab_width + TAB_BAR_EDGE_ALLOWANCE).min(available_tab_space)
    };

    TabLayout {
        window_width,
        tab_bar_width,
        tab_width,
        overflowing,
    }
}

#[cfg(test)]
mod tests {
    use super::{calculate_tab_layout, TabLayoutInput};

    #[test]
    fn normal_window_keeps_its_width_when_tabs_are_added() {
        let layout = calculate_tab_layout(TabLayoutInput::new(4, 1280));

        assert_eq!(layout.window_width, 1280);
        assert_eq!(layout.tab_width, 176);
        assert!(!layout.overflowing);
    }

    #[test]
    fn user_resized_window_width_is_preserved() {
        let layout = calculate_tab_layout(TabLayoutInput::new(8, 1540));

        assert_eq!(layout.window_width, 1540);
    }

    #[test]
    fn tabs_compress_then_overflow_inside_a_fixed_window() {
        let compressed = calculate_tab_layout(TabLayoutInput::new(8, 1280));

        assert!(compressed.tab_width < 176);
        assert!(compressed.tab_width >= 110);
        assert!(!compressed.overflowing);

        let overflow = calculate_tab_layout(TabLayoutInput::new(11, 1280));
        assert_eq!(overflow.window_width, 1280);
        assert_eq!(overflow.tab_width, 110);
        assert!(overflow.overflowing);
    }

    #[test]
    fn compact_window_width_is_preserved() {
        let layout = calculate_tab_layout(TabLayoutInput::new(11, 960));

        assert_eq!(layout.window_width, 960);
        assert!(layout.overflowing);
    }

    #[test]
    fn overflow_reserves_space_for_all_titlebar_controls() {
        let layout = calculate_tab_layout(TabLayoutInput::new(11, 1280));

        assert!(layout.overflowing);
        assert_eq!(layout.tab_bar_width, 1008);
    }

    #[test]
    fn every_tab_count_preserves_the_supplied_window_width() {
        for page_count in [1, 8, 11, 20] {
            assert_eq!(
                calculate_tab_layout(TabLayoutInput::new(page_count, 1280)).window_width,
                1280,
            );
        }
    }
}
