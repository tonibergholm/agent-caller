//! Platform-independent core of the agent-caller apps: opening the inbox,
//! pairing import, the secret store (the Keychain on Apple targets, the
//! Credential Manager on Windows; a refusing store elsewhere, see `nostore`) and the command
//! functions that do not depend on a window, tray, hotkey or voice.
//!
//! The Mac app and the iOS shell wrap these in their own `#[tauri::command]`
//! functions. The shells own the Tauri state, events and platform commands.

pub mod commands;
pub mod credman;
pub mod nostore;
pub mod pairing;
pub mod setup;

pub use nostore::platform_secrets;

#[cfg(target_vendor = "apple")]
pub mod keychain;
