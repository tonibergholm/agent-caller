//! Remote machine secrets in the Windows Credential Manager, the Windows
//! counterpart of `keychain` (decision D7). One generic credential per machine
//! key, named `dev.agentcaller.inbox/<key>`, holding the same JSON
//! (`MachineSecret`: token and pin) the Keychain holds.
//!
//! - Per user: Credential Manager stores credentials under the user's profile,
//!   encrypted with the user's DPAPI key; another user cannot read them.
//! - `CRED_PERSIST_LOCAL_MACHINE`, never `CRED_PERSIST_ENTERPRISE`: the latter
//!   roams with the domain profile, and a pairing token must stay on this PC.
//! - Nothing is logged. Errors carry the Win32 code and never the key's value;
//!   the target name (service and machine key) is not secret but is also left
//!   out of errors.
//! - If the facility is unavailable (no logon session, any other failure) the
//!   call fails. There is no file or settings fallback.
//!
//! The pure parts (name, size limit, error classification) build on every
//! target so Linux tests pin them; the store itself is Windows only and was
//! type-checked, not run, from Linux.
#![cfg_attr(not(windows), allow(dead_code))]

use acd_client::settings::SecretError;

pub const SERVICE_NAME: &str = "dev.agentcaller.inbox";

/// `CRED_MAX_CREDENTIAL_BLOB_SIZE` (wincred.h): 5 * 512 bytes.
pub const MAX_BLOB_BYTES: usize = 2560;
/// Longest machine key accepted. Credential Manager allows far more; machine
/// keys are short, and a bound keeps the name predictable.
pub const MAX_KEY_BYTES: usize = 256;

const ERROR_NOT_FOUND: u32 = 1168;
const ERROR_NO_SUCH_LOGON_SESSION: u32 = 1312;

/// `<service>/<key>`, or an error for a key that is empty, too long, or has
/// control characters. Keys are not trusted: they come from settings.json.
pub fn target_name(key: &str) -> Result<String, SecretError> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES || key.chars().any(char::is_control) {
        return Err(SecretError("invalid secret key".into()));
    }
    Ok(format!("{SERVICE_NAME}/{key}"))
}

/// A value larger than a credential can hold is refused, not truncated: a
/// truncated token would store "successfully" and fail later.
pub fn check_blob(value: &str) -> Result<&[u8], SecretError> {
    if value.len() > MAX_BLOB_BYTES {
        return Err(SecretError("secret is too large for the Credential Manager".into()));
    }
    Ok(value.as_bytes())
}

#[derive(Debug)]
pub enum Outcome {
    NotFound,
    Failed(SecretError),
}

/// What a failed call means. A missing credential is not an error for `get`
/// and `delete`; a missing logon session is the Windows form of a locked
/// keyring; anything else is reported by code only.
pub fn classify(code: u32) -> Outcome {
    match code {
        ERROR_NOT_FOUND => Outcome::NotFound,
        ERROR_NO_SUCH_LOGON_SESSION => Outcome::Failed(SecretError::locked()),
        c => Outcome::Failed(SecretError(format!("Credential Manager error {c}"))),
    }
}

pub fn decode(bytes: Vec<u8>) -> Result<String, SecretError> {
    String::from_utf8(bytes).map_err(|_| SecretError("stored value is not UTF-8".into()))
}

#[cfg(windows)]
mod store {
    use std::{ptr::null_mut, sync::Arc};

    use acd_client::settings::{CachedSecrets, SecretError, SecretStore};
    use windows_sys::Win32::{
        Foundation::GetLastError,
        Security::Credentials::{
            CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
            CRED_TYPE_GENERIC,
        },
    };

    use super::{check_blob, classify, decode, target_name, Outcome};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    pub struct CredentialSecrets;

    impl SecretStore for CredentialSecrets {
        fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
            let target = wide(&target_name(key)?);
            let mut p: *mut CREDENTIALW = null_mut();
            // SAFETY: target is NUL terminated; p receives a buffer freed with CredFree below.
            if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) } == 0 {
                return match classify(unsafe { GetLastError() }) {
                    Outcome::NotFound => Ok(None),
                    Outcome::Failed(e) => Err(e),
                };
            }
            // SAFETY: CredReadW succeeded, so p points at a CREDENTIALW whose blob is
            // CredentialBlobSize bytes; the bytes are copied before CredFree.
            let bytes = unsafe {
                let c = &*p;
                let v = if c.CredentialBlob.is_null() || c.CredentialBlobSize == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
                };
                CredFree(p.cast());
                v
            };
            decode(bytes).map(Some)
        }

        fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
            let target = wide(&target_name(key)?);
            let blob = check_blob(value)?;
            let user = wide("agent-caller");
            let mut cred: CREDENTIALW = unsafe { std::mem::zeroed() };
            cred.Type = CRED_TYPE_GENERIC;
            cred.TargetName = target.as_ptr() as *mut u16;
            cred.UserName = user.as_ptr() as *mut u16;
            cred.CredentialBlobSize = blob.len() as u32;
            cred.CredentialBlob = blob.as_ptr() as *mut u8;
            cred.Persist = CRED_PERSIST_LOCAL_MACHINE;
            // SAFETY: cred and the buffers it points at live through the call; CredWriteW
            // copies them and does not write through the pointers.
            if unsafe { CredWriteW(&cred, 0) } == 0 {
                return match classify(unsafe { GetLastError() }) {
                    Outcome::NotFound => Err(SecretError("Credential Manager error 1168".into())),
                    Outcome::Failed(e) => Err(e),
                };
            }
            Ok(())
        }

        fn delete(&self, key: &str) -> Result<(), SecretError> {
            let target = wide(&target_name(key)?);
            // SAFETY: target is NUL terminated.
            if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
                return match classify(unsafe { GetLastError() }) {
                    Outcome::NotFound => Ok(()),
                    Outcome::Failed(e) => Err(e),
                };
            }
            Ok(())
        }
    }

    /// The store both shells hand to `setup::open_inbox` on Windows. Reads are
    /// cached per process, like the Keychain's.
    pub fn cached_secrets() -> Arc<dyn SecretStore> {
        Arc::new(CachedSecrets::new(CredentialSecrets))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Windows only; touches the real Credential Manager of the current user.
        #[test]
        fn round_trip() {
            let s = CredentialSecrets;
            let key = format!("acd-test-{}", std::process::id());
            s.set(&key, "value").unwrap();
            assert_eq!(s.get(&key).unwrap().as_deref(), Some("value"));
            s.set(&key, "second").unwrap();
            assert_eq!(s.get(&key).unwrap().as_deref(), Some("second"));
            s.delete(&key).unwrap();
            assert_eq!(s.get(&key).unwrap(), None);
            s.delete(&key).unwrap();
            assert!(s.set(&key, &"x".repeat(super::super::MAX_BLOB_BYTES + 1)).is_err());
            assert_eq!(s.get(&key).unwrap(), None);
        }
    }
}

#[cfg(windows)]
pub use store::{cached_secrets, CredentialSecrets};

#[cfg(test)]
mod tests {
    use super::*;
    use acd_client::settings::SecretErrorKind;

    #[test]
    fn target_names_carry_the_service_and_key_only() {
        assert_eq!(target_name("local").unwrap(), "dev.agentcaller.inbox/local");
        assert_eq!(target_name("m-3f9a").unwrap(), "dev.agentcaller.inbox/m-3f9a");
        assert_eq!(SERVICE_NAME, "dev.agentcaller.inbox");
    }

    #[test]
    fn bad_keys_are_refused() {
        for k in ["", "a\nb", "a\0b", "\t", &"k".repeat(MAX_KEY_BYTES + 1)] {
            assert!(target_name(k).is_err(), "{k:?}");
        }
        assert!(target_name(&"k".repeat(MAX_KEY_BYTES)).is_ok());
    }

    #[test]
    fn oversize_values_are_refused_not_truncated() {
        assert_eq!(check_blob(&"a".repeat(MAX_BLOB_BYTES)).unwrap().len(), MAX_BLOB_BYTES);
        assert!(check_blob(&"a".repeat(MAX_BLOB_BYTES + 1)).is_err());
        // The limit is in bytes, not characters.
        assert!(check_blob(&"é".repeat(MAX_BLOB_BYTES / 2 + 1)).is_err());
    }

    /// The largest real secret (a token and a pin as JSON) fits with room to spare.
    #[test]
    fn a_real_machine_secret_fits() {
        let s = acd_client::settings::MachineSecret {
            token: "A".repeat(43),
            pin: "B".repeat(64),
        };
        assert!(s.to_json().len() < MAX_BLOB_BYTES / 4);
    }

    #[test]
    fn errors_classify_and_never_name_a_secret() {
        assert!(matches!(classify(1168), Outcome::NotFound));
        match classify(1312) {
            Outcome::Failed(e) => assert_eq!(e.kind(), SecretErrorKind::Locked),
            o => panic!("{o:?}"),
        }
        match classify(5) {
            Outcome::Failed(e) => {
                assert_eq!(e.kind(), SecretErrorKind::Other);
                assert!(e.to_string().contains("error 5"));
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn non_utf8_is_unreadable_not_lossy() {
        assert!(decode(vec![0xff, 0xfe]).is_err());
        assert_eq!(decode(b"ok".to_vec()).unwrap(), "ok");
    }
}
