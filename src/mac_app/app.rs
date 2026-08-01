use crate::terminal::pty::{Pty, PtyEvent};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSAutoresizingMaskOptions,
    NSBackingStoreType, NSImage, NSMenu, NSMenuItem, NSResponder, NSWindow, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, ns_string};
use std::cell::RefCell;
use std::sync::mpsc::{Receiver, Sender, channel};

use super::font::{load_terminal_font, measure_cell_size};
use super::view::{ShittyTerminalView, TerminalViewState};

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let pty = Pty::spawn()?;
    let (tx_pty_output, rx_pty_output) = channel::<Vec<u8>>();
    let (tx_pty_input, rx_pty_input) = channel::<PtyEvent>();
    // The AppKit timer polls `rx_pty_output`, so no extra repaint wake-up is needed.
    pty.spawn_threads(tx_pty_output, rx_pty_input, || {});

    let mtm = MainThreadMarker::new().ok_or("must be on main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    if let Some(icon) = load_app_icon(mtm) {
        unsafe {
            app.setApplicationIconImage(Some(&icon));
        }
    }

    let delegate = ShittyAppDelegate::new(mtm, rx_pty_output, tx_pty_input);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    app.run();
    Ok(())
}

fn load_app_icon(_mtm: MainThreadMarker) -> Option<Retained<NSImage>> {
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

struct QuitMenuItemSpec {
    title: &'static str,
    key_equivalent: &'static str,
    modifier_mask: objc2_app_kit::NSEventModifierFlags,
}

fn macos_quit_menu_item_spec() -> QuitMenuItemSpec {
    QuitMenuItemSpec {
        title: "Quit shitty",
        key_equivalent: "q",
        modifier_mask: objc2_app_kit::NSEventModifierFlags::Command,
    }
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
            setup_main_menu(mtm);

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
            self.ivars().terminal_view.replace(Some(view));
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
    fn new(
        mtm: MainThreadMarker,
        rx_output: Receiver<Vec<u8>>,
        tx_input: Sender<PtyEvent>,
    ) -> Retained<Self> {
        let view_state = terminal_view_state(rx_output, tx_input);
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::new(mtm, view_state));
        unsafe { msg_send![super(this), init] }
    }
}

fn terminal_view_state(
    rx_output: Receiver<Vec<u8>>,
    tx_input: Sender<PtyEvent>,
) -> TerminalViewState {
    let font = load_terminal_font(14.0);
    let (cell_width, cell_height) = measure_cell_size(&font);
    TerminalViewState::new(rx_output, tx_input, font, cell_width, cell_height)
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

fn setup_main_menu(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);

    // Menu bar with an application menu.
    let menubar = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!(""));
    let app_menu_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!(""),
            None,
            ns_string!(""),
        )
    };
    menubar.addItem(&app_menu_item);
    app.setMainMenu(Some(&menubar));

    let app_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!(""));
    app_menu_item.setSubmenu(Some(&app_menu));

    let quit_spec = macos_quit_menu_item_spec();
    let quit_title = NSString::from_str(quit_spec.title);
    let quit_key = NSString::from_str(quit_spec.key_equivalent);
    let quit_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &quit_title,
            Some(sel!(terminate:)),
            &quit_key,
        )
    };
    quit_item.setKeyEquivalentModifierMask(quit_spec.modifier_mask);
    app_menu.addItem(&quit_item);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_command_q_quit_spec_is_standard() {
        let spec = macos_quit_menu_item_spec();
        assert_eq!(spec.title, "Quit shitty");
        assert_eq!(spec.key_equivalent, "q");
        assert!(
            spec.modifier_mask
                .contains(objc2_app_kit::NSEventModifierFlags::Command)
        );
    }
}
