mod keymap;
mod terminal;

#[cfg(target_os = "macos")]
pub mod mac_app;

#[cfg(not(target_os = "macos"))]
pub mod app;
#[cfg(not(target_os = "macos"))]
mod pty;
#[cfg(not(target_os = "macos"))]
mod ui;
