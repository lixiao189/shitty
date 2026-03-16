use std::error::Error;

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn Error>> {
    shitty::mac_app::run()
}

#[cfg(not(target_os = "macos"))]
fn main() -> Result<(), Box<dyn Error>> {
    shitty::fallback_app::run()
}
