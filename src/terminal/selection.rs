use super::grid::{CellPos, TerminalGrid};

/// A text selection anchored at one cell and extending to another.
///
/// Selections are line-based: they cover the anchor-to-head rectangle, with
/// every column of the rows in between included, which matches how terminal
/// emulators copy multi-line text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    anchor: CellPos,
    head: CellPos,
}

impl Selection {
    pub(crate) fn start(anchor: CellPos) -> Self {
        Self {
            anchor,
            head: anchor,
        }
    }

    pub(crate) fn set_head(&mut self, head: CellPos) {
        self.head = head;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Whether the cell at `(row, col)` is part of the selection.
    pub(crate) fn contains(&self, row: usize, col: usize) -> bool {
        let (start, end) = self.normalized_range();
        if row < start.row || row > end.row {
            return false;
        }
        if start.row == end.row {
            col >= start.col && col <= end.col
        } else if row == start.row {
            col >= start.col
        } else if row == end.row {
            col <= end.col
        } else {
            true
        }
    }

    /// The selected text: each covered line with trailing blanks trimmed,
    /// lines joined by newlines.
    pub(crate) fn selected_text(&self, grid: &TerminalGrid) -> String {
        let (start, end) = self.normalized_range();
        let mut text = String::new();
        for row in start.row..=end.row {
            let first_col = if row == start.row { start.col } else { 0 };
            let last_col = if row == end.row {
                end.col
            } else {
                grid.cols() - 1
            };
            let mut line = String::new();
            for col in first_col..=last_col {
                if let Some(cell) = grid.get_cell(row, col)
                    && !cell.wide_continuation
                {
                    line.push_str(&cell.text);
                }
            }
            text.push_str(line.trim_end());
            if row != end.row {
                text.push('\n');
            }
        }
        text
    }

    /// The range ordered from top-left to bottom-right.
    fn normalized_range(&self) -> (CellPos, CellPos) {
        let (start, end) = if self.anchor.row < self.head.row
            || (self.anchor.row == self.head.row && self.anchor.col <= self.head.col)
        {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        (start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(from: CellPos, to: CellPos) -> Selection {
        let mut selection = Selection::start(from);
        selection.set_head(to);
        selection
    }

    #[test]
    fn selection_normalizes_reverse_drag_direction() {
        let selection = selection(CellPos::new(2, 5), CellPos::new(1, 2));
        assert!(!selection.contains(0, 0));
        assert!(selection.contains(1, 2));
        assert!(selection.contains(1, 9));
        assert!(selection.contains(2, 0));
        assert!(selection.contains(2, 5));
        assert!(!selection.contains(2, 6));
    }

    #[test]
    fn empty_selection_contains_only_its_cell() {
        let selection = Selection::start(CellPos::new(1, 2));
        assert!(selection.is_empty());
        assert!(selection.contains(1, 2));
        assert!(!selection.contains(1, 3));
    }

    #[test]
    fn selected_text_trims_trailing_blanks_and_joins_lines() {
        let mut grid = TerminalGrid::new(6, 2);
        grid.process_pty_bytes(b"ab  \r\ncd  ");
        let selection = selection(CellPos::new(0, 0), CellPos::new(1, 5));
        assert_eq!(selection.selected_text(&grid), "ab\ncd");
    }

    #[test]
    fn selected_text_only_includes_the_partial_lines() {
        let mut grid = TerminalGrid::new(6, 3);
        grid.process_pty_bytes(b"abcdef\r\nghijkl\r\nmnopqr");
        let selection = selection(CellPos::new(0, 1), CellPos::new(2, 3));
        assert_eq!(selection.selected_text(&grid), "bcdef\nghijkl\nmnop");
    }

    #[test]
    fn snap_cell_pos_keeps_wide_characters_whole() {
        let mut grid = TerminalGrid::new(4, 1);
        grid.process_pty_bytes("文".as_bytes());
        let pos = grid.snap_cell_pos(CellPos::new(0, 1));
        assert_eq!(pos, CellPos::new(0, 0));

        let selection = selection(pos, pos);
        assert_eq!(selection.selected_text(&grid), "文");
    }

    #[test]
    fn selected_text_keeps_wide_characters_on_any_line() {
        let mut grid = TerminalGrid::new(6, 2);
        grid.process_pty_bytes("文x\r\nab".as_bytes());
        let selection = selection(CellPos::new(0, 0), CellPos::new(1, 1));
        assert_eq!(selection.selected_text(&grid), "文x\nab");
    }

    #[test]
    fn snap_cell_pos_clamps_out_of_bounds_positions() {
        let grid = TerminalGrid::new(4, 2);
        assert_eq!(grid.snap_cell_pos(CellPos::new(99, 99)), CellPos::new(1, 3));
    }
}
