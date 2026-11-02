//! Tab-strip layout and painting, split so hit-testing can change without the drawer.

mod layout;
mod paint;

pub(in crate::mac_app) use layout::{TabBarHit, tab_bar_hit};
pub(in crate::mac_app) use paint::paint_tab_bar;
