//! The non-activating floating panel (spec §8). It takes key focus, so J/K/Esc
//! and the reply field work, without activating the app: the frontmost app stays frontmost.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Size, WebviewUrl, WebviewWindow,
};
use tauri_nspanel::{
    CollectionBehavior, ManagerExt, PanelBuilder, PanelLevel, StyleMask, tauri_panel,
};

use crate::frame::{self, DEFAULT_SIZE, Frame, MIN_SIZE};

static VISIBLE: AtomicBool = AtomicBool::new(false);
/// The frame `place()` asked for, and until when callbacks that match it are
/// ours. tao applies `set_size` and `set_position` through
/// `DispatchQueue::main().exec_async`, so `windowDidResize` and
/// `windowDidMove` fire after `place()` has returned (and possibly between
/// the two calls). A "currently placing" flag therefore cannot tell them from
/// the user dragging; comparing the live frame with the requested one can.
static EXPECTED: Mutex<Option<(Frame, Instant)>> = Mutex::new(None);
/// How long after `place()` its own callbacks can still arrive.
const PLACE_SETTLE: Duration = Duration::from_millis(1000);
/// The user moved or resized the panel since the last save. Only then is the
/// frame written, so a frame refitted after a display went away never
/// replaces the remembered one.
static USER_CHANGED: AtomicBool = AtomicBool::new(false);
/// Bumped on every move or resize; a save runs only if no newer change arrived.
static FRAME_GENERATION: AtomicU64 = AtomicU64::new(0);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

// The macro requires an explicit return type; an alias keeps clippy's
// `unused_unit` quiet without an allow.
type Unit = ();

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
        window_did_resize(notification: &NSNotification) -> Unit,
        window_did_move(notification: &NSNotification) -> Unit
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
    events.window_did_resize(move |_| on_frame_event(&handle));
    let handle = app.clone();
    events.window_did_move(move |_| on_frame_event(&handle));
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

fn logical_frame(pos: tauri::PhysicalPosition<i32>, size: tauri::PhysicalSize<u32>, scale: f64) -> Frame {
    let pos = pos.to_logical::<f64>(scale);
    let size = size.to_logical::<f64>(scale);
    Frame {
        x: pos.x,
        y: pos.y,
        w: size.width,
        h: size.height,
    }
}

/// Every display in logical points, as (full frame, usable area with menu
/// bar and Dock excluded), in the same order.
fn screen_frames(window: &WebviewWindow) -> (Vec<Frame>, Vec<Frame>) {
    window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let scale = m.scale_factor();
            let area = m.work_area();
            (
                logical_frame(*m.position(), *m.size(), scale),
                logical_frame(area.position, area.size, scale),
            )
        })
        .unzip()
}

/// Index (into `full`) of the screen under the cursor, else the primary, else 0.
fn fallback_screen(window: &WebviewWindow, full: &[Frame]) -> usize {
    let primary = window.primary_monitor().ok().flatten();
    let primary_index = primary.as_ref().and_then(|p| {
        let f = logical_frame(*p.position(), *p.size(), p.scale_factor());
        full.iter().position(|s| *s == f)
    });
    // The cursor is physical, scaled by the primary display's factor.
    let under_cursor = primary.as_ref().zip(window.cursor_position().ok()).and_then(|(p, c)| {
        let point = frame::cursor_logical((c.x, c.y), p.scale_factor());
        frame::screen_at(point, full)
    });
    under_cursor.or(primary_index).unwrap_or(0)
}

/// Applies the remembered (or current) frame, fitted to the displays that exist now.
fn place(window: &WebviewWindow, wanted: Option<Frame>) {
    let (full, screens) = screen_frames(window);
    let fallback = fallback_screen(window, &full);
    let Some(target) = frame::resolve(wanted, &screens, fallback) else {
        return;
    };
    if live_frame(window) == Some(target) {
        return;
    }
    if let Ok(mut expected) = EXPECTED.lock() {
        *expected = Some((target, Instant::now() + PLACE_SETTLE));
    }
    if let Err(error) = window.set_size(LogicalSize::new(target.w, target.h)) {
        tracing::warn!(%error, "could not size inbox panel");
    }
    if let Err(error) = window.set_position(LogicalPosition::new(target.x, target.y)) {
        tracing::warn!(%error, "could not position inbox panel");
    }
}

/// Delegate callback for a move or resize (main thread). Ignores the ones
/// `place()` caused; anything else is the user.
fn on_frame_event(app: &AppHandle) {
    let live = app.get_webview_window("panel").and_then(|w| live_frame(&w));
    if let (Some(live), Ok(mut expected)) = (live, EXPECTED.lock()) {
        match *expected {
            Some((target, until)) if Instant::now() < until && frame::is_programmatic(&live, &target) => {
                return;
            }
            _ => *expected = None,
        }
    }
    schedule_save(app);
}

/// Saves the frame shortly after the user stops dragging or resizing.
fn schedule_save(app: &AppHandle) {
    USER_CHANGED.store(true, Ordering::SeqCst);
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

/// Writes the current frame if the user changed it. Main thread only (it reads the window).
pub fn save_now(app: &AppHandle) {
    // A pending debounced save is now redundant.
    FRAME_GENERATION.fetch_add(1, Ordering::SeqCst);
    if !USER_CHANGED.swap(false, Ordering::SeqCst) {
        return;
    }
    let (Some(path), Some(window)) = (frame_path(app), app.get_webview_window("panel")) else {
        return;
    };
    if let Some(frame) = live_frame(&window)
        && let Err(error) = frame::save(&path, &frame)
    {
        tracing::warn!(%error, "could not save panel frame");
    }
}
