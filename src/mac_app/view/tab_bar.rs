//! The tab strip: its layout, hit testing, and painting.

use crate::terminal::color::Color32;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSBezierPath, NSFont, NSStringDrawing};
use objc2_foundation::{
    NSAttributedStringKey, NSMutableDictionary, NSPoint, NSRect, NSSize, NSString,
};

use super::geometry::TAB_BAR_HEIGHT;
use super::renderer::{text_attributes, to_nscolor};
use super::state::TerminalViewState;

/// Width of one tab in the tab strip.
const TAB_WIDTH: f64 = 170.0;
/// Click zone at the right edge of a tab that closes it.
const TAB_CLOSE_ZONE: f64 = 22.0;
/// Horizontal padding between a tab's edge and its label.
const TAB_TEXT_PADDING: f64 = 8.0;
/// Width of the "+" button drawn after the last tab.
const NEW_TAB_WIDTH: f64 = 30.0;
/// Point size of tab-strip labels.
const TAB_FONT_SIZE: f64 = 12.0;
/// Number of tabs reachable through ⌘1–⌘9; the hint is drawn for each.
const TAB_SHORTCUT_COUNT: usize = 9;
/// Height of the ⌘N keycap hint drawn on tabs with a shortcut.
const TAB_SHORTCUT_HEIGHT: f64 = 16.0;
/// Horizontal padding inside the ⌘N keycap.
const TAB_SHORTCUT_PADDING: f64 = 5.0;
/// Gap between the ⌘N keycap and the close button.
const TAB_SHORTCUT_MARGIN: f64 = 4.0;
/// Corner radius of the ⌘N keycap.
const TAB_SHORTCUT_RADIUS: f64 = 3.0;

const TAB_BAR_BG: Color32 = Color32::from_rgb(36, 36, 42);
const TAB_ACTIVE_BG: Color32 = Color32::from_rgb(18, 18, 22);
const TAB_SEPARATOR: Color32 = Color32::from_rgb(58, 58, 66);
const TAB_ACTIVE_ACCENT: Color32 = Color32::from_rgb(90, 140, 240);
const TAB_ACTIVE_FG: Color32 = Color32::from_rgb(225, 225, 230);
const TAB_INACTIVE_FG: Color32 = Color32::from_rgb(150, 150, 158);
const TAB_CLOSE_FG: Color32 = Color32::from_rgb(140, 140, 148);
const TAB_SHORTCUT_FG: Color32 = Color32::from_rgb(132, 132, 141);
const TAB_SHORTCUT_BG: Color32 = Color32::from_rgba(255, 255, 255, 18);

/// The strip at the top of the view that holds the tabs.
fn tab_bar_rect(bounds: NSRect) -> NSRect {
    NSRect::new(
        NSPoint::new(
            bounds.origin.x,
            bounds.origin.y + bounds.size.height - TAB_BAR_HEIGHT,
        ),
        NSSize::new(bounds.size.width, TAB_BAR_HEIGHT),
    )
}

fn tab_rect(index: usize, bounds: NSRect) -> NSRect {
    let bar = tab_bar_rect(bounds);
    NSRect::new(
        NSPoint::new(bar.origin.x + index as f64 * TAB_WIDTH, bar.origin.y),
        NSSize::new(TAB_WIDTH, bar.size.height),
    )
}

fn new_tab_rect(tab_count: usize, bounds: NSRect) -> NSRect {
    let bar = tab_bar_rect(bounds);
    NSRect::new(
        NSPoint::new(bar.origin.x + tab_count as f64 * TAB_WIDTH, bar.origin.y),
        NSSize::new(NEW_TAB_WIDTH, bar.size.height),
    )
}

/// Where a click inside the tab strip landed.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum TabBarHit {
    Tab(usize),
    Close(usize),
    NewTab,
}

/// Map a point (view coordinates) to a tab-strip control, if any.
/// A hidden strip (a single tab) never consumes clicks.
pub(super) fn tab_bar_hit(
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
    let index = (x / TAB_WIDTH).floor() as usize;
    if index < tab_count {
        if x >= (index as f64 + 1.0) * TAB_WIDTH - TAB_CLOSE_ZONE {
            return Some(TabBarHit::Close(index));
        }
        return Some(TabBarHit::Tab(index));
    }
    if x < tab_count as f64 * TAB_WIDTH + NEW_TAB_WIDTH {
        return Some(TabBarHit::NewTab);
    }
    None
}

/// Draw the whole strip: one cell per tab plus the "+" button.
pub(super) fn paint_tab_bar(state: &TerminalViewState, bounds: NSRect) {
    let bar = tab_bar_rect(bounds);
    if bar.size.height <= 0.0 {
        return;
    }
    to_nscolor(TAB_BAR_BG).set();
    NSBezierPath::fillRect(bar);

    let font = NSFont::systemFontOfSize(TAB_FONT_SIZE);
    let font_height = font.ascender() - font.descender();

    for (index, session) in state.sessions.iter().enumerate() {
        let rect = tab_rect(index, bounds);
        if rect.origin.x >= bounds.origin.x + bounds.size.width {
            break;
        }
        let is_active = index == state.active;
        to_nscolor(if is_active { TAB_ACTIVE_BG } else { TAB_BAR_BG }).set();
        NSBezierPath::fillRect(rect);

        if is_active {
            to_nscolor(TAB_ACTIVE_ACCENT).set();
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(rect.origin.x, rect.origin.y + rect.size.height - 2.0),
                NSSize::new(rect.size.width, 2.0),
            ));
        }

        to_nscolor(TAB_SEPARATOR).set();
        NSBezierPath::fillRect(NSRect::new(
            NSPoint::new(rect.origin.x + rect.size.width - 1.0, rect.origin.y),
            NSSize::new(1.0, rect.size.height),
        ));

        let text_color = if is_active {
            TAB_ACTIVE_FG
        } else {
            TAB_INACTIVE_FG
        };
        let attrs = text_attributes(&font, &to_nscolor(text_color));
        let shortcut_space = paint_tab_shortcut(index, rect, &font, font_height);
        let max_text_width =
            (rect.size.width - TAB_CLOSE_ZONE - 2.0 * TAB_TEXT_PADDING - shortcut_space).max(0.0);
        let title = truncate_title(session.display_title(), &attrs, max_text_width);
        let title_pos = NSPoint::new(
            rect.origin.x + TAB_TEXT_PADDING,
            rect.origin.y + (rect.size.height - font_height) / 2.0,
        );
        let ns_title = NSString::from_str(&title);
        unsafe { ns_title.drawAtPoint_withAttributes(title_pos, Some(&attrs)) };

        let close_attrs = text_attributes(&font, &to_nscolor(TAB_CLOSE_FG));
        let close = NSString::from_str("×");
        let close_size = unsafe { close.sizeWithAttributes(Some(&close_attrs)) };
        let close_pos = NSPoint::new(
            rect.origin.x + rect.size.width - TAB_CLOSE_ZONE
                + (TAB_CLOSE_ZONE - close_size.width) / 2.0,
            rect.origin.y + (rect.size.height - font_height) / 2.0,
        );
        unsafe { close.drawAtPoint_withAttributes(close_pos, Some(&close_attrs)) };
    }

    let plus_rect = new_tab_rect(state.sessions.len(), bounds);
    if plus_rect.origin.x < bounds.origin.x + bounds.size.width {
        let attrs = text_attributes(&font, &to_nscolor(TAB_INACTIVE_FG));
        let plus = NSString::from_str("+");
        let plus_size = unsafe { plus.sizeWithAttributes(Some(&attrs)) };
        let plus_pos = NSPoint::new(
            plus_rect.origin.x + (plus_rect.size.width - plus_size.width) / 2.0,
            plus_rect.origin.y + (plus_rect.size.height - font_height) / 2.0,
        );
        unsafe { plus.drawAtPoint_withAttributes(plus_pos, Some(&attrs)) };
    }

    // Separates the tab strip from the terminal below it.
    to_nscolor(TAB_SEPARATOR).set();
    NSBezierPath::fillRect(NSRect::new(
        NSPoint::new(bar.origin.x, bar.origin.y),
        NSSize::new(bar.size.width, 1.0),
    ));
}

/// Draw the ⌘N keycap hint at the trailing edge of a tab, just left of its
/// close button, and return the horizontal space it reserved. Tabs past the
/// ninth have no shortcut, so they reserve nothing.
fn paint_tab_shortcut(index: usize, rect: NSRect, font: &NSFont, font_height: f64) -> f64 {
    let Some(label) = tab_shortcut_label(index) else {
        return 0.0;
    };
    let attrs = text_attributes(font, &to_nscolor(TAB_SHORTCUT_FG));
    let label = NSString::from_str(&label);
    let size = unsafe { label.sizeWithAttributes(Some(&attrs)) };
    let width = size.width + 2.0 * TAB_SHORTCUT_PADDING;
    let badge = NSRect::new(
        NSPoint::new(
            rect.origin.x + rect.size.width - TAB_CLOSE_ZONE - TAB_SHORTCUT_MARGIN - width,
            rect.origin.y + (rect.size.height - TAB_SHORTCUT_HEIGHT) / 2.0,
        ),
        NSSize::new(width, TAB_SHORTCUT_HEIGHT),
    );
    to_nscolor(TAB_SHORTCUT_BG).set();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        badge,
        TAB_SHORTCUT_RADIUS,
        TAB_SHORTCUT_RADIUS,
    )
    .fill();
    let text_pos = NSPoint::new(
        badge.origin.x + (badge.size.width - size.width) / 2.0,
        badge.origin.y + (badge.size.height - font_height) / 2.0,
    );
    unsafe { label.drawAtPoint_withAttributes(text_pos, Some(&attrs)) };
    width + TAB_SHORTCUT_MARGIN
}

/// Label of the ⌘N hint for a tab index, or None past the ninth tab.
fn tab_shortcut_label(index: usize) -> Option<String> {
    (index < TAB_SHORTCUT_COUNT).then(|| format!("⌘{}", index + 1))
}

/// Shorten a tab title with an ellipsis so it fits `max_width`.
fn truncate_title(
    text: &str,
    attrs: &NSMutableDictionary<NSAttributedStringKey, AnyObject>,
    max_width: f64,
) -> String {
    let ns_text = NSString::from_str(text);
    if max_width <= 0.0 || unsafe { ns_text.sizeWithAttributes(Some(attrs)) }.width <= max_width {
        return text.to_owned();
    }
    let mut characters: Vec<char> = text.chars().collect();
    while !characters.is_empty() {
        characters.pop();
        let candidate: String = characters.iter().collect::<String>() + "…";
        let ns_candidate = NSString::from_str(&candidate);
        if unsafe { ns_candidate.sizeWithAttributes(Some(attrs)) }.width <= max_width {
            return candidate;
        }
    }
    "…".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> NSRect {
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0))
    }

    #[test]
    fn tab_bar_hit_maps_clicks_to_tabs_and_controls() {
        let bounds = bounds();
        let y = bounds.size.height - TAB_BAR_HEIGHT / 2.0;

        assert_eq!(
            tab_bar_hit(NSPoint::new(10.0, y), bounds, 2),
            Some(TabBarHit::Tab(0))
        );
        assert_eq!(
            tab_bar_hit(NSPoint::new(TAB_WIDTH + 10.0, y), bounds, 2),
            Some(TabBarHit::Tab(1))
        );
        assert_eq!(
            tab_bar_hit(NSPoint::new(TAB_WIDTH - 5.0, y), bounds, 2),
            Some(TabBarHit::Close(0))
        );
        assert_eq!(
            tab_bar_hit(NSPoint::new(2.0 * TAB_WIDTH + 5.0, y), bounds, 2),
            Some(TabBarHit::NewTab)
        );
        assert_eq!(tab_bar_hit(NSPoint::new(10.0, 10.0), bounds, 2), None);
        // A lone tab hides the strip, so its former area is terminal content.
        assert_eq!(tab_bar_hit(NSPoint::new(10.0, y), bounds, 1), None);
    }

    #[test]
    fn only_the_first_nine_tabs_have_shortcut_hints() {
        assert_eq!(tab_shortcut_label(0).as_deref(), Some("⌘1"));
        assert_eq!(tab_shortcut_label(8).as_deref(), Some("⌘9"));
        assert_eq!(tab_shortcut_label(9), None);
    }
}
