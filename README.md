# Shitty

```
        _            _       _     _        _          _    _        _
       / /\         / /\    / /\  /\ \     /\ \       /\ \ /\ \     /\_\
      / /  \       / / /   / / /  \ \ \    \_\ \      \_\ \\ \ \   / / /
     / / /\ \__   / /_/   / / /   /\ \_\   /\__ \     /\__ \\ \ \_/ / /
    / / /\ \___\ / /\ \__/ / /   / /\/_/  / /_ \ \   / /_ \ \\ \___/ /
    \ \ \ \/___// /\ \___\/ /   / / /    / / /\ \ \ / / /\ \ \\ \ \_/
     \ \ \     / / /\/___/ /   / / /    / / /  \/_// / /  \/_/ \ \ \
 _    \ \ \   / / /   / / /   / / /    / / /      / / /         \ \ \
/_/\__/ / /  / / /   / / /___/ / /__  / / /      / / /           \ \ \
\ \/___/ /  / / /   / / //\__\/_/___\/_/ /      /_/ /             \ \_\
 \_____\/   \/_/    \/_/ \/_________/\_\/       \_\/               \/_/
```

A tiny terminal emulator in Rust. Uses [libghostty-vt](https://github.com/Uzaaft/libghostty-rs)
(Ghostty's VT parser) for parsing, a PTY for the shell, and a native AppKit renderer on macOS.

## Quick Start

```bash
cargo run
```

Other useful commands:

```bash
cargo build
cargo build --release
cargo clippy
cargo fmt
cargo test                    # unit + e2e (vtebench workloads, vim TUI)
```

The e2e suite needs the vtebench submodule: `git submodule update --init`.

Binary output: `target_local/` (see `.cargo/config.toml`).

## Releases

`.github/workflows/release.yml` publishes `Shitty.app` (Apple Silicon) as a `.zip`
and a `.dmg`, each with a SHA-256 file, on the GitHub Releases page. Every release
runs the full CI suite first.

- **Official:** bump `version` in `Cargo.toml`, merge to `main`, then
  `git tag v0.2.0 && git push origin v0.2.0`. The tag must match `Cargo.toml`.
- **Nightly:** built from `main` every day at 03:17 UTC as the rolling `nightly`
  pre-release; skipped when `main` has not changed. Run it now with
  `gh workflow run release.yml` (add `-f force=true` to rebuild an unchanged `main`).

To package locally: `tools/package-macos.sh 0.1.0` (writes to `dist/`).
Builds are ad-hoc signed, not notarized: open the app the first time with right-click > Open.

## Features

- ANSI/VT parsing via `libghostty-vt` with a cell grid model.
- PTY-backed shell (`/bin/zsh`) with proper resize signaling.
- ANSI 16-color, xterm 256-color, and truecolor RGB support.
- Wide-character handling and underline rendering.
- Cursor rendering with inverse cell colors and DECSCUSR shape support.
- 1000-row scrollback with a scrollbar and mouse-wheel scrolling.
- Mouse text selection with clipboard copy/paste (⌘C/⌘V, ESC clears selection).
- Font zoom (⌘+/⌘−/⌘0) and IME composition support.
- macOS AppKit renderer with native key handling.
- Nerd Font monospace rendering (bundled fonts in `assets/`).

## Architecture

![Architecture](assets/arch.png)

### Core Data Flow

```mermaid
flowchart LR
  APP[NSApp + NSWindow] --> VIEW[ShittyTerminalView]
  VIEW --> GRID[TerminalGrid + libghostty-vt parser]
  VIEW -- key events --> TX[tx_pty_input]
  TX --> W[PTY Write Thread]
  W --> MFD[PTY master FD]
  MFD --> SHELL["/bin/zsh (PTY slave)"]
  SHELL --> MFD
  MFD --> R[PTY Read Thread]
  R --> RX[rx_pty_output]
  RX --> VIEW
```

## Project Layout

- `src/main.rs`: binary entry point calling `shitty::mac_app::run()`; `src/lib.rs` is the library root.
- `src/terminal/`: platform-independent terminal core.
  - `grid/`: the libghostty-vt `Terminal` and scrollback (`term/`), plus the
    read-only `GridView` (`view.rs`) the UI paints from. The only module that
    imports `libghostty_vt`.
  - `io/`: PTY spawn and IO threads (`pty.rs`), per-tab `TerminalSession`
    (`session.rs`), and resize debouncing (`resize.rs`).
  - `input/`: AppKit key and paste translation to terminal bytes.
  - `theme/`: named themes, palettes, `Color32`, and the config file.
  - `selection/`: cell-range selection over a `GridView`.
- `src/mac_app/`: native AppKit UI.
  - `app/`: `NSApplication` launch, the window, and the main menu (`menu.rs`).
  - `shell/`: the `NSView` subclass, window state, IME, and the PTY wake bridge.
  - `render/`: glyph cache and painting (cells, text, cursor, selection).
  - `chrome/`: tabs, the tab strip, and the scrollbar.
  - `layout/`: rectangles shared by the tab strip and the grid.
  - `font/`: the bundled face and cell metrics.
- `src/e2e/`: test-only end-to-end suite (vtebench workloads, Vim, shell environment).

## Notes

- Rendering is event-driven: PTY output is coalesced on the main run loop
  (8ms debounce) and redraws go through `setNeedsDisplay`/`drawRect`.
- Window resizes are debounced for 150ms, then applied to the grid and PTY together.
- Resizes propagate through TIOCSWINSZ and SIGWINCH to keep shells happy.

## Build Requirements

- Rust 2024 edition (see `Cargo.toml`).
- Zig 0.17 to build libghostty-vt. `tools/zig` downloads a pinned, checksum-verified
  copy into `~/.cache/shitty/zig/` on first build; export `ZIG` to use your own.
- macOS-only build; uses objc2 + AppKit frameworks (no extra setup required).
