//! End-to-end tests: real PTYs, a real shell, and real programs, read back
//! through the same [`GridView`](crate::terminal::grid::GridView) the renderer paints.
//!
//! - [`vtebench`] — the vtebench workloads from `third_party/vtebench`.
//! - [`pi`] — the `pi` coding agent's TUI, skipped when `pi` is not installed.

mod harness;
mod pi;
mod vtebench;
