//! Cursor and in-progress input-method text.

use super::text::TextPainter;
use super::{col_x, row_span, snap_to_pixel, to_nscolor};
use crate::mac_app::render::glyphs::{GlyphCache, TextAttributes};
use crate::mac_app::shell::TerminalViewState;
use crate::terminal::grid::{CursorInfo, CursorShape, GridView, display_cells};
use objc2_app_kit::{NSBezierPath, NSColor};
use objc2_core_graphics::CGContext;
use objc2_foundation::{NSPoint, NSRect, NSSize};

pub(super) fn paint_cursor(
    state: &TerminalViewState,
    glyphs: &mut GlyphCache,
    attributes: &TextAttributes,
    context: Option<&CGContext>,
    cursor: &CursorInfo,
    bounds: NSRect,
    scale: f64,
) {
    let origin = bounds.origin;
    // A wide character is covered by two columns, so a block cursor over it has
    // to be two columns wide: one column would hide only half the character.
    let cursor_cols = if cursor.wide { 2 } else { 1 };
    let cursor_x0 = col_x(origin.x, cursor.col, state.cell_width, scale);
    let cursor_x1 = col_x(origin.x, cursor.col + cursor_cols, state.cell_width, scale);
    let (cursor_y0, cursor_y1) = row_span(
        origin.y,
        bounds.size.height,
        cursor.row,
        state.cell_height,
        scale,
    );
    let cell_w = (cursor_x1 - cursor_x0).max(0.0);
    let cell_h = (cursor_y1 - cursor_y0).max(0.0);
    to_nscolor(cursor.bg).set();

    match cursor.shape {
        CursorShape::Block => {
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(cursor_x0, cursor_y0),
                NSSize::new(cell_w, cell_h),
            ));
            if !cursor.text.is_empty() {
                let font_height = state.font.ascender() - state.font.descender();
                let text_y = snap_to_pixel(cursor_y0 + (cell_h - font_height) / 2.0, scale);
                to_nscolor(cursor.fg).set();
                TextPainter {
                    state,
                    glyphs,
                    attributes,
                    context,
                    scale,
                    origin_x: origin.x,
                    text_y,
                }
                .draw(&cursor.text, cursor.col);
            }
        }
        CursorShape::Beam => {
            // The bar marks the leading edge of the cell, so it stays one cell
            // thick even when the cursor covers a wide character.
            let bar_width = snap_to_pixel((state.cell_width / 6.0).max(1.0), scale);
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(cursor_x0, cursor_y0),
                NSSize::new(bar_width, cell_h),
            ));
        }
        CursorShape::Underline => {
            let bar_height = snap_to_pixel((cell_h / 8.0).max(1.0), scale);
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(cursor_x0, cursor_y0),
                NSSize::new(cell_w, bar_height),
            ));
        }
        CursorShape::HollowBlock => {
            let thickness = snap_to_pixel(1.0, scale);
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(cursor_x0, cursor_y0),
                NSSize::new(cell_w, thickness),
            ));
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(cursor_x0, cursor_y1 - thickness),
                NSSize::new(cell_w, thickness),
            ));
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(cursor_x0, cursor_y0),
                NSSize::new(thickness, cell_h),
            ));
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(cursor_x1 - thickness, cursor_y0),
                NSSize::new(thickness, cell_h),
            ));
        }
    }
}

/// Draw the composition text at the cursor with an underline.
pub(super) fn paint_marked_text(
    state: &TerminalViewState,
    glyphs: &mut GlyphCache,
    attributes: &TextAttributes,
    context: Option<&CGContext>,
    bounds: NSRect,
    scale: f64,
) {
    let origin = bounds.origin;
    let (cursor_row, cursor_col) = state.active().frame().cursor_pos();
    let (marked_y0, marked_y1) = row_span(
        origin.y,
        bounds.size.height,
        cursor_row,
        state.cell_height,
        scale,
    );
    let marked_rect_h = (marked_y1 - marked_y0).max(0.0);
    let font_height = state.font.ascender() - state.font.descender();
    let marked_text_y = snap_to_pixel(marked_y0 + (marked_rect_h - font_height) / 2.0, scale);
    let marked_fg = NSColor::colorWithSRGBRed_green_blue_alpha(0.7, 0.7, 0.7, 1.0);

    // Draw the composition the same way the grid rows are drawn: each glyph
    // anchored to its column, so composing CJK cannot slide away from the
    // cursor as the preedit grows. Combining marks stay on the cell they mark.
    let marked = state.marked_text();
    marked_fg.set();
    TextPainter {
        state,
        glyphs,
        attributes,
        context,
        scale,
        origin_x: origin.x,
        text_y: marked_text_y,
    }
    .draw(marked, cursor_col);
    let marked_cols = marked.chars().map(display_cells).sum::<usize>().max(1);
    let marked_width = marked_cols as f64 * state.cell_width;
    let underline_rect = NSRect::new(
        NSPoint::new(
            col_x(origin.x, cursor_col, state.cell_width, scale),
            marked_y0,
        ),
        NSSize::new(marked_width, 1.0),
    );
    marked_fg.set();
    NSBezierPath::fillRect(underline_rect);
}
