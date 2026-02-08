use eframe::egui;

use crate::terminal::color::{DEFAULT_BG, DEFAULT_FG, xterm_256_color};

pub(crate) struct TerminalGrid {
    parser: vt100::Parser,
    palette: [Option<egui::Color32>; 256],
    default_fg: egui::Color32,
    default_bg: egui::Color32,
    cursor_color: Option<egui::Color32>,
    has_changes: bool,
}

impl TerminalGrid {
    pub(crate) fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1) as u16;
        let rows = rows.max(1) as u16;
        Self {
            parser: vt100::Parser::new(rows, cols, 0),
            palette: [None; 256],
            default_fg: DEFAULT_FG,
            default_bg: DEFAULT_BG,
            cursor_color: None,
            has_changes: false,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn rows(&self) -> usize {
        self.parser.screen().size().0 as usize // size() returns (rows, cols)
    }

    #[allow(dead_code)]
    pub(crate) fn cols(&self) -> usize {
        self.parser.screen().size().1 as usize // size() returns (rows, cols)
    }

    pub(crate) fn default_bg(&self) -> egui::Color32 {
        self.default_bg
    }

    pub(crate) fn cursor_visible(&self) -> bool {
        !self.parser.screen().hide_cursor()
    }

    pub(crate) fn cursor_color(&self) -> Option<egui::Color32> {
        self.cursor_color
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
        self.parser.set_size(rows, cols);
        self.has_changes = true;
        true
    }

    pub(crate) fn process_pty_bytes(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
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

    pub(crate) fn get_cell(&self, row: usize, col: usize) -> Option<CellInfo> {
        let screen = self.parser.screen();
        let size = screen.size(); // (rows, cols)
        if row >= size.0 as usize || col >= size.1 as usize {
            return None;
        }

        let cell = screen.cell(row as u16, col as u16)?;
        Some(CellInfo {
            text: cell.contents(),
            fg: self.resolve_color(cell.fgcolor(), true),
            bg: self.resolve_color(cell.bgcolor(), false),
            bold: cell.bold(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
        })
    }

    pub(crate) fn resolve_cell_colors(&self, cell: &CellInfo) -> (egui::Color32, egui::Color32) {
        let (fg, bg) = if cell.inverse {
            (cell.bg, cell.fg)
        } else {
            (cell.fg, cell.bg)
        };
        (fg, bg)
    }

    pub(crate) fn cell_underline(&self, cell: &CellInfo) -> bool {
        cell.underline
    }

    fn resolve_color(&self, color: vt100::Color, is_fg: bool) -> egui::Color32 {
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
            vt100::Color::Rgb(r, g, b) => egui::Color32::from_rgb(r, g, b),
        }
    }
}

#[derive(Clone)]
pub(crate) struct CellInfo {
    pub text: String,
    pub fg: egui::Color32,
    pub bg: egui::Color32,
    #[allow(dead_code)]
    pub bold: bool,
    #[allow(dead_code)]
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}
