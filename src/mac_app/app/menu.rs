//! Main menu. Shortcut items come from one table; the View menu lists [`Theme`]s.

use crate::terminal::theme::{self, Theme};
use objc2::runtime::Sel;
use objc2::{MainThreadMarker, MainThreadOnly, sel};
use objc2_app_kit::{
    NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSMenu, NSMenuItem,
};
use objc2_foundation::{NSString, ns_string};

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

pub(super) fn setup_main_menu(mtm: MainThreadMarker) {
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

    add_theme_menu(mtm, &menubar);
}

/// View menu listing every built-in theme. Each item carries its theme's
/// index as a tag, and the one read from the config starts out checked.
fn add_theme_menu(mtm: MainThreadMarker, menubar: &NSMenu) {
    let view_menu_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("View"),
            None,
            ns_string!(""),
        )
    };
    menubar.addItem(&view_menu_item);

    let view_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("View"));
    view_menu_item.setSubmenu(Some(&view_menu));

    let active = theme::configured_theme();
    for theme in Theme::NAMES.map(|name| Theme::parse(name).expect("theme name round-trips")) {
        let title = NSString::from_str(theme.menu_title());
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &title,
                Some(sel!(selectTheme:)),
                ns_string!(""),
            )
        };
        item.setTag(theme as isize);
        if theme == active {
            item.setState(NSControlStateValueOn);
        }
        view_menu.addItem(&item);
    }
}

/// Move the checkmark to the theme the user just picked. The view applies the
/// colors; the menu only reflects which one is current.
pub(in crate::mac_app) fn mark_theme_menu(theme: Theme) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(menubar) = NSApplication::sharedApplication(mtm).mainMenu() else {
        return;
    };
    let Some(view_menu) = (0..menubar.numberOfItems())
        .filter_map(|index| menubar.itemAtIndex(index))
        .filter_map(|item| item.submenu())
        .find(|menu| menu.title().isEqualToString(ns_string!("View")))
    else {
        return;
    };
    let selected = theme as isize;
    for index in 0..view_menu.numberOfItems() {
        let Some(item) = view_menu.itemAtIndex(index) else {
            continue;
        };
        let state = if item.tag() == selected {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        };
        item.setState(state);
    }
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
