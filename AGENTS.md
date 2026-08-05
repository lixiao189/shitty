# Repository Guidelines

## Project Structure & Module Organization

This is **Shitty**, a small terminal emulator written in Rust (2024 edition). The crate is a binary with no separate `tests/` directory; unit tests live inline in source modules.

- `src/main.rs` — entry point that runs the macOS AppKit backend.
- `src/mac_app/` — native macOS AppKit window, view, and renderer.
- `src/terminal/` — terminal core: `grid.rs` (vt100 parser wrapper), `color.rs` (ANSI/xterm/RGB color mapping), `keymap.rs` (key translation), `pty.rs` (PTY lifecycle and resize).
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
- Prefer small, focused modules; keep platform-specific code behind the entry-point routing in `main.rs`.

## Testing Guidelines

- Tests are inline `#[cfg(test)] mod tests` blocks, co-located with the code they exercise (see `src/terminal/keymap.rs` and `src/mac_app.rs`).
- Name tests for the behavior they verify, e.g., `test_key_translation` or a descriptive sentence case.
- Run the full suite with `cargo test` before opening a PR; no coverage threshold is enforced.

## Commit & Pull Request Guidelines

- Use Conventional Commits with a type and optional scope, matching existing history: `feat:`, `fix:`, `refactor:`, `docs:`, `repo:`, `perf:`, `init:` (e.g., `fix(keyboard): correct arrow key mapping`).
- Open focused PRs that describe what changed and why, reference the issue being addressed, and include screenshots or GIFs for UI-affecting changes.

## Platform-Specific Notes

- This project targets macOS only: the AppKit path (`src/mac_app/`) uses objc2 and the AppKit frameworks.
