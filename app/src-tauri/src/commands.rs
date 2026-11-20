use acd_app_core::commands::{self as core, AppState};
use acd_app_core::pairing::PairingPreview;
use acd_client::view::{InboxView, MachineEntryView};
use tauri::{AppHandle, Runtime, State, WebviewWindow};

// The windows that call each inbox-changing command (security review F7).
// The HUD shows peer text and calls only the `voice_*` HUD commands.
const PANEL: &[&str] = &["panel"];
const SETTINGS: &[&str] = &["settings"];
const PANEL_OR_SETTINGS: &[&str] = &["panel", "settings"];

fn allowed<R: Runtime>(window: &WebviewWindow<R>, labels: &[&str]) -> bool {
    let ok = labels.contains(&window.label());
    if !ok {
        tracing::debug!(label = window.label(), "ignored an inbox command from another window");
    }
    ok
}

#[tauri::command]
pub fn get_view(state: State<'_, AppState>) -> InboxView {
    core::get_view(&state.inbox)
}

#[tauri::command]
pub fn pick_option<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, AppState>,
    card_key: String,
    request_id: String,
    option_id: String,
    revision: u64,
) -> Result<String, String> {
    if !allowed(&window, PANEL) {
        return Err("Not available from this window.".into());
    }
    core::pick_option(&state.inbox, &card_key, &request_id, &option_id, revision)
}

#[tauri::command]
pub fn send_reply<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, AppState>,
    card_key: String,
    text: String,
) -> Result<String, String> {
    if !allowed(&window, PANEL) {
        return Err("Not available from this window.".into());
    }
    core::send_reply(&state.inbox, &card_key, text)
}

#[tauri::command]
pub fn answer_text<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, AppState>,
    card_key: String,
    request_id: String,
    revision: u64,
    text: String,
) -> Result<String, String> {
    if !allowed(&window, PANEL) {
        return Err("Not available from this window.".into());
    }
    core::answer_text(&state.inbox, &card_key, &request_id, revision, text)
}

#[tauri::command]
pub fn cancel_action<R: Runtime>(window: WebviewWindow<R>, state: State<'_, AppState>, action_id: String) -> bool {
    if !allowed(&window, PANEL) {
        return false;
    }
    core::cancel_action(&state.inbox, &action_id)
}

/// Esc in the inbox panel stops speech and also runs the usual chain, so
/// one Esc never has to choose between them (plan V12).
fn escape_all(stop_speech: impl FnOnce() -> bool, rest: impl FnOnce() -> bool) -> bool {
    let stopped = stop_speech();
    rest() || stopped
}

/// Esc in the inbox panel: stops speech, cancels a recording (plan S25),
/// else undoes the newest countdown, else cancels a voice step that is
/// still thinking. False when none ran.
#[tauri::command]
pub fn escape<R: Runtime>(window: WebviewWindow<R>, state: State<'_, AppState>, voice: State<'_, crate::voice::VoiceState>) -> bool {
    if !allowed(&window, PANEL) {
        return false;
    }
    escape_all(
        || voice.controller.stop_speaking(None),
        || voice.controller.escape_listening() || core::escape(&state.inbox) || voice.controller.cancel_thinking(),
    )
}

#[tauri::command]
pub fn mark_seen<R: Runtime>(window: WebviewWindow<R>, state: State<'_, AppState>, card_key: String) -> Result<(), String> {
    if !allowed(&window, PANEL) {
        return Err("Not available from this window.".into());
    }
    core::mark_seen(&state.inbox, tauri::async_runtime::handle().inner(), &card_key)
}

#[tauri::command]
pub fn set_show_idle<R: Runtime>(window: WebviewWindow<R>, state: State<'_, AppState>, on: bool) {
    if !allowed(&window, PANEL_OR_SETTINGS) {
        return;
    }
    core::set_show_idle(&state.inbox, on);
}

#[tauri::command]
pub fn set_show_subagents<R: Runtime>(window: WebviewWindow<R>, state: State<'_, AppState>, on: bool) -> Result<(), String> {
    if !allowed(&window, SETTINGS) {
        return Err("Not available from this window.".into());
    }
    core::set_show_subagents(&state.inbox, on)
}

#[tauri::command]
pub fn set_show_hook_only<R: Runtime>(window: WebviewWindow<R>, state: State<'_, AppState>, on: bool) -> Result<(), String> {
    if !allowed(&window, SETTINGS) {
        return Err("Not available from this window.".into());
    }
    core::set_show_hook_only(&state.inbox, on)
}

#[tauri::command]
pub fn list_machines(state: State<'_, AppState>) -> Vec<MachineEntryView> {
    core::list_machines(&state.inbox)
}

/// Reads the clipboard and says what it would pair; saves nothing (F9).
#[tauri::command]
pub fn preview_pairing_from_clipboard<R: Runtime>(
    window: WebviewWindow<R>,
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<PairingPreview, String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    if !allowed(&window, SETTINGS) {
        return Err("Not available from this window.".into());
    }
    state.pairing.preview(app.clipboard().read_text().ok().as_deref())
}

/// Pairs the clipboard line, but only when it is still the one `shown`.
#[tauri::command]
pub fn import_pairing_from_clipboard<R: Runtime>(
    window: WebviewWindow<R>,
    app: AppHandle<R>,
    state: State<'_, AppState>,
    shown: PairingPreview,
) -> Result<MachineEntryView, String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    if !allowed(&window, SETTINGS) {
        return Err("Not available from this window.".into());
    }
    acd_app_core::pairing::import_previewed_and_clear(
        &state.inbox,
        tauri::async_runtime::handle().inner(),
        app.clipboard().read_text().ok(),
        &shown,
        &state.pairing,
        || {
            let _ = app.clipboard().clear();
        },
    )
}

#[tauri::command]
pub fn rename_machine<R: Runtime>(window: WebviewWindow<R>, state: State<'_, AppState>, key: String, name: String) -> Result<(), String> {
    if !allowed(&window, SETTINGS) {
        return Err("Not available from this window.".into());
    }
    core::rename_machine(&state.inbox, &key, &name)
}

#[tauri::command]
pub fn remove_machine<R: Runtime>(window: WebviewWindow<R>, state: State<'_, AppState>, key: String) -> Result<(), String> {
    if !allowed(&window, SETTINGS) {
        return Err("Not available from this window.".into());
    }
    core::remove_machine(&state.inbox, &key)
}

#[tauri::command]
pub fn retry_machine<R: Runtime>(window: WebviewWindow<R>, state: State<'_, AppState>, key: String) {
    if !allowed(&window, SETTINGS) {
        return;
    }
    core::retry_machine(&state.inbox, &key);
}

#[tauri::command]
pub fn get_hotkey(state: State<'_, AppState>) -> String {
    state.inbox.settings().hotkey
}

#[tauri::command]
pub fn set_hotkey(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, AppState>,
    hotkey: String,
    voice: State<'_, crate::voice::VoiceState>,
) -> Result<(), String> {
    if !allowed(&window, SETTINGS) {
        return Err("Not available from this window.".into());
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hud_may_call_no_inbox_command() {
        for labels in [PANEL, SETTINGS, PANEL_OR_SETTINGS] {
            assert!(!labels.contains(&crate::voice::hud::LABEL));
        }
    }

    // The allow-lists above only say who may call. These send real IPC from a
    // mock-runtime webview, so a handler that loses its `allowed` check fails.
    mod window_scope {
        use super::*;
        use serde_json::{Value, json};
        use tauri::ipc::{CallbackFn, InvokeBody};
        use tauri::test::{INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets};
        use tauri::{Manager, WebviewUrl, WebviewWindowBuilder, webview::InvokeRequest};

        const REFUSED: &str = "Not available from this window.";

        struct Harness {
            app: tauri::App<MockRuntime>,
            _runtime: tokio::runtime::Runtime,
        }

        fn harness() -> Harness {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("acd-window-scope-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let inbox = acd_app_core::setup::open_inbox(
                &dir.join("settings.json"),
                std::sync::Arc::new(acd_client::settings::MemorySecrets::default()),
                runtime.handle(),
                acd_app_core::setup::OpenOptions { client_name: "test", client_version: "0", local_machine: false },
            )
            .unwrap();
            let app = mock_builder()
                .invoke_handler(tauri::generate_handler![
                    pick_option,
                    send_reply,
                    remove_machine,
                    import_pairing_from_clipboard,
                ])
                .build(mock_context(noop_assets()))
                .unwrap();
            app.manage(AppState { inbox, pairing: Default::default() });
            Harness { app, _runtime: runtime }
        }

        fn call(h: &Harness, label: &str, cmd: &str, args: Value) -> Result<Value, Value> {
            let window = h.app.get_webview_window(label).unwrap_or_else(|| {
                WebviewWindowBuilder::new(&h.app, label, WebviewUrl::default()).build().unwrap()
            });
            get_ipc_response(
                &window,
                InvokeRequest {
                    cmd: cmd.into(),
                    callback: CallbackFn(0),
                    error: CallbackFn(1),
                    url: "tauri://localhost".parse().unwrap(),
                    body: InvokeBody::Json(args),
                    headers: Default::default(),
                    invoke_key: INVOKE_KEY.to_string(),
                },
            )
            .map(|body| body.deserialize().unwrap())
        }

        fn commands() -> [(&'static str, Value); 4] {
            [
                ("pick_option", json!({"cardKey": "c", "requestId": "r", "optionId": "o", "revision": 1})),
                ("send_reply", json!({"cardKey": "c", "text": "hi"})),
                ("remove_machine", json!({"key": "nope"})),
                ("import_pairing_from_clipboard", json!({"shown": {"host": "h", "port": 1, "pin": "p"}})),
            ]
        }

        #[test]
        fn the_hud_window_is_refused_by_every_inbox_command() {
            let h = harness();
            for (cmd, args) in commands() {
                let got = call(&h, crate::voice::hud::LABEL, cmd, args);
                assert_eq!(got, Err(json!(REFUSED)), "{cmd} from the HUD");
            }
        }

        #[test]
        fn an_allowed_window_gets_past_the_window_check() {
            let h = harness();
            // Settings may remove a machine: it fails on the unknown key, not the window.
            let got = call(&h, "settings", "remove_machine", json!({"key": "nope"}));
            assert_ne!(got, Err(json!(REFUSED)), "settings reached the handler");
            // The panel may not.
            let got = call(&h, "panel", "remove_machine", json!({"key": "nope"}));
            assert_eq!(got, Err(json!(REFUSED)));
        }
    }

    /// Plan V12: one Esc stops speech and still cancels an inbox countdown.
    #[test]
    fn escape_stops_speech_and_still_runs_the_rest() {
        let mut ran = false;
        assert!(escape_all(|| true, || { ran = true; false }));
        assert!(ran, "the inbox chain still ran");
        assert!(escape_all(|| false, || true));
        assert!(!escape_all(|| false, || false));
    }
}
