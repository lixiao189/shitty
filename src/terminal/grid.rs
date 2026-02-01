use eframe::egui;
use termwiz::cell::{CellAttributes, Underline};
use termwiz::color::{ColorAttribute, SrgbaTuple};
use termwiz::escape::{Action, ControlCode, Esc, EscCode, OperatingSystemCommand};
use termwiz::escape::parser::Parser;
use termwiz::surface::{Change, CursorVisibility, Line, Position, Surface};

use crate::terminal::color::{xterm_256_color, DEFAULT_BG, DEFAULT_FG};

const TAB_SIZE: usize = 8;

type Params = Vec<Vec<u16>>;

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

pub(crate) struct TerminalGrid {
    surface: Surface,
    parser: Parser,
    cur_attrs: CellAttributes,
    cur_fg_base: Option<u8>,
    bold: bool,
    palette: [Option<egui::Color32>; 256],
    default_fg: egui::Color32,
    default_bg: egui::Color32,
    cursor_color: Option<egui::Color32>,
    scroll_top: usize,
    scroll_bottom: usize,
    saved_cursor: (usize, usize),
    alt_surface: Option<Surface>,
    alt_saved_cursor: (usize, usize),
    alt_scroll_top: usize,
    alt_scroll_bottom: usize,
    in_alt: bool,
    saved_cursor_1049: Option<(usize, usize)>,
    g0: Charset,
    g1: Charset,
    use_g1: bool,
}

impl TerminalGrid {
    pub(crate) fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            surface: Surface::new(cols, rows),
            parser: Parser::new(),
            cur_attrs: CellAttributes::default(),
            cur_fg_base: None,
            bold: false,
            palette: [None; 256],
            default_fg: DEFAULT_FG,
            default_bg: DEFAULT_BG,
            cursor_color: None,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            saved_cursor: (0, 0),
            alt_surface: None,
            alt_saved_cursor: (0, 0),
            alt_scroll_top: 0,
            alt_scroll_bottom: rows - 1,
            in_alt: false,
            saved_cursor_1049: None,
            g0: Charset::Ascii,
            g1: Charset::Ascii,
            use_g1: false,
        }
    }

    pub(crate) fn rows(&self) -> usize {
        self.surface.dimensions().1
    }

    pub(crate) fn cols(&self) -> usize {
        self.surface.dimensions().0
    }

    pub(crate) fn default_bg(&self) -> egui::Color32 {
        self.default_bg
    }

    pub(crate) fn cursor_visible(&self) -> bool {
        self.surface.cursor_visibility() == CursorVisibility::Visible
    }

    pub(crate) fn cursor_color(&self) -> Option<egui::Color32> {
        self.cursor_color
    }

    pub(crate) fn cursor_pos(&self) -> (usize, usize) {
        let (col, row) = self.surface.cursor_position();
        (row, col)
    }

    pub(crate) fn resize(&mut self, cols: usize, rows: usize) -> bool {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if (cols, rows) == self.surface.dimensions() {
            return false;
        }
        self.surface.resize(cols, rows);
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        if let Some(alt) = &mut self.alt_surface {
            alt.resize(cols, rows);
            self.alt_scroll_top = 0;
            self.alt_scroll_bottom = rows - 1;
        }
        true
    }

    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) {
        let mut parser = std::mem::replace(&mut self.parser, Parser::new());
        parser.parse(bytes, |action| self.handle_action(action));
        self.parser = parser;
    }

    pub(crate) fn screen_lines(&self) -> Vec<std::borrow::Cow<'_, Line>> {
        self.surface.screen_lines()
    }

    pub(crate) fn resolve_cell_colors(&self, attrs: &CellAttributes) -> (egui::Color32, egui::Color32) {
        (
            self.resolve_color(attrs.foreground(), true),
            self.resolve_color(attrs.background(), false),
        )
    }

    pub(crate) fn cell_underline(&self, attrs: &CellAttributes) -> bool {
        attrs.underline() != Underline::None
    }

    fn resolve_color(&self, attr: ColorAttribute, is_fg: bool) -> egui::Color32 {
        match attr {
            ColorAttribute::Default => {
                if is_fg {
                    self.default_fg
                } else {
                    self.default_bg
                }
            }
            ColorAttribute::PaletteIndex(idx) => {
                if let Some(color) = self.palette[idx as usize] {
                    color
                } else {
                    xterm_256_color(idx)
                }
            }
            ColorAttribute::TrueColorWithPaletteFallback(color, _) => self.srgba_to_color(color),
            ColorAttribute::TrueColorWithDefaultFallback(color) => self.srgba_to_color(color),
        }
    }

    fn srgba_to_color(&self, color: SrgbaTuple) -> egui::Color32 {
        let SrgbaTuple(r, g, b, _) = color;
        egui::Color32::from_rgb(
            (r.clamp(0.0, 1.0) * 255.0) as u8,
            (g.clamp(0.0, 1.0) * 255.0) as u8,
            (b.clamp(0.0, 1.0) * 255.0) as u8,
        )
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::Print(ch) => self.print_char(ch),
            Action::PrintString(text) => self.print_string(&text),
            Action::Control(code) => self.handle_control_code(code),
            Action::CSI(csi) => {
                if let Some((final_byte, params, private)) = Self::parse_csi_string(&csi.to_string()) {
                    self.execute_csi(final_byte, &params, private);
                }
            }
            Action::Esc(esc) => self.handle_esc_action(esc),
            Action::OperatingSystemCommand(cmd) => self.handle_osc_action(*cmd),
            Action::DeviceControl(_)
            | Action::Sixel(_)
            | Action::KittyImage(_)
            | Action::XtGetTcap(_) => {}
        }
    }

    fn print_char(&mut self, ch: char) {
        let mapped = self.map_charset_char(ch);
        self.surface.add_change(Change::Text(mapped.to_string()));
    }

    fn print_string(&mut self, text: &str) {
        let mut mapped = String::with_capacity(text.len());
        for ch in text.chars() {
            mapped.push(self.map_charset_char(ch));
        }
        self.surface.add_change(Change::Text(mapped));
    }

    fn handle_control_code(&mut self, code: ControlCode) {
        let byte = code as u8;
        match byte {
            0x08 => self.backspace(),
            0x09 => self.tab(),
            0x0a | 0x0b | 0x0c => self.newline(),
            0x0d => self.carriage_return(),
            0x0e => self.use_g1 = true,
            0x0f => self.use_g1 = false,
            _ => {}
        }
    }

    fn handle_esc_action(&mut self, esc: Esc) {
        match esc {
            Esc::Unspecified { intermediate, control } => {
                if let Some(value) = intermediate {
                    self.esc_dispatch(&[value], control);
                } else {
                    self.esc_dispatch(&[], control);
                }
            }
            Esc::Code(code) => match code {
                EscCode::Index => self.newline(),
                EscCode::ReverseIndex => self.reverse_index(),
                EscCode::NextLine => {
                    self.newline();
                    self.carriage_return();
                }
                EscCode::FullReset => self.reset(),
                EscCode::DecSaveCursorPosition => {
                    let (col, row) = self.surface.cursor_position();
                    self.saved_cursor = (row, col);
                }
                EscCode::DecRestoreCursorPosition => self.restore_saved_cursor(),
                EscCode::DecLineDrawingG0 => self.select_charset(b'(', b'0'),
                EscCode::AsciiCharacterSetG0 | EscCode::UkCharacterSetG0 => {
                    self.select_charset(b'(', b'B')
                }
                EscCode::DecLineDrawingG1 => self.select_charset(b')', b'0'),
                EscCode::AsciiCharacterSetG1 | EscCode::UkCharacterSetG1 => {
                    self.select_charset(b')', b'B')
                }
                _ => {}
            },
        }
    }

    fn handle_osc_action(&mut self, cmd: OperatingSystemCommand) {
        let params = match cmd {
            OperatingSystemCommand::Unspecified(params) => params,
            other => {
                if let Some(params) = Self::parse_osc_string(&other.to_string()) {
                    params
                } else {
                    return;
                }
            }
        };

        self.osc_dispatch(&params);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], byte: u8) {
        match (intermediates, byte) {
            ([], b'D') => self.newline(),
            ([], b'M') => self.reverse_index(),
            ([], b'E') => {
                self.newline();
                self.carriage_return();
            }
            ([], b'c') => self.reset(),
            ([], b'7') => {
                let (col, row) = self.surface.cursor_position();
                self.saved_cursor = (row, col);
            }
            ([], b'8') => self.restore_saved_cursor(),
            ([i], designator) if *i == b'(' || *i == b')' => {
                self.select_charset(*i, designator);
            }
            _ => {}
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
        self.surface.add_change(Change::Text("\n".to_string()));
    }

    fn reverse_index(&mut self) {
        let (_, row) = self.surface.cursor_position();
        if row <= self.scroll_top {
            self.scroll_down(1);
        } else {
            self.surface.add_change(Change::CursorPosition {
                x: Position::Relative(0),
                y: Position::Relative(-1),
            });
        }
    }

    fn scroll_up(&mut self, lines: usize) {
        let top = self.scroll_top.min(self.rows().saturating_sub(1));
        let bottom = self.scroll_bottom.min(self.rows().saturating_sub(1));
        if top >= bottom {
            return;
        }
        let region_size = bottom - top + 1;
        let count = lines.min(region_size);
        self.surface.add_change(Change::ScrollRegionUp {
            first_row: top,
            region_size,
            scroll_count: count,
        });
    }

    fn scroll_down(&mut self, lines: usize) {
        let top = self.scroll_top.min(self.rows().saturating_sub(1));
        let bottom = self.scroll_bottom.min(self.rows().saturating_sub(1));
        if top >= bottom {
            return;
        }
        let region_size = bottom - top + 1;
        let count = lines.min(region_size);
        self.surface.add_change(Change::ScrollRegionDown {
            first_row: top,
            region_size,
            scroll_count: count,
        });
    }

    fn carriage_return(&mut self) {
        self.surface.add_change(Change::CursorPosition {
            x: Position::Absolute(0),
            y: Position::Relative(0),
        });
    }

    fn backspace(&mut self) {
        self.surface.add_change(Change::CursorPosition {
            x: Position::Relative(-1),
            y: Position::Relative(0),
        });
    }

    fn tab(&mut self) {
        let (col, row) = self.surface.cursor_position();
        let width = self.cols().max(1);
        let next = ((col / TAB_SIZE) + 1) * TAB_SIZE;
        let target = next.min(width.saturating_sub(1));
        self.surface.add_change(Change::CursorPosition {
            x: Position::Absolute(target),
            y: Position::Absolute(row),
        });
    }

    fn insert_lines(&mut self, n: usize) {
        let (_, row) = self.surface.cursor_position();
        if row < self.scroll_top || row > self.scroll_bottom {
            return;
        }
        let region_size = self.scroll_bottom - row + 1;
        let count = n.min(region_size);
        self.surface.add_change(Change::ScrollRegionDown {
            first_row: row,
            region_size,
            scroll_count: count,
        });
    }

    fn delete_lines(&mut self, n: usize) {
        let (_, row) = self.surface.cursor_position();
        if row < self.scroll_top || row > self.scroll_bottom {
            return;
        }
        let region_size = self.scroll_bottom - row + 1;
        let count = n.min(region_size);
        self.surface.add_change(Change::ScrollRegionUp {
            first_row: row,
            region_size,
            scroll_count: count,
        });
    }

    fn set_scroll_region(&mut self, params: &Params) {
        let mut top = Self::param(params, 0, 1) as usize;
        let mut bottom = Self::param(params, 1, self.rows() as u16) as usize;
        if top == 0 {
            top = 1;
        }
        if bottom == 0 {
            bottom = self.rows();
        }
        let top = top.saturating_sub(1).min(self.rows().saturating_sub(1));
        let bottom = bottom.saturating_sub(1).min(self.rows().saturating_sub(1));
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        } else {
            self.scroll_top = 0;
            self.scroll_bottom = self.rows().saturating_sub(1);
        }
        self.surface.add_change(Change::CursorPosition {
            x: Position::Absolute(0),
            y: Position::Absolute(self.scroll_top),
        });
    }

    fn reset(&mut self) {
        if self.in_alt {
            self.exit_alternate(false);
        }
        self.saved_cursor = (0, 0);
        self.scroll_top = 0;
        self.scroll_bottom = self.rows().saturating_sub(1);
        self.g0 = Charset::Ascii;
        self.g1 = Charset::Ascii;
        self.use_g1 = false;
        self.cur_attrs = CellAttributes::default();
        self.cur_fg_base = None;
        self.bold = false;
        self.surface
            .add_change(Change::AllAttributes(self.cur_attrs.clone()));
        self.palette = [None; 256];
        self.default_fg = DEFAULT_FG;
        self.default_bg = DEFAULT_BG;
        self.cursor_color = None;
        self.surface.add_change(Change::ClearScreen(ColorAttribute::Default));
    }

    fn restore_saved_cursor(&mut self) {
        let (row, col) = self.saved_cursor;
        self.surface.add_change(Change::CursorPosition {
            x: Position::Absolute(col),
            y: Position::Absolute(row),
        });
    }

    fn execute_csi(&mut self, final_byte: u8, params: &Params, private: bool) {
        match final_byte {
            b'A' => {
                let n = Self::csi_count(params, 0);
                self.surface.add_change(Change::CursorPosition {
                    x: Position::Relative(0),
                    y: Position::Relative(-(n as isize)),
                });
            }
            b'B' => {
                let n = Self::csi_count(params, 0);
                self.surface.add_change(Change::CursorPosition {
                    x: Position::Relative(0),
                    y: Position::Relative(n as isize),
                });
            }
            b'C' => {
                let n = Self::csi_count(params, 0);
                self.surface.add_change(Change::CursorPosition {
                    x: Position::Relative(n as isize),
                    y: Position::Relative(0),
                });
            }
            b'D' => {
                let n = Self::csi_count(params, 0);
                self.surface.add_change(Change::CursorPosition {
                    x: Position::Relative(-(n as isize)),
                    y: Position::Relative(0),
                });
            }
            b'E' => {
                let n = Self::csi_count(params, 0);
                self.surface.add_change(Change::CursorPosition {
                    x: Position::Absolute(0),
                    y: Position::Relative(n as isize),
                });
            }
            b'F' => {
                let n = Self::csi_count(params, 0);
                self.surface.add_change(Change::CursorPosition {
                    x: Position::Absolute(0),
                    y: Position::Relative(-(n as isize)),
                });
            }
            b'G' => {
                let col = Self::csi_position(params, 0, self.cols().saturating_sub(1));
                self.surface.add_change(Change::CursorPosition {
                    x: Position::Absolute(col),
                    y: Position::Relative(0),
                });
            }
            b'd' => {
                let row = Self::csi_position(params, 0, self.rows().saturating_sub(1));
                self.surface.add_change(Change::CursorPosition {
                    x: Position::Relative(0),
                    y: Position::Absolute(row),
                });
            }
            b'H' | b'f' => {
                let row = Self::csi_position(params, 0, self.rows().saturating_sub(1));
                let col = Self::csi_position(params, 1, self.cols().saturating_sub(1));
                self.surface.add_change(Change::CursorPosition {
                    x: Position::Absolute(col),
                    y: Position::Absolute(row),
                });
            }
            b'J' => match Self::param(params, 0, 0) {
                2 | 3 => {
                    self.surface
                        .add_change(Change::ClearScreen(ColorAttribute::Default));
                }
                0 => {
                    self.surface
                        .add_change(Change::ClearToEndOfScreen(ColorAttribute::Default));
                }
                1 => {
                    // Minimal: clear full screen for "erase to start".
                    self.surface
                        .add_change(Change::ClearScreen(ColorAttribute::Default));
                }
                _ => {}
            },
            b'K' => {
                let mode = Self::param(params, 0, 0);
                if mode == 0 {
                    self.surface.add_change(Change::ClearToEndOfLine(ColorAttribute::Default));
                } else {
                    let (col, row) = self.surface.cursor_position();
                    self.surface.add_change(Change::CursorPosition {
                        x: Position::Absolute(0),
                        y: Position::Absolute(row),
                    });
                    self.surface.add_change(Change::ClearToEndOfLine(ColorAttribute::Default));
                    self.surface.add_change(Change::CursorPosition {
                        x: Position::Absolute(col),
                        y: Position::Absolute(row),
                    });
                }
            }
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
            b's' => {
                let (col, row) = self.surface.cursor_position();
                self.saved_cursor = (row, col);
            }
            b'u' => self.restore_saved_cursor(),
            b'r' => self.set_scroll_region(params),
            b'm' => {
                self.apply_sgr_colors(params);
            }
            b'h' | b'l' => {
                if private {
                    let set = final_byte == b'h';
                    for param in params.iter() {
                        if let Some(&p) = param.first() {
                            match p {
                                25 => {
                                    let visibility = if set {
                                        CursorVisibility::Visible
                                    } else {
                                        CursorVisibility::Hidden
                                    };
                                    self.surface.add_change(Change::CursorVisibility(visibility));
                                }
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

    fn apply_sgr_colors(&mut self, params: &Params) {
        if params.is_empty() {
            self.cur_attrs = CellAttributes::default();
            self.cur_fg_base = None;
            self.bold = false;
            self.surface
                .add_change(Change::AllAttributes(self.cur_attrs.clone()));
            return;
        }

        let mut i = 0;
        let params_vec: Vec<u16> = params.iter().filter_map(|p| p.first().copied()).collect();
        while i < params_vec.len() {
            match params_vec[i] {
                0 => {
                    self.cur_attrs = CellAttributes::default();
                    self.cur_fg_base = None;
                    self.bold = false;
                }
                1 => {
                    self.bold = true;
                    self.apply_effective_bold_color();
                }
                22 => {
                    self.bold = false;
                    self.apply_effective_bold_color();
                }
                // Ignore italic/underline/reverse and other style changes.
                3 | 4 | 7 | 23 | 24 | 27 => {}
                30..=37 => {
                    let idx = (params_vec[i] - 30) as u8;
                    self.cur_fg_base = Some(idx);
                    self.apply_effective_bold_color();
                }
                40..=47 => {
                    let idx = (params_vec[i] - 40) as u8;
                    self.cur_attrs.set_background(ColorAttribute::PaletteIndex(idx));
                }
                90..=97 => {
                    let idx = (params_vec[i] - 90 + 8) as u8;
                    self.cur_fg_base = Some(idx);
                    self.apply_effective_bold_color();
                }
                100..=107 => {
                    let idx = (params_vec[i] - 100 + 8) as u8;
                    self.cur_attrs.set_background(ColorAttribute::PaletteIndex(idx));
                }
                39 => {
                    self.cur_attrs.set_foreground(ColorAttribute::Default);
                    self.cur_fg_base = None;
                }
                49 => {
                    self.cur_attrs.set_background(ColorAttribute::Default);
                }
                38 | 48 => {
                    let is_fg = params_vec[i] == 38;
                    if i + 1 < params_vec.len() {
                        match params_vec[i + 1] {
                            5 if i + 2 < params_vec.len() => {
                                let idx = params_vec[i + 2] as u8;
                                if is_fg {
                                    self.cur_fg_base = Some(idx);
                                    self.apply_effective_bold_color();
                                } else {
                                    self.cur_attrs
                                        .set_background(ColorAttribute::PaletteIndex(idx));
                                }
                                i += 2;
                            }
                            2 if i + 4 < params_vec.len() => {
                                let r = params_vec[i + 2] as f32 / 255.0;
                                let g = params_vec[i + 3] as f32 / 255.0;
                                let b = params_vec[i + 4] as f32 / 255.0;
                                let color = SrgbaTuple(r, g, b, 1.0);
                                if is_fg {
                                    self.cur_fg_base = None;
                                    self.cur_attrs.set_foreground(
                                        ColorAttribute::TrueColorWithDefaultFallback(color),
                                    );
                                } else {
                                    self.cur_attrs.set_background(
                                        ColorAttribute::TrueColorWithDefaultFallback(color),
                                    );
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

        self.surface
            .add_change(Change::AllAttributes(self.cur_attrs.clone()));
    }

    fn ensure_alt_buffer(&mut self) {
        if self.alt_surface.is_none() {
            let (cols, rows) = self.surface.dimensions();
            self.alt_surface = Some(Surface::new(cols, rows));
            self.alt_saved_cursor = (0, 0);
            self.alt_scroll_top = 0;
            self.alt_scroll_bottom = rows.saturating_sub(1);
        }
    }

    fn swap_screens(&mut self) {
        if let Some(alt) = &mut self.alt_surface {
            std::mem::swap(&mut self.surface, alt);
            std::mem::swap(&mut self.saved_cursor, &mut self.alt_saved_cursor);
            std::mem::swap(&mut self.scroll_top, &mut self.alt_scroll_top);
            std::mem::swap(&mut self.scroll_bottom, &mut self.alt_scroll_bottom);
            self.in_alt = !self.in_alt;
        }
    }

    fn enter_alternate(&mut self, save_cursor: bool, clear: bool) {
        if self.in_alt {
            return;
        }
        if save_cursor {
            let (col, row) = self.surface.cursor_position();
            self.saved_cursor_1049 = Some((row, col));
        }
        self.ensure_alt_buffer();
        self.swap_screens();
        if clear {
            self.surface.add_change(Change::ClearScreen(ColorAttribute::Default));
        }
    }

    fn exit_alternate(&mut self, restore_cursor: bool) {
        if !self.in_alt {
            return;
        }
        self.swap_screens();
        if restore_cursor {
            if let Some((row, col)) = self.saved_cursor_1049.take() {
                self.surface.add_change(Change::CursorPosition {
                    x: Position::Absolute(col),
                    y: Position::Absolute(row),
                });
            }
        }
    }

    fn param(params: &Params, idx: usize, default: u16) -> u16 {
        params
            .get(idx)
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

    fn parse_csi_string(s: &str) -> Option<(u8, Params, bool)> {
        let bytes = s.as_bytes();
        if bytes.is_empty() {
            return None;
        }
        let mut idx = if bytes.starts_with(&[0x1b, b'[']) {
            2
        } else if bytes[0] == 0x9b {
            1
        } else {
            return None;
        };

        let mut private = false;
        if idx < bytes.len() && bytes[idx] == b'?' {
            private = true;
            idx += 1;
        }

        let mut final_idx = None;
        for i in idx..bytes.len() {
            let b = bytes[i];
            if (0x40..=0x7e).contains(&b) {
                final_idx = Some(i);
                break;
            }
        }
        let final_idx = final_idx?;
        let final_byte = bytes[final_idx];

        let mut params: Params = Vec::new();
        let mut current: Vec<u16> = Vec::new();
        let mut num: Option<u16> = None;
        let mut saw_param = false;

        for &b in &bytes[idx..final_idx] {
            match b {
                b'0'..=b'9' => {
                    saw_param = true;
                    let digit = (b - b'0') as u16;
                    num = Some(num.unwrap_or(0).saturating_mul(10).saturating_add(digit));
                }
                b':' => {
                    saw_param = true;
                    if let Some(value) = num.take() {
                        current.push(value);
                    } else {
                        current.push(0);
                    }
                }
                b';' => {
                    saw_param = true;
                    if let Some(value) = num.take() {
                        current.push(value);
                    }
                    params.push(std::mem::take(&mut current));
                }
                _ => {}
            }
        }
        if let Some(value) = num.take() {
            current.push(value);
        }
        if saw_param || !current.is_empty() {
            params.push(current);
        }

        Some((final_byte, params, private))
    }

    fn apply_effective_bold_color(&mut self) {
        let Some(base) = self.cur_fg_base else {
            return;
        };
        let effective = if self.bold && base < 8 { base + 8 } else { base };
        self.cur_attrs
            .set_foreground(ColorAttribute::PaletteIndex(effective));
    }

    fn osc_dispatch(&mut self, params: &[Vec<u8>]) {
        if params.is_empty() {
            return;
        }
        let Ok(cmd) = std::str::from_utf8(&params[0]) else {
            return;
        };
        let Ok(cmd_num) = cmd.parse::<u16>() else {
            return;
        };

        match cmd_num {
            4 => {
                if params.len() < 2 {
                    return;
                }
                let mut i = 1;
                while i + 1 < params.len() {
                    let Ok(idx_str) = std::str::from_utf8(&params[i]) else {
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
                    let Ok(spec) = std::str::from_utf8(&params[i + 1]) else {
                        i += 2;
                        continue;
                    };
                    if spec == "?" {
                        i += 2;
                        continue;
                    }
                    if let Some(color) = Self::parse_osc_color(spec) {
                        self.palette[idx] = Some(color);
                    }
                    i += 2;
                }
            }
            10..=12 => {
                if params.len() < 2 {
                    return;
                }
                let Ok(spec) = std::str::from_utf8(&params[1]) else {
                    return;
                };
                if spec == "?" {
                    return;
                }
                if let Some(color) = Self::parse_osc_color(spec) {
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

    fn parse_osc_color(spec: &str) -> Option<egui::Color32> {
        let spec = spec.trim();
        if spec.eq_ignore_ascii_case("none") {
            return None;
        }
        if let Some(rest) = spec.strip_prefix("rgb:") {
            let comps: Vec<&str> = rest.split('/').collect();
            if comps.len() < 3 {
                return None;
            }
            let r = Self::parse_hex_component(comps[0])?;
            let g = Self::parse_hex_component(comps[1])?;
            let b = Self::parse_hex_component(comps[2])?;
            return Some(egui::Color32::from_rgb(r, g, b));
        }
        if let Some(hex) = spec.strip_prefix('#') {
            if hex.len() % 3 != 0 {
                return None;
            }
            let step = hex.len() / 3;
            if step == 0 || step > 4 {
                return None;
            }
            let r = Self::parse_hex_component(&hex[0..step])?;
            let g = Self::parse_hex_component(&hex[step..step * 2])?;
            let b = Self::parse_hex_component(&hex[step * 2..step * 3])?;
            return Some(egui::Color32::from_rgb(r, g, b));
        }
        None
    }

    fn parse_hex_component(comp: &str) -> Option<u8> {
        if comp.is_empty() || comp.len() > 4 {
            return None;
        }
        let value = u32::from_str_radix(comp, 16).ok()?;
        let max = (1u32 << (comp.len() * 4)) - 1;
        let scaled = (value.saturating_mul(255) + max / 2) / max;
        Some(scaled as u8)
    }

    fn parse_osc_string(s: &str) -> Option<Vec<Vec<u8>>> {
        let mut text = s;
        if let Some(rest) = text.strip_prefix("\u{1b}]") {
            text = rest;
        } else if let Some(rest) = text.strip_prefix("\u{9d}") {
            text = rest;
        } else {
            return None;
        }

        if let Some(rest) = text.strip_suffix("\u{1b}\\") {
            text = rest;
        } else if let Some(rest) = text.strip_suffix('\u{7}') {
            text = rest;
        }

        let params = text
            .split(';')
            .map(|part| part.as_bytes().to_vec())
            .collect::<Vec<_>>();
        Some(params)
    }
}
