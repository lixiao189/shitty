# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**Shitty** — a macOS terminal emulator built in Rust. AppKit draws the window. [`libghostty-vt`](https://github.com/Uzaaft/libghostty-rs) parses ANSI/VT. Each tab spawns a `/bin/zsh` shell via a Unix PTY.

Building `libghostty-vt` needs **Zig 0.17**. `.cargo/config.toml` sets `ZIG=tools/zig`, a wrapper that downloads the pinned, SHA-256-verified Zig release into `~/.cache/shitty/zig/` on first use, so no manual install is needed; an explicitly exported `ZIG` overrides it. The crates.io 0.2.2 release needs Zig 0.15.2 instead, so this repo pins a git revision that matches Zig 0.17.

## Commands

```bash
cargo build               # debug build
cargo build --release     # release build
cargo run                 # build and run
cargo clippy              # lint
cargo fmt                 # format
cargo test                # inline unit tests + e2e (vtebench, vim)
```

The binary is output to `target_local/` (custom dir set in `.cargo/config.toml`).

## Architecture

The UI thread owns every session. Two PTY worker threads per tab move bytes; the grid is `!Send` and never leaves the main thread.

```
UI Main Thread (AppKit)
    │  tx_input ──► PTY Write Thread ──► PTY master FD ──► zsh (slave)
    │  rx_output ◄─ PTY Read Thread  ◄── PTY master FD ◄── zsh (slave)
```

**E2E tests** (`e2e/`, `#[cfg(test)]`): `harness.rs` drives a real `TerminalSession` (PTY + `zsh -f` + libghostty) headlessly and reads the screen through `GridView`. `vtebench.rs` replays every workload from the `third_party/vtebench` git submodule (run `git submodule update --init` after cloning) and asserts the final screen; `vim.rs` drives Vim (preinstalled on macOS) as a fullscreen TUI and skips when `vim` is not on `PATH`. Dev builds compile Ghostty as `ReleaseFast` (a profile override in `Cargo.toml`); Zig `Debug` scrolls too slowly for these tests.

**Entry point** (`main.rs`): calls `mac_app::run()`.

**App shell** (`mac_app/app/`): `NSApplication` launch, the window, and the main menu. Shortcut items and the View menu's theme list are data in `app/menu.rs`, not one-off functions in the app delegate.

**Shell** (`mac_app/shell/`): the `NSView` subclass, per-window state, IME, the run-loop wake, and resize debouncing. It forwards drawing to `render` and the tab strip to `chrome`.

**Rendering** (`mac_app/render/`): paints a [`GridView`](src/terminal/grid/view.rs). Cell runs, glyph placement, the cursor, and the selection highlight are separate files under `render/paint/`. It does not talk to libghostty.

**Chrome** (`mac_app/chrome/`): tab commands, the tab strip (`chrome/tab_bar/`, layout vs paint), and the scrollbar.

**Layout** (`mac_app/layout/`): view rectangles shared by chrome and the renderer (tab-strip height, terminal bounds, grid size).

**Font** (`mac_app/font/`): the bundled face and cell metrics.

**Terminal core** (`terminal/`):

- `grid/` — libghostty `Terminal`, scrollback, and the read-only `GridView` the UI is allowed to use.
- `io/` — PTY threads, `TerminalSession`, resize debouncing.
- `input/` — AppKit keys and paste to terminal bytes.
- `theme/` — palettes (`palette.rs`), the config file (`config.rs`), and `Color32`.
- `selection/` — a cell range over a `GridView`.

Sessions expose `frame() -> &impl GridView` for reads and methods (`scroll_by`, `resize_grid`, `set_theme`, …) for writes. UI code does not reach into `TerminalGrid` fields.

## Parallel work / module ownership

Each directory has one owner. A change that only affects that area should not edit another owner's files.

| Module | Owner area | Public interface | Cross-module rule |
| --- | --- | --- | --- |
| `terminal/grid` | VT / grid (libghostty) | `TerminalGrid`, `GridView`, `CellPos`, `CellRun`, `CursorInfo`, `CursorShape`, `display_cells` | UI and selection depend on `GridView` only. Do not import `libghostty_vt` outside `grid/`. |
| `terminal/io` | PTY and session I/O | `Pty`, `PtyEvent`, `TerminalSession`, `SessionOptions`, `ResizeDebouncer` | Sessions own the grid. Callers use `frame()` and the session methods, not `grid` fields. |
| `terminal/input` | Keys and paste | `control_key_bytes`, `append_input_from_text`, `append_paste_text`, `append_input_from_selector` | No AppKit view types. |
| `terminal/theme` | Colors and theme config | `Theme`, `ColorScheme`, `Color32`, `configured_theme`, `save_theme`, `xterm_256_color` | Palettes stay in `palette.rs`; the config file stays in `config.rs`. |
| `terminal/selection` | Selection | `Selection` | Reads cells through `GridView`. |
| `mac_app/render` | Painting | `paint_terminal`, `GlyphCache`, `text_attributes`, `snap_to_pixel` | Paint from `GridView` snapshots. Do not scroll or parse bytes here. |
| `mac_app/chrome` | Tabs and scrollbar | `paint_tab_bar`, `tab_bar_hit`, `paint_scrollbar`, tab key helpers | Hit-testing lives in `tab_bar/layout.rs`; drawing in `tab_bar/paint.rs`. |
| `mac_app/shell` | The `NSView` and its state | `ShittyTerminalView`, `TerminalViewState` | Forward events. Do not add drawing or VT parsing here. |
| `mac_app/app` | Process and menus | `run`, `mark_theme_menu` | New menu items go in the shortcut table in `menu.rs`. |
| `mac_app/layout` | Shared rectangles | `terminal_bounds`, `grid_size_for`, `TAB_BAR_HEIGHT` | No session or grid types. |
| `mac_app/font` | Typeface | `load_terminal_font`, `measure_cell_size` | No terminal state. |
| `e2e` | End-to-end tests | `Harness` (test-only) | Drive sessions through `TerminalSession`'s public methods and read via `GridView`; no product code depends on it. |

When two areas must change together (a new `GridView` method, a new menu action), add the type or method on the owning side first and keep the other side's edit to a call site.
