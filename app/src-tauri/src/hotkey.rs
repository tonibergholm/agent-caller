use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                tracing::info!("hotkey pressed");
                crate::panel::toggle(app);
            }
        })
        .build()
}

pub fn apply(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    let shortcut: Shortcut = hotkey
        .parse()
        .map_err(|error| format!("Not a valid shortcut: {error}"))?;
    let shortcuts = app.global_shortcut();
    shortcuts
        .unregister_all()
        .map_err(|error| error.to_string())?;
    shortcuts
        .register(shortcut)
        .map_err(|error| format!("Could not register {hotkey}: {error}"))
}
