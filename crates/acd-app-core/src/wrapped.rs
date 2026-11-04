//! A secret store that keeps each secret in its own file in app-private
//! storage, encrypted by a key the platform never lets the app read. On
//! Android the key is an AES-GCM key in the Android Keystore and the Java side
//! of the shell implements [`KeyWrap`] through JNI. The module compiles on
//! every platform so the store, and its fail-closed rules, are tested on the
//! host with a fake cipher.
//!
//! Rules, all tested:
//! - A missing file reads as `Ok(None)`. Nothing else does: an unreadable file,
//!   a blob the cipher refuses, a cipher that is not available, or a blob that
//!   was written for another key (the key name is the AAD) is an `Err`, so the
//!   machine shows as unreadable and is never taken for "not paired".
//! - A failed write leaves the old file in place (temp file, then rename).
//! - Error text never holds a secret, a key name or a path.
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use acd_client::settings::{CachedSecrets, SecretError, SecretStore};

/// File header: the format name and version, so a later format is not read as this one.
const MAGIC: &[u8; 5] = b"ACDW\x01";
/// Domain separation for the AAD, so a blob wrapped for this store is not
/// accepted by the cipher for any other purpose.
const AAD_PREFIX: &[u8] = b"dev.agentcaller.secret.v1\0";
/// Machine keys are UUIDs. The cap keeps the hex file name under 255 bytes.
const MAX_KEY_LEN: usize = 100;

pub const KEYSTORE_UNAVAILABLE: &str = "secure store unavailable";
pub const SECRET_UNREADABLE: &str = "stored secret is unreadable";
pub const SECRET_NOT_STORED: &str = "could not store the secret";
pub const BAD_SECRET_KEY: &str = "secret key rejected";

/// Why a cipher call failed. No detail travels with it: the Java side's
/// messages could name a key alias or a provider, and nothing may reach a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapError {
    /// The platform key store cannot be used now (not initialised, no key).
    Unavailable,
    /// The cipher refused: wrong key, wrong AAD, damaged blob.
    Failed,
}

/// Authenticated encryption under a platform-held key. `aad` is bound into the
/// blob: `unwrap` must fail unless the same `aad` is given.
pub trait KeyWrap: Send + Sync {
    fn wrap(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, WrapError>;
    fn unwrap(&self, aad: &[u8], blob: &[u8]) -> Result<Vec<u8>, WrapError>;
}

pub struct WrappedFileSecrets<W> {
    dir: PathBuf,
    cipher: W,
}

impl<W: KeyWrap> WrappedFileSecrets<W> {
    /// `dir` is app-private storage. It is created on the first write.
    pub fn new(dir: impl Into<PathBuf>, cipher: W) -> Self {
        Self { dir: dir.into(), cipher }
    }

    fn path(&self, key: &str) -> Result<PathBuf, SecretError> {
        if key.is_empty() || key.len() > MAX_KEY_LEN {
            return Err(SecretError(BAD_SECRET_KEY.into()));
        }
        // Hex of the key: injective, and no key can name a path.
        let mut name = String::from("s-");
        for byte in key.as_bytes() {
            name.push_str(&format!("{byte:02x}"));
        }
        Ok(self.dir.join(name))
    }
}

fn aad(key: &str) -> Vec<u8> {
    let mut aad = AAD_PREFIX.to_vec();
    aad.extend_from_slice(key.as_bytes());
    aad
}

fn io_error(what: &str, error: &std::io::Error) -> SecretError {
    SecretError(format!("{what} ({:?})", error.kind()))
}

impl<W: KeyWrap> SecretStore for WrappedFileSecrets<W> {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        let path = self.path(key)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(SECRET_UNREADABLE, &error)),
        };
        let blob = bytes
            .strip_prefix(MAGIC.as_slice())
            .ok_or_else(|| SecretError(SECRET_UNREADABLE.into()))?;
        let plain = self.cipher.unwrap(&aad(key), blob).map_err(|error| match error {
            WrapError::Unavailable => SecretError(KEYSTORE_UNAVAILABLE.into()),
            WrapError::Failed => SecretError(SECRET_UNREADABLE.into()),
        })?;
        String::from_utf8(plain)
            .map(Some)
            .map_err(|_| SecretError(SECRET_UNREADABLE.into()))
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let path = self.path(key)?;
        let blob = self.cipher.wrap(&aad(key), value.as_bytes()).map_err(|error| match error {
            WrapError::Unavailable => SecretError(KEYSTORE_UNAVAILABLE.into()),
            WrapError::Failed => SecretError(SECRET_NOT_STORED.into()),
        })?;
        let mut content = MAGIC.to_vec();
        content.extend_from_slice(&blob);
        fs::create_dir_all(&self.dir).map_err(|e| io_error(SECRET_NOT_STORED, &e))?;
        write_atomic(&path, &content).map_err(|e| io_error(SECRET_NOT_STORED, &e))
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        match fs::remove_file(self.path(key)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(SECRET_NOT_STORED, &error)),
        }
    }
}

/// Writes `content` to a sibling temp file, syncs it and renames it over
/// `path`, so a crash leaves the old file or the new one, never half of one.
fn write_atomic(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // App-private storage is already owner-only; this is belt and braces.
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(content)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// What `platform_secrets()` returns on Android until the shell has installed
/// the Keystore-backed store, and if it never does. It refuses every call.
pub struct KeystoreNotInstalled;

impl SecretStore for KeystoreNotInstalled {
    fn get(&self, _key: &str) -> Result<Option<String>, SecretError> {
        Err(SecretError(KEYSTORE_UNAVAILABLE.into()))
    }
    fn set(&self, _key: &str, _value: &str) -> Result<(), SecretError> {
        Err(SecretError(KEYSTORE_UNAVAILABLE.into()))
    }
    fn delete(&self, _key: &str) -> Result<(), SecretError> {
        Err(SecretError(KEYSTORE_UNAVAILABLE.into()))
    }
}

static INSTALLED: OnceLock<Arc<dyn SecretStore>> = OnceLock::new();

/// Called once by the Android shell, with the app's private directory and the
/// Keystore cipher. Reads are cached per process, as the Keychain store's are.
/// Returns `false` if a store was already installed (the first one stays).
pub fn install(dir: impl Into<PathBuf>, cipher: Arc<dyn KeyWrap>) -> bool {
    let store: Arc<dyn SecretStore> =
        Arc::new(CachedSecrets::new(WrappedFileSecrets::new(dir, cipher)));
    INSTALLED.set(store).is_ok()
}

/// The installed store, or the refusing one.
pub fn installed_or_refusing() -> Arc<dyn SecretStore> {
    store_or_refusing(INSTALLED.get().cloned())
}

fn store_or_refusing(installed: Option<Arc<dyn SecretStore>>) -> Arc<dyn SecretStore> {
    installed.unwrap_or_else(|| Arc::new(KeystoreNotInstalled))
}

impl KeyWrap for Arc<dyn KeyWrap> {
    fn wrap(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, WrapError> {
        (**self).wrap(aad, plaintext)
    }
    fn unwrap(&self, aad: &[u8], blob: &[u8]) -> Result<Vec<u8>, WrapError> {
        (**self).unwrap(aad, blob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acd_client::settings::{SecretErrorKind, SecretStatus};
    use std::sync::atomic::{AtomicBool, Ordering};

    const SECRET: &str = r#"{"client_token":"TOKEN-VALUE-MUST-NOT-LEAK"}"#;
    const KEY_A: &str = "5f1c9d2e-8a1b-4c3d-9e8f-0a1b2c3d4e5f";
    const KEY_B: &str = "6a2d0e3f-9b2c-4d4e-8f9a-1b2c3d4e5f60";

    /// A stand-in for the Keystore: it is not secure, it only has the same
    /// contract. The blob is `tag(aad) ‖ plaintext ^ 0x5a`, and `unwrap`
    /// refuses unless the tag matches the AAD it is given.
    #[derive(Default)]
    struct FakeKeystore {
        unavailable: AtomicBool,
    }

    fn tag(aad: &[u8]) -> [u8; 8] {
        // FNV-1a: stable, no dependency.
        let mut h: u64 = 0xcbf29ce484222325;
        for b in aad {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100000001b3);
        }
        h.to_be_bytes()
    }

    impl KeyWrap for FakeKeystore {
        fn wrap(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, WrapError> {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(WrapError::Unavailable);
            }
            let mut blob = tag(aad).to_vec();
            blob.extend(plaintext.iter().map(|b| b ^ 0x5a));
            Ok(blob)
        }
        fn unwrap(&self, aad: &[u8], blob: &[u8]) -> Result<Vec<u8>, WrapError> {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(WrapError::Unavailable);
            }
            if blob.len() < 8 || blob[..8] != tag(aad) {
                return Err(WrapError::Failed);
            }
            Ok(blob[8..].iter().map(|b| b ^ 0x5a).collect())
        }
    }

    fn store(dir: &Path) -> WrappedFileSecrets<FakeKeystore> {
        WrappedFileSecrets::new(dir, FakeKeystore::default())
    }

    #[test]
    fn a_secret_round_trips_and_the_file_does_not_hold_it() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.set(KEY_A, SECRET).unwrap();
        assert_eq!(s.get(KEY_A).unwrap().as_deref(), Some(SECRET));
        for entry in fs::read_dir(dir.path()).unwrap() {
            let bytes = fs::read(entry.unwrap().path()).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("TOKEN-VALUE"));
        }
    }

    #[test]
    fn a_missing_file_is_none_and_delete_of_a_missing_file_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        assert_eq!(s.get(KEY_A).unwrap(), None);
        s.delete(KEY_A).unwrap();
        s.set(KEY_A, SECRET).unwrap();
        s.delete(KEY_A).unwrap();
        assert_eq!(s.get(KEY_A).unwrap(), None);
    }

    #[test]
    fn set_replaces_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.set(KEY_A, "one").unwrap();
        s.set(KEY_A, "two").unwrap();
        assert_eq!(s.get(KEY_A).unwrap().as_deref(), Some("two"));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    /// The key name is the AAD: a blob copied to another key's file must not
    /// open. Without the AAD (an empty one) this test fails: the swapped file
    /// would read back as the other machine's secret.
    #[test]
    fn a_blob_moved_to_another_keys_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.set(KEY_A, SECRET).unwrap();
        let from = s.path(KEY_A).unwrap();
        let to = s.path(KEY_B).unwrap();
        fs::copy(from, to).unwrap();
        let err = s.get(KEY_B).unwrap_err();
        assert_eq!(err.0, SECRET_UNREADABLE);
    }

    #[test]
    fn a_damaged_or_foreign_file_is_an_error_never_none() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.set(KEY_A, SECRET).unwrap();
        let path = s.path(KEY_A).unwrap();
        // Truncated blob.
        let mut bytes = fs::read(&path).unwrap();
        bytes.truncate(MAGIC.len() + 3);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(s.get(KEY_A).unwrap_err().0, SECRET_UNREADABLE);
        // Wrong header.
        fs::write(&path, b"plain text, not a blob").unwrap();
        assert_eq!(s.get(KEY_A).unwrap_err().0, SECRET_UNREADABLE);
        // Empty file.
        fs::write(&path, b"").unwrap();
        assert_eq!(s.get(KEY_A).unwrap_err().0, SECRET_UNREADABLE);
        assert_eq!(s.status(KEY_A), SecretStatus::Unreadable);
    }

    #[test]
    fn an_unavailable_keystore_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.set(KEY_A, SECRET).unwrap();
        s.cipher.unavailable.store(true, Ordering::SeqCst);
        // An existing secret is an error, not "missing".
        assert_eq!(s.get(KEY_A).unwrap_err().0, KEYSTORE_UNAVAILABLE);
        assert_ne!(s.status(KEY_A), SecretStatus::Missing);
        // A write fails and keeps the old file.
        assert_eq!(s.set(KEY_A, "new").unwrap_err().0, KEYSTORE_UNAVAILABLE);
        s.cipher.unavailable.store(false, Ordering::SeqCst);
        assert_eq!(s.get(KEY_A).unwrap().as_deref(), Some(SECRET));
        // A first write with no Keystore stores nothing.
        s.cipher.unavailable.store(true, Ordering::SeqCst);
        assert!(s.set(KEY_B, SECRET).is_err());
        s.cipher.unavailable.store(false, Ordering::SeqCst);
        assert_eq!(s.get(KEY_B).unwrap(), None);
    }

    #[test]
    fn a_secret_that_is_not_utf8_is_an_error() {
        struct Raw;
        impl KeyWrap for Raw {
            fn wrap(&self, _: &[u8], p: &[u8]) -> Result<Vec<u8>, WrapError> {
                Ok(p.to_vec())
            }
            fn unwrap(&self, _: &[u8], _: &[u8]) -> Result<Vec<u8>, WrapError> {
                Ok(vec![0xff, 0xfe])
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let s = WrappedFileSecrets::new(dir.path(), Raw);
        s.set(KEY_A, "x").unwrap();
        assert_eq!(s.get(KEY_A).unwrap_err().0, SECRET_UNREADABLE);
    }

    #[test]
    fn keys_cannot_name_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        for bad in ["", &"k".repeat(MAX_KEY_LEN + 1)] {
            assert_eq!(s.get(bad).unwrap_err().0, BAD_SECRET_KEY);
            assert_eq!(s.set(bad, SECRET).unwrap_err().0, BAD_SECRET_KEY);
            assert_eq!(s.delete(bad).unwrap_err().0, BAD_SECRET_KEY);
        }
        // "../escape" is a legal key: it is hex-named inside the directory.
        s.set("../escape", SECRET).unwrap();
        assert_eq!(s.get("../escape").unwrap().as_deref(), Some(SECRET));
        assert!(fs::read_dir(dir.path()).unwrap().all(|e| e.unwrap().file_name().to_string_lossy().starts_with("s-")));
        assert!(!dir.path().parent().unwrap().join("escape").exists());
    }

    #[test]
    fn errors_never_echo_values_keys_or_paths() {
        // The directory is a file, so every write fails with an io error.
        let blocker = tempfile::NamedTempFile::new().unwrap();
        let broken = store(&blocker.path().join("sub"));
        let err = broken.set(KEY_A, SECRET).unwrap_err();
        assert!(err.0.starts_with(SECRET_NOT_STORED), "{}", err.0);
        let path = blocker.path().to_str().unwrap();
        assert!(!err.0.contains("TOKEN-VALUE") && !err.0.contains(KEY_A) && !err.0.contains(path));
        // A read that fails with an io error other than NotFound.
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        fs::create_dir(s.path(KEY_A).unwrap()).unwrap();
        let err = s.get(KEY_A).unwrap_err();
        assert!(err.0.starts_with(SECRET_UNREADABLE), "{}", err.0);
        assert!(!err.0.contains(KEY_A) && !err.0.contains(dir.path().to_str().unwrap()));
    }

    #[test]
    fn the_kind_of_every_error_is_other() {
        // Not a locked or unknown keyring: the status stays Unreadable.
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.cipher.unavailable.store(true, Ordering::SeqCst);
        assert_eq!(s.set(KEY_A, SECRET).unwrap_err().kind(), SecretErrorKind::Other);
    }

    #[test]
    fn an_uninstalled_keystore_refuses_everything() {
        let s = store_or_refusing(None);
        assert_eq!(s.get(KEY_A).unwrap_err().0, KEYSTORE_UNAVAILABLE);
        assert_eq!(s.set(KEY_A, SECRET).unwrap_err().0, KEYSTORE_UNAVAILABLE);
        assert_eq!(s.delete(KEY_A).unwrap_err().0, KEYSTORE_UNAVAILABLE);
        assert_eq!(s.status(KEY_A), SecretStatus::Unreadable);
        let installed: Arc<dyn SecretStore> = Arc::new(acd_client::settings::MemorySecrets::default());
        assert!(store_or_refusing(Some(installed)).set(KEY_A, "v").is_ok());
    }

    /// The only test that touches the process-wide slot.
    #[test]
    fn install_keeps_the_first_store() {
        let dir = tempfile::tempdir().unwrap();
        let cipher: Arc<dyn KeyWrap> = Arc::new(FakeKeystore::default());
        assert!(installed_or_refusing().get(KEY_A).is_err());
        assert!(install(dir.path(), cipher.clone()));
        assert!(!install(dir.path(), cipher));
        let s = installed_or_refusing();
        s.set(KEY_A, SECRET).unwrap();
        assert_eq!(s.get(KEY_A).unwrap().as_deref(), Some(SECRET));
    }
}
