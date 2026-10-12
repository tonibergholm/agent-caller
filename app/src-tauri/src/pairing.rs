use std::sync::Arc;

use acd_client::{inbox::Inbox, view::MachineEntryView};

pub fn import_text(inbox: &Arc<Inbox>, clip: Option<String>) -> Result<MachineEntryView, String> {
    let text = clip
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| "The clipboard does not hold an acd pairing line.".to_owned())?;
    let _runtime_guard = tauri::async_runtime::handle().inner().enter();
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

    fn view_has_only_public_fields(view: MachineEntryView) {
        let MachineEntryView {
            key: _,
            name: _,
            endpoint: _,
            pairing: _,
            secret: _,
        } = view;
    }

    fn inbox() -> Arc<Inbox> {
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
        tauri::async_runtime::block_on(async {
            inbox.start();
        });
        inbox
    }

    #[test]
    fn errors_never_echo_the_clipboard() {
        let inbox = inbox();
        let empty = import_text(&inbox, None).unwrap_err();
        let whitespace = import_text(&inbox, Some(" \n\t ".into())).unwrap_err();
        let malformed = import_text(&inbox, Some("hello #tok".into())).unwrap_err();
        assert!(!empty.contains("tok"));
        assert!(!whitespace.contains("tok"));
        assert!(!malformed.contains("tok"));
    }

    #[test]
    fn successful_import_enters_tauri_runtime_from_sync_caller() {
        let inbox = inbox();
        let token = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let pin = acd_protocol::pairing::spki_pin(b"test certificate");
        let imported = import_text(
            &inbox,
            Some(format!("acd://example.test:47800?pin={pin}#{token}")),
        )
        .expect("valid pairing should import outside a Tokio runtime");
        view_has_only_public_fields(imported);
    }
}
