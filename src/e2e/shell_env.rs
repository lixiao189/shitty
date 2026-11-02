//! A fresh session starts where Terminal.app would: in `$HOME`, with `TERM`
//! and a UTF-8 locale set, whatever environment the app itself inherited.

use super::harness::Harness;
use std::time::Duration;

#[test]
fn shell_starts_in_home_with_term_set() {
    let home = std::env::var("HOME").expect("HOME is set for tests");
    let mut harness = Harness::spawn(120, 24);
    harness.run("print -r -- \"<$PWD|$TERM|$TERM_PROGRAM|${LANG:+lang}>\"");
    harness.wait_for_text(
        &format!("<{home}|xterm-256color|shitty|lang>"),
        Duration::from_secs(10),
    );
}
