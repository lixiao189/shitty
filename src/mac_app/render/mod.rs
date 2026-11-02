//! Cell-anchored painting. Depends on [`crate::terminal::grid::GridView`], not on
//! libghostty or session internals.

mod glyphs;
mod paint;

pub(in crate::mac_app) use glyphs::GlyphCache;
pub(in crate::mac_app) use paint::{paint_terminal, snap_to_pixel, text_attributes};
