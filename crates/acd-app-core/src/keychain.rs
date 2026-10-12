//! Remote machine secrets in the login Keychain (decision D7) on macOS, and in
//! the app's Keychain on iOS. The same service name on both platforms.
use std::sync::Arc;

use acd_client::settings::{CachedSecrets, SecretError, SecretStore};
use security_framework::passwords::{delete_generic_password, get_generic_password};
#[cfg(not(target_os = "ios"))]
use security_framework::passwords::set_generic_password;
#[cfg(target_os = "ios")]
use security_framework::access_control::{ProtectionMode, SecAccessControl};
#[cfg(target_os = "ios")]
use security_framework::passwords::set_generic_password_options;
#[cfg(target_os = "ios")]
use security_framework::passwords_options::PasswordOptions;

pub const SERVICE_NAME: &str = "dev.agentcaller.inbox";
const NOT_FOUND: i32 = -25300; // errSecItemNotFound

pub struct KeychainSecrets {
    service: String,
}

impl KeychainSecrets {
    pub fn new(service: &str) -> Self {
        Self {
            service: service.into(),
        }
    }
}

/// The store both apps hand to `setup::open_inbox`. Reads are cached per
/// process: the Keychain can ask the user for every read, so listings must not
/// go back to it.
pub fn cached_secrets() -> Arc<dyn SecretStore> {
    Arc::new(CachedSecrets::new(KeychainSecrets::new(SERVICE_NAME)))
}

// GATE (Task 13): remove in Task 16.
/// Stores a throwaway value, verifies it through the SecretStore interface, and removes it.
#[allow(dead_code)]
pub fn gate_roundtrip() -> Result<(), SecretError> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let keychain = KeychainSecrets::new(SERVICE_NAME);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| SecretError(error.to_string()))?
        .as_nanos();
    let key = format!("gate-roundtrip-{nonce}");
    let value = format!("agent-caller-gate-{nonce}");

    keychain.set(&key, &value)?;
    let check = keychain.get(&key);
    let cleanup = keychain.delete(&key);

    match (check, cleanup) {
        (Ok(Some(actual)), Ok(())) if actual == value => Ok(()),
        (Ok(_), Ok(())) => Err(SecretError("Keychain readback mismatch".into())),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(SecretError(format!("Keychain cleanup failed: {error}"))),
    }
}

impl KeychainSecrets {
    /// Stores one value. Writes the macOS login Keychain item as before.
    #[cfg(not(target_os = "ios"))]
    fn write(&self, key: &str, value: &[u8]) -> security_framework::base::Result<()> {
        set_generic_password(&self.service, key, value)
    }

    /// Stores one value on iOS, device-only: the item is not restored from a
    /// backup or moved to another device. `set_generic_password` only rewrites
    /// the value of an existing item (`SecItemUpdate`), so an item stored before
    /// this change would keep its old protection class. The item is therefore
    /// deleted and added again. The two calls are not atomic: if the add fails
    /// after the delete, the old value is gone and the machine must be paired
    /// again.
    #[cfg(target_os = "ios")]
    fn write(&self, key: &str, value: &[u8]) -> security_framework::base::Result<()> {
        match delete_generic_password(&self.service, key) {
            Ok(()) => {}
            Err(error) if error.code() == NOT_FOUND => {}
            Err(error) => return Err(error),
        }
        let access = SecAccessControl::create_with_protection(
            Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
            0,
        )?;
        let mut options = PasswordOptions::new_generic_password(&self.service, key);
        options.set_access_control(access);
        set_generic_password_options(value, options)
    }
}

impl SecretStore for KeychainSecrets {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        match get_generic_password(&self.service, key) {
            Ok(bytes) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| SecretError("stored value is not UTF-8".into())),
            Err(error) if error.code() == NOT_FOUND => Ok(None),
            Err(error) => Err(SecretError(error.to_string())),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        self.write(key, value.as_bytes())
            .map_err(|error| SecretError(error.to_string()))
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        match delete_generic_password(&self.service, key) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == NOT_FOUND => Ok(()),
            Err(error) => Err(SecretError(error.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "touches the login Keychain; run manually in Task 13"]
    fn round_trip() {
        let keychain = KeychainSecrets::new("dev.agentcaller.inbox.test");
        keychain.set("t1", "value").unwrap();
        assert_eq!(keychain.get("t1").unwrap().as_deref(), Some("value"));
        keychain.delete("t1").unwrap();
        assert_eq!(keychain.get("t1").unwrap(), None);
        keychain.delete("t1").unwrap();
    }
}
