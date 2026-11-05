//! The seam between the engine and the wake sender (relay spec §5.1).
//!
//! `acd-core` knows nothing about the relay. The daemon gives the core one
//! [`PushControl`] when push is configured: the core then advertises the
//! `push` capability, routes `push.*` commands to it and tells it when a
//! request opens. With none set the core behaves as before.

use acd_protocol::{ErrorCode, PushInfo, PushPlatform, Timestamp};
use async_trait::async_trait;

/// What `push.register` carries, minus the command id. The handle is a relay
/// secret: `Debug` shows its length only.
#[derive(Clone, PartialEq, Eq)]
pub struct PushRegistration {
    pub relay: String,
    pub handle: String,
    pub platform: PushPlatform,
    pub expires_at: Timestamp,
}

impl std::fmt::Debug for PushRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushRegistration")
            .field("relay", &self.relay)
            .field("handle_bytes", &self.handle.len())
            .field("platform", &self.platform)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// A refused or failed push command. `message` is fixed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushFailure {
    pub code: ErrorCode,
    pub message: &'static str,
}

#[async_trait]
pub trait PushControl: Send + Sync {
    /// A request opened. Called while the engine lock is held, so it must not
    /// block, and it takes no argument on purpose: nothing about the request
    /// can reach the sender through it.
    fn request_opened(&self);
    async fn register(&self, client_id: &str, reg: PushRegistration) -> Result<(), PushFailure>;
    async fn unregister(&self, client_id: &str) -> Result<(), PushFailure>;
    async fn status(&self, client_id: &str) -> PushInfo;
}
