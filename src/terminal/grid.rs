use crate::terminal::color::{Color32, DEFAULT_BG, DEFAULT_FG, ansi_16_color, xterm_256_color};
use crate::terminal::pty::PtyEvent;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Flags, LineLength};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor, Rgb};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use unicode_width::UnicodeWidthChar;

/// Maximum number of scrollback rows kept above the visible screen.
pub(crate) const SCROLLBACK_LEN: usize = 1000;

/// Columns `ch` occupies in the grid: two for wide (CJK) characters, none for
/// combining marks and other zero-width characters.
pub(crate) fn display_cells(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
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

/// Dimensions used when creating or resizing an alacritty terminal.
#[derive(Clone, Copy)]
struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// Buffer for terminal-generated events that need a response (DSR, color queries, ...).
#[derive(Clone)]
struct EventBuffer(Rc<RefCell<Vec<Event>>>);

impl EventListener for EventBuffer {
    fn send_event(&self, event: Event) {
        self.0.borrow_mut().push(event);
    }
}

pub(crate) struct TerminalGrid {
    term: Term<EventBuffer>,
    parser: Processor,
    palette: [Option<Color32>; 256],
    default_fg: Color32,
    default_bg: Color32,
    has_changes: bool,
    cell_width: f64,
    cell_height: f64,
    event_tx: Option<Sender<PtyEvent>>,
    pending_events: Rc<RefCell<Vec<Event>>>,
    /// Window title last set by the shell via OSC 0/2, used for tab labels.
    title: Option<String>,
}

impl TerminalGrid {
    pub(crate) fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let config = Config {
            scrolling_history: SCROLLBACK_LEN,
            ..Default::default()
        };
        let pending_events: Rc<RefCell<Vec<Event>>> = Rc::new(RefCell::new(Vec::new()));
        let listener = EventBuffer(pending_events.clone());
        Self {
            term: Term::new(config, &GridSize { cols, rows }, listener),
            parser: Processor::new(),
            palette: [None; 256],
            default_fg: DEFAULT_FG,
            default_bg: DEFAULT_BG,
            has_changes: false,
            cell_width: 1.0,
            cell_height: 1.0,
            event_tx: None,
            pending_events,
            title: None,
        }
    }

    /// Create a grid wired to a PTY writer so terminal queries can be answered.
    pub(crate) fn new_with_pty_size(
        cols: usize,
        rows: usize,
        tx_input: Sender<PtyEvent>,
        cell_width: f64,
        cell_height: f64,
    ) -> Self {
        let mut grid = Self::new(cols, rows);
        grid.event_tx = Some(tx_input);
        grid.cell_width = cell_width;
        grid.cell_height = cell_height;
        grid
    }

    /// Update the cell metrics used to answer pixel-size queries
    /// (e.g. after a font zoom); also flags the grid for redraw.
    pub(crate) fn set_cell_size(&mut self, cell_width: f64, cell_height: f64) {
        self.cell_width = cell_width;
        self.cell_height = cell_height;
        self.has_changes = true;
    }

    pub(crate) fn rows(&self) -> usize {
        self.term.screen_lines()
    }

    pub(crate) fn cols(&self) -> usize {
        self.term.columns()
    }

    pub(crate) fn default_bg(&self) -> Color32 {
        self.default_bg
    }

    /// The title set by the shell (OSC 0/2), if any.
    pub(crate) fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub(crate) fn cursor_visible(&self) -> bool {
        self.term.mode().contains(TermMode::SHOW_CURSOR)
    }

    pub(crate) fn cursor_pos(&self) -> (usize, usize) {
        let point = self.term.grid().cursor.point;
        let row = (point.line.0 + self.scroll_offset() as i32) as usize;
        (row, point.column.0)
    }

    pub(crate) fn resize(&mut self, cols: usize, rows: usize) -> bool {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols() && rows == self.rows() {
            return false;
        }
        self.term.resize(GridSize { cols, rows });
        self.has_changes = true;
        true
    }

    pub(crate) fn process_pty_bytes(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
        self.handle_terminal_events();
        self.has_changes = true;
    }

    /// Drain pending terminal events and write query responses (DSR, color, size) to the PTY.
    fn handle_terminal_events(&mut self) {
        let events = std::mem::take(&mut *self.pending_events.borrow_mut());
        for event in events {
            match event {
                Event::Title(title) => self.title = (!title.is_empty()).then_some(title),
                Event::ResetTitle => self.title = None,
                Event::PtyWrite(text) => self.write_to_pty(text.into_bytes()),
                Event::ColorRequest(index, formatter) => {
                    if let Some(color) = self.color_for_index(index) {
                        let rgb = Rgb {
                            r: color.r,
                            g: color.g,
                            b: color.b,
                        };
                        self.write_to_pty(formatter(rgb).into_bytes());
                    }
                }
                Event::TextAreaSizeRequest(formatter) => {
                    let window_size = WindowSize {
                        num_lines: self.rows() as u16,
                        num_cols: self.cols() as u16,
                        cell_width: self.cell_width as u16,
                        cell_height: self.cell_height as u16,
                    };
                    self.write_to_pty(formatter(window_size).into_bytes());
                }
                _ => {}
            }
        }
    }

    fn write_to_pty(&self, bytes: Vec<u8>) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(PtyEvent::Input(bytes));
        }
    }

    fn color_for_index(&self, index: usize) -> Option<Color32> {
        match index {
            0..=255 => Some(self.palette[index].unwrap_or_else(|| {
                if index < 16 {
                    ansi_16_color(index as u8)
                } else {
                    xterm_256_color(index as u8)
                }
            })),
            256 => Some(self.default_fg),
            257 => Some(self.default_bg),
            258 => Some(Color32::WHITE),
            259..=266 => Some(ansi_16_color((index - 259) as u8)),
            267 => Some(self.default_fg),
            268 => Some(self.default_bg),
            _ => None,
        }
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
        self.term.grid().display_offset()
    }

    /// How many scrollback rows are available above the screen right now.
    #[cfg(target_os = "macos")]
    pub(crate) fn max_scroll(&self) -> usize {
        self.term.grid().history_size()
    }

    /// Scroll the view so it starts `offset` rows above the bottom. Returns
    /// whether the offset actually changed.
    #[cfg(target_os = "macos")]
    pub(crate) fn scroll_to(&mut self, offset: usize) -> bool {
        let target = offset.min(self.max_scroll());
        if target == self.scroll_offset() {
            return false;
        }
        let delta = target as isize - self.scroll_offset() as isize;
        self.term
            .grid_mut()
            .scroll_display(Scroll::Delta(delta as i32));
        true
    }

    /// Scroll by `delta` rows; positive moves toward older history, negative
    /// back toward the live screen. Returns whether the offset changed.
    #[cfg(target_os = "macos")]
    pub(crate) fn scroll_by(&mut self, delta: isize) -> bool {
        let target =
            (self.scroll_offset() as isize + delta).clamp(0, self.max_scroll() as isize) as usize;
        self.scroll_to(target)
    }

    pub(crate) fn get_cell(&self, row: usize, col: usize) -> Option<CellInfo> {
        if row >= self.rows() || col >= self.cols() {
            return None;
        }

        let line = Line(row as i32 - self.scroll_offset() as i32);
        let cell = &self.term.grid()[Point::new(line, Column(col))];
        let wide_continuation = cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
        let is_unwritten_blank = cell.c == ' '
            && col >= self.term.grid()[line].line_length().0
            && cell.flags.is_empty()
            && cell.fg == Color::Named(NamedColor::Foreground)
            && cell.bg == Color::Named(NamedColor::Background);

        let text = if wide_continuation || is_unwritten_blank {
            String::new()
        } else {
            let mut text = cell.c.to_string();
            for c in cell.zerowidth().into_iter().flatten() {
                text.push(*c);
            }
            text
        };

        Some(CellInfo {
            text,
            fg: self.resolve_color(cell.fg),
            bg: self.resolve_color(cell.bg),
            underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
            inverse: cell.flags.contains(Flags::INVERSE),
            wide: cell.flags.contains(Flags::WIDE_CHAR),
            wide_continuation,
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
                    fg: self.default_fg,
                    bg: self.default_bg,
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
                // real text. Alacritty cannot distinguish an explicitly typed
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

    /// Cursor position and colors, resolved for direct drawing.
    pub(crate) fn cursor(&self) -> Option<CursorInfo> {
        // While scrolled up the cursor sits somewhere in the hidden history;
        // hide it and let the next keypress snap back to the live screen.
        if !self.cursor_visible() || self.scroll_offset() > 0 {
            return None;
        }
        let (row, col) = self.cursor_pos();
        let (text, cell_fg, cell_bg, wide) = match self.get_cell(row, col) {
            Some(cell) => {
                let (fg, bg) = self.resolve_cell_colors(&cell);
                (cell.text, fg, bg, cell.wide)
            }
            None => (String::new(), Color32::WHITE, self.default_bg, false),
        };
        // Solid-block cursor: paint with the cell foreground, falling back to
        // white when foreground and background are indistinguishable.
        let bg = if cell_fg == cell_bg {
            Color32::WHITE
        } else {
            cell_fg
        };
        let fg = if bg == cell_bg { cell_fg } else { cell_bg };
        let shape = self.term.cursor_style().shape; // Shape set by the application via DECSCUSR.
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

    fn resolve_color(&self, color: Color) -> Color32 {
        match color {
            Color::Named(NamedColor::Foreground) => self.default_fg,
            Color::Named(NamedColor::Background) => self.default_bg,
            Color::Named(named) => self.resolve_named_color(named),
            Color::Indexed(idx) => {
                if let Some(color) = self.palette[idx as usize] {
                    color
                } else {
                    xterm_256_color(idx)
                }
            }
            Color::Spec(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        }
    }

    fn resolve_named_color(&self, named: NamedColor) -> Color32 {
        match named as usize {
            0..=15 => ansi_16_color(named as usize as u8),
            258 => Color32::WHITE,
            259..=266 => ansi_16_color((named as usize - 259) as u8),
            267 | 268 => self.default_fg,
            _ => self.default_fg,
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

    #[test]
    #[ignore = "manual profiling harness"]
    fn profiling_parse_and_render_stress() {
        use std::time::Instant;

        let cols = 120usize;
        let rows = 40usize;
        let mut grid = TerminalGrid::new(cols, rows);

        // Simulate a full-screen redraw burst after nvim exits (cursor motion + colored text).
        let mut bytes = Vec::new();
        for _ in 0..20 {
            for r in 1..=rows {
                bytes.extend_from_slice(format!("\x1b[{};1H", r).as_bytes());
                for c in 0..cols {
                    let color = 16 + (r * 7 + c * 13) % 230;
                    bytes.extend_from_slice(format!("\x1b[38;5;{}m", color).as_bytes());
                    bytes.push(b'x');
                }
                bytes.extend_from_slice(b"\x1b[0m");
            }
        }
        bytes.extend_from_slice(b"\x1b[?1049l\x1b[2J\x1b[H");
        // Add some typical statusline/alternate-screen restore noise.
        for _ in 0..rows {
            bytes.extend_from_slice(b"\x1b[K\x1b[38;5;250m\x1b[48;5;235m \x1b[0m\r\n");
        }

        let t0 = Instant::now();
        for _ in 0..100 {
            grid.process_pty_bytes(&bytes);
        }
        let parse_dur = t0.elapsed();

        let t1 = Instant::now();
        let mut total_runs = 0usize;
        for _ in 0..1000 {
            for row in 0..grid.rows() {
                total_runs += grid.row_runs(row).len();
            }
        }
        let render_dur = t1.elapsed();

        eprintln!(
            "profile: rows={} cols={} bytes={} parse100={:?} render1000_full={:?} total_runs={}",
            rows,
            cols,
            bytes.len(),
            parse_dur,
            render_dur,
            total_runs
        );
        assert!(total_runs > 0);
    }

    #[test]
    fn terminal_queries_are_written_back_to_pty() {
        use std::sync::mpsc::channel;

        let (tx, rx) = channel::<PtyEvent>();
        let mut grid = TerminalGrid::new_with_pty_size(4, 2, tx, 1.0, 1.0);

        // Primary device attributes / DSR responses are written back to the PTY.
        grid.process_pty_bytes(b"\x1b[5n");
        match rx.try_recv() {
            Ok(PtyEvent::Input(bytes)) => assert_eq!(bytes, b"\x1b[0n"),
            Ok(_) => panic!("expected DSR response, got non-input event"),
            Err(e) => panic!("expected DSR response, got {e:?}"),
        }

        // OSC 11 (background color) queries must be answered, or nvim hangs on exit.
        grid.process_pty_bytes(b"\x1b]11;?\x07");
        match rx.try_recv() {
            Ok(PtyEvent::Input(bytes)) => {
                assert!(
                    bytes.starts_with(b"\x1b]11;rgb:"),
                    "unexpected OSC 11 response: {bytes:?}"
                );
            }
            Ok(_) => panic!("expected OSC 11 response, got non-input event"),
            Err(e) => panic!("expected OSC 11 response, got {e:?}"),
        }

        assert!(rx.try_recv().is_err(), "expected no extra PTY writes");
    }

    #[test]
    fn osc_title_events_set_and_reset_the_grid_title() {
        let mut grid = TerminalGrid::new(10, 2);
        assert_eq!(grid.title(), None);

        grid.process_pty_bytes(b"\x1b]0;my shell\x07");
        assert_eq!(grid.title(), Some("my shell"));

        grid.process_pty_bytes(b"\x1b]2;other\x1b\\");
        assert_eq!(grid.title(), Some("other"));

        grid.process_pty_bytes(b"\x1b]0;\x07");
        assert_eq!(grid.title(), None);
    }
}
