use crate::terminal::color::{Color32, SELECTION_BG};
use crate::terminal::grid::{CellPos, TerminalGrid};
use crate::terminal::keymap;
use crate::terminal::pty::PtyEvent;
use crate::terminal::resize::{RESIZE_DEBOUNCE_DELAY, ResizeDebouncer};
use crate::terminal::selection::Selection;
use alacritty_terminal::vte::ansi::CursorShape;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Bool, ProtocolObject, Sel};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSEvent, NSEventModifierFlags, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSPasteboard, NSPasteboardTypeString, NSStringDrawing,
    NSTextInputClient, NSView,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSDictionary, NSMutableDictionary,
    NSNotFound, NSObject, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString, NSUInteger,
};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use super::cast_any_object;
use super::font::{
    DEFAULT_FONT_SIZE, FONT_SIZE_STEP, clamp_font_size, load_terminal_font, measure_cell_size,
};

/// Coalescing window for bursts of PTY output.
const OUTPUT_DEBOUNCE_DELAY: Duration = Duration::from_millis(8);
/// Cap on PTY bytes parsed per event so a huge burst never freezes the main thread.
const MAX_PTY_BYTES_PER_DRAIN: usize = 256 * 1024;

/// Objective-C target used to bridge background PTY events onto the main run loop.
/// It uses only Foundation selectors, avoiding a separate dispatch dependency.
pub(super) struct ViewWakeIvars {
    output_pending: AtomicBool,
    exit_pending: AtomicBool,
    view: AtomicPtr<ShittyTerminalView>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = AnyThread]
    #[ivars = ViewWakeIvars]
    pub(super) struct ViewWake;

    unsafe impl NSObjectProtocol for ViewWake {}

    impl ViewWake {
        #[unsafe(method(scheduleOutputDrain:))]
        fn schedule_output_drain(&self, _sender: Option<&AnyObject>) {
            unsafe {
                let _: () = msg_send![
                    self,
                    performSelector: sel!(drainOutput:),
                    withObject: None::<&AnyObject>,
                    afterDelay: OUTPUT_DEBOUNCE_DELAY.as_secs_f64()
                ];
            }
        }

        #[unsafe(method(drainOutput:))]
        fn drain_output(&self, _sender: Option<&AnyObject>) {
            self.ivars().output_pending.store(false, Ordering::Release);
            let view = self.ivars().view.load(Ordering::Acquire);
            if let Some(view) = unsafe { Retained::retain(view) }
                && view.pty_output_ready()
            {
                self.signal_output();
            }
        }

        #[unsafe(method(sessionExited:))]
        fn session_exited(&self, _sender: Option<&AnyObject>) {
            let view = self.ivars().view.load(Ordering::Acquire);
            if let Some(view) = unsafe { Retained::retain(view) } {
                view.close_window();
            }
        }

        #[unsafe(method(resizeDebounceReady:))]
        fn resize_debounce_ready(&self, _sender: Option<&AnyObject>) {
            let view = self.ivars().view.load(Ordering::Acquire);
            if let Some(view) = unsafe { Retained::retain(view) } {
                view.resize_debounce_ready();
            }
        }
    }
);

impl ViewWake {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(ViewWakeIvars {
            output_pending: AtomicBool::new(false),
            exit_pending: AtomicBool::new(false),
            view: AtomicPtr::new(std::ptr::null_mut()),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn attach(&self, view: *mut ShittyTerminalView) {
        self.ivars().view.store(view, Ordering::Release);
    }

    fn detach(&self) {
        self.ivars()
            .view
            .store(std::ptr::null_mut(), Ordering::Release);
    }

    /// Coalesce output events and drain them on the main run loop after 8 ms.
    pub(super) fn signal_output(&self) {
        if !self.ivars().output_pending.swap(true, Ordering::AcqRel) {
            unsafe {
                let _: () = msg_send![
                    self,
                    performSelectorOnMainThread: sel!(scheduleOutputDrain:),
                    withObject: None::<&AnyObject>,
                    waitUntilDone: false
                ];
            }
        }
    }

    /// Deliver shell exit immediately and at most once on the main thread.
    pub(super) fn signal_exit(&self) {
        if !self.ivars().exit_pending.swap(true, Ordering::AcqRel) {
            unsafe {
                let _: () = msg_send![
                    self,
                    performSelectorOnMainThread: sel!(sessionExited:),
                    withObject: None::<&AnyObject>,
                    waitUntilDone: false
                ];
            }
        }
    }

    /// Schedule the single outstanding resize debounce callback.
    fn schedule_resize(&self, delay: Duration) {
        unsafe {
            let _: () = msg_send![
                self,
                performSelector: sel!(resizeDebounceReady:),
                withObject: None::<&AnyObject>,
                afterDelay: delay.as_secs_f64()
            ];
        }
    }
}

/// Width of the scrollbar drawn over the right edge of the view.
const SCROLLBAR_WIDTH: f64 = 12.0;
/// Minimum scrollbar thumb height in points.
const SCROLLBAR_MIN_THUMB: f64 = 24.0;
/// Horizontal inset of the thumb inside the scrollbar track.
const SCROLLBAR_INSET: f64 = 2.0;
/// Lines scrolled per notch of a non-precise (mouse wheel) event.
const WHEEL_NOTCH_LINES: f64 = 3.0;

/// Rust-side state owned by the terminal view.
pub(super) struct TerminalViewState {
    grid: TerminalGrid,
    rx_output: Receiver<Vec<u8>>,
    tx_input: Sender<PtyEvent>,
    font: Retained<NSFont>,
    font_size: f64,
    cell_width: f64,
    cell_height: f64,
    marked_text: String,
    ime_composing: bool,
    had_composition: bool,
    skip_next_command: bool,
    selection: Option<Selection>,
    resize_debouncer: ResizeDebouncer,
    /// Whether one delayed resize callback is already queued.
    resize_task_scheduled: bool,
    /// Fractional scroll-wheel delta waiting to be applied to the grid.
    scroll_accum: f64,
    /// Whether the scrollbar thumb is being dragged.
    scrollbar_dragging: bool,
    /// Shared with the PTY reader thread, which signals it when shell output arrives.
    wake: Retained<ViewWake>,
}

impl TerminalViewState {
    pub(super) fn new(
        rx_output: Receiver<Vec<u8>>,
        tx_input: Sender<PtyEvent>,
        font: Retained<NSFont>,
        cell_width: f64,
        cell_height: f64,
    ) -> Self {
        let font_size = font.pointSize();
        Self {
            grid: TerminalGrid::new_with_pty_size(
                80,
                24,
                tx_input.clone(),
                cell_width,
                cell_height,
            ),
            rx_output,
            tx_input,
            font,
            font_size,
            cell_width,
            cell_height,
            marked_text: String::new(),
            ime_composing: false,
            had_composition: false,
            skip_next_command: false,
            selection: None,
            resize_debouncer: ResizeDebouncer::new(),
            resize_task_scheduled: false,
            scroll_accum: 0.0,
            scrollbar_dragging: false,
            wake: ViewWake::new(),
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

    /// Clipboard shortcuts (⌘C copy / ⌘V paste, ESC cancels selection).
    /// Returns true when consumed.
    ///
    /// ⌘C is always consumed: with a selection it copies, otherwise it sends
    /// ^C to interrupt (AppKit's key binding would swallow it as `copy:`).
    fn handle_clipboard_key(&mut self, event: &NSEvent) -> bool {
        let characters = event
            .charactersIgnoringModifiers()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let has_command = event
            .modifierFlags()
            .contains(NSEventModifierFlags::Command);

        if has_command && characters.eq_ignore_ascii_case("c") {
            if self.selection.is_some() {
                self.copy_selection();
            } else {
                self.send_input(vec![0x03]);
            }
            return true;
        }

        if has_command && characters.eq_ignore_ascii_case("v") {
            self.paste_from_pasteboard();
            return true;
        }

        if characters == "\u{1b}" && self.selection.is_some() {
            self.selection = None;
            return true;
        }

        false
    }

    /// Font zoom shortcuts (⌘+/⌘− steps the size, ⌘0 resets to default).
    /// Returns true when the key was consumed.
    fn handle_zoom_key(&mut self, event: &NSEvent) -> bool {
        if !event
            .modifierFlags()
            .contains(NSEventModifierFlags::Command)
        {
            return false;
        }
        let characters = event
            .charactersIgnoringModifiers()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let step = match characters.as_str() {
            "=" | "+" => FONT_SIZE_STEP,
            "-" | "_" => -FONT_SIZE_STEP,
            "0" => {
                self.change_font_size(DEFAULT_FONT_SIZE);
                return true;
            }
            _ => return false,
        };
        self.change_font_size(self.font_size + step);
        true
    }

    /// Rebuild the font and cell metrics for a new size; the grid reflow and
    /// PTY resize are driven by the caller through `record_grid_size`.
    fn change_font_size(&mut self, size: f64) {
        let size = clamp_font_size(size);
        if (size - self.font_size).abs() < f64::EPSILON {
            return;
        }
        let font = load_terminal_font(size);
        let (cell_width, cell_height) = measure_cell_size(&font);
        self.font = font;
        self.font_size = size;
        self.cell_width = cell_width;
        self.cell_height = cell_height;
        self.grid.set_cell_size(cell_width, cell_height);
        // The selection is stored in cell coordinates, so it no longer fits.
        self.selection = None;
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

    /// Paste the general pasteboard's text into the shell.
    fn paste_from_pasteboard(&mut self) {
        let text = autoreleasepool(|pool| {
            let pasteboard = NSPasteboard::generalPasteboard();
            let ns_text = unsafe { pasteboard.stringForType(NSPasteboardTypeString) }?;
            let text = unsafe { ns_text.to_str(pool).to_string() };
            (!text.is_empty()).then_some(text)
        });
        let Some(text) = text else {
            return;
        };
        let mut input_bytes = Vec::new();
        keymap::append_paste_text(&text, &mut input_bytes);
        self.send_input(input_bytes);
    }

    fn cursor_cell_rect(&self, bounds: NSRect) -> NSRect {
        let origin = bounds.origin;
        let (cell_w, cell_h) = (self.cell_width, self.cell_height);
        let (cursor_row, cursor_col) = self.grid.cursor_pos();
        let x0 = origin.x + cursor_col as f64 * cell_w;
        let y0 = origin.y + bounds.size.height - ((cursor_row + 1) as f64 * cell_h);
        NSRect::new(NSPoint::new(x0, y0), NSSize::new(cell_w, cell_h))
    }

    /// Drain pending PTY output into the grid, bounded so a huge redraw burst
    /// never stalls the main thread for long.
    fn drain_pty_output(&mut self) -> bool {
        let mut processed_bytes = 0usize;
        while processed_bytes < MAX_PTY_BYTES_PER_DRAIN {
            match self.rx_output.try_recv() {
                Ok(bytes) => {
                    processed_bytes += bytes.len();
                    self.grid.process_pty_bytes(&bytes);
                }
                Err(_) => return false,
            }
        }
        true
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
        // Null the wake handle first so queued main-queue wake-ups can never
        // reach a dangling view pointer.
        self.wake.detach();
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
                if state.handle_clipboard_key(event) {
                    self.setNeedsDisplay(true);
                    return;
                }
                // Font zoom shortcuts (⌘+/⌘−/⌘0) are handled here rather
                // than through interpretKeyEvents:, which would ignore them.
                if !state.ime_composing && state.handle_zoom_key(event) {
                    // The cell size changed, so re-derive the grid size from
                    // the (unchanged) view bounds and let the debounced resize
                    // path reflow the grid and resize the PTY together.
                    self.record_grid_size(self.bounds().size);
                    self.setNeedsDisplay(true);
                    return;
                }
                // Translate Control+key (^C, ^D, ...) straight to control
                // bytes. Going through interpretKeyEvents: would let
                // AppKit's key bindings swallow most of them (^D becomes
                // deleteForward:, ^A/^E/^K/^U/^N/^P/^B/^F/^W/^T/^V become
                // editing commands), so they'd never reach the shell.
                if !state.ime_composing
                    && let Some(bytes) = keymap::control_key_bytes(event)
                {
                    state.send_input(bytes);
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
                let cell_w = (cursor_x1 - cursor_x0).max(0.0);
                let cell_h = (cursor_y1 - cursor_y0).max(0.0);
                let cursor_paint_color = to_nscolor(cursor.bg);
                cursor_paint_color.set();

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
                            let text_color = to_nscolor(cursor.fg);
                            let text_attrs = text_attributes(&state.font, &text_color);
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
                    // Fallbacks for AppKit routing ⌘C/⌘V here directly.
                    "paste:" => {
                        state.paste_from_pasteboard();
                        self.setNeedsDisplay(true);
                        return;
                    }
                    "copy:" => {
                        state.copy_selection();
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
        let wake = unsafe { (*state_ptr).wake.clone() };
        let this = Self::alloc(mtm).set_ivars(TerminalViewIvars {
            state: state_ptr,
            wake,
        });
        let view: Retained<Self> = unsafe {
            msg_send![
                super(this),
                initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0))
            ]
        };
        view.record_grid_size(view.bounds().size);
        // Register the view with the wake handle so the PTY reader thread can
        // trigger coalesced drain+redraw passes from now on.
        view.ivars().wake.attach(Retained::as_ptr(&view).cast_mut());
        view
    }

    /// Record a window size change and schedule one trailing debounce callback.
    fn record_grid_size(&self, size: NSSize) {
        let state_ptr = self.ivars().state;
        if state_ptr.is_null() {
            return;
        }
        let state = unsafe { &mut *state_ptr };
        let cols = ((size.width / state.cell_width).floor() as usize).max(1);
        let rows = ((size.height / state.cell_height).floor() as usize).max(1);
        if !state.resize_debouncer.record(cols, rows, Instant::now()) {
            return;
        }

        // The first size is safe to apply immediately because no shell redraw
        // for a previous width can still be in flight.
        if !state.resize_debouncer.has_sent() {
            if let Some((cols, rows)) = state.resize_debouncer.take_now() {
                Self::apply_grid_size(state, cols, rows);
            }
        } else if !state.resize_task_scheduled {
            state.resize_task_scheduled = true;
            state.wake.schedule_resize(RESIZE_DEBOUNCE_DELAY);
        }
        self.setNeedsDisplay(true);
    }

    /// Run the one outstanding resize callback, rescheduling only when a newer
    /// frame event pushed the trailing deadline farther out.
    fn resize_debounce_ready(&self) {
        let state_ptr = self.ivars().state;
        let Some(state) = (unsafe { state_ptr.as_mut() }) else {
            return;
        };
        state.resize_task_scheduled = false;

        // Process output generated for the old width before changing both the
        // grid and PTY. If the byte budget is exhausted, continue in another
        // event and retry the resize afterwards.
        let needs_output_follow_up = state.drain_pty_output();
        self.redraw_grid_if_changed(state);
        if needs_output_follow_up {
            state.wake.signal_output();
            state.resize_task_scheduled = true;
            state.wake.schedule_resize(OUTPUT_DEBOUNCE_DELAY);
            return;
        }

        let now = Instant::now();
        if let Some((cols, rows)) = state.resize_debouncer.take_ready(now) {
            Self::apply_grid_size(state, cols, rows);
            self.redraw_grid_if_changed(state);
        } else if let Some(delay) = state.resize_debouncer.remaining_delay(now) {
            state.resize_task_scheduled = true;
            state.wake.schedule_resize(delay);
        }
    }

    fn apply_grid_size(state: &mut TerminalViewState, cols: usize, rows: usize) {
        if state.grid.resize(cols, rows) {
            state.send_resize(cols, rows);
            state.selection = None;
        } else {
            // Grid already has this size; make sure the PTY knows too.
            state.send_resize(cols, rows);
        }
    }

    fn redraw_grid_if_changed(&self, state: &mut TerminalViewState) {
        if state.grid.has_changes() {
            self.setNeedsDisplay(true);
            state.grid.mark_rendered();
        }
    }

    /// Close the window, which the app delegate turns into a full quit.
    fn close_window(&self) {
        if let Some(window) = self.window() {
            window.performClose(None);
        }
    }

    /// Drain a bounded PTY output batch and request a redraw. Returns true
    /// when another event is needed to continue draining a large burst.
    fn pty_output_ready(&self) -> bool {
        let state_ptr = self.ivars().state;
        if state_ptr.is_null() {
            return false;
        }
        let state = unsafe { &mut *state_ptr };
        let needs_follow_up = state.drain_pty_output();
        self.redraw_grid_if_changed(state);
        needs_follow_up
    }

    /// The event bridge shared with the PTY reader thread.
    pub(super) fn view_wake_handle(&self) -> Retained<ViewWake> {
        self.ivars().wake.clone()
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
    /// Clone of the event bridge inside the state, for the PTY reader thread.
    wake: Retained<ViewWake>,
}

// Drop impl ensures the boxed TerminalViewState is freed when the ObjC view
// is deallocated by ARC, which also detaches queued event callbacks.
impl Drop for TerminalViewIvars {
    fn drop(&mut self) {
        if !self.state.is_null() {
            unsafe { drop(Box::from_raw(self.state)) };
            self.state = std::ptr::null_mut();
        }
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
) {
    let color = NSColor::colorWithSRGBRed_green_blue_alpha(
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
    let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(thumb_rect, radius, radius);
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
