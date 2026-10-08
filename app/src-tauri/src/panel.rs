//! The non-activating floating panel (spec §8). It takes key focus, so J/K/Esc
//! and the reply field work, without activating the app: the frontmost app stays frontmost.
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter, LogicalSize, Manager, Size, WebviewUrl};
use tauri_nspanel::{
    CollectionBehavior, ManagerExt, PanelBuilder, PanelLevel, StyleMask, tauri_panel,
};

static VISIBLE: AtomicBool = AtomicBool::new(false);

tauri_panel! {
    panel!(InboxPanel {
        config: {
            can_become_key_window: true,
            can_become_main_window: false,
            is_floating_panel: true
        }
    })
    panel_event!(InboxPanelEvents {
        window_did_resign_key(notification: &NSNotification) -> bool
    })
}

/// Creates the panel directly because Tauri `setup` runs on the main thread.
/// This must only be called from `setup`.
pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let panel = PanelBuilder::<_, InboxPanel>::new(app, "panel")
        .url(WebviewUrl::App("index.html".into()))
        .title("Agent inbox")
        .size(Size::Logical(LogicalSize::new(440.0, 560.0)))
        .level(PanelLevel::Floating)
        // Borderless comes from Tauri at creation; replacing the style mask
        // after the webview is attached makes AppKit throw (KVO observer).
        .with_window(|w| w.decorations(false))
        .add_style_mask(StyleMask::empty().nonactivating_panel())
        .collection_behavior(
            CollectionBehavior::new()
                .can_join_all_spaces()
                .full_screen_auxiliary(),
        )
        .hides_on_deactivate(false)
        .build()?;
    let events = InboxPanelEvents::new();
    let handle = app.clone();
    events.window_did_resign_key(move |_| {
        hide(&handle);
        false
    });
    panel.set_event_handler(Some(events.as_ref()));
    // Not `panel.to_window()`: that converts the panel back into a plain
    // window and unregisters it, so later lookups fail.
    if let Some(window) = app.get_webview_window("panel") {
        let _ = window.center();
    }
    panel.hide();
    Ok(())
}

pub fn is_visible() -> bool {
    VISIBLE.load(Ordering::SeqCst)
}

pub fn toggle(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if is_visible() {
            hide_on_main_thread(&handle);
        } else {
            show_on_main_thread(&handle);
        }
    });
}

#[allow(dead_code)] // Public thread-safe operation; toggle uses its main-thread implementation.
pub fn show(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || show_on_main_thread(&handle));
}

pub fn hide(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || hide_on_main_thread(&handle));
}

fn show_on_main_thread(handle: &AppHandle) {
    // GATE (Task 13): remove in Task 16
    let found = handle.get_webview_panel("panel");
    if found.is_err() {
        // GATE (Task 13): remove in Task 16
        tracing::warn!(windows = ?handle.webview_windows().keys().collect::<Vec<_>>(), "show: panel not found");
    }
    if let Ok(panel) = found {
        panel.show_and_make_key();
        VISIBLE.store(true, Ordering::SeqCst);
        tracing::info!("panel shown");
        let _ = handle.emit("panel://shown", ());
    }
}

fn hide_on_main_thread(handle: &AppHandle) {
    if let Ok(panel) = handle.get_webview_panel("panel") {
        panel.hide();
        VISIBLE.store(false, Ordering::SeqCst);
        tracing::info!("panel hidden");
    }
}
