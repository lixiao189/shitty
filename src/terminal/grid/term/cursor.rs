//! Cursor snapshot. Shape comes from DECSCUSR; scrolling hides the cursor.

use super::super::{CursorInfo, CursorShape};
use super::TerminalGrid;
use crate::terminal::theme::Color32;
use libghostty_vt::render::CursorVisualStyle;

impl TerminalGrid {
    /// Cursor position and colors, resolved for direct drawing.
    pub(crate) fn cursor(&self) -> Option<CursorInfo> {
        // While scrolled up the cursor sits somewhere in the hidden history;
        // hide it and let the next keypress snap back to the live screen.
        if !self.cursor_visible() || self.scroll_offset() > 0 {
            return None;
        }
        let (row, mut col) = self.cursor_pos();
        if self
            .get_cell(row, col)
            .is_some_and(|cell| cell.wide_continuation)
        {
            col = col.saturating_sub(1);
        }
        let (text, cell_fg, cell_bg, wide) = match self.get_cell(row, col) {
            Some(cell) => {
                let (fg, bg) = self.resolve_cell_colors(&cell);
                (cell.text, fg, bg, cell.wide)
            }
            None => (String::new(), self.scheme.fg, self.scheme.bg, false),
        };
        // Solid-block cursor: paint with the cell foreground, falling back to
        // white when foreground and background are indistinguishable.
        let bg = if cell_fg == cell_bg {
            Color32::WHITE
        } else {
            cell_fg
        };
        let fg = if bg == cell_bg { cell_fg } else { cell_bg };
        let shape = self.cursor_shape(); // Shape set by the application via DECSCUSR.
        Some(CursorInfo {
            row,
            col,
            text,
            fg,
            bg,
            wide,
            shape,
        })
    }

    fn cursor_shape(&self) -> CursorShape {
        let mut render = self.render.borrow_mut();
        let Ok(snapshot) = render.update(&self.term) else {
            return CursorShape::Block;
        };
        match snapshot.cursor_visual_style() {
            Ok(CursorVisualStyle::Bar) => CursorShape::Beam,
            Ok(CursorVisualStyle::Underline) => CursorShape::Underline,
            Ok(CursorVisualStyle::BlockHollow) => CursorShape::HollowBlock,
            _ => CursorShape::Block,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::CursorShape;
    use super::super::TerminalGrid;

    #[test]
    fn cursor_reports_position() {
        let mut grid = TerminalGrid::new(8, 2);
        grid.process_pty_bytes(b"\x1b[2;3H");
        let cursor = grid.cursor().expect("cursor should be visible");
        assert_eq!((cursor.row, cursor.col), (1, 2));
    }

    #[test]
    fn cursor_is_hidden_when_asked() {
        let mut grid = TerminalGrid::new(4, 1);
        grid.process_pty_bytes(b"\x1b[?25l");
        assert!(grid.cursor().is_none());
    }

    #[test]
    fn cursor_reports_a_wide_cell_underneath_it() {
        let mut grid = TerminalGrid::new(8, 1);
        // "中文" occupies columns 0-1 and 2-3, so a cursor moved back two
        // columns sits on the leading half of "文".
        grid.process_pty_bytes("中文".as_bytes());
        grid.process_pty_bytes(b"\x1b[2D");
        let cursor = grid.cursor().expect("cursor should be visible");
        assert_eq!((cursor.row, cursor.col), (0, 2));
        assert!(cursor.wide, "the cursor block must cover both columns");
        assert_eq!(cursor.text, "文");

        // A narrow cell keeps the one-column cursor.
        grid.process_pty_bytes(b"\x1b[5G");
        let cursor = grid.cursor().expect("cursor should be visible");
        assert_eq!((cursor.row, cursor.col), (0, 4));
        assert!(!cursor.wide);
    }

    #[test]
    fn cursor_shape_follows_decscusr() {
        let mut grid = TerminalGrid::new(8, 2);

        // Steady bar: what nvim sends when entering insert mode.
        grid.process_pty_bytes(b"\x1b[6 q");
        assert_eq!(grid.cursor().unwrap().shape, CursorShape::Beam);

        // Blinking variants share the same shape.
        grid.process_pty_bytes(b"\x1b[5 q");
        assert_eq!(grid.cursor().unwrap().shape, CursorShape::Beam);

        // Steady underline.
        grid.process_pty_bytes(b"\x1b[4 q");
        assert_eq!(grid.cursor().unwrap().shape, CursorShape::Underline);

        // Back to block, then reset (0) returns to the default shape.
        grid.process_pty_bytes(b"\x1b[2 q");
        assert_eq!(grid.cursor().unwrap().shape, CursorShape::Block);
        grid.process_pty_bytes(b"\x1b[0 q");
        assert_eq!(grid.cursor().unwrap().shape, CursorShape::Block);
    }

    #[test]
    fn cursor_is_hidden_while_scrolled_up() {
        let mut grid = TerminalGrid::new(4, 2);
        for i in 0..5u8 {
            grid.process_pty_bytes(&[b'a' + i, b'\r', b'\n']);
        }
        grid.scroll_to(4);
        assert!(grid.cursor().is_none());
        grid.scroll_to(0);
        assert!(grid.cursor().is_some());
    }
}
