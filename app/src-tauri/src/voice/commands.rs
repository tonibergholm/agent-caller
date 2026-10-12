use acd_voice::snapshot::Presentation;
use tauri::{AppHandle, Emitter, State, WebviewWindow};

use super::VoiceState;
use super::hud;
use super::settings::VoiceSettings;

#[tauri::command]
pub fn voice_get_settings(voice: State<'_, VoiceState>) -> VoiceSettings {
    *voice.settings.lock().unwrap()
}

#[tauri::command]
pub fn voice_set_settings(
    app: AppHandle,
    voice: State<'_, VoiceState>,
    enabled: bool,
    debug_input: bool,
) -> Result<VoiceSettings, String> {
    let next = VoiceSettings { enabled, debug_input };
    if let Some(path) = &voice.path {
        next.save(path).map_err(|_| "Could not save the voice settings".to_owned())?;
    }
    *voice.settings.lock().unwrap() = next;
    voice.controller.set_enabled(enabled);
    if let Err(error) = app.emit("voice://settings", next) {
        tracing::error!(%error, "could not emit the voice settings");
    }
    Ok(next)
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
