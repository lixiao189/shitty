//! Key dispatch that must happen before AppKit's key-binding interpreter.

use objc2::DefinedClass;
use objc2_app_kit::NSEvent;
use objc2_foundation::NSArray;

use super::view::ShittyTerminalView;
use crate::mac_app::chrome::{handle_tab_index_key, handle_tab_switch_key};
use crate::terminal::input as keymap;

enum KeyAction {
    Consumed,
    SelectTab(usize),
    SelectRelativeTab(isize),
    Zoomed,
    Interpret,
}

pub(super) fn key_down(view: &ShittyTerminalView, event: &NSEvent) {
    let action = {
        let mut state = view.ivars().state.borrow_mut();
        if state.handle_clipboard_key(event) {
            KeyAction::Consumed
        } else if !state.ime_composing
            && let Some(index) = handle_tab_index_key(event)
        {
            // ⌘1–⌘9 focus the tab at that position; tabs past the ninth
            // stay reachable through the tab strip.
            KeyAction::SelectTab(index)
        } else if !state.ime_composing
            && let Some(delta) = handle_tab_switch_key(event)
        {
            // ⌘⇧[ / ⌘⇧] are normally consumed by the menu key
            // equivalents; this covers layouts where they are not.
            KeyAction::SelectRelativeTab(delta)
        } else if !state.ime_composing && state.handle_zoom_key(event) {
            // Font zoom shortcuts (⌘+/⌘−/⌘0) are handled here rather
            // than through interpretKeyEvents:, which would ignore them.
            KeyAction::Zoomed
        } else if !state.ime_composing
            && let Some(bytes) = keymap::control_key_bytes(event)
        {
            // Translate Control+key (^C, ^D, ...) straight to control
            // bytes. Going through interpretKeyEvents: would let
            // AppKit's key bindings swallow most of them (^D becomes
            // deleteForward:, ^A/^E/^K/^U/^N/^P/^B/^F/^W/^T/^V become
            // editing commands), so they'd never reach the shell.
            state.send_input(bytes);
            KeyAction::Consumed
        } else {
            state.begin_key_event();
            KeyAction::Interpret
        }
    };

    match action {
        KeyAction::Consumed => view.setNeedsDisplay(true),
        KeyAction::SelectTab(index) => view.select_tab_at(index),
        KeyAction::SelectRelativeTab(delta) => view.select_relative_tab(delta),
        KeyAction::Zoomed => {
            // The cell size changed, so re-derive the grid size from
            // the (unchanged) view bounds and let the debounced resize
            // path reflow the grid and resize the PTY together.
            view.record_grid_size(view.bounds().size);
            view.setNeedsDisplay(true);
        }
        KeyAction::Interpret => {
            let events = NSArray::from_slice(&[event]);
            view.interpretKeyEvents(&events);
        }
    }
}
