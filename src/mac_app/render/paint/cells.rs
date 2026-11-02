//! Cell runs: background, glyphs, underlines.

use super::text::TextPainter;
use super::{col_x, row_span, snap_to_pixel, to_nscolor};
use crate::mac_app::render::glyphs::{GlyphCache, TextAttributes};
use crate::mac_app::shell::TerminalViewState;
use crate::terminal::grid::GridView;
use objc2_app_kit::NSBezierPath;
use objc2_core_graphics::CGContext;
use objc2_foundation::{NSPoint, NSRect, NSSize};

/// Paint one run per same-styled group of cells.
///
/// Every glyph is anchored to the columns its characters occupy, never to the
/// advance the font reports: a face the system falls back to (CJK, emoji,
/// symbol fonts) advances by its own metrics, which is not the number of
/// columns the grid reserved, so one span of text would slide out of step with
/// the grid the further right it reaches.
pub(super) fn paint_cells(
    state: &TerminalViewState,
    glyphs: &mut GlyphCache,
    attributes: &TextAttributes,
    context: Option<&CGContext>,
    bounds: NSRect,
    scale: f64,
) {
    let origin = bounds.origin;
    let session = state.active();
    let default_bg = session.frame().default_bg();
    let font_height = state.font.ascender() - state.font.descender();

    for row in 0..session.frame().rows() {
        let (y0, y1) = row_span(origin.y, bounds.size.height, row, state.cell_height, scale);
        let rect_h = (y1 - y0).max(0.0);
        // Top of the line box; glyphs hang from the baseline below it.
        let text_y = snap_to_pixel(y0 + (rect_h - font_height) / 2.0, scale);

        for run in session.frame().row_runs(row) {
            let x0 = col_x(origin.x, run.col, state.cell_width, scale);
            let x1 = col_x(origin.x, run.col + run.width, state.cell_width, scale);
            let rect_w = (x1 - x0).max(0.0);
            let rect = NSRect::new(NSPoint::new(x0, y0), NSSize::new(rect_w, rect_h));

            if run.bg != default_bg {
                to_nscolor(run.bg).set();
                NSBezierPath::fillRect(rect);
            }

            if !run.text.trim().is_empty() {
                to_nscolor(run.fg).set();
                TextPainter {
                    state,
                    glyphs,
                    attributes,
                    context,
                    scale,
                    origin_x: origin.x,
                    text_y,
                }
                .draw(&run.text, run.col);
            }

            if run.underline {
                // y0 is the cell bottom in NSView coords (y-up), so the underline sits there.
                let underline_rect = NSRect::new(NSPoint::new(x0, y0), NSSize::new(rect_w, 1.0));
                to_nscolor(run.fg).set();
                NSBezierPath::fillRect(underline_rect);
            }
        }
    }
}
