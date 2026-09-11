# Repository Guidelines

## Project Structure & Module Organization

This is **Shitty**, a small terminal emulator written in Rust (2024 edition). The crate builds a library (`src/lib.rs`) plus a binary; there is no separate `tests/` directory and unit tests live inline in source modules.

- `src/lib.rs` — library root exposing the `mac_app` module.
- `src/main.rs` — thin binary entry point that calls `shitty::mac_app::run()`.
- `src/mac_app/` — native macOS AppKit window, view, and renderer. The view is
  split by component under `src/mac_app/view/`: `terminal_view.rs` (NSView sub-
  class, events, IME), `state.rs`, `tabs.rs`, `tab_bar.rs`, `scrollbar.rs`,
  `renderer.rs`, `resize.rs`, `geometry.rs`, `ime.rs`, `wake.rs`.
- `src/terminal/` — terminal core: `grid.rs` (alacritty_terminal parser wrapper), `color.rs` (ANSI/xterm/RGB color mapping), `keymap.rs` (key translation), `pty.rs` (PTY lifecycle and resize), `resize.rs` (resize debouncing), `selection.rs` (text selection).
- `assets/` — bundled Nerd Font variants and images (e.g., `arch.png`, `icon.png`).
- `target_local/` — build output; the custom target dir is set in `.cargo/config.toml`.

## Build, Test, and Development Commands

```bash
cargo build          # debug build
cargo build --release  # optimized release build
cargo run            # build and run the terminal locally
cargo clippy         # lint; keep it warning-free
cargo fmt            # format the codebase
cargo test           # run all inline unit tests
```

## Coding Style & Naming Conventions

- Follow `rustfmt` defaults and keep `cargo clippy` clean (no project-specific config files exist).
- Use standard Rust naming: `snake_case` for functions and variables, `UpperCamelCase` for types, `SCREAMING_SNAKE_CASE` for constants.
- Comment variables with a trailing comment after the definition, not above it (e.g. `let shape = ...; // shape set via DECSCUSR`). Doc comments (`///`) on types, functions, and struct fields stay above the item.
- Prefer small, focused modules; keep platform-specific code in `src/mac_app/`, routed through `lib.rs` so `main.rs` stays a thin entry point.

## Testing Guidelines

- Tests are inline `#[cfg(test)] mod tests` blocks, co-located with the code they exercise (see `src/terminal/keymap.rs`, `src/terminal/grid.rs`, `src/terminal/resize.rs`, `src/terminal/selection.rs`, `src/mac_app/font.rs`, and `src/mac_app/app.rs`).
- Name tests for the behavior they verify in descriptive sentence case, e.g., `control_chars_map_for_shell_keys` or `resize_is_applied_after_debounce_deadline`.
- Run the full suite with `cargo test` before opening a PR; no coverage threshold is enforced.

## Commit & Pull Request Guidelines

- Use Conventional Commits with a type and optional scope, matching existing history: `feat:`, `fix:`, `refactor:`, `docs:`, `repo:`, `perf:`, `init:` (e.g., `fix(keyboard): correct arrow key mapping`).
- Open focused PRs that describe what changed and why, reference the issue being addressed, and include screenshots or GIFs for UI-affecting changes.

## Platform-Specific Notes

- This project targets macOS only: the AppKit path (`src/mac_app/`) uses objc2 and the AppKit frameworks.
