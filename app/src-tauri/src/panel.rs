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
/// When the display configuration last changed.
static LAST_SCREEN_CHANGE: Mutex<Option<Instant>> = Mutex::new(None);
/// The frame `place()` asked for, and until when callbacks that match it are
/// ours. tao applies `set_size` and `set_position` through
/// `DispatchQueue::main().exec_async`, so `windowDidResize` and
/// `windowDidMove` fire after `place()` has returned (and possibly between
/// the two calls). A "currently placing" flag therefore cannot tell them from
/// the user dragging; comparing the live frame with the requested one can.
static EXPECTED: Mutex<Option<(Frame, Instant)>> = Mutex::new(None);
/// Backstop for how long after `place()` its own callbacks can still arrive
/// (a slow launch can deliver them late). Normally `EXPECTED` is cleared as
/// soon as a callback shows the full target.
const PLACE_SETTLE: Duration = Duration::from_secs(5);
/// When the user last moved or resized the panel, if that is not saved yet.
/// Only then is the frame written, so a frame refitted after a display went
/// away never replaces the remembered one. The time lets a save drop a change
/// that turns out to sit next to a display change (see `save_now`).
static USER_CHANGED: Mutex<Option<Instant>> = Mutex::new(None);

fn has_unsaved_change() -> bool {
    USER_CHANGED.lock().is_ok_and(|c| c.is_some())
}
/// Bumped on every move or resize; a save runs only if no newer change arrived.
static FRAME_GENERATION: AtomicU64 = AtomicU64::new(0);
/// The debounced save waits this long after the last change, a little more
/// than `SCREEN_CHANGE_WINDOW`, so a display change that follows a move within
/// that window is known by the time `save_now` judges it. Hide and quit save
/// at once and cannot wait; a display change reported after them is not seen.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(2_100);

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
        // The voice HUD took key for input on purpose: stay open (D17).
        if !crate::voice::hud::holds_key() {
            hide(&handle);
        }
        false
    });
    observe_screen_changes();
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
        // Refit from the remembered frame, so a panel remembered on a display
        // that is back returns there. The live frame (possibly moved by macOS)
        // is the fallback, and also wins while a user change is still unsaved.
        if let Some(window) = handle.get_webview_window("panel") {
            let saved = if has_unsaved_change() {
                None
            } else {
                frame_path(handle).and_then(|p| frame::load(&p))
            };
            place(&window, saved.or_else(|| live_frame(&window)));
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
pub(crate) fn screen_frames(window: &WebviewWindow) -> (Vec<Frame>, Vec<Frame>) {
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
pub(crate) fn fallback_screen(window: &WebviewWindow, full: &[Frame]) -> usize {
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
    let Some(live) = app.get_webview_window("panel").and_then(|w| live_frame(&w)) else {
        return;
    };
    let Ok(mut expected) = EXPECTED.lock() else {
        return;
    };
    let target = (*expected)
        .filter(|(_, until)| Instant::now() < *until)
        .map(|(target, _)| target);
    let since_screen_change = LAST_SCREEN_CHANGE
        .lock()
        .ok()
        .and_then(|t| *t)
        .map(|t| t.elapsed());
    match frame::classify(&live, target.as_ref(), since_screen_change) {
        frame::FrameChange::Placement { done } => {
            // Both queued operations have landed: nothing more is ours.
            if done {
                *expected = None;
            }
        }
        frame::FrameChange::User => {
            *expected = None;
            drop(expected);
            schedule_save(app);
        }
        // macOS refitting the panel after a display change: not remembered.
        frame::FrameChange::System => {}
    }
}

/// Records display configuration changes (a display added, removed or
/// rearranged) so the moves macOS makes in response are not remembered.
/// Main thread only; call once from `create`.
fn observe_screen_changes() {
    use block2::RcBlock;
    use std::ptr::NonNull;
    use tauri_nspanel::objc2_app_kit::NSApplicationDidChangeScreenParametersNotification;
    use tauri_nspanel::objc2_foundation::{NSNotification, NSNotificationCenter};

    let block = RcBlock::new(|_: NonNull<NSNotification>| {
        if let Ok(mut last) = LAST_SCREEN_CHANGE.lock() {
            *last = Some(Instant::now());
        }
    });
    // SAFETY: the name is the system constant; no queue means the block runs
    // on the posting (main) thread; the block only touches a static Mutex.
    // The returned observer token is not kept: the notification center
    // retains the registration until it is removed, which this app never does,
    // so dropping the token does not unregister the observer.
    let _observer = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(NSApplicationDidChangeScreenParametersNotification),
            None,
            None,
            &block,
        )
    };
}

/// Saves the frame shortly after the user stops dragging or resizing.
fn schedule_save(app: &AppHandle) {
    if let Ok(mut changed) = USER_CHANGED.lock() {
        *changed = Some(Instant::now());
    }
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
/// The change stays pending until a write succeeds, so a failure is retried
/// on the next save. A change within `SCREEN_CHANGE_WINDOW` of a display
/// change, before or after it, is discarded: macOS refitting windows around
/// a display change can look like a user move, and the display change may be
/// reported after the move it caused.
pub fn save_now(app: &AppHandle) {
    // A pending debounced save is now redundant.
    FRAME_GENERATION.fetch_add(1, Ordering::SeqCst);
    let Some(changed_at) = USER_CHANGED.lock().ok().and_then(|c| *c) else {
        return;
    };
    let screen_change = LAST_SCREEN_CHANGE.lock().ok().and_then(|t| *t);
    if frame::near_screen_change(changed_at, screen_change) {
        if let Ok(mut changed) = USER_CHANGED.lock() {
            // Keep a newer change that arrived meanwhile.
            if *changed == Some(changed_at) {
                *changed = None;
            }
        }
        return;
    }
    let (Some(path), Some(window)) = (frame_path(app), app.get_webview_window("panel")) else {
        return;
    };
    let Some(frame) = live_frame(&window) else {
        return;
    };
    match frame::save(&path, &frame) {
        Ok(()) => {
            if let Ok(mut changed) = USER_CHANGED.lock()
                && *changed == Some(changed_at)
            {
                *changed = None;
            }
        }
        Err(error) => tracing::warn!(%error, "could not save panel frame"),
    }
}
