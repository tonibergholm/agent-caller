//! Glue between acd-voice and acd-client (voice plan D19). It freezes the
//! stamped inbox view into a voice snapshot and implements `InboxPort`
//! over `Inbox`. Logs carry tags, lengths, timings and outcomes only.
mod freeze;
mod port;

pub use freeze::freeze;
pub use port::{AcdPort, voice_outcome};
mod controller;

pub use controller::{Controller, DISPLAY_TIMEOUT, HudSink, InputError};
