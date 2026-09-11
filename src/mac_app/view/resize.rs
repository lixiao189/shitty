//! Debounced frame changes: record a new grid size, then reflow every tab and
//! resize its PTY once the trailing edge of the debounce window passes.

use crate::terminal::resize::RESIZE_DEBOUNCE_DELAY;
use objc2::DefinedClass;
use objc2_foundation::NSSize;
use std::time::Instant;

use super::geometry::grid_size_for;
use super::state::TerminalViewState;
use super::terminal_view::ShittyTerminalView;
use super::wake::OUTPUT_DEBOUNCE_DELAY;

impl ShittyTerminalView {
    /// Record a window size change and schedule one trailing debounce callback.
    pub(super) fn record_grid_size(&self, size: NSSize) {
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
    pub(super) fn resize_debounce_ready(&self) {
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

    /// Re-derive the grid size from the current bounds and apply it without
    /// waiting for the resize debounce. Used when the tab strip appears or
    /// disappears, which immediately changes the terminal area.
    pub(super) fn resize_grid_now(&self, state: &mut TerminalViewState) {
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
}
