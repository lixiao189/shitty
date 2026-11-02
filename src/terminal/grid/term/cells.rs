//! Cell snapshots and the runs a renderer paints.

use super::super::{CellInfo, CellPos, CellRun};
use super::{TerminalGrid, cell_text, width_flags};
use crate::terminal::theme::{Color32, xterm_256_color};
use libghostty_vt::screen::{Cell as VtCell, CellContentTag};
use libghostty_vt::style::{StyleColor, Underline};
use libghostty_vt::terminal::{Point, PointCoordinate};

impl TerminalGrid {
    pub(crate) fn get_cell(&self, row: usize, col: usize) -> Option<CellInfo> {
        if row >= self.rows() || col >= self.cols() {
            return None;
        }
        Some(self.cell_at(row, col).unwrap_or_else(|| self.blank_cell()))
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
    ///
    /// A wide character always ends up in a run of its own (see
    /// [`CellRun::wide`]): renderers draw a run as one string starting at
    /// `col`, which is only correct while the advances the font reports match
    /// the columns the run covers.
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
                        wide: cell.wide,
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
                    wide: false,
                    text: String::new(),
                    fg: self.scheme.fg,
                    bg: self.scheme.bg,
                    underline: false,
                },
            };

            let width = run.width;
            if let Some(prev) = runs.last_mut()
                // Wide characters never merge: their glyph comes from a CJK
                // fallback face that advances one em instead of the two cells
                // they cover, so a merged run would render short and leave the
                // grid-anchored cursor drifting away from the text.
                && !prev.wide
                && !run.wide
                && prev.fg == run.fg
                && prev.bg == run.bg
                && prev.underline == run.underline
                && prev.col + prev.width == run.col
                // Never merge across content boundaries: a text run must not
                // absorb blank cells (or vice versa), otherwise distant text
                // would collapse into a single run drawn at the wrong column.
                && prev.text.is_empty() == run.text.is_empty()
                // Keep whitespace-only cells separate from each other and from
                // real text. The grid cannot distinguish an explicitly typed
                // space from an untouched cell in the occupied prefix, so this
                // preserves the old rendering where leading spaces were
                // individual runs.
                && (!prev.text.is_empty() && prev.text.trim().is_empty())
                    == (!run.text.is_empty() && run.text.trim().is_empty())
                && (prev.text.is_empty()
                    || run.text.is_empty()
                    || !prev.text.trim().is_empty()
                    || !run.text.trim().is_empty())
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

    fn cell_at(&self, row: usize, col: usize) -> Option<CellInfo> {
        let grid_ref = self
            .term
            .grid_ref(Point::Viewport(PointCoordinate {
                x: u16::try_from(col).unwrap_or(u16::MAX),
                y: u32::try_from(row).unwrap_or(u32::MAX),
            }))
            .ok()?;
        let cell = grid_ref.cell().ok()?;
        let style = grid_ref.style().unwrap_or_default();
        let (wide, wide_continuation) = width_flags(cell);
        Some(CellInfo {
            text: cell_text(&grid_ref, cell, wide_continuation),
            fg: self.resolve_style_color(style.fg_color, true),
            bg: self.background(cell, style.bg_color),
            underline: !matches!(style.underline, Underline::None),
            inverse: style.inverse,
            wide,
            wide_continuation,
        })
    }

    fn background(&self, cell: VtCell, style_bg: StyleColor) -> Color32 {
        match cell.content_tag() {
            Ok(CellContentTag::BgColorRgb) => cell
                .bg_color_rgb()
                .map(|rgb| Color32::from_rgb(rgb.r, rgb.g, rgb.b))
                .unwrap_or(self.scheme.bg),
            Ok(CellContentTag::BgColorPalette) => cell
                .bg_color_palette()
                .map(|index| self.indexed_color(index.0))
                .unwrap_or(self.scheme.bg),
            _ => self.resolve_style_color(style_bg, false),
        }
    }

    fn resolve_style_color(&self, color: StyleColor, foreground: bool) -> Color32 {
        match color {
            StyleColor::None => {
                if foreground {
                    self.scheme.fg
                } else {
                    self.scheme.bg
                }
            }
            StyleColor::Rgb(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
            StyleColor::Palette(index) => self.indexed_color(index.0),
        }
    }

    fn indexed_color(&self, index: u8) -> Color32 {
        self.scheme
            .ansi(index)
            .unwrap_or_else(|| xterm_256_color(index))
    }

    fn blank_cell(&self) -> CellInfo {
        CellInfo {
            text: String::new(),
            fg: self.scheme.fg,
            bg: self.scheme.bg,
            underline: false,
            inverse: false,
            wide: false,
            wide_continuation: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::TerminalGrid;
    use crate::terminal::theme::Color32;

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
    fn row_runs_keeps_wide_characters_anchored_to_their_lead_column() {
        let mut grid = TerminalGrid::new(6, 1);
        grid.process_pty_bytes("a中b".as_bytes());
        let runs = grid.row_runs(0);
        // Each run is drawn at its own column, so neither neighbour may absorb
        // the wide character: the CJK glyph advances one em, not two cells.
        assert_eq!(runs.len(), 4);
        assert_eq!(
            (runs[0].col, runs[0].width, runs[0].text.as_str()),
            (0, 1, "a")
        );
        assert!(!runs[0].wide);
        assert_eq!(
            (runs[1].col, runs[1].width, runs[1].text.as_str()),
            (1, 2, "中")
        );
        assert!(runs[1].wide);
        assert_eq!(
            (runs[2].col, runs[2].width, runs[2].text.as_str()),
            (3, 1, "b")
        );
        assert!(!runs[2].wide);
        assert!(runs[3].text.is_empty());
    }

    #[test]
    fn row_runs_keeps_neighbouring_wide_characters_in_separate_runs() {
        let mut grid = TerminalGrid::new(4, 1);
        grid.process_pty_bytes("中文".as_bytes());
        let runs = grid.row_runs(0);
        assert_eq!(runs.len(), 2);
        assert_eq!(
            (runs[0].col, runs[0].width, runs[0].text.as_str()),
            (0, 2, "中")
        );
        assert_eq!(
            (runs[1].col, runs[1].width, runs[1].text.as_str()),
            (2, 2, "文")
        );
        assert!(runs[0].wide && runs[1].wide);
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
}
