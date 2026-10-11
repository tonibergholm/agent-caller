//! Voice control core (voice spec §4.1). No UI, platform, acd-client or
//! logging code: every voice decision is plain Rust that Linux CI tests.
pub mod fast;
pub mod refusal;
pub mod snapshot;
pub mod text;
