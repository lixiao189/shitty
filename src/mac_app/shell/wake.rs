//! Bridge from background PTY threads onto the main run loop.
//!
//! Each reader thread signals this object, which coalesces output into one
//! delayed drain per burst and forwards shell exits and resize deadlines.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_foundation::NSObject;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::time::Duration;

use super::view::ShittyTerminalView;

/// Coalescing window for bursts of PTY output.
pub(in crate::mac_app) const OUTPUT_DEBOUNCE_DELAY: Duration = Duration::from_millis(8);

/// Objective-C target used to bridge background PTY events onto the main run loop.
/// It uses only Foundation selectors, avoiding a separate dispatch dependency.
pub(in crate::mac_app) struct ViewWakeIvars {
    output_pending: AtomicBool,
    view: AtomicPtr<ShittyTerminalView>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = AnyThread]
    #[ivars = ViewWakeIvars]
    pub(in crate::mac_app) struct ViewWake;

    unsafe impl NSObjectProtocol for ViewWake {}

    impl ViewWake {
        #[unsafe(method(scheduleOutputDrain:))]
        fn schedule_output_drain(&self, _sender: Option<&AnyObject>) {
            self.perform_after_delay(sel!(drainOutput:), OUTPUT_DEBOUNCE_DELAY);
        }

        #[unsafe(method(drainOutput:))]
        fn drain_output(&self, _sender: Option<&AnyObject>) {
            self.ivars().output_pending.store(false, Ordering::Release);
            let view = self.ivars().view.load(Ordering::Acquire);
            if let Some(view) = unsafe { Retained::retain(view) }
                && view.pty_output_ready()
            {
                self.signal_output();
            }
        }

        #[unsafe(method(sessionExited:))]
        fn session_exited(&self, _sender: Option<&AnyObject>) {
            let view = self.ivars().view.load(Ordering::Acquire);
            if let Some(view) = unsafe { Retained::retain(view) } {
                view.session_exited();
            }
        }

        #[unsafe(method(resizeDebounceReady:))]
        fn resize_debounce_ready(&self, _sender: Option<&AnyObject>) {
            let view = self.ivars().view.load(Ordering::Acquire);
            if let Some(view) = unsafe { Retained::retain(view) } {
                view.resize_debounce_ready();
            }
        }
    }
);

impl ViewWake {
    pub(in crate::mac_app) fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(ViewWakeIvars {
            output_pending: AtomicBool::new(false),
            view: AtomicPtr::new(std::ptr::null_mut()),
        });
        unsafe { msg_send![super(this), init] }
    }

    pub(in crate::mac_app) fn attach(&self, view: *mut ShittyTerminalView) {
        self.ivars().view.store(view, Ordering::Release);
    }

    pub(in crate::mac_app) fn detach(&self) {
        self.ivars()
            .view
            .store(std::ptr::null_mut(), Ordering::Release);
    }

    /// Coalesce output events and drain them on the main run loop after 8 ms.
    pub(in crate::mac_app) fn signal_output(&self) {
        if !self.ivars().output_pending.swap(true, Ordering::AcqRel) {
            self.perform_on_main(sel!(scheduleOutputDrain:));
        }
    }

    /// Deliver a shell exit on the main thread. Every session dispatches its
    /// own callback so one exiting tab can never mask another.
    pub(in crate::mac_app) fn signal_exit(&self) {
        self.perform_on_main(sel!(sessionExited:));
    }

    /// Schedule the single outstanding resize debounce callback.
    pub(in crate::mac_app) fn schedule_resize(&self, delay: Duration) {
        self.perform_after_delay(sel!(resizeDebounceReady:), delay);
    }

    fn perform_on_main(&self, selector: Sel) {
        unsafe {
            let _: () = msg_send![
                self,
                performSelectorOnMainThread: selector,
                withObject: None::<&AnyObject>,
                waitUntilDone: false
            ];
        }
    }

    fn perform_after_delay(&self, selector: Sel, delay: Duration) {
        unsafe {
            let _: () = msg_send![
                self,
                performSelector: selector,
                withObject: None::<&AnyObject>,
                afterDelay: delay.as_secs_f64()
            ];
        }
    }
}
