//! herdr adapter (spec §6.4).
pub mod client;
#[cfg(any(test, feature = "testing"))]
pub mod fake;
pub mod wire;
