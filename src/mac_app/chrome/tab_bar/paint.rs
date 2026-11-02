//! The tab strip: its layout, hit testing, and painting.
//!
//! Styled after the native macOS window tabs (Terminal, Safari, Finder):
//! tabs share the strip's width equally, the selected tab takes the window
//! background while the others sit a shade darker, titles are centered, the
//! close button appears on the leading edge of the hovered tab, and the ⌘N
//! shortcut sits on the trailing edge. Colors come from the system palette so
//! the strip follows the light/dark appearance of the title bar above it.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSBezierPath, NSColor, NSFont,
    NSLineCapStyle, NSStringDrawing,
};
use objc2_foundation::{
    NSArray, NSAttributedStringKey, NSMutableDictionary, NSPoint, NSRect, NSSize, NSString,
};

use super::layout::{
    TAB_CLOSE_ZONE, TabBarHit, new_tab_rect, tab_bar_rect, tab_rect, tabs_area_width,
};
use crate::mac_app::render::text_attributes;
use crate::mac_app::shell::TerminalViewState;
use std::borrow::Cow;

/// Diameter of the hover circle behind the close glyph.
const TAB_CLOSE_DIAMETER: f64 = 16.0;
/// Half the length of each stroke of the close "×".
const TAB_CLOSE_ARM: f64 = 3.5;
/// Horizontal padding between a tab's edge and its trailing shortcut label.
const TAB_TEXT_PADDING: f64 = 8.0;
/// Half the length of each stroke of the "+".
const NEW_TAB_ARM: f64 = 5.0;
/// Point size of tab-strip labels.
const TAB_FONT_SIZE: f64 = 12.0;
/// Number of tabs reachable through ⌘1–⌘9; the hint is drawn for each.
const TAB_SHORTCUT_COUNT: usize = 9;

/// How far the unselected tabs are darkened from the window background.
const INACTIVE_SHADE_DARK: f64 = 0.35;
const INACTIVE_SHADE_LIGHT: f64 = 0.09;
/// How far a hovered, unselected tab is darkened (less than the others).
const HOVER_SHADE_DARK: f64 = 0.2;
const HOVER_SHADE_LIGHT: f64 = 0.04;
/// Opacity of the circle behind a hovered close or "+" button.
const BUTTON_HOVER_ALPHA: f64 = 0.12;

/// Whether the strip is being drawn for a dark appearance.
fn is_dark_appearance() -> bool {
    let names = unsafe { NSArray::from_slice(&[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]) };
    NSAppearance::currentDrawingAppearance()
        .bestMatchFromAppearancesWithNames(&names)
        .is_some_and(|name| unsafe { &*name == NSAppearanceNameDarkAqua })
}

/// The window background darkened by `fraction`, used for unselected tabs.
fn shaded_background(fraction: f64) -> Retained<NSColor> {
    let background = NSColor::windowBackgroundColor();
    background
        .blendedColorWithFraction_ofColor(fraction, &NSColor::blackColor())
        .unwrap_or(background)
}

fn fill_rect(rect: NSRect, color: &NSColor) {
    color.set();
    NSBezierPath::fillRect(rect);
}

/// Draw the whole strip: one cell per tab plus the "+" button.
pub(in crate::mac_app) fn paint_tab_bar(state: &TerminalViewState, bounds: NSRect) {
    let bar = tab_bar_rect(bounds);
    if bar.size.height <= 0.0 {
        return;
    }
    let dark = is_dark_appearance();
    let (inactive_shade, hover_shade) = if dark {
        (INACTIVE_SHADE_DARK, HOVER_SHADE_DARK)
    } else {
        (INACTIVE_SHADE_LIGHT, HOVER_SHADE_LIGHT)
    };
    let separator = NSColor::separatorColor();
    let hover = state.tab_bar_hover;
    let hovered_tab = hover.and_then(TabBarHit::tab);
    let tab_count = state.sessions.len();
    let tabs_right = bar.origin.x + tabs_area_width(bounds);

    fill_rect(bar, &shaded_background(inactive_shade));

    let font = NSFont::systemFontOfSize(TAB_FONT_SIZE);
    let font_height = font.ascender() - font.descender();

    for (index, session) in state.sessions.iter().enumerate() {
        let rect = tab_rect(index, bounds, tab_count);
        if rect.origin.x >= tabs_right {
            break;
        }
        let is_active = index == state.active;
        let is_hovered = hovered_tab == Some(index);
        if is_active {
            fill_rect(rect, &NSColor::windowBackgroundColor());
        } else if is_hovered {
            fill_rect(rect, &shaded_background(hover_shade));
        }

        // Dividers sit between unselected tabs; the selected tab's own edges
        // come from its lighter fill.
        let next_is_active = index + 1 == state.active;
        if index + 1 < tab_count && !is_active && !next_is_active {
            fill_rect(
                NSRect::new(
                    NSPoint::new(rect.origin.x + rect.size.width - 0.5, rect.origin.y + 6.0),
                    NSSize::new(1.0, rect.size.height - 12.0),
                ),
                &separator,
            );
        }

        let text_color = if is_active {
            NSColor::labelColor()
        } else {
            NSColor::secondaryLabelColor()
        };
        let attrs = text_attributes(&font, &text_color);
        let shortcut_width = paint_tab_shortcut(index, rect, &font, font_height);
        // Reserve the same inset on both sides so the title stays centered.
        let inset = TAB_CLOSE_ZONE.max(shortcut_width + TAB_TEXT_PADDING);
        let max_text_width = (rect.size.width - 2.0 * inset).max(0.0);
        let title = truncate_title(session.display_title(), &attrs, max_text_width);
        let ns_title = NSString::from_str(&title);
        let title_width = unsafe { ns_title.sizeWithAttributes(Some(&attrs)) }.width;
        let title_pos = NSPoint::new(
            rect.origin.x + (rect.size.width - title_width) / 2.0,
            rect.origin.y + (rect.size.height - font_height) / 2.0,
        );
        unsafe { ns_title.drawAtPoint_withAttributes(title_pos, Some(&attrs)) };

        if is_hovered {
            paint_close_button(rect, hover == Some(TabBarHit::Close(index)));
        }
    }

    let plus_rect = new_tab_rect(bounds);
    fill_rect(
        NSRect::new(
            NSPoint::new(plus_rect.origin.x - 0.5, plus_rect.origin.y + 6.0),
            NSSize::new(1.0, plus_rect.size.height - 12.0),
        ),
        &separator,
    );
    paint_new_tab_button(plus_rect, hover == Some(TabBarHit::NewTab));

    // Hairlines above and below separate the strip from the title bar and the
    // terminal; the selected tab stays open to the title bar like AppKit's.
    fill_rect(
        NSRect::new(bar.origin, NSSize::new(bar.size.width, 1.0)),
        &separator,
    );
    let top = bar.origin.y + bar.size.height - 1.0;
    let active = tab_rect(state.active, bounds, tab_count);
    for (x0, x1) in [
        (bar.origin.x, active.origin.x),
        (
            active.origin.x + active.size.width,
            bar.origin.x + bar.size.width,
        ),
    ] {
        if x1 > x0 {
            fill_rect(
                NSRect::new(NSPoint::new(x0, top), NSSize::new(x1 - x0, 1.0)),
                &separator,
            );
        }
    }
}

/// A round button face: a faint circle, drawn only while hovered.
fn paint_button_hover(center: NSPoint, diameter: f64) {
    NSColor::labelColor()
        .colorWithAlphaComponent(BUTTON_HOVER_ALPHA)
        .set();
    NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
        NSPoint::new(center.x - diameter / 2.0, center.y - diameter / 2.0),
        NSSize::new(diameter, diameter),
    ))
    .fill();
}

/// Stroke a glyph made of straight segments (the "×" and "+" icons).
fn stroke_icon(segments: &[(NSPoint, NSPoint)], color: &NSColor) {
    color.set();
    let path = NSBezierPath::bezierPath();
    path.setLineWidth(1.25);
    path.setLineCapStyle(NSLineCapStyle::Round);
    for &(from, to) in segments {
        path.moveToPoint(from);
        path.lineToPoint(to);
    }
    path.stroke();
}

/// The "×" on the leading edge of a hovered tab.
fn paint_close_button(rect: NSRect, button_hovered: bool) {
    let center = NSPoint::new(
        rect.origin.x + TAB_CLOSE_ZONE / 2.0 + 2.0,
        rect.origin.y + rect.size.height / 2.0,
    );
    if button_hovered {
        paint_button_hover(center, TAB_CLOSE_DIAMETER);
    }
    let a = TAB_CLOSE_ARM;
    stroke_icon(
        &[
            (
                NSPoint::new(center.x - a, center.y - a),
                NSPoint::new(center.x + a, center.y + a),
            ),
            (
                NSPoint::new(center.x - a, center.y + a),
                NSPoint::new(center.x + a, center.y - a),
            ),
        ],
        &NSColor::secondaryLabelColor(),
    );
}

/// The "+" button pinned to the trailing end of the strip.
fn paint_new_tab_button(rect: NSRect, hovered: bool) {
    let center = NSPoint::new(
        rect.origin.x + rect.size.width / 2.0,
        rect.origin.y + rect.size.height / 2.0,
    );
    if hovered {
        paint_button_hover(center, TAB_CLOSE_DIAMETER + 4.0);
    }
    let a = NEW_TAB_ARM;
    stroke_icon(
        &[
            (
                NSPoint::new(center.x - a, center.y),
                NSPoint::new(center.x + a, center.y),
            ),
            (
                NSPoint::new(center.x, center.y - a),
                NSPoint::new(center.x, center.y + a),
            ),
        ],
        &NSColor::secondaryLabelColor(),
    );
}

/// Draw the ⌘N hint at the trailing edge of a tab and return its width.
/// Tabs past the ninth have no shortcut, so they reserve nothing.
fn paint_tab_shortcut(index: usize, rect: NSRect, font: &NSFont, font_height: f64) -> f64 {
    let Some(label) = tab_shortcut_label(index) else {
        return 0.0;
    };
    let attrs = text_attributes(font, &NSColor::tertiaryLabelColor());
    let label = NSString::from_str(label);
    let size = unsafe { label.sizeWithAttributes(Some(&attrs)) };
    let text_pos = NSPoint::new(
        rect.origin.x + rect.size.width - TAB_TEXT_PADDING - size.width,
        rect.origin.y + (rect.size.height - font_height) / 2.0,
    );
    unsafe { label.drawAtPoint_withAttributes(text_pos, Some(&attrs)) };
    size.width
}

/// Label of the ⌘N hint for a tab index, or None past the ninth tab.
fn tab_shortcut_label(index: usize) -> Option<&'static str> {
    const LABELS: [&str; TAB_SHORTCUT_COUNT] =
        ["⌘1", "⌘2", "⌘3", "⌘4", "⌘5", "⌘6", "⌘7", "⌘8", "⌘9"];
    LABELS.get(index).copied()
}

/// Shorten a tab title with an ellipsis so it fits `max_width`.
fn truncate_title<'a>(
    text: &'a str,
    attrs: &NSMutableDictionary<NSAttributedStringKey, AnyObject>,
    max_width: f64,
) -> Cow<'a, str> {
    let ns_text = NSString::from_str(text);
    if max_width <= 0.0 || unsafe { ns_text.sizeWithAttributes(Some(attrs)) }.width <= max_width {
        return Cow::Borrowed(text);
    }
    let mut characters: Vec<char> = text.chars().collect();
    while !characters.is_empty() {
        characters.pop();
        let candidate: String = characters.iter().collect::<String>() + "…";
        let ns_candidate = NSString::from_str(&candidate);
        if unsafe { ns_candidate.sizeWithAttributes(Some(attrs)) }.width <= max_width {
            return Cow::Owned(candidate);
        }
    }
    Cow::Borrowed("…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_nine_tabs_have_shortcut_hints() {
        assert_eq!(tab_shortcut_label(0), Some("⌘1"));
        assert_eq!(tab_shortcut_label(8), Some("⌘9"));
        assert_eq!(tab_shortcut_label(9), None);
    }
}
