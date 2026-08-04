use crate::terminal::color::{Color32, SELECTION_BG};
use crate::terminal::grid::{CellPos, TerminalGrid};
use crate::terminal::keymap;
use crate::terminal::pty::PtyEvent;
use crate::terminal::resize::ResizeDebouncer;
use crate::terminal::selection::Selection;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Bool, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSEvent, NSEventModifierFlags, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSPasteboard, NSPasteboardTypeString, NSStringDrawing,
    NSTextInputClient, NSView,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSDictionary, NSMutableDictionary,
    NSNotFound, NSPoint, NSRange, NSRect, NSSize, NSString, NSTimer, NSUInteger,
};
use std::sync::mpsc::{Receiver, Sender};

use super::cast_any_object;

/// Refresh rate for the terminal rendering timer (in Hz).
const RENDER_REFRESH_RATE_HZ: f64 = 120.0;

/// Width of the scrollbar drawn over the right edge of the view.
const SCROLLBAR_WIDTH: f64 = 12.0;
/// Minimum scrollbar thumb height in points.
const SCROLLBAR_MIN_THUMB: f64 = 24.0;
/// Horizontal inset of the thumb inside the scrollbar track.
const SCROLLBAR_INSET: f64 = 2.0;
/// Lines scrolled per notch of a non-precise (mouse wheel) event.
const WHEEL_NOTCH_LINES: f64 = 3.0;

/// Rust-side state shared by the terminal view and its render timer.
pub(super) struct TerminalViewState {
    grid: TerminalGrid,
    rx_output: Receiver<Vec<u8>>,
    tx_input: Sender<PtyEvent>,
    font: Retained<NSFont>,
    cell_width: f64,
    cell_height: f64,
    timer: Option<Retained<NSTimer>>,
    marked_text: String,
    ime_composing: bool,
    had_composition: bool,
    skip_next_command: bool,
    selection: Option<Selection>,
    resize_debouncer: ResizeDebouncer,
    /// Fractional scroll-wheel delta waiting to be applied to the grid.
    scroll_accum: f64,
    /// Whether the scrollbar thumb is being dragged.
    scrollbar_dragging: bool,
}

impl TerminalViewState {
    pub(super) fn new(
        rx_output: Receiver<Vec<u8>>,
        tx_input: Sender<PtyEvent>,
        font: Retained<NSFont>,
        cell_width: f64,
        cell_height: f64,
    ) -> Self {
        Self {
            grid: TerminalGrid::new(80, 24),
            rx_output,
            tx_input,
            font,
            cell_width,
            cell_height,
            timer: None,
            marked_text: String::new(),
            ime_composing: false,
            had_composition: false,
            skip_next_command: false,
            selection: None,
            resize_debouncer: ResizeDebouncer::new(),
            scroll_accum: 0.0,
            scrollbar_dragging: false,
        }
    }

    fn send_input(&mut self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            // Any keyboard input snaps the view back to the live screen.
            self.grid.scroll_to(0);
            let _ = self.tx_input.send(PtyEvent::Input(bytes));
        }
    }

    /// Scroll the view by whole lines; positive moves toward older history.
    fn scroll_by_lines(&mut self, lines: isize) {
        if self.grid.scroll_by(lines) {
            self.selection = None;
        }
    }

    /// Map a y position inside the scrollbar track to a scroll offset,
    /// keeping the thumb centered under the cursor.
    fn scroll_to_track_y(&mut self, y: f64, track: NSRect) {
        let Some(thumb) = self.scrollbar_thumb(track) else {
            return;
        };
        let track_h = track.size.height.max(0.0);
        let travel = (track_h - thumb.size.height).max(0.0);
        let max_scroll = self.grid.max_scroll();
        let y = y - track.origin.y;
        let thumb_y = (y - thumb.size.height / 2.0).clamp(0.0, travel);
        let offset = if travel > 0.0 {
            (thumb_y / travel * max_scroll as f64).round() as usize
        } else {
            0
        };
        if self.grid.scroll_to(offset) {
            self.selection = None;
        }
    }

    /// The thumb rect for the given track, or None when there is no history.
    fn scrollbar_thumb(&self, track: NSRect) -> Option<NSRect> {
        let max_scroll = self.grid.max_scroll();
        if max_scroll == 0 {
            return None;
        }
        let rows = self.grid.rows();
        let content = max_scroll + rows;
        let track_h = track.size.height.max(0.0);
        let thumb_h = (rows as f64 / content as f64 * track_h)
            .max(SCROLLBAR_MIN_THUMB)
            .min(track_h);
        let travel = (track_h - thumb_h).max(0.0);
        // offset 0 (newest) puts the thumb at the bottom of the track.
        let thumb_y = self.grid.scroll_offset() as f64 / max_scroll as f64 * travel;
        let thumb_w = (track.size.width - 2.0 * SCROLLBAR_INSET).max(0.0);
        Some(NSRect::new(
            NSPoint::new(track.origin.x + SCROLLBAR_INSET, track.origin.y + thumb_y),
            NSSize::new(thumb_w, thumb_h),
        ))
    }

    fn send_resize(&mut self, cols: usize, rows: usize) {
        if self.resize_debouncer.mark_sent((cols, rows)) {
            let _ = self.tx_input.send(PtyEvent::Resize {
                cols: cols as u16,
                rows: rows as u16,
            });
        }
    }

    /// Copy/clear shortcuts that belong to the selection. Returns true when
    /// the event was consumed instead of being sent to the shell.
    fn handle_selection_key(&mut self, event: &NSEvent) -> bool {
        let characters = event
            .charactersIgnoringModifiers()
            .map(|s| s.to_string())
            .unwrap_or_default();

        if characters.eq_ignore_ascii_case("c")
            && event
                .modifierFlags()
                .contains(NSEventModifierFlags::Command)
            && self.selection.is_some()
        {
            self.copy_selection();
            return true;
        }

        if characters == "\u{1b}" && self.selection.is_some() {
            self.selection = None;
            return true;
        }

        false
    }

    fn copy_selection(&self) {
        let Some(selection) = &self.selection else {
            return;
        };
        let text = selection.selected_text(&self.grid);
        if text.is_empty() {
            return;
        }
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        let ns_text = NSString::from_str(&text);
        unsafe {
            pasteboard.setString_forType(&ns_text, NSPasteboardTypeString);
        }
    }

    fn cursor_cell_rect(&self, bounds: NSRect) -> NSRect {
        let origin = bounds.origin;
        let (cell_w, cell_h) = (self.cell_width, self.cell_height);
        let (cursor_row, cursor_col) = self.grid.cursor_pos();
        let x0 = origin.x + cursor_col as f64 * cell_w;
        let y0 = origin.y + bounds.size.height - ((cursor_row + 1) as f64 * cell_h);
        NSRect::new(NSPoint::new(x0, y0), NSSize::new(cell_w, cell_h))
    }

    fn marked_text_range(&self) -> NSRange {
        if self.marked_text.is_empty() {
            return NSRange::new(NSNotFound as NSUInteger, 0);
        }
        let ns = NSString::from_str(&self.marked_text);
        NSRange::new(0, ns.length())
    }
}

impl Drop for TerminalViewState {
    fn drop(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.invalidate();
        }
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = objc2::MainThreadOnly]
    #[ivars = TerminalViewIvars]
    pub(super) struct ShittyTerminalView;

    impl ShittyTerminalView {
        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, new_size: NSSize) {
            let _: () = unsafe { msg_send![super(self), setFrameSize: new_size] };
            self.record_grid_size(new_size);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };
            let location = self.convertPoint_fromView(event.locationInWindow(), None);
            let bounds = self.bounds();
            if state.grid.max_scroll() > 0
                && location.x >= bounds.origin.x + bounds.size.width - SCROLLBAR_WIDTH
            {
                state.scrollbar_dragging = true;
                state.scroll_to_track_y(location.y, scrollbar_track(bounds));
                self.setNeedsDisplay(true);
                return;
            }
            state.selection = Some(Selection::start(self.cell_at_event(event)));
            if let Some(window) = self.window() {
                window.makeFirstResponder(Some(self));
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };
            if state.scrollbar_dragging {
                let location = self.convertPoint_fromView(event.locationInWindow(), None);
                state.scroll_to_track_y(location.y, scrollbar_track(self.bounds()));
                self.setNeedsDisplay(true);
                return;
            }
            if let Some(selection) = state.selection.as_mut() {
                selection.set_head(self.cell_at_event(event));
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };
            state.scrollbar_dragging = false;
            if state.selection.as_ref().is_some_and(|s| s.is_empty()) {
                state.selection = None;
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };
            let mut delta = event.scrollingDeltaY();
            // Non-precise wheels report notches; scale them to whole lines.
            if !event.hasPreciseScrollingDeltas() {
                delta *= WHEEL_NOTCH_LINES;
            }
            state.scroll_accum += delta;
            let lines = state.scroll_accum.trunc() as isize;
            if lines != 0 {
                state.scroll_accum -= lines as f64;
                if state.grid.scroll_by(lines) {
                    state.selection = None;
                    self.setNeedsDisplay(true);
                }
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let state_ptr = self.ivars().state;
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                if state.handle_selection_key(event) {
                    self.setNeedsDisplay(true);
                    return;
                }
                state.had_composition = state.ime_composing;
                state.marked_text.clear();
                state.skip_next_command = false;
            }
            let events = NSArray::from_slice(&[event]);
            self.interpretKeyEvents(&events);
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let state_ptr = self.ivars().state;
            if state_ptr.is_null() {
                return;
            }
            let state = unsafe { &*state_ptr };

            let bounds = self.bounds();
            let origin = bounds.origin;
            let scale = self
                .window()
                .map(|window| window.backingScaleFactor())
                .unwrap_or(1.0);
            let default_bg = state.grid.default_bg();

            to_nscolor(default_bg).set();
            NSBezierPath::fillRect(bounds);

            if let Some(selection) = &state.selection {
                paint_selection_overlay(state, selection, bounds, scale);
            }

            for row in 0..state.grid.rows() {
                for run in state.grid.row_runs(row) {
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
                        let text_pos = NSPoint::new(
                            x0,
                            snap_to_pixel(y0 + (rect_h - font_height) / 2.0, scale),
                        );
                        let text = NSString::from_str(&run.text);
                        let text_attrs = text_attributes(&state.font, &to_nscolor(run.fg));
                        unsafe { text.drawAtPoint_withAttributes(text_pos, Some(&text_attrs)) };
                    }

                    if run.underline {
                        // y0 is the cell bottom in NSView coords (y-up), so the underline sits there.
                        let underline_rect =
                            NSRect::new(NSPoint::new(x0, y0), NSSize::new(rect_w, 1.0));
                        to_nscolor(run.fg).set();
                        NSBezierPath::fillRect(underline_rect);
                    }
                }
            }

            if let Some(cursor) = state.grid.cursor() {
                let cursor_x0 = snap_to_pixel(origin.x + cursor.col as f64 * state.cell_width, scale);
                let cursor_x1 = snap_to_pixel(
                    origin.x + (cursor.col + 1) as f64 * state.cell_width,
                    scale,
                );
                let cursor_y0 = snap_to_pixel(
                    origin.y + bounds.size.height - ((cursor.row + 1) as f64 * state.cell_height),
                    scale,
                );
                let cursor_y1 = snap_to_pixel(
                    origin.y + bounds.size.height - (cursor.row as f64 * state.cell_height),
                    scale,
                );
                let cursor_rect = NSRect::new(
                    NSPoint::new(cursor_x0, cursor_y0),
                    NSSize::new(
                        (cursor_x1 - cursor_x0).max(0.0),
                        (cursor_y1 - cursor_y0).max(0.0),
                    ),
                );
                let cursor_bg_color = to_nscolor(cursor.bg);
                let cursor_fg_color = to_nscolor(cursor.fg);
                cursor_bg_color.set();
                NSBezierPath::fillRect(cursor_rect);

                let cursor_rect_h = (cursor_y1 - cursor_y0).max(0.0);
                let font_height = state.font.ascender() - state.font.descender();
                let text_pos = NSPoint::new(
                    cursor_x0,
                    snap_to_pixel(cursor_y0 + (cursor_rect_h - font_height) / 2.0, scale),
                );
                let text = NSString::from_str(&cursor.text);
                let text_attrs = text_attributes(&state.font, &cursor_fg_color);
                unsafe { text.drawAtPoint_withAttributes(text_pos, Some(&text_attrs)) };
            }

            if !state.marked_text.is_empty() {
                let (cursor_row, cursor_col) = state.grid.cursor_pos();
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
                let marked_str = NSString::from_str(&state.marked_text);
                let marked_attrs = text_attributes(&state.font, &marked_fg);
                unsafe { marked_str.drawAtPoint_withAttributes(marked_text_pos, Some(&marked_attrs)) };

                let underline_rect = NSRect::new(
                    NSPoint::new(marked_x0, marked_y0),
                    NSSize::new(
                        (state.marked_text.len() as f64 * state.cell_width).max(state.cell_width),
                        1.0,
                    ),
                );
                marked_fg.set();
                NSBezierPath::fillRect(underline_rect);
            }

            if state.grid.max_scroll() > 0 {
                paint_scrollbar(state, bounds, scale);
            }
        }
    }

    unsafe impl NSTextInputClient for ShittyTerminalView {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text_replacement_range(&self, string: &AnyObject, _replacement_range: NSRange) {
            let Some(text) = ns_object_to_string(string) else {
                return;
            };
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };

            let had_composition = state.had_composition;
            state.marked_text.clear();
            state.ime_composing = false;
            state.had_composition = false;

            let mut input_bytes = Vec::new();
            keymap::append_input_from_text(&text, &mut input_bytes);
            state.send_input(input_bytes);

            if had_composition {
                state.skip_next_command = true;
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, selector: Sel) {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };

            if state.skip_next_command {
                state.skip_next_command = false;
                return;
            }

            // PageUp/PageDown scroll the local history instead of reaching
            // the shell.
            if let Ok(name) = selector.name().to_str() {
                match name {
                    "scrollPageUp:" => {
                        state.scroll_by_lines(state.grid.rows() as isize);
                        self.setNeedsDisplay(true);
                        return;
                    }
                    "scrollPageDown:" => {
                        state.scroll_by_lines(-(state.grid.rows() as isize));
                        self.setNeedsDisplay(true);
                        return;
                    }
                    _ => {}
                }
            }

            let mut input_bytes = Vec::new();
            if keymap::append_input_from_selector(selector, &mut input_bytes) {
                state.send_input(input_bytes);
            }
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text_selected_range_replacement_range(
            &self,
            string: &AnyObject,
            _selected_range: NSRange,
            _replacement_range: NSRange,
        ) {
            let text = ns_object_to_string(string).unwrap_or_default();
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };

            state.marked_text = text;
            state.ime_composing = !state.marked_text.is_empty();
            state.had_composition = false;
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };

            state.marked_text.clear();
            state.ime_composing = false;
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_ref() }) else {
                return NSRange::new(NSNotFound as NSUInteger, 0);
            };
            state.marked_text_range()
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_ref() }) else {
                return NSRange::new(NSNotFound as NSUInteger, 0);
            };
            state.marked_text_range()
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> Bool {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_ref() }) else {
                return Bool::NO;
            };
            (!state.marked_text.is_empty()).into()
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring_for_proposed_range_actual_range(
            &self,
            _range: NSRange,
            _actual_range: *mut NSRange,
        ) -> Option<Retained<NSAttributedString>> {
            None
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes_for_marked_text(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new()
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect_for_character_range_actual_range(
            &self,
            _range: NSRange,
            _actual_range: *mut NSRange,
        ) -> NSRect {
            let Some(window) = self.window() else {
                return NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
            };

            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_ref() }) else {
                return NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
            };

            let view_rect = state.cursor_cell_rect(self.bounds());
            window.convertRectToScreen(view_rect)
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, _point: NSPoint) -> NSUInteger {
            0
        }
    }
);

impl ShittyTerminalView {
    pub(super) fn new(mtm: MainThreadMarker, state: TerminalViewState) -> Retained<Self> {
        let state_ptr = Box::into_raw(Box::new(state));
        let this = Self::alloc(mtm).set_ivars(TerminalViewIvars { state: state_ptr });
        let view: Retained<Self> = unsafe {
            msg_send![
                super(this),
                initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0))
            ]
        };
        let timer_target = TimerTarget::new(mtm, &view);
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                1.0 / RENDER_REFRESH_RATE_HZ,
                &timer_target,
                sel!(onTimerTick:),
                None,
                true,
            )
        };
        unsafe {
            if let Some(state) = state_ptr.as_mut() {
                state.timer = Some(timer);
            }
        }
        view.record_grid_size(view.bounds().size);
        view
    }

    /// Record a window size change. The grid and PTY are resized together on
    /// the render timer once the size has been stable, so the shell's redraw
    /// is always processed at the width it was generated for.
    fn record_grid_size(&self, size: NSSize) {
        let state_ptr = self.ivars().state;
        if state_ptr.is_null() {
            return;
        }
        let state = unsafe { &mut *state_ptr };
        let cols = ((size.width / state.cell_width).floor() as usize).max(1);
        let rows = ((size.height / state.cell_height).floor() as usize).max(1);
        if state.resize_debouncer.record(cols, rows) {
            // The very first size is applied right away so the shell starts
            // with a sane terminal size; there is no prior redraw to race.
            if !state.resize_debouncer.has_sent()
                && let Some((cols, rows)) = state.resize_debouncer.take_now()
                && state.grid.resize(cols, rows)
            {
                state.send_resize(cols, rows);
            }
            self.setNeedsDisplay(true);
        }
    }

    /// Convert a mouse event into a snapped grid cell.
    fn cell_at_event(&self, event: &NSEvent) -> CellPos {
        let state_ptr = self.ivars().state;
        let Some(state) = (unsafe { state_ptr.as_ref() }) else {
            return CellPos::new(0, 0);
        };
        let bounds = self.bounds();
        let location = self.convertPoint_fromView(event.locationInWindow(), None);
        let x = (location.x - bounds.origin.x).max(0.0);
        let y = (bounds.size.height - (location.y - bounds.origin.y)).max(0.0);
        let pos = CellPos::new(
            (y / state.cell_height) as usize,
            (x / state.cell_width) as usize,
        );
        state.grid.snap_cell_pos(pos)
    }
}

pub(super) struct TerminalViewIvars {
    state: *mut TerminalViewState,
}

// Drop impl ensures the boxed TerminalViewState is freed when the ObjC view
// is deallocated by ARC, which also triggers TerminalViewState::drop() which
// invalidates the NSTimer and breaks the retain cycle.
impl Drop for TerminalViewIvars {
    fn drop(&mut self) {
        if !self.state.is_null() {
            unsafe { drop(Box::from_raw(self.state)) };
            self.state = std::ptr::null_mut();
        }
    }
}

define_class!(
    #[unsafe(super(objc2_foundation::NSObject))]
    #[thread_kind = objc2::MainThreadOnly]
    #[ivars = TimerTargetIvars]
    struct TimerTarget;

    impl TimerTarget {
        #[unsafe(method(onTimerTick:))]
        fn on_timer_tick(&self, _timer: &NSTimer) {
            let state_ptr = self.ivars().view.ivars().state;
            if state_ptr.is_null() {
                return;
            }
            let state = unsafe { &mut *state_ptr };

            // Drain pending output before resizing: bytes written for the
            // current width must be processed at that width, otherwise a
            // shell redraw can wrap at a smaller grid and leave duplicates.
            let mut received_data = false;
            while let Ok(bytes) = state.rx_output.try_recv() {
                state.grid.process_pty_bytes(&bytes);
                received_data = true;
            }

            if received_data {
                // The grid contents changed, so a stale highlight no longer matches.
                state.selection = None;
            }

            // Apply a debounced resize once the drag settles: the grid and
            // PTY move together, so the shell only redraws once at the final
            // size and that redraw is processed at the matching width.
            if let Some((cols, rows)) = state.resize_debouncer.take_stable() {
                if state.grid.resize(cols, rows) {
                    state.send_resize(cols, rows);
                    // The grid geometry changed, so the selection no longer fits.
                    state.selection = None;
                } else {
                    // Grid already at this size; make sure the PTY knows too.
                    state.send_resize(cols, rows);
                }
            }

            if state.grid.has_changes() {
                self.ivars().view.setNeedsDisplay(true);
                state.grid.mark_rendered();
            }
        }
    }
);

struct TimerTargetIvars {
    view: Retained<ShittyTerminalView>,
}

impl TimerTarget {
    fn new(mtm: MainThreadMarker, view: &Retained<ShittyTerminalView>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TimerTargetIvars { view: view.clone() });
        unsafe { msg_send![super(this), init] }
    }
}

/// Build the foreground-color + font attribute dictionary used by every text draw.
fn text_attributes(
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

fn paint_selection_overlay(
    state: &TerminalViewState,
    selection: &Selection,
    bounds: NSRect,
    scale: f64,
) {    let color = NSColor::colorWithSRGBRed_green_blue_alpha(
        SELECTION_BG.r as f64 / 255.0,
        SELECTION_BG.g as f64 / 255.0,
        SELECTION_BG.b as f64 / 255.0,
        SELECTION_BG.a as f64 / 255.0,
    );
    color.set();
    for row in 0..state.grid.rows() {
        let mut col = 0;
        while col < state.grid.cols() {
            if selection.contains(row, col) {
                let start_col = col;
                while col < state.grid.cols() && selection.contains(row, col) {
                    col += 1;
                }
                let x0 =
                    snap_to_pixel(bounds.origin.x + start_col as f64 * state.cell_width, scale);
                let x1 = snap_to_pixel(bounds.origin.x + col as f64 * state.cell_width, scale);
                let y0 = snap_to_pixel(
                    bounds.origin.y + bounds.size.height - ((row + 1) as f64 * state.cell_height),
                    scale,
                );
                let y1 = snap_to_pixel(
                    bounds.origin.y + bounds.size.height - (row as f64 * state.cell_height),
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

/// The full-height scrollbar track rect along the right edge of the view.
fn scrollbar_track(bounds: NSRect) -> NSRect {
    NSRect::new(
        NSPoint::new(
            bounds.origin.x + bounds.size.width - SCROLLBAR_WIDTH,
            bounds.origin.y,
        ),
        NSSize::new(SCROLLBAR_WIDTH, bounds.size.height),
    )
}

fn paint_scrollbar(state: &TerminalViewState, bounds: NSRect, scale: f64) {
    let track = scrollbar_track(bounds);
    let Some(thumb) = state.scrollbar_thumb(track) else {
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
    let path =
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(thumb_rect, radius, radius);
    let thumb_color = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.28);
    thumb_color.set();
    path.fill();
}

fn ns_object_to_string(obj: &AnyObject) -> Option<String> {
    autoreleasepool(|pool| {
        if let Some(string) = obj.downcast_ref::<NSString>() {
            return Some(unsafe { string.to_str(pool).to_string() });
        }
        if let Some(string) = obj.downcast_ref::<NSAttributedString>() {
            let ns = string.string();
            return Some(unsafe { ns.to_str(pool).to_string() });
        }
        None
    })
}

fn to_nscolor(c: Color32) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        c.r as f64 / 255.0,
        c.g as f64 / 255.0,
        c.b as f64 / 255.0,
        c.a as f64 / 255.0,
    )
}

fn snap_to_pixel(value: f64, scale: f64) -> f64 {
    if scale <= 0.0 {
        return value;
    }
    (value * scale).round() / scale
}
