//! Pairing from clipboard text: the `acd pair` line becomes a remote machine.

use std::sync::Arc;

use acd_client::{inbox::Inbox, view::MachineEntryView};
use tokio::runtime::Handle;

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
