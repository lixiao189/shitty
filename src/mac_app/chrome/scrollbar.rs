//! Scrolling: the overlay scrollbar, wheel gestures, and history scrolling.

use crate::mac_app::render::snap_to_pixel;
use crate::mac_app::shell::TerminalViewState;
use crate::terminal::grid::GridView;
use crate::terminal::io::TerminalSession;
use objc2_app_kit::{NSBezierPath, NSColor};
use objc2_foundation::{NSPoint, NSRect, NSSize};

/// Width of the scrollbar drawn over the right edge of the view.
pub(in crate::mac_app) const SCROLLBAR_WIDTH: f64 = 12.0;
/// Minimum scrollbar thumb height in points.
const SCROLLBAR_MIN_THUMB: f64 = 24.0;
/// Horizontal inset of the thumb inside the scrollbar track.
const SCROLLBAR_INSET: f64 = 2.0;
/// Lines scrolled per notch of a non-precise (mouse wheel) event.
pub(in crate::mac_app) const WHEEL_NOTCH_LINES: f64 = 3.0;

impl TerminalViewState {
    /// Scroll the view by whole lines; positive moves toward older history.
    pub(in crate::mac_app) fn scroll_by_lines(&mut self, lines: isize) {
        let session = self.active_mut();
        if session.scroll_by(lines) {
            session.selection = None;
        }
    }

    /// Accumulate a scroll-wheel delta and apply whole lines. Returns whether
    /// the grid scrolled.
    pub(in crate::mac_app) fn scroll_by_wheel_delta(&mut self, delta: f64) -> bool {
        let session = self.active_mut();
        session.scroll_accum += delta;
        let lines = session.scroll_accum.trunc() as isize;
        if lines == 0 {
            return false;
        }
        session.scroll_accum -= lines as f64;
        if session.scroll_by(lines) {
            session.selection = None;
            return true;
        }
        false
    }

    /// Map a y position inside the scrollbar track to a scroll offset,
    /// keeping the thumb centered under the cursor.
    pub(in crate::mac_app) fn scroll_to_track_y(&mut self, y: f64, track: NSRect) {
        let Some(thumb) = scrollbar_thumb_for(self.active(), track) else {
            return;
        };
        let track_h = track.size.height.max(0.0);
        let travel = (track_h - thumb.size.height).max(0.0);
        let session = self.active_mut();
        let max_scroll = session.frame().max_scroll();
        let y = y - track.origin.y;
        let thumb_y = (y - thumb.size.height / 2.0).clamp(0.0, travel);
        let offset = if travel > 0.0 {
            (thumb_y / travel * max_scroll as f64).round() as usize
        } else {
            0
        };
        if session.scroll_to(offset) {
            session.selection = None;
        }
    }
}

/// The full-height scrollbar track rect along the right edge of the view.
pub(in crate::mac_app) fn scrollbar_track(bounds: NSRect) -> NSRect {
    NSRect::new(
        NSPoint::new(
            bounds.origin.x + bounds.size.width - SCROLLBAR_WIDTH,
            bounds.origin.y,
        ),
        NSSize::new(SCROLLBAR_WIDTH, bounds.size.height),
    )
}

/// The thumb rect for a session's scrollbar track, or None without history.
fn scrollbar_thumb_for(session: &TerminalSession, track: NSRect) -> Option<NSRect> {
    let grid = session.frame();
    let max_scroll = grid.max_scroll();
    if max_scroll == 0 {
        return None;
    }
    let rows = grid.rows();
    let content = max_scroll + rows;
    let track_h = track.size.height.max(0.0);
    let thumb_h = (rows as f64 / content as f64 * track_h)
        .max(SCROLLBAR_MIN_THUMB)
        .min(track_h);
    let travel = (track_h - thumb_h).max(0.0);
    // offset 0 (newest) puts the thumb at the bottom of the track.
    let thumb_y = grid.scroll_offset() as f64 / max_scroll as f64 * travel;
    let thumb_w = (track.size.width - 2.0 * SCROLLBAR_INSET).max(0.0);
    Some(NSRect::new(
        NSPoint::new(track.origin.x + SCROLLBAR_INSET, track.origin.y + thumb_y),
        NSSize::new(thumb_w, thumb_h),
    ))
}

/// Draw the track and thumb over the terminal content.
pub(in crate::mac_app) fn paint_scrollbar(session: &TerminalSession, bounds: NSRect, scale: f64) {
    let track = scrollbar_track(bounds);
    let Some(thumb) = scrollbar_thumb_for(session, track) else {
        return;
    };

    let track_color = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.05);
    track_color.set();
    NSBezierPath::fillRect(track);

    let thumb_x0 = snap_to_pixel(thumb.origin.x, scale);
    let thumb_y0 = snap_to_pixel(thumb.origin.y, scale);
    let thumb_rect = NSRect::new(
        NSPoint::new(thumb_x0, thumb_y0),
        NSSize::new(
            (thumb.origin.x + thumb.size.width - thumb_x0).max(0.0),
            (thumb.origin.y + thumb.size.height - thumb_y0).max(0.0),
        ),
    );
    let radius = (thumb_rect.size.width / 2.0).max(0.0);
    let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(thumb_rect, radius, radius);
    let thumb_color = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.28);
    thumb_color.set();
    path.fill();
}
