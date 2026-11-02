mod app;
mod chrome;
mod font;
mod layout;
mod render;
mod shell;

pub use app::run;

/// Locate a bundled asset such as `assets/icon.png`.
///
/// A packaged `Shitty.app` carries its assets in `Contents/Resources/`, next to
/// `Contents/MacOS/shitty`. `cargo run` falls back to the source tree.
pub(in crate::mac_app) fn asset_path(rel: &str) -> std::path::PathBuf {
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("Resources").join(rel)));
    match bundled {
        Some(path) if path.exists() => path,
        _ => std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel),
    }
}

/// Reinterpret any reference as an Objective-C `AnyObject` reference.
pub(in crate::mac_app) fn cast_any_object<T: ?Sized>(obj: &T) -> &objc2::runtime::AnyObject {
    unsafe { &*(obj as *const T as *const objc2::runtime::AnyObject) }
}
