//! Selection highlight, one horizontal span per row.

use super::{col_x, row_span, to_nscolor};
use crate::terminal::grid::GridView;
use crate::terminal::io::TerminalSession;
use crate::terminal::selection::Selection;
use objc2_app_kit::NSBezierPath;
use objc2_foundation::{NSPoint, NSRect, NSSize};

pub(super) fn paint_selection_overlay(
    session: &TerminalSession,
    selection: &Selection,
    bounds: NSRect,
    scale: f64,
    cell_width: f64,
    cell_height: f64,
) {
    let color = to_nscolor(session.frame().selection_bg());
    color.set();
    for row in 0..session.frame().rows() {
        let mut col = 0;
        while col < session.frame().cols() {
            if selection.contains(row, col) {
                let start_col = col;
                while col < session.frame().cols() && selection.contains(row, col) {
                    col += 1;
                }
                let x0 = col_x(bounds.origin.x, start_col, cell_width, scale);
                let x1 = col_x(bounds.origin.x, col, cell_width, scale);
                let (y0, y1) =
                    row_span(bounds.origin.y, bounds.size.height, row, cell_height, scale);
                let rect = NSRect::new(
                    NSPoint::new(x0, y0),
                    NSSize::new((x1 - x0).max(0.0), (y1 - y0).max(0.0)),
                );
                NSBezierPath::fillRect(rect);
            } else {
                col += 1;
            }
        }
    }
}
