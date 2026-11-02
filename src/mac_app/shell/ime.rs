//! Input-method (IME) composition state.
//!
//! The `NSTextInputClient` methods live with the view class in
//! [`super::terminal_view`]; this module owns the state transitions they drive
//! and the AppKit object conversions they need.

use objc2::rc::autoreleasepool;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSAttributedString, NSNotFound, NSRange, NSString, NSUInteger};

use super::state::TerminalViewState;

impl TerminalViewState {
    /// Whether the input method currently holds uncommitted text.
    pub(in crate::mac_app) fn has_marked_text(&self) -> bool {
        !self.marked_text.is_empty()
    }

    /// The text the input method is still composing.
    pub(in crate::mac_app) fn marked_text(&self) -> &str {
        &self.marked_text
    }

    /// NSRange covering the marked text, or an empty not-found range.
    pub(in crate::mac_app) fn marked_text_range(&self) -> NSRange {
        if self.marked_text.is_empty() {
            return NSRange::new(NSNotFound as NSUInteger, 0);
        }
        // NSString length is UTF-16 code units, same as `encode_utf16`.
        NSRange::new(0, self.marked_text.encode_utf16().count() as NSUInteger)
    }

    /// Forget any composition; used when the focused tab changes.
    pub(in crate::mac_app) fn reset_composition(&mut self) {
        self.marked_text.clear();
        self.ime_composing = false;
        self.had_composition = false;
        self.skip_next_command = false;
    }

    /// Remember the composition text reported by the input method.
    pub(in crate::mac_app) fn set_marked_text(&mut self, text: String) {
        self.marked_text = text;
        self.ime_composing = !self.marked_text.is_empty();
        self.had_composition = false;
    }

    /// Drop the marked text after the input method cancels it.
    pub(in crate::mac_app) fn unmark_text(&mut self) {
        self.marked_text.clear();
        self.ime_composing = false;
    }

    /// Start a key event: remember whether a composition preceded it.
    pub(in crate::mac_app) fn begin_key_event(&mut self) {
        self.had_composition = self.ime_composing;
        self.marked_text.clear();
        self.skip_next_command = false;
    }

    /// Commit text from the input method and report whether it ended a
    /// composition, which AppKit follows with a `doCommandBySelector:`.
    pub(in crate::mac_app) fn commit_text(&mut self) -> bool {
        let had_composition = self.had_composition;
        self.marked_text.clear();
        self.ime_composing = false;
        self.had_composition = false;
        had_composition
    }
}

/// Read an `NSString` or `NSAttributedString` argument sent by AppKit.
pub(in crate::mac_app) fn ns_object_to_string(obj: &AnyObject) -> Option<String> {
    autoreleasepool(|pool| {
        if let Some(string) = obj.downcast_ref::<NSString>() {
            return Some(unsafe { string.to_str(pool).to_string() });
        }
        if let Some(string) = obj.downcast_ref::<NSAttributedString>() {
            let ns = string.string();
            return Some(unsafe { ns.to_str(pool).to_string() });
        }
        None
    })
}
