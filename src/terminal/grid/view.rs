//! Read-only grid types. Renderers and selection depend on [`GridView`], not on
//! libghostty.

use crate::terminal::theme::Color32;
use libghostty_vt::unicode::codepoint_width;

/// Columns `ch` occupies in the grid: two for wide (CJK) characters, none for
/// combining marks and other zero-width characters.
///
/// This is libghostty's own width table, so a renderer that walks run text
/// with [`display_cells`] lands on the same columns the grid used.
pub(crate) fn display_cells(ch: char) -> usize {
    usize::from(codepoint_width(ch))
}

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

/// Cursor glyph selected with DECSCUSR.
///
/// Blinking and steady variants share a shape; the renderer paints the
/// steady form. Hiding the cursor is mode 25, which makes [`TerminalGrid::cursor`]
/// return `None` rather than a separate shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CursorShape {
    /// Filled block. The default, and DECSCUSR 0–2.
    Block,
    /// Bar at the leading edge of the cell. DECSCUSR 5–6.
    Beam,
    /// Underline along the bottom of the cell. DECSCUSR 3–4.
    Underline,
    /// Box outline.
    HollowBlock,
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
    /// True when the run holds a single wide character. Such a run is never
    /// merged with its neighbours, so a renderer can anchor the glyph to its
    /// lead column instead of relying on the font advance.
    pub wide: bool,
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
    /// The cell under the cursor holds a wide character, so a block cursor has
    /// to cover both of its columns.
    pub wide: bool,
    pub shape: CursorShape,
}

/// What the UI may read from a terminal grid. The VT backend can change as
/// long as it still exposes this view.
pub(crate) trait GridView {
    fn rows(&self) -> usize;
    fn cols(&self) -> usize;
    fn default_bg(&self) -> Color32;
    fn selection_bg(&self) -> Color32;
    fn cursor(&self) -> Option<CursorInfo>;
    fn cursor_pos(&self) -> (usize, usize);
    fn row_runs(&self, row: usize) -> Vec<CellRun>;
    fn max_scroll(&self) -> usize;
    fn scroll_offset(&self) -> usize;
    fn has_changes(&self) -> bool;
    fn snap_cell_pos(&self, pos: CellPos) -> CellPos;
    fn get_cell(&self, row: usize, col: usize) -> Option<CellInfo>;
}

#[cfg(test)]
mod tests {
    use super::super::term::TerminalGrid;
    use super::display_cells;

    #[test]
    fn display_cells_follows_terminal_width_not_byte_length() {
        assert_eq!(display_cells('a'), 1);
        assert_eq!(display_cells('中'), 2);
        assert_eq!(display_cells('，'), 2);
        // Combining acute accent: drawn on the cell it marks, not a new one.
        assert_eq!(display_cells('\u{0301}'), 0);
    }

    #[test]
    fn display_cells_matches_the_columns_the_grid_assigns() {
        // The renderer finds each character's column by accumulating
        // `display_cells` over the run text, so it has to agree with the
        // widths the grid itself used when it placed the cells — otherwise
        // glyphs would land on the wrong columns even though each one is
        // anchored, only now with the error spread unevenly.
        for ch in [
            'a', 'W', 'é', '±', 'Ω', '→', '─', '中', '，', 'ｱ', 'ｶ', '한', '😀', '❤', '\u{fe0f}',
            '\u{200d}', '\u{f015}',
        ] {
            let mut grid = TerminalGrid::new(8, 1);
            grid.process_pty_bytes(ch.to_string().as_bytes());
            let (row, col) = grid.cursor_pos();
            assert_eq!(row, 0, "U+{:04X} wrapped", ch as u32);
            assert_eq!(
                col,
                display_cells(ch),
                "U+{:04X} takes {col} column(s) in the grid, display_cells says {}",
                ch as u32,
                display_cells(ch)
            );
        }
    }
}
