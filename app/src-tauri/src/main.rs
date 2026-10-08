mod hotkey;
mod keychain;
mod panel;

use tauri::{RunEvent, WebviewWindow};
use tauri_plugin_notification::NotificationExt;

// GATE (Task 13): remove in Task 16
#[tauri::command]
fn gate_notify(window: WebviewWindow) -> Result<(), String> {
    window
        .notification()
        .builder()
        .title("Agent Caller")
        .body("Gate test")
        .show()
        .map_err(|error| error.to_string())
}

// GATE (Task 13): remove in Task 16
#[tauri::command]
fn gate_keychain_roundtrip() -> Result<(), String> {
    keychain::gate_roundtrip().map_err(|error| error.to_string())
}

fn main() {
    // GATE (Task 13): remove in Task 16
    // Gate diagnostics: `open` discards stdout, so log to a file as well.
    let log_dir = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join("Library/Logs/AgentCaller");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("gate.log"))
        .ok();
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    match log_file {
        Some(file) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init(),
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
    let app = tauri::Builder::default()
        .plugin(tauri_nspanel::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(hotkey::plugin())
        // GATE (Task 13): remove in Task 16
        .invoke_handler(tauri::generate_handler![gate_notify, gate_keychain_roundtrip])
        .setup(|app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            panel::create(app.handle())?;
            // GATE (Task 13): remove in Task 16
            let registered = hotkey::apply(app.handle(), "Control+Alt+Space");
            tracing::info!(?registered, "gate hotkey Control+Alt+Space");
            registered.map_err(std::io::Error::other)?;
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
