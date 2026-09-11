//! The AppKit terminal view, split by component.
//!
//! - [`terminal_view`] owns the `NSView` subclass: event handling, the
//!   `NSTextInputClient` (IME) conformance, and the view's lifetime.
//! - [`state`] is the Rust-side state shared by every tab of one view.
//! - [`tabs`] manages the tab list and the tab commands on the view.
//! - [`tab_bar`] draws the tab strip and maps clicks inside it.
//! - [`scrollbar`] draws the scrollbar and applies scroll gestures.
//! - [`renderer`] paints cells, cursor, selection, and IME text.
//! - [`resize`] turns frame changes into debounced grid and PTY resizes.
//! - [`geometry`] converts between view bounds and grid sizes.
//! - [`ime`] tracks the input-method composition state.
//! - [`wake`] bridges background PTY events onto the main run loop.

mod geometry;
mod ime;
mod renderer;
mod resize;
mod scrollbar;
mod state;
mod tab_bar;
mod tabs;
mod terminal_view;
mod wake;

pub(super) use state::TerminalViewState;
pub(super) use terminal_view::ShittyTerminalView;
