//! The command set both apps expose, as plain functions over the inbox. Each
//! shell wraps these in `#[tauri::command]` functions that keep the IPC names
//! and argument shapes the Svelte UI calls. Functions that need the Tauri
//! runtime take a `tokio::runtime::Handle` instead.

use std::sync::Arc;

use acd_client::{
    inbox::Inbox,
    view::{InboxView, MachineEntryView},
};
use tokio::runtime::Handle;

/// Tauri managed state in both shells.
pub struct AppState {
    pub inbox: Arc<Inbox>,
}

pub fn get_view(inbox: &Arc<Inbox>) -> InboxView {
    inbox.view().borrow().clone()
}

pub fn pick_option(
    inbox: &Arc<Inbox>,
    card_key: &str,
    request_id: &str,
    option_id: &str,
    revision: u64,
) -> Result<String, String> {
    inbox
        .pick(card_key, request_id, option_id, revision)
        .map_err(|e| e.to_string())
}

pub fn send_reply(inbox: &Arc<Inbox>, card_key: &str, text: String) -> Result<String, String> {
    inbox.reply(card_key, text).map_err(|e| e.to_string())
}

pub fn answer_text(
    inbox: &Arc<Inbox>,
    card_key: &str,
    request_id: &str,
    revision: u64,
    text: String,
) -> Result<String, String> {
    inbox
        .answer_text(card_key, request_id, revision, text)
        .map_err(|e| e.to_string())
}

pub fn cancel_action(inbox: &Arc<Inbox>, action_id: &str) -> bool {
    inbox.cancel(action_id)
}

/// Undoes the newest countdown. The Mac app checks its voice controller first.
pub fn escape(inbox: &Arc<Inbox>) -> bool {
    inbox.escape()
}

pub fn mark_seen(inbox: &Arc<Inbox>, runtime: &Handle, card_key: &str) -> Result<(), String> {
    let _runtime_guard = runtime.enter();
    inbox.mark_seen(card_key).map_err(|e| e.to_string())
}

pub fn set_show_idle(inbox: &Arc<Inbox>, on: bool) {
    inbox.set_show_idle(on);
}

pub fn set_show_subagents(inbox: &Arc<Inbox>, on: bool) -> Result<(), String> {
    inbox.set_show_subagents(on)
}

pub fn set_show_hook_only(inbox: &Arc<Inbox>, on: bool) -> Result<(), String> {
    inbox.set_show_hook_only(on)
}

pub fn list_machines(inbox: &Arc<Inbox>) -> Vec<MachineEntryView> {
    inbox.machines()
}

pub fn rename_machine(inbox: &Arc<Inbox>, key: &str, name: &str) -> Result<(), String> {
    inbox.rename_machine(key, name)
}

pub fn remove_machine(inbox: &Arc<Inbox>, key: &str) -> Result<(), String> {
    inbox.remove_machine(key)
}

pub fn retry_machine(inbox: &Arc<Inbox>, key: &str) {
    inbox.retry_machine(key);
}
