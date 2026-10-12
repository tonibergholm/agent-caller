use std::sync::Mutex;
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

static CURRENT_SHORTCUT: Mutex<Option<Shortcut>> = Mutex::new(None);

/// The voice toggle (plan S12); registered only while voice is on.
static VOICE_SHORTCUT: Mutex<Option<Shortcut>> = Mutex::new(None);

#[derive(Debug, PartialEq, Eq)]
pub enum Fired {
    Panel,
    Voice,
}

/// Which action a pressed shortcut belongs to. Anything but the voice
/// toggle keeps toggling the panel, as before.
pub fn route(fired: &Shortcut, voice: Option<Shortcut>) -> Fired {
    if voice.as_ref() == Some(fired) { Fired::Voice } else { Fired::Panel }
}

pub fn voice_shortcut() -> Option<Shortcut> {
    VOICE_SHORTCUT.lock().ok().and_then(|v| *v)
}

/// Parses a voice toggle and refuses the inbox hotkey.
pub fn check_voice(hotkey: &str, panel: Option<Shortcut>) -> Result<Shortcut, String> {
    let shortcut = parse_shortcut(hotkey)?;
    if Some(shortcut) == panel {
        return Err("That is the inbox hotkey. Choose another voice shortcut.".into());
    }
    Ok(shortcut)
}

/// Whether `hotkey` is the voice toggle; the inbox hotkey must not be.
pub fn conflicts_with_voice(hotkey: &str) -> bool {
    parse_shortcut(hotkey).ok().is_some_and(|s| Some(s) == voice_shortcut())
}

/// Registers the voice toggle, or unregisters it with `None`.
pub fn apply_voice(app: &AppHandle, hotkey: Option<&str>) -> Result<(), String> {
    let next = hotkey.map(|h| check_voice(h, current_shortcut())).transpose()?;
    let shortcuts = app.global_shortcut();
    let mut voice = VOICE_SHORTCUT.lock().map_err(|_| "Voice shortcut lock poisoned".to_string())?;
    if *voice == next {
        return Ok(());
    }
    if let Some(shortcut) = next {
        shortcuts
            .register(shortcut)
            .map_err(|error| format!("Could not register {shortcut}: {error}"))?;
    }
    if let Some(old) = std::mem::replace(&mut *voice, next)
        && let Err(error) = shortcuts.unregister(old)
    {
        tracing::warn!(%error, "could not unregister the previous voice shortcut");
    }
    Ok(())
}

pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, shortcut, event| {
            if event.state != ShortcutState::Pressed {
                return;
            }
            match route(shortcut, voice_shortcut()) {
                Fired::Voice => {
                    tracing::info!("voice shortcut pressed");
                    #[cfg(target_os = "macos")]
                    crate::voice::trigger::toggle(app);
                }
                Fired::Panel => {
                    tracing::info!("hotkey pressed");
                    crate::panel::toggle(app);
                }
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
    use super::{Fired, check_voice, parse_shortcut, route};

    #[test]
    fn parses_new_shortcut_before_any_registration_change() {
        let parsed = parse_shortcut("Control+Alt+Space");
        assert!(parsed.is_ok());

        let invalid = parse_shortcut("not a shortcut");
        assert!(invalid.is_err());
    }

    #[test]
    fn only_the_voice_shortcut_triggers_voice() {
        let voice = parse_shortcut("Control+Alt+V").unwrap();
        let panel = parse_shortcut("Control+Alt+Space").unwrap();
        assert_eq!(route(&voice, Some(voice)), Fired::Voice);
        assert_eq!(route(&panel, Some(voice)), Fired::Panel);
        assert_eq!(route(&panel, None), Fired::Panel);
    }

    /// Plan S12: the two shortcuts can never be the same.
    #[test]
    fn the_voice_shortcut_cannot_be_the_inbox_hotkey() {
        let panel = parse_shortcut("Control+Alt+Space").unwrap();
        assert!(check_voice("Control+Alt+Space", Some(panel)).is_err());
        assert!(check_voice("not a shortcut", Some(panel)).is_err());
        assert_eq!(check_voice("Control+Alt+V", Some(panel)), parse_shortcut("Control+Alt+V"));
    }
}
