//! The inbox the activity talks to: opened once per process, then asked for the
//! screen model and told what the user did. Everything crosses as strings, so
//! the JNI layer stays thin and this file is tested on the host.

use std::path::Path;
use std::sync::{Arc, Mutex};

use acd_app_core::wrapped::{ResetOutcome, SecureStoreReset};
use acd_app_core::{commands as core, pairing, setup};
use acd_client::{
    inbox::Inbox,
    settings::{SecretStatus, SecretStore},
};
use serde::Serialize;

use crate::model;

/// Shown when the secure store could not be reset. Nothing was stored.
pub const RESET_FAILED: &str = "The phone's secure storage could not be reset. Try again later.";

pub struct Bridge {
    inbox: Arc<Inbox>,
    secrets: Arc<dyn SecretStore>,
    reset: Option<Arc<dyn SecureStoreReset>>,
    /// What the user must be told after the secure store was reset.
    notice: Mutex<Option<String>>,
    // Held so the workers the inbox spawned keep running.
    runtime: tokio::runtime::Runtime,
}

/// The answer to every call that can fail: `{"ok": "..."}` or `{"error": "..."}`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Reply {
    Ok(String),
    Error(String),
}

pub fn reply(result: Result<String, String>) -> String {
    let reply = match result {
        Ok(text) => Reply::Ok(text),
        Err(text) => Reply::Error(text),
    };
    serde_json::to_string(&reply).unwrap_or_else(|_| r#"{"error":"internal error"}"#.into())
}

impl Bridge {
    /// Opens the inbox over `files_dir` (the app's private directory). The
    /// `local` machine is never offered: there is no acd on the phone.
    ///
    /// `reset` is the explicit secure-store reset. If the wrapping key is
    /// permanently unusable, it runs before anything reads a secret.
    pub fn open(
        files_dir: &Path,
        secrets: Arc<dyn SecretStore>,
        reset: Option<Arc<dyn SecureStoreReset>>,
    ) -> Result<Bridge, String> {
        let lost = match &reset {
            Some(reset) => match reset.reset_if_key_unusable() {
                Ok(ResetOutcome::Reset { lost }) => Some(lost),
                Ok(ResetOutcome::NotNeeded) => None,
                Err(_) => return Err(RESET_FAILED.to_owned()),
            },
            None => None,
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| format!("could not start the runtime ({:?})", e.kind()))?;
        let inbox = setup::open_inbox(
            &files_dir.join("settings.json"),
            secrets.clone(),
            runtime.handle(),
            setup::OpenOptions {
                client_name: "agent-caller-android",
                client_version: env!("CARGO_PKG_VERSION"),
                local_machine: false,
            },
        )
        .map_err(|e| e.to_string())?;
        let bridge = Bridge { inbox, secrets, reset, notice: Mutex::new(None), runtime };
        if let Some(lost) = lost {
            bridge.announce_reset(&lost);
        }
        Ok(bridge)
    }

    /// Tells the user, in plain words, which machines lost their pairing.
    fn announce_reset(&self, lost: &[String]) {
        let names: Vec<String> = self
            .inbox
            .machines()
            .into_iter()
            .filter(|m| lost.contains(&m.key))
            .map(|m| m.name)
            .collect();
        let text = if names.is_empty() {
            "The phone's secure key was lost and has been replaced. No saved pairing was affected.".to_owned()
        } else {
            format!(
                "The phone's secure key was lost and has been replaced. These machines must be paired again: {}.",
                names.join(", ")
            )
        };
        *self.notice.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
    }

    /// Replaces a permanently unusable key. Returns true only if it did; a
    /// transient failure (or no reset installed) deletes nothing.
    fn recover(&self) -> bool {
        let Some(reset) = &self.reset else { return false };
        match reset.reset_if_key_unusable() {
            Ok(ResetOutcome::Reset { lost }) => {
                // Drop what the cached store still holds for them.
                for key in &lost {
                    let _ = self.secrets.delete(key);
                }
                self.announce_reset(&lost);
                true
            }
            Ok(ResetOutcome::NotNeeded) => false,
            Err(_) => {
                *self.notice.lock().unwrap_or_else(|e| e.into_inner()) = Some(RESET_FAILED.to_owned());
                false
            }
        }
    }

    /// The screen as JSON (`model::ScreenModel`).
    pub fn view_json(&self) -> String {
        let secrets = self.inbox.machines().into_iter().map(|m| (m.key, m.secret)).collect();
        let mut screen = model::screen(&core::get_view(&self.inbox), &secrets);
        screen.notice = self.notice.lock().unwrap_or_else(|e| e.into_inner()).clone();
        serde_json::to_string(&screen).unwrap_or_else(|_| "{}".into())
    }

    /// Pairs from the text of an `acd pair` line, pasted or decoded from a QR
    /// code. Returns the machine's name. The error never echoes the text.
    pub fn import_pairing(&self, text: &str) -> Result<String, String> {
        let import = || {
            pairing::import_text(&self.inbox, self.runtime.handle(), Some(text.trim().to_owned()))
                .map(|machine| machine.name)
        };
        match import() {
            Ok(name) => Ok(name),
            // A dead key fails every wrap, so pairing again would fail for
            // good. Replace it (never on a transient failure) and try once.
            Err(first) => {
                if self.recover() {
                    import()
                } else {
                    Err(first)
                }
            }
        }
    }

    pub fn pick(&self, card_key: &str, request_id: &str, option_id: &str, revision: u64) -> Result<String, String> {
        core::pick_option(&self.inbox, card_key, request_id, option_id, revision)
    }

    pub fn answer_text(&self, card_key: &str, request_id: &str, revision: u64, text: String) -> Result<String, String> {
        core::answer_text(&self.inbox, card_key, request_id, revision, text)
    }

    pub fn cancel(&self, action_id: &str) -> bool {
        core::cancel_action(&self.inbox, action_id)
    }

    pub fn mark_seen(&self, card_key: &str) -> Result<(), String> {
        core::mark_seen(&self.inbox, self.runtime.handle(), card_key)
    }

    /// Removes a machine. `Inbox::remove_machine` and a re-pairing both read
    /// the stored secret first and refuse if that fails, so a machine whose
    /// secret cannot be opened (a damaged file, a Keystore key that is gone)
    /// could be neither removed nor paired again. For that one case the
    /// secret, which is unusable anyway, is deleted first, on the user's
    /// explicit Remove. `delete` removes the file and never calls the cipher.
    pub fn remove_machine(&self, key: &str) -> Result<(), String> {
        let unusable = self
            .inbox
            .machines()
            .iter()
            .any(|m| m.key == key && m.secret == SecretStatus::Unreadable);
        if unusable {
            self.secrets.delete(key).map_err(|e| e.to_string())?;
        }
        core::remove_machine(&self.inbox, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acd_app_core::wrapped::{KeyState, KeyWrap, SecureStoreReset, WrapError, WrappedFileSecrets};
    use acd_client::settings::MemorySecrets;
    use std::sync::atomic::{AtomicBool, Ordering};

    const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    fn line() -> String {
        let pin = acd_protocol::pairing::spki_pin(b"test certificate");
        format!("acd://example.test:47800?pin={pin}#{TOKEN}")
    }

    /// Stands in for the Keystore: XOR. `.0` switches it off (a transient
    /// failure); `.1` invalidates the key for good until `reset_key`.
    #[derive(Clone, Default)]
    struct Fake(Arc<AtomicBool>, Arc<AtomicBool>);
    impl KeyWrap for Fake {
        fn key_state(&self) -> KeyState {
            if self.1.load(Ordering::SeqCst) {
                KeyState::PermanentlyUnusable
            } else if self.0.load(Ordering::SeqCst) {
                KeyState::Unknown
            } else {
                KeyState::Usable
            }
        }
        fn reset_key(&self) -> Result<(), WrapError> {
            self.1.store(false, Ordering::SeqCst);
            Ok(())
        }
        fn wrap(&self, aad: &[u8], p: &[u8]) -> Result<Vec<u8>, WrapError> {
            if self.0.load(Ordering::SeqCst) || self.1.load(Ordering::SeqCst) {
                return Err(WrapError::Unavailable);
            }
            let mut blob = vec![aad.len() as u8];
            blob.extend(p.iter().map(|b| b ^ 0x5a));
            Ok(blob)
        }
        fn unwrap(&self, _aad: &[u8], blob: &[u8]) -> Result<Vec<u8>, WrapError> {
            if self.0.load(Ordering::SeqCst) || self.1.load(Ordering::SeqCst) {
                return Err(WrapError::Unavailable);
            }
            Ok(blob[1..].iter().map(|b| b ^ 0x5a).collect())
        }
    }

    fn secrets(dir: &Path, fake: &Fake) -> Arc<dyn SecretStore> {
        Arc::new(WrappedFileSecrets::new(dir.join("secrets"), fake.clone()))
    }

    /// The store and its reset, as the Android shell installs them.
    fn secrets_and_reset(dir: &Path, fake: &Fake) -> (Arc<dyn SecretStore>, Arc<dyn SecureStoreReset>) {
        let files = Arc::new(WrappedFileSecrets::new(dir.join("secrets"), fake.clone()));
        struct Shared(Arc<WrappedFileSecrets<Fake>>);
        impl SecretStore for Shared {
            fn get(&self, k: &str) -> Result<Option<String>, acd_client::settings::SecretError> {
                self.0.get(k)
            }
            fn set(&self, k: &str, v: &str) -> Result<(), acd_client::settings::SecretError> {
                self.0.set(k, v)
            }
            fn delete(&self, k: &str) -> Result<(), acd_client::settings::SecretError> {
                self.0.delete(k)
            }
        }
        (Arc::new(Shared(files.clone())), files)
    }

    fn open_with_reset(dir: &Path, fake: &Fake) -> Bridge {
        let (store, reset) = secrets_and_reset(dir, fake);
        Bridge::open(dir, store, Some(reset)).unwrap()
    }

    fn secret_files(dir: &Path) -> usize {
        let d = dir.join("secrets");
        if d.exists() { walk(&d).len() } else { 0 }
    }

    /// The key is invalidated while the app is closed. Without the reset the
    /// alias stays dead: removing the machine deletes the ciphertext, but
    /// pairing again wraps with the same dead key and fails for good. With it
    /// the first open replaces the key, deletes what it wrapped, names the
    /// machines to pair again, and pairing works.
    #[test]
    fn an_invalidated_key_is_replaced_at_start_and_the_user_is_told_which_machines() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let first = open_with_reset(dir.path(), &fake);
        let name = first.import_pairing(&line()).unwrap();
        assert_eq!(secret_files(dir.path()), 1);
        drop(first);
        fake.1.store(true, Ordering::SeqCst);

        let second = open_with_reset(dir.path(), &fake);
        assert!(!fake.1.load(Ordering::SeqCst), "the key was not replaced");
        assert_eq!(secret_files(dir.path()), 0, "ciphertext wrapped by the dead key was kept");
        let view = view_with_machines(&second, 1);
        let notice = view["notice"].as_str().expect("the user is not told");
        assert!(notice.contains(&name) && notice.contains("paired again"), "{notice}");
        // The machine is not offered as working, and pairing it again works.
        assert_ne!(view["machines"][0]["status"], "online", "{view}");
        assert!(second.import_pairing(&line()).is_ok(), "re-pairing still fails");
        assert_eq!(secret_files(dir.path()), 1);
    }

    /// The key dies while the app runs: the pairing that fails because of it
    /// replaces the key and goes through once, and the user is told.
    #[test]
    fn a_key_invalidated_while_running_is_replaced_by_the_next_pairing() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let bridge = open_with_reset(dir.path(), &fake);
        fake.1.store(true, Ordering::SeqCst);
        assert!(bridge.import_pairing(&line()).is_ok());
        assert!(!fake.1.load(Ordering::SeqCst));
        assert!(bridge.view_json().contains("secure key was lost"));
        assert_eq!(secret_files(dir.path()), 1);
    }

    /// A failure that may pass (provider busy, device locked) never deletes a
    /// pairing and never resets the key: retry later.
    #[test]
    fn a_transient_keystore_failure_deletes_nothing_and_says_nothing_was_lost() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let first = open_with_reset(dir.path(), &fake);
        first.import_pairing(&line()).unwrap();
        drop(first);
        fake.0.store(true, Ordering::SeqCst);
        let second = open_with_reset(dir.path(), &fake);
        assert!(second.import_pairing(&line()).is_err());
        assert_eq!(secret_files(dir.path()), 1, "a transient failure deleted a secret");
        assert!(!second.view_json().contains("secure key was lost"));
        fake.0.store(false, Ordering::SeqCst);
        let third = open_with_reset(dir.path(), &fake);
        let view = view_with_machines(&third, 1);
        assert_eq!(view["notice"], serde_json::Value::Null, "{view}");
    }

    #[test]
    fn a_pasted_line_pairs_a_machine_that_shows_up_in_the_view() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = Bridge::open(dir.path(), Arc::new(MemorySecrets::default()), None).unwrap();
        // Surrounding whitespace (a paste, a decoded QR) is fine.
        let name = bridge.import_pairing(&format!("  {}\n", line())).unwrap();
        let view = view_with_machines(&bridge, 1);
        let machines = view["machines"].as_array().unwrap();
        assert_eq!(machines.len(), 1, "{view}");
        assert_eq!(machines[0]["name"], name);
        assert!(view["cards"].as_array().unwrap().is_empty());
        // No local machine is offered.
        assert!(!bridge.view_json().contains("\"local\""));
    }

    #[test]
    fn a_bad_line_is_refused_and_the_error_never_echoes_it() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = Bridge::open(dir.path(), Arc::new(MemorySecrets::default()), None).unwrap();
        for text in ["", "   ", "hello #tok", "acd://192.168.1.10:47800?pin=x#SECRETSECRET"] {
            let err = bridge.import_pairing(text).unwrap_err();
            assert!(!err.contains("SECRETSECRET") && !err.contains("tok"), "{err}");
        }
        assert_eq!(bridge.view_json().matches("\"key\"").count(), 0);
    }

    #[test]
    fn a_pairing_survives_a_restart_through_the_wrapped_store() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let first = Bridge::open(dir.path(), secrets(dir.path(), &fake), None).unwrap();
        first.import_pairing(&line()).unwrap();
        // The token is in no plain file.
        for entry in walk(dir.path()) {
            let bytes = std::fs::read(&entry).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains(TOKEN), "{entry:?}");
        }
        drop(first);
        let second = Bridge::open(dir.path(), secrets(dir.path(), &fake), None).unwrap();
        let view = view_with_machines(&second, 1);
        assert_eq!(view["machines"].as_array().unwrap().len(), 1, "{view}");
        // Not "Pair this machine": the secret was read back.
        assert!(!second.view_json().contains("Pair this machine"));
    }

    #[test]
    fn a_machine_whose_secret_cannot_be_opened_says_so_and_never_reads_as_not_paired() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let first = Bridge::open(dir.path(), secrets(dir.path(), &fake), None).unwrap();
        first.import_pairing(&line()).unwrap();
        drop(first);
        fake.0.store(true, Ordering::SeqCst);
        let second = Bridge::open(dir.path(), secrets(dir.path(), &fake), None).unwrap();
        let view = view_with_machines(&second, 1);
        let machine = &view["machines"][0];
        assert_eq!(machine["status"], "attention", "{view}");
        assert_eq!(machine["problem"], model::SECRET_UNUSABLE, "{view}");
        assert!(!view.to_string().contains("Pair this machine"));
    }

    /// The message for an unusable secret tells the user to remove the machine
    /// and pair it again. This is that path, for a damaged file and for an
    /// unavailable Keystore.
    #[test]
    fn the_recovery_the_message_promises_works() {
        for damage in ["file", "keystore"] {
            let dir = tempfile::tempdir().unwrap();
            let fake = Fake::default();
            let first = Bridge::open(dir.path(), secrets(dir.path(), &fake), None).unwrap();
            first.import_pairing(&line()).unwrap();
            let key = view_with_machines(&first, 1)["machines"][0]["key"].as_str().unwrap().to_owned();
            drop(first);
            if damage == "file" {
                let file = walk(&dir.path().join("secrets")).pop().unwrap();
                std::fs::write(file, b"damaged").unwrap();
            } else {
                fake.0.store(true, Ordering::SeqCst);
            }
            let second = Bridge::open(dir.path(), secrets(dir.path(), &fake), None).unwrap();
            let view = view_with_machines(&second, 1);
            assert_eq!(view["machines"][0]["problem"], model::SECRET_UNUSABLE, "{damage}: {view}");
            let removed = second.remove_machine(&key);
            assert!(removed.is_ok(), "{damage}: the user cannot remove the machine: {removed:?}");
            fake.0.store(false, Ordering::SeqCst);
            assert!(second.import_pairing(&line()).is_ok(), "{damage}: the user cannot pair it again");
        }
    }

    #[test]
    fn with_the_keystore_unavailable_pairing_commits_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        fake.0.store(true, Ordering::SeqCst);
        let bridge = Bridge::open(dir.path(), secrets(dir.path(), &fake), None).unwrap();
        let err = bridge.import_pairing(&line()).unwrap_err();
        assert!(!err.contains(TOKEN), "{err}");
        assert_eq!(bridge.view_json().matches("\"key\"").count(), 0);
        assert!(!dir.path().join("secrets").exists() || walk(&dir.path().join("secrets")).is_empty());
    }

    #[test]
    fn unknown_cards_and_actions_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = Bridge::open(dir.path(), Arc::new(MemorySecrets::default()), None).unwrap();
        assert!(bridge.pick("nope:nope", "r", "allow", 1).is_err());
        assert!(bridge.answer_text("nope:nope", "r", 1, "x".into()).is_err());
        assert!(!bridge.cancel("none"));
        assert!(bridge.remove_machine("missing").is_err());
    }

    #[test]
    fn replies_are_json_with_one_key() {
        assert_eq!(reply(Ok("a".into())), r#"{"ok":"a"}"#);
        assert_eq!(reply(Err("b \"q\"".into())), r#"{"error":"b \"q\""}"#);
    }

    /// The view fills in once the workers have run: poll, as the activity does.
    fn view_with_machines(bridge: &Bridge, n: usize) -> serde_json::Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let view: serde_json::Value = serde_json::from_str(&bridge.view_json()).unwrap();
            if view["machines"].as_array().unwrap().len() == n || std::time::Instant::now() > deadline {
                return view;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out = vec![];
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }
}
