//! T3 Code adapter (spec §6.5, gate 3 evidence in docs/research/2026-10-08-t3-gate3.md).
pub mod conn;
pub mod detail;
#[cfg(any(test, feature = "testing"))]
pub mod fake;
pub mod map;
pub mod wire;
