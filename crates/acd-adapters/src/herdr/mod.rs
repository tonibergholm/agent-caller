//! herdr adapter (spec §6.4).
pub mod client;
pub mod discovery;
#[cfg(any(test, feature = "testing"))]
pub mod fake;
pub mod fallback;
pub mod map;
pub mod reconciler;
pub mod screen;
pub mod wire;
