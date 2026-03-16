mod terminal;

#[cfg(target_os = "macos")]
pub mod mac_app;

#[cfg(not(target_os = "macos"))]
pub mod fallback_app;
