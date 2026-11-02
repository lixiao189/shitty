//! Vim as a real-world fullscreen TUI workload: the alternate screen, a
//! status line, insert mode with wide characters, `:!` shell escapes, and
//! redraws on resize.
//!
//! Vim ships with macOS, so these tests need no install, network, or
//! credentials. They skip when `vim` is not on `PATH`. Vim starts with no
//! vimrc, plugins, viminfo, or swap file, so the user's setup cannot change
//! what it draws.

use super::harness::Harness;
use crate::terminal::grid::GridView;
use std::process::Command;
use std::time::Duration;

const COLS: usize = 100;
const ROWS: usize = 30;
const STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// No vimrc, plugins, viminfo, or swap file. The status line reports the
/// window size, so it only reads `W{cols}x{rows}` once vim has redrawn at
/// that size (the window is the screen minus the status and command lines).
const VIM: &str = "vim -N -u NONE -i NONE -n --noplugin \
                   -c 'set laststatus=2 statusline=W%{winwidth(0)}x%{winheight(0)}'";

/// Whether `vim` can be launched; prints why a test is skipped when it cannot.
fn vim_available() -> bool {
    let found = Command::new("sh")
        .args(["-c", "command -v vim"])
        .output()
        .is_ok_and(|out| out.status.success());
    if !found {
        eprintln!("skipping: `vim` is not on PATH");
    }
    found
}

/// The status line vim draws for a `cols`x`rows` screen.
fn status_for(cols: usize, rows: usize) -> String {
    format!("W{cols}x{}", rows - 2)
}

/// A running vim inside a session, plus the marker zsh prints once it exits.
struct Vim {
    term: Harness,
    exit_marker: String,
}

impl Vim {
    fn launch(before: &str) -> Self {
        let mut term = Harness::spawn(COLS, ROWS);
        term.run(&format!("echo {before}"));
        term.wait_for_text(before, STEP_TIMEOUT);
        let exit_marker = term.start(VIM);
        let status = status_for(COLS, ROWS);
        term.wait_until("vim's empty buffer", STEP_TIMEOUT, |t| {
            t.screen_text().contains(&status) && t.row_text(1).starts_with('~')
        });
        Self { term, exit_marker }
    }

    /// Leave insert or pending modes, then quit without saving.
    fn quit(mut self) -> Harness {
        self.term.send(b"\x1b:qa!\r");
        let marker = self.exit_marker.clone();
        self.term.wait_for_text(&marker, STEP_TIMEOUT);
        self.term
    }
}

#[test]
fn vim_quits_and_restores_the_shell_screen() {
    if !vim_available() {
        return;
    }
    let term = Vim::launch("before-vim").quit();
    // Vim drew on the alternate screen; leaving it brings the shell back.
    let screen = term.screen_text();
    assert!(screen.contains("before-vim"), "{screen}");
    assert!(
        !(0..ROWS).any(|row| term.row_text(row) == "~"),
        "vim's filler lines are still on screen:\n{screen}"
    );
}

#[test]
fn vim_shows_inserted_wide_characters() {
    if !vim_available() {
        return;
    }
    let mut vim = Vim::launch("editor");
    let text = "hello from shitty — 你好";
    vim.term.send(b"i");
    vim.term.send(text.as_bytes());
    vim.term.send(b"\x1b");
    vim.term
        .wait_until("inserted text on the first line", STEP_TIMEOUT, |t| {
            t.row_text(0).trim_end() == text
        });
    vim.quit();
}

#[test]
fn vim_runs_a_shell_command_and_shows_its_output() {
    if !vim_available() {
        return;
    }
    let mut vim = Vim::launch("bang");
    // The typed line holds `$((6*7))`, so only real output reads "e2e 42".
    vim.term.send(b":!echo e2e $((6*7))\r");
    vim.term
        .wait_until("shell output under vim", STEP_TIMEOUT, |t| {
            (0..ROWS).any(|row| t.row_text(row).trim() == "e2e 42")
        });
    // Dismiss "Press ENTER" and confirm vim redrew its buffer.
    vim.term.send(b"\r");
    let status = status_for(COLS, ROWS);
    vim.term
        .wait_until("vim back on its buffer", STEP_TIMEOUT, |t| {
            t.screen_text().contains(&status)
                && !(0..ROWS).any(|row| t.row_text(row).trim() == "e2e 42")
        });
    vim.quit();
}

#[test]
fn vim_redraws_for_a_resized_window() {
    if !vim_available() {
        return;
    }
    let mut vim = Vim::launch("resize");
    for (cols, rows) in [(70, 20), (120, 40), (COLS, ROWS)] {
        vim.term.resize(cols, rows);
        let status = status_for(cols, rows);
        vim.term.wait_until(
            &format!("vim redrawn at {cols}x{rows}"),
            STEP_TIMEOUT,
            |t| t.row_text(rows - 2).trim_end() == status,
        );
        let grid = vim.term.frame();
        assert_eq!((grid.cols(), grid.rows()), (cols, rows));
    }
    vim.quit();
}
