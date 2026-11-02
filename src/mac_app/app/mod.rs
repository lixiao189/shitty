use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSAutoresizingMaskOptions,
    NSBackingStoreType, NSImage, NSResponder, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, ns_string};
use std::cell::RefCell;
use std::error::Error;

mod menu;

pub(in crate::mac_app) use menu::mark_theme_menu;

use super::font::{DEFAULT_FONT_SIZE, load_terminal_font, measure_cell_size};
use super::shell::{ShittyTerminalView, TerminalViewState};
use crate::terminal::theme;

pub fn run() -> Result<(), Box<dyn Error>> {
    let mtm = MainThreadMarker::new().ok_or("must be on main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    if let Some(icon) = load_app_icon() {
        unsafe {
            app.setApplicationIconImage(Some(&icon));
        }
    }

    let delegate = ShittyAppDelegate::new(mtm)?;
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    app.run();
    Ok(())
}

fn load_app_icon() -> Option<Retained<NSImage>> {
    let icon_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon.png");
    if !icon_path.exists() {
        eprintln!("Warning: Icon file not found at {:?}", icon_path);
        return None;
    }

    let path_str = icon_path.to_str()?;
    let ns_string = NSString::from_str(path_str);
    let image = NSImage::alloc();
    let image = NSImage::initWithContentsOfFile(image, &ns_string)?;
    Some(image)
}

define_class!(
    #[unsafe(super(NSResponder))]
    #[thread_kind = objc2::MainThreadOnly]
    #[ivars = AppDelegateIvars]
    struct ShittyAppDelegate;

    unsafe impl NSObjectProtocol for ShittyAppDelegate {}

    unsafe impl NSApplicationDelegate for ShittyAppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, _notification: &AnyObject) {
            let mtm = self.mtm();
            menu::setup_main_menu(mtm);

            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0)),
                    NSWindowStyleMask::Titled
                        | NSWindowStyleMask::Closable
                        | NSWindowStyleMask::Miniaturizable
                        | NSWindowStyleMask::Resizable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };

            window.center();
            window.setTitle(ns_string!("shitty (AppKit)"));
            unsafe { window.setReleasedWhenClosed(false) };

            let view = self
                .ivars()
                .terminal_view
                .borrow()
                .as_ref()
                .cloned()
                .expect("terminal view initialized before launch");

            window.setContentView(Some(&view));
            view.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            window.makeKeyAndOrderFront(None);
            window.setAcceptsMouseMovedEvents(true);
            window.setDelegate(Some(ProtocolObject::from_ref(self)));

            self.ivars().window.replace(Some(window));
        }
    }

    unsafe impl NSWindowDelegate for ShittyAppDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &AnyObject) {
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }
);

impl ShittyAppDelegate {
    fn new(mtm: MainThreadMarker) -> Result<Retained<Self>, Box<dyn Error>> {
        let view_state = terminal_view_state()?;
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::new(mtm, view_state));
        Ok(unsafe { msg_send![super(this), init] })
    }
}

fn terminal_view_state() -> Result<TerminalViewState, Box<dyn Error>> {
    let font = load_terminal_font(DEFAULT_FONT_SIZE);
    let (cell_width, cell_height) = measure_cell_size(&font);
    TerminalViewState::new(font, cell_width, cell_height, theme::configured_theme())
}

struct AppDelegateIvars {
    window: RefCell<Option<Retained<NSWindow>>>,
    terminal_view: RefCell<Option<Retained<ShittyTerminalView>>>,
}

impl AppDelegateIvars {
    fn new(mtm: MainThreadMarker, view_state: TerminalViewState) -> Self {
        Self {
            window: RefCell::new(None),
            terminal_view: RefCell::new(Some(ShittyTerminalView::new(mtm, view_state))),
        }
    }
}
