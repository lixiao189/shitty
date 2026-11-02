//! Painting of the terminal surface: cells, cursor, selection, and the
//! in-progress input-method text.

use crate::mac_app::cast_any_object;
use crate::mac_app::layout::terminal_bounds;
use crate::mac_app::shell::TerminalViewState;
use crate::terminal::grid::GridView;
use crate::terminal::theme::Color32;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSGraphicsContext,
};
use objc2_foundation::{NSAttributedStringKey, NSMutableDictionary, NSRect};

mod cells;
mod cursor;
mod selection;
mod text;

/// Paint the active session: background, selection, cells, cursor, and the
/// marked text of an in-progress composition.
pub(in crate::mac_app) fn paint_terminal(state: &TerminalViewState, bounds: NSRect, scale: f64) {
    let terminal = terminal_bounds(bounds, state.show_tab_bar());
    let session = state.active();

    to_nscolor(session.frame().default_bg()).set();
    NSBezierPath::fillRect(terminal);

    if let Some(selection) = &session.selection {
        selection::paint_selection_overlay(
            session,
            selection,
            terminal,
            scale,
            state.cell_width,
            state.cell_height,
        );
    }

    // Text is drawn as glyphs, which need the context Core Text draws into.
    let context = NSGraphicsContext::currentContext().map(|context| context.CGContext());
    // One attribute set for the whole frame: shaping only needs the font, and
    // each run's color is set on the context as it is painted.
    let attributes = text_attributes(&state.font, &to_nscolor(Color32::WHITE));
    let mut glyphs = state.glyphs.borrow_mut();
    cells::paint_cells(
        state,
        &mut glyphs,
        &attributes,
        context.as_deref(),
        terminal,
        scale,
    );

    if let Some(cursor) = session.frame().cursor() {
        cursor::paint_cursor(
            state,
            &mut glyphs,
            &attributes,
            context.as_deref(),
            &cursor,
            terminal,
            scale,
        );
    }

    if state.has_marked_text() {
        cursor::paint_marked_text(
            state,
            &mut glyphs,
            &attributes,
            context.as_deref(),
            terminal,
            scale,
        );
    }
}

/// Pixel-snapped x of a column edge.
fn col_x(origin_x: f64, col: usize, cell_width: f64, scale: f64) -> f64 {
    snap_to_pixel(origin_x + col as f64 * cell_width, scale)
}

/// Pixel-snapped bottom and top of a cell row (AppKit y-up).
fn row_span(origin_y: f64, height: f64, row: usize, cell_height: f64, scale: f64) -> (f64, f64) {
    (
        snap_to_pixel(origin_y + height - ((row + 1) as f64 * cell_height), scale),
        snap_to_pixel(origin_y + height - (row as f64 * cell_height), scale),
    )
}

/// Map a terminal color onto an sRGB AppKit color.
pub(in crate::mac_app) fn to_nscolor(c: Color32) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        c.r as f64 / 255.0,
        c.g as f64 / 255.0,
        c.b as f64 / 255.0,
        c.a as f64 / 255.0,
    )
}

/// Snap a coordinate to the backing-store pixel grid so edges stay crisp.
pub(in crate::mac_app) fn snap_to_pixel(value: f64, scale: f64) -> f64 {
    if scale <= 0.0 {
        return value;
    }
    (value * scale).round() / scale
}

/// Build the foreground-color + font attribute dictionary used by every text draw.
pub(in crate::mac_app) fn text_attributes(
    font: &NSFont,
    color: &NSColor,
) -> Retained<NSMutableDictionary<NSAttributedStringKey, AnyObject>> {
    let attrs = NSMutableDictionary::new();
    unsafe {
        attrs.setObject_forKey(
            cast_any_object(color),
            ProtocolObject::from_ref(NSForegroundColorAttributeName),
        );
        attrs.setObject_forKey(
            cast_any_object(font),
            ProtocolObject::from_ref(NSFontAttributeName),
        );
    }
    attrs
}
