//! The AppKit view that owns tabs, focus, and the run-loop bridge.
//!
//! Drawing lives in [`crate::mac_app::render`] and the tab strip in
//! [`crate::mac_app::chrome`]. This module forwards events to those owners.

mod ime;
mod keys;
mod resize;
mod state;
mod view;
mod wake;

pub(in crate::mac_app) use state::TerminalViewState;
pub(in crate::mac_app) use view::ShittyTerminalView;
