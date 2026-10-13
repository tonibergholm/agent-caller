mod commands;
mod frame;
mod hotkey;
mod notify;
mod panel;
mod settings_window;
mod tray;
mod voice;

use std::sync::Arc;

use acd_app_core::{commands::AppState, setup};
use acd_client::settings::DEFAULT_HOTKEY;
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
            notify::notification_authorization,
            notify::request_notification_authorization,
            voice::commands::voice_get_settings,
            voice::commands::voice_set_settings,
            voice::commands::voice_publish_presentation,
            voice::commands::voice_text_input,
            voice::commands::voice_shown,
            voice::commands::voice_confirm,
            voice::commands::voice_choose,
            voice::commands::voice_cancel,
            voice::commands::voice_hud_fit,
            voice::commands::voice_probe,
            voice::commands::voice_open_privacy,
            voice::commands::voice_status,
            voice::commands::voice_request_permission,
            voice::commands::voice_install_assets,
        ])
        .setup(|app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            if let Err(error) = notify::init(app.handle()) {
                tracing::error!(%error, "could not initialize macOS notifications");
            }
            // macOS prompts only while the state is not determined, so asking
            // on every start is the first-run request.
            notify::request_authorization();
            let path = app.path().app_config_dir()?.join("settings.json");
            let inbox = setup::open_inbox(
                &path,
                acd_app_core::keychain::cached_secrets(),
                tauri::async_runtime::handle().inner(),
                setup::OpenOptions {
                    client_name: "agent-caller-mac",
                    client_version: env!("CARGO_PKG_VERSION"),
                    local_machine: true,
                },
            )?;
            app.manage(AppState {
                inbox: inbox.clone(),
            });
            let voice_path = app.path().app_config_dir()?.join("voice.json");
            let voice_settings = voice::settings::VoiceSettings::load(&voice_path);
            let sink = Arc::new(voice::TauriSink::new(app.handle().clone()));
            // Step 3: Apple's on-device model on macOS (plan V6); elsewhere the fast path only.
            #[cfg(target_os = "macos")]
            let model = Arc::new(voice::model::MacIntentModel::new(Arc::new(voice::model::SwiftModelPort), voice::model::MODEL_TIMEOUT));
            #[cfg(target_os = "macos")]
            let intent: Option<Arc<dyn acd_voice::router::model::IntentModel>> = Some(model.clone());
            #[cfg(not(target_os = "macos"))]
            let intent: Option<Arc<dyn acd_voice::router::model::IntentModel>> = None;
            let controller = tauri::async_runtime::block_on(async {
                acd_voice_client::Controller::start(inbox.clone(), intent, sink, voice_settings.enabled)
            });
            #[cfg(target_os = "macos")]
            let speech = Arc::new(voice::bridge::MacSpeech::new());
            #[cfg(target_os = "macos")]
            controller.attach_speech(speech.clone());
            app.manage(voice::VoiceState {
                controller,
                settings: std::sync::Mutex::new(voice_settings),
                path: Some(voice_path),
                trigger_error: std::sync::Mutex::new(None),
                #[cfg(target_os = "macos")]
                speech,
                #[cfg(target_os = "macos")]
                model,
            });
            tray::build(app.handle())?;
            panel::create(app.handle())?;
            voice::hud::create(app.handle())?;
            #[cfg(target_os = "macos")]
            if let Some((link, events)) = voice::bridge::start() {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(voice::bridge::pump(link, events, move |ev| voice::dispatch(&handle, ev)));
            }

            let saved_hotkey = inbox.settings().hotkey;
            if let Err(error) = hotkey::apply(app.handle(), &saved_hotkey) {
                tracing::error!(%error, "could not register saved hotkey; using default");
                if let Err(default_error) = hotkey::apply(app.handle(), DEFAULT_HOTKEY) {
                    tracing::error!(%default_error, "could not register default hotkey");
                }
            }
            #[cfg(target_os = "macos")]
            {
                let voice = app.state::<voice::VoiceState>();
                let settings = voice.settings.lock().unwrap().clone();
                *voice.trigger_error.lock().unwrap() = voice::trigger::apply(app.handle(), &settings, &voice::settings::VoiceSettings { enabled: false, ..settings.clone() });
                // An upgrade can leave voice on with permissions never asked
                // for (step 1 needed none): ask for those now, once.
                if settings.enabled {
                    use voice::permissions;
                    let hold = settings.hold_key != voice::settings::HoldKey::Off;
                    for p in permissions::to_request(hold, permissions::status) {
                        permissions::request(p);
                    }
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
