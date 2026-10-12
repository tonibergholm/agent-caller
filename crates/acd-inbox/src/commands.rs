//! Commands from views (omarchy spec §5.2, plan D10, D11, D31).
use std::sync::Arc;

use acd_client::inbox::PickError;
use serde_json::{Map, Value};

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

/// Inline commands run under the admission lock; a closed connection runs
/// nothing and gets no reply (plan D31).
fn dispatch(svc: &Arc<Service>, conn: &Arc<Conn>, tag: &'static str, id: &str, m: &Map<String, Value>) {
    let Some(r) = svc.admit(conn, || admitted(svc, conn, tag, m)) else { return };
    svc.reply(conn, line_for(id, r));
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
        "panel" => {
            svc.set_panel(conn.id, b(m, "open")?);
            Ok(Value::Null)
        }
        _ => Err(ErrorCode::Unsupported),
    }
}
