//! The iOS command subset. Same IPC names and argument shapes as the Mac app,
//! so the shared Svelte UI calls them unchanged. There is no panel, tray,
//! hotkey, voice or notification here. The no-op and error commands answer
//! the calls the shared UI makes on start or on a button, for which iOS has no
//! equivalent.

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

#[tauri::command]
pub fn escape(state: State<'_, AppState>) -> bool {
    core::escape(&state.inbox)
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

/// No global hotkey on iOS. The error leaves the Svelte default in place.
#[tauri::command]
pub fn get_hotkey() -> Result<String, String> {
    Err("no hotkey on iOS".into())
}

#[tauri::command]
pub fn set_hotkey(hotkey: String) -> Result<(), String> {
    let _ = hotkey;
    Err("no hotkey on iOS".into())
}

/// No floating panel to hide.
#[tauri::command]
pub fn hide_panel() {}

/// No settings window. Machine pairing has no screen yet (see apps/ios/README.md).
#[tauri::command]
pub fn open_settings() {}
