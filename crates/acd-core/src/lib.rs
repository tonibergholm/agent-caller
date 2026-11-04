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
pub use idem::{
    Begin, IDEM_CAPACITY, IDEM_CLIENT_IDS_PER_SOURCE, IDEM_CLIENT_SHARE_DIVISOR, IDEM_LOCAL_SHARE_DIVISOR,
    IDEM_SOURCE_SHARE_DIVISOR, IdemCache, IdemLimits, Origin,
};
pub use log::EventLog;
pub use registry::*;
pub mod server;
pub mod tls;
pub use server::{Auth, ServerOptions, router};
