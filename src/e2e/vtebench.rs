//! vtebench workloads (`third_party/vtebench`) driven through a real session.
//!
//! Each benchmark runs the way vtebench runs it: the `benchmark` script's
//! output is captured once, repeated up to [`MIN_PAYLOAD_BYTES`], and written
//! to the terminal after `setup`. The test then checks the screen the
//! workload leaves behind and that the shell is still usable afterwards.

use super::harness::Harness;
use crate::terminal::grid::GridView;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const COLS: usize = 80;
const ROWS: usize = 24;
/// Default payload written per benchmark, matching vtebench's 1 MiB minimum.
/// `SHITTY_E2E_PAYLOAD_BYTES` overrides it for longer throughput runs.
const DEFAULT_PAYLOAD_BYTES: usize = 1 << 20;
const PREPARE_TIMEOUT: Duration = Duration::from_secs(180); // shell-loop scripts are slow under parallel tests
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

fn benchmarks_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/vtebench/benchmarks");
    assert!(
        dir.join("light_cells/benchmark").exists(),
        "vtebench is missing at {}; run `git submodule update --init`",
        dir.display()
    );
    dir
}

fn min_payload_bytes() -> usize {
    std::env::var("SHITTY_E2E_PAYLOAD_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PAYLOAD_BYTES)
}

/// A `ps` that trims its output. The scripts redirect from
/// `/dev/$(ps -o tty= -p $$)`, and macOS pads that column (`ttys008 `), so
/// the redirect names a file that does not exist and `tput` reports nothing.
fn install_ps_shim(bin: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(bin).unwrap();
    let ps = bin.join("ps");
    fs::write(&ps, "#!/bin/sh\n/bin/ps \"$@\" | sed 's/[[:space:]]*$//'\n").unwrap();
    fs::set_permissions(&ps, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Shell-quote `path` for a single-quoted argument.
fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// Run one vtebench benchmark, then hand the held screen to `check`.
fn run_benchmark(name: &str, check: impl FnOnce(&Harness)) {
    let bench = benchmarks_dir().join(name);
    let setup = bench.join("setup");
    let setup = setup.exists().then_some(setup);
    let uses_alt_screen = setup
        .as_ref()
        .is_some_and(|s| fs::read_to_string(s).unwrap().contains("1049h"));

    let work = std::env::temp_dir().join(format!("shitty-e2e-{}-{name}", std::process::id()));
    fs::create_dir_all(&work).unwrap();
    install_ps_shim(&work.join("bin"));

    // Driver scripts keep the typed command line short: macOS caps a tty
    // input line at 1024 bytes, and a long line typed while zsh is still busy
    // gets cut off. The benchmark reads the size from its controlling tty, so
    // it runs inside the session even though its output goes to a file.
    let prepare = work.join("prepare.sh");
    fs::write(
        &prepare,
        format!(
            "export PATH={bin}:$PATH\n\
             {script} > once && [ -s once ] || exit 1\n\
             cp once payload\n\
             while [ $(wc -c < payload) -lt {min_bytes} ]; do\n\
             \tcat payload payload > next && mv next payload\n\
             done\n",
            bin = quote(&work.join("bin")),
            script = quote(&bench.join("benchmark")),
            min_bytes = min_payload_bytes(),
        ),
    )
    .unwrap();
    let run = work.join("run.sh");
    fs::write(
        &run,
        format!(
            "export PATH={bin}:$PATH\n{setup}cat payload\n",
            bin = quote(&work.join("bin")),
            setup = setup
                .as_ref()
                .map(|s| format!("{}\n", quote(s)))
                .unwrap_or_default(),
        ),
    )
    .unwrap();

    let mut term = Harness::spawn(COLS, ROWS);
    term.run(&format!("cd {}", quote(&work)));
    let (prepared, _) = term.run_to_completion(&format!("sh {}", quote(&prepare)), PREPARE_TIMEOUT);
    let payload_bytes = fs::metadata(work.join("payload"))
        .unwrap_or_else(|_| {
            panic!(
                "{name}: benchmark produced no output\n{}",
                term.screen_text()
            )
        })
        .len();

    let before = term.bytes_seen();
    let elapsed = term.run_and_hold(&format!("sh {}", quote(&run)), RUN_TIMEOUT);
    let written = term.bytes_seen() - before;
    eprintln!(
        "vtebench {name}: {payload_bytes} B payload, {written} B through the PTY in {elapsed:.2?} \
         ({:.1} MiB/s)",
        written as f64 / elapsed.as_secs_f64() / (1 << 20) as f64
    );

    let grid = term.frame();
    assert_eq!(
        (grid.cols(), grid.rows()),
        (COLS, ROWS),
        "{name}: grid resized"
    );
    let (row, col) = grid.cursor_pos();
    assert!(
        row < ROWS && col < COLS,
        "{name}: cursor out of bounds at {row},{col}"
    );
    check(&term);

    // Leave the workload's modes, then make sure the shell still answers and,
    // for alternate-screen workloads, that the primary screen came back.
    term.release();
    term.run_to_completion(r"printf '\e[r\e[?1049l'", RUN_TIMEOUT);
    if uses_alt_screen {
        assert!(
            term.screen_text().contains(&prepared),
            "{name}: primary screen not restored after leaving the alternate screen\n{}",
            term.screen_text()
        );
    }

    let _ = fs::remove_dir_all(&work);
}

/// The held screen, row by row, against `expected` (missing rows must be blank).
fn assert_screen(term: &Harness, expected: &[String]) {
    let actual: Vec<String> = (0..ROWS).map(|row| term.row_text(row)).collect();
    let mut expected = expected.to_vec();
    expected.resize(ROWS, String::new());
    assert_eq!(actual, expected, "\n{}", term.screen_text());
}

/// `n` copies of `line`.
fn lines(line: &str, n: usize) -> Vec<String> {
    vec![line.to_owned(); n]
}

/// A row of `ch` across the full width.
fn full(ch: char) -> String {
    ch.to_string().repeat(COLS)
}

/// The vim session vtebench replays leaves its shell prompt on screen.
fn assert_vim_session(term: &Harness) {
    let screen = term.screen_text();
    assert!(
        screen.contains(" UL  ~/programming/alacritty/alacritty  master ↑  exit"),
        "{screen}"
    );
}

macro_rules! vtebench_tests {
    ($($name:ident => $check:expr;)*) => {
        $(
            #[test]
            fn $name() {
                run_benchmark(stringify!($name), $check);
            }
        )*

        /// New upstream benchmarks should get a test here, not be skipped silently.
        #[test]
        fn every_vtebench_benchmark_has_a_test() {
            let mut upstream: Vec<String> = fs::read_dir(benchmarks_dir())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().join("benchmark").exists())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            upstream.sort();
            let mut covered = vec![$(stringify!($name).to_owned()),*];
            covered.sort();
            assert_eq!(upstream, covered);
        }
    };
}

// Expected screens are the last frame of each workload at 80x24. The cursor
// row is blank wherever a workload ends in a newline.
vtebench_tests! {
    cursor_motion => |t| assert_screen(t, &lines(&full('Z'), ROWS));
    dense_cells => |t| assert_screen(t, &lines(&full('Z'), ROWS));
    light_cells => |t| assert_screen(t, &lines(&full('Z'), ROWS));
    medium_cells => assert_vim_session;
    sync_medium_cells => assert_vim_session;
    scrolling => |t| assert_screen(t, &lines("y", ROWS - 1));
    scrolling_fullscreen => |t| {
        let letters: Vec<String> = ('D'..='Z').map(full).collect();
        assert_screen(t, &letters);
    };
    // DECSTBM homes the cursor to row 0, so the top-region workloads write
    // their first line above the region and scroll the rest inside it.
    scrolling_top_region => |t| assert_screen(t, &lines("y", ROWS - 1));
    scrolling_top_small_region => |t| assert_screen(t, &lines("y", ROWS - 1));
    // Region is rows 1..=23: the last region row holds the cursor, row 24 is untouched.
    scrolling_bottom_region => |t| assert_screen(t, &lines("y", ROWS - 2));
    // Region is rows 1..=12.
    scrolling_bottom_small_region => |t| assert_screen(t, &lines("y", ROWS / 2 - 1));
    unicode => |t| {
        // The symbol table ends with the emoticons block, two cells each.
        let emoji = |range: std::ops::RangeInclusive<u32>| -> String {
            range.filter_map(char::from_u32).collect()
        };
        assert_eq!(t.row_text(ROWS - 3), emoji(0x1F600..=0x1F627), "\n{}", t.screen_text());
        assert_eq!(t.row_text(ROWS - 2), emoji(0x1F628..=0x1F64F), "\n{}", t.screen_text());
        assert_eq!(t.row_text(ROWS - 1), "", "\n{}", t.screen_text());
    };
}
