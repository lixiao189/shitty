//! Named themes and the colors they resolve to.

mod catalog;
mod color;
mod config;
mod palette;

pub(crate) use catalog::{ColorScheme, Theme};
pub(crate) use color::{Color32, xterm_256_color};
pub(crate) use config::{configured_theme, save_theme};
