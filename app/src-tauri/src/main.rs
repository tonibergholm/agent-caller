mod commands;
mod frame;
mod hotkey;
mod keychain;
mod notify;
mod pairing;
mod panel;
mod settings_window;
mod tray;
mod voice;

use std::sync::Arc;

use acd_client::{
    Timing,
    inbox::{Inbox, InboxOptions},
    settings::{CachedSecrets, DEFAULT_HOTKEY, Settings},
};
use tauri::{Emitter, Manager, RunEvent};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let builder = tauri::Builder::default();
    // macOS posts through mac-notification-sys (notify.rs); elsewhere the
    // plugin is the backend and must be registered.
    #[cfg(not(target_os = "macos"))]
    let builder = builder.plugin(tauri_plugin_notification::init());
    let app = builder
        .plugin(tauri_nspanel::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(hotkey::plugin())
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
            commands::list_machines,
            commands::import_pairing_from_clipboard,
            commands::rename_machine,
            commands::remove_machine,
            commands::retry_machine,
            commands::get_hotkey,
            commands::set_hotkey,
            commands::hide_panel,
            commands::open_settings,
            voice::commands::voice_get_settings,
            voice::commands::voice_set_settings,
            voice::commands::voice_publish_presentation,
            voice::commands::voice_text_input,
            voice::commands::voice_shown,
            voice::commands::voice_confirm,
            voice::commands::voice_choose,
            voice::commands::voice_cancel,
        ])
        .setup(|app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            if let Err(error) = notify::init(app.handle()) {
                tracing::error!(%error, "could not initialize macOS notifications");
            }
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
                    client_name: "agent-caller-mac".into(),
                    client_version: env!("CARGO_PKG_VERSION").into(),
                },
            );
            tauri::async_runtime::block_on(async {
                inbox.start();
            });
            app.manage(commands::AppState {
                inbox: inbox.clone(),
            });
            let voice_path = app.path().app_config_dir()?.join("voice.json");
            let voice_settings = voice::settings::VoiceSettings::load(&voice_path);
            let sink = Arc::new(voice::TauriSink::new(app.handle().clone()));
            // Step 1 has no intent model (D14): the fast path only.
            let controller = tauri::async_runtime::block_on(async {
                acd_voice_client::Controller::start(inbox.clone(), None, sink, voice_settings.enabled)
            });
            app.manage(voice::VoiceState {
                controller,
                settings: std::sync::Mutex::new(voice_settings),
                path: Some(voice_path),
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
            let notifications = inbox.events();
            let notification_handle = handle.clone();
            tauri::async_runtime::spawn(notify::run(notification_handle, notifications));
            tauri::async_runtime::spawn(async move {
                let mut rx = inbox.view();
                loop {
                    let view = rx.borrow_and_update().clone();
                    tray::update(&handle, &view.tray);
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
        .build(tauri::generate_context!())
        .expect("failed to build agent-caller");
    app.run(|handle, event| match event {
        // Menu bar app: closing the settings window must not quit.
        RunEvent::ExitRequested {
            api, code: None, ..
        } => api.prevent_exit(),
        // Keep a drag made just before quitting.
        RunEvent::Exit => panel::save_now(handle),
        _ => {}
    });
}
