# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**Shitty** — a terminal emulator built in Rust using egui for rendering and the `vt100` crate for VT100 parsing. It spawns a `/bin/zsh` shell via a Unix PTY.

## Commands

```bash
cargo build               # debug build
cargo build --release     # release build
cargo run                 # build and run
cargo clippy              # lint
cargo fmt                 # format
```

The binary is output to `target_local/` (custom dir set in `.cargo/config.toml`).

## Architecture

The app is multi-threaded with channels connecting the UI main thread to two PTY worker threads:

```
UI Main Thread (egui or AppKit)
    │  tx_pty_input ──► PTY Write Thread ──► PTY master FD ──► zsh (slave)
    │  rx_pty_output ◄─ PTY Read Thread  ◄── PTY master FD ◄── zsh (slave)
```

**Entry point** (`main.rs`): Routes to either `mac_app::run()` on macOS or `fallback_app::run()` on other platforms.

**egui path** (`fallback_app.rs`): Creates the PTY pair, spawns zsh on the slave, starts the two PTY threads, configures egui fonts (JetBrains Mono Nerd Font, 14pt), and runs the egui event loop. Each frame drains `rx_pty_output` and feeds bytes into `TerminalGrid`, then renders the grid cell-by-cell using the egui painter. Keyboard input is converted to terminal bytes and sent through `tx_pty_input`. Window resize is detected and sent as a `PtyEvent::Resize` to the write thread.

**macOS path** (`mac_app.rs`): Native AppKit window and view with similar PTY threading model. Uses NSTimer for ~60Hz render updates.

**Terminal state** (`terminal/grid.rs: TerminalGrid`): Wraps the `vt100` parser. Processes raw PTY bytes and exposes the resulting grid (cell text, colors, attributes, cursor position).

**PTY events** (`terminal/pty.rs`): Defines `PtyEvent` — either `Input(Vec<u8>)` or `Resize { cols, rows }`. `apply_resize()` sends `TIOCSWINSZ` + `SIGWINCH`.

**Color mapping** (`terminal/color.rs`): Converts ANSI color indices (0–255) and true RGB values to egui `Color32`. Covers ANSI-16, xterm-256 (216 color cube + 24 grayscale), and RGB passthrough.

**Key mapping** (`terminal/keymap.rs`): Translates egui `Key` events (arrows, function keys, Ctrl combos, etc.) into the byte sequences expected by the terminal.
