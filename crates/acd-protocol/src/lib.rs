//! Wire types for the acd/1 protocol. See PROTOCOL.md.
mod messages;
mod model;
pub mod pairing;
pub mod schema;
pub mod text;

pub use messages::*;
pub use model::*;

/// The protocol identifier clients send in `hello`.
pub const PROTOCOL: &str = "acd/1";
/// Capabilities advertised by a v1 daemon.
pub const CAPABILITIES: [&str; 4] = ["answer", "send", "mark_seen", "answer.revision"];

/// Largest WebSocket frame or message either side accepts (PROTOCOL.md §1).
pub const MAX_FRAME_BYTES: usize = 1 << 20;
/// Most agents a client holds for one machine. More ends the connection.
pub const MAX_AGENTS_PER_MACHINE: usize = 1000;
/// Largest encoded `Agent` a client keeps. A larger one ends the connection.
pub const MAX_AGENT_BYTES: usize = 64 * 1024;
