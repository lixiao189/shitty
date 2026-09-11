//! Bridge from background PTY threads onto the main run loop.
//!
//! Each reader thread signals this object, which coalesces output into one
//! delayed drain per burst and forwards shell exits and resize deadlines.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_foundation::NSObject;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::time::Duration;

use super::terminal_view::ShittyTerminalView;

/// Coalescing window for bursts of PTY output.
pub(super) const OUTPUT_DEBOUNCE_DELAY: Duration = Duration::from_millis(8);

/// Objective-C target used to bridge background PTY events onto the main run loop.
/// It uses only Foundation selectors, avoiding a separate dispatch dependency.
pub(super) struct ViewWakeIvars {
    output_pending: AtomicBool,
    view: AtomicPtr<ShittyTerminalView>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = AnyThread]
    #[ivars = ViewWakeIvars]
    pub(super) struct ViewWake;

    unsafe impl NSObjectProtocol for ViewWake {}

    impl ViewWake {
        #[unsafe(method(scheduleOutputDrain:))]
        fn schedule_output_drain(&self, _sender: Option<&AnyObject>) {
            unsafe {
                let _: () = msg_send![
                    self,
                    performSelector: sel!(drainOutput:),
                    withObject: None::<&AnyObject>,
                    afterDelay: OUTPUT_DEBOUNCE_DELAY.as_secs_f64()
                ];
            }
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
    pub(super) fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(ViewWakeIvars {
            output_pending: AtomicBool::new(false),
            view: AtomicPtr::new(std::ptr::null_mut()),
        });
        unsafe { msg_send![super(this), init] }
    }

    pub(super) fn attach(&self, view: *mut ShittyTerminalView) {
        self.ivars().view.store(view, Ordering::Release);
    }

    pub(super) fn detach(&self) {
        self.ivars()
            .view
            .store(std::ptr::null_mut(), Ordering::Release);
    }

    /// Coalesce output events and drain them on the main run loop after 8 ms.
    pub(super) fn signal_output(&self) {
        if !self.ivars().output_pending.swap(true, Ordering::AcqRel) {
            unsafe {
                let _: () = msg_send![
                    self,
                    performSelectorOnMainThread: sel!(scheduleOutputDrain:),
                    withObject: None::<&AnyObject>,
                    waitUntilDone: false
                ];
            }
        }
    }

    /// Deliver a shell exit on the main thread. Every session dispatches its
    /// own callback so one exiting tab can never mask another.
    pub(super) fn signal_exit(&self) {
        unsafe {
            let _: () = msg_send![
                self,
                performSelectorOnMainThread: sel!(sessionExited:),
                withObject: None::<&AnyObject>,
                waitUntilDone: false
            ];
        }
    }

    /// Schedule the single outstanding resize debounce callback.
    pub(super) fn schedule_resize(&self, delay: Duration) {
        unsafe {
            let _: () = msg_send![
                self,
                performSelector: sel!(resizeDebounceReady:),
                withObject: None::<&AnyObject>,
                afterDelay: delay.as_secs_f64()
            ];
        }
    }
}
