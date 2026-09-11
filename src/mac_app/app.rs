use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject, Sel};
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
use std::error::Error;

use super::font::{DEFAULT_FONT_SIZE, load_terminal_font, measure_cell_size};
use super::view::{ShittyTerminalView, TerminalViewState};

pub fn run() -> Result<(), Box<dyn Error>> {
    let mtm = MainThreadMarker::new().ok_or("must be on main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    if let Some(icon) = load_app_icon(mtm) {
        unsafe {
            app.setApplicationIconImage(Some(&icon));
        }
    }

    let delegate = ShittyAppDelegate::new(mtm)?;
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

struct ShortcutMenuItemSpec {
    title: &'static str,
    key_equivalent: &'static str,
    modifier_mask: objc2_app_kit::NSEventModifierFlags,
}

fn macos_quit_menu_item_spec() -> ShortcutMenuItemSpec {
    ShortcutMenuItemSpec {
        title: "Quit shitty",
        key_equivalent: "q",
        modifier_mask: objc2_app_kit::NSEventModifierFlags::Command,
    }
}

fn macos_new_tab_menu_item_spec() -> ShortcutMenuItemSpec {
    ShortcutMenuItemSpec {
        title: "New Tab",
        key_equivalent: "t",
        modifier_mask: objc2_app_kit::NSEventModifierFlags::Command,
    }
}

fn macos_close_tab_menu_item_spec() -> ShortcutMenuItemSpec {
    ShortcutMenuItemSpec {
        title: "Close Tab",
        key_equivalent: "w",
        modifier_mask: objc2_app_kit::NSEventModifierFlags::Command,
    }
}

fn macos_close_window_menu_item_spec() -> ShortcutMenuItemSpec {
    // An uppercase key equivalent implies Shift, i.e. ⌘⇧W.
    ShortcutMenuItemSpec {
        title: "Close Window",
        key_equivalent: "W",
        modifier_mask: objc2_app_kit::NSEventModifierFlags::Command,
    }
}

fn macos_next_tab_menu_item_spec() -> ShortcutMenuItemSpec {
    // Shift is carried by the shifted character itself, i.e. ⌘⇧].
    ShortcutMenuItemSpec {
        title: "Show Next Tab",
        key_equivalent: "}",
        modifier_mask: objc2_app_kit::NSEventModifierFlags::Command,
    }
}

fn macos_previous_tab_menu_item_spec() -> ShortcutMenuItemSpec {
    ShortcutMenuItemSpec {
        title: "Show Previous Tab",
        key_equivalent: "{",
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
    fn new(mtm: MainThreadMarker) -> Result<Retained<Self>, Box<dyn Error>> {
        let view_state = terminal_view_state()?;
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::new(mtm, view_state));
        Ok(unsafe { msg_send![super(this), init] })
    }
}

fn terminal_view_state() -> Result<TerminalViewState, Box<dyn Error>> {
    let font = load_terminal_font(DEFAULT_FONT_SIZE);
    let (cell_width, cell_height) = measure_cell_size(&font);
    TerminalViewState::new(font, cell_width, cell_height)
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
    add_shortcut_item(
        mtm,
        &app_menu,
        macos_quit_menu_item_spec(),
        sel!(terminate:),
    );

    let file_menu_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("File"),
            None,
            ns_string!(""),
        )
    };
    menubar.addItem(&file_menu_item);

    let file_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("File"));
    file_menu_item.setSubmenu(Some(&file_menu));

    add_shortcut_item(
        mtm,
        &file_menu,
        macos_new_tab_menu_item_spec(),
        sel!(newTab:),
    );
    add_shortcut_item(
        mtm,
        &file_menu,
        macos_close_tab_menu_item_spec(),
        sel!(closeTab:),
    );
    file_menu.addItem(&NSMenuItem::separatorItem(mtm));
    add_shortcut_item(
        mtm,
        &file_menu,
        macos_close_window_menu_item_spec(),
        sel!(performClose:),
    );
    file_menu.addItem(&NSMenuItem::separatorItem(mtm));
    add_shortcut_item(
        mtm,
        &file_menu,
        macos_previous_tab_menu_item_spec(),
        sel!(selectPreviousTab:),
    );
    add_shortcut_item(
        mtm,
        &file_menu,
        macos_next_tab_menu_item_spec(),
        sel!(selectNextTab:),
    );
}

fn add_shortcut_item(
    mtm: MainThreadMarker,
    menu: &NSMenu,
    spec: ShortcutMenuItemSpec,
    action: Sel,
) {
    let title = NSString::from_str(spec.title);
    let key = NSString::from_str(spec.key_equivalent);
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &title,
            Some(action),
            &key,
        )
    };
    item.setKeyEquivalentModifierMask(spec.modifier_mask);
    menu.addItem(&item);
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

    #[test]
    fn macos_command_t_new_tab_spec_is_standard() {
        let spec = macos_new_tab_menu_item_spec();
        assert_eq!(spec.title, "New Tab");
        assert_eq!(spec.key_equivalent, "t");
        assert!(
            spec.modifier_mask
                .contains(objc2_app_kit::NSEventModifierFlags::Command)
        );
    }

    #[test]
    fn macos_command_w_closes_a_tab() {
        let spec = macos_close_tab_menu_item_spec();
        assert_eq!(spec.title, "Close Tab");
        assert_eq!(spec.key_equivalent, "w");
        assert!(
            spec.modifier_mask
                .contains(objc2_app_kit::NSEventModifierFlags::Command)
        );
    }

    #[test]
    fn macos_command_shift_w_close_window_spec_is_standard() {
        let spec = macos_close_window_menu_item_spec();
        assert_eq!(spec.title, "Close Window");
        assert_eq!(spec.key_equivalent, "W");
        assert!(
            spec.modifier_mask
                .contains(objc2_app_kit::NSEventModifierFlags::Command)
        );
    }

    #[test]
    fn macos_command_shift_bracket_switches_tabs() {
        let next = macos_next_tab_menu_item_spec();
        assert_eq!(next.key_equivalent, "}");
        let previous = macos_previous_tab_menu_item_spec();
        assert_eq!(previous.key_equivalent, "{");
        for spec in [next, previous] {
            assert!(
                spec.modifier_mask
                    .contains(objc2_app_kit::NSEventModifierFlags::Command)
            );
            assert!(
                !spec
                    .modifier_mask
                    .contains(objc2_app_kit::NSEventModifierFlags::Shift)
            );
        }
    }
}
