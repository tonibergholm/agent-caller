//! Remote machine secrets in the login Keychain (decision D7).
use acd_client::settings::{SecretError, SecretStore};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

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

/// Stores a throwaway value, verifies it through the SecretStore interface, and removes it.
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
        (Ok(actual), Ok(())) => Err(SecretError(format!(
            "Keychain readback mismatch: expected throwaway value, got {actual:?}"
        ))),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(SecretError(format!("Keychain cleanup failed: {error}"))),
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
        set_generic_password(&self.service, key, value.as_bytes())
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
