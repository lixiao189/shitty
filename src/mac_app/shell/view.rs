//! The `NSView` subclass: its Objective-C surface, event handling, and the
//! `NSTextInputClient` (IME) conformance.

use crate::mac_app::app::mark_theme_menu;
use crate::mac_app::chrome::{
    self, SCROLLBAR_WIDTH, TabBarHit, WHEEL_NOTCH_LINES, scrollbar_track, tab_bar_hit,
};
use crate::mac_app::layout::terminal_bounds;
use crate::mac_app::render;
use crate::terminal::grid::{CellPos, GridView};
use crate::terminal::input as keymap;
use crate::terminal::selection::Selection;
use crate::terminal::theme::{self, Theme};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, Sel};
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
};
use objc2_app_kit::{NSEvent, NSTextInputClient, NSTrackingArea, NSTrackingAreaOptions, NSView};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSPoint, NSRange, NSRect, NSSize,
    NSUInteger,
};
use std::cell::RefCell;

use super::ime;
use super::state::TerminalViewState;

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
            let location = self.convertPoint_fromView(event.locationInWindow(), None);
            let bounds = self.bounds();
            let hit = tab_bar_hit(location, bounds, self.ivars().state.borrow().sessions.len());

            if let Some(hit) = hit {
                if let Some(window) = self.window() {
                    window.makeFirstResponder(Some(self));
                }
                match hit {
                    TabBarHit::Tab(index) => self.select_tab_at(index),
                    TabBarHit::Close(index) => self.close_tab_at(index),
                    TabBarHit::NewTab => self.create_tab(),
                }
                // The tab count changed under the pointer, so re-aim the hover.
                self.update_tab_bar_hover(event);
                return;
            }

            let handled_scrollbar = {
                let mut state = self.ivars().state.borrow_mut();
                if state.active().frame().max_scroll() > 0
                    && location.x >= bounds.origin.x + bounds.size.width - SCROLLBAR_WIDTH
                {
                    state.active_mut().scrollbar_dragging = true;
                    let track = scrollbar_track(terminal_bounds(bounds, state.show_tab_bar()));
                    state.scroll_to_track_y(location.y, track);
                    true
                } else {
                    let pos = self.cell_at_event(&state, event);
                    state.active_mut().selection = Some(Selection::start(pos));
                    false
                }
            };
            if !handled_scrollbar && let Some(window) = self.window() {
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
            let changed = self.ivars().state.borrow_mut().tab_bar_hover.take().is_some();
            if changed {
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let needs_display = {
                let mut state = self.ivars().state.borrow_mut();
                if state.active().scrollbar_dragging {
                    let location = self.convertPoint_fromView(event.locationInWindow(), None);
                    let track =
                        scrollbar_track(terminal_bounds(self.bounds(), state.show_tab_bar()));
                    state.scroll_to_track_y(location.y, track);
                    true
                } else {
                    let pos = self.cell_at_event(&state, event);
                    if let Some(selection) = state.active_mut().selection.as_mut() {
                        selection.set_head(pos);
                        true
                    } else {
                        false
                    }
                }
            };
            if needs_display {
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            let cleared = {
                let mut state = self.ivars().state.borrow_mut();
                state.active_mut().scrollbar_dragging = false;
                if state.active().selection.as_ref().is_some_and(|s| s.is_empty()) {
                    state.active_mut().selection = None;
                    true
                } else {
                    false
                }
            };
            if cleared {
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let mut delta = event.scrollingDeltaY();
            // Non-precise wheels report notches; scale them to whole lines.
            if !event.hasPreciseScrollingDeltas() {
                delta *= WHEEL_NOTCH_LINES;
            }
            let scrolled = self.ivars().state.borrow_mut().scroll_by_wheel_delta(delta);
            if scrolled {
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(newTab:))]
        fn new_tab(&self, _sender: Option<&AnyObject>) {
            self.create_tab();
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
            let Some(theme) = usize::try_from(index)
                .ok()
                .and_then(|i| Theme::NAMES.get(i).copied())
                .and_then(Theme::parse)
            else {
                return;
            };
            self.ivars().state.borrow_mut().set_theme(theme);
            theme::save_theme(theme);
            mark_theme_menu(theme);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            super::keys::key_down(self, event);
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let state = self.ivars().state.borrow();
            let bounds = self.bounds();
            let show_tab_bar = state.show_tab_bar();
            let scale = self
                .window()
                .map(|window| window.backingScaleFactor())
                .unwrap_or(1.0);

            render::paint_terminal(&state, bounds, scale);

            if state.active().frame().max_scroll() > 0 {
                chrome::paint_scrollbar(
                    state.active(),
                    terminal_bounds(bounds, show_tab_bar),
                    scale,
                );
            }

            if show_tab_bar {
                chrome::paint_tab_bar(&state, bounds);
            }
        }
    }

    unsafe impl NSTextInputClient for ShittyTerminalView {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text_replacement_range(&self, string: &AnyObject, _replacement_range: NSRange) {
            let Some(text) = ime::ns_object_to_string(string) else {
                return;
            };
            {
                let mut state = self.ivars().state.borrow_mut();
                if state.commit_text() {
                    state.skip_next_command = true;
                }

                let mut input_bytes = Vec::new();
                keymap::append_input_from_text(&text, &mut input_bytes);
                state.send_input(input_bytes);
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, selector: Sel) {
            let needs_display = {
                let mut state = self.ivars().state.borrow_mut();
                if state.skip_next_command {
                    state.skip_next_command = false;
                    false
                } else {
                    match selector.name().to_str() {
                        // PageUp/PageDown scroll the local history instead of reaching
                        // the shell.
                        Ok(name @ ("scrollPageUp:" | "scrollPageDown:")) => {
                            let rows = state.active().frame().rows() as isize;
                            let lines = if name == "scrollPageUp:" { rows } else { -rows };
                            state.scroll_by_lines(lines);
                            true
                        }
                        // Fallbacks for AppKit routing ⌘C/⌘V here directly.
                        Ok("paste:") => {
                            state.paste_from_pasteboard();
                            true
                        }
                        Ok("copy:") => {
                            state.copy_selection();
                            false
                        }
                        _ => {
                            let mut input_bytes = Vec::new();
                            let sent =
                                keymap::append_input_from_selector(selector, &mut input_bytes);
                            if sent {
                                state.send_input(input_bytes);
                            }
                            sent
                        }
                    }
                }
            };
            if needs_display {
                self.setNeedsDisplay(true);
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
            self.ivars().state.borrow_mut().set_marked_text(text);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            self.ivars().state.borrow_mut().unmark_text();
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            self.ivars().state.borrow().marked_text_range()
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            self.ivars().state.borrow().marked_text_range()
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> Bool {
            self.ivars().state.borrow().has_marked_text().into()
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
                return NSRect::ZERO;
            };

            let view_rect = self
                .ivars()
                .state
                .borrow()
                .cursor_cell_rect(self.bounds());
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
        let wake = state.wake.clone();
        let this = Self::alloc(mtm).set_ivars(TerminalViewIvars {
            state: RefCell::new(state),
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
        wake.attach(Retained::as_ptr(&view).cast_mut());
        // The shell may have printed its prompt before the view existed; its
        // bytes are still queued, so drain them once now.
        view.pty_output_ready();
        view.setNeedsDisplay(true);
        view
    }

    /// Convert a mouse event into a snapped grid cell.
    fn cell_at_event(&self, state: &TerminalViewState, event: &NSEvent) -> CellPos {
        let bounds = self.bounds();
        let location = self.convertPoint_fromView(event.locationInWindow(), None);
        let x = (location.x - bounds.origin.x).max(0.0);
        let terminal_height = terminal_bounds(bounds, state.show_tab_bar()).size.height;
        let y = (terminal_height - (location.y - bounds.origin.y)).max(0.0);
        let pos = CellPos::new(
            (y / state.cell_height) as usize,
            (x / state.cell_width) as usize,
        );
        state.active().frame().snap_cell_pos(pos)
    }

    /// Track which tab-strip control is under the pointer, repainting only
    /// when that changes.
    fn update_tab_bar_hover(&self, event: &NSEvent) {
        let location = self.convertPoint_fromView(event.locationInWindow(), None);
        let bounds = self.bounds();
        let changed = {
            let mut state = self.ivars().state.borrow_mut();
            let hover = tab_bar_hit(location, bounds, state.sessions.len());
            let changed = hover != state.tab_bar_hover;
            state.tab_bar_hover = hover;
            changed
        };
        if changed {
            self.setNeedsDisplay(true);
        }
    }

    /// Close the window, which the app delegate turns into a full quit.
    pub(in crate::mac_app) fn close_window(&self) {
        if let Some(window) = self.window() {
            window.performClose(None);
        }
    }

    /// Redraw when the active grid changed; background tabs are parsed but
    /// only need a repaint when their titles change.
    pub(in crate::mac_app) fn redraw_grid_if_changed(&self, state: &mut TerminalViewState) {
        let active = state.active;
        let mut tab_bar_dirty = false;
        for (index, session) in state.sessions.iter_mut().enumerate() {
            tab_bar_dirty |= session.sync_title();
            if session.frame().has_changes() {
                session.mark_rendered();
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
    pub(in crate::mac_app) fn pty_output_ready(&self) -> bool {
        let mut state = self.ivars().state.borrow_mut();
        let needs_follow_up = state.drain_pty_output();
        self.redraw_grid_if_changed(&mut state);
        needs_follow_up
    }
}

pub(in crate::mac_app) struct TerminalViewIvars {
    pub(in crate::mac_app) state: RefCell<TerminalViewState>,
}
