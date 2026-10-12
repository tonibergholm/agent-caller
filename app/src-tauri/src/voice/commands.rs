use acd_voice::snapshot::Presentation;
use tauri::{AppHandle, Emitter, State, WebviewWindow};

use super::VoiceState;
use super::hud;
use super::settings::VoiceSettings;

#[tauri::command]
pub fn voice_get_settings(voice: State<'_, VoiceState>) -> VoiceSettings {
    voice.settings.lock().unwrap().clone()
}

#[tauri::command]
pub fn voice_set_settings(
    app: AppHandle,
    voice: State<'_, VoiceState>,
    settings: VoiceSettings,
) -> Result<VoiceSettings, String> {
    if let Some(toggle) = &settings.toggle_shortcut {
        crate::hotkey::check_voice(toggle, crate::hotkey::current_shortcut())?;
    }
    if let Some(path) = &voice.path {
        settings.save(path).map_err(|_| "Could not save the voice settings".to_owned())?;
    }
    let prev = std::mem::replace(&mut *voice.settings.lock().unwrap(), settings.clone());
    let was_on = prev.enabled;
    voice.controller.set_enabled(settings.enabled);
    #[cfg(target_os = "macos")]
    {
        use super::permissions;
        use super::settings::HoldKey;
        // Spec §6.4: ask when voice is turned on.
        if settings.enabled && !was_on {
            for p in permissions::to_request(settings.hold_key != HoldKey::Off, permissions::status) {
                permissions::request(p);
            }
        }
        *voice.trigger_error.lock().unwrap() = super::trigger::apply(&app, &settings, &prev);
        super::emit_status(&app);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = was_on;
    if let Err(error) = app.emit("voice://settings", &settings) {
        tracing::error!(%error, "could not emit the voice settings");
    }
    Ok(settings)
}

#[tauri::command]
pub fn voice_publish_presentation(voice: State<'_, VoiceState>, presentation: Presentation) {
    voice.controller.publish(presentation);
}

/// The step-1 trigger: typed text routed as a transcript. Errors are
/// static strings; the text never appears in one.
#[tauri::command]
pub async fn voice_text_input(voice: State<'_, VoiceState>, text: String) -> Result<(), String> {
    let controller = voice.controller.clone();
    controller
        .text_input(text, crate::panel::is_visible())
        .await
        .map_err(|e| e.to_string())
}

/// The four HUD commands below ignore calls from any window but the HUD.
/// The HUD rendered proposal `proposal_id` (D22).
#[tauri::command]
pub fn voice_shown(window: WebviewWindow, voice: State<'_, VoiceState>, proposal_id: u64) {
    if !hud::is_hud(window.label()) {
        tracing::debug!(label = window.label(), "ignored a HUD command from another window");
        return;
    }
    voice.controller.shown(proposal_id);
}

#[tauri::command]
pub fn voice_confirm(window: WebviewWindow, voice: State<'_, VoiceState>, proposal_id: u64) {
    if !hud::is_hud(window.label()) {
        tracing::debug!(label = window.label(), "ignored a HUD command from another window");
        return;
    }
    voice.controller.confirm(proposal_id);
}

#[tauri::command]
pub fn voice_choose(window: WebviewWindow, voice: State<'_, VoiceState>, choice_id: u64, index: usize) {
    if !hud::is_hud(window.label()) {
        tracing::debug!(label = window.label(), "ignored a HUD command from another window");
        return;
    }
    voice.controller.choose(choice_id, index);
}

/// Esc or Cancel in the HUD. `id` is the `id` of the `voice://hud` event the
/// frontend is showing. A HUD state without an id (Sending, Done, Error,
/// Speaking) has nothing to cancel, so the frontend sends nothing; a missing
/// id is ignored here too and is never turned into a cancel-whatever-is-current.
#[tauri::command]
pub fn voice_cancel(window: WebviewWindow, voice: State<'_, VoiceState>, id: Option<u64>) {
    if !hud::is_hud(window.label()) {
        tracing::debug!(label = window.label(), "ignored a HUD command from another window");
        return;
    }
    if let Some(id) = id {
        voice.controller.cancel(Some(id));
    }
}

/// The HUD measured the content of state `seq`: fit the panel to it.
#[tauri::command]
pub fn voice_hud_fit(window: WebviewWindow, seq: u64, height: f64) {
    if !hud::is_hud(window.label()) {
        tracing::debug!(label = window.label(), "ignored a HUD command from another window");
        return;
    }
    let app = tauri::Manager::app_handle(&window).clone();
    if let Err(error) = window.run_on_main_thread(move || hud::fit_state(&app, seq, height)) {
        tracing::error!(%error, "could not dispatch the HUD resize");
    }
}

/// Spike probe step `step` (plan Task 1A, S20). Settings window only, and
/// only while voice and the debug setting are both on.
#[tauri::command]
pub fn voice_probe(window: WebviewWindow, voice: State<'_, VoiceState>, step: u32) {
    let allowed = {
        let s = voice.settings.lock().unwrap();
        s.enabled && s.debug_input
    };
    if window.label() != "settings" || !allowed {
        tracing::debug!(label = window.label(), allowed, "ignored a probe request");
        return;
    }
    #[cfg(target_os = "macos")]
    super::bridge::probe(step);
    #[cfg(not(target_os = "macos"))]
    let _ = step;
}
