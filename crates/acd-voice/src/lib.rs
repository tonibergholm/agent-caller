//! Voice control core (voice spec §4.1). No UI, platform, acd-client or
//! logging code: every voice decision is plain Rust that Linux CI tests.
pub mod fast;
pub mod hud;
pub mod listen;
pub mod plan;
pub mod refusal;
pub mod router;
pub mod safety;
pub mod snapshot;
pub mod speak;
pub mod speech;
pub mod text;
