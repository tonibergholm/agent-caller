use acd_client::inbox::InboxEvent;
use tauri::AppHandle;
use tokio::sync::broadcast;

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

#[cfg(test)]
mod tests {
    use super::*;

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
