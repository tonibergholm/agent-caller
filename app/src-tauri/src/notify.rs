use std::sync::atomic::{AtomicI32, Ordering};

use acd_client::inbox::InboxEvent;
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::broadcast;

static NEXT_NOTIFICATION_ID: AtomicI32 = AtomicI32::new(1);

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
                let id = NEXT_NOTIFICATION_ID.fetch_add(1, Ordering::Relaxed);
                let _ = app
                    .notification()
                    .builder()
                    .id(id)
                    .title(title)
                    .body(body)
                    .show();
            }
            Ok(_) => {}
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
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
