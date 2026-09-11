//! Painting of the terminal surface: cells, cursor, selection, and the
//! in-progress input-method text.

use crate::terminal::color::{Color32, SELECTION_BG};
use crate::terminal::grid::CursorInfo;
use crate::terminal::selection::Selection;
use crate::terminal::session::TerminalSession;
use alacritty_terminal::vte::ansi::CursorShape;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSStringDrawing,
};
use objc2_foundation::{
    NSAttributedStringKey, NSDictionary, NSMutableDictionary, NSPoint, NSRect, NSSize, NSString,
};

use super::geometry::terminal_bounds;
use super::state::TerminalViewState;
use crate::mac_app::cast_any_object;

/// Paint the active session: background, selection, cells, cursor, and the
/// marked text of an in-progress composition.
pub(super) fn paint_terminal(state: &TerminalViewState, bounds: NSRect, scale: f64) {
    let terminal = terminal_bounds(bounds, state.show_tab_bar());
    let session = state.active();

    to_nscolor(session.grid.default_bg()).set();
    NSBezierPath::fillRect(terminal);

    if let Some(selection) = &session.selection {
        paint_selection_overlay(
            session,
            selection,
            terminal,
            scale,
            state.cell_width,
            state.cell_height,
        );
    }

    paint_cells(state, terminal, scale);

    if let Some(cursor) = session.grid.cursor() {
        paint_cursor(state, &cursor, terminal, scale);
    }

    if state.has_marked_text() {
        paint_marked_text(state, terminal, scale);
    }
}

/// Paint one run per same-styled group of cells.
fn paint_cells(state: &TerminalViewState, bounds: NSRect, scale: f64) {
    let origin = bounds.origin;
    let session = state.active();
    let default_bg = session.grid.default_bg();

    for row in 0..session.grid.rows() {
        for run in session.grid.row_runs(row) {
            let x0 = snap_to_pixel(origin.x + run.col as f64 * state.cell_width, scale);
            let x1 = snap_to_pixel(
                origin.x + (run.col + run.width) as f64 * state.cell_width,
                scale,
            );
            let y0 = snap_to_pixel(
                origin.y + bounds.size.height - ((row + 1) as f64 * state.cell_height),
                scale,
            );
            let y1 = snap_to_pixel(
                origin.y + bounds.size.height - (row as f64 * state.cell_height),
                scale,
            );
            let rect_w = (x1 - x0).max(0.0);
            let rect_h = (y1 - y0).max(0.0);
            let rect = NSRect::new(NSPoint::new(x0, y0), NSSize::new(rect_w, rect_h));

            if run.bg != default_bg {
                to_nscolor(run.bg).set();
                NSBezierPath::fillRect(rect);
            }

            if !run.text.is_empty() && !run.text.trim().is_empty() {
                let font_height = state.font.ascender() - state.font.descender();
                let text_pos =
                    NSPoint::new(x0, snap_to_pixel(y0 + (rect_h - font_height) / 2.0, scale));
                let text = NSString::from_str(&run.text);
                let text_attrs = text_attributes(&state.font, &to_nscolor(run.fg));
                unsafe { text.drawAtPoint_withAttributes(text_pos, Some(&text_attrs)) };
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

/// Paint the cursor in the shape the shell selected with DECSCUSR.
fn paint_cursor(state: &TerminalViewState, cursor: &CursorInfo, bounds: NSRect, scale: f64) {
    let origin = bounds.origin;
    let cursor_x0 = snap_to_pixel(origin.x + cursor.col as f64 * state.cell_width, scale);
    let cursor_x1 = snap_to_pixel(origin.x + (cursor.col + 1) as f64 * state.cell_width, scale);
    let cursor_y0 = snap_to_pixel(
        origin.y + bounds.size.height - ((cursor.row + 1) as f64 * state.cell_height),
        scale,
    );
    let cursor_y1 = snap_to_pixel(
        origin.y + bounds.size.height - (cursor.row as f64 * state.cell_height),
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
                let text_pos = NSPoint::new(
                    cursor_x0,
                    snap_to_pixel(cursor_y0 + (cell_h - font_height) / 2.0, scale),
                );
                let text = NSString::from_str(&cursor.text);
                let text_attrs = text_attributes(&state.font, &to_nscolor(cursor.fg));
                unsafe { text.drawAtPoint_withAttributes(text_pos, Some(&text_attrs)) };
            }
        }
        CursorShape::Beam => {
            let bar_width = snap_to_pixel((cell_w / 6.0).max(1.0), scale);
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
        CursorShape::Hidden => {}
    }
}

/// Draw the composition text at the cursor with an underline.
fn paint_marked_text(state: &TerminalViewState, bounds: NSRect, scale: f64) {
    let origin = bounds.origin;
    let (cursor_row, cursor_col) = state.active().grid.cursor_pos();
    let marked_x0 = snap_to_pixel(origin.x + cursor_col as f64 * state.cell_width, scale);
    let marked_y0 = snap_to_pixel(
        origin.y + bounds.size.height - ((cursor_row + 1) as f64 * state.cell_height),
        scale,
    );
    let marked_y1 = snap_to_pixel(
        origin.y + bounds.size.height - (cursor_row as f64 * state.cell_height),
        scale,
    );
    let marked_rect_h = (marked_y1 - marked_y0).max(0.0);
    let font_height = state.font.ascender() - state.font.descender();
    let marked_text_pos = NSPoint::new(
        marked_x0,
        snap_to_pixel(marked_y0 + (marked_rect_h - font_height) / 2.0, scale),
    );
    let marked_fg = NSColor::colorWithSRGBRed_green_blue_alpha(0.7, 0.7, 0.7, 1.0);
    let marked_str = NSString::from_str(state.marked_text());
    let marked_attrs = text_attributes(&state.font, &marked_fg);
    unsafe { marked_str.drawAtPoint_withAttributes(marked_text_pos, Some(&marked_attrs)) };

    let underline_rect = NSRect::new(
        NSPoint::new(marked_x0, marked_y0),
        NSSize::new(
            (state.marked_text().len() as f64 * state.cell_width).max(state.cell_width),
            1.0,
        ),
    );
    marked_fg.set();
    NSBezierPath::fillRect(underline_rect);
}

/// Fill the cells covered by the selection, one horizontal span per row.
fn paint_selection_overlay(
    session: &TerminalSession,
    selection: &Selection,
    bounds: NSRect,
    scale: f64,
    cell_width: f64,
    cell_height: f64,
) {
    let color = NSColor::colorWithSRGBRed_green_blue_alpha(
        SELECTION_BG.r as f64 / 255.0,
        SELECTION_BG.g as f64 / 255.0,
        SELECTION_BG.b as f64 / 255.0,
        SELECTION_BG.a as f64 / 255.0,
    );
    color.set();
    for row in 0..session.grid.rows() {
        let mut col = 0;
        while col < session.grid.cols() {
            if selection.contains(row, col) {
                let start_col = col;
                while col < session.grid.cols() && selection.contains(row, col) {
                    col += 1;
                }
                let x0 = snap_to_pixel(bounds.origin.x + start_col as f64 * cell_width, scale);
                let x1 = snap_to_pixel(bounds.origin.x + col as f64 * cell_width, scale);
                let y0 = snap_to_pixel(
                    bounds.origin.y + bounds.size.height - ((row + 1) as f64 * cell_height),
                    scale,
                );
                let y1 = snap_to_pixel(
                    bounds.origin.y + bounds.size.height - (row as f64 * cell_height),
                    scale,
                );
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

/// Build the foreground-color + font attribute dictionary used by every text draw.
pub(super) fn text_attributes(
    font: &NSFont,
    color: &NSColor,
) -> Retained<NSMutableDictionary<NSAttributedStringKey, AnyObject>> {
    let fg_attrs: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> = unsafe {
        NSDictionary::dictionaryWithObject_forKey(
            cast_any_object(color),
            ProtocolObject::from_ref(NSForegroundColorAttributeName),
        )
    };
    let attrs = NSMutableDictionary::dictionaryWithDictionary(&fg_attrs);
    unsafe {
        attrs.setObject_forKey(
            cast_any_object(font),
            ProtocolObject::from_ref(NSFontAttributeName),
        )
    };
    attrs
}

/// Map a terminal color onto an sRGB AppKit color.
pub(super) fn to_nscolor(c: Color32) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        c.r as f64 / 255.0,
        c.g as f64 / 255.0,
        c.b as f64 / 255.0,
        c.a as f64 / 255.0,
    )
}

/// Snap a coordinate to the backing-store pixel grid so edges stay crisp.
pub(super) fn snap_to_pixel(value: f64, scale: f64) -> f64 {
    if scale <= 0.0 {
        return value;
    }
    (value * scale).round() / scale
}
