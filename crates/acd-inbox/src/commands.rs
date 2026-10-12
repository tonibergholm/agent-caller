//! Commands from views (omarchy spec §5.2, plan D10, D11, D31).
use std::sync::Arc;

use acd_client::{
    inbox::{MachineError, PickError},
    settings::{EndpointConfig, SecretError, SecretErrorKind, SecretStatus},
};
use serde_json::{Map, Value, json};

use crate::protocol::{self, ErrorCode};
use crate::server::{Conn, Service};

/// The line is not a JSON object with a string `type` and a string `id` of
/// 1 to 64 bytes. The connection closes (§11).
#[derive(Debug)]
pub struct Malformed;

/// The command types this service knows. A view's `type` text is logged
/// only when it is one of these; anything else as "unknown".
const KNOWN: &[&str] = &[
    "pick", "answer_text", "reply", "cancel", "escape", "mark_seen", "set_show", "pair", "rename_machine",
    "remove_machine", "retry_machine", "unlock_secrets", "set_notifications", "panel",
];

pub fn handle(svc: &Arc<Service>, conn: &Arc<Conn>, line: &[u8]) -> Result<(), Malformed> {
    let Ok(Value::Object(m)) = serde_json::from_slice::<Value>(line) else {
        return Err(Malformed);
    };
    let (Some(ty), Some(id)) = (m.get("type").and_then(Value::as_str), m.get("id").and_then(Value::as_str)) else {
        return Err(Malformed);
    };
    if id.is_empty() || id.len() > protocol::MAX_ID {
        return Err(Malformed);
    }
    let tag = KNOWN.iter().copied().find(|k| *k == ty).unwrap_or("unknown");
    tracing::debug!(conn = conn.id, command = tag, "command");
    if !conn.remember(id) {
        svc.reply(conn, protocol::error_line(id, ErrorCode::DuplicateId));
        return Ok(());
    }
    dispatch(svc, conn, tag, id, &m);
    Ok(())
}

pub(crate) fn line_for(id: &str, r: Result<Value, ErrorCode>) -> String {
    match r {
        Ok(v) => protocol::ok_line(id, v),
        Err(c) => protocol::error_line(id, c),
    }
}

pub(crate) fn s<'a>(m: &'a Map<String, Value>, k: &str) -> Result<&'a str, ErrorCode> {
    m.get(k).and_then(Value::as_str).ok_or(ErrorCode::BadRequest)
}

pub(crate) fn n(m: &Map<String, Value>, k: &str) -> Result<u64, ErrorCode> {
    m.get(k).and_then(Value::as_u64).ok_or(ErrorCode::BadRequest)
}

pub(crate) fn b(m: &Map<String, Value>, k: &str) -> Result<bool, ErrorCode> {
    m.get(k).and_then(Value::as_bool).ok_or(ErrorCode::BadRequest)
}

fn pick_err(e: PickError) -> ErrorCode {
    protocol::pick_error_code(&e)
}

/// Inline commands run under the admission lock (plan D31); slow ones, which
/// may wait on the keyring or the edit lock, on the blocking pool (D11).
fn dispatch(svc: &Arc<Service>, conn: &Arc<Conn>, tag: &'static str, id: &str, m: &Map<String, Value>) {
    match tag {
        "pair" | "remove_machine" | "retry_machine" | "unlock_secrets" | "rename_machine" | "set_show" => {
            slow(svc, conn, tag, id, m.clone())
        }
        "unknown" => svc.reply(conn, protocol::error_line(id, ErrorCode::Unsupported)),
        _ => {
            let Some(r) = svc.admit(conn, || admitted(svc, conn, tag, m)) else { return };
            svc.reply(conn, line_for(id, r));
        }
    }
}

fn admitted(svc: &Arc<Service>, conn: &Conn, tag: &str, m: &Map<String, Value>) -> Result<Value, ErrorCode> {
    let ib = &svc.inbox;
    match tag {
        "pick" => ib
            .pick_owned(conn.owner, s(m, "card")?, s(m, "request")?, s(m, "option")?, n(m, "revision")?)
            .map(Value::String)
            .map_err(pick_err),
        "answer_text" => ib
            .answer_text_owned(conn.owner, s(m, "card")?, s(m, "request")?, n(m, "revision")?, s(m, "text")?.to_owned())
            .map(Value::String)
            .map_err(pick_err),
        "reply" => ib
            .reply_owned(conn.owner, s(m, "card")?, s(m, "text")?.to_owned())
            .map(Value::String)
            .map_err(pick_err),
        "cancel" => Ok(Value::Bool(ib.cancel(s(m, "action")?))),
        "escape" => Ok(Value::Bool(ib.escape_owned(conn.owner))),
        "mark_seen" => ib.mark_seen(s(m, "card")?).map(|()| Value::Null).map_err(|e| match e {
            PickError::CannotReply => ErrorCode::CannotMarkSeen,
            e => pick_err(e),
        }),
        "set_notifications" => {
            svc.set_notifications(b(m, "on")?).map_err(|_| ErrorCode::SettingsFailed)?;
            svc.settings_changed();
            Ok(Value::Null)
        }
        "panel" => {
            svc.set_panel(conn.id, b(m, "open")?);
            Ok(Value::Null)
        }
        _ => Err(ErrorCode::Unsupported),
    }
}

/// Runs off the reader, so a disconnect is seen at once (plan D11). The
/// reply is dropped if the view left meanwhile.
fn slow(svc: &Arc<Service>, conn: &Arc<Conn>, tag: &'static str, id: &str, m: Map<String, Value>) {
    if !svc.is_open(conn) {
        return;
    }
    let (svc, conn, id) = (svc.clone(), conn.clone(), id.to_owned());
    tokio::spawn(async move {
        let worker = svc.clone();
        let r = tokio::task::spawn_blocking(move || run_slow(&worker, tag, &m))
            .await
            .unwrap_or(Err(ErrorCode::SecretStoreFailed));
        svc.reply(&conn, line_for(&id, r));
        svc.publish_settings().await;
    });
}

fn run_slow(svc: &Arc<Service>, tag: &str, m: &Map<String, Value>) -> Result<Value, ErrorCode> {
    let ib = &svc.inbox;
    match tag {
        "pair" => {
            let entry = ib.pair_machine(s(m, "pairing")?).map_err(|e| machine_error_code(&e))?;
            Ok(json!({"machine": entry.key, "name": entry.name}))
        }
        "remove_machine" => ib
            .remove_machine_checked(s(m, "machine")?)
            .map(|()| Value::Null)
            .map_err(|e| machine_error_code(&e)),
        "retry_machine" => {
            let key = s(m, "machine")?;
            // A junk key must cause no keyring read and no cache entry.
            if !ib.settings().machines.iter().any(|e| e.key == key) {
                return Err(ErrorCode::UnknownMachine);
            }
            // A cached keyring error is retried only when asked (plan D14).
            svc.secrets.refresh(&[key.to_owned()]);
            ib.reconnect_machine(key).map(|()| Value::Null).map_err(|e| machine_error_code(&e))
        }
        "unlock_secrets" => unlock(svc).map(|()| Value::Null),
        "rename_machine" => {
            let (key, name) = (s(m, "machine")?, s(m, "name")?);
            if !ib.settings().machines.iter().any(|e| e.key == key) {
                return Err(ErrorCode::UnknownMachine);
            }
            // The same rule as `MachineEntry::validate`, on the name alone, so
            // a bad key is never reported as a bad name.
            if name.trim().is_empty() || name.chars().count() > 64 {
                return Err(ErrorCode::BadName);
            }
            ib.rename_machine(key, name).map(|()| Value::Null).map_err(|_| ErrorCode::SettingsFailed)
        }
        "set_show" => {
            let on = b(m, "on")?;
            match s(m, "what")? {
                "idle" => ib.set_show_idle(on),
                "subagents" => ib.set_show_subagents(on).map_err(|_| ErrorCode::SettingsFailed)?,
                "hook_only" => ib.set_show_hook_only(on).map_err(|_| ErrorCode::SettingsFailed)?,
                _ => return Err(ErrorCode::BadRequest),
            }
            Ok(Value::Null)
        }
        _ => Err(ErrorCode::Unsupported),
    }
}

/// §9: unlock, re-read the affected secrets, reconnect those machines. A
/// dismissed or timed-out prompt changes nothing.
fn unlock(svc: &Arc<Service>) -> Result<(), ErrorCode> {
    let affected: Vec<String> = svc
        .inbox
        .machines()
        .into_iter()
        .filter(|m| matches!(m.endpoint, EndpointConfig::Tcp { .. }) && m.secret != SecretStatus::Ok)
        .map(|m| m.key)
        .collect();
    svc.secrets.unlock().map_err(|e| secret_code(&e))?;
    svc.secrets.refresh(&affected);
    for key in &affected {
        if let Err(e) = svc.inbox.reconnect_machine(key) {
            tracing::info!(code = machine_error_code(&e).as_str(), "machine not reconnected after unlock");
        }
    }
    Ok(())
}

/// The reply code for a keyring failure; the failure's text is never sent.
pub fn secret_code(e: &SecretError) -> ErrorCode {
    match e.kind() {
        SecretErrorKind::Locked => ErrorCode::SecretStoreLocked,
        // An unconfirmed change is reported as a timeout; the machine shows
        // `secret: unknown` (D36).
        SecretErrorKind::Timeout | SecretErrorKind::Unknown => ErrorCode::SecretStoreTimeout,
        SecretErrorKind::Other => ErrorCode::SecretStoreFailed,
    }
}

/// The reply code for a machine edit failure; the error's text may name
/// hosts, so only the code is sent.
pub fn machine_error_code(e: &MachineError) -> ErrorCode {
    match e {
        MachineError::UnknownMachine => ErrorCode::UnknownMachine,
        MachineError::BadPairing | MachineError::NotTailscale => ErrorCode::BadPairing,
        MachineError::NotPaired => ErrorCode::NotPaired,
        MachineError::SecretRead(s) | MachineError::SecretWrite(s) | MachineError::SecretDelete(s) => secret_code(s),
        MachineError::Settings(_) => ErrorCode::SettingsFailed,
        MachineError::PairRecoveryFailed | MachineError::RemoveRecoveryFailed => ErrorCode::SecretStoreFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the mapping. `NotTailscale` shares `bad_pairing` because a new
    /// wire code is a protocol change; Omarchy then shows the generic
    /// "not an acd pairing line" text (a documented follow-up).
    #[test]
    fn a_refused_address_is_reported_as_bad_pairing() {
        assert_eq!(machine_error_code(&MachineError::NotTailscale), ErrorCode::BadPairing);
        assert_eq!(machine_error_code(&MachineError::BadPairing), ErrorCode::BadPairing);
    }
}
