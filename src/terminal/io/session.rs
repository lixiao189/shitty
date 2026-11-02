use super::{Pty, PtyEvent};
use crate::terminal::grid::{GridView, TerminalGrid};
use crate::terminal::selection::Selection;
use crate::terminal::theme::Theme;
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

/// Label shown on a tab whose shell has not set a title.
const DEFAULT_TITLE: &str = "zsh";

/// Size and theme a new session is opened with.
pub(crate) struct SessionOptions {
    pub cols: usize,
    pub rows: usize,
    pub cell_width: f64,
    pub cell_height: f64,
    pub theme: Theme,
}

/// One shell session: its own PTY, grid, channels, and view state.
///
/// A terminal window owns a list of sessions and renders the active one.
/// `TerminalGrid` is `!Send`, so sessions stay on the main thread; only the
/// PTY reader/writer threads live off it.
pub(crate) struct TerminalSession {
    id: u64,
    grid: TerminalGrid,
    /// Selection in cell coordinates; empty when the user has not selected.
    pub(crate) selection: Option<Selection>,
    /// Fractional scroll-wheel delta waiting to be applied to the grid.
    pub(crate) scroll_accum: f64,
    /// Whether the scrollbar thumb is being dragged.
    pub(crate) scrollbar_dragging: bool,
    /// Set by the PTY reader thread when the shell exits.
    exited: Arc<AtomicBool>,
    rx_output: Receiver<Vec<u8>>,
    tx_input: Sender<PtyEvent>,
    /// RAII handle that hangs the shell up when the session is dropped.
    _pty: Pty,
    /// Last title seen by the view, used to detect tab-label changes.
    last_title: Option<String>,
}

impl TerminalSession {
    pub(crate) fn spawn(
        id: u64,
        options: SessionOptions,
        on_output: impl Fn() + Send + 'static,
        on_exit: impl Fn() + Send + 'static,
    ) -> Result<Self, Box<dyn Error>> {
        let pty = Pty::spawn()?;
        let (tx_output, rx_output) = channel::<Vec<u8>>();
        let (tx_input, rx_input) = channel::<PtyEvent>();
        let mut grid = TerminalGrid::new_with_pty_size(
            options.cols,
            options.rows,
            tx_input.clone(),
            options.cell_width,
            options.cell_height,
        );
        grid.set_theme(options.theme);
        let exited = Arc::new(AtomicBool::new(false));
        let exited_flag = exited.clone();
        pty.spawn_threads(tx_output, rx_input, on_output, move || {
            exited_flag.store(true, Ordering::Release);
            on_exit();
        });

        Ok(Self {
            id,
            grid,
            selection: None,
            scroll_accum: 0.0,
            scrollbar_dragging: false,
            exited,
            rx_output,
            tx_input,
            _pty: pty,
            last_title: None,
        })
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Read-only view of the grid. Painting and hit-testing use this instead of
    /// the libghostty terminal.
    pub(crate) fn frame(&self) -> &impl GridView {
        &self.grid
    }

    pub(crate) fn scroll_by(&mut self, delta: isize) -> bool {
        self.grid.scroll_by(delta)
    }

    pub(crate) fn scroll_to(&mut self, offset: usize) -> bool {
        self.grid.scroll_to(offset)
    }

    pub(crate) fn resize_grid(&mut self, cols: usize, rows: usize) -> bool {
        self.grid.resize(cols, rows)
    }

    pub(crate) fn set_theme(&mut self, theme: Theme) {
        self.grid.set_theme(theme);
    }

    pub(crate) fn set_cell_size(&mut self, cell_width: f64, cell_height: f64) {
        self.grid.set_cell_size(cell_width, cell_height);
    }

    pub(crate) fn mark_rendered(&mut self) {
        self.grid.mark_rendered();
    }

    /// Whether the shell process has exited.
    pub(crate) fn exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    /// Stdin for the shell; any keystroke snaps the view back to the live screen.
    pub(crate) fn send_input(&mut self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            self.grid.scroll_to(0);
            let _ = self.tx_input.send(PtyEvent::Input(bytes));
        }
    }

    pub(crate) fn send_resize(&self, cols: usize, rows: usize) {
        let _ = self.tx_input.send(PtyEvent::Resize {
            cols: cols as u16,
            rows: rows as u16,
        });
    }

    /// Drain pending PTY output, drawing from `budget` and returning true when
    /// the budget ran out with more output possibly still queued.
    pub(crate) fn drain_output(&mut self, budget: &mut usize) -> bool {
        while *budget > 0 {
            match self.rx_output.try_recv() {
                Ok(bytes) => {
                    *budget = budget.saturating_sub(bytes.len());
                    self.grid.process_pty_bytes(&bytes);
                }
                Err(_) => return false,
            }
        }
        true
    }

    /// The label to draw on this session's tab.
    pub(crate) fn display_title(&self) -> &str {
        self.last_title.as_deref().unwrap_or(DEFAULT_TITLE)
    }

    /// Pick up a title the shell has changed; returns whether it changed.
    pub(crate) fn sync_title(&mut self) -> bool {
        let title = self.grid.title();
        if self.last_title.as_deref() == title {
            return false;
        }
        self.last_title = title.map(str::to_owned);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn dropping_a_session_hangs_up_its_shell() {
        let session = TerminalSession::spawn(
            1,
            SessionOptions {
                cols: 80,
                rows: 24,
                cell_width: 8.0,
                cell_height: 16.0,
                theme: Theme::Default,
            },
            || {},
            || {},
        )
        .expect("session should spawn a shell");
        assert!(!session.exited());
        assert_eq!(session.display_title(), "zsh");

        let exited = session.exited.clone();
        drop(session);

        let deadline = Instant::now() + Duration::from_secs(5);
        while !exited.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "shell should exit after its session is dropped"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}
