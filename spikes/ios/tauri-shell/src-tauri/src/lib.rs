//! THROWAWAY SPIKE (2026-10-12). Minimal Tauri 2 iOS shell: runs acd-client's
//! Inbox and serves the existing Svelte UI. No panel, tray, hotkey, voice,
//! notifications or Swift bridge. keychain.rs and pairing.rs are shared with
//! app/src-tauri through #[path]; neither file is modified.

#[path = "../../../../../app/src-tauri/src/keychain.rs"]
mod keychain;
#[path = "../../../../../app/src-tauri/src/pairing.rs"]
mod pairing;
mod commands;

use std::sync::Arc;

use acd_client::{
    Timing,
    inbox::{Inbox, InboxOptions},
    settings::{CachedSecrets, Settings},
};
use tauri::{Emitter, Manager};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_view,
            commands::pick_option,
            commands::send_reply,
            commands::answer_text,
            commands::cancel_action,
            commands::escape,
            commands::mark_seen,
            commands::set_show_idle,
            commands::set_show_subagents,
            commands::set_show_hook_only,
            commands::list_machines,
            commands::import_pairing_from_clipboard,
            commands::rename_machine,
            commands::remove_machine,
            commands::retry_machine,
            commands::get_hotkey,
            commands::set_hotkey,
            commands::hide_panel,
            commands::open_settings,
        ])
        .setup(|app| {
            let path = app.path().app_config_dir()?.join("settings.json");
            let settings = Settings::load_or_init(&path)?;
            let inbox = Inbox::new(
                settings,
                Arc::new(CachedSecrets::new(keychain::KeychainSecrets::new(
                    keychain::SERVICE_NAME,
                ))),
                InboxOptions {
                    timing: Timing::default(),
                    settings_path: Some(path),
                    client_name: "agent-caller-ios-spike".into(),
                    client_version: env!("CARGO_PKG_VERSION").into(),
                },
            );
            tauri::async_runtime::block_on(async {
                inbox.start();
            });
            // Spike-only pairing: ACD_SPIKE_PAIRING_FILE names a file with one
            // pairing line, passed through `simctl launch`. The token is never
            // logged. The secret goes through the real Keychain store.
            if let Some(path) = std::env::var_os("ACD_SPIKE_PAIRING_FILE") {
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        let _runtime_guard = tauri::async_runtime::handle().inner().enter();
                        match inbox.import_pairing(text.trim()) {
                            Ok(entry) => tracing::info!(name = %entry.name, "spike: imported pairing"),
                            Err(error) => tracing::error!(%error, "spike: pairing import refused"),
                        }
                    }
                    Err(error) => tracing::error!(kind = ?error.kind(), "spike: cannot read pairing file"),
                }
            }
            app.manage(commands::AppState {
                inbox: inbox.clone(),
            });
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut rx = inbox.view();
                loop {
                    let view = rx.borrow_and_update().clone();
                    if let Err(error) = handle.emit("inbox://view", &view) {
                        tracing::error!(%error, "could not emit inbox view");
                    }
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run the iOS spike shell");
}
