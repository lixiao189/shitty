//! Terminal core, split so each area can change without editing the others.
//!
//! - [`grid`] — libghostty VT state and the read-only [`grid::GridView`] the UI paints.
//! - [`io`] — PTY threads, per-tab sessions, and resize debouncing.
//! - [`input`] — AppKit key and paste translation.
//! - [`theme`] — palettes and the config file that selects one.
//! - [`selection`] — cell-range selection over a [`grid::GridView`].

pub(crate) mod grid;
pub(crate) mod input;
pub(crate) mod io;
pub(crate) mod selection;
pub(crate) mod theme;
