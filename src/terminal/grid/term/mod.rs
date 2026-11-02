use crate::terminal::io::PtyEvent;
use crate::terminal::theme::{Color32, ColorScheme, Theme, xterm_256_color};
use libghostty_vt::render::RenderState;
use libghostty_vt::screen::{Cell as VtCell, CellWide, GridRef};
use libghostty_vt::style::{Palette, RgbColor};
use libghostty_vt::terminal::{
    ConformanceLevel, DeviceAttributes, DeviceType, PrimaryDeviceAttributes, ScrollViewport,
    SecondaryDeviceAttributes, SizeReportSize, Terminal, TertiaryDeviceAttributes,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Sender;

mod cells;
mod cursor;

/// Maximum number of scrollback rows kept above the visible screen.
pub(crate) const SCROLLBACK_LEN: usize = 1000;

/// Primary DA matches the previous emulator (`CSI ? 6 c`, VT102). Secondary
/// uses device type 0 and a `0.1.0`-style firmware number, the same shape
/// Alacritty reported.
const DEVICE_ATTRIBUTES: DeviceAttributes = DeviceAttributes {
    primary: PrimaryDeviceAttributes::new(ConformanceLevel::VT102, &[]),
    secondary: SecondaryDeviceAttributes {
        device_type: DeviceType::VT100,
        firmware_version: 100,
        rom_cartridge: 1,
    },
    tertiary: TertiaryDeviceAttributes { unit_id: 0 },
};

/// State the libghostty callbacks need. The callbacks outlive any single
/// `&mut TerminalGrid` borrow, so this sits behind `Rc`.
struct HostState {
    pty_tx: RefCell<Option<Sender<PtyEvent>>>,
    cell_width_px: Cell<u32>,
    cell_height_px: Cell<u32>,
}

use super::{CellInfo, CellPos, CellRun, CursorInfo, GridView};

pub(crate) struct TerminalGrid {
    term: Terminal<'static, 'static>,
    /// Cursor shape lives on the render-state snapshot, which needs `&mut`.
    render: RefCell<RenderState<'static>>,
    host: Rc<HostState>,
    /// Named colors for the active theme. Explicit RGB cells ignore this.
    scheme: ColorScheme,
    has_changes: bool,
    cell_width: f64,
    cell_height: f64,
}

impl TerminalGrid {
    pub(crate) fn new(cols: usize, rows: usize) -> Self {
        let cols = clamp_dim(cols);
        let rows = clamp_dim(rows);
        let host = Rc::new(HostState {
            pty_tx: RefCell::new(None),
            cell_width_px: Cell::new(1),
            cell_height_px: Cell::new(1),
        });
        let mut term = Terminal::new(cols, rows).expect("libghostty terminal");
        let _ = term.set_scrollback_max_lines(Some(SCROLLBACK_LEN));
        wire_callbacks(&mut term, &host);
        let mut grid = Self {
            term,
            render: RefCell::new(RenderState::new().expect("libghostty render state")),
            host,
            scheme: Theme::Default.scheme(),
            has_changes: false,
            cell_width: 1.0,
            cell_height: 1.0,
        };
        grid.install_scheme();
        // Pixel size starts at 1×1 until the view reports a real cell size.
        let _ = grid.term.resize(cols, rows, 1, 1);
        grid
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
        *grid.host.pty_tx.borrow_mut() = Some(tx_input);
        grid.set_cell_size(cell_width, cell_height);
        grid
    }

    /// Update the cell metrics used to answer pixel-size queries
    /// (e.g. after a font zoom); also flags the grid for redraw.
    pub(crate) fn set_cell_size(&mut self, cell_width: f64, cell_height: f64) {
        self.cell_width = cell_width;
        self.cell_height = cell_height;
        let width = px(cell_width);
        let height = px(cell_height);
        self.host.cell_width_px.set(width);
        self.host.cell_height_px.set(height);
        let _ = self.term.resize(
            clamp_dim(self.cols()),
            clamp_dim(self.rows()),
            width,
            height,
        );
        self.has_changes = true;
    }

    pub(crate) fn rows(&self) -> usize {
        usize::from(self.term.rows().unwrap_or(1))
    }

    pub(crate) fn cols(&self) -> usize {
        usize::from(self.term.cols().unwrap_or(1))
    }

    pub(crate) fn default_bg(&self) -> Color32 {
        self.scheme.bg
    }

    /// Highlight drawn behind a selection; themes tint it to match.
    pub(crate) fn selection_bg(&self) -> Color32 {
        self.scheme.selection
    }

    /// Retint the grid to `theme`. Cells that name a color pick it up on the
    /// next paint; cells with an explicit RGB stay as the application set them.
    pub(crate) fn set_theme(&mut self, theme: Theme) {
        self.scheme = theme.scheme();
        self.install_scheme();
        self.has_changes = true;
    }

    /// The title set by the shell (OSC 0/2), if any.
    pub(crate) fn title(&self) -> Option<&str> {
        let bytes = self.term.title().ok()?;
        let title = std::str::from_utf8(bytes).ok()?;
        (!title.is_empty()).then_some(title)
    }

    pub(crate) fn cursor_visible(&self) -> bool {
        self.term.is_cursor_visible().unwrap_or(true)
    }

    pub(crate) fn cursor_pos(&self) -> (usize, usize) {
        let row = usize::from(self.term.cursor_y().unwrap_or(0)) + self.scroll_offset();
        let col = usize::from(self.term.cursor_x().unwrap_or(0));
        (row, col)
    }

    pub(crate) fn resize(&mut self, cols: usize, rows: usize) -> bool {
        let cols = clamp_dim(cols);
        let rows = clamp_dim(rows);
        if usize::from(cols) == self.cols() && usize::from(rows) == self.rows() {
            return false;
        }
        if self
            .term
            .resize(cols, rows, px(self.cell_width), px(self.cell_height))
            .is_err()
        {
            return false;
        }
        self.has_changes = true;
        true
    }

    pub(crate) fn process_pty_bytes(&mut self, bytes: &[u8]) {
        self.term.vt_write(bytes);
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
        self.scroll_metrics().0
    }

    /// How many scrollback rows are available above the screen right now.
    #[cfg(target_os = "macos")]
    pub(crate) fn max_scroll(&self) -> usize {
        self.scroll_metrics().1
    }

    /// Scroll the view so it starts `offset` rows above the bottom. Returns
    /// whether the offset actually changed.
    #[cfg(target_os = "macos")]
    pub(crate) fn scroll_to(&mut self, offset: usize) -> bool {
        let (_, max) = self.scroll_metrics();
        let target = offset.min(max);
        if target == self.scroll_offset() {
            return false;
        }
        // libghostty's row is counted from the top of the scrollback.
        let top = max - target;
        self.term.scroll_viewport(ScrollViewport::Row(top));
        true
    }

    /// Scroll by `delta` rows; positive moves toward older history, negative
    /// back toward the live screen. Returns whether the offset changed.
    #[cfg(target_os = "macos")]
    pub(crate) fn scroll_by(&mut self, delta: isize) -> bool {
        let (offset, max) = self.scroll_metrics();
        let target = (offset as isize + delta).clamp(0, max as isize) as usize;
        self.scroll_to(target)
    }

    fn install_scheme(&mut self) {
        let fg = rgb_of(self.scheme.fg);
        let bg = rgb_of(self.scheme.bg);
        let palette = themed_palette(&self.scheme);
        // Queries (OSC 10/11/4) read these. Painted cells resolve palette
        // indexes through `scheme` instead, so a later theme change recolors
        // text that is already on screen.
        let _ = self.term.set_default_fg_color(Some(fg));
        let _ = self.term.set_default_bg_color(Some(bg));
        let _ = self.term.set_default_color_palette(Some(palette));
    }

    /// `(rows above the bottom, maximum such rows)`.
    fn scroll_metrics(&self) -> (usize, usize) {
        let Ok(bar) = self.term.scrollbar() else {
            return (0, 0);
        };
        let total = usize::try_from(bar.total).unwrap_or(usize::MAX);
        let len = usize::try_from(bar.len).unwrap_or(0);
        let offset = usize::try_from(bar.offset).unwrap_or(0);
        let max = total.saturating_sub(len);
        let from_bottom = max.saturating_sub(offset.min(max));
        (from_bottom, max)
    }
}

fn wire_callbacks(term: &mut Terminal<'static, 'static>, host: &Rc<HostState>) {
    let pty = Rc::clone(host);
    let size = Rc::clone(host);
    term.on_pty_write(move |_term, data| {
        if let Some(tx) = pty.pty_tx.borrow().as_ref() {
            let _ = tx.send(PtyEvent::Input(data.to_vec()));
        }
    })
    .expect("pty write callback")
    .on_size(move |term| {
        Some(SizeReportSize {
            rows: term.rows().unwrap_or(1),
            columns: term.cols().unwrap_or(1),
            cell_width: size.cell_width_px.get(),
            cell_height: size.cell_height_px.get(),
        })
    })
    .expect("size callback")
    .on_device_attributes(|_| Some(DEVICE_ATTRIBUTES))
    .expect("device attributes callback");
}

fn clamp_dim(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX).max(1)
}

fn px(value: f64) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        return 1;
    }
    let rounded = value.round();
    if rounded >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        rounded as u32
    }
}

fn rgb_of(color: Color32) -> RgbColor {
    RgbColor {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

fn themed_palette(scheme: &ColorScheme) -> Palette {
    let mut colors = [RgbColor { r: 0, g: 0, b: 0 }; 256];
    for (index, slot) in colors.iter_mut().enumerate() {
        let index = u8::try_from(index).expect("palette index fits in u8");
        let color = scheme.ansi(index).unwrap_or_else(|| xterm_256_color(index));
        *slot = rgb_of(color);
    }
    Palette(colors)
}

fn width_flags(cell: VtCell) -> (bool, bool) {
    match cell.wide() {
        Ok(CellWide::Wide) => (true, false),
        Ok(CellWide::SpacerTail | CellWide::SpacerHead) => (false, true),
        _ => (false, false),
    }
}

fn cell_text(grid_ref: &GridRef<'_>, cell: VtCell, wide_continuation: bool) -> String {
    if wide_continuation || !cell.has_text().unwrap_or(false) {
        return String::new();
    }
    read_graphemes(grid_ref).unwrap_or_else(|| codepoint_text(cell))
}

fn read_graphemes(grid_ref: &GridRef<'_>) -> Option<String> {
    let mut buf = ['\0'; 32];
    match grid_ref.graphemes(&mut buf) {
        Ok(0) => Some(String::new()),
        Ok(len) => Some(buf[..len].iter().collect()),
        Err(libghostty_vt::Error::OutOfSpace { required }) => {
            let mut big = vec!['\0'; required];
            match grid_ref.graphemes(&mut big) {
                Ok(len) => Some(big[..len].iter().collect()),
                _ => None,
            }
        }
        Err(_) => None,
    }
}

fn codepoint_text(cell: VtCell) -> String {
    cell.codepoint()
        .ok()
        .and_then(char::from_u32)
        .filter(|ch| *ch != '\0')
        .map(|ch| ch.to_string())
        .unwrap_or_default()
}

impl GridView for TerminalGrid {
    fn rows(&self) -> usize {
        TerminalGrid::rows(self)
    }
    fn cols(&self) -> usize {
        TerminalGrid::cols(self)
    }
    fn default_bg(&self) -> Color32 {
        TerminalGrid::default_bg(self)
    }
    fn selection_bg(&self) -> Color32 {
        TerminalGrid::selection_bg(self)
    }
    fn cursor(&self) -> Option<CursorInfo> {
        TerminalGrid::cursor(self)
    }
    fn cursor_pos(&self) -> (usize, usize) {
        TerminalGrid::cursor_pos(self)
    }
    fn row_runs(&self, row: usize) -> Vec<CellRun> {
        TerminalGrid::row_runs(self, row)
    }
    fn max_scroll(&self) -> usize {
        TerminalGrid::max_scroll(self)
    }
    fn scroll_offset(&self) -> usize {
        TerminalGrid::scroll_offset(self)
    }
    fn has_changes(&self) -> bool {
        TerminalGrid::has_changes(self)
    }
    fn snap_cell_pos(&self, pos: CellPos) -> CellPos {
        TerminalGrid::snap_cell_pos(self, pos)
    }
    fn get_cell(&self, row: usize, col: usize) -> Option<CellInfo> {
        TerminalGrid::get_cell(self, row, col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_recolors_named_colors_already_on_screen() {
        let mut grid = TerminalGrid::new(6, 1);
        grid.set_theme(Theme::CatppuccinMocha);
        grid.process_pty_bytes(b"\x1b[31mred\x1b[0m");

        let scheme = Theme::CatppuccinMocha.scheme();
        assert_eq!(grid.default_bg(), scheme.bg);
        assert_eq!(
            grid.get_cell(0, 0).map(|cell| cell.fg),
            Some(scheme.ansi[1])
        );

        grid.set_theme(Theme::CatppuccinLatte);
        let scheme = Theme::CatppuccinLatte.scheme();
        assert_eq!(grid.default_bg(), scheme.bg);
        assert_eq!(
            grid.get_cell(0, 0).map(|cell| cell.fg),
            Some(scheme.ansi[1])
        );
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
