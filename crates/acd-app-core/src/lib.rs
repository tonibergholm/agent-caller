//! Platform-independent core of the agent-caller apps: opening the inbox,
//! pairing import, the Keychain secret store (Apple targets; a refusing store
//! elsewhere, see `nostore`) and the command
//! functions that do not depend on a window, tray, hotkey or voice.
//!
//! The Mac app and the iOS shell wrap these in their own `#[tauri::command]`
//! functions. The shells own the Tauri state, events and platform commands.

pub mod commands;
pub mod nostore;
pub mod pairing;
pub mod setup;

pub use nostore::platform_secrets;

#[cfg(target_vendor = "apple")]
pub mod keychain;
