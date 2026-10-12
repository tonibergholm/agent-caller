//! Linux-only backends: the Secret Service (omarchy spec §9) and D-Bus
//! notifications (§8). The logic they plug into is tested on every platform
//! with fakes.
pub mod dbus;
pub mod keyring;
