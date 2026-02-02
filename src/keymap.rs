#![allow(dead_code)]

#[cfg(not(target_os = "macos"))]
pub(crate) use egui_keymap::*;
#[cfg(target_os = "macos")]
pub(crate) use macos_keymap::*;

#[cfg(not(target_os = "macos"))]
pub mod egui_keymap {
    use eframe::egui;

    pub(crate) fn append_input_from_event(
        event: &egui::Event,
        mods: egui::Modifiers,
        out: &mut Vec<u8>,
    ) {
        match event {
            egui::Event::Text(text) => {
                if !mods.ctrl {
                    out.extend_from_slice(text.as_bytes());
                }
            }
            egui::Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } if *pressed => {
                if *key == egui::Key::Escape {
                    out.push(0x1b);
                } else if modifiers.ctrl {
                    if let Some(byte) = ctrl_key_byte(*key) {
                        out.push(byte);
                    }
                } else {
                    let _ = push_key_bytes(*key, out);
                }
            }
            _ => {}
        }
    }

    fn ctrl_key_byte(key: egui::Key) -> Option<u8> {
        let name = key.name();
        let bytes = name.as_bytes();
        if bytes.len() == 1 {
            let b = bytes[0];
            if b.is_ascii_uppercase() {
                return Some(b - b'A' + 1);
            }
        }
        None
    }

    fn push_key_bytes(key: egui::Key, out: &mut Vec<u8>) -> bool {
        match key {
            egui::Key::Enter => out.push(b'\r'),
            egui::Key::Backspace => out.push(0x7f),
            egui::Key::Tab => out.push(b'\t'),
            egui::Key::ArrowUp => out.extend_from_slice(b"\x1b[A"),
            egui::Key::ArrowDown => out.extend_from_slice(b"\x1b[B"),
            egui::Key::ArrowRight => out.extend_from_slice(b"\x1b[C"),
            egui::Key::ArrowLeft => out.extend_from_slice(b"\x1b[D"),
            _ => {
                let name = key.name();
                let Some(rest) = name.strip_prefix('F') else {
                    return false;
                };
                let Ok(n) = rest.parse::<u8>() else {
                    return false;
                };
                if !(1..=10).contains(&n) {
                    return false;
                }
                if n <= 4 {
                    out.extend_from_slice(&[0x1b, b'O', b'P' + (n - 1)]);
                } else {
                    let code = [15u8, 17, 18, 19, 20, 21][(n - 5) as usize];
                    out.extend_from_slice(b"\x1b[");
                    out.push(b'0' + (code / 10));
                    out.push(b'0' + (code % 10));
                    out.push(b'~');
                }
            }
        }
        true
    }
}

#[cfg(target_os = "macos")]
pub mod macos_keymap {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};

    pub(crate) fn append_input_from_nsevent(event: &NSEvent, out: &mut Vec<u8>) {
        if event.r#type() != NSEventType::KeyDown {
            return;
        }

        let modifiers = event.modifierFlags();
        let key_code = event.keyCode();

        // Handle special keys first
        let handled = match key_code {
            53 => {
                // Escape
                out.push(0x1b);
                true
            }
            36 => {
                // Enter
                out.push(b'\r');
                true
            }
            51 => {
                // Backspace
                out.push(0x7f);
                true
            }
            48 => {
                // Tab
                out.push(b'\t');
                true
            }
            126 => {
                // ArrowUp
                out.extend_from_slice(b"\x1b[A");
                true
            }
            125 => {
                // ArrowDown
                out.extend_from_slice(b"\x1b[B");
                true
            }
            124 => {
                // ArrowRight
                out.extend_from_slice(b"\x1b[C");
                true
            }
            123 => {
                // ArrowLeft
                out.extend_from_slice(b"\x1b[D");
                true
            }
            _ => false,
        };

        if handled {
            return;
        }

        let chars = event.characters().unwrap();
        let chars_str = autoreleasepool(|pool| unsafe { chars.to_str(pool).to_string() });

        if modifiers.contains(NSEventModifierFlags::Control) {
            if let Some(first_char) = chars_str.chars().next() {
                if first_char.is_ascii_alphabetic() {
                    out.push(first_char.to_ascii_uppercase() as u8 - b'A' + 1);
                    return;
                }
            }
        }

        if !chars_str.is_empty() {
            out.extend_from_slice(chars_str.as_bytes());
        }
    }
}
