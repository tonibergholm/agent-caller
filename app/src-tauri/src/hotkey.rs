use std::sync::Mutex;
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

static CURRENT_SHORTCUT: Mutex<Option<Shortcut>> = Mutex::new(None);

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
    let shortcut = parse_shortcut(hotkey)?;
    apply_shortcut(app, shortcut)
}

pub fn current_shortcut() -> Option<Shortcut> {
    CURRENT_SHORTCUT.lock().ok().and_then(|current| *current)
}

pub fn restore(app: &AppHandle, previous: Option<Shortcut>) -> Result<(), String> {
    let shortcuts = app.global_shortcut();
    let mut current = CURRENT_SHORTCUT
        .lock()
        .map_err(|_| "Hotkey state lock poisoned".to_string())?;
    if *current == previous {
        return Ok(());
    }
    if let Some(previous) = previous {
        shortcuts
            .register(previous)
            .map_err(|error| format!("Could not restore previous hotkey: {error}"))?;
    }
    let old = *current;
    *current = previous;
    if let Some(old) = old
        && let Err(error) = shortcuts.unregister(old)
    {
        tracing::warn!(%error, "could not unregister the hotkey being rolled back");
    }
    Ok(())
}

fn apply_shortcut(app: &AppHandle, shortcut: Shortcut) -> Result<(), String> {
    let label = shortcut.to_string();
    let shortcuts = app.global_shortcut();
    let mut current = CURRENT_SHORTCUT
        .lock()
        .map_err(|_| "Hotkey state lock poisoned".to_string())?;
    if current.as_ref() == Some(&shortcut) {
        return Ok(());
    }
    shortcuts
        .register(shortcut)
        .map_err(|error| format!("Could not register {label}: {error}"))?;
    // The new shortcut is live from here on, so record it even if the old
    // one fails to unregister.
    if let Some(old) = current.replace(shortcut)
        && let Err(error) = shortcuts.unregister(old)
    {
        tracing::warn!(%error, "could not unregister the previous hotkey");
    }
    Ok(())
}

fn parse_shortcut(hotkey: &str) -> Result<Shortcut, String> {
    hotkey
        .parse()
        .map_err(|error| format!("Not a valid shortcut: {error}"))
}

#[cfg(test)]
mod tests {
    use super::parse_shortcut;

    #[test]
    fn parses_new_shortcut_before_any_registration_change() {
        let parsed = parse_shortcut("Control+Alt+Space");
        assert!(parsed.is_ok());

        let invalid = parse_shortcut("not a shortcut");
        assert!(invalid.is_err());
    }
}
