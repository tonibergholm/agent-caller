//! The acd daemon core: registry, request lifecycle, event log and acd/1 server.
pub mod log;
pub mod registry;

pub use log::EventLog;
pub use registry::*;
