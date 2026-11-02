//! Headless driver for a real [`TerminalSession`]: the same PTY, zsh, and
//! libghostty grid the app uses, with no window.

use crate::terminal::grid::GridView;
use crate::terminal::io::{SessionOptions, TerminalSession};
use crate::terminal::theme::Theme;
use std::thread;
use std::time::{Duration, Instant};

/// Bytes drained from the PTY per pump, matching the view's per-frame budget order of magnitude.
const DRAIN_BUDGET: usize = 256 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(5);

pub(crate) struct Harness {
    session: TerminalSession,
    bytes_seen: usize, // total PTY output fed to the grid
    markers: usize,    // completion markers issued so far, so each one is unique
}

impl Harness {
    /// Open a session at `cols`×`rows`, then swap the login shell for a bare
    /// `zsh -f` so the user's dotfiles cannot change prompts or timing. The
    /// environment the PTY sets (`TERM`, `LANG`, …) survives the `exec`.
    pub(crate) fn spawn(cols: usize, rows: usize) -> Self {
        let session = TerminalSession::spawn(
            1,
            SessionOptions {
                cols,
                rows,
                cell_width: 8.0,
                cell_height: 16.0,
                theme: Theme::Default,
            },
            || {},
            || {},
        )
        .expect("session should spawn a shell");
        session.send_resize(cols, rows);
        let mut harness = Self {
            session,
            bytes_seen: 0,
            markers: 0,
        };
        harness.run("exec /bin/zsh -f");
        harness.run("export PS1='$ ' PROMPT_EOL_MARK=''");
        harness.run_to_completion("true", Duration::from_secs(10));
        harness
    }

    /// Type `line` at the prompt and press return.
    pub(crate) fn run(&mut self, line: &str) {
        self.send(format!("{line}\r").as_bytes());
    }

    pub(crate) fn send(&mut self, bytes: &[u8]) {
        self.session.send_input(bytes.to_vec());
    }

    /// Run `command`, then wait until the shell prints a completion marker.
    /// Returns the marker text and how long the command took.
    ///
    /// The marker is computed by the shell (`$((…))`), so the echoed command
    /// line never matches it; only real output does.
    pub(crate) fn run_to_completion(
        &mut self,
        command: &str,
        timeout: Duration,
    ) -> (String, Duration) {
        let start = Instant::now();
        let marker = self.start(command);
        self.wait_for_text(&marker, timeout);
        (marker, start.elapsed())
    }

    /// Start `command` without waiting; returns the marker it prints when it
    /// exits, for a later [`Harness::wait_for_text`].
    pub(crate) fn start(&mut self, command: &str) -> String {
        let marker = self.next_marker();
        self.run(&format!("{command}; echo E2E_$(({}+0))_DONE", self.markers));
        marker
    }

    /// Resize the grid and the PTY together, as the view does after a window resize.
    pub(crate) fn resize(&mut self, cols: usize, rows: usize) {
        self.session.resize_grid(cols, rows);
        self.session.send_resize(cols, rows);
    }

    /// Run `command` and leave the screen exactly as it drew it: completion is
    /// signalled through the window title, and `read` holds the prompt back
    /// until [`Harness::release`]. Returns how long the command took.
    pub(crate) fn run_and_hold(&mut self, command: &str, timeout: Duration) -> Duration {
        let marker = self.next_marker();
        let start = Instant::now();
        self.run(&format!(
            "{command}; printf '\\e]2;E2E_%s_DONE\\a' $(({}+0)); read -r _",
            self.markers
        ));
        self.wait_until(&format!("title {marker:?}"), timeout, |h| {
            h.session.display_title() == marker
        });
        start.elapsed()
    }

    /// Let a command started with [`Harness::run_and_hold`] finish.
    pub(crate) fn release(&mut self) {
        self.send(b"\r");
    }

    fn next_marker(&mut self) -> String {
        self.markers += 1;
        format!("E2E_{}_DONE", self.markers)
    }

    /// Feed pending PTY output to the grid.
    pub(crate) fn pump(&mut self) {
        let mut budget = DRAIN_BUDGET;
        self.session.drain_output(&mut budget);
        self.session.sync_title();
        self.bytes_seen += DRAIN_BUDGET - budget;
    }

    /// Pump until `pred` holds for the screen, panicking with a screen dump on timeout.
    pub(crate) fn wait_until(
        &mut self,
        what: &str,
        timeout: Duration,
        mut pred: impl FnMut(&Self) -> bool,
    ) {
        let deadline = Instant::now() + timeout;
        loop {
            self.pump();
            if pred(self) {
                return;
            }
            assert!(
                !self.session.exited(),
                "shell exited while waiting for {what}\n{}",
                self.dump()
            );
            assert!(
                Instant::now() < deadline,
                "timed out after {timeout:?} waiting for {what}\n{}",
                self.dump()
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub(crate) fn wait_for_text(&mut self, needle: &str, timeout: Duration) {
        self.wait_until(&format!("{needle:?} on screen"), timeout, |h| {
            h.screen_text().contains(needle)
        });
    }

    pub(crate) fn frame(&self) -> &impl GridView {
        self.session.frame()
    }

    pub(crate) fn bytes_seen(&self) -> usize {
        self.bytes_seen
    }

    /// One visible row as plain text, right-trimmed. Wide characters appear once.
    pub(crate) fn row_text(&self, row: usize) -> String {
        let grid = self.frame();
        let mut line = String::new();
        for col in 0..grid.cols() {
            match grid.get_cell(row, col) {
                Some(cell) if cell.wide_continuation => {}
                Some(cell) if cell.text.is_empty() => line.push(' '),
                Some(cell) => line.push_str(&cell.text),
                None => {}
            }
        }
        line.trim_end().to_owned()
    }

    /// The visible screen, one line per row.
    pub(crate) fn screen_text(&self) -> String {
        (0..self.frame().rows())
            .map(|row| self.row_text(row))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn dump(&self) -> String {
        let grid = self.frame();
        format!(
            "--- screen {}x{}, cursor {:?} ---\n{}\n---",
            grid.cols(),
            grid.rows(),
            grid.cursor_pos(),
            self.screen_text()
        )
    }
}
