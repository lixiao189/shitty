//! Rust-side state owned by the terminal view: every tab plus the view-level
//! state (font metrics, IME, resize debouncing) shared by them.
//!
//! Tab bookkeeping lives in [`super::tabs`], scrolling in
//! [`super::scrollbar`], and composition state in [`super::ime`]; the fields
//! below are shared by all of them.

use crate::mac_app::font::{
    DEFAULT_FONT_SIZE, FONT_SIZE_STEP, clamp_font_size, load_terminal_font, measure_cell_size,
};
use crate::terminal::keymap;
use crate::terminal::resize::ResizeDebouncer;
use crate::terminal::session::TerminalSession;
use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSFont, NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use std::error::Error;

use super::wake::ViewWake;

/// Cap on PTY bytes parsed per event so a huge burst never freezes the main thread.
const MAX_PTY_BYTES_PER_DRAIN: usize = 256 * 1024;

/// View-wide state, shared by every tab of one window.
pub(in crate::mac_app) struct TerminalViewState {
    pub(super) sessions: Vec<TerminalSession>,
    pub(super) active: usize,
    pub(super) next_session_id: u64,
    pub(super) font: Retained<NSFont>,
    pub(super) font_size: f64,
    pub(super) cell_width: f64,
    pub(super) cell_height: f64,
    pub(super) marked_text: String,
    pub(super) ime_composing: bool,
    pub(super) had_composition: bool,
    pub(super) skip_next_command: bool,
    pub(super) resize_debouncer: ResizeDebouncer,
    /// Whether one delayed resize callback is already queued.
    pub(super) resize_task_scheduled: bool,
    /// Shared with the PTY reader threads, which signal it when output arrives.
    pub(super) wake: Retained<ViewWake>,
}

impl TerminalViewState {
    pub(in crate::mac_app) fn new(
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
    pub(super) fn active(&self) -> &TerminalSession {
        &self.sessions[self.active]
    }

    pub(super) fn active_mut(&mut self) -> &mut TerminalSession {
        &mut self.sessions[self.active]
    }

    pub(super) fn send_input(&mut self, bytes: Vec<u8>) {
        self.active_mut().send_input(bytes);
    }

    /// Clipboard shortcuts (⌘C copy / ⌘V paste, ESC cancels selection).
    /// Returns true when consumed.
    ///
    /// ⌘C is always consumed: with a selection it copies, otherwise it sends
    /// ^C to interrupt (AppKit's key binding would swallow it as `copy:`).
    pub(super) fn handle_clipboard_key(&mut self, event: &NSEvent) -> bool {
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
    pub(super) fn handle_zoom_key(&mut self, event: &NSEvent) -> bool {
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

    /// Copy the active selection to the general pasteboard.
    pub(super) fn copy_selection(&self) {
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
    pub(super) fn paste_from_pasteboard(&mut self) {
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

    /// The cursor cell rect in view coordinates, used to place the IME popup.
    pub(super) fn cursor_cell_rect(&self, bounds: NSRect) -> NSRect {
        let origin = bounds.origin;
        let (cell_w, cell_h) = (self.cell_width, self.cell_height);
        let (cursor_row, cursor_col) = self.active().grid.cursor_pos();
        let x0 = origin.x + cursor_col as f64 * cell_w;
        let y0 = origin.y + bounds.size.height - ((cursor_row + 1) as f64 * cell_h);
        NSRect::new(NSPoint::new(x0, y0), NSSize::new(cell_w, cell_h))
    }

    /// Drain pending PTY output for every session, bounded so a huge redraw
    /// burst never stalls the main thread for long.
    pub(super) fn drain_pty_output(&mut self) -> bool {
        let session_count = self.sessions.len().max(1);
        let mut budget = (MAX_PTY_BYTES_PER_DRAIN / session_count).max(4096);
        let mut needs_follow_up = false;
        for session in &mut self.sessions {
            needs_follow_up |= session.drain_output(&mut budget);
        }
        needs_follow_up
    }
}

impl Drop for TerminalViewState {
    fn drop(&mut self) {
        // Null the wake handle first so queued main-queue wake-ups can never
        // reach a dangling view pointer.
        self.wake.detach();
    }
}
