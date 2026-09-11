use crate::terminal::color::{Color32, SELECTION_BG};
use crate::terminal::grid::CellPos;
use crate::terminal::keymap;
use crate::terminal::resize::{RESIZE_DEBOUNCE_DELAY, ResizeDebouncer};
use crate::terminal::selection::Selection;
use crate::terminal::session::TerminalSession;
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
use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::time::{Duration, Instant};

use super::cast_any_object;
use super::font::{
    DEFAULT_FONT_SIZE, FONT_SIZE_STEP, clamp_font_size, load_terminal_font, measure_cell_size,
};

/// Coalescing window for bursts of PTY output.
const OUTPUT_DEBOUNCE_DELAY: Duration = Duration::from_millis(8);
/// Cap on PTY bytes parsed per event so a huge burst never freezes the main thread.
const MAX_PTY_BYTES_PER_DRAIN: usize = 256 * 1024;

/// Height of the tab strip drawn above the terminal content.
const TAB_BAR_HEIGHT: f64 = 30.0;
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

/// Objective-C target used to bridge background PTY events onto the main run loop.
/// It uses only Foundation selectors, avoiding a separate dispatch dependency.
pub(super) struct ViewWakeIvars {
    output_pending: AtomicBool,
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
                view.session_exited();
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

    /// Deliver a shell exit on the main thread. Every session dispatches its
    /// own callback so one exiting tab can never mask another.
    pub(super) fn signal_exit(&self) {
        unsafe {
            let _: () = msg_send![
                self,
                performSelectorOnMainThread: sel!(sessionExited:),
                withObject: None::<&AnyObject>,
                waitUntilDone: false
            ];
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

/// Rust-side state owned by the terminal view. It owns every tab and the
/// view-level state (font metrics, IME, resize debouncing) shared by them.
pub(super) struct TerminalViewState {
    sessions: Vec<TerminalSession>,
    active: usize,
    next_session_id: u64,
    font: Retained<NSFont>,
    font_size: f64,
    cell_width: f64,
    cell_height: f64,
    marked_text: String,
    ime_composing: bool,
    had_composition: bool,
    skip_next_command: bool,
    resize_debouncer: ResizeDebouncer,
    /// Whether one delayed resize callback is already queued.
    resize_task_scheduled: bool,
    /// Shared with the PTY reader threads, which signal it when output arrives.
    wake: Retained<ViewWake>,
}

impl TerminalViewState {
    pub(super) fn new(
        font: Retained<NSFont>,
        cell_width: f64,
        cell_height: f64,
    ) -> Result<Self, Box<dyn Error>> {
        let font_size = font.pointSize();
        let mut state = Self {
            sessions: Vec::new(),
            active: 0,
            next_session_id: 1,
            font,
            font_size,
            cell_width,
            cell_height,
            marked_text: String::new(),
            ime_composing: false,
            had_composition: false,
            skip_next_command: false,
            resize_debouncer: ResizeDebouncer::new(),
            resize_task_scheduled: false,
            wake: ViewWake::new(),
        };
        // The first size is applied by the view once it knows its bounds.
        state.spawn_session(80, 24)?;
        Ok(state)
    }

    /// The focused session. The view always owns at least one: closing the
    /// last tab closes the window instead of emptying the list.
    fn active(&self) -> &TerminalSession {
        &self.sessions[self.active]
    }

    fn active_mut(&mut self) -> &mut TerminalSession {
        &mut self.sessions[self.active]
    }

    /// The tab strip is only worth showing once there is more than one tab;
    /// a lone tab gets the whole view.
    fn show_tab_bar(&self) -> bool {
        self.sessions.len() > 1
    }

    /// Open a new shell session and focus it.
    fn spawn_session(&mut self, cols: usize, rows: usize) -> Result<(), Box<dyn Error>> {
        let id = self.next_session_id;
        self.next_session_id += 1;
        let wake_output = self.wake.clone();
        let wake_exit = self.wake.clone();
        let session = TerminalSession::spawn(
            id,
            cols,
            rows,
            self.cell_width,
            self.cell_height,
            move || wake_output.signal_output(),
            move || wake_exit.signal_exit(),
        )?;
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
        Ok(())
    }

    /// Close the tab at `index`. Returns false when it is the last tab, in
    /// which case the caller closes the window instead.
    fn close_tab(&mut self, index: usize) -> bool {
        if self.sessions.len() <= 1 || index >= self.sessions.len() {
            return false;
        }
        let previous_active_id = self.sessions.get(self.active).map(TerminalSession::id);
        self.sessions.remove(index);
        self.active = previous_active_id
            .and_then(|id| self.sessions.iter().position(|session| session.id() == id))
            .unwrap_or_else(|| index.min(self.sessions.len() - 1));
        true
    }

    /// Drop sessions whose shells have exited. Returns true when no live
    /// session remains, in which case the caller closes the window.
    fn remove_exited_sessions(&mut self) -> bool {
        if self.sessions.iter().all(TerminalSession::exited) {
            return true;
        }
        let previous_active_id = self.sessions.get(self.active).map(TerminalSession::id);
        self.sessions.retain(|session| !session.exited());
        self.active = previous_active_id
            .and_then(|id| self.sessions.iter().position(|session| session.id() == id))
            .unwrap_or_else(|| self.sessions.len() - 1);
        false
    }

    /// Focus the tab at `index`; returns whether the focus moved.
    fn select_tab(&mut self, index: usize) -> bool {
        if index >= self.sessions.len() || index == self.active {
            return false;
        }
        self.active = index;
        // Composition belongs to the focused session, so it never carries over.
        self.marked_text.clear();
        self.ime_composing = false;
        self.had_composition = false;
        self.skip_next_command = false;
        true
    }

    fn send_input(&mut self, bytes: Vec<u8>) {
        self.active_mut().send_input(bytes);
    }

    /// Scroll the view by whole lines; positive moves toward older history.
    fn scroll_by_lines(&mut self, lines: isize) {
        let session = self.active_mut();
        if session.grid.scroll_by(lines) {
            session.selection = None;
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
        let session = self.active_mut();
        let max_scroll = session.grid.max_scroll();
        let y = y - track.origin.y;
        let thumb_y = (y - thumb.size.height / 2.0).clamp(0.0, travel);
        let offset = if travel > 0.0 {
            (thumb_y / travel * max_scroll as f64).round() as usize
        } else {
            0
        };
        if session.grid.scroll_to(offset) {
            session.selection = None;
        }
    }

    /// The thumb rect for the given track, or None when there is no history.
    fn scrollbar_thumb(&self, track: NSRect) -> Option<NSRect> {
        scrollbar_thumb_for(self.active(), track)
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
            if self.active().selection.is_some() {
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

        if characters == "\u{1b}" && self.active().selection.is_some() {
            self.active_mut().selection = None;
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
        for session in &mut self.sessions {
            session.grid.set_cell_size(cell_width, cell_height);
            // The selection is stored in cell coordinates, so it no longer fits.
            session.selection = None;
        }
    }

    fn copy_selection(&self) {
        let Some(selection) = &self.active().selection else {
            return;
        };
        let text = selection.selected_text(&self.active().grid);
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
        let (cursor_row, cursor_col) = self.active().grid.cursor_pos();
        let x0 = origin.x + cursor_col as f64 * cell_w;
        let y0 = origin.y + bounds.size.height - ((cursor_row + 1) as f64 * cell_h);
        NSRect::new(NSPoint::new(x0, y0), NSSize::new(cell_w, cell_h))
    }

    /// Drain pending PTY output for every session, bounded so a huge redraw
    /// burst never stalls the main thread for long.
    fn drain_pty_output(&mut self) -> bool {
        let session_count = self.sessions.len().max(1);
        let mut budget = (MAX_PTY_BYTES_PER_DRAIN / session_count).max(4096);
        let mut needs_follow_up = false;
        for session in &mut self.sessions {
            needs_follow_up |= session.drain_output(&mut budget);
        }
        needs_follow_up
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

            if let Some(hit) = tab_bar_hit(location, bounds, state.sessions.len()) {
                if let Some(window) = self.window() {
                    window.makeFirstResponder(Some(self));
                }
                match hit {
                    TabBarHit::Tab(index) => {
                        if state.select_tab(index) {
                            self.setNeedsDisplay(true);
                        }
                    }
                    TabBarHit::Close(index) => {
                        if state.close_tab(index) {
                            self.resize_grid_now(state);
                        } else {
                            self.close_window();
                        }
                    }
                    TabBarHit::NewTab => self.create_tab(state),
                }
                return;
            }

            if state.active().grid.max_scroll() > 0
                && location.x >= bounds.origin.x + bounds.size.width - SCROLLBAR_WIDTH
            {
                state.active_mut().scrollbar_dragging = true;
                let track = scrollbar_track(terminal_bounds(bounds, state.show_tab_bar()));
                state.scroll_to_track_y(location.y, track);
                self.setNeedsDisplay(true);
                return;
            }
            state.active_mut().selection = Some(Selection::start(self.cell_at_event(event)));
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
            if state.active().scrollbar_dragging {
                let location = self.convertPoint_fromView(event.locationInWindow(), None);
                let track =
                    scrollbar_track(terminal_bounds(self.bounds(), state.show_tab_bar()));
                state.scroll_to_track_y(location.y, track);
                self.setNeedsDisplay(true);
                return;
            }
            if let Some(selection) = state.active_mut().selection.as_mut() {
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
            state.active_mut().scrollbar_dragging = false;
            if state.active().selection.as_ref().is_some_and(|s| s.is_empty()) {
                state.active_mut().selection = None;
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
            let session = state.active_mut();
            session.scroll_accum += delta;
            let lines = session.scroll_accum.trunc() as isize;
            if lines != 0 {
                session.scroll_accum -= lines as f64;
                if session.grid.scroll_by(lines) {
                    session.selection = None;
                    self.setNeedsDisplay(true);
                }
            }
        }

        #[unsafe(method(newTab:))]
        fn new_tab(&self, _sender: Option<&AnyObject>) {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };
            self.create_tab(state);
        }

        #[unsafe(method(closeTab:))]
        fn close_tab(&self, _sender: Option<&AnyObject>) {
            self.close_active_tab();
        }

        #[unsafe(method(selectNextTab:))]
        fn select_next_tab(&self, _sender: Option<&AnyObject>) {
            self.select_relative_tab(1);
        }

        #[unsafe(method(selectPreviousTab:))]
        fn select_previous_tab(&self, _sender: Option<&AnyObject>) {
            self.select_relative_tab(-1);
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let state_ptr = self.ivars().state;
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                if state.handle_clipboard_key(event) {
                    self.setNeedsDisplay(true);
                    return;
                }
                // ⌘1–⌘9 focus the tab at that position; tabs past the ninth
                // stay reachable through the tab strip.
                if !state.ime_composing && let Some(index) = handle_tab_index_key(event) {
                    self.select_tab_at(index);
                    return;
                }
                // ⌘⇧[ / ⌘⇧] are normally consumed by the menu key
                // equivalents; this covers layouts where they are not.
                if !state.ime_composing && handle_tab_switch_key(event) {
                    let delta = if event
                        .charactersIgnoringModifiers()
                        .is_some_and(|characters| characters.to_string() == "{")
                    {
                        -1
                    } else {
                        1
                    };
                    self.select_relative_tab(delta);
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
            let show_tab_bar = state.show_tab_bar();
            let terminal = terminal_bounds(bounds, show_tab_bar);
            let origin = terminal.origin;
            let scale = self
                .window()
                .map(|window| window.backingScaleFactor())
                .unwrap_or(1.0);
            let session = state.active();
            let default_bg = session.grid.default_bg();

            to_nscolor(default_bg).set();
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

            for row in 0..session.grid.rows() {
                for run in session.grid.row_runs(row) {
                    let x0 = snap_to_pixel(origin.x + run.col as f64 * state.cell_width, scale);
                    let x1 = snap_to_pixel(
                        origin.x + (run.col + run.width) as f64 * state.cell_width,
                        scale,
                    );
                    let y0 = snap_to_pixel(
                        origin.y + terminal.size.height - ((row + 1) as f64 * state.cell_height),
                        scale,
                    );
                    let y1 = snap_to_pixel(
                        origin.y + terminal.size.height - (row as f64 * state.cell_height),
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

            if let Some(cursor) = session.grid.cursor() {
                let cursor_x0 = snap_to_pixel(origin.x + cursor.col as f64 * state.cell_width, scale);
                let cursor_x1 = snap_to_pixel(
                    origin.x + (cursor.col + 1) as f64 * state.cell_width,
                    scale,
                );
                let cursor_y0 = snap_to_pixel(
                    origin.y + terminal.size.height - ((cursor.row + 1) as f64 * state.cell_height),
                    scale,
                );
                let cursor_y1 = snap_to_pixel(
                    origin.y + terminal.size.height - (cursor.row as f64 * state.cell_height),
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
                let (cursor_row, cursor_col) = session.grid.cursor_pos();
                let marked_x0 = snap_to_pixel(origin.x + cursor_col as f64 * state.cell_width, scale);
                let marked_y0 = snap_to_pixel(
                    origin.y + terminal.size.height - ((cursor_row + 1) as f64 * state.cell_height),
                    scale,
                );
                let marked_y1 = snap_to_pixel(
                    origin.y + terminal.size.height - (cursor_row as f64 * state.cell_height),
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

            if session.grid.max_scroll() > 0 {
                paint_scrollbar(session, terminal, scale);
            }

            if show_tab_bar {
                paint_tab_bar(state, bounds);
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
                        let rows = state.active().grid.rows() as isize;
                        state.scroll_by_lines(rows);
                        self.setNeedsDisplay(true);
                        return;
                    }
                    "scrollPageDown:" => {
                        let rows = state.active().grid.rows() as isize;
                        state.scroll_by_lines(-rows);
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
        // The shell may have printed its prompt before the view existed; its
        // bytes are still queued, so drain them once now.
        view.pty_output_ready();
        view.setNeedsDisplay(true);
        view
    }

    /// Record a window size change and schedule one trailing debounce callback.
    fn record_grid_size(&self, size: NSSize) {
        let state_ptr = self.ivars().state;
        if state_ptr.is_null() {
            return;
        }
        let state = unsafe { &mut *state_ptr };
        let show_tab_bar = state.show_tab_bar();
        let (cols, rows) = grid_size_for(size, state.cell_width, state.cell_height, show_tab_bar);
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

    /// Apply one size to every tab so switching tabs never shows a stale
    /// width; the PTY is told once per size.
    fn apply_grid_size(state: &mut TerminalViewState, cols: usize, rows: usize) {
        let notify_pty = state.resize_debouncer.mark_sent((cols, rows));
        for session in &mut state.sessions {
            if session.grid.resize(cols, rows) {
                session.selection = None;
            }
            if notify_pty {
                session.send_resize(cols, rows);
            }
        }
    }

    /// Redraw when the active grid changed; background tabs are parsed but
    /// only need a repaint when their titles change.
    fn redraw_grid_if_changed(&self, state: &mut TerminalViewState) {
        let active = state.active;
        let mut tab_bar_dirty = false;
        for (index, session) in state.sessions.iter_mut().enumerate() {
            tab_bar_dirty |= session.sync_title();
            if session.grid.has_changes() {
                session.grid.mark_rendered();
                if index == active {
                    self.setNeedsDisplay(true);
                }
            }
        }
        if tab_bar_dirty {
            self.setNeedsDisplay(true);
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

    /// Open a tab sized to the current view. The new tab always makes the
    /// strip visible, so the session starts at the shrunken size.
    fn create_tab(&self, state: &mut TerminalViewState) {
        let (cols, rows) = grid_size_for(
            self.bounds().size,
            state.cell_width,
            state.cell_height,
            true,
        );
        match state.spawn_session(cols, rows) {
            Ok(()) => {
                state
                    .sessions
                    .last()
                    .expect("spawn_session pushes the new tab")
                    .send_resize(cols, rows);
                // A second tab reveals the strip, which shrinks the terminal.
                self.resize_grid_now(state);
            }
            Err(error) => eprintln!("failed to create tab: {error}"),
        }
    }

    /// Re-derive the grid size from the current bounds and apply it without
    /// waiting for the resize debounce. Used when the tab strip appears or
    /// disappears, which immediately changes the terminal area.
    fn resize_grid_now(&self, state: &mut TerminalViewState) {
        let show_tab_bar = state.show_tab_bar();
        let (cols, rows) = grid_size_for(
            self.bounds().size,
            state.cell_width,
            state.cell_height,
            show_tab_bar,
        );
        if state.resize_debouncer.record(cols, rows, Instant::now())
            && let Some((cols, rows)) = state.resize_debouncer.take_now()
        {
            Self::apply_grid_size(state, cols, rows);
        }
        self.setNeedsDisplay(true);
    }

    /// Close the focused tab, or the window when it is the last one.
    fn close_active_tab(&self) {
        let state_ptr = self.ivars().state;
        let Some(state) = (unsafe { state_ptr.as_mut() }) else {
            return;
        };
        let index = state.active;
        if state.close_tab(index) {
            self.resize_grid_now(state);
        } else {
            self.close_window();
        }
    }

    /// Focus the tab `delta` positions away, wrapping around.
    fn select_relative_tab(&self, delta: isize) {
        let state_ptr = self.ivars().state;
        let Some(state) = (unsafe { state_ptr.as_mut() }) else {
            return;
        };
        let count = state.sessions.len();
        if count < 2 {
            return;
        }
        let next = (state.active as isize + delta).rem_euclid(count as isize) as usize;
        if state.select_tab(next) {
            self.setNeedsDisplay(true);
        }
    }

    /// Focus the tab at `index`; out-of-range indices are ignored so only
    /// the first nine tabs have a ⌘1–⌘9 shortcut.
    fn select_tab_at(&self, index: usize) {
        let state_ptr = self.ivars().state;
        let Some(state) = (unsafe { state_ptr.as_mut() }) else {
            return;
        };
        if state.select_tab(index) {
            self.setNeedsDisplay(true);
        }
    }

    /// Called when a shell exits: drop its tab, or close the window when no
    /// session is left.
    fn session_exited(&self) {
        let state_ptr = self.ivars().state;
        let Some(state) = (unsafe { state_ptr.as_mut() }) else {
            return;
        };
        if state.remove_exited_sessions() {
            self.close_window();
        } else {
            self.resize_grid_now(state);
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
        let terminal_height = terminal_bounds(bounds, state.show_tab_bar()).size.height;
        let y = (terminal_height - (location.y - bounds.origin.y)).max(0.0);
        let pos = CellPos::new(
            (y / state.cell_height) as usize,
            (x / state.cell_width) as usize,
        );
        state.active().grid.snap_cell_pos(pos)
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

/// The thumb rect for a session's scrollbar track, or None without history.
fn scrollbar_thumb_for(session: &TerminalSession, track: NSRect) -> Option<NSRect> {
    let grid = &session.grid;
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

fn paint_scrollbar(session: &TerminalSession, bounds: NSRect, scale: f64) {
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

/// Terminal content rect: the view minus the tab strip, when shown.
fn terminal_bounds(bounds: NSRect, show_tab_bar: bool) -> NSRect {
    let bar_height = if show_tab_bar { TAB_BAR_HEIGHT } else { 0.0 };
    NSRect::new(
        bounds.origin,
        NSSize::new(
            bounds.size.width,
            (bounds.size.height - bar_height).max(0.0),
        ),
    )
}

/// The grid dimensions that fit a view of `size`.
fn grid_size_for(
    size: NSSize,
    cell_width: f64,
    cell_height: f64,
    show_tab_bar: bool,
) -> (usize, usize) {
    let cols = ((size.width / cell_width).floor() as usize).max(1);
    let bar_height = if show_tab_bar { TAB_BAR_HEIGHT } else { 0.0 };
    let terminal_height = (size.height - bar_height).max(cell_height);
    let rows = ((terminal_height / cell_height).floor() as usize).max(1);
    (cols, rows)
}

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
enum TabBarHit {
    Tab(usize),
    Close(usize),
    NewTab,
}

/// Map a point (view coordinates) to a tab-strip control, if any.
/// A hidden strip (a single tab) never consumes clicks.
fn tab_bar_hit(location: NSPoint, bounds: NSRect, tab_count: usize) -> Option<TabBarHit> {
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

/// Fallback handling for ⌘⇧[ / ⌘⇧] when the menu key equivalents do not
/// consume them (e.g. on some keyboard layouts).
fn handle_tab_switch_key(event: &NSEvent) -> bool {
    let flags = event.modifierFlags();
    if !flags.contains(NSEventModifierFlags::Command)
        || !flags.contains(NSEventModifierFlags::Shift)
    {
        return false;
    }
    matches!(
        event
            .charactersIgnoringModifiers()
            .map(|characters| characters.to_string())
            .as_deref(),
        Some("{") | Some("}")
    )
}

/// Maps a ⌘1–⌘9 key press to a zero-based tab index; None for any other
/// combination, including ⌘0 and digits past nine.
fn handle_tab_index_key(event: &NSEvent) -> Option<usize> {
    let characters = event
        .charactersIgnoringModifiers()
        .map(|characters| characters.to_string())
        .unwrap_or_default();
    tab_index_shortcut(&characters, event.modifierFlags())
}

fn tab_index_shortcut(characters: &str, modifiers: NSEventModifierFlags) -> Option<usize> {
    if !modifiers.contains(NSEventModifierFlags::Command) {
        return None;
    }
    let mut chars = characters.chars();
    let digit = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    match digit {
        '1'..='9' => Some(digit as usize - '1' as usize),
        _ => None,
    }
}

fn paint_tab_bar(state: &TerminalViewState, bounds: NSRect) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> NSRect {
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0))
    }

    #[test]
    fn hidden_tab_bar_gives_the_whole_view_to_the_terminal() {
        let size = NSSize::new(800.0, 600.0);
        assert_eq!(grid_size_for(size, 10.0, 20.0, false), (80, 30));
        assert_eq!(terminal_bounds(bounds(), false), bounds());
    }

    #[test]
    fn grid_size_uses_the_area_below_the_tab_bar() {
        assert_eq!(
            grid_size_for(NSSize::new(800.0, 600.0), 10.0, 20.0, true),
            (80, 28)
        );
    }

    #[test]
    fn terminal_bounds_leaves_room_for_the_tab_bar() {
        let terminal = terminal_bounds(bounds(), true);
        assert_eq!(terminal.origin.y, 0.0);
        assert_eq!(terminal.size.height, 600.0 - TAB_BAR_HEIGHT);
    }

    #[test]
    fn view_state_opens_and_closes_tabs() {
        let font = load_terminal_font(DEFAULT_FONT_SIZE);
        let (cell_width, cell_height) = measure_cell_size(&font);
        let mut state =
            TerminalViewState::new(font, cell_width, cell_height).expect("first tab spawns");

        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.active, 0);
        assert!(!state.show_tab_bar());

        state.spawn_session(80, 24).expect("second tab spawns");
        assert_eq!(state.sessions.len(), 2);
        assert_eq!(state.active, 1);
        assert!(state.show_tab_bar());

        assert!(state.select_tab(0));
        assert_eq!(state.active, 0);

        assert!(state.close_tab(0));
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.active, 0);
        assert!(!state.show_tab_bar());

        // The last tab stays in the list; the caller closes the window instead.
        assert!(!state.close_tab(0));
        assert_eq!(state.sessions.len(), 1);
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
    fn command_digits_map_to_tab_indices() {
        let command = NSEventModifierFlags::Command;
        assert_eq!(tab_index_shortcut("1", command), Some(0));
        assert_eq!(tab_index_shortcut("5", command), Some(4));
        assert_eq!(tab_index_shortcut("9", command), Some(8));

        assert_eq!(tab_index_shortcut("0", command), None);
        assert_eq!(tab_index_shortcut("1", NSEventModifierFlags::empty()), None);
        assert_eq!(tab_index_shortcut("!", command), None);
        assert_eq!(tab_index_shortcut("12", command), None);
        assert_eq!(tab_index_shortcut("", command), None);
    }

    #[test]
    fn only_the_first_nine_tabs_have_shortcut_hints() {
        assert_eq!(tab_shortcut_label(0).as_deref(), Some("⌘1"));
        assert_eq!(tab_shortcut_label(8).as_deref(), Some("⌘9"));
        assert_eq!(tab_shortcut_label(9), None);
    }
}
