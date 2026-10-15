//! The `NSView` subclass: its Objective-C surface, event handling, and the
//! `NSTextInputClient` (IME) conformance.

use crate::mac_app::app::mark_theme_menu;
use crate::terminal::grid::CellPos;
use crate::terminal::keymap;
use crate::terminal::selection::Selection;
use crate::terminal::theme::{self, Theme};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, Sel};
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
};
use objc2_app_kit::{NSEvent, NSTextInputClient, NSTrackingArea, NSTrackingAreaOptions, NSView};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSNotFound, NSPoint, NSRange, NSRect,
    NSSize, NSUInteger,
};

use super::geometry::terminal_bounds;
use super::ime;
use super::renderer;
use super::scrollbar::{self, SCROLLBAR_WIDTH, WHEEL_NOTCH_LINES, scrollbar_track};
use super::state::TerminalViewState;
use super::tab_bar::{self, TabBarHit, tab_bar_hit};
use super::tabs::{handle_tab_index_key, handle_tab_switch_key};
use super::wake::ViewWake;

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = objc2::MainThreadOnly]
    #[ivars = TerminalViewIvars]
    pub(in crate::mac_app) struct ShittyTerminalView;

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
                // The tab count changed under the pointer, so re-aim the hover.
                self.update_tab_bar_hover(event);
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

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.update_tab_bar_hover(event);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };
            if state.tab_bar_hover.take().is_some() {
                self.setNeedsDisplay(true);
            }
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
            if state.scroll_by_wheel_delta(delta) {
                self.setNeedsDisplay(true);
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

        #[unsafe(method(selectTheme:))]
        fn select_theme(&self, sender: Option<&AnyObject>) {
            let Some(item) = sender else {
                return;
            };
            let index: isize = unsafe { msg_send![item, tag] };
            let Some(theme) = Theme::NAMES.get(index as usize).copied().and_then(Theme::parse)
            else {
                return;
            };
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };
            state.set_theme(theme);
            theme::save_theme(theme);
            mark_theme_menu(theme);
            self.setNeedsDisplay(true);
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
                state.begin_key_event();
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
            let scale = self
                .window()
                .map(|window| window.backingScaleFactor())
                .unwrap_or(1.0);

            renderer::paint_terminal(state, bounds, scale);

            if state.active().grid.max_scroll() > 0 {
                scrollbar::paint_scrollbar(
                    state.active(),
                    terminal_bounds(bounds, show_tab_bar),
                    scale,
                );
            }

            if show_tab_bar {
                tab_bar::paint_tab_bar(state, bounds);
            }
        }
    }

    unsafe impl NSTextInputClient for ShittyTerminalView {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text_replacement_range(&self, string: &AnyObject, _replacement_range: NSRange) {
            let Some(text) = ime::ns_object_to_string(string) else {
                return;
            };
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };

            if state.commit_text() {
                state.skip_next_command = true;
            }

            let mut input_bytes = Vec::new();
            keymap::append_input_from_text(&text, &mut input_bytes);
            state.send_input(input_bytes);
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
            let text = ime::ns_object_to_string(string).unwrap_or_default();
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };

            state.set_marked_text(text);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };

            state.unmark_text();
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
            state.has_marked_text().into()
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
    pub(in crate::mac_app) fn new(
        mtm: MainThreadMarker,
        state: TerminalViewState,
    ) -> Retained<Self> {
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
        // Pointer tracking for the tab strip's hover highlights; `InVisibleRect`
        // keeps the area matched to the view as it resizes.
        let tracking = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                NSTrackingAreaOptions::MouseEnteredAndExited
                    | NSTrackingAreaOptions::MouseMoved
                    | NSTrackingAreaOptions::ActiveInKeyWindow
                    | NSTrackingAreaOptions::InVisibleRect,
                Some(&view),
                None,
            )
        };
        view.addTrackingArea(&tracking);
        // Register the view with the wake handle so the PTY reader thread can
        // trigger coalesced drain+redraw passes from now on.
        view.ivars().wake.attach(Retained::as_ptr(&view).cast_mut());
        // The shell may have printed its prompt before the view existed; its
        // bytes are still queued, so drain them once now.
        view.pty_output_ready();
        view.setNeedsDisplay(true);
        view
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

    /// Track which tab-strip control is under the pointer, repainting only
    /// when that changes.
    fn update_tab_bar_hover(&self, event: &NSEvent) {
        let state_ptr = self.ivars().state;
        let Some(state) = (unsafe { state_ptr.as_mut() }) else {
            return;
        };
        let location = self.convertPoint_fromView(event.locationInWindow(), None);
        let hover = tab_bar_hit(location, self.bounds(), state.sessions.len());
        if hover != state.tab_bar_hover {
            state.tab_bar_hover = hover;
            self.setNeedsDisplay(true);
        }
    }

    /// Close the window, which the app delegate turns into a full quit.
    pub(super) fn close_window(&self) {
        if let Some(window) = self.window() {
            window.performClose(None);
        }
    }

    /// Redraw when the active grid changed; background tabs are parsed but
    /// only need a repaint when their titles change.
    pub(super) fn redraw_grid_if_changed(&self, state: &mut TerminalViewState) {
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

    /// Drain a bounded PTY output batch and request a redraw. Returns true
    /// when another event is needed to continue draining a large burst.
    pub(super) fn pty_output_ready(&self) -> bool {
        let state_ptr = self.ivars().state;
        if state_ptr.is_null() {
            return false;
        }
        let state = unsafe { &mut *state_ptr };
        let needs_follow_up = state.drain_pty_output();
        self.redraw_grid_if_changed(state);
        needs_follow_up
    }
}

pub(in crate::mac_app) struct TerminalViewIvars {
    pub(super) state: *mut TerminalViewState,
    /// Clone of the event bridge inside the state, for the PTY reader thread.
    pub(super) wake: Retained<ViewWake>,
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
