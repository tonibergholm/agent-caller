//! The non-activating floating panel (spec §8). It takes key focus, so J/K/Esc
//! and the reply field work, without activating the app: the frontmost app stays frontmost.
// tauri_panel! requires an explicit return type, so `-> ()` is unavoidable.
#![allow(clippy::unused_unit)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Size, WebviewUrl, WebviewWindow,
};
use tauri_nspanel::{
    CollectionBehavior, ManagerExt, PanelBuilder, PanelLevel, StyleMask, tauri_panel,
};

use crate::frame::{self, DEFAULT_SIZE, Frame, MIN_SIZE};

static VISIBLE: AtomicBool = AtomicBool::new(false);
/// Bumped on every move or resize; a save runs only if no newer change arrived.
static FRAME_GENERATION: AtomicU64 = AtomicU64::new(0);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

tauri_panel! {
    panel!(InboxPanel {
        config: {
            can_become_key_window: true,
            can_become_main_window: false,
            is_floating_panel: true
        }
    })
    panel_event!(InboxPanelEvents {
        window_did_resign_key(notification: &NSNotification) -> bool,
        window_did_resize(notification: &NSNotification) -> (),
        window_did_move(notification: &NSNotification) -> ()
    })
}

/// Creates the panel directly because Tauri `setup` runs on the main thread.
/// This must only be called from `setup`.
pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let panel = PanelBuilder::<_, InboxPanel>::new(app, "panel")
        .url(WebviewUrl::App("index.html".into()))
        .title("Agent inbox")
        .size(Size::Logical(LogicalSize::new(DEFAULT_SIZE.0, DEFAULT_SIZE.1)))
        .min_size(Size::Logical(LogicalSize::new(MIN_SIZE.0, MIN_SIZE.1)))
        .resizable(true)
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
    let handle = app.clone();
    events.window_did_resize(move |_| schedule_save(&handle));
    let handle = app.clone();
    events.window_did_move(move |_| schedule_save(&handle));
    panel.set_event_handler(Some(events.as_ref()));
    // Not `panel.to_window()`: that converts the panel back into a plain
    // window and unregisters it, so later lookups fail.
    if let Some(window) = app.get_webview_window("panel") {
        let saved = frame_path(app).and_then(|p| frame::load(&p));
        place(&window, saved);
    }
    panel.hide();
    Ok(())
}

pub fn is_visible() -> bool {
    VISIBLE.load(Ordering::SeqCst)
}

pub fn toggle(app: &AppHandle) {
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || {
        if is_visible() {
            hide_on_main_thread(&handle);
        } else {
            show_on_main_thread(&handle);
        }
    }) {
        tracing::error!(%error, "could not dispatch panel toggle to main thread");
    }
}

#[allow(dead_code)] // Public thread-safe operation; toggle uses its main-thread implementation.
pub fn show(app: &AppHandle) {
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || show_on_main_thread(&handle)) {
        tracing::error!(%error, "could not dispatch panel show to main thread");
    }
}

pub fn hide(app: &AppHandle) {
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || hide_on_main_thread(&handle)) {
        tracing::error!(%error, "could not dispatch panel hide to main thread");
    }
}

fn show_on_main_thread(handle: &AppHandle) {
    // GATE (Task 13): remove in Task 16
    let found = handle.get_webview_panel("panel");
    if found.is_err() {
        // GATE (Task 13): remove in Task 16
        tracing::warn!(windows = ?handle.webview_windows().keys().collect::<Vec<_>>(), "show: panel not found");
    }
    if let Ok(panel) = found {
        // Re-fit the live frame first: a display may have gone away while hidden.
        if let Some(window) = handle.get_webview_window("panel") {
            place(&window, live_frame(&window));
        }
        panel.show_and_make_key();
        VISIBLE.store(true, Ordering::SeqCst);
        tracing::info!("panel shown");
        if let Err(error) = handle.emit("panel://shown", ()) {
            tracing::error!(%error, "could not emit panel shown event");
        }
    }
}

fn hide_on_main_thread(handle: &AppHandle) {
    if let Ok(panel) = handle.get_webview_panel("panel") {
        save_now(handle);
        panel.hide();
        VISIBLE.store(false, Ordering::SeqCst);
        tracing::info!("panel hidden");
    }
}

fn frame_path(app: &AppHandle) -> Option<PathBuf> {
    match app.path().app_config_dir() {
        Ok(dir) => Some(dir.join("panel-frame.json")),
        Err(error) => {
            tracing::warn!(%error, "no config dir; panel frame is not remembered");
            None
        }
    }
}

/// The window's frame in logical points.
fn live_frame(window: &WebviewWindow) -> Option<Frame> {
    let scale = window.scale_factor().ok()?;
    let pos = window.outer_position().ok()?.to_logical::<f64>(scale);
    let size = window.outer_size().ok()?.to_logical::<f64>(scale);
    Some(Frame {
        x: pos.x,
        y: pos.y,
        w: size.width,
        h: size.height,
    })
}

/// Usable area (menu bar and Dock excluded) of every display, in logical points.
fn screen_frames(window: &WebviewWindow) -> Vec<Frame> {
    window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let scale = m.scale_factor();
            let area = m.work_area();
            let pos = area.position.to_logical::<f64>(scale);
            let size = area.size.to_logical::<f64>(scale);
            Frame {
                x: pos.x,
                y: pos.y,
                w: size.width,
                h: size.height,
            }
        })
        .collect()
}

/// Index of the screen under the cursor, else the primary, else the first.
fn fallback_screen(window: &WebviewWindow, screens: &[Frame]) -> usize {
    let frame_of = |m: &tauri::Monitor| {
        let scale = m.scale_factor();
        let pos = m.work_area().position.to_logical::<f64>(scale);
        (pos.x, pos.y)
    };
    let under_cursor = window
        .cursor_position()
        .ok()
        .and_then(|c| window.monitor_from_point(c.x, c.y).ok().flatten());
    let monitor = under_cursor.or_else(|| window.primary_monitor().ok().flatten());
    monitor
        .map(|m| frame_of(&m))
        .and_then(|origin| screens.iter().position(|s| (s.x, s.y) == origin))
        .unwrap_or(0)
}

/// Applies the remembered (or current) frame, fitted to the displays that exist now.
fn place(window: &WebviewWindow, wanted: Option<Frame>) {
    let screens = screen_frames(window);
    let fallback = fallback_screen(window, &screens);
    let Some(target) = frame::resolve(wanted, &screens, fallback) else {
        return;
    };
    if live_frame(window) == Some(target) {
        return;
    }
    if let Err(error) = window.set_size(LogicalSize::new(target.w, target.h)) {
        tracing::warn!(%error, "could not size inbox panel");
    }
    if let Err(error) = window.set_position(LogicalPosition::new(target.x, target.y)) {
        tracing::warn!(%error, "could not position inbox panel");
    }
}

/// Saves the frame shortly after the user stops dragging or resizing.
fn schedule_save(app: &AppHandle) {
    let generation = FRAME_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(SAVE_DEBOUNCE).await;
        if FRAME_GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        let inner = handle.clone();
        if let Err(error) = handle.run_on_main_thread(move || save_now(&inner)) {
            tracing::warn!(%error, "could not dispatch panel frame save");
        }
    });
}

/// Writes the current frame. Main thread only (it reads the window).
fn save_now(app: &AppHandle) {
    // A pending debounced save is now redundant.
    FRAME_GENERATION.fetch_add(1, Ordering::SeqCst);
    let (Some(path), Some(window)) = (frame_path(app), app.get_webview_window("panel")) else {
        return;
    };
    if let Some(frame) = live_frame(&window)
        && let Err(error) = frame::save(&path, &frame)
    {
        tracing::warn!(%error, "could not save panel frame");
    }
}
