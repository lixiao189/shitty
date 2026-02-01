use eframe::egui;
use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

use crate::terminal::color::{
    ansi_16_color, parse_color_spec, xterm_256_color, ColorKind, DEFAULT_BG, DEFAULT_FG,
};

const TAB_SIZE: usize = 8;

#[derive(Clone, Copy)]
enum Charset {
    Ascii,
    DecSpecial,
}

fn map_dec_special(ch: char) -> char {
    match ch {
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'q' => '─',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        _ => ch,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Cell {
    ch: char,
    fg_kind: ColorKind,
    bg_kind: ColorKind,
    underline: bool,
    cont: bool,
}

impl Cell {
    pub(crate) fn ch(&self) -> char {
        self.ch
    }

    pub(crate) fn underline(&self) -> bool {
        self.underline
    }

    pub(crate) fn cont(&self) -> bool {
        self.cont
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg_kind: ColorKind::Default,
            bg_kind: ColorKind::Default,
            underline: false,
            cont: false,
        }
    }
}

pub(crate) struct TerminalGrid {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    cursor_row: usize,
    cursor_col: usize,
    saved_cursor: (usize, usize),
    scroll_top: usize,
    scroll_bottom: usize,
    alt_cells: Vec<Cell>,
    alt_cursor_row: usize,
    alt_cursor_col: usize,
    alt_saved_cursor: (usize, usize),
    alt_scroll_top: usize,
    alt_scroll_bottom: usize,
    in_alt: bool,
    cursor_visible: bool,
    saved_cursor_1049: Option<(usize, usize)>,
    parser: vte::Parser,
    cur_fg_kind: ColorKind,
    cur_bg_kind: ColorKind,
    cur_bold: bool,
    cur_underline: bool,
    cur_inverse: bool,
    g0: Charset,
    g1: Charset,
    use_g1: bool,
    last_printable: Option<char>,
    default_fg: egui::Color32,
    default_bg: egui::Color32,
    cursor_color: Option<egui::Color32>,
    palette: [Option<egui::Color32>; 256],
}

impl TerminalGrid {
    pub(crate) fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            cols,
            rows,
            cells: vec![Cell::default(); cols * rows],
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor: (0, 0),
            scroll_top: 0,
            scroll_bottom: rows - 1,
            alt_cells: Vec::new(),
            alt_cursor_row: 0,
            alt_cursor_col: 0,
            alt_saved_cursor: (0, 0),
            alt_scroll_top: 0,
            alt_scroll_bottom: rows - 1,
            in_alt: false,
            cursor_visible: true,
            saved_cursor_1049: None,
            parser: vte::Parser::new(),
            cur_fg_kind: ColorKind::Default,
            cur_bg_kind: ColorKind::Default,
            cur_bold: false,
            cur_underline: false,
            cur_inverse: false,
            g0: Charset::Ascii,
            g1: Charset::Ascii,
            use_g1: false,
            last_printable: None,
            default_fg: DEFAULT_FG,
            default_bg: DEFAULT_BG,
            cursor_color: None,
            palette: [None; 256],
        }
    }

    pub(crate) fn rows(&self) -> usize {
        self.rows
    }

    pub(crate) fn cols(&self) -> usize {
        self.cols
    }

    pub(crate) fn default_bg(&self) -> egui::Color32 {
        self.default_bg
    }

    pub(crate) fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    pub(crate) fn cursor_color(&self) -> Option<egui::Color32> {
        self.cursor_color
    }

    pub(crate) fn cursor_pos(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    pub(crate) fn resize(&mut self, cols: usize, rows: usize) -> bool {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return false;
        }
        self.cols = cols;
        self.rows = rows;
        self.cells = vec![Cell::default(); cols * rows];
        self.cursor_row = self.cursor_row.min(rows - 1);
        self.cursor_col = self.cursor_col.min(cols - 1);
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        if !self.alt_cells.is_empty() {
            self.alt_cells = vec![Cell::default(); cols * rows];
        }
        self.alt_cursor_row = self.alt_cursor_row.min(rows - 1);
        self.alt_cursor_col = self.alt_cursor_col.min(cols - 1);
        self.alt_scroll_top = 0;
        self.alt_scroll_bottom = rows - 1;
        true
    }

    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) {
        let mut parser = std::mem::replace(&mut self.parser, vte::Parser::new());
        for &byte in bytes {
            parser.advance(self, byte);
        }
        self.parser = parser;
    }

    pub(crate) fn cell_at(&self, row: usize, col: usize) -> Cell {
        if row < self.rows && col < self.cols {
            self.cells[self.cell_index(row, col)]
        } else {
            Cell::default()
        }
    }

    pub(crate) fn resolve_cell_colors(&self, cell: &Cell) -> (egui::Color32, egui::Color32) {
        (
            self.resolve_color(cell.fg_kind, true),
            self.resolve_color(cell.bg_kind, false),
        )
    }

    fn clear(&mut self) {
        let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
        for cell in &mut self.cells {
            cell.ch = ' ';
            cell.fg_kind = fg_kind;
            cell.bg_kind = bg_kind;
            cell.underline = underline;
            cell.cont = false;
        }
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    fn cell_index(&self, row: usize, col: usize) -> usize {
        row * self.cols + col
    }

    fn set_cell(
        &mut self,
        row: usize,
        col: usize,
        ch: char,
        fg_kind: ColorKind,
        bg_kind: ColorKind,
        underline: bool,
        cont: bool,
    ) {
        if row < self.rows && col < self.cols {
            let idx = self.cell_index(row, col);
            self.cells[idx].ch = ch;
            self.cells[idx].fg_kind = fg_kind;
            self.cells[idx].bg_kind = bg_kind;
            self.cells[idx].underline = underline;
            self.cells[idx].cont = cont;
        }
    }

    fn set_blank_cell(&mut self, row: usize, col: usize) {
        let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
        self.set_cell(row, col, ' ', fg_kind, bg_kind, underline, false);
    }

    fn clear_wide_at(&mut self, row: usize, col: usize) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let idx = self.cell_index(row, col);
        if self.cells[idx].cont {
            if col > 0 {
                self.set_blank_cell(row, col - 1);
            }
            self.cells[idx].cont = false;
        }
        if col + 1 < self.cols && self.cells[idx + 1].cont {
            self.set_blank_cell(row, col + 1);
        }
    }

    fn current_charset(&self) -> Charset {
        if self.use_g1 { self.g1 } else { self.g0 }
    }

    fn select_charset(&mut self, slot: u8, designator: u8) {
        let set = match designator {
            b'0' => Charset::DecSpecial,
            b'B' => Charset::Ascii,
            _ => Charset::Ascii,
        };
        match slot {
            b'(' => self.g0 = set,
            b')' => self.g1 = set,
            _ => {}
        }
    }

    fn map_charset_char(&self, ch: char) -> char {
        match self.current_charset() {
            Charset::Ascii => ch,
            Charset::DecSpecial => map_dec_special(ch),
        }
    }

    fn newline(&mut self) {
        if self.cursor_row >= self.scroll_bottom {
            self.scroll_up(1);
        } else {
            self.cursor_row += 1;
        }
    }

    fn scroll_up(&mut self, lines: usize) {
        let top = self.scroll_top.min(self.rows - 1);
        let bottom = self.scroll_bottom.min(self.rows - 1);
        if top >= bottom {
            return;
        }
        for _ in 0..lines {
            for row in (top + 1)..=bottom {
                let src = row * self.cols;
                let dst = (row - 1) * self.cols;
                let (left, right) = self.cells.split_at_mut(src);
                left[dst..dst + self.cols].copy_from_slice(&right[..self.cols]);
            }
            let start = bottom * self.cols;
            let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
            for cell in &mut self.cells[start..start + self.cols] {
                cell.ch = ' ';
                cell.fg_kind = fg_kind;
                cell.bg_kind = bg_kind;
                cell.underline = underline;
                cell.cont = false;
            }
        }
    }

    fn scroll_down(&mut self, lines: usize) {
        let top = self.scroll_top.min(self.rows - 1);
        let bottom = self.scroll_bottom.min(self.rows - 1);
        if top >= bottom {
            return;
        }
        for _ in 0..lines {
            for row in (top..bottom).rev() {
                let src = row * self.cols;
                let dst = (row + 1) * self.cols;
                self.cells.copy_within(src..src + self.cols, dst);
            }
            let start = top * self.cols;
            let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
            for cell in &mut self.cells[start..start + self.cols] {
                cell.ch = ' ';
                cell.fg_kind = fg_kind;
                cell.bg_kind = bg_kind;
                cell.underline = underline;
                cell.cont = false;
            }
        }
    }

    fn carriage_return(&mut self) {
        self.cursor_col = 0;
    }

    fn next_line(&mut self, n: usize) {
        let n = n.max(1);
        self.cursor_row = (self.cursor_row + n).min(self.rows - 1);
        self.cursor_col = 0;
    }

    fn prev_line(&mut self, n: usize) {
        let n = n.max(1);
        self.cursor_row = self.cursor_row.saturating_sub(n);
        self.cursor_col = 0;
    }

    fn backspace(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        }
    }

    fn tab(&mut self) {
        let next = ((self.cursor_col / TAB_SIZE) + 1) * TAB_SIZE;
        let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
        while self.cursor_col < self.cols && self.cursor_col < next {
            self.set_cell(
                self.cursor_row,
                self.cursor_col,
                ' ',
                fg_kind,
                bg_kind,
                underline,
                false,
            );
            self.cursor_col += 1;
        }
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.newline();
        }
    }

    fn put_char(&mut self, ch: char) {
        if self.cursor_row >= self.rows || self.cursor_col >= self.cols {
            return;
        }
        let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
        let width = UnicodeWidthChar::width(ch).unwrap_or(1);
        if width == 0 {
            return;
        }
        if width >= 2 {
            if self.cursor_col + 1 >= self.cols {
                self.cursor_col = 0;
                self.newline();
                if self.cursor_row >= self.rows {
                    return;
                }
            }
            self.clear_wide_at(self.cursor_row, self.cursor_col);
            self.set_cell(
                self.cursor_row,
                self.cursor_col,
                ch,
                fg_kind,
                bg_kind,
                underline,
                false,
            );
            if self.cursor_col + 1 < self.cols {
                self.clear_wide_at(self.cursor_row, self.cursor_col + 1);
                self.set_cell(
                    self.cursor_row,
                    self.cursor_col + 1,
                    ' ',
                    fg_kind,
                    bg_kind,
                    underline,
                    true,
                );
            }
            self.cursor_col += 2;
            self.last_printable = Some(ch);
        } else {
            self.clear_wide_at(self.cursor_row, self.cursor_col);
            self.set_cell(
                self.cursor_row,
                self.cursor_col,
                ch,
                fg_kind,
                bg_kind,
                underline,
                false,
            );
            self.cursor_col += 1;
            self.last_printable = Some(ch);
        }
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.newline();
        }
    }

    fn insert_lines(&mut self, n: usize) {
        if self.cursor_row < self.scroll_top || self.cursor_row > self.scroll_bottom {
            return;
        }
        let n = n.min(self.scroll_bottom - self.cursor_row + 1);
        let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
        for _ in 0..n {
            for row in (self.cursor_row..self.scroll_bottom).rev() {
                let src = row * self.cols;
                let dst = (row + 1) * self.cols;
                self.cells.copy_within(src..src + self.cols, dst);
            }
            let start = self.cursor_row * self.cols;
            for cell in &mut self.cells[start..start + self.cols] {
                cell.ch = ' ';
                cell.fg_kind = fg_kind;
                cell.bg_kind = bg_kind;
                cell.underline = underline;
                cell.cont = false;
            }
        }
    }

    fn delete_lines(&mut self, n: usize) {
        if self.cursor_row < self.scroll_top || self.cursor_row > self.scroll_bottom {
            return;
        }
        let n = n.min(self.scroll_bottom - self.cursor_row + 1);
        let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
        for _ in 0..n {
            for row in self.cursor_row..self.scroll_bottom {
                let src = (row + 1) * self.cols;
                let dst = row * self.cols;
                self.cells.copy_within(src..src + self.cols, dst);
            }
            let start = self.scroll_bottom * self.cols;
            for cell in &mut self.cells[start..start + self.cols] {
                cell.ch = ' ';
                cell.fg_kind = fg_kind;
                cell.bg_kind = bg_kind;
                cell.underline = underline;
                cell.cont = false;
            }
        }
    }

    fn insert_chars(&mut self, n: usize) {
        let row = self.cursor_row;
        if row >= self.rows || self.cursor_col >= self.cols {
            return;
        }
        let n = n.min(self.cols - self.cursor_col);
        let line_start = row * self.cols;
        let line_end = line_start + self.cols;
        self.cells.copy_within(
            line_start + self.cursor_col..line_end - n,
            line_start + self.cursor_col + n,
        );
        let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
        for col in self.cursor_col..self.cursor_col + n {
            self.set_cell(row, col, ' ', fg_kind, bg_kind, underline, false);
        }
    }

    fn delete_chars(&mut self, n: usize) {
        let row = self.cursor_row;
        if row >= self.rows || self.cursor_col >= self.cols {
            return;
        }
        let n = n.min(self.cols - self.cursor_col);
        let line_start = row * self.cols;
        let line_end = line_start + self.cols;
        self.cells.copy_within(
            line_start + self.cursor_col + n..line_end,
            line_start + self.cursor_col,
        );
        let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
        for col in (self.cols - n)..self.cols {
            self.set_cell(row, col, ' ', fg_kind, bg_kind, underline, false);
        }
    }

    fn erase_chars(&mut self, n: usize) {
        let row = self.cursor_row;
        if row >= self.rows || self.cursor_col >= self.cols {
            return;
        }
        let n = n.min(self.cols - self.cursor_col);
        let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
        for col in self.cursor_col..self.cursor_col + n {
            self.set_cell(row, col, ' ', fg_kind, bg_kind, underline, false);
        }
    }

    fn repeat_last(&mut self, n: usize) {
        if let Some(ch) = self.last_printable {
            for _ in 0..n.max(1) {
                self.put_char(ch);
            }
        }
    }

    fn param(params: &Params, idx: usize, default: u16) -> u16 {
        params
            .iter()
            .nth(idx)
            .and_then(|p| p.first().copied())
            .unwrap_or(default)
    }

    fn csi_count(params: &Params, idx: usize) -> usize {
        let mut n = Self::param(params, idx, 1);
        if n == 0 {
            n = 1;
        }
        n as usize
    }

    fn csi_position(params: &Params, idx: usize, max: usize) -> usize {
        let mut v = Self::param(params, idx, 1);
        if v == 0 {
            v = 1;
        }
        (v as usize).saturating_sub(1).min(max)
    }

    fn execute_csi(&mut self, final_byte: u8, params: &Params, private: bool) {
        match final_byte {
            b'A' => {
                let n = Self::csi_count(params, 0);
                self.cursor_row = self.cursor_row.saturating_sub(n);
            }
            b'B' => {
                let n = Self::csi_count(params, 0);
                self.cursor_row = (self.cursor_row + n).min(self.rows - 1);
            }
            b'C' => {
                let n = Self::csi_count(params, 0);
                self.cursor_col = (self.cursor_col + n).min(self.cols - 1);
            }
            b'D' => {
                let n = Self::csi_count(params, 0);
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            b'E' => {
                let n = Self::csi_count(params, 0);
                self.next_line(n);
            }
            b'F' => {
                let n = Self::csi_count(params, 0);
                self.prev_line(n);
            }
            b'G' => {
                let col = Self::csi_position(params, 0, self.cols - 1);
                self.cursor_col = col;
            }
            b'd' => {
                let row = Self::csi_position(params, 0, self.rows - 1);
                self.cursor_row = row;
            }
            b'H' | b'f' => {
                let row = Self::csi_position(params, 0, self.rows - 1);
                let col = Self::csi_position(params, 1, self.cols - 1);
                self.cursor_row = row;
                self.cursor_col = col;
            }
            b'@' => {
                let n = Self::csi_count(params, 0);
                self.insert_chars(n);
            }
            b'P' => {
                let n = Self::csi_count(params, 0);
                self.delete_chars(n);
            }
            b'X' => {
                let n = Self::csi_count(params, 0);
                self.erase_chars(n);
            }
            b'J' => self.erase_display(Self::param(params, 0, 0)),
            b'K' => self.erase_line(Self::param(params, 0, 0)),
            b'L' => {
                let n = Self::csi_count(params, 0);
                self.insert_lines(n);
            }
            b'M' => {
                let n = Self::csi_count(params, 0);
                self.delete_lines(n);
            }
            b'S' => {
                let n = Self::csi_count(params, 0);
                self.scroll_up(n);
            }
            b'T' => {
                let n = Self::csi_count(params, 0);
                self.scroll_down(n);
            }
            b'b' => {
                let n = Self::csi_count(params, 0);
                self.repeat_last(n);
            }
            b's' => self.saved_cursor = (self.cursor_row, self.cursor_col),
            b'u' => {
                let (row, col) = self.saved_cursor;
                self.cursor_row = row.min(self.rows - 1);
                self.cursor_col = col.min(self.cols - 1);
            }
            b'r' => self.set_scroll_region(params),
            b'm' => self.apply_sgr(params),
            b'h' | b'l' => {
                if private {
                    let set = final_byte == b'h';
                    for param in params.iter() {
                        if let Some(&p) = param.first() {
                            match p {
                                25 => self.cursor_visible = set,
                                47 | 1047 => {
                                    if set {
                                        self.enter_alternate(false, false);
                                    } else {
                                        self.exit_alternate(false);
                                    }
                                }
                                1049 => {
                                    if set {
                                        self.enter_alternate(true, true);
                                    } else {
                                        self.exit_alternate(true);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn erase_display(&mut self, mode: u16) {
        match mode {
            2 => self.clear(),
            3 => self.clear(),
            0 => {
                let idx = self.cell_index(self.cursor_row, self.cursor_col);
                let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
                for cell in &mut self.cells[idx..] {
                    cell.ch = ' ';
                    cell.fg_kind = fg_kind;
                    cell.bg_kind = bg_kind;
                    cell.underline = underline;
                    cell.cont = false;
                }
            }
            1 => {
                let idx = self.cell_index(self.cursor_row, self.cursor_col);
                let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
                for cell in &mut self.cells[..=idx] {
                    cell.ch = ' ';
                    cell.fg_kind = fg_kind;
                    cell.bg_kind = bg_kind;
                    cell.underline = underline;
                    cell.cont = false;
                }
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let row_start = self.cursor_row * self.cols;
        match mode {
            2 => {
                let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
                for cell in &mut self.cells[row_start..row_start + self.cols] {
                    cell.ch = ' ';
                    cell.fg_kind = fg_kind;
                    cell.bg_kind = bg_kind;
                    cell.underline = underline;
                    cell.cont = false;
                }
            }
            0 => {
                let idx = row_start + self.cursor_col;
                let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
                for cell in &mut self.cells[idx..row_start + self.cols] {
                    cell.ch = ' ';
                    cell.fg_kind = fg_kind;
                    cell.bg_kind = bg_kind;
                    cell.underline = underline;
                    cell.cont = false;
                }
            }
            1 => {
                let idx = row_start + self.cursor_col;
                let (fg_kind, bg_kind, underline) = self.current_cell_attrs();
                for cell in &mut self.cells[row_start..=idx] {
                    cell.ch = ' ';
                    cell.fg_kind = fg_kind;
                    cell.bg_kind = bg_kind;
                    cell.underline = underline;
                    cell.cont = false;
                }
            }
            _ => {}
        }
    }

    fn reset_attributes(&mut self) {
        self.cur_fg_kind = ColorKind::Default;
        self.cur_bg_kind = ColorKind::Default;
        self.cur_bold = false;
        self.cur_underline = false;
        self.cur_inverse = false;
    }

    fn apply_sgr(&mut self, params: &Params) {
        if params.is_empty() {
            self.reset_attributes();
            return;
        }
        let mut i = 0;
        let params_vec: Vec<u16> = params.iter().filter_map(|p| p.first().copied()).collect();
        while i < params_vec.len() {
            match params_vec[i] {
                0 => self.reset_attributes(),
                // Ignore bold/italic so output stays regular.
                1 | 3 => {}
                4 => self.cur_underline = true,
                7 => self.cur_inverse = true,
                22 | 23 => {}
                24 => self.cur_underline = false,
                27 => self.cur_inverse = false,
                30..=37 => {
                    self.cur_fg_kind = ColorKind::Ansi((params_vec[i] - 30) as u8);
                }
                40..=47 => {
                    self.cur_bg_kind = ColorKind::Ansi((params_vec[i] - 40) as u8);
                }
                90..=97 => {
                    self.cur_fg_kind = ColorKind::Ansi((params_vec[i] - 90 + 8) as u8);
                }
                100..=107 => {
                    self.cur_bg_kind = ColorKind::Ansi((params_vec[i] - 100 + 8) as u8);
                }
                39 => {
                    self.cur_fg_kind = ColorKind::Default;
                }
                49 => {
                    self.cur_bg_kind = ColorKind::Default;
                }
                38 | 48 => {
                    let is_fg = params_vec[i] == 38;
                    if i + 1 < params_vec.len() {
                        match params_vec[i + 1] {
                            5 if i + 2 < params_vec.len() => {
                                if is_fg {
                                    self.cur_fg_kind = ColorKind::Xterm(params_vec[i + 2] as u8);
                                } else {
                                    self.cur_bg_kind = ColorKind::Xterm(params_vec[i + 2] as u8);
                                }
                                i += 2;
                            }
                            2 if i + 4 < params_vec.len() => {
                                let r = params_vec[i + 2] as u8;
                                let g = params_vec[i + 3] as u8;
                                let b = params_vec[i + 4] as u8;
                                if is_fg {
                                    self.cur_fg_kind =
                                        ColorKind::Rgb(egui::Color32::from_rgb(r, g, b));
                                } else {
                                    self.cur_bg_kind =
                                        ColorKind::Rgb(egui::Color32::from_rgb(r, g, b));
                                }
                                i += 4;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn set_scroll_region(&mut self, params: &Params) {
        let mut top = Self::param(params, 0, 1) as usize;
        let mut bottom = Self::param(params, 1, self.rows as u16) as usize;
        if top == 0 {
            top = 1;
        }
        if bottom == 0 {
            bottom = self.rows;
        }
        let top = top.saturating_sub(1).min(self.rows - 1);
        let bottom = bottom.saturating_sub(1).min(self.rows - 1);
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        } else {
            self.scroll_top = 0;
            self.scroll_bottom = self.rows - 1;
        }
        self.cursor_row = self.scroll_top;
        self.cursor_col = 0;
    }

    fn ensure_alt_buffer(&mut self) {
        if self.alt_cells.len() != self.cols * self.rows {
            self.alt_cells = vec![Cell::default(); self.cols * self.rows];
            self.alt_cursor_row = 0;
            self.alt_cursor_col = 0;
            self.alt_saved_cursor = (0, 0);
            self.alt_scroll_top = 0;
            self.alt_scroll_bottom = self.rows - 1;
        }
    }

    fn swap_screens(&mut self) {
        std::mem::swap(&mut self.cells, &mut self.alt_cells);
        std::mem::swap(&mut self.cursor_row, &mut self.alt_cursor_row);
        std::mem::swap(&mut self.cursor_col, &mut self.alt_cursor_col);
        std::mem::swap(&mut self.saved_cursor, &mut self.alt_saved_cursor);
        std::mem::swap(&mut self.scroll_top, &mut self.alt_scroll_top);
        std::mem::swap(&mut self.scroll_bottom, &mut self.alt_scroll_bottom);
        self.in_alt = !self.in_alt;
    }

    fn enter_alternate(&mut self, save_cursor: bool, clear: bool) {
        if self.in_alt {
            return;
        }
        if save_cursor {
            self.saved_cursor_1049 = Some((self.cursor_row, self.cursor_col));
        }
        self.ensure_alt_buffer();
        self.swap_screens();
        if clear {
            self.clear();
            self.cursor_row = 0;
            self.cursor_col = 0;
            self.scroll_top = 0;
            self.scroll_bottom = self.rows - 1;
        }
    }

    fn exit_alternate(&mut self, restore_cursor: bool) {
        if !self.in_alt {
            return;
        }
        self.swap_screens();
        if restore_cursor {
            if let Some((row, col)) = self.saved_cursor_1049.take() {
                self.cursor_row = row.min(self.rows - 1);
                self.cursor_col = col.min(self.cols - 1);
            }
        }
    }

    fn reset(&mut self) {
        if self.in_alt {
            self.exit_alternate(false);
        }
        self.cur_fg_kind = ColorKind::Default;
        self.cur_bg_kind = ColorKind::Default;
        self.cur_bold = false;
        self.cur_underline = false;
        self.cur_inverse = false;
        self.g0 = Charset::Ascii;
        self.g1 = Charset::Ascii;
        self.use_g1 = false;
        self.cursor_visible = true;
        self.saved_cursor = (0, 0);
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
        self.last_printable = None;
        self.default_fg = DEFAULT_FG;
        self.default_bg = DEFAULT_BG;
        self.cursor_color = None;
        self.palette = [None; 256];
        self.clear();
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    fn effective_color_kinds(&self) -> (ColorKind, ColorKind) {
        let mut fg = self.cur_fg_kind;
        let mut bg = self.cur_bg_kind;
        if self.cur_bold {
            if let ColorKind::Ansi(idx) = fg
                && idx < 8
            {
                fg = ColorKind::Ansi(idx + 8);
            }
        }
        if self.cur_inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        (fg, bg)
    }

    fn current_cell_attrs(&self) -> (ColorKind, ColorKind, bool) {
        let (fg, bg) = self.effective_color_kinds();
        (fg, bg, self.cur_underline)
    }

    fn resolve_color(&self, kind: ColorKind, is_fg: bool) -> egui::Color32 {
        match kind {
            ColorKind::Default => {
                if is_fg {
                    self.default_fg
                } else {
                    self.default_bg
                }
            }
            ColorKind::Ansi(idx) => {
                if let Some(color) = self.palette[idx as usize] {
                    color
                } else {
                    ansi_16_color(idx)
                }
            }
            ColorKind::Xterm(idx) => {
                if let Some(color) = self.palette[idx as usize] {
                    color
                } else {
                    xterm_256_color(idx)
                }
            }
            ColorKind::Rgb(color) => color,
        }
    }
}

impl Perform for TerminalGrid {
    fn print(&mut self, c: char) {
        let ch = self.map_charset_char(c);
        self.put_char(ch);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => self.backspace(),
            0x09 => self.tab(),
            0x0a => self.newline(),
            0x0d => self.carriage_return(),
            0x0e => self.use_g1 = true,
            0x0f => self.use_g1 = false,
            _ => {}
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _c: char) {}

    fn put(&mut self, _byte: u8) {}

    fn unhook(&mut self) {}

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }
        let Ok(cmd) = std::str::from_utf8(params[0]) else {
            return;
        };
        let Ok(cmd_num) = cmd.parse::<u16>() else {
            return;
        };

        match cmd_num {
            0 | 2 => {}
            4 => {
                if params.len() < 2 {
                    return;
                }
                let mut i = 1;
                while i + 1 < params.len() {
                    let Ok(idx_str) = std::str::from_utf8(params[i]) else {
                        i += 2;
                        continue;
                    };
                    let Ok(idx) = idx_str.parse::<usize>() else {
                        i += 2;
                        continue;
                    };
                    if idx >= 256 {
                        i += 2;
                        continue;
                    }
                    let Ok(spec) = std::str::from_utf8(params[i + 1]) else {
                        i += 2;
                        continue;
                    };
                    if spec == "?" {
                        i += 2;
                        continue;
                    }
                    if let Some(color) = parse_color_spec(spec) {
                        self.palette[idx] = Some(color);
                    }
                    i += 2;
                }
            }
            10..=12 => {
                if params.len() < 2 {
                    return;
                }
                let Ok(spec) = std::str::from_utf8(params[1]) else {
                    return;
                };
                if spec == "?" {
                    return;
                }
                if let Some(color) = parse_color_spec(spec) {
                    match cmd_num {
                        10 => self.default_fg = color,
                        11 => self.default_bg = color,
                        12 => self.cursor_color = Some(color),
                        _ => {}
                    }
                }
            }
            104 => {
                if params.len() < 2 {
                    self.palette = [None; 256];
                } else {
                    for item in &params[1..] {
                        let Ok(idx_str) = std::str::from_utf8(item) else {
                            continue;
                        };
                        let Ok(idx) = idx_str.parse::<usize>() else {
                            continue;
                        };
                        if idx < 256 {
                            self.palette[idx] = None;
                        }
                    }
                }
            }
            110 => self.default_fg = DEFAULT_FG,
            111 => self.default_bg = DEFAULT_BG,
            112 => self.cursor_color = None,
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &Params,
        intermediates: &[u8],
        ignore: bool,
        c: char,
    ) {
        if ignore {
            return;
        }
        
        // Check if this is a private mode (prefixed with '?')
        let private = !intermediates.is_empty() && intermediates[0] == b'?';
        
        self.execute_csi(c as u8, params, private);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match (intermediates, byte) {
            ([], b'D') => self.newline(),
            ([], b'M') => {
                if self.cursor_row <= self.scroll_top {
                    self.scroll_down(1);
                } else {
                    self.cursor_row = self.cursor_row.saturating_sub(1);
                }
            }
            ([], b'E') => {
                self.newline();
                self.carriage_return();
            }
            ([], b'c') => self.reset(),
            ([], b'7') => self.saved_cursor = (self.cursor_row, self.cursor_col),
            ([], b'8') => {
                let (row, col) = self.saved_cursor;
                self.cursor_row = row.min(self.rows - 1);
                self.cursor_col = col.min(self.cols - 1);
            }
            ([i], designator) if *i == b'(' || *i == b')' => {
                self.select_charset(*i, designator);
            }
            _ => {}
        }
    }
}
