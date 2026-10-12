//! Opening the inbox at app start: the settings file, the secret store, and
//! the start of the connection workers.

use std::path::Path;
use std::sync::Arc;

use acd_client::{
    Timing,
    inbox::{Inbox, InboxOptions},
    settings::{EndpointConfig, SecretStore, Settings, SettingsError},
};
use tokio::runtime::Handle;

/// What differs between the shells at startup.
pub struct OpenOptions<'a> {
    /// Name and version the daemon sees in the client handshake.
    pub client_name: &'a str,
    pub client_version: &'a str,
    /// Whether the `local` entry (this machine's `acd.sock`) is offered. Only
    /// the Mac app runs an acd on the same machine. The iOS shell offers paired
    /// TCP machines only.
    pub local_machine: bool,
}

/// Loads the settings at `settings_path` (creating them on first run), builds
/// the inbox over `secrets`, and starts it. `runtime` is entered for the start,
/// because the shells call this from their setup, outside Tokio.
pub fn open_inbox(
    settings_path: &Path,
    secrets: Arc<dyn SecretStore>,
    runtime: &Handle,
    opts: OpenOptions<'_>,
) -> Result<Arc<Inbox>, SettingsError> {
    let mut settings = Settings::load_or_init(settings_path)?;
    offered_machines(&mut settings, opts.local_machine);
    let inbox = Inbox::new(
        settings,
        secrets,
        InboxOptions {
            timing: Timing::default(),
            settings_path: Some(settings_path.to_path_buf()),
            client_name: opts.client_name.into(),
            client_version: opts.client_version.into(),
        },
    );
    let _runtime_guard = runtime.enter();
    inbox.start();
    Ok(inbox)
}

/// Drops the `local` entry unless the platform offers it. The file keeps it
/// until the inbox next saves its settings, which is harmless: it is dropped
/// on every load.
fn offered_machines(settings: &mut Settings, local_machine: bool) {
    if !local_machine {
        settings
            .machines
            .retain(|machine| machine.endpoint != EndpointConfig::Local);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acd_client::settings::{LOCAL_KEY, MachineEntry, MemorySecrets};

    const REMOTE_KEY: &str = "5f1c9d2e-8a1b-4c3d-9e8f-0a1b2c3d4e5f";

    fn settings_with_remote_and_local() -> Settings {
        let mut settings = Settings::fresh();
        settings.machines.push(MachineEntry {
            key: REMOTE_KEY.into(),
            name: "devbox".into(),
            endpoint: EndpointConfig::Tcp {
                host: "127.0.0.1".into(),
                // Nothing listens here: the worker fails and the test only reads the list.
                port: 1,
            },
        });
        settings
    }

    fn keys(settings: &Settings) -> Vec<&str> {
        settings.machines.iter().map(|m| m.key.as_str()).collect()
    }

    #[test]
    fn ios_offers_no_local_machine() {
        let mut settings = settings_with_remote_and_local();
        offered_machines(&mut settings, false);
        assert_eq!(keys(&settings), vec![REMOTE_KEY]);
    }

    #[test]
    fn mac_keeps_the_local_machine() {
        let mut settings = settings_with_remote_and_local();
        offered_machines(&mut settings, true);
        assert_eq!(keys(&settings), vec![LOCAL_KEY, REMOTE_KEY]);
    }

    #[test]
    fn opened_inbox_on_ios_lists_no_local_machine() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        settings_with_remote_and_local().save(&path).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let inbox = open_inbox(
            &path,
            Arc::new(MemorySecrets::default()),
            runtime.handle(),
            OpenOptions {
                client_name: "agent-caller-test",
                client_version: "0.0.0",
                local_machine: false,
            },
        )
        .unwrap();
        let listed: Vec<String> = inbox.machines().into_iter().map(|m| m.key).collect();
        assert_eq!(listed, vec![REMOTE_KEY.to_owned()]);
    }
}
