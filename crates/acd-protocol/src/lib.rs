//! Wire types for the acd/1 protocol. See PROTOCOL.md.
mod messages;
mod model;
pub mod pairing;
pub mod schema;

pub use messages::*;
pub use model::*;

/// The protocol identifier clients send in `hello`.
pub const PROTOCOL: &str = "acd/1";
/// Capabilities advertised by a v1 daemon.
pub const CAPABILITIES: [&str; 3] = ["answer", "send", "mark_seen"];
