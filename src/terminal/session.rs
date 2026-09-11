use crate::terminal::grid::TerminalGrid;
use crate::terminal::pty::{Pty, PtyEvent};
use crate::terminal::selection::Selection;
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

/// Label shown on a tab whose shell has not set a title.
const DEFAULT_TITLE: &str = "zsh";

/// One shell session: its own PTY, grid, channels, and view state.
///
/// A terminal window owns a list of sessions and renders the active one.
/// `TerminalGrid` is `!Send`, so sessions stay on the main thread; only the
/// PTY reader/writer threads live off it.
pub(crate) struct TerminalSession {
    id: u64,
    pub(crate) grid: TerminalGrid,
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
        cols: usize,
        rows: usize,
        cell_width: f64,
        cell_height: f64,
        on_output: impl Fn() + Send + 'static,
        on_exit: impl Fn() + Send + 'static,
    ) -> Result<Self, Box<dyn Error>> {
        let pty = Pty::spawn()?;
        let (tx_output, rx_output) = channel::<Vec<u8>>();
        let (tx_input, rx_input) = channel::<PtyEvent>();
        let grid =
            TerminalGrid::new_with_pty_size(cols, rows, tx_input.clone(), cell_width, cell_height);
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
        let title = self.grid.title().map(str::to_owned);
        if self.last_title == title {
            return false;
        }
        self.last_title = title;
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
        let session = TerminalSession::spawn(1, 80, 24, 8.0, 16.0, || {}, || {})
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
