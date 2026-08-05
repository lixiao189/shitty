pub(crate) use macos_keymap::*;

pub mod macos_keymap {
    use objc2::runtime::Sel;
    use objc2_app_kit::{NSEvent, NSEventModifierFlags};

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

    /// Map the unmodified character of a key to the control byte the shell
    /// expects for a Control+key press (^C = 0x03, ^D = 0x04, ^[ = ESC, ...).
    /// Follows the same table as xterm/Terminal.app for digit and symbol keys.
    pub(crate) fn control_byte_for_char(c: char) -> Option<u8> {
        let byte = match c {
            'a'..='z' => (c as u8) & 0x1f,
            'A'..='Z' => (c as u8) & 0x1f,
            '[' => 0x1b,
            '\\' => 0x1c,
            ']' => 0x1d,
            '^' => 0x1e,
            '_' => 0x1f,
            '@' => 0x00,
            ' ' => 0x00,
            '2' => 0x00,
            '3' => 0x1b,
            '4' => 0x1c,
            '5' => 0x1d,
            '6' => 0x1e,
            '7' => 0x1f,
            '8' => 0x7f,
            '/' => 0x1f,
            _ => return None,
        };
        Some(byte)
    }

    /// Translate a Control-modified key event to its raw control byte(s).
    /// Returns None for events that should keep going through the normal
    /// text-input path (no Control, or Control combined with Command).
    ///
    /// This must run before `interpretKeyEvents:`, because AppKit's key
    /// binding table swallows most Control combos as editing commands
    /// (^D -> deleteForward:, ^A/^E/^K/^U/^N/^P/^B/^F/^W/^T/^V -> editing
    /// selectors), which would never reach the shell otherwise.
    pub(crate) fn control_key_bytes(event: &NSEvent) -> Option<Vec<u8>> {
        let flags = event.modifierFlags();
        if !flags.contains(NSEventModifierFlags::Control)
            || flags.contains(NSEventModifierFlags::Command)
        {
            return None;
        }
        let chars = event.charactersIgnoringModifiers()?.to_string();
        let mut chars = chars.chars();
        let c = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        control_byte_for_char(c).map(|byte| vec![byte])
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

        #[test]
        fn control_chars_map_for_shell_keys() {
            assert_eq!(control_byte_for_char('c'), Some(0x03));
            assert_eq!(control_byte_for_char('d'), Some(0x04));
            assert_eq!(control_byte_for_char('z'), Some(0x1a));
            assert_eq!(control_byte_for_char('C'), Some(0x03));
            assert_eq!(control_byte_for_char('['), Some(0x1b));
            assert_eq!(control_byte_for_char('\\'), Some(0x1c));
            assert_eq!(control_byte_for_char(']'), Some(0x1d));
            assert_eq!(control_byte_for_char(' '), Some(0x00));
            assert_eq!(control_byte_for_char('2'), Some(0x00));
            assert_eq!(control_byte_for_char('3'), Some(0x1b));
            assert_eq!(control_byte_for_char('6'), Some(0x1e));
            assert_eq!(control_byte_for_char('8'), Some(0x7f));
            assert_eq!(control_byte_for_char('/'), Some(0x1f));
            assert_eq!(control_byte_for_char('\r'), None);
            assert_eq!(control_byte_for_char('é'), None);
        }
    }
}
