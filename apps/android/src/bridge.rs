//! The inbox the activity talks to: opened once per process, then asked for the
//! screen model and told what the user did. Everything crosses as strings, so
//! the JNI layer stays thin and this file is tested on the host.

use std::path::Path;
use std::sync::Arc;

use acd_app_core::{commands as core, pairing, setup};
use acd_client::{inbox::Inbox, settings::SecretStore};
use serde::Serialize;

use crate::model;

pub struct Bridge {
    inbox: Arc<Inbox>,
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
    pub fn open(files_dir: &Path, secrets: Arc<dyn SecretStore>) -> Result<Bridge, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| format!("could not start the runtime ({:?})", e.kind()))?;
        let inbox = setup::open_inbox(
            &files_dir.join("settings.json"),
            secrets,
            runtime.handle(),
            setup::OpenOptions {
                client_name: "agent-caller-android",
                client_version: env!("CARGO_PKG_VERSION"),
                local_machine: false,
            },
        )
        .map_err(|e| e.to_string())?;
        Ok(Bridge { inbox, runtime })
    }

    /// The screen as JSON (`model::ScreenModel`).
    pub fn view_json(&self) -> String {
        let secrets = self.inbox.machines().into_iter().map(|m| (m.key, m.secret)).collect();
        let screen = model::screen(&core::get_view(&self.inbox), &secrets);
        serde_json::to_string(&screen).unwrap_or_else(|_| "{}".into())
    }

    /// Pairs from the text of an `acd pair` line, pasted or decoded from a QR
    /// code. Returns the machine's name. The error never echoes the text.
    pub fn import_pairing(&self, text: &str) -> Result<String, String> {
        pairing::import_text(&self.inbox, self.runtime.handle(), Some(text.trim().to_owned()))
            .map(|machine| machine.name)
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

    pub fn remove_machine(&self, key: &str) -> Result<(), String> {
        core::remove_machine(&self.inbox, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acd_app_core::wrapped::{KeyWrap, WrapError, WrappedFileSecrets};
    use acd_client::settings::MemorySecrets;
    use std::sync::atomic::{AtomicBool, Ordering};

    const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    fn line() -> String {
        let pin = acd_protocol::pairing::spki_pin(b"test certificate");
        format!("acd://example.test:47800?pin={pin}#{TOKEN}")
    }

    /// Stands in for the Keystore: XOR, and it can be switched off.
    #[derive(Clone, Default)]
    struct Fake(Arc<AtomicBool>);
    impl KeyWrap for Fake {
        fn wrap(&self, aad: &[u8], p: &[u8]) -> Result<Vec<u8>, WrapError> {
            if self.0.load(Ordering::SeqCst) {
                return Err(WrapError::Unavailable);
            }
            let mut blob = vec![aad.len() as u8];
            blob.extend(p.iter().map(|b| b ^ 0x5a));
            Ok(blob)
        }
        fn unwrap(&self, _aad: &[u8], blob: &[u8]) -> Result<Vec<u8>, WrapError> {
            if self.0.load(Ordering::SeqCst) {
                return Err(WrapError::Unavailable);
            }
            Ok(blob[1..].iter().map(|b| b ^ 0x5a).collect())
        }
    }

    fn secrets(dir: &Path, fake: &Fake) -> Arc<dyn SecretStore> {
        Arc::new(WrappedFileSecrets::new(dir.join("secrets"), fake.clone()))
    }

    #[test]
    fn a_pasted_line_pairs_a_machine_that_shows_up_in_the_view() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = Bridge::open(dir.path(), Arc::new(MemorySecrets::default())).unwrap();
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
        let bridge = Bridge::open(dir.path(), Arc::new(MemorySecrets::default())).unwrap();
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
        let first = Bridge::open(dir.path(), secrets(dir.path(), &fake)).unwrap();
        first.import_pairing(&line()).unwrap();
        // The token is in no plain file.
        for entry in walk(dir.path()) {
            let bytes = std::fs::read(&entry).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains(TOKEN), "{entry:?}");
        }
        drop(first);
        let second = Bridge::open(dir.path(), secrets(dir.path(), &fake)).unwrap();
        let view = view_with_machines(&second, 1);
        assert_eq!(view["machines"].as_array().unwrap().len(), 1, "{view}");
        // Not "Pair this machine": the secret was read back.
        assert!(!second.view_json().contains("Pair this machine"));
    }

    #[test]
    fn a_machine_whose_secret_cannot_be_opened_says_so_and_never_reads_as_not_paired() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let first = Bridge::open(dir.path(), secrets(dir.path(), &fake)).unwrap();
        first.import_pairing(&line()).unwrap();
        drop(first);
        fake.0.store(true, Ordering::SeqCst);
        let second = Bridge::open(dir.path(), secrets(dir.path(), &fake)).unwrap();
        let view = view_with_machines(&second, 1);
        let machine = &view["machines"][0];
        assert_eq!(machine["status"], "attention", "{view}");
        assert_eq!(machine["problem"], model::SECRET_UNUSABLE, "{view}");
        assert!(!view.to_string().contains("Pair this machine"));
    }

    #[test]
    fn with_the_keystore_unavailable_pairing_commits_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        fake.0.store(true, Ordering::SeqCst);
        let bridge = Bridge::open(dir.path(), secrets(dir.path(), &fake)).unwrap();
        let err = bridge.import_pairing(&line()).unwrap_err();
        assert!(!err.contains(TOKEN), "{err}");
        assert_eq!(bridge.view_json().matches("\"key\"").count(), 0);
        assert!(!dir.path().join("secrets").exists() || walk(&dir.path().join("secrets")).is_empty());
    }

    #[test]
    fn unknown_cards_and_actions_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = Bridge::open(dir.path(), Arc::new(MemorySecrets::default())).unwrap();
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
