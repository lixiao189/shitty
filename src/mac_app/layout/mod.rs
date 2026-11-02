//! Layout math shared by the terminal surface, the tab strip, and resizing.

use objc2_foundation::{NSRect, NSSize};

/// Height of the tab strip drawn above the terminal content.
pub(in crate::mac_app) const TAB_BAR_HEIGHT: f64 = 30.0;

fn tab_strip_height(show_tab_bar: bool) -> f64 {
    if show_tab_bar { TAB_BAR_HEIGHT } else { 0.0 }
}

/// Terminal content rect: the view minus the tab strip, when shown.
pub(in crate::mac_app) fn terminal_bounds(bounds: NSRect, show_tab_bar: bool) -> NSRect {
    let bar_height = tab_strip_height(show_tab_bar);
    NSRect::new(
        bounds.origin,
        NSSize::new(
            bounds.size.width,
            (bounds.size.height - bar_height).max(0.0),
        ),
    )
}

/// The grid dimensions that fit a view of `size`.
pub(in crate::mac_app) fn grid_size_for(
    size: NSSize,
    cell_width: f64,
    cell_height: f64,
    show_tab_bar: bool,
) -> (usize, usize) {
    let cols = ((size.width / cell_width).floor() as usize).max(1);
    let terminal_height = (size.height - tab_strip_height(show_tab_bar)).max(cell_height);
    let rows = ((terminal_height / cell_height).floor() as usize).max(1);
    (cols, rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::NSPoint;

    fn bounds() -> NSRect {
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0))
    }

    #[test]
    fn hidden_tab_bar_gives_the_whole_view_to_the_terminal() {
        let size = NSSize::new(800.0, 600.0);
        assert_eq!(grid_size_for(size, 10.0, 20.0, false), (80, 30));
        assert_eq!(terminal_bounds(bounds(), false), bounds());
    }

    #[test]
    fn grid_size_uses_the_area_below_the_tab_bar() {
        assert_eq!(
            grid_size_for(NSSize::new(800.0, 600.0), 10.0, 20.0, true),
            (80, 28)
        );
    }

    #[test]
    fn terminal_bounds_leaves_room_for_the_tab_bar() {
        let terminal = terminal_bounds(bounds(), true);
        assert_eq!(terminal.origin.y, 0.0);
        assert_eq!(terminal.size.height, 600.0 - TAB_BAR_HEIGHT);
    }
}
