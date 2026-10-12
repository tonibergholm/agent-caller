use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Brings an open Settings window back to the front. The app has no Dock
/// icon, so when a system permission dialog closes, macOS returns focus to
/// the previous app and Settings would be left buried behind it.
pub fn raise(app: &AppHandle) {
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window("settings") else {
            return;
        };
        if !window.is_visible().unwrap_or(false) {
            return;
        }
        if let Err(error) = window.set_focus() {
            tracing::warn!(%error, "could not bring the settings window back");
        }
    }) {
        tracing::error!(%error, "could not dispatch settings raise to main thread");
    }
}

pub fn open(app: &AppHandle) {
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || {
        if let Some(window) = handle.get_webview_window("settings") {
            if let Err(error) = window.set_focus() {
                tracing::warn!(%error, "could not focus settings window");
            }
            return;
        }

        if let Err(error) =
            WebviewWindowBuilder::new(&handle, "settings", WebviewUrl::App("index.html".into()))
                .title("Agent Caller settings")
                .inner_size(520.0, 560.0)
                .build()
        {
            tracing::error!(%error, "could not build settings window");
        }
    }) {
        tracing::error!(%error, "could not dispatch settings window to main thread");
    }
}
