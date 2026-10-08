mod commands;
mod hotkey;
mod keychain;
mod pairing;
mod panel;
mod settings_window;
mod tray;

use std::sync::Arc;

use acd_client::{
    Timing,
    inbox::{Inbox, InboxOptions},
    settings::{DEFAULT_HOTKEY, Settings},
};
use tauri::{Emitter, Manager, RunEvent};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let app = tauri::Builder::default()
        .plugin(tauri_nspanel::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(hotkey::plugin())
        .invoke_handler(tauri::generate_handler![
            commands::get_view,
            commands::pick_option,
            commands::send_reply,
            commands::cancel_action,
            commands::escape,
            commands::mark_seen,
            commands::set_show_idle,
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
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let path = app.path().app_config_dir()?.join("settings.json");
            let settings = Settings::load_or_init(&path)?;
            let inbox = Inbox::new(
                settings,
                Arc::new(keychain::KeychainSecrets::new(keychain::SERVICE_NAME)),
                InboxOptions {
                    timing: Timing::default(),
                    settings_path: Some(path),
                    client_name: "agent-caller-mac".into(),
                    client_version: env!("CARGO_PKG_VERSION").into(),
                },
            );
            app.manage(commands::AppState {
                inbox: inbox.clone(),
            });
            tray::build(app.handle())?;
            panel::create(app.handle())?;

            let saved_hotkey = inbox.settings().hotkey;
            if let Err(error) = hotkey::apply(app.handle(), &saved_hotkey) {
                tracing::error!(%error, "could not register saved hotkey; using default");
                if let Err(default_error) = hotkey::apply(app.handle(), DEFAULT_HOTKEY) {
                    tracing::error!(%default_error, "could not register default hotkey");
                }
            }

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                inbox.start();
                let mut rx = inbox.view();
                loop {
                    let view = rx.borrow_and_update().clone();
                    tray::update(&handle, &view.tray);
                    let _ = handle.emit("inbox://view", &view);
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build agent-caller");
    app.run(|_, event| {
        // Menu bar app: closing the settings window must not quit.
        if let RunEvent::ExitRequested {
            api, code: None, ..
        } = event
        {
            api.prevent_exit();
        }
    });
}
