use crate::terminal::color::{Color32, DEFAULT_BG, DEFAULT_FG, xterm_256_color};

/// Maximum number of scrollback rows kept above the visible screen.
pub(crate) const SCROLLBACK_LEN: usize = 1000;

/// A position in the terminal grid; row 0 is the top line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CellPos {
    pub row: usize,
    pub col: usize,
}

impl CellPos {
    pub(crate) const fn new(row: usize, col: usize) -> Self {
        Self { row, col }
    }
}

pub(crate) struct TerminalGrid {
    parser: vt100::Parser,
    palette: [Option<Color32>; 256],
    default_fg: Color32,
    default_bg: Color32,
    has_changes: bool,
    /// Number of scrollback rows currently available above the screen.
    max_scrollback: usize,
}

impl TerminalGrid {
    pub(crate) fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1) as u16;
        let rows = rows.max(1) as u16;
        Self {
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_LEN),
            palette: [None; 256],
            default_fg: DEFAULT_FG,
            default_bg: DEFAULT_BG,
            has_changes: false,
            max_scrollback: 0,
        }
    }

    pub(crate) fn rows(&self) -> usize {
        self.parser.screen().size().0 as usize // size() returns (rows, cols)
    }

    pub(crate) fn cols(&self) -> usize {
        self.parser.screen().size().1 as usize // size() returns (rows, cols)
    }

    pub(crate) fn default_bg(&self) -> Color32 {
        self.default_bg
    }

    pub(crate) fn cursor_visible(&self) -> bool {
        !self.parser.screen().hide_cursor()
    }

    pub(crate) fn cursor_pos(&self) -> (usize, usize) {
        let pos = self.parser.screen().cursor_position();
        (pos.0 as usize, pos.1 as usize)
    }

    pub(crate) fn resize(&mut self, cols: usize, rows: usize) -> bool {
        let cols = cols.max(1) as u16;
        let rows = rows.max(1) as u16;
        let current_size = self.parser.screen().size();
        // size() returns (rows, cols), so compare with (rows, cols)
        if (rows, cols) == current_size {
            return false;
        }
        self.parser.screen_mut().set_size(rows, cols);
        self.refresh_max_scrollback();
        self.has_changes = true;
        true
    }

    pub(crate) fn process_pty_bytes(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        self.refresh_max_scrollback();
        self.has_changes = true;
    }

    /// Check if there are any changes since the last render
    pub(crate) fn has_changes(&self) -> bool {
        self.has_changes
    }

    /// Mark the current state as rendered
    pub(crate) fn mark_rendered(&mut self) {
        self.has_changes = false;
    }

    /// How many rows the view is currently scrolled up: 0 is the normal
    /// (bottom) screen, larger values show older history.
    pub(crate) fn scroll_offset(&self) -> usize {
        self.parser.screen().scrollback()
    }

    /// How many scrollback rows are available above the screen right now.
    #[cfg(target_os = "macos")]
    pub(crate) fn max_scroll(&self) -> usize {
        self.max_scrollback
    }

    /// Scroll the view so it starts `offset` rows above the bottom. Returns
    /// whether the offset actually changed.
    #[cfg(target_os = "macos")]
    pub(crate) fn scroll_to(&mut self, offset: usize) -> bool {
        let target = offset.min(self.max_scrollback);
        if target == self.scroll_offset() {
            return false;
        }
        self.parser.screen_mut().set_scrollback(target);
        true
    }

    /// Scroll by `delta` rows; positive moves toward older history, negative
    /// back toward the live screen. Returns whether the offset changed.
    #[cfg(target_os = "macos")]
    pub(crate) fn scroll_by(&mut self, delta: isize) -> bool {
        let target =
            (self.scroll_offset() as isize + delta).clamp(0, self.max_scrollback as isize) as usize;
        self.scroll_to(target)
    }

    /// Re-measure the scrollback fill and re-clamp the current offset.
    ///
    /// vt100 only exposes the current offset, so the fill level is probed by
    /// asking for an absurd offset and reading back what it clamps to.
    fn refresh_max_scrollback(&mut self) {
        let current = self.scroll_offset();
        self.parser.screen_mut().set_scrollback(usize::MAX);
        self.max_scrollback = self.scroll_offset();
        self.parser.screen_mut().set_scrollback(current);
    }

    pub(crate) fn get_cell(&self, row: usize, col: usize) -> Option<CellInfo> {
        let screen = self.parser.screen();
        let size = screen.size(); // (rows, cols)
        if row >= size.0 as usize || col >= size.1 as usize {
            return None;
        }

        let cell = screen.cell(row as u16, col as u16)?;
        Some(CellInfo {
            text: cell.contents().to_string(),
            fg: self.resolve_color(cell.fgcolor(), true),
            bg: self.resolve_color(cell.bgcolor(), false),
            underline: cell.underline(),
            inverse: cell.inverse(),
            wide: cell.is_wide(),
            wide_continuation: cell.is_wide_continuation(),
        })
    }

    /// Clamp a pointer-derived position into the grid, moving it off the right
    /// half of a wide character so a selection always spans whole characters.
    pub(crate) fn snap_cell_pos(&self, pos: CellPos) -> CellPos {
        let row = pos.row.min(self.rows().saturating_sub(1));
        let col = pos.col.min(self.cols().saturating_sub(1));
        let col = if self
            .get_cell(row, col)
            .is_some_and(|cell| cell.wide_continuation)
        {
            col.saturating_sub(1)
        } else {
            col
        };
        CellPos::new(row, col)
    }

    pub(crate) fn resolve_cell_colors(&self, cell: &CellInfo) -> (Color32, Color32) {
        let (fg, bg) = if cell.inverse {
            (cell.bg, cell.fg)
        } else {
            (cell.fg, cell.bg)
        };
        (fg, bg)
    }

    /// Render-ready runs for `row`, tiling the whole row from column 0 to `cols()`.
    /// Adjacent cells with identical style are merged, and the right halves of
    /// wide characters are folded into their wide run, so renderers can paint
    /// each run with a single draw call.
    pub(crate) fn row_runs(&self, row: usize) -> Vec<CellRun> {
        let mut runs: Vec<CellRun> = Vec::new();
        let mut col = 0;
        while col < self.cols() {
            let run = match self.get_cell(row, col) {
                Some(cell) if !cell.wide_continuation => {
                    let (fg, bg) = self.resolve_cell_colors(&cell);
                    CellRun {
                        col,
                        width: if cell.wide { 2 } else { 1 },
                        text: cell.text,
                        fg,
                        bg,
                        underline: cell.underline,
                    }
                }
                // Right half of a wide character: already covered by the wide run.
                Some(_) => {
                    col += 1;
                    continue;
                }
                None => CellRun {
                    col,
                    width: 1,
                    text: String::new(),
                    fg: self.default_fg,
                    bg: self.default_bg,
                    underline: false,
                },
            };

            let width = run.width;
            if let Some(prev) = runs.last_mut()
                && prev.fg == run.fg
                && prev.bg == run.bg
                && prev.underline == run.underline
                && prev.col + prev.width == run.col
                // Never merge across content boundaries: a text run must not
                // absorb blank cells (or vice versa), otherwise distant text
                // would collapse into a single run drawn at the wrong column.
                && prev.text.is_empty() == run.text.is_empty()
            {
                prev.text.push_str(&run.text);
                prev.width += run.width;
            } else {
                runs.push(run);
            }
            col += width;
        }
        runs
    }

    /// Cursor position and colors, resolved for direct drawing.
    pub(crate) fn cursor(&self) -> Option<CursorInfo> {
        // While scrolled up the cursor sits somewhere in the hidden history;
        // hide it and let the next keypress snap back to the live screen.
        if !self.cursor_visible() || self.scroll_offset() > 0 {
            return None;
        }
        let (row, col) = self.cursor_pos();
        let (text, cell_fg, cell_bg) = match self.get_cell(row, col) {
            Some(cell) => {
                let (fg, bg) = self.resolve_cell_colors(&cell);
                (cell.text, fg, bg)
            }
            None => (String::new(), Color32::WHITE, self.default_bg),
        };
        // Solid-block cursor: paint with the cell foreground, falling back to
        // white when foreground and background are indistinguishable.
        let bg = if cell_fg == cell_bg {
            Color32::WHITE
        } else {
            cell_fg
        };
        let fg = if bg == cell_bg { cell_fg } else { cell_bg };
        Some(CursorInfo {
            row,
            col,
            text,
            fg,
            bg,
        })
    }

    fn resolve_color(&self, color: vt100::Color, is_fg: bool) -> Color32 {
        match color {
            vt100::Color::Default => {
                if is_fg {
                    self.default_fg
                } else {
                    self.default_bg
                }
            }
            vt100::Color::Idx(idx) => {
                if let Some(color) = self.palette[idx as usize] {
                    color
                } else {
                    xterm_256_color(idx)
                }
            }
            vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        }
    }
}

#[derive(Clone)]
pub(crate) struct CellInfo {
    pub text: String,
    pub fg: Color32,
    pub bg: Color32,
    pub underline: bool,
    pub inverse: bool,
    pub wide: bool,
    pub wide_continuation: bool,
}

/// A horizontal run of same-styled cells, ready to be painted by a renderer.
#[derive(Clone)]
pub(crate) struct CellRun {
    pub col: usize,
    /// Width in terminal columns (2 for wide characters).
    pub width: usize,
    pub text: String,
    pub fg: Color32,
    pub bg: Color32,
    pub underline: bool,
}

/// Fully resolved cursor style, ready to be painted by a renderer.
#[derive(Clone)]
pub(crate) struct CursorInfo {
    pub row: usize,
    pub col: usize,
    pub text: String,
    pub fg: Color32,
    pub bg: Color32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_runs_merges_adjacent_cells_with_the_same_style() {
        let mut grid = TerminalGrid::new(8, 1);
        grid.process_pty_bytes(b"ab");
        let runs = grid.row_runs(0);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].col, 0);
        assert_eq!(runs[0].width, 2);
        assert_eq!(runs[0].text, "ab");
        assert!(runs[1].text.is_empty());
    }

    #[test]
    fn row_runs_splits_runs_with_different_styles() {
        let mut grid = TerminalGrid::new(6, 1);
        grid.process_pty_bytes(b"\x1b[31mred\x1b[0m");
        let runs = grid.row_runs(0);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "red");
        assert_eq!(runs[0].fg, Color32::from_rgb(128, 0, 0));
        assert_eq!(runs[0].width, 3);
        assert_eq!(runs[1].text, "");
        assert_eq!(runs[1].width, 3);
    }

    #[test]
    fn row_runs_folds_wide_character_continuations() {
        let mut grid = TerminalGrid::new(4, 1);
        grid.process_pty_bytes("文".as_bytes());
        let runs = grid.row_runs(0);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "文");
        assert_eq!(runs[0].width, 2);
        assert!(runs[1].text.is_empty());
    }

    #[test]
    fn row_runs_keeps_distant_text_separate_across_blank_cells() {
        let mut grid = TerminalGrid::new(10, 1);
        grid.process_pty_bytes(b" \x1b[6Gat");
        let runs = grid.row_runs(0);
        // The trailing space (col 0) and the text at col 5 must not merge
        // across the blank cells in between.
        assert_eq!(runs[0].col, 0);
        assert_eq!(runs[0].text, " ");
        let at_run = runs.iter().find(|r| r.text == "at").expect("at run");
        assert_eq!(at_run.col, 5);
        assert_eq!(at_run.width, 2);
        // Blank cells sit between the two runs.
        assert!(runs.iter().any(|r| r.text.is_empty()));
    }

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

    #[cfg(target_os = "macos")]
    #[test]
    fn scrollback_accumulates_history() {
        let mut grid = TerminalGrid::new(4, 2);
        for i in 0..5u8 {
            grid.process_pty_bytes(&[b'a' + i, b'\r', b'\n']);
        }
        // 5 newline-terminated lines into a 2-row screen: 4 rows of history.
        assert_eq!(grid.scroll_offset(), 0);
        assert_eq!(grid.max_scroll(), 4);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scroll_to_top_reveals_oldest_lines() {
        let mut grid = TerminalGrid::new(4, 2);
        for i in 0..5u8 {
            grid.process_pty_bytes(&[b'a' + i, b'\r', b'\n']);
        }
        grid.scroll_to(4);
        assert_eq!(grid.scroll_offset(), 4);
        assert_eq!(grid.get_cell(0, 0).map(|c| c.text), Some("a".into()));
        assert_eq!(grid.get_cell(1, 0).map(|c| c.text), Some("b".into()));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scroll_offset_clamps_to_available_history() {
        let mut grid = TerminalGrid::new(4, 2);
        for i in 0..3u8 {
            grid.process_pty_bytes(&[b'a' + i, b'\r', b'\n']);
        }
        // "a\r\nb\r\nc\r\n" into 2 rows leaves 2 lines of history.
        assert_eq!(grid.max_scroll(), 2);
        assert!(grid.scroll_to(1000));
        assert_eq!(grid.scroll_offset(), 2);
        // Already at the same offset is a no-op.
        assert!(!grid.scroll_to(2));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scroll_by_moves_and_clamps() {
        let mut grid = TerminalGrid::new(4, 2);
        for i in 0..5u8 {
            grid.process_pty_bytes(&[b'a' + i, b'\r', b'\n']);
        }
        assert!(grid.scroll_by(2));
        assert_eq!(grid.scroll_offset(), 2);
        assert!(grid.scroll_by(100));
        assert_eq!(grid.scroll_offset(), 4);
        assert!(grid.scroll_by(-100));
        assert_eq!(grid.scroll_offset(), 0);
        assert!(!grid.scroll_by(-100));
    }

    #[cfg(target_os = "macos")]
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

    #[cfg(target_os = "macos")]
    #[test]
    fn view_stays_pinned_to_same_content_while_scrolled() {
        let mut grid = TerminalGrid::new(4, 2);
        for i in 0..5u8 {
            grid.process_pty_bytes(&[b'a' + i, b'\r', b'\n']);
        }
        grid.scroll_to(1);
        let before = grid.get_cell(0, 0).map(|c| c.text);
        // New output while scrolled up must not yank the view to the bottom.
        grid.process_pty_bytes(b"f\r\n");
        assert_eq!(grid.get_cell(0, 0).map(|c| c.text), before);
    }
}
