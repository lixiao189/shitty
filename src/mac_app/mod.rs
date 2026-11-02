mod app;
mod chrome;
mod font;
mod layout;
mod render;
mod shell;

pub use app::run;

/// Reinterpret any reference as an Objective-C `AnyObject` reference.
pub(in crate::mac_app) fn cast_any_object<T: ?Sized>(obj: &T) -> &objc2::runtime::AnyObject {
    unsafe { &*(obj as *const T as *const objc2::runtime::AnyObject) }
}
