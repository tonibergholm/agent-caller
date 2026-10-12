//! Notifications through `org.freedesktop.Notifications` (omarchy spec §8).
use std::collections::HashMap;

use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::{Connection, names::BusName, proxy, zvariant::Value};

use crate::notify::{ActionSignal, NotifyBackend, NotifyError, OPEN_ACTION};

pub const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
pub const APP_NAME: &str = "Agent Caller";

#[proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
}

/// The actions array: "Open" only when omarchy-shell was found (§4.2).
pub fn actions(open: bool) -> Vec<&'static str> {
    if open { vec![OPEN_ACTION, "Open"] } else { vec![] }
}

/// A class for logs; never the D-Bus message.
pub fn error_class(e: &zbus::Error) -> &'static str {
    match e {
        zbus::Error::MethodError(..) => "method_error",
        zbus::Error::FDO(_) => "fdo",
        zbus::Error::InputOutput(_) | zbus::Error::Connection(..) => "io",
        _ => "other",
    }
}

pub struct DbusNotify {
    conn: Connection,
    proxy: NotificationsProxy<'static>,
}

impl DbusNotify {
    /// Connects to the session bus and forwards every `ActionInvoked`
    /// signal with its sender; `Notifier::on_action` decides what to trust.
    pub async fn connect() -> zbus::Result<(DbusNotify, mpsc::Receiver<ActionSignal>)> {
        let conn = Connection::session().await?;
        let proxy = NotificationsProxy::new(&conn).await?;
        let mut stream = proxy.receive_action_invoked().await?;
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            while let Some(signal) = stream.next().await {
                let sender = signal.message().header().sender().map(|s| s.to_string()).unwrap_or_default();
                let Ok(args) = signal.args() else { continue };
                let action = ActionSignal { id: args.id, action: args.action_key.clone(), sender };
                if tx.send(action).await.is_err() {
                    return;
                }
            }
        });
        Ok((DbusNotify { conn, proxy }, rx))
    }
}

#[async_trait::async_trait]
impl NotifyBackend for DbusNotify {
    async fn show(&self, summary: &str, body: &str, open_action: bool) -> Result<u32, NotifyError> {
        self.proxy
            .notify(APP_NAME, 0, "", summary, body, &actions(open_action), HashMap::new(), -1)
            .await
            .map_err(|e| {
                tracing::warn!(class = error_class(&e), "notify failed");
                NotifyError::Unavailable
            })
    }

    async fn close(&self, id: u32) {
        if let Err(e) = self.proxy.close_notification(id).await {
            tracing::debug!(class = error_class(&e), "close notification failed");
        }
    }

    async fn owner(&self) -> Option<String> {
        let dbus = zbus::fdo::DBusProxy::new(&self.conn).await.ok()?;
        let name = BusName::try_from(NOTIFICATIONS).ok()?;
        dbus.get_name_owner(name).await.ok().map(|o| o.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_open_action_is_offered_only_with_omarchy_shell() {
        assert_eq!(actions(true), [OPEN_ACTION, "Open"]);
        assert!(actions(false).is_empty());
    }

    #[test]
    fn errors_are_logged_by_class() {
        assert_eq!(error_class(&zbus::Error::InterfaceNotFound), "other");
        let io = zbus::Error::InputOutput(std::sync::Arc::new(std::io::Error::other("SECRET")));
        assert_eq!(error_class(&io), "io");
    }
}
