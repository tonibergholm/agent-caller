//! The acd daemon core: registry, request lifecycle, event log and acd/1 server.
pub mod adapter;
pub mod engine;
pub mod idem;
pub mod log;
pub mod registry;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use adapter::{Adapter, DeliverError};
pub use engine::{Core, IDEM_TTL, MAX_ID_BYTES, MAX_TEXT_BYTES, RING_CAPACITY, Txn};
pub use idem::{Begin, IDEM_CAPACITY, IdemCache};
pub use log::EventLog;
pub use registry::*;
pub mod server;
pub mod tls;
pub use server::{Auth, ServerOptions, router};
