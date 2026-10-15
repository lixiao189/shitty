//! Tab management: the session list, tab commands, and tab key shortcuts.

use crate::terminal::session::{SessionOptions, TerminalSession};
use objc2::DefinedClass;
use objc2_app_kit::{NSEvent, NSEventModifierFlags};
use std::error::Error;

use super::geometry::grid_size_for;
use super::state::TerminalViewState;
use super::terminal_view::ShittyTerminalView;

impl TerminalViewState {
    /// The tab strip is only worth showing once there is more than one tab;
    /// a lone tab gets the whole view.
    pub(super) fn show_tab_bar(&self) -> bool {
        self.sessions.len() > 1
    }

    /// Open a new shell session and focus it.
    pub(super) fn spawn_session(&mut self, cols: usize, rows: usize) -> Result<(), Box<dyn Error>> {
        let id = self.next_session_id;
        self.next_session_id += 1;
        let wake_output = self.wake.clone();
        let wake_exit = self.wake.clone();
        let session = TerminalSession::spawn(
            id,
            SessionOptions {
                cols,
                rows,
                cell_width: self.cell_width,
                cell_height: self.cell_height,
                theme: self.theme,
            },
            move || wake_output.signal_output(),
            move || wake_exit.signal_exit(),
        )?;
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
        Ok(())
    }

    /// Close the tab at `index`. Returns false when it is the last tab, in
    /// which case the caller closes the window instead.
    pub(super) fn close_tab(&mut self, index: usize) -> bool {
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
    pub(super) fn remove_exited_sessions(&mut self) -> bool {
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
    pub(super) fn select_tab(&mut self, index: usize) -> bool {
        if index >= self.sessions.len() || index == self.active {
            return false;
        }
        self.active = index;
        // Composition belongs to the focused session, so it never carries over.
        self.reset_composition();
        true
    }
}

impl ShittyTerminalView {
    /// Open a tab sized to the current view. The new tab always makes the
    /// strip visible, so the session starts at the shrunken size.
    pub(super) fn create_tab(&self, state: &mut TerminalViewState) {
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

    /// Close the focused tab, or the window when it is the last one.
    pub(super) fn close_active_tab(&self) {
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
    pub(super) fn select_relative_tab(&self, delta: isize) {
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
    pub(super) fn select_tab_at(&self, index: usize) {
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
    pub(super) fn session_exited(&self) {
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
}

/// Fallback handling for ⌘⇧[ / ⌘⇧] when the menu key equivalents do not
/// consume them (e.g. on some keyboard layouts).
pub(super) fn handle_tab_switch_key(event: &NSEvent) -> bool {
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
pub(super) fn handle_tab_index_key(event: &NSEvent) -> Option<usize> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac_app::font::{DEFAULT_FONT_SIZE, load_terminal_font, measure_cell_size};

    #[test]
    fn view_state_opens_and_closes_tabs() {
        let font = load_terminal_font(DEFAULT_FONT_SIZE);
        let (cell_width, cell_height) = measure_cell_size(&font);
        let mut state = TerminalViewState::new(
            font,
            cell_width,
            cell_height,
            crate::terminal::theme::Theme::Default,
        )
        .expect("first tab spawns");

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
}
