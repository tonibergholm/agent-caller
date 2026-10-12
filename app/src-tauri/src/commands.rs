use std::sync::Arc;

use acd_client::{
    inbox::Inbox,
    view::{InboxView, MachineEntryView},
};
use tauri::{AppHandle, State};

pub struct AppState {
    pub inbox: Arc<Inbox>,
}

#[tauri::command]
pub fn get_view(state: State<'_, AppState>) -> InboxView {
    state.inbox.view().borrow().clone()
}

#[tauri::command]
pub fn pick_option(
    state: State<'_, AppState>,
    card_key: String,
    request_id: String,
    option_id: String,
    revision: u64,
) -> Result<String, String> {
    state
        .inbox
        .pick(&card_key, &request_id, &option_id, revision)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn send_reply(
    state: State<'_, AppState>,
    card_key: String,
    text: String,
) -> Result<String, String> {
    state
        .inbox
        .reply(&card_key, text)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn answer_text(
    state: State<'_, AppState>,
    card_key: String,
    request_id: String,
    revision: u64,
    text: String,
) -> Result<String, String> {
    state
        .inbox
        .answer_text(&card_key, &request_id, revision, text)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cancel_action(state: State<'_, AppState>, action_id: String) -> bool {
    state.inbox.cancel(&action_id)
}

/// Esc in the inbox panel: undoes the newest countdown, else cancels a
/// voice step that is still thinking. False when neither ran.
#[tauri::command]
pub fn escape(state: State<'_, AppState>, voice: State<'_, crate::voice::VoiceState>) -> bool {
    state.inbox.escape() || voice.controller.cancel_thinking()
}

#[tauri::command]
pub fn mark_seen(state: State<'_, AppState>, card_key: String) -> Result<(), String> {
    let _runtime_guard = tauri::async_runtime::handle().inner().enter();
    state.inbox.mark_seen(&card_key).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_show_idle(state: State<'_, AppState>, on: bool) {
    state.inbox.set_show_idle(on);
}

#[tauri::command]
pub fn set_show_subagents(state: State<'_, AppState>, on: bool) -> Result<(), String> {
    state.inbox.set_show_subagents(on)
}

#[tauri::command]
pub fn set_show_hook_only(state: State<'_, AppState>, on: bool) -> Result<(), String> {
    state.inbox.set_show_hook_only(on)
}

#[tauri::command]
pub fn list_machines(state: State<'_, AppState>) -> Vec<MachineEntryView> {
    state.inbox.machines()
}

#[tauri::command]
pub fn import_pairing_from_clipboard(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<MachineEntryView, String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    crate::pairing::import_text(&state.inbox, app.clipboard().read_text().ok())
}

#[tauri::command]
pub fn rename_machine(state: State<'_, AppState>, key: String, name: String) -> Result<(), String> {
    state.inbox.rename_machine(&key, &name)
}

#[tauri::command]
pub fn remove_machine(state: State<'_, AppState>, key: String) -> Result<(), String> {
    state.inbox.remove_machine(&key)
}

#[tauri::command]
pub fn retry_machine(state: State<'_, AppState>, key: String) {
    state.inbox.retry_machine(&key);
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
) -> Result<(), String> {
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
