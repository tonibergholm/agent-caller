use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub fn open(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(window) = handle.get_webview_window("settings") {
            let _ = window.set_focus();
            return;
        }

        let _ =
            WebviewWindowBuilder::new(&handle, "settings", WebviewUrl::App("index.html".into()))
                .title("Agent Caller settings")
                .inner_size(520.0, 560.0)
                .build();
    });
}
