//! Tab-strip geometry and hit testing. Painting lives in [`super::paint`].

use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::mac_app::layout::TAB_BAR_HEIGHT;

/// Narrowest a tab may shrink to before tabs run off the strip.
pub(super) const MIN_TAB_WIDTH: f64 = 90.0;
/// Click zone at the leading edge of a tab that closes it.
pub(super) const TAB_CLOSE_ZONE: f64 = 26.0;
/// Width of the "+" button pinned to the trailing end of the strip.
pub(super) const NEW_TAB_WIDTH: f64 = 28.0;

/// The strip at the top of the view that holds the tabs.
pub(super) fn tab_bar_rect(bounds: NSRect) -> NSRect {
    NSRect::new(
        NSPoint::new(
            bounds.origin.x,
            bounds.origin.y + bounds.size.height - TAB_BAR_HEIGHT,
        ),
        NSSize::new(bounds.size.width, TAB_BAR_HEIGHT),
    )
}

/// Width available to the tabs themselves: the strip minus the "+" button.
pub(super) fn tabs_area_width(bounds: NSRect) -> f64 {
    (bounds.size.width - NEW_TAB_WIDTH).max(0.0)
}

/// Every tab gets an equal share of the strip, down to `MIN_TAB_WIDTH`.
pub(super) fn tab_width(bounds: NSRect, tab_count: usize) -> f64 {
    (tabs_area_width(bounds) / tab_count.max(1) as f64).max(MIN_TAB_WIDTH)
}

pub(super) fn tab_rect(index: usize, bounds: NSRect, tab_count: usize) -> NSRect {
    let bar = tab_bar_rect(bounds);
    let width = tab_width(bounds, tab_count);
    NSRect::new(
        NSPoint::new(bar.origin.x + index as f64 * width, bar.origin.y),
        NSSize::new(width, bar.size.height),
    )
}

pub(super) fn new_tab_rect(bounds: NSRect) -> NSRect {
    let bar = tab_bar_rect(bounds);
    NSRect::new(
        NSPoint::new(bar.origin.x + tabs_area_width(bounds), bar.origin.y),
        NSSize::new(bar.size.width - tabs_area_width(bounds), bar.size.height),
    )
}

/// Where a pointer inside the tab strip is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::mac_app) enum TabBarHit {
    Tab(usize),
    Close(usize),
    NewTab,
}

impl TabBarHit {
    /// The tab under the pointer, whether on its body or its close button.
    pub(super) fn tab(self) -> Option<usize> {
        match self {
            TabBarHit::Tab(index) | TabBarHit::Close(index) => Some(index),
            TabBarHit::NewTab => None,
        }
    }
}

/// Map a point (view coordinates) to a tab-strip control, if any.
/// A hidden strip (a single tab) never consumes clicks.
pub(in crate::mac_app) fn tab_bar_hit(
    location: NSPoint,
    bounds: NSRect,
    tab_count: usize,
) -> Option<TabBarHit> {
    if tab_count <= 1 {
        return None;
    }
    let bar = tab_bar_rect(bounds);
    if location.y < bar.origin.y || location.y > bar.origin.y + bar.size.height {
        return None;
    }
    let x = location.x - bar.origin.x;
    if x < 0.0 || x > bar.size.width {
        return None;
    }
    if x >= tabs_area_width(bounds) {
        return Some(TabBarHit::NewTab);
    }
    let width = tab_width(bounds, tab_count);
    let index = (x / width).floor() as usize;
    if index >= tab_count {
        return None;
    }
    if x - index as f64 * width < TAB_CLOSE_ZONE {
        return Some(TabBarHit::Close(index));
    }
    Some(TabBarHit::Tab(index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    fn bounds() -> NSRect {
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0))
    }

    #[test]
    fn tab_bar_hit_maps_clicks_to_tabs_and_controls() {
        let bounds = bounds();
        let y = bounds.size.height - TAB_BAR_HEIGHT / 2.0;
        let width = tab_width(bounds, 2);
        assert_eq!(width, (800.0 - NEW_TAB_WIDTH) / 2.0);

        assert_eq!(
            tab_bar_hit(NSPoint::new(TAB_CLOSE_ZONE + 10.0, y), bounds, 2),
            Some(TabBarHit::Tab(0))
        );
        assert_eq!(
            tab_bar_hit(NSPoint::new(width + TAB_CLOSE_ZONE + 10.0, y), bounds, 2),
            Some(TabBarHit::Tab(1))
        );
        assert_eq!(
            tab_bar_hit(NSPoint::new(width + 5.0, y), bounds, 2),
            Some(TabBarHit::Close(1))
        );
        assert_eq!(
            tab_bar_hit(NSPoint::new(800.0 - 5.0, y), bounds, 2),
            Some(TabBarHit::NewTab)
        );
        assert_eq!(tab_bar_hit(NSPoint::new(10.0, 10.0), bounds, 2), None);
        // A lone tab hides the strip, so its former area is terminal content.
        assert_eq!(tab_bar_hit(NSPoint::new(10.0, y), bounds, 1), None);
    }

    #[test]
    fn tabs_share_the_strip_until_they_reach_the_minimum_width() {
        let bounds = bounds();
        assert_eq!(tab_width(bounds, 4), (800.0 - NEW_TAB_WIDTH) / 4.0);
        assert_eq!(tab_width(bounds, 20), MIN_TAB_WIDTH);
        let y = bounds.size.height - TAB_BAR_HEIGHT / 2.0;
        // Tabs pushed under the "+" button are not reachable by clicks there.
        assert_eq!(
            tab_bar_hit(NSPoint::new(800.0 - 1.0, y), bounds, 20),
            Some(TabBarHit::NewTab)
        );
    }
}
