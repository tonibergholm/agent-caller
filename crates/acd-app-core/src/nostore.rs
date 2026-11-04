//! The secret store for platforms that have no secure one yet (Android; Windows
//! has `credman`). It implements the same `SecretStore` trait as the Apple Keychain
//! store and refuses every call, so a machine secret is never written
//! anywhere and never read from anywhere. The user sees a plain error, not a
//! pairing that silently lost its token.
//!
//! shortcut: no machine secret can be stored on Android, so a remote machine
//! cannot be paired there; upgrade with an Android Keystore-wrapped file behind
//! this same trait. Do not add a file or plain-settings fallback in its place.
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
/// the Keychain on Apple targets, the Credential Manager on Windows, the
/// refusing store everywhere else.
pub fn platform_secrets() -> Arc<dyn SecretStore> {
    #[cfg(target_vendor = "apple")]
    {
        crate::keychain::cached_secrets()
    }
    #[cfg(windows)]
    {
        crate::credman::cached_secrets()
    }
    #[cfg(not(any(target_vendor = "apple", windows)))]
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

    #[cfg(not(any(target_vendor = "apple", windows)))]
    #[test]
    fn the_platform_store_refuses_here() {
        assert!(platform_secrets().set("k", "v").is_err());
        assert!(platform_secrets().get("k").is_err());
    }
}
