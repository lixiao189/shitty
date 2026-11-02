//! Key dispatch that must happen before AppKit's key-binding interpreter.

use objc2_app_kit::NSEvent;
use objc2_foundation::NSArray;

use super::state::TerminalViewState;
use super::view::ShittyTerminalView;
use crate::mac_app::chrome::{handle_tab_index_key, handle_tab_switch_key};
use crate::terminal::input as keymap;

pub(super) fn key_down(
    view: &ShittyTerminalView,
    state: Option<&mut TerminalViewState>,
    event: &NSEvent,
) {
    if let Some(state) = state {
        if state.handle_clipboard_key(event) {
            view.setNeedsDisplay(true);
            return;
        }
        // ⌘1–⌘9 focus the tab at that position; tabs past the ninth
        // stay reachable through the tab strip.
        if !state.ime_composing
            && let Some(index) = handle_tab_index_key(event)
        {
            view.select_tab_at(index);
            return;
        }
        // ⌘⇧[ / ⌘⇧] are normally consumed by the menu key
        // equivalents; this covers layouts where they are not.
        if !state.ime_composing
            && let Some(delta) = handle_tab_switch_key(event)
        {
            view.select_relative_tab(delta);
            return;
        }
        // Font zoom shortcuts (⌘+/⌘−/⌘0) are handled here rather
        // than through interpretKeyEvents:, which would ignore them.
        if !state.ime_composing && state.handle_zoom_key(event) {
            // The cell size changed, so re-derive the grid size from
            // the (unchanged) view bounds and let the debounced resize
            // path reflow the grid and resize the PTY together.
            view.record_grid_size(view.bounds().size);
            view.setNeedsDisplay(true);
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
            view.setNeedsDisplay(true);
            return;
        }
        state.begin_key_event();
    }
    let events = NSArray::from_slice(&[event]);
    view.interpretKeyEvents(&events);
}
