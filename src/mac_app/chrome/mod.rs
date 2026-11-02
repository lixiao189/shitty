//! Tab strip, tab commands, and the scrollbar. Does not parse terminal bytes.

mod scrollbar;
mod tab_bar;
mod tabs;

pub(in crate::mac_app) use scrollbar::{
    SCROLLBAR_WIDTH, WHEEL_NOTCH_LINES, paint_scrollbar, scrollbar_track,
};
pub(in crate::mac_app) use tab_bar::{TabBarHit, paint_tab_bar, tab_bar_hit};
pub(in crate::mac_app) use tabs::{handle_tab_index_key, handle_tab_switch_key};
