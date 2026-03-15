mod app;
mod keymap;
#[cfg(not(target_os = "macos"))]
mod pty;
mod terminal;
#[cfg(not(target_os = "macos"))]
mod ui;

#[cfg(target_os = "macos")]
pub mod macos_ui;

pub use app::run;
