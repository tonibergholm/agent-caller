//! Menu bar icon: one dot colored by the most urgent state, with the
//! number of needs_you agents as the title (spec §8).
use acd_client::view::{TrayColor, TraySummary};
use tauri::AppHandle;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub const SIZE: u32 = 36;

fn rgba(color: TrayColor) -> [u8; 4] {
    match color {
        TrayColor::Amber => [245, 166, 35, 255],
        TrayColor::Blue => [66, 133, 244, 255],
        TrayColor::Green => [52, 168, 83, 255],
        TrayColor::Grey => [142, 142, 147, 255],
    }
}

pub fn summary_icon(color: TrayColor) -> Vec<u8> {
    let c = rgba(color);
    let (mid, r) = (SIZE as f32 / 2.0, SIZE as f32 * 0.32);
    let mut out = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 + 0.5 - mid, y as f32 + 0.5 - mid);
            if dx * dx + dy * dy <= r * r {
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
        .icon(Image::new_owned(summary_icon(TrayColor::Grey), SIZE, SIZE))
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
    let (handle, summary) = (app.clone(), summary.clone());
    let _ = app.run_on_main_thread(move || {
        if let Some(tray) = handle.tray_by_id("main") {
            let _ = tray.set_icon(Some(Image::new_owned(
                summary_icon(summary.color),
                SIZE,
                SIZE,
            )));
            let _ = tray.set_title(title(summary.needs_you));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_a_filled_dot_in_the_state_color() {
        let px = summary_icon(TrayColor::Amber);
        assert_eq!(px.len(), (SIZE * SIZE * 4) as usize);
        let center = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
        assert_eq!(&px[center..center + 4], &[245, 166, 35, 255]);
        assert_eq!(&px[0..4], &[0, 0, 0, 0], "corner is transparent");
        assert_ne!(
            summary_icon(TrayColor::Blue),
            summary_icon(TrayColor::Green)
        );
    }

    #[test]
    fn title_shows_count_only_when_needed() {
        assert_eq!(title(0), None);
        assert_eq!(title(3).as_deref(), Some("3"));
    }
}
