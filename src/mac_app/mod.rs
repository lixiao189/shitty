mod app;
mod font;
mod view;

pub use app::run;

/// Reinterpret any reference as an Objective-C `AnyObject` reference.
pub(super) fn cast_any_object<T: ?Sized>(obj: &T) -> &objc2::runtime::AnyObject {
    unsafe { &*(obj as *const T as *const objc2::runtime::AnyObject) }
}
