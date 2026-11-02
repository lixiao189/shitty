# Repository Guidelines

## Project Structure & Module Organization

This is **Shitty**, a small terminal emulator written in Rust (2024 edition). The crate builds a library (`src/lib.rs`) plus a binary; there is no separate `tests/` directory and unit tests live inline in source modules.

- `src/lib.rs` — library root exposing the `mac_app` module.
- `src/main.rs` — thin binary entry point that calls `shitty::mac_app::run()`.
- `src/terminal/grid/` — libghostty VT state. `view.rs` is the read-only `GridView` the UI paints; `term/` mutates the terminal.
- `src/terminal/io/` — PTY threads (`pty.rs`), per-tab sessions (`session.rs`), and resize debouncing (`resize.rs`).
- `src/terminal/input/` — AppKit key and paste translation.
- `src/terminal/theme/` — `Color32` (`color.rs`), named themes (`catalog.rs`), Catppuccin palettes (`palette.rs`), and the config file (`config.rs`).
- `src/terminal/selection/` — cell-range selection over a `GridView`.
- `src/mac_app/app/` — `NSApplication` launch and the main menu (`menu.rs`).
- `src/mac_app/shell/` — the `NSView`, window state, IME, wake-ups, and resize scheduling.
- `src/mac_app/render/` — glyph cache and painting. `paint/` splits cells, glyph placement, the cursor, and the selection highlight.
- `src/mac_app/chrome/` — tab commands, the scrollbar, and the tab strip (`tab_bar/layout.rs` for hits, `tab_bar/paint.rs` for drawing).
- `src/mac_app/layout/` — rectangles shared by the tab strip and the grid.
- `src/mac_app/font/` — bundled face and cell metrics.
- `src/e2e/` — test-only end-to-end suite: a headless `TerminalSession` harness, the vtebench workloads, and the `pi` agent TUI.
- `third_party/vtebench/` — git submodule with alacritty's vtebench; run `git submodule update --init` after cloning.
- `assets/` — bundled Nerd Font variants and images (e.g., `arch.png`, `icon.png`).
- `target_local/` — build output; the custom target dir is set in `.cargo/config.toml`.

The renderer and the rest of the UI read a session through `TerminalSession::frame() -> &impl GridView`. They do not depend on libghostty types or on `TerminalGrid` fields.

## Build, Test, and Development Commands

`libghostty-vt` is compiled from source with **Zig 0.17**, provided automatically by `tools/zig` (pinned and checksum-verified, selected via `ZIG` in `.cargo/config.toml`; export `ZIG` to override). The crates.io 0.2.2 crate needs Zig 0.15.2, which this tree does not use.

```bash
cargo build          # debug build
cargo build --release  # optimized release build
cargo run            # build and run the terminal locally
cargo clippy         # lint; keep it warning-free
cargo fmt            # format the codebase
cargo test           # run all inline unit tests and the e2e suite
cargo test e2e -- --ignored  # also the live pi agent turn (needs model credentials)
```

## Coding Style & Naming Conventions

- Follow `rustfmt` defaults and keep `cargo clippy` clean (no project-specific config files exist).
- Use standard Rust naming: `snake_case` for functions and variables, `UpperCamelCase` for types, `SCREAMING_SNAKE_CASE` for constants.
- Comment variables with a trailing comment after the definition, not above it (e.g. `let shape = ...; // shape set via DECSCUSR`). Doc comments (`///`) on types, functions, and struct fields stay above the item.
- Prefer small, focused modules; keep platform-specific code in `src/mac_app/`, routed through `lib.rs` so `main.rs` stays a thin entry point.

## Parallel work / module ownership

One directory, one owner. Stay inside the owner directory unless the public interface listed below has to change.

| Module | Owner area | Public interface | Cross-module rule |
| --- | --- | --- | --- |
| `terminal/grid` | VT / grid via libghostty | `TerminalGrid`, `GridView`, cell and cursor snapshots, `display_cells` | Only this module imports `libghostty_vt`. Callers use `GridView`. |
| `terminal/io` | PTY and sessions | `Pty`, `PtyEvent`, `TerminalSession`, `SessionOptions`, `ResizeDebouncer` | Read the grid with `frame()`. Mutate it with session methods. |
| `terminal/input` | Keys and paste | byte translators for keys, paste, and selectors | No view or grid types. |
| `terminal/theme` | Theming and colors | `Theme`, `ColorScheme`, `Color32`, `configured_theme`, `save_theme` | New palettes go in `palette.rs`. Config-file parsing stays in `config.rs`. |
| `terminal/selection` | Selection | `Selection` | Ask a `GridView` for cells. |
| `mac_app/render` | Painting | `paint_terminal`, `GlyphCache`, `text_attributes`, `snap_to_pixel` | Paint snapshots. Do not scroll, resize, or parse PTY bytes. |
| `mac_app/chrome` | Tab and window chrome | tab-bar hit/paint, scrollbar, tab key helpers | Layout and hit-testing stay in `tab_bar/layout.rs`. |
| `mac_app/shell` | The view | `ShittyTerminalView`, `TerminalViewState` | Dispatch events to chrome, render, input, and the session. |
| `mac_app/app` | App shell | `run`, `mark_theme_menu` | Menu commands are rows in `menu.rs`, not new functions on the delegate. |
| `mac_app/layout` | Shared geometry | `terminal_bounds`, `grid_size_for`, `TAB_BAR_HEIGHT` | No terminal types. |
| `mac_app/font` | Fonts | `load_terminal_font`, `measure_cell_size` | No terminal types. |
| `e2e` | End-to-end tests | `Harness` (test-only) | Use `TerminalSession` methods and `GridView`; product code never imports it. |

A cross-module change adds or uses the public item and stops there. Do not reach into another module's private helpers to avoid editing its interface.

## Testing Guidelines

- Tests are inline `#[cfg(test)] mod tests` blocks, co-located with the code they exercise (see `src/terminal/input/mod.rs`, `src/terminal/grid/`, `src/terminal/io/resize.rs`, `src/terminal/selection/mod.rs`, `src/mac_app/font/mod.rs`, and `src/mac_app/app/menu.rs`).
- Name tests for the behavior they verify in descriptive sentence case, e.g., `control_chars_map_for_shell_keys` or `resize_is_applied_after_debounce_deadline`.
- Run the full suite with `cargo test` before opening a PR; no coverage threshold is enforced.

## Commit & Pull Request Guidelines

- Use Conventional Commits with a type and optional scope, matching existing history: `feat:`, `fix:`, `refactor:`, `docs:`, `repo:`, `perf:`, `init:` (e.g., `fix(keyboard): correct arrow key mapping`).
- Open focused PRs that describe what changed and why, reference the issue being addressed, and include screenshots or GIFs for UI-affecting changes.

## Platform-Specific Notes

- This project targets macOS only: the AppKit path (`src/mac_app/`) uses objc2 and the AppKit frameworks.
- `libghostty-vt` needs Zig 0.17; `tools/zig` fetches the pinned release automatically (bump `ZIG_VERSION` and the checksums there together). Export `ZIG=/path/to/zig` to use another binary.
