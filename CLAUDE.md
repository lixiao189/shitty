# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**Shitty** — a macOS terminal emulator built in Rust using AppKit and the `alacritty_terminal` crate for ANSI/VT parsing. It spawns a `/bin/zsh` shell via a Unix PTY.

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
UI Main Thread (AppKit)
    │  tx_pty_input ──► PTY Write Thread ──► PTY master FD ──► zsh (slave)
    │  rx_pty_output ◄─ PTY Read Thread  ◄── PTY master FD ◄── zsh (slave)
```

**Entry point** (`main.rs`): Runs `mac_app::run()` (macOS-only).

**AppKit path** (`mac_app/`): Creates the PTY pair, spawns zsh on the slave, starts the two PTY threads, opens an NSWindow, and renders the grid cell-by-cell in the terminal view. Keyboard input is converted to terminal bytes and sent through `tx_pty_input`. Window resize is detected and sent as a `PtyEvent::Resize` to the write thread. An NSTimer drives render updates at ~120Hz.

**Terminal state** (`terminal/grid.rs: TerminalGrid`): Wraps the `alacritty_terminal` terminal/parser. Processes raw PTY bytes and exposes the resulting grid (cell text, colors, attributes, cursor position).

**PTY events** (`terminal/pty.rs`): Defines `PtyEvent` — either `Input(Vec<u8>)` or `Resize { cols, rows }`. `apply_resize()` sends `TIOCSWINSZ` + `SIGWINCH`.

**Color mapping** (`terminal/color.rs`): Converts ANSI color indices (0–255) and true RGB values into an RGBA color type consumed by AppKit. Covers ANSI-16, xterm-256 (216 color cube + 24 grayscale), and RGB passthrough.

**Key mapping** (`terminal/keymap.rs`): Translates AppKit key events (editing selectors, Control combos, and text input) into the byte sequences expected by the terminal.
