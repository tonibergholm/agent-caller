//! Menu bar icon: one dot colored by the most urgent state, with the
//! number of needs_you agents as the title (spec §8).
use acd_client::view::{TrayColor, TraySummary};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::AppHandle;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub const SIZE: u32 = 36;

/// The live-microphone dot (spec §6.2, plan S18).
pub const MIC: [u8; 4] = [255, 59, 48, 255];

/// The last summary, so the microphone dot and the summary are always
/// drawn together and a view update never erases the dot.
static LAST: Mutex<Option<TraySummary>> = Mutex::new(None);
static LISTENING: AtomicBool = AtomicBool::new(false);

fn rgba(color: TrayColor) -> [u8; 4] {
    match color {
        TrayColor::Amber => [245, 166, 35, 255],
        TrayColor::Blue => [66, 133, 244, 255],
        TrayColor::Green => [52, 168, 83, 255],
        TrayColor::Grey => [142, 142, 147, 255],
    }
}

pub fn summary_icon(color: TrayColor, listening: bool) -> Vec<u8> {
    let c = rgba(color);
    let (mid, r) = (SIZE as f32 / 2.0, SIZE as f32 * 0.32);
    let (mic_x, mic_y, mic_r) = (SIZE as f32 * 0.82, SIZE as f32 * 0.18, SIZE as f32 * 0.16);
    let mut out = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let (dx, dy) = (px - mid, py - mid);
            let (mx, my) = (px - mic_x, py - mic_y);
            if listening && mx * mx + my * my <= mic_r * mic_r {
                out.extend_from_slice(&MIC);
            } else if dx * dx + dy * dy <= r * r {
                out.extend_from_slice(&c);
            } else {
                out.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    out
}

pub fn title(needs_you: u32) -> Option<String> {
    (needs_you > 0).then(|| needs_you.to_string())
}

/// Call from setup (main thread).
pub fn build(app: &AppHandle) -> tauri::Result<TrayIcon> {
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Agent Caller", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&settings, &quit])?;
    TrayIconBuilder::with_id("main")
        .icon(Image::new_owned(summary_icon(TrayColor::Grey, false), SIZE, SIZE))
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id().as_ref() {
            "settings" => crate::settings_window::open(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                crate::panel::toggle(tray.app_handle());
            }
        })
        .build(app)
}

pub fn update(app: &AppHandle, summary: &TraySummary) {
    *LAST.lock().unwrap() = Some(summary.clone());
    render(app);
}

/// The microphone went on or off (spec §6.2).
pub fn set_listening(app: &AppHandle, on: bool) {
    LISTENING.store(on, Ordering::SeqCst);
    render(app);
}

fn render(app: &AppHandle) {
    let summary = LAST.lock().unwrap().clone().unwrap_or(TraySummary {
        color: TrayColor::Grey,
        needs_you: 0,
    });
    let listening = LISTENING.load(Ordering::SeqCst);
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || {
        if let Some(tray) = handle.tray_by_id("main") {
            if let Err(error) = tray.set_icon(Some(Image::new_owned(
                summary_icon(summary.color, listening),
                SIZE,
                SIZE,
            ))) {
                tracing::warn!(%error, "could not update tray icon");
            }
            if let Err(error) = tray.set_title(title(summary.needs_you)) {
                tracing::warn!(%error, "could not update tray title");
            }
        }
    }) {
        tracing::error!(%error, "could not dispatch tray update to main thread");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_a_filled_dot_in_the_state_color() {
        let px = summary_icon(TrayColor::Amber, false);
        assert_eq!(px.len(), (SIZE * SIZE * 4) as usize);
        let center = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
        assert_eq!(&px[center..center + 4], &[245, 166, 35, 255]);
        assert_eq!(&px[0..4], &[0, 0, 0, 0], "corner is transparent");
        assert_ne!(
            summary_icon(TrayColor::Blue, false),
            summary_icon(TrayColor::Green, false)
        );
    }

    /// Spec §6.2: a live microphone indicator in the menu bar icon (S18).
    #[test]
    fn the_microphone_dot_shows_only_while_listening() {
        let at = |px: &[u8], x: u32, y: u32| {
            let i = ((y * SIZE + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        let (corner_x, corner_y) = ((SIZE as f32 * 0.82) as u32, (SIZE as f32 * 0.18) as u32);
        let on = summary_icon(TrayColor::Amber, true);
        let off = summary_icon(TrayColor::Amber, false);
        assert_eq!(at(&on, corner_x, corner_y), MIC);
        assert_eq!(at(&off, corner_x, corner_y), [0, 0, 0, 0]);
        assert_eq!(at(&on, SIZE / 2, SIZE / 2), [245, 166, 35, 255], "the state colour stays");
    }

    #[test]
    fn title_shows_count_only_when_needed() {
        assert_eq!(title(0), None);
        assert_eq!(title(3).as_deref(), Some("3"));
    }
}
