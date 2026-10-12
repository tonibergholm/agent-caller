//! Remote secrets in the Secret Service (omarchy spec §9): items in the
//! default collection with attributes `service=agent-caller`,
//! `machine=<key>`. Reads check the collection and item lock first.
//! Writes and deletes call `CreateItem` and `Item.Delete` directly and
//! dismiss any prompt the service returns (reported `Locked`). Only
//! `unlock` runs a prompt (plan D13).
use std::collections::HashMap;

use secret_service::{EncryptionType, Error, SecretService};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::secrets::{BackendError, SecretBackend};

pub const SERVICE_ATTRIBUTE: &str = "agent-caller";
const CONTENT_TYPE: &str = "text/plain";

pub fn attributes(key: &str) -> HashMap<&str, &str> {
    HashMap::from([("service", SERVICE_ATTRIBUTE), ("machine", key)])
}

/// A class for logs and replies; the D-Bus message is never kept.
pub fn map_error(e: &Error) -> BackendError {
    match e {
        Error::Locked => BackendError::Locked,
        Error::Prompt => BackendError::Dismissed,
        Error::Unavailable | Error::PromptDisconnected => BackendError::Unavailable,
        _ => BackendError::Failed,
    }
}

fn class(e: Error) -> BackendError {
    map_error(&e)
}

#[derive(Default)]
pub struct SecretServiceBackend {
    ss: Option<SecretService<'static>>,
}

impl SecretServiceBackend {
    pub fn new() -> Self {
        Self::default()
    }

    async fn service(&mut self) -> Result<&SecretService<'static>, BackendError> {
        if self.ss.is_none() {
            self.ss = Some(SecretService::connect(EncryptionType::Dh).await.map_err(class)?);
        }
        Ok(self.ss.as_ref().expect("connected above"))
    }

    /// Drops a broken connection so the next call reconnects.
    fn settle<T>(&mut self, r: Result<T, BackendError>) -> Result<T, BackendError> {
        if matches!(r, Err(BackendError::Unavailable | BackendError::Failed)) {
            self.ss = None;
        }
        r
    }
}

async fn read_in(ss: &SecretService<'_>, key: &str) -> Result<Option<String>, BackendError> {
    let col = ss.get_default_collection().await.map_err(class)?;
    if col.is_locked().await.map_err(class)? {
        return Err(BackendError::Locked);
    }
    let items = col.search_items(attributes(key)).await.map_err(class)?;
    let Some(item) = items.first() else { return Ok(None) };
    if item.is_locked().await.map_err(class)? {
        return Err(BackendError::Locked);
    }
    let bytes = item.get_secret().await.map_err(class)?;
    String::from_utf8(bytes).map(Some).map_err(|_| BackendError::Failed)
}

const SECRETS: &str = "org.freedesktop.secrets";

/// Whether a Secret Service reply asks for a prompt: any path but "/".
pub fn needs_prompt(prompt: &str) -> bool {
    prompt != "/"
}

fn bus(e: zbus::Error) -> BackendError {
    match e {
        zbus::Error::InputOutput(_) | zbus::Error::Connection(..) => BackendError::Unavailable,
        _ => BackendError::Failed,
    }
}

/// A returned prompt is never run: it is dismissed, and the change is
/// reported as `Locked` (plan D13). The user unlocks from the panel.
async fn refuse_prompt(conn: &zbus::Connection, prompt: &OwnedObjectPath) -> Result<(), BackendError> {
    if !needs_prompt(prompt.as_str()) {
        return Ok(());
    }
    let _ = conn
        .call_method(Some(SECRETS), prompt.as_str(), Some("org.freedesktop.Secret.Prompt"), "Dismiss", &())
        .await;
    Err(BackendError::Locked)
}

/// `CreateItem` called directly, so its prompt can be refused; the crate's
/// `create_item` would run it. The secret travels in a "plain" session
/// (plan D38): secret-service 5.2 keeps its encrypted session private.
async fn write_in(ss: &SecretService<'_>, key: &str, value: &str) -> Result<(), BackendError> {
    let col = ss.get_default_collection().await.map_err(class)?;
    if col.is_locked().await.map_err(class)? {
        return Err(BackendError::Locked);
    }
    let conn = zbus::Connection::session().await.map_err(bus)?;
    let reply = conn
        .call_method(
            Some(SECRETS),
            "/org/freedesktop/secrets",
            Some("org.freedesktop.Secret.Service"),
            "OpenSession",
            &("plain", Value::from("")),
        )
        .await
        .map_err(bus)?;
    let (_, session): (OwnedValue, OwnedObjectPath) = reply.body().deserialize().map_err(|_| BackendError::Failed)?;
    let mut props: HashMap<&str, Value<'_>> = HashMap::new();
    props.insert("org.freedesktop.Secret.Item.Label", Value::from(format!("agent-caller machine {key}")));
    props.insert("org.freedesktop.Secret.Item.Attributes", Value::from(attributes(key)));
    let secret = (session.clone(), Vec::<u8>::new(), value.as_bytes().to_vec(), CONTENT_TYPE);
    let result = conn
        .call_method(
            Some(SECRETS),
            col.collection_path.as_str(),
            Some("org.freedesktop.Secret.Collection"),
            "CreateItem",
            &(props, secret, true),
        )
        .await;
    let _ = conn
        .call_method(Some(SECRETS), session.as_str(), Some("org.freedesktop.Secret.Session"), "Close", &())
        .await;
    let (_item, prompt): (OwnedObjectPath, OwnedObjectPath) =
        result.map_err(bus)?.body().deserialize().map_err(|_| BackendError::Failed)?;
    refuse_prompt(&conn, &prompt).await
}

/// `Item.Delete` called directly, so its prompt can be refused.
async fn delete_in(ss: &SecretService<'_>, key: &str) -> Result<(), BackendError> {
    let col = ss.get_default_collection().await.map_err(class)?;
    if col.is_locked().await.map_err(class)? {
        return Err(BackendError::Locked);
    }
    let conn = zbus::Connection::session().await.map_err(bus)?;
    for item in col.search_items(attributes(key)).await.map_err(class)? {
        let reply = conn
            .call_method(Some(SECRETS), item.item_path.as_str(), Some("org.freedesktop.Secret.Item"), "Delete", &())
            .await
            .map_err(bus)?;
        let prompt: OwnedObjectPath = reply.body().deserialize().map_err(|_| BackendError::Failed)?;
        refuse_prompt(&conn, &prompt).await?;
    }
    Ok(())
}

/// The only call that may prompt (§9). secret-service 5.2 has no way to
/// dismiss its prompt: when the worker's 120 s limit drops this future, the
/// prompt stays on screen until the user answers it. A late "unlock" then
/// unlocks the collection and the next `refresh` or `retry_machine` reads
/// the secrets; a late dismiss changes nothing.
async fn unlock_in(ss: &SecretService<'_>) -> Result<(), BackendError> {
    let col = ss.get_default_collection().await.map_err(class)?;
    if col.is_locked().await.map_err(class)? {
        col.unlock().await.map_err(class)?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl SecretBackend for SecretServiceBackend {
    async fn read(&mut self, key: &str) -> Result<Option<String>, BackendError> {
        let r = match self.service().await {
            Ok(ss) => read_in(ss, key).await,
            Err(e) => Err(e),
        };
        self.settle(r)
    }

    async fn write(&mut self, key: &str, value: &str) -> Result<(), BackendError> {
        let r = match self.service().await {
            Ok(ss) => write_in(ss, key, value).await,
            Err(e) => Err(e),
        };
        self.settle(r)
    }

    async fn delete(&mut self, key: &str) -> Result<(), BackendError> {
        let r = match self.service().await {
            Ok(ss) => delete_in(ss, key).await,
            Err(e) => Err(e),
        };
        self.settle(r)
    }

    async fn unlock(&mut self) -> Result<(), BackendError> {
        let r = match self.service().await {
            Ok(ss) => unlock_in(ss).await,
            Err(e) => Err(e),
        };
        self.settle(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_are_found_by_service_and_machine() {
        let a = attributes("11111111-1111-4111-8111-111111111111");
        assert_eq!(a.get("service"), Some(&"agent-caller"));
        assert_eq!(a.get("machine"), Some(&"11111111-1111-4111-8111-111111111111"));
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn only_the_no_prompt_path_lets_a_change_through() {
        assert!(!needs_prompt("/"));
        assert!(needs_prompt("/org/freedesktop/secrets/prompt/u1"));
    }

    #[test]
    fn errors_map_to_classes() {
        assert_eq!(map_error(&secret_service::Error::Locked), BackendError::Locked);
        assert_eq!(map_error(&secret_service::Error::Prompt), BackendError::Dismissed);
        assert_eq!(map_error(&secret_service::Error::Unavailable), BackendError::Unavailable);
        assert_eq!(map_error(&secret_service::Error::PromptDisconnected), BackendError::Unavailable);
        assert_eq!(map_error(&secret_service::Error::NoResult), BackendError::Failed);
    }

    /// Needs a desktop session bus with an unlocked keyring and a Rust
    /// toolchain (omarchy has none; Task 22 check 4 covers the keyring
    /// there): `cargo test -p acd-inbox --locked --lib -- --ignored keyring_round_trip`
    #[tokio::test]
    #[ignore = "touches the user's keyring"]
    async fn keyring_round_trip() {
        let mut b = SecretServiceBackend::new();
        let key = "acd-inbox-test-00000000-0000-4000-8000-000000000000";
        b.write(key, "value").await.unwrap();
        assert_eq!(b.read(key).await.unwrap().as_deref(), Some("value"));
        b.delete(key).await.unwrap();
        assert_eq!(b.read(key).await.unwrap(), None);
    }
}
