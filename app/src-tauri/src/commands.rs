use acd_app_core::commands::{self as core, AppState};
use acd_client::view::{InboxView, MachineEntryView};
use tauri::{AppHandle, State};

#[tauri::command]
pub fn get_view(state: State<'_, AppState>) -> InboxView {
    core::get_view(&state.inbox)
}

#[tauri::command]
pub fn pick_option(
    state: State<'_, AppState>,
    card_key: String,
    request_id: String,
    option_id: String,
    revision: u64,
) -> Result<String, String> {
    core::pick_option(&state.inbox, &card_key, &request_id, &option_id, revision)
}

#[tauri::command]
pub fn send_reply(
    state: State<'_, AppState>,
    card_key: String,
    text: String,
) -> Result<String, String> {
    core::send_reply(&state.inbox, &card_key, text)
}

#[tauri::command]
pub fn answer_text(
    state: State<'_, AppState>,
    card_key: String,
    request_id: String,
    revision: u64,
    text: String,
) -> Result<String, String> {
    core::answer_text(&state.inbox, &card_key, &request_id, revision, text)
}

#[tauri::command]
pub fn cancel_action(state: State<'_, AppState>, action_id: String) -> bool {
    core::cancel_action(&state.inbox, &action_id)
}

/// Esc in the inbox panel: cancels a recording first (plan S25), else
/// undoes the newest countdown, else cancels a voice step that is still
/// thinking. False when none ran.
#[tauri::command]
pub fn escape(state: State<'_, AppState>, voice: State<'_, crate::voice::VoiceState>) -> bool {
    voice.controller.escape_listening() || core::escape(&state.inbox) || voice.controller.cancel_thinking()
}

#[tauri::command]
pub fn mark_seen(state: State<'_, AppState>, card_key: String) -> Result<(), String> {
    core::mark_seen(&state.inbox, tauri::async_runtime::handle().inner(), &card_key)
}

#[tauri::command]
pub fn set_show_idle(state: State<'_, AppState>, on: bool) {
    core::set_show_idle(&state.inbox, on);
}

#[tauri::command]
pub fn set_show_subagents(state: State<'_, AppState>, on: bool) -> Result<(), String> {
    core::set_show_subagents(&state.inbox, on)
}

#[tauri::command]
pub fn set_show_hook_only(state: State<'_, AppState>, on: bool) -> Result<(), String> {
    core::set_show_hook_only(&state.inbox, on)
}

#[tauri::command]
pub fn list_machines(state: State<'_, AppState>) -> Vec<MachineEntryView> {
    core::list_machines(&state.inbox)
}

#[tauri::command]
pub fn import_pairing_from_clipboard(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<MachineEntryView, String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    acd_app_core::pairing::import_and_clear(
        &state.inbox,
        tauri::async_runtime::handle().inner(),
        app.clipboard().read_text().ok(),
        || {
            let _ = app.clipboard().clear();
        },
    )
}

#[tauri::command]
pub fn rename_machine(state: State<'_, AppState>, key: String, name: String) -> Result<(), String> {
    core::rename_machine(&state.inbox, &key, &name)
}

#[tauri::command]
pub fn remove_machine(state: State<'_, AppState>, key: String) -> Result<(), String> {
    core::remove_machine(&state.inbox, &key)
}

#[tauri::command]
pub fn retry_machine(state: State<'_, AppState>, key: String) {
    core::retry_machine(&state.inbox, &key);
}

#[tauri::command]
pub fn get_hotkey(state: State<'_, AppState>) -> String {
    state.inbox.settings().hotkey
}

#[tauri::command]
pub fn set_hotkey(
    app: AppHandle,
    state: State<'_, AppState>,
    hotkey: String,
    voice: State<'_, crate::voice::VoiceState>,
) -> Result<(), String> {
    let saved = voice.settings.lock().unwrap().toggle_shortcut.clone();
    if crate::hotkey::conflicts_with_voice(&hotkey, saved.as_deref()) {
        return Err("That is the voice shortcut. Choose another inbox hotkey.".into());
    }
    let previous = crate::hotkey::current_shortcut();
    crate::hotkey::apply(&app, &hotkey)?;
    if let Err(error) = state.inbox.set_hotkey(&hotkey) {
        if let Err(rollback_error) = crate::hotkey::restore(&app, previous) {
            tracing::error!(%rollback_error, "could not restore previous hotkey after settings save failed");
        }
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
pub fn hide_panel(_state: State<'_, AppState>, app: AppHandle) {
    crate::panel::hide(&app);
}

#[tauri::command]
pub fn open_settings(_state: State<'_, AppState>, app: AppHandle) {
    crate::settings_window::open(&app);
}
