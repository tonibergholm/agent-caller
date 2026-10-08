use std::sync::Arc;

use acd_client::{inbox::Inbox, view::MachineEntryView};

pub fn import_text(inbox: &Arc<Inbox>, clip: Option<String>) -> Result<MachineEntryView, String> {
    let text = clip
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| "The clipboard does not hold an acd pairing line.".to_owned())?;
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
        } = view;
    }

    #[tokio::test]
    async fn errors_never_echo_the_clipboard() {
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
        inbox.start();

        let empty = import_text(&inbox, None).unwrap_err();
        let malformed = import_text(&inbox, Some("hello #tok".into())).unwrap_err();
        assert!(!empty.contains("tok"));
        assert!(!malformed.contains("tok"));

        let _ = view_has_only_public_fields;
    }
}
