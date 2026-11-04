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
//! - A read that failed is remembered for `FAILURE_BACKOFF`, so a status poll
//!   does not go back to a slow or broken provider every second. A write, a
//!   delete or a reset clears it. Nothing is ever downgraded to plaintext.
//! - A key that is permanently unusable (invalidated, unrecoverable) is
//!   replaced by `reset_if_key_unusable`: the ciphertext it wrapped is deleted,
//!   the key is regenerated, and the caller learns which keys were lost. A
//!   transient failure never deletes anything.
//! - The reset is restartable. Before it deletes anything it writes a
//!   pending-recovery record (the lost machine keys, no secrets) next to the
//!   ciphertext. The record is cleared only by `acknowledge_recovery`, which
//!   the shell calls after the user has been shown the notice. A crash or a
//!   failure at any step leaves the record, and the next start finishes the
//!   job and tells the user.
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

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

/// How long a failed read is remembered before the cipher is asked again.
pub const FAILURE_BACKOFF: Duration = Duration::from_secs(5);

/// Why a cipher call failed. No detail travels with it: the Java side's
/// messages could name a key alias or a provider, and nothing may reach a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapError {
    /// The platform key store cannot be used now (not initialised, no key).
    Unavailable,
    /// The cipher refused: wrong key, wrong AAD, damaged blob.
    Failed,
}

/// What the platform says about the wrapping key itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    /// Usable, or not created yet (the first `wrap` creates it).
    Usable,
    /// Gone for good: invalidated, or unrecoverable. No retry will help.
    PermanentlyUnusable,
    /// Could not tell (provider busy or failing). Retry later; delete nothing.
    Unknown,
}

/// Authenticated encryption under a platform-held key. `aad` is bound into the
/// blob: `unwrap` must fail unless the same `aad` is given.
pub trait KeyWrap: Send + Sync {
    fn wrap(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, WrapError>;
    fn unwrap(&self, aad: &[u8], blob: &[u8]) -> Result<Vec<u8>, WrapError>;
    /// Whether the key can ever be used again. Unknown unless overridden.
    fn key_state(&self) -> KeyState {
        KeyState::Unknown
    }
    /// Deletes the key and creates a fresh one. Only called after
    /// `key_state` said `PermanentlyUnusable`.
    fn reset_key(&self) -> Result<(), WrapError> {
        Err(WrapError::Unavailable)
    }
}

/// What `reset_if_key_unusable` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResetOutcome {
    /// The key is fine (or the provider could not say): nothing was touched.
    NotNeeded,
    /// The key was permanently unusable. It was replaced and these secret
    /// keys (machine keys) had their unreadable ciphertext deleted.
    Reset { lost: Vec<String> },
    /// An earlier reset got as far as replacing the key but the user was never
    /// told (the app was closed or failed first). This call deleted nothing;
    /// the caller shows the notice and then calls `acknowledge_recovery`.
    Pending { lost: Vec<String> },
}

/// The explicit secure-store reset, as the bridge sees it.
pub trait SecureStoreReset: Send + Sync {
    fn reset_if_key_unusable(&self) -> Result<ResetOutcome, SecretError>;
    /// Clears the pending-recovery record. Call it only after the user has
    /// been shown which machines were lost.
    fn acknowledge_recovery(&self) -> Result<(), SecretError>;
}

pub struct WrappedFileSecrets<W> {
    dir: PathBuf,
    cipher: W,
    now: Box<dyn Fn() -> Instant + Send + Sync>,
    failed: Mutex<HashMap<String, (Instant, String)>>,
}

impl<W: KeyWrap> WrappedFileSecrets<W> {
    /// `dir` is app-private storage. It is created on the first write.
    pub fn new(dir: impl Into<PathBuf>, cipher: W) -> Self {
        Self::with_clock(dir, cipher, Instant::now)
    }

    /// As `new`, with the clock the failure backoff reads (tests control it).
    pub fn with_clock(
        dir: impl Into<PathBuf>,
        cipher: W,
        now: impl Fn() -> Instant + Send + Sync + 'static,
    ) -> Self {
        Self { dir: dir.into(), cipher, now: Box::new(now), failed: Mutex::new(HashMap::new()) }
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
        let now = (self.now)();
        {
            let mut failed = self.failed.lock().unwrap_or_else(|e| e.into_inner());
            match failed.get(key) {
                Some((at, error)) if now.saturating_duration_since(*at) < FAILURE_BACKOFF => {
                    return Err(SecretError(error.clone()));
                }
                Some(_) => {
                    failed.remove(key);
                }
                None => {}
            }
        }
        let result = self.read(key, &path);
        if let Err(error) = &result {
            self.failed.lock().unwrap_or_else(|e| e.into_inner()).insert(key.into(), (now, error.0.clone()));
        }
        result
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        self.forget_failure(key);
        self.write(key, value)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.forget_failure(key);
        self.remove(key)
    }
}

impl<W: KeyWrap> WrappedFileSecrets<W> {
    fn forget_failure(&self, key: &str) {
        self.failed.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
    }

    fn read(&self, key: &str, path: &Path) -> Result<Option<String>, SecretError> {
        let bytes = match fs::read(path) {
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

    fn write(&self, key: &str, value: &str) -> Result<(), SecretError> {
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

    fn remove(&self, key: &str) -> Result<(), SecretError> {
        match fs::remove_file(self.path(key)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(SECRET_NOT_STORED, &error)),
        }
    }
}

/// The pending-recovery record: one hex-encoded machine key per line. It sits
/// in the secrets directory, outside the `s-` namespace, so the sweep that
/// deletes ciphertext never touches it. It holds no secret, only the keys.
const RECOVERY_RECORD: &str = "recovery-pending";

impl<W: KeyWrap> WrappedFileSecrets<W> {
    fn record_path(&self) -> PathBuf {
        self.dir.join(RECOVERY_RECORD)
    }

    /// `None`: no recovery is pending. An unreadable record is an error
    /// (fail closed), never "nothing pending".
    fn read_record(&self) -> Result<Option<Vec<String>>, SecretError> {
        match fs::read(self.record_path()) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                Ok(Some(text.lines().filter_map(unhex).collect()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error(SECRET_UNREADABLE, &error)),
        }
    }

    fn write_record(&self, lost: &[String]) -> Result<(), SecretError> {
        let mut text = String::new();
        for key in lost {
            for byte in key.as_bytes() {
                text.push_str(&format!("{byte:02x}"));
            }
            text.push('\n');
        }
        fs::create_dir_all(&self.dir).map_err(|e| io_error(SECRET_NOT_STORED, &e))?;
        write_atomic(&self.record_path(), text.as_bytes()).map_err(|e| io_error(SECRET_NOT_STORED, &e))?;
        sync_dir(&self.dir);
        Ok(())
    }
}

impl<W: KeyWrap> SecureStoreReset for WrappedFileSecrets<W> {
    /// Only a key the platform reports as permanently unusable is replaced; a
    /// transient or unknown state touches nothing. Order, each step safe to
    /// repeat after a crash:
    /// 1. write the pending-recovery record (every key whose ciphertext is
    ///    about to go, plus any already in the record);
    /// 2. delete the ciphertext, so a failure leaves the old key in place;
    /// 3. replace the key.
    /// The record outlives all three. A record with a usable key means an
    /// earlier run finished steps 2 and 3 (or lost the alias after step 2 and
    /// the next write creates one): nothing is deleted again, since the
    /// ciphertext now present belongs to the new key, and the caller is told.
    fn reset_if_key_unusable(&self) -> Result<ResetOutcome, SecretError> {
        let pending = self.read_record()?;
        match (&pending, self.cipher.key_state()) {
            (None, state) if state != KeyState::PermanentlyUnusable => return Ok(ResetOutcome::NotNeeded),
            (Some(lost), KeyState::Usable) => return Ok(ResetOutcome::Pending { lost: lost.clone() }),
            // Cannot tell whether the old ciphertext is still there: touch nothing.
            (Some(_), KeyState::Unknown) => return Err(SecretError(KEYSTORE_UNAVAILABLE.into())),
            _ => {}
        }
        let mut lost = pending.unwrap_or_default();
        let mut files = Vec::new();
        match fs::read_dir(&self.dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|e| io_error(SECRET_NOT_STORED, &e))?;
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with(RECOVERY_RECORD) {
                        continue;
                    }
                    if let Some(hex) = name.strip_prefix("s-")
                        && let Some(key) = unhex(hex)
                        && !lost.contains(&key)
                    {
                        lost.push(key);
                    }
                    files.push(entry.path());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(SECRET_NOT_STORED, &error)),
        }
        lost.sort();
        self.write_record(&lost)?;
        for file in files {
            match fs::remove_file(&file) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error(SECRET_NOT_STORED, &error)),
            }
        }
        self.cipher.reset_key().map_err(|_| SecretError(KEYSTORE_UNAVAILABLE.into()))?;
        self.failed.lock().unwrap_or_else(|e| e.into_inner()).clear();
        Ok(ResetOutcome::Reset { lost })
    }

    fn acknowledge_recovery(&self) -> Result<(), SecretError> {
        match fs::remove_file(self.record_path()) {
            Ok(()) => {
                sync_dir(&self.dir);
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(SECRET_NOT_STORED, &error)),
        }
    }
}

/// Makes a rename or a delete in `dir` durable. Best effort: not every
/// platform can open a directory.
fn sync_dir(dir: &Path) {
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

fn unhex(hex: &str) -> Option<String> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
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

/// Lets the cached store and the reset share one file store.
struct Shared<W>(Arc<WrappedFileSecrets<W>>);

impl<W: KeyWrap> SecretStore for Shared<W> {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        self.0.get(key)
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        self.0.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.0.delete(key)
    }
}

static INSTALLED: OnceLock<Arc<dyn SecretStore>> = OnceLock::new();
static INSTALLED_RESET: OnceLock<Arc<dyn SecureStoreReset>> = OnceLock::new();

/// Called once by the Android shell, with the app's private directory and the
/// Keystore cipher. Reads are cached per process, as the Keychain store's are.
/// Returns `false` if a store was already installed (the first one stays).
pub fn install(dir: impl Into<PathBuf>, cipher: Arc<dyn KeyWrap>) -> bool {
    let files = Arc::new(WrappedFileSecrets::new(dir, cipher));
    let store: Arc<dyn SecretStore> = Arc::new(CachedSecrets::new(Shared(files.clone())));
    let first = INSTALLED.set(store).is_ok();
    if first {
        let _ = INSTALLED_RESET.set(files);
    }
    first
}

/// The reset for the installed store, if one is installed.
pub fn installed_reset() -> Option<Arc<dyn SecureStoreReset>> {
    INSTALLED_RESET.get().cloned()
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
    fn key_state(&self) -> KeyState {
        (**self).key_state()
    }
    fn reset_key(&self) -> Result<(), WrapError> {
        (**self).reset_key()
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
        /// Simulates a key the platform has invalidated for good.
        invalidated: AtomicBool,
        unwraps: std::sync::atomic::AtomicUsize,
        resets: std::sync::atomic::AtomicUsize,
        /// `reset_key` fails and leaves the dead key as it was.
        reset_fails: AtomicBool,
        /// `reset_key` deletes the alias, then fails to generate a new key:
        /// the platform reports "no key yet", which reads as usable.
        reset_loses_alias: AtomicBool,
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
            if self.unavailable.load(Ordering::SeqCst) || self.invalidated.load(Ordering::SeqCst) {
                return Err(WrapError::Unavailable);
            }
            let mut blob = tag(aad).to_vec();
            blob.extend(plaintext.iter().map(|b| b ^ 0x5a));
            Ok(blob)
        }
        fn unwrap(&self, aad: &[u8], blob: &[u8]) -> Result<Vec<u8>, WrapError> {
            self.unwraps.fetch_add(1, Ordering::SeqCst);
            if self.unavailable.load(Ordering::SeqCst) || self.invalidated.load(Ordering::SeqCst) {
                return Err(WrapError::Unavailable);
            }
            if blob.len() < 8 || blob[..8] != tag(aad) {
                return Err(WrapError::Failed);
            }
            Ok(blob[8..].iter().map(|b| b ^ 0x5a).collect())
        }
        fn key_state(&self) -> KeyState {
            if self.invalidated.load(Ordering::SeqCst) {
                KeyState::PermanentlyUnusable
            } else if self.unavailable.load(Ordering::SeqCst) {
                KeyState::Unknown
            } else {
                KeyState::Usable
            }
        }
        fn reset_key(&self) -> Result<(), WrapError> {
            self.resets.fetch_add(1, Ordering::SeqCst);
            if self.reset_fails.load(Ordering::SeqCst) {
                return Err(WrapError::Unavailable);
            }
            self.invalidated.store(false, Ordering::SeqCst);
            if self.reset_loses_alias.load(Ordering::SeqCst) {
                return Err(WrapError::Unavailable);
            }
            Ok(())
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

    /// A key the platform invalidated for good: every wrap and unwrap fails,
    /// and without a reset removing the machine does not help, because the
    /// next pairing wraps with the same dead key. The reset deletes all the
    /// ciphertext, replaces the key, names what was lost, and pairing works.
    #[test]
    fn a_permanently_invalid_key_is_replaced_and_the_lost_secrets_are_named() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.set(KEY_A, SECRET).unwrap();
        s.set(KEY_B, SECRET).unwrap();
        s.cipher.invalidated.store(true, Ordering::SeqCst);
        assert!(s.get(KEY_A).is_err());
        s.delete(KEY_A).unwrap();
        assert!(s.set(KEY_A, SECRET).is_err(), "the dead key must not wrap");
        assert_eq!(
            s.reset_if_key_unusable().unwrap(),
            ResetOutcome::Reset { lost: vec![KEY_B.to_owned()] }
        );
        assert_eq!(s.cipher.resets.load(Ordering::SeqCst), 1);
        assert_eq!(secret_files(dir.path()), 0, "ciphertext left behind");
        s.acknowledge_recovery().unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0, "the record outlived the acknowledgement");
        s.set(KEY_A, SECRET).unwrap();
        assert_eq!(s.get(KEY_A).unwrap().as_deref(), Some(SECRET));
        assert_eq!(s.get(KEY_B).unwrap(), None);
        // Nothing to do the second time.
        assert_eq!(s.reset_if_key_unusable().unwrap(), ResetOutcome::NotNeeded);
        assert_eq!(s.cipher.resets.load(Ordering::SeqCst), 1);
    }

    /// A provider that is only failing now (state unknown) never costs the
    /// user a pairing: nothing is deleted, the key is not reset.
    #[test]
    fn a_transient_failure_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.set(KEY_A, SECRET).unwrap();
        s.cipher.unavailable.store(true, Ordering::SeqCst);
        assert_eq!(s.reset_if_key_unusable().unwrap(), ResetOutcome::NotNeeded);
        assert_eq!(s.cipher.resets.load(Ordering::SeqCst), 0);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        s.cipher.unavailable.store(false, Ordering::SeqCst);
        assert_eq!(s.get(KEY_A).unwrap().as_deref(), Some(SECRET));
    }

    /// The ciphertext is deleted before the key is replaced, and a file that
    /// cannot be deleted stops the reset with the key untouched.
    #[test]
    fn a_reset_that_cannot_delete_the_ciphertext_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.set(KEY_A, SECRET).unwrap();
        // A directory named like a secret file cannot be removed with remove_file.
        fs::create_dir(dir.path().join("s-00")).unwrap();
        s.cipher.invalidated.store(true, Ordering::SeqCst);
        assert!(s.reset_if_key_unusable().is_err());
        assert_eq!(s.cipher.resets.load(Ordering::SeqCst), 0);
        assert!(s.cipher.invalidated.load(Ordering::SeqCst));
    }

    fn secret_files(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("s-"))
            .count()
    }

    fn record_text(dir: &Path) -> Option<String> {
        fs::read_to_string(dir.join(RECOVERY_RECORD)).ok()
    }

    /// The record is on disk before the first ciphertext file goes. A file that
    /// cannot be deleted stops the sweep with the record already written, so a
    /// crash at that point still tells the user what was lost. Without the
    /// write-first order the record would not exist here.
    #[test]
    fn the_record_is_written_before_anything_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.set(KEY_A, SECRET).unwrap();
        s.set(KEY_B, SECRET).unwrap();
        fs::create_dir(dir.path().join("s-00")).unwrap(); // cannot be removed as a file
        s.cipher.invalidated.store(true, Ordering::SeqCst);
        assert!(s.reset_if_key_unusable().is_err());
        assert_eq!(s.cipher.resets.load(Ordering::SeqCst), 0, "the key must not be replaced");
        let record = record_text(dir.path()).expect("no record before the deletes");
        for key in [KEY_A, KEY_B] {
            let hex: String = key.bytes().map(|b| format!("{b:02x}")).collect();
            assert!(record.lines().any(|l| l == hex), "{key} missing from the record");
        }
        assert!(!record.contains("TOKEN-VALUE") && !record.contains(SECRET));
        // The fault clears; the next run finishes and still names both.
        fs::remove_dir(dir.path().join("s-00")).unwrap();
        let mut lost = match s.reset_if_key_unusable().unwrap() {
            ResetOutcome::Reset { lost } => lost,
            other => panic!("{other:?}"),
        };
        lost.sort();
        // The decoy directory `s-00` decodes to the key "\0".
        let mut want = vec!["\0".to_owned(), KEY_A.to_owned(), KEY_B.to_owned()];
        want.sort();
        assert_eq!(lost, want);
        assert_eq!(secret_files(dir.path()), 0);
    }

    /// The alias is deleted and generating the replacement fails. The platform
    /// now reports "no key" (usable), so without the record the next start
    /// sees nothing wrong and never says which machines need pairing again.
    /// With it, the next start (a new store over the same directory) is told.
    #[test]
    fn a_failure_after_the_alias_was_deleted_is_reported_on_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let first = store(dir.path());
        first.set(KEY_A, SECRET).unwrap();
        first.cipher.invalidated.store(true, Ordering::SeqCst);
        first.cipher.reset_loses_alias.store(true, Ordering::SeqCst);
        assert!(first.reset_if_key_unusable().is_err());
        assert_eq!(first.cipher.key_state(), KeyState::Usable, "the fake must model a missing alias");
        assert_eq!(secret_files(dir.path()), 0);
        drop(first);

        let second = store(dir.path());
        assert_eq!(
            second.reset_if_key_unusable().unwrap(),
            ResetOutcome::Pending { lost: vec![KEY_A.to_owned()] }
        );
        assert_eq!(second.cipher.resets.load(Ordering::SeqCst), 0, "nothing is reset a second time");
        // Told again until it is acknowledged, then never.
        assert_eq!(
            second.reset_if_key_unusable().unwrap(),
            ResetOutcome::Pending { lost: vec![KEY_A.to_owned()] }
        );
        second.acknowledge_recovery().unwrap();
        assert_eq!(second.reset_if_key_unusable().unwrap(), ResetOutcome::NotNeeded);
        assert!(record_text(dir.path()).is_none());
    }

    /// A crash after the ciphertext went but before the key was replaced: the
    /// key is still dead, the files are gone, and the names come from the
    /// record alone.
    #[test]
    fn a_crash_between_the_delete_and_the_key_replacement_is_finished_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let first = store(dir.path());
        first.set(KEY_A, SECRET).unwrap();
        first.cipher.invalidated.store(true, Ordering::SeqCst);
        first.cipher.reset_fails.store(true, Ordering::SeqCst);
        assert!(first.reset_if_key_unusable().is_err());
        assert_eq!(secret_files(dir.path()), 0);
        assert!(first.cipher.invalidated.load(Ordering::SeqCst));
        drop(first);

        let second = store(dir.path());
        second.cipher.invalidated.store(true, Ordering::SeqCst);
        assert_eq!(
            second.reset_if_key_unusable().unwrap(),
            ResetOutcome::Reset { lost: vec![KEY_A.to_owned()] }
        );
        assert!(!second.cipher.invalidated.load(Ordering::SeqCst));
    }

    /// Fresh ciphertext written under the new key after the reset must survive
    /// a restart that finds the record still pending: it is told, not swept.
    #[test]
    fn a_pending_record_never_deletes_ciphertext_written_under_the_new_key() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.set(KEY_A, SECRET).unwrap();
        s.cipher.invalidated.store(true, Ordering::SeqCst);
        assert!(matches!(s.reset_if_key_unusable().unwrap(), ResetOutcome::Reset { .. }));
        s.set(KEY_B, SECRET).unwrap(); // paired again, record not yet acknowledged
        let restarted = store(dir.path());
        assert!(matches!(restarted.reset_if_key_unusable().unwrap(), ResetOutcome::Pending { .. }));
        assert_eq!(secret_files(dir.path()), 1, "the fresh secret was swept");
        assert_eq!(s.get(KEY_B).unwrap().as_deref(), Some(SECRET));
    }

    /// A pending record and a provider that cannot answer: unknown means
    /// retry later. Nothing is deleted and the record stays.
    #[test]
    fn a_pending_record_with_an_unknown_key_state_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.set(KEY_A, SECRET).unwrap();
        s.cipher.invalidated.store(true, Ordering::SeqCst);
        s.cipher.reset_fails.store(true, Ordering::SeqCst);
        assert!(s.reset_if_key_unusable().is_err());
        s.cipher.invalidated.store(false, Ordering::SeqCst);
        s.cipher.unavailable.store(true, Ordering::SeqCst);
        assert!(s.reset_if_key_unusable().is_err());
        assert!(record_text(dir.path()).is_some());
        assert_eq!(s.cipher.resets.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn nothing_is_recorded_when_no_reset_is_needed() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.set(KEY_A, SECRET).unwrap();
        assert_eq!(s.reset_if_key_unusable().unwrap(), ResetOutcome::NotNeeded);
        assert!(record_text(dir.path()).is_none());
        s.acknowledge_recovery().unwrap(); // nothing to clear is fine
    }

    /// A failed read is remembered for FAILURE_BACKOFF so a poll does not go
    /// back to the provider every second; after that it is asked again. The
    /// clock is controlled: no sleep, no wall-clock bound.
    #[test]
    fn a_failed_read_is_not_retried_inside_the_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let base = Instant::now();
        let offset = Arc::new(Mutex::new(Duration::ZERO));
        let clock = offset.clone();
        let s = WrappedFileSecrets::with_clock(dir.path(), FakeKeystore::default(), move || {
            base + *clock.lock().unwrap()
        });
        s.set(KEY_A, SECRET).unwrap();
        s.cipher.unavailable.store(true, Ordering::SeqCst);
        for _ in 0..5 {
            assert_eq!(s.get(KEY_A).unwrap_err().0, KEYSTORE_UNAVAILABLE);
        }
        assert_eq!(s.cipher.unwraps.load(Ordering::SeqCst), 1, "the provider was asked again inside the backoff");
        *offset.lock().unwrap() = FAILURE_BACKOFF - Duration::from_millis(1);
        assert!(s.get(KEY_A).is_err());
        assert_eq!(s.cipher.unwraps.load(Ordering::SeqCst), 1);
        // Recovered provider, but still inside the remembered failure: closed.
        s.cipher.unavailable.store(false, Ordering::SeqCst);
        assert!(s.get(KEY_A).is_err());
        *offset.lock().unwrap() = FAILURE_BACKOFF;
        assert_eq!(s.get(KEY_A).unwrap().as_deref(), Some(SECRET));
        assert_eq!(s.cipher.unwraps.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_write_clears_a_remembered_failure() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.set(KEY_A, SECRET).unwrap();
        s.cipher.unavailable.store(true, Ordering::SeqCst);
        assert!(s.get(KEY_A).is_err());
        s.cipher.unavailable.store(false, Ordering::SeqCst);
        s.set(KEY_A, "fresh").unwrap();
        assert_eq!(s.get(KEY_A).unwrap().as_deref(), Some("fresh"));
    }
}
