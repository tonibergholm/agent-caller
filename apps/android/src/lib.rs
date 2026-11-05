//! Android shell for the agent-caller inbox: a foreground client that reaches
//! the daemon over Tailscale, like the iOS shell. It runs acd-client's
//! `Inbox` in a native library that a small Java activity loads. No push, no
//! relay, no background work; the local `acd.sock` is never offered.
//!
//! - `model`: the screen the activity draws, decided here.
//! - `bridge`: the inbox and what the user can do to it.
//! - `jni_glue`: the JNI exports and the Keystore cipher behind `KeyWrap`.

pub mod bridge;
pub mod jni_glue;
pub mod model;
