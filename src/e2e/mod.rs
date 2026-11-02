//! End-to-end tests: real PTYs, a real shell, and real programs, read back
//! through the same [`GridView`](crate::terminal::grid::GridView) the renderer paints.
//!
//! - [`vtebench`] — the vtebench workloads from `third_party/vtebench`.
//! - [`vim`] — Vim as a fullscreen TUI, skipped when `vim` is not installed.

mod harness;
mod vim;
mod vtebench;
