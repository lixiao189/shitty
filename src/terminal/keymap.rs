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
    use objc2::runtime::Sel;

    pub(crate) fn append_input_from_selector(selector: Sel, out: &mut Vec<u8>) -> bool {
        let Ok(name) = selector.name().to_str() else {
            return false;
        };
        match name {
            "insertNewline:" => out.push(b'\r'),
            "deleteBackward:" => out.push(0x7f),
            "insertTab:" => out.push(b'\t'),
            "cancelOperation:" => out.push(0x1b),
            "moveLeft:" => out.extend_from_slice(b"\x1b[D"),
            "moveRight:" => out.extend_from_slice(b"\x1b[C"),
            "moveUp:" => out.extend_from_slice(b"\x1b[A"),
            "moveDown:" => out.extend_from_slice(b"\x1b[B"),
            _ => return false,
        }
        true
    }

    pub(crate) fn append_input_from_text(text: &str, out: &mut Vec<u8>) {
        if text.is_empty() {
            return;
        }
        if text.len() == 1 {
            let byte = text.as_bytes()[0];
            if byte < 0x20 || byte == 0x7f {
                out.push(byte);
                return;
            }
        }
        out.extend_from_slice(text.as_bytes());
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use objc2::sel;

        #[test]
        fn selector_maps_editing_commands() {
            let mut out = Vec::new();
            assert!(append_input_from_selector(sel!(insertNewline:), &mut out));
            assert_eq!(out, b"\r");

            out.clear();
            assert!(append_input_from_selector(sel!(deleteBackward:), &mut out));
            assert_eq!(out, &[0x7f]);

            out.clear();
            assert!(append_input_from_selector(sel!(moveUp:), &mut out));
            assert_eq!(out, b"\x1b[A");
        }

        #[test]
        fn text_appends_control_chars() {
            let mut out = Vec::new();
            append_input_from_text("\x03", &mut out);
            assert_eq!(out, &[0x03]);

            out.clear();
            append_input_from_text("hello", &mut out);
            assert_eq!(out, b"hello");
        }
    }
}
