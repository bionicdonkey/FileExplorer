pub const BASE_WINDOW_WIDTH: i32 = 1280;
pub const PREFERRED_TAB_WIDTH: i32 = 176;
pub const MIN_TAB_WIDTH: i32 = 110;
pub const MONITOR_MARGIN: i32 = 32;

const FIXED_TITLEBAR_WIDTH: i32 = 176;
const TAB_BAR_EDGE_ALLOWANCE: i32 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabLayoutInput {
    pub page_count: i32,
    pub current_window_width: i32,
    pub monitor_width: i32,
    pub maximized: bool,
    pub fullscreen: bool,
}

impl TabLayoutInput {
    pub fn normal(page_count: i32, current_window_width: i32, monitor_width: i32) -> Self {
        Self {
            page_count,
            current_window_width,
            monitor_width,
            maximized: false,
            fullscreen: false,
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
    let monitor_limit = (input.monitor_width - MONITOR_MARGIN).max(MIN_TAB_WIDTH);
    let desired_width = BASE_WINDOW_WIDTH
        .saturating_add((page_count - 1).saturating_mul(PREFERRED_TAB_WIDTH));
    let window_width = if input.maximized || input.fullscreen {
        input.current_window_width
    } else {
        desired_width.min(monitor_limit)
    };

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
    fn normal_window_grows_by_one_preferred_tab_width() {
        let layout = calculate_tab_layout(TabLayoutInput::normal(2, 1280, 1920));

        assert_eq!(layout.window_width, 1456);
        assert_eq!(layout.tab_width, 176);
        assert!(!layout.overflowing);
    }

    #[test]
    fn growth_stops_at_monitor_safety_limit() {
        let layout = calculate_tab_layout(TabLayoutInput::normal(5, 1808, 1920));

        assert_eq!(layout.window_width, 1888);
    }

    #[test]
    fn tabs_compress_before_they_overflow() {
        let layout = calculate_tab_layout(TabLayoutInput::normal(12, 1888, 1920));

        assert!(layout.tab_width < 176);
        assert!(layout.tab_width >= 110);
        assert!(!layout.overflowing);
    }

    #[test]
    fn tabs_scroll_after_reaching_the_minimum_width() {
        let layout = calculate_tab_layout(TabLayoutInput::normal(20, 1888, 1920));

        assert_eq!(layout.tab_width, 110);
        assert!(layout.overflowing);
    }

    #[test]
    fn maximized_window_keeps_its_current_width() {
        let input = TabLayoutInput {
            maximized: true,
            ..TabLayoutInput::normal(4, 1600, 1920)
        };

        assert_eq!(calculate_tab_layout(input).window_width, 1600);
    }

    #[test]
    fn closing_tabs_does_not_shrink_below_the_initial_width() {
        let layout = calculate_tab_layout(TabLayoutInput::normal(1, 1456, 1920));

        assert_eq!(layout.window_width, 1280);
    }
}
