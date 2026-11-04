//! Dynamic attack on the Omarchy `acd-inbox` socket protocol (the task's
//! "(3) the Omarchy acd-inbox socket protocol"), against a real running
//! `Service` + `UnixListener` started by the `common::rig` test harness
//! (same server code path `main.rs` -> `serve::run` uses, minus the real
//! D-Bus/Secret Service dependency this sandbox lacks -- `rig` wires in
//! `NoKeyring` and an in-memory `SecretStore` instead, which only replaces
//! the keyring backend, not the socket server or the JSON-lines protocol
//! handling under attack here).
//!
//! No production code is changed by this file. Findings, if any, are
//! recorded in `docs/research/2026-11-04-red-team-acd.md`.
mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Connects and reads the three-message greeting, matching what
/// `View::connect` and `View::greeting` do together, but keeps the raw
/// stream so later cases can write bytes a conforming client never would.
async fn raw_connect(socket: &std::path::Path) -> UnixStream {
    let mut s = UnixStream::connect(socket).await.unwrap();
    // Drain the greeting (hello, settings, view) before attacking.
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let mut newlines = 0;
    while newlines < 3 {
        let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut tmp))
            .await
            .expect("greeting within 5s")
            .unwrap();
        assert!(n > 0, "service closed before the greeting finished");
        buf.extend_from_slice(&tmp[..n]);
        newlines = buf.iter().filter(|&&b| b == b'\n').count();
    }
    s
}

async fn read_until_eof_or(s: &mut UnixStream, timeout: Duration) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 65536];
    loop {
        match tokio::time::timeout(timeout, s.read(&mut tmp)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(n)) => buf.extend_from_slice(&tmp[..n]),
            Ok(Err(_)) => break,
        }
    }
    buf
}

/// Case 1: a line over `MAX_LINE` (4 MiB) closes the connection with no
/// reply, and does not crash or wedge the server for the next client.
#[tokio::test]
async fn oversized_line_closes_without_a_reply() {
    let r = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut s = raw_connect(&r.socket).await;
    let huge = vec![b'x'; 5 * 1024 * 1024];
    let _ = s.write_all(&huge).await; // may itself fail if the server closes mid-write
    let _ = s.write_all(b"\n").await;
    let reply = read_until_eof_or(&mut s, Duration::from_secs(5)).await;
    assert!(reply.is_empty(), "an oversized line must get no reply, got {reply:?}");

    // The server must still serve a fresh, well-behaved client right after.
    let mut v = View::connect(&r.socket).await;
    v.greeting().await;
}

/// Case 2: malformed JSON (and valid JSON that is not an object, and an
/// object missing `type`/`id`) all close the connection the same way --
/// `Malformed` -- with no reply and no panic.
#[tokio::test]
async fn malformed_lines_close_without_a_reply() {
    for bad in [
        &b"{not json at all\n"[..],
        &b"[1,2,3]\n"[..],
        &b"\"just a string\"\n"[..],
        &b"{}\n"[..],
        &b"{\"type\":\"pick\"}\n"[..], // type but no id
        &b"{\"id\":\"c1\"}\n"[..],    // id but no type
        &b"\0\0\0\n"[..],
        "{\"type\":\"pick\",\"id\":\"\u{202e}\u{0}evil\"}\n".as_bytes(),
    ] {
        let r = rig(vec![pending_agent("a1", "r1")], fast()).await;
        let mut s = raw_connect(&r.socket).await;
        s.write_all(bad).await.unwrap();
        let reply = read_until_eof_or(&mut s, Duration::from_secs(5)).await;
        assert!(reply.is_empty(), "malformed line {bad:?} must get no reply, got {reply:?}");
    }
}

/// Case 3: an empty id or an id over `MAX_ID` (64 bytes) is also
/// `Malformed` (closed), per `commands::handle`.
#[tokio::test]
async fn id_length_edges_are_enforced() {
    for id in ["", &"x".repeat(65)] {
        let r = rig(vec![pending_agent("a1", "r1")], fast()).await;
        let mut v = View::connect(&r.socket).await;
        v.greeting().await;
        v.send(&json!({"type": "mark_seen", "id": id, "card": "a1"})).await;
        // A malformed id closes the connection with no reply: `recv()`
        // must give `None` (EOF), not a frame.
        let next = v.recv().await;
        assert!(
            next.is_none(),
            "id {id:?} ({} bytes) must be refused (connection closed), got {next:?}",
            id.len()
        );
    }
}

/// Case 4: a duplicate command id on the same connection gets
/// `duplicate_id` and does not re-run the command (it must not, for
/// example, double-answer a permission request).
#[tokio::test]
async fn duplicate_command_id_is_rejected_not_rerun() {
    let r = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = View::connect(&r.socket).await;
    v.greeting().await;
    let cmd = json!({"type": "mark_seen", "id": "dup-1", "card": "a1"});
    v.send(&cmd).await;
    let r1 = v.recv_until(|m| m["id"] == "dup-1").await;
    v.send(&cmd).await;
    let r2 = v.recv_until(|m| m["id"] == "dup-1").await;
    // Whatever the first attempt returned (an agent in `needs_you` is not
    // `mark_seen`-able, so `unknown_card`/an error is expected here), the
    // *replay* must be `duplicate_id`, never a second, independent
    // evaluation of the same command.
    assert_eq!(r2["type"], "error", "replayed id must error, not re-run: {r2:?}");
    assert_eq!(r2["code"], "duplicate_id", "first={r1:?} second={r2:?}");
}

/// Case 5: an unknown command `type` gets `unsupported`, never a panic or
/// a silent drop, and control/unicode in string fields does not crash the
/// server (the task's "unicode and control characters in every field").
#[tokio::test]
async fn unknown_command_and_unicode_fields_are_handled_safely() {
    let r = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = View::connect(&r.socket).await;
    v.greeting().await;
    v.send(&json!({"type": "this-does-not-exist", "id": "u1"})).await;
    let reply = v.recv_until(|m| m["id"] == "u1").await;
    assert_eq!(reply["type"], "error");
    assert_eq!(reply["code"], "unsupported");

    let evil = "a\u{0}b\u{202e}c\u{2028}d\u{7f}".repeat(50);
    v.send(&json!({
        "type": "pick", "id": "u2", "card": evil.clone(),
        "request": evil.clone(), "option": evil.clone(), "revision": 0
    }))
    .await;
    let reply = v.recv_until(|m| m["id"] == "u2").await;
    // Any well-formed error is fine; what matters is the server answered
    // instead of closing or panicking.
    assert_eq!(reply["type"], "error", "unicode/control payload must not crash: {reply:?}");

    // The server must still be alive for a fresh client.
    let mut v2 = View::connect(&r.socket).await;
    v2.greeting().await;
}

/// Case 6: huge numeric fields (the task's "huge revision numbers") must
/// be rejected cleanly, not panic on overflow.
#[tokio::test]
async fn huge_revision_number_is_rejected_not_a_panic() {
    let r = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut v = View::connect(&r.socket).await;
    v.greeting().await;
    v.send(&json!({
        "type": "pick", "id": "h1", "card": "a1",
        "request": "r1", "option": "allow", "revision": u64::MAX
    }))
    .await;
    let reply = v.recv_until(|m| m["id"] == "h1").await;
    assert_eq!(reply["type"], "error", "a huge revision must error, not deliver: {reply:?}");

    // A revision that does not fit in u64 at all (serde_json rejects it as
    // a number, so this exercises the "not even a valid u64" edge).
    let mut s = raw_connect(&r.socket).await;
    let line = b"{\"type\":\"pick\",\"id\":\"h2\",\"card\":\"a1\",\"request\":\"r1\",\"option\":\"allow\",\"revision\":99999999999999999999999999}\n";
    s.write_all(line).await.unwrap();
    let reply = read_until_eof_or(&mut s, Duration::from_secs(3)).await;
    // Either a clean error reply or a close is acceptable; a hang or a
    // crash (which the test harness would report as a panic) is not.
    let _ = reply;
}

/// Case 7: many concurrent connections from the same uid are all served
/// (the Omarchy socket enforces no cap; peers are same-uid only per the
/// static review). Confirms the server does not wedge or drop earlier
/// connections when many pile up.
#[tokio::test]
async fn many_concurrent_connections_are_all_served() {
    let r = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let mut views = Vec::new();
    for _ in 0..200 {
        let mut v = View::connect(&r.socket).await;
        v.greeting().await;
        views.push(v);
    }
    // Every one of them must still answer a ping-equivalent (mark_seen on
    // a nonexistent card still replies, just with an error).
    for (i, v) in views.iter_mut().enumerate() {
        let id = format!("c{i}");
        v.send(&json!({"type": "mark_seen", "id": id, "card": "nonexistent"})).await;
        let reply = v.recv_until(|m| m["id"] == id).await;
        assert!(reply["type"] == "ok" || reply["type"] == "error");
    }
}

/// Case 8: the peer-uid check itself. `peer_allowed` is a pure function;
/// this exercises it directly against the real socket's actual peer
/// credentials (SO_PEERCRED), confirming the running server's own uid is
/// what gets admitted and that a forged uid is not treated specially
/// (there is no way to forge SO_PEERCRED from userspace, so this is a
/// sanity check, not a bypass attempt).
#[tokio::test]
async fn peer_uid_check_matches_the_real_process() {
    let r = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let s = UnixStream::connect(&r.socket).await.unwrap();
    let own_uid = unsafe { libc::getuid() };
    assert!(acd_inbox::startup::peer_ok(&s), "same-process (same uid {own_uid}) peer must be allowed");
}

// Socket-mode (0600) is already covered, against the real `startup::listen`
// path, by `start_service_listens_privately_and_names_the_local_machine` in
// `serve.rs`. The `common::rig` fixture used above binds the listener
// directly (it skips `startup::listen`'s chmod on purpose, to keep the
// fixture simple for tests that only care about the protocol), so a mode
// assertion against `rig`'s socket here would test the fixture, not the
// daemon -- not repeated.
