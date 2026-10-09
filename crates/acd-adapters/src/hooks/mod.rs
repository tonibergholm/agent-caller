//! Claude and Codex hooks (spec §6.1 to §6.3).
pub mod claude_sessions;
pub mod dialogs;
pub mod endpoint;
pub mod evidence;
pub mod mapping;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod wire;
