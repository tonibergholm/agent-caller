//! The secret store for platforms that have no secure one yet (Windows). It implements the same `SecretStore` trait as the Apple Keychain
//! store and refuses every call, so a machine secret is never written
//! anywhere and never read from anywhere. The user sees a plain error, not a
//! pairing that silently lost its token.
//!
//! shortcut: no machine secret can be stored on Windows, so a remote machine
//! cannot be paired there; upgrade with Credential Manager behind this same
//! trait. Android has its own store (`wrapped`, an Android Keystore key
//! wrapping a file); it refuses until the shell installs it. Do not add a
//! file or plain-settings fallback in place of either.
use std::sync::Arc;

use acd_client::settings::{SecretError, SecretStore};

/// What every call says.
pub const NO_SECURE_STORE: &str = "no secure store on this platform";

#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedSecrets;

impl SecretStore for UnsupportedSecrets {
    fn get(&self, _key: &str) -> Result<Option<String>, SecretError> {
        Err(SecretError(NO_SECURE_STORE.into()))
    }
    fn set(&self, _key: &str, _value: &str) -> Result<(), SecretError> {
        Err(SecretError(NO_SECURE_STORE.into()))
    }
    fn delete(&self, _key: &str) -> Result<(), SecretError> {
        Err(SecretError(NO_SECURE_STORE.into()))
    }
}

/// The store a shell hands to `setup::open_inbox` without naming a platform:
/// the Keychain on Apple targets, the Keystore-wrapped store on Android (the
/// refusing one until the shell installs it), the refusing store elsewhere.
pub fn platform_secrets() -> Arc<dyn SecretStore> {
    #[cfg(target_vendor = "apple")]
    {
        crate::keychain::cached_secrets()
    }
    #[cfg(target_os = "android")]
    {
        crate::wrapped::installed_or_refusing()
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "android")))]
    {
        Arc::new(UnsupportedSecrets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acd_client::settings::SecretStatus;

    #[test]
    fn every_call_refuses_and_nothing_is_stored() {
        let s = UnsupportedSecrets;
        assert!(s.set("k", "v").unwrap_err().to_string().contains(NO_SECURE_STORE));
        // A refused write must not read back as a stored value or as "missing".
        assert!(s.get("k").is_err());
        assert!(s.delete("k").is_err());
        assert_eq!(s.status("k"), SecretStatus::Unreadable);
    }

    #[cfg(not(any(target_vendor = "apple", target_os = "android")))]
    #[test]
    fn the_platform_store_refuses_here() {
        assert!(platform_secrets().set("k", "v").is_err());
        assert!(platform_secrets().get("k").is_err());
    }
}
