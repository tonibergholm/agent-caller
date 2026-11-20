//! Pairing from clipboard text: the `acd pair` line becomes a remote machine.

use std::sync::{Arc, Mutex};

use acd_client::{
    endpoint,
    inbox::{Inbox, MachineError},
    view::MachineEntryView,
};
use acd_protocol::pairing::Pairing;
use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;

/// What a pairing line says about the machine it would add: no token. Shown
/// to the user before anything is saved (security review F9).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingPreview {
    pub host: String,
    pub port: u16,
    pub pin: String,
}

/// Parses the clipboard text without saving anything. The error text never
/// echoes the clipboard.
pub fn preview_text(clip: Option<&str>) -> Result<PairingPreview, String> {
    let text = clip
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| "The clipboard does not hold an acd pairing line.".to_owned())?;
    let pairing = Pairing::parse(text).map_err(|_| MachineError::BadPairing.to_string())?;
    if endpoint::refuses_literal(&pairing.host) {
        return Err(MachineError::NotTailscale.to_string());
    }
    Ok(PairingPreview { host: pairing.host, port: pairing.port, pin: pairing.pin })
}

/// Digest of the whole pairing line the user was last shown, token included.
/// It lives in Rust state, never in the webview: the preview payload carries no
/// token, so a clipboard that keeps host, port and pin but swaps the token
/// still has to fail the confirm.
#[derive(Default)]
pub struct PendingPairing(Mutex<Option<String>>);

/// `spki_pin` is a plain SHA-256 over the bytes it is given, so it serves as
/// the digest here without a new dependency.
fn line_digest(clip: Option<&str>) -> Option<String> {
    let pairing = Pairing::parse(clip?).ok()?;
    Some(acd_protocol::pairing::spki_pin(pairing.to_line().as_bytes()))
}

impl PendingPairing {
    /// [`preview_text`], remembering the digest of what it showed. A failed
    /// preview forgets the old one.
    pub fn preview(&self, clip: Option<&str>) -> Result<PairingPreview, String> {
        let preview = preview_text(clip);
        *self.0.lock().unwrap() = preview.as_ref().ok().and_then(|_| line_digest(clip));
        preview
    }
}

/// [`import_and_clear`], but only when the clipboard still holds the line the
/// user was shown: the same host, port and pin (`shown`) and the same digest
/// of the whole line (`pending`). A clipboard rewritten after the preview,
/// the token included, imports nothing and is left alone.
pub fn import_previewed_and_clear(
    inbox: &Arc<Inbox>,
    runtime: &Handle,
    clip: Option<String>,
    shown: &PairingPreview,
    pending: &PendingPairing,
    clear: impl FnOnce(),
) -> Result<MachineEntryView, String> {
    let digest = line_digest(clip.as_deref());
    let same = preview_text(clip.as_deref())? == *shown
        && digest.is_some()
        && *pending.0.lock().unwrap() == digest;
    if !same {
        return Err("The clipboard changed after it was shown. Nothing was added.".to_owned());
    }
    let imported = import_and_clear(inbox, runtime, clip, clear)?;
    *pending.0.lock().unwrap() = None;
    Ok(imported)
}

/// Imports the clipboard text as a pairing line. `runtime` is entered for the
/// import: shells call this from synchronous commands, outside Tokio. The
/// error text never echoes the clipboard.
pub fn import_text(
    inbox: &Arc<Inbox>,
    runtime: &Handle,
    clip: Option<String>,
) -> Result<MachineEntryView, String> {
    let text = clip
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| "The clipboard does not hold an acd pairing line.".to_owned())?;
    let _runtime_guard = runtime.enter();
    let entry = inbox.import_pairing(&text)?;
    inbox
        .machines()
        .into_iter()
        .find(|machine| machine.key == entry.key)
        .ok_or_else(|| "Could not list saved machines".to_owned())
}

/// Like [`import_text`], and clears the clipboard through `clear` once the
/// machine is saved. The pairing line carries the client token, so nothing
/// else needs it afterwards. A failed import leaves the line for another try.
pub fn import_and_clear(
    inbox: &Arc<Inbox>,
    runtime: &Handle,
    clip: Option<String>,
    clear: impl FnOnce(),
) -> Result<MachineEntryView, String> {
    let imported = import_text(inbox, runtime, clip)?;
    clear();
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use acd_client::{
        Timing,
        inbox::InboxOptions,
        settings::{MemorySecrets, Settings},
    };

    const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    fn view_has_only_public_fields(view: MachineEntryView) {
        let MachineEntryView {
            key: _,
            name: _,
            endpoint: _,
            pairing: _,
            secret: _,
        } = view;
    }

    /// The runtime must outlive the inbox's workers, so the caller holds it.
    fn inbox(runtime: &tokio::runtime::Runtime) -> Arc<Inbox> {
        let inbox = Inbox::new(
            Settings {
                machines: vec![],
                ..Settings::fresh()
            },
            Arc::new(MemorySecrets::default()),
            InboxOptions {
                timing: Timing::default(),
                settings_path: None,
                client_name: "agent-caller-test".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
            },
        );
        let _guard = runtime.enter();
        inbox.start();
        inbox
    }

    #[test]
    fn a_preview_saves_nothing_and_carries_no_token() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let inbox = inbox(&runtime);
        let pin = acd_protocol::pairing::spki_pin(b"test certificate");
        let line = format!("acd://example.test:47800?pin={pin}#{TOKEN}");
        let preview = preview_text(Some(&line)).unwrap();
        assert_eq!(preview, PairingPreview { host: "example.test".into(), port: 47800, pin });
        assert!(!format!("{preview:?}").contains(TOKEN));
        assert!(inbox.machines().is_empty(), "a preview must not save");
        for bad in [None, Some("  "), Some("hello #tok")] {
            let err = preview_text(bad).unwrap_err();
            assert!(!err.contains("tok"), "{err}");
        }
        let lan = format!("acd://192.168.1.10:47800?pin={}#{TOKEN}", preview.pin);
        assert!(preview_text(Some(&lan)).unwrap_err().contains("Tailscale"));
    }

    #[test]
    fn a_changed_clipboard_is_not_imported_after_the_preview() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let inbox = inbox(&runtime);
        let pin = acd_protocol::pairing::spki_pin(b"test certificate");
        let line = format!("acd://example.test:47800?pin={pin}#{TOKEN}");
        let pending = PendingPairing::default();
        let shown = pending.preview(Some(&line)).unwrap();
        let swapped = format!("acd://evil.test:47800?pin={pin}#{TOKEN}");
        let mut cleared = 0;
        let err = import_previewed_and_clear(&inbox, runtime.handle(), Some(swapped), &shown, &pending, || cleared += 1)
            .unwrap_err();
        assert!(err.contains("changed"), "{err}");
        assert!(inbox.machines().is_empty());
        assert_eq!(cleared, 0);
        import_previewed_and_clear(&inbox, runtime.handle(), Some(line), &shown, &pending, || cleared += 1).unwrap();
        assert_eq!((inbox.machines().len(), cleared), (1, 1));
    }

    /// The preview payload has no token, so only the Rust-side digest can tell
    /// a clipboard that swapped just the token (finding 19).
    #[test]
    fn a_clipboard_with_only_the_token_changed_is_not_imported() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let inbox = inbox(&runtime);
        let pin = acd_protocol::pairing::spki_pin(b"test certificate");
        let pending = PendingPairing::default();
        let shown = pending.preview(Some(&format!("acd://example.test:47800?pin={pin}#{TOKEN}"))).unwrap();
        let other_token = format!("B{}", &TOKEN[1..]);
        let swapped = format!("acd://example.test:47800?pin={pin}#{other_token}");
        let mut cleared = 0;
        let err = import_previewed_and_clear(&inbox, runtime.handle(), Some(swapped), &shown, &pending, || cleared += 1)
            .unwrap_err();
        assert!(err.contains("changed"), "{err}");
        assert!(!err.contains(&other_token), "the error names the token ({} bytes)", err.len());
        assert!(inbox.machines().is_empty());
        assert_eq!(cleared, 0);
    }

    #[test]
    fn a_confirm_without_a_preview_imports_nothing() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let inbox = inbox(&runtime);
        let pin = acd_protocol::pairing::spki_pin(b"test certificate");
        let line = format!("acd://example.test:47800?pin={pin}#{TOKEN}");
        let shown = preview_text(Some(&line)).unwrap();
        let err = import_previewed_and_clear(&inbox, runtime.handle(), Some(line), &shown, &PendingPairing::default(), || {})
            .unwrap_err();
        assert!(err.contains("changed"), "{err}");
        assert!(inbox.machines().is_empty());
    }

    #[test]
    fn errors_never_echo_the_clipboard() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let inbox = inbox(&runtime);
        let handle = runtime.handle();
        let empty = import_text(&inbox, handle, None).unwrap_err();
        let whitespace = import_text(&inbox, handle, Some(" \n\t ".into())).unwrap_err();
        let malformed = import_text(&inbox, handle, Some("hello #tok".into())).unwrap_err();
        assert!(!empty.contains("tok"));
        assert!(!whitespace.contains("tok"));
        assert!(!malformed.contains("tok"));
    }

    #[test]
    fn a_literal_lan_address_is_refused_at_import() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let inbox = inbox(&runtime);
        let pin = acd_protocol::pairing::spki_pin(b"test certificate");
        let refused = import_text(
            &inbox,
            runtime.handle(),
            Some(format!("acd://192.168.1.10:47800?pin={pin}#{TOKEN}")),
        )
        .unwrap_err();
        assert!(refused.contains("Tailscale"), "{refused}");
        assert!(inbox.machines().is_empty());
    }

    /// With a refusing store (Windows; Android before the Keystore store is
    /// installed) a pairing stores no secret, so it must commit nothing: no
    /// machine listed, no settings written, and the clipboard line kept.
    #[test]
    fn a_refusing_secret_store_commits_no_machine() {
        refusing_store_commits_nothing(Arc::new(crate::nostore::UnsupportedSecrets));
    }

    #[test]
    fn an_uninstalled_android_keystore_commits_no_machine() {
        refusing_store_commits_nothing(Arc::new(crate::wrapped::KeystoreNotInstalled));
    }

    fn refusing_store_commits_nothing(secrets: Arc<dyn acd_client::settings::SecretStore>) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let _guard = runtime.enter();
        let inbox = Inbox::new(
            Settings { machines: vec![], ..Settings::fresh() },
            secrets,
            InboxOptions {
                timing: Timing::default(),
                settings_path: Some(path.clone()),
                client_name: "agent-caller-test".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
            },
        );
        inbox.start();
        let pin = acd_protocol::pairing::spki_pin(b"test certificate");
        let mut cleared = 0;
        let err = import_and_clear(
            &inbox,
            runtime.handle(),
            Some(format!("acd://example.test:47800?pin={pin}#{TOKEN}")),
            || cleared += 1,
        )
        .unwrap_err();
        assert!(!err.contains(TOKEN), "{err}");
        assert!(inbox.machines().is_empty(), "no machine may be listed");
        assert!(!path.exists(), "no settings may be written");
        assert_eq!(cleared, 0, "the clipboard line is kept");
    }

    #[test]
    fn the_clipboard_is_cleared_after_a_saved_machine_and_kept_after_a_failure() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let inbox = inbox(&runtime);
        let pin = acd_protocol::pairing::spki_pin(b"test certificate");
        let mut cleared = 0;
        import_and_clear(
            &inbox,
            runtime.handle(),
            Some(format!("acd://example.test:47800?pin={pin}#{TOKEN}")),
            || cleared += 1,
        )
        .expect("valid pairing imports");
        assert_eq!(cleared, 1);
        let failed = import_and_clear(&inbox, runtime.handle(), Some("hello #tok".into()), || {
            cleared += 1
        });
        assert!(failed.is_err());
        assert_eq!(cleared, 1, "a failed import must leave the clipboard");
    }

    #[test]
    fn successful_import_works_from_a_sync_caller() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let inbox = inbox(&runtime);
        let pin = acd_protocol::pairing::spki_pin(b"test certificate");
        let imported = import_text(
            &inbox,
            runtime.handle(),
            Some(format!("acd://example.test:47800?pin={pin}#{TOKEN}")),
        )
        .expect("valid pairing should import outside a Tokio runtime");
        view_has_only_public_fields(imported);
    }
}
