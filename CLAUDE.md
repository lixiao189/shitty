# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**Shitty** — a macOS-only terminal emulator in Rust (2024 edition). AppKit (via `objc2`) draws the window, [`libghostty-vt`](https://github.com/Uzaaft/libghostty-rs) parses ANSI/VT, and each tab runs `/bin/zsh` on a Unix PTY. The crate is a library (`src/lib.rs`) plus a thin binary (`main.rs` calls `mac_app::run()`).

Building `libghostty-vt` needs **Zig 0.17**. `.cargo/config.toml` sets `ZIG=tools/zig`, a wrapper that downloads the pinned, SHA-256-verified Zig into `~/.cache/shitty/zig/` on first use; an exported `ZIG` overrides it. To bump Zig, change `ZIG_VERSION` and the checksums in `tools/zig` together. `libghostty-vt` is pinned to an exact git rev, because the crates.io 0.2.2 release needs Zig 0.15.2.

## Commands

```bash
cargo run                                   # build and run
cargo build --release
cargo fmt --check                           # CI gate
cargo clippy --all-targets -- -D warnings   # CI gate (warnings fail)
cargo test                                  # unit tests + e2e (vtebench, vim)
cargo test shell_env                        # tests whose path matches a substring
cargo test e2e::vim -- --nocapture          # one e2e module, with output
git submodule update --init                 # required once for the vtebench e2e tests
tools/package-macos.sh 0.1.0                # build Shitty.app + .zip/.dmg into dist/
```

Build output goes to `target_local/`, not `target/`.

Dev builds compile Ghostty as `ReleaseFast` (a `[profile.dev.package.libghostty-vt-sys]` override in `Cargo.toml`). Zig `Debug` scrolls too slowly and the vtebench tests time out, so don't remove it.

## Architecture

The UI thread owns every session. Each tab has two PTY worker threads that only move bytes. `TerminalGrid` is `!Send` and never leaves the main thread.

```
UI Main Thread (AppKit)
    │  tx_input ──► PTY Write Thread ──► PTY master FD ──► zsh (slave)
    │  rx_output ◄─ PTY Read Thread  ◄── PTY master FD ◄── zsh (slave)
```

- **Wake-ups:** reader threads signal `mac_app/shell/wake.rs`. It coalesces output bursts (8 ms) into one drain on the main run loop, and also forwards shell exits and resize deadlines.
- **Resize:** `terminal/io/resize.rs` (`ResizeDebouncer`) applies grid and PTY resizes together, only after the size has been stable for 150 ms. Output is therefore never parsed at a width it wasn't generated for.
- **Reads vs writes:** sessions expose `frame() -> &impl GridView` for reads, and methods (`scroll_by`, `resize_grid`, `set_theme`, …) for writes. Rendering, selection and chrome only ever see `GridView`.
- **Shell spawn** (`terminal/io/pty.rs`): zsh starts as a login shell (`argv[0] = -zsh`) in `$HOME`, with `TERM=xterm-256color`, `COLORTERM`, `TERM_PROGRAM` set, and `LANG` defaulted to `en_US.UTF-8`. This matters because an app launched from Finder/Dock inherits launchd's bare environment (cwd `/`, no `TERM`/`LANG`). `cargo run` hides that, since it inherits your terminal's env.
- **Themes** (`terminal/theme/`): chosen at startup from `SHITTY_THEME`, then `theme` in `$SHITTY_CONFIG` (default `~/.config/shitty/config.toml`), then `Theme::Default`. Picking a theme in the View menu writes it back to that file. Only the default fg/bg and the 16 ANSI slots are themed.
- **Menus** (`mac_app/app/menu.rs`): shortcut items and the theme list are data rows, not functions on the app delegate.

**E2E tests** (`src/e2e/`, `#[cfg(test)]`, no separate `tests/` dir):
- `harness.rs` drives a real `TerminalSession` (PTY + libghostty) headlessly. It `exec`s `zsh -f` so dotfiles can't affect prompts or timing, and reads the screen back through `GridView`.
- `vtebench.rs` replays every workload from the `third_party/vtebench` submodule and asserts the final screen.
- `vim.rs` drives Vim as a fullscreen TUI and skips when `vim` isn't on `PATH`.
- `shell_env.rs` checks the spawned shell's cwd and environment.

## Module ownership

Each directory has one owner. A change that only affects that area shouldn't edit another owner's files. When two areas must change together (a new `GridView` method, a new menu action), add the item on the owning side first and keep the other side's edit to a call site. Don't reach into another module's private helpers.

| Module | Owner area | Public interface | Cross-module rule |
| --- | --- | --- | --- |
| `terminal/grid` | VT / grid (libghostty) | `TerminalGrid`, `GridView`, `CellInfo`, `CellPos`, `CellRun`, `CursorInfo`, `CursorShape`, `display_cells` | Only this module imports `libghostty_vt`. `term/` mutates; `view.rs` is the read-only interface. |
| `terminal/io` | PTY and session I/O | `Pty`, `PtyEvent`, `TerminalSession`, `SessionOptions`, `ResizeDebouncer` | Sessions own the grid. Callers use `frame()` and session methods, not `grid` fields. |
| `terminal/input` | Keys and paste | `control_key_bytes`, `append_input_from_text`, `append_paste_text`, `append_input_from_selector` | No AppKit view types. |
| `terminal/theme` | Colors and theme config | `Theme`, `ColorScheme`, `Color32`, `xterm_256_color`, `configured_theme`, `save_theme` | Named themes in `catalog.rs`, palettes in `palette.rs`, config file in `config.rs`, `Color32` in `color.rs`. |
| `terminal/selection` | Selection | `Selection` | Reads cells through `GridView`. |
| `mac_app/render` | Painting | `paint_terminal`, `GlyphCache`, `text_attributes`, `snap_to_pixel` | Paint from `GridView` snapshots. Don't scroll, resize, or parse bytes here. |
| `mac_app/chrome` | Tabs and scrollbar | `paint_tab_bar`, `tab_bar_hit`, `paint_scrollbar`, tab key helpers | Hit-testing in `tab_bar/layout.rs`, drawing in `tab_bar/paint.rs`. |
| `mac_app/shell` | The `NSView` and its state | `ShittyTerminalView`, `TerminalViewState` | Forward events to chrome, render, input and the session. No drawing or VT parsing. |
| `mac_app/app` | Process and menus | `run`, `mark_theme_menu` | New menu items are rows in `menu.rs`. |
| `mac_app/layout` | Shared rectangles | `terminal_bounds`, `grid_size_for`, `TAB_BAR_HEIGHT` | No session or grid types. |
| `mac_app/font` | Typeface | `load_terminal_font`, `measure_cell_size` | No terminal state. |
| `e2e` | End-to-end tests | `Harness` (test-only) | Drive sessions through `TerminalSession`'s public methods, read via `GridView`. Product code never imports it. |

## Conventions

- Comment a variable with a trailing comment on its definition line (`let shape = ...; // set via DECSCUSR`), not a comment above it. `///` doc comments stay above items.
- Unit tests are inline `#[cfg(test)] mod tests` blocks, named for the behavior they check (`resize_is_applied_after_debounce_deadline`).
- Commits use Conventional Commits with an optional scope (`fix(pty): …`, `test(e2e): …`, `ci: …`).

## CI and releases

- `.github/workflows/ci.yml` runs fmt, clippy and test on `macos-latest`. Its check, `fmt, clippy, test`, is required by branch protection on `main`, so work goes through a PR.
- `.github/workflows/release.yml` reruns CI, then publishes `Shitty.app` (arm64, ad-hoc signed, not notarized) as a `.zip` and a `.dmg` with SHA-256 sums.
  - **Official release:** bump `version` in `Cargo.toml`, merge, then push tag `vX.Y.Z`. The tag must match `Cargo.toml`.
  - **Nightly:** a rolling `nightly` pre-release built from `main` daily at 03:17 UTC, skipped if `main` hasn't changed. Trigger it with `gh workflow run release.yml` (add `-f force=true` to rebuild an unchanged `main`).
