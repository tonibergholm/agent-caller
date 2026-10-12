use acd_client::inbox::InboxEvent;
use serde::Serialize;
use tauri::AppHandle;
use tokio::sync::broadcast;

/// What macOS allows for this app's notifications (the settings view shows it).
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Authorization {
    Granted,
    Denied,
    NotDetermined,
    /// Not a bundled app, or macOS did not answer in time.
    Unavailable,
}

/// Maps an `UNAuthorizationStatus` raw value: 0 not determined, 1 denied,
/// 2 authorized, 3 provisional, 4 ephemeral.
pub fn authorization_from_status(raw: isize) -> Authorization {
    match raw {
        0 => Authorization::NotDetermined,
        1 => Authorization::Denied,
        2..=4 => Authorization::Granted,
        _ => Authorization::Unavailable,
    }
}

/// How long the permission button waits for the user to answer the prompt.
const PROMPT_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Asks macOS for notification permission at startup. Does nothing in an
/// unbundled build. The answer is not awaited: startup must not block on it.
pub fn request_authorization() {
    let _ = platform::request();
}

/// The current state. Runs the blocking macOS query off the async workers.
#[tauri::command]
pub async fn notification_authorization() -> Authorization {
    tokio::task::spawn_blocking(platform::read)
        .await
        .unwrap_or(Authorization::Unavailable)
}

/// Asks macOS for permission (it prompts only while the state is not
/// determined) and reports the state once the user has answered. macOS
/// answers asynchronously, so the answer is awaited, not read straight away.
#[tauri::command]
pub async fn request_notification_authorization() -> Authorization {
    tokio::task::spawn_blocking(|| match platform::request() {
        Some(answer) => match answer.recv_timeout(PROMPT_WAIT) {
            Ok(true) => Authorization::Granted,
            // Denied, or no answer in time: the stored state is authoritative.
            _ => platform::read(),
        },
        None => Authorization::Unavailable,
    })
    .await
    .unwrap_or(Authorization::Unavailable)
}

pub fn text(e: &InboxEvent) -> (String, String) {
    let InboxEvent::RequestOpened {
        machine_name,
        agent_title,
        request_title,
        ..
    } = e;
    (
        format!("{agent_title} on {machine_name}"),
        request_title.clone(),
    )
}

pub async fn run(app: AppHandle, mut events: broadcast::Receiver<InboxEvent>) {
    loop {
        match events.recv().await {
            Ok(e) if !crate::panel::is_visible() => {
                let (title, body) = text(&e);
                // The macOS backend can block for up to 2 s waiting for
                // delivery, so keep it off the async workers.
                let app = app.clone();
                tokio::task::spawn_blocking(move || {
                    if let Err(error) = show(&app, &title, &body) {
                        tracing::error!(%error, "could not show notification");
                    }
                });
            }
            Ok(_) => {}
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[cfg(target_os = "macos")]
pub fn init(app: &AppHandle) -> Result<(), String> {
    // An unbundled dev build has no registered bundle id; post as Terminal,
    // as tauri-plugin-notification does in dev.
    let bundle = if tauri::is_dev() {
        "com.apple.Terminal"
    } else {
        app.config().identifier.as_str()
    };
    mac_notification_sys::set_application(bundle)
        .map_err(|error| format!("could not set notification application: {error}"))
}

#[cfg(not(target_os = "macos"))]
pub fn init(_app: &AppHandle) -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn show(_app: &AppHandle, title: &str, body: &str) -> Result<(), String> {
    let mut notification = mac_notification_sys::Notification::new();
    notification.title(title).message(body).asynchronous(true);
    notification
        .send()
        .map(|_| ())
        .map_err(|error| format!("macOS notification backend failed: {error}"))
}

#[cfg(not(target_os = "macos"))]
fn show(app: &AppHandle, title: &str, body: &str) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;

    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|error| error.to_string())
}

/// Permission through UNUserNotificationCenter. Delivery still goes through
/// `mac-notification-sys` above; macOS applies the same per-app setting to both.
#[cfg(target_os = "macos")]
mod platform {
    use super::{Authorization, authorization_from_status};
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSBundle, NSError};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNNotificationSettings, UNUserNotificationCenter,
    };
    use std::ptr::NonNull;
    use std::sync::mpsc;
    use std::time::Duration;

    /// `currentNotificationCenter` raises an Objective-C exception, which
    /// aborts the process, outside a bundle (`tauri dev`, unit-test binaries).
    /// Those builds report `Unavailable`. The check uses the bundle path, not
    /// `bundleIdentifier`: `set_application` swaps that method so it reports
    /// an id even without a bundle.
    fn bundled() -> bool {
        NSBundle::mainBundle().bundlePath().to_string().ends_with(".app")
    }

    /// Starts the request. The receiver gets macOS's `granted` answer when
    /// the prompt is closed; `None` when the build is not bundled. A failed
    /// request sends nothing, so the receiver times out.
    pub fn request() -> Option<mpsc::Receiver<bool>> {
        if !bundled() {
            return None;
        }
        let (tx, rx) = mpsc::channel();
        let options = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound;
        let block = RcBlock::new(move |granted: Bool, error: *mut NSError| {
            if error.is_null() {
                let _ = tx.send(granted.as_bool());
            } else {
                tracing::warn!("notification permission request failed");
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(options, &block);
        Some(rx)
    }

    pub fn read() -> Authorization {
        if !bundled() {
            return Authorization::Unavailable;
        }
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // SAFETY: the framework passes a settings object that stays valid
            // for the duration of this callback.
            let raw = unsafe { settings.as_ref() }.authorizationStatus().0;
            let _ = tx.send(raw);
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .getNotificationSettingsWithCompletionHandler(&block);
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(raw) => authorization_from_status(raw),
            Err(_) => Authorization::Unavailable,
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::Authorization;

    pub fn request() -> Option<std::sync::mpsc::Receiver<bool>> {
        None
    }

    pub fn read() -> Authorization {
        Authorization::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_status_values_map_to_the_settings_states() {
        assert_eq!(authorization_from_status(0), Authorization::NotDetermined);
        assert_eq!(authorization_from_status(1), Authorization::Denied);
        assert_eq!(authorization_from_status(2), Authorization::Granted);
        assert_eq!(authorization_from_status(3), Authorization::Granted);
        assert_eq!(authorization_from_status(4), Authorization::Granted);
        assert_eq!(authorization_from_status(9), Authorization::Unavailable);
    }

    #[test]
    fn an_unbundled_test_process_reports_unavailable_without_asking_macos() {
        // Asking UNUserNotificationCenter here would abort the test binary.
        assert_eq!(platform::read(), Authorization::Unavailable);
        assert!(platform::request().is_none());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_unbundled_guard_holds_after_notify_init_installs_its_hook() {
        // `notify::init` makes `bundleIdentifier` report an id even when
        // unbundled; a guard that read that method would ask macOS and abort.
        let _ = mac_notification_sys::set_application("com.apple.Terminal");
        assert_eq!(platform::read(), Authorization::Unavailable);
        assert!(platform::request().is_none());
    }

    #[test]
    fn notification_text_never_includes_detail() {
        let e = InboxEvent::RequestOpened {
            machine_key: "m".into(),
            machine_name: "devbox".into(),
            agent_title: "api refactor".into(),
            request_title: "Bash".into(),
            request_id: "r".into(),
        };
        assert_eq!(
            text(&e),
            ("api refactor on devbox".to_string(), "Bash".to_string())
        );
    }
}
