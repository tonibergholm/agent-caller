//! iOS shell for the agent-caller inbox: a foreground client that reaches the
//! daemon over Tailscale. It runs acd-client's Inbox and serves the Svelte UI
//! from app/dist. No panel, tray, hotkey, voice or notifications. The local
//! `acd.sock` is never offered (`local_machine: false`).

mod commands;

#[cfg(all(debug_assertions, target_os = "ios", target_abi = "sim"))]
use std::sync::Arc;

use acd_app_core::{commands::AppState, keychain, setup};
#[cfg(all(debug_assertions, target_os = "ios", target_abi = "sim"))]
use acd_client::inbox::Inbox;
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
            let inbox = setup::open_inbox(
                &path,
                keychain::cached_secrets(),
                tauri::async_runtime::handle().inner(),
                setup::OpenOptions {
                    client_name: "agent-caller-ios",
                    client_version: env!("CARGO_PKG_VERSION"),
                    local_machine: false,
                },
            )?;
            #[cfg(all(debug_assertions, target_os = "ios", target_abi = "sim"))]
            import_dev_pairing(&inbox);
            app.manage(AppState {
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
        .expect("failed to run the iOS shell");
}

/// Simulator debug builds only, for development. `ACD_IOS_DEV_PAIRING_FILE`
/// names a file that holds one `acd pair` line. It goes through the real
/// Keychain store at launch, because the app has no pairing screen yet. The
/// line is never logged. Device and release builds do not compile this.
#[cfg(all(debug_assertions, target_os = "ios", target_abi = "sim"))]
fn import_dev_pairing(inbox: &Arc<Inbox>) {
    let Some(path) = std::env::var_os("ACD_IOS_DEV_PAIRING_FILE") else {
        return;
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => match acd_app_core::pairing::import_text(
            inbox,
            tauri::async_runtime::handle().inner(),
            Some(text.trim().to_owned()),
        ) {
            Ok(machine) => tracing::info!(name = %machine.name, "dev pairing imported"),
            Err(error) => tracing::error!(%error, "dev pairing refused"),
        },
        Err(error) => tracing::error!(kind = ?error.kind(), "cannot read the dev pairing file"),
    }
}
