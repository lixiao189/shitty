//! The `pi` coding agent (<https://github.com/badlogic/pi-mono>) as a
//! real-world TUI workload: a fullscreen differential renderer with a
//! bordered editor, a status bar, and its own `!` bash runner.
//!
//! Tests skip when `pi` is not on `PATH`. The offline tests load no
//! extensions, skills, context files, MCP servers, or sessions, so they do
//! not depend on the user's pi setup beyond the binary. The model test is
//! `#[ignore]`d because it needs credentials and the network:
//!
//! ```sh
//! cargo test e2e::pi -- --ignored          # PI_E2E_MODEL=provider/id to pick a model
//! ```

use super::harness::Harness;
use crate::terminal::grid::GridView;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const COLS: usize = 100;
const ROWS: usize = 30;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const STEP_TIMEOUT: Duration = Duration::from_secs(15);
const MODEL_TIMEOUT: Duration = Duration::from_secs(180);
/// Flags that keep pi from loading anything the user configured besides the binary.
const ISOLATED: &str = "--no-session -ne -ns -nc -np --no-mcp --no-themes";
/// Shown in pi's startup header.
const STARTUP_HINT: &str = "escape interrupt";

/// Whether `pi` can be launched; prints why a test is skipped when it cannot.
fn pi_available() -> bool {
    let found = Command::new("sh")
        .args(["-c", "command -v pi"])
        .output()
        .is_ok_and(|out| out.status.success());
    if !found {
        eprintln!("skipping: `pi` is not on PATH");
    }
    found
}

/// An empty working directory, so pi finds no project-local files.
fn scratch_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shitty-e2e-pi-{}-{test}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A running pi TUI inside a session, plus the marker zsh prints once it exits.
struct Pi {
    term: Harness,
    exit_marker: String,
    dir: PathBuf,
}

impl Pi {
    fn launch(test: &str, args: &str) -> Self {
        let dir = scratch_dir(test);
        let mut term = Harness::spawn(COLS, ROWS);
        term.run(&format!("cd '{}'", dir.display()));
        let exit_marker = term.start(&format!("pi {ISOLATED} {args}"));
        term.wait_until("pi's startup screen", STARTUP_TIMEOUT, |t| {
            t.screen_text().contains(STARTUP_HINT) && editor_borders(t).len() >= 2
        });
        Self {
            term,
            exit_marker,
            dir,
        }
    }

    /// Ctrl+D on an empty editor quits; zsh then prints the exit marker.
    fn quit(mut self) {
        self.term.send(b"\x03"); // clear whatever is in the editor first
        self.term.send(b"\x04");
        let marker = self.exit_marker.clone();
        self.term.wait_for_text(&marker, STEP_TIMEOUT);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Rows that are a full-width horizontal rule: pi draws one above and below
/// its editor and between message blocks.
fn editor_borders(term: &Harness) -> Vec<usize> {
    let rule = "─".repeat(term.frame().cols());
    (0..term.frame().rows())
        .filter(|&row| term.row_text(row) == rule)
        .collect()
}

#[test]
fn pi_starts_and_quits_cleanly() {
    if !pi_available() {
        return;
    }
    // `launch` waits for the header and editor; `quit` for zsh to answer again.
    Pi::launch("quit", "--offline").quit();
}

#[test]
fn pi_echoes_typed_text_inside_its_editor() {
    if !pi_available() {
        return;
    }
    let mut pi = Pi::launch("editor", "--offline");
    pi.term.send("hello from shitty — 你好".as_bytes());
    pi.term
        .wait_until("typed text in pi's editor", STEP_TIMEOUT, |t| {
            // The editor is the last bordered block; its text sits between the rules.
            let borders = editor_borders(t);
            borders.windows(2).last().is_some_and(|pair| {
                (pair[0] + 1..pair[1])
                    .any(|row| t.row_text(row).contains("hello from shitty — 你好"))
            })
        });
    pi.quit();
}

#[test]
fn pi_runs_a_bang_command_and_shows_its_output() {
    if !pi_available() {
        return;
    }
    let mut pi = Pi::launch("bang", "--offline");
    // pi runs `!cmd` with bash itself, no model involved. The typed line holds
    // `$((6*7))`, so only real output contains "e2e 42".
    pi.term.send(b"!echo e2e $((6*7))\r");
    pi.term
        .wait_until("bash output in pi's transcript", STEP_TIMEOUT, |t| {
            (0..ROWS).any(|row| t.row_text(row).trim() == "e2e 42")
        });
    pi.quit();
}

#[test]
fn pi_redraws_for_a_resized_window() {
    if !pi_available() {
        return;
    }
    let mut pi = Pi::launch("resize", "--offline");
    for (cols, rows) in [(70, 20), (120, 40), (COLS, ROWS)] {
        pi.term.resize(cols, rows);
        pi.term
            .wait_until(&format!("pi redrawn at {cols}x{rows}"), STEP_TIMEOUT, |t| {
                editor_borders(t).len() >= 2 && t.screen_text().contains("ctrl+o")
            });
        let grid = pi.term.frame();
        assert_eq!((grid.cols(), grid.rows()), (cols, rows));
    }
    pi.quit();
}

/// A full agent turn: the model calls pi's bash tool and reports the result.
#[test]
#[ignore = "needs pi model credentials and network; run with --ignored"]
fn pi_agent_uses_a_tool_and_answers() {
    if !pi_available() {
        return;
    }
    let model = std::env::var("PI_E2E_MODEL")
        .map(|m| format!("--model '{m}'"))
        .unwrap_or_default();
    let mut pi = Pi::launch("agent", &format!("--tools bash {model}"));
    // The prompt holds `$((1000+337))`, so "1337" on screen came from the run.
    pi.term.send(
        b"Use the bash tool to run exactly: echo $((1000+337)) then reply with only its output.\r",
    );
    // Wait for the whole turn: the tool result is on screen early, but the
    // spinner on the editor border ("Working") stays until the model is done.
    pi.term
        .wait_until("the agent's finished turn", MODEL_TIMEOUT, |t| {
            let screen = t.screen_text();
            screen.contains("1337") && !screen.contains("Working")
        });
    pi.quit();
}
