//! Task 19: retained work stays bounded (A1 to A8) and service stays fair
//! (A6), under a stalled delivery lock, a stalled keys holder and floods.
//!
//! A held connection is closed only after the server has admitted it: an
//! admitted hold raises the live `control_queued` by two (its `Held` and the
//! slot reserved for its close), and a request that fails open is read.
mod common;
use common::*;

use std::time::Duration;

use acd_adapters::herdr::fake::fake_agent;
use acd_adapters::hooks::testing::{read_response, send};
use acd_protocol::{ClientMessage, ServerMessage};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::time::Instant;

const PR: &str = "/hook/claude/PermissionRequest";

fn dialog() -> String {
    std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/herdr/claude-permission-bash.txt",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// Shows the permission dialog on w1:p1 and returns its agent id and the
/// headers that bind a Claude session to it.
fn bound(r: &Rig) -> (String, Vec<(&'static str, String)>) {
    let f = r.fake.as_ref().unwrap();
    f.set_screen("w1:p1", "detection", &dialog());
    f.set_screen("w1:p1", "visible", &dialog());
    let agent = r
        .herdr
        .as_ref()
        .unwrap()
        .panes()
        .read()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let h = vec![
        ("X-Herdr-Pane", "w1:p1".to_string()),
        ("X-Herdr-Socket", r.sock()),
        ("X-Claude-Entrypoint", "cli".to_string()),
    ];
    (agent, h)
}

fn refs<'a>(h: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    h.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

fn permission(sid: &str, command: &str) -> Value {
    let mut p = fx("claude/payload-PermissionRequest-Bash.json");
    p["session_id"] = sid.into();
    p["tool_input"]["command"] = command.into();
    p
}

/// Waits until the live control queue depth is exactly `n`.
async fn control_at(r: &Rig, n: usize) {
    until(&format!("control_queued == {n}"), || {
        (r.hooks.stats().control_queued == n).then_some(())
    })
    .await;
}

/// The reply, which must come within `within` (a RED run fails, never hangs).
async fn reply_within(stream: TcpStream, within: Duration) -> (u16, String) {
    tokio::time::timeout(within, read_response(stream))
        .await
        .expect("a reply in time")
}

#[tokio::test]
async fn a_stalled_delivery_lock_keeps_holds_bounded_and_overflow_fails_open() {
    let r = rig(&[fake_agent("w1:p1", "claude", "blocked")], |c| {
        c.max_holds = 8
    })
    .await;
    let (agent, h) = bound(&r);
    let h = refs(&h);
    let locks = r.herdr.as_ref().unwrap().agent_locks();
    let guard = locks.lock(&agent).await;
    let p = fx("claude/payload-PermissionRequest-Bash.json").to_string();
    // The first hold stalls the reconciler on the delivery lock: its `Held`
    // is taken, the slot for its close stays reserved.
    let first = send(r.port, PR, TOKEN, &h, &p).await;
    control_at(&r, 1).await;
    // Seven more fill the remaining permits; each is admitted, then Claude
    // goes away.
    for i in 1..=7 {
        let s = send(r.port, PR, TOKEN, &h, &p).await;
        control_at(&r, 1 + 2 * i).await;
        drop(s);
    }
    // Every further request fails open at once while the lock is stalled.
    for _ in 0..20 {
        let t = Instant::now();
        let s = send(r.port, PR, TOKEN, &h, &p).await;
        assert_eq!(
            reply_within(s, Duration::from_secs(2)).await,
            (200, String::new())
        );
        assert!(
            t.elapsed() < Duration::from_millis(500),
            "A1: fails open at once"
        );
    }
    let st = r.hooks.stats();
    assert_eq!(st.control_queued, 15, "A2: {st:?}");
    assert!(st.control_queued <= 2 * 8);
    // Once the lock frees, the queues drain and the permits come back.
    drop(guard);
    drop(first);
    until("drained", || {
        let st = r.hooks.stats();
        (st.control_queued == 0 && st.holds == 0).then_some(())
    })
    .await;
    let _held = send(r.port, PR, TOKEN, &h, &p).await;
    until("a new hold is admitted", || {
        (r.hooks.stats().holds == 1).then_some(())
    })
    .await;
}

fn agent_of(r: &Rig, pane: &str) -> String {
    r.herdr
        .as_ref()
        .unwrap()
        .panes()
        .read()
        .unwrap()
        .iter()
        .find(|(_, p)| p.pane_id == pane)
        .unwrap()
        .0
        .clone()
}

#[tokio::test]
async fn queued_holds_cannot_delay_a_takeover_past_one_batch() {
    // A6: a T3 change is served at the start of the next turn, so at most
    // `batch` control messages run between it and the takeover. The
    // reconciler is stalled at the first item of a batch (`x`, on p2's
    // delivery lock) when the change arrives.
    let r = rig(
        &[
            fake_agent("w1:p1", "claude", "blocked"),
            fake_agent("w1:p2", "claude", "blocked"),
        ],
        |c| {
            c.batch = 2;
            c.max_holds = 16;
        },
    )
    .await;
    let f = r.fake.as_ref().unwrap();
    for p in ["w1:p1", "w1:p2"] {
        f.set_screen(p, "detection", &dialog());
    }
    let sock = r.sock();
    let on = |pane: &'static str| {
        vec![
            ("X-Herdr-Pane", pane),
            ("X-Herdr-Socket", sock.as_str()),
            ("X-Claude-Entrypoint", "cli"),
        ]
    };
    // The takeover target: a hooks-only session with an open hold.
    let target = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &CLI)
        .await;
    let rid = until("request", || {
        r.agent(&format!("claude:{SID}"))
            .and_then(|a| a.pending)
            .map(|p| p.id)
    })
    .await;
    control_at(&r, 1).await;
    let (_, mut rx) = r.core.attach(None);
    let locks = r.herdr.as_ref().unwrap().agent_locks();
    let l1 = locks.lock(&agent_of(&r, "w1:p1")).await;
    let l2 = locks.lock(&agent_of(&r, "w1:p2")).await;
    // `w` stalls the idle reconciler on p1's lock...
    let w = permission("5f4996ff-0000-4000-8000-0000000000a0", "touch /tmp/w");
    let _w = send(r.port, PR, TOKEN, &on("w1:p1"), &w.to_string()).await;
    control_at(&r, 2).await;
    // ...while `x` (bound to p2) and six hooks-only holds queue behind it.
    let x = permission("5f4996ff-0000-4000-8000-0000000000b0", "touch /tmp/x");
    let _x = send(r.port, PR, TOKEN, &on("w1:p2"), &x.to_string()).await;
    control_at(&r, 4).await;
    let queued: Vec<String> = (1..=6)
        .map(|i| format!("5f4996ff-0000-4000-8000-00000000000{i}"))
        .collect();
    let mut streams = Vec::new();
    for (i, sid) in queued.iter().enumerate() {
        let body = permission(sid, "touch /tmp/c").to_string();
        streams.push(send(r.port, PR, TOKEN, &CLI, &body).await);
        control_at(&r, 4 + 2 * (i + 1)).await;
    }
    // `w` finishes; the next turn's batch takes `x`, which stalls on p2.
    drop(l1);
    control_at(&r, 15).await;
    r.t3.set_for_tests("t3:thread-1", Some(SID));
    drop(l2);
    let second = format!("claude:{}", queued[1]);
    let mut order = Vec::new();
    let collect = async {
        while order.len() < 2 {
            match rx.recv().await.unwrap() {
                ServerMessage::RequestResolved { request_id, .. } if request_id == rid => {
                    order.push("takeover")
                }
                ServerMessage::RequestOpened { request, .. } if request.agent_id == second => {
                    order.push("second queued hold")
                }
                _ => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), collect)
        .await
        .expect("both observed");
    assert_eq!(
        order,
        ["takeover", "second queued hold"],
        "A6: the takeover is served after at most `batch` queued holds"
    );
    assert_eq!(
        reply_within(target, Duration::from_secs(5)).await,
        (200, String::new()),
        "the takeover released the hold"
    );
}

#[tokio::test]
async fn a_codex_flood_never_runs_more_reads_or_watches_than_slots() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_owned();
    let mut pane = fake_agent("w1:p1", "codex", "working");
    pane["cwd"] = cwd.clone().into();
    pane["foreground_cwd"] = cwd.clone().into();
    let r = rig(&[pane], |c| {
        c.max_reads = 2;
        c.max_tracks = 4;
    })
    .await;
    // Every session binds to the only Codex pane in the cwd; the idle
    // screen makes every bind read a full read that opens nothing.
    r.fake.as_ref().unwrap().set_screen(
        "w1:p1",
        "detection",
        &std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/herdr/codex-idle-detection.txt",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    );
    let mut worst = acd_adapters::hooks::HookStats::default();
    for i in 0..200 {
        let mut p = fx("codex/payload-PermissionRequest-Bash.json");
        p["session_id"] = format!("01a116d3-0000-7000-8000-{i:012}").into();
        p["cwd"] = cwd.clone().into();
        r.hook("codex", "PermissionRequest", &p, &[]).await;
        let st = r.hooks.stats();
        worst.read_tasks = worst.read_tasks.max(st.read_tasks);
        worst.internal_queued = worst.internal_queued.max(st.internal_queued);
        worst.tracks = worst.tracks.max(st.tracks);
        worst.codex_watches = worst.codex_watches.max(st.codex_watches);
    }
    assert!(
        worst.read_tasks <= 2,
        "A3: no task waits for a permit: {worst:?}"
    );
    assert!(worst.internal_queued <= 2 + 2, "A3: {worst:?}");
    assert!(worst.tracks <= 4, "A5: {worst:?}");
    assert!(
        worst.codex_watches <= 4,
        "A5: pending watches are capped too: {worst:?}"
    );
    assert!(worst.read_tasks > 0, "the flood did read: {worst:?}");
}

#[tokio::test]
async fn a_flood_of_unique_sessions_keeps_session_and_history_storage_bounded() {
    // Sol R3-3: lifecycle events and refused permissions from fresh ids.
    let r = rig(&[], |c| {
        c.max_sessions = 32;
        c.max_generation_ids = 32;
    })
    .await;
    let sid = |i: usize| format!("6a000000-0000-4000-8000-{i:012}");
    for i in 0..300 {
        let mut p = fx("claude/payload-UserPromptSubmit.json");
        p["session_id"] = sid(i).into();
        r.hook("claude", "UserPromptSubmit", &p, &CLI).await;
        let mut c = fx("codex/payload-PermissionRequest-Bash.json");
        c["session_id"] = sid(i).into();
        r.hook("codex", "PermissionRequest", &c, &[]).await; // no herdr: unbound
    }
    until("drained", || {
        (r.hooks.stats().events_queued == 0).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let st = r.hooks.stats();
    assert!(st.sessions <= 32 && st.codex_sessions <= 32, "A8: {st:?}");
    assert!(st.generation_ids <= 4 * 32, "A8: {st:?}");
    assert!(
        st.generation_ids_dropped > 0,
        "the cap was reached and counted: {st:?}"
    );
    assert_eq!(
        st.sessions_refused, 0,
        "idle sessions are evicted, not refused"
    );
    assert!(
        r.agent(&format!("claude:{}", sid(0))).is_none(),
        "an evicted hooks-only session leaves core"
    );
    assert!(r.agent(&format!("claude:{}", sid(299))).is_some());
}

#[tokio::test]
async fn a_full_session_map_of_busy_sessions_refuses_and_releases_the_hold() {
    let r = rig(&[], |c| c.max_sessions = 2).await;
    let mut held = Vec::new();
    for i in 0..2 {
        let sid = format!("6c000000-0000-4000-8000-{i:012}");
        held.push(r.hold(&permission(&sid, "touch /tmp/a"), &CLI).await);
        until("pending", || {
            r.agent(&format!("claude:{sid}")).and_then(|a| a.pending)
        })
        .await;
    }
    let t = Instant::now();
    let s = r
        .hold(
            &permission("6c000000-0000-4000-8000-000000000099", "touch /tmp/a"),
            &CLI,
        )
        .await;
    assert_eq!(
        reply_within(s, Duration::from_secs(2)).await,
        (200, String::new()),
        "A8: refused, the terminal answers it"
    );
    assert!(t.elapsed() < Duration::from_millis(500));
    until("counted", || {
        (r.hooks.stats().sessions_refused == 1).then_some(())
    })
    .await;
    assert_eq!(
        r.hooks.stats().sessions,
        2,
        "busy sessions are never evicted"
    );
}

#[tokio::test]
async fn evicted_ambiguity_still_shadows_new_requests() {
    // Sol R3-3 (round-3 verification): real unconsumed identities in more
    // sessions than the cap, then an identical request in an evicted one.
    let r = rig(&[], |c| {
        c.max_sessions = 8;
        c.evidence_window = Duration::from_millis(50);
    })
    .await;
    let sid = |i: usize| format!("6b000000-0000-4000-8000-{i:012}");
    for i in 0..40 {
        // A, then B in the same session: O1 ends A without evidence (an
        // unconsumed identity); closing B's hold ends it `uncertain` after
        // the evidence window (another one). No turn end clears them.
        let id = format!("claude:{}", sid(i));
        let a = r.hold(&permission(&sid(i), "touch /tmp/a"), &CLI).await;
        let first = until("A", || r.agent(&id).and_then(|q| q.pending).map(|q| q.id)).await;
        let b = r.hold(&permission(&sid(i), "touch /tmp/b"), &CLI).await;
        until("B", || {
            r.agent(&id)
                .and_then(|q| q.pending)
                .filter(|q| q.id != first)
        })
        .await;
        drop((a, b));
        until("tracks ended", || {
            (r.hooks.stats().tracks == 0).then_some(())
        })
        .await;
    }
    let st = r.hooks.stats();
    assert!(
        st.unconsumed_sessions <= 8,
        "A8: identity history is capped: {st:?}"
    );
    assert!(st.sessions <= 8, "A8: {st:?}");
    // Session 0's identities were evicted long ago; a new identical request
    // there must still be shadowed (A8's global shadow), so a PostToolUse
    // for the same command cannot resolve it.
    let _held = r.hold(&permission(&sid(0), "touch /tmp/a"), &CLI).await;
    let id0 = format!("claude:{}", sid(0));
    let rid = until("new A", || {
        r.agent(&id0).and_then(|q| q.pending).map(|q| q.id)
    })
    .await;
    let mut post = fx("claude/payload-PostToolUse-Bash.json");
    post["session_id"] = sid(0).into();
    post["tool_input"] = permission(&sid(0), "touch /tmp/a")["tool_input"].clone();
    r.hook("claude", "PostToolUse", &post, &CLI).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        r.core.request(&rid).map(|q| q.status),
        Some(acd_protocol::RequestStatus::Open),
        "still shadowed after eviction"
    );
}

#[tokio::test]
async fn holds_beyond_the_track_cap_are_released_at_once() {
    let r = rig(&[], |c| c.max_tracks = 4).await;
    let mut held = Vec::new();
    for i in 0..4 {
        let sid = format!("5f4996ff-0000-4000-8000-{i:012}");
        held.push(r.hold(&permission(&sid, "touch /tmp/a"), &CLI).await);
    }
    until("four tracks", || {
        (r.hooks.stats().tracks == 4).then_some(())
    })
    .await;
    let t = Instant::now();
    let s = r
        .hold(
            &permission("5f4996ff-0000-4000-8000-000000000099", "touch /tmp/a"),
            &CLI,
        )
        .await;
    assert_eq!(
        reply_within(s, Duration::from_secs(2)).await,
        (200, String::new()),
        "A5: the terminal answers it"
    );
    assert!(t.elapsed() < Duration::from_millis(500));
    assert_eq!(r.hooks.stats().tracks, 4);
}

#[tokio::test]
async fn reevaluations_reuse_the_resolved_socket_paths() {
    // Task 11 follow-up: a header socket is resolved once per session and
    // header value, a pane socket once per path; scans, T3 changes and later
    // hooks of the session reuse them, so a stalled mount is not touched on
    // every re-evaluation.
    let r = rig(&[fake_agent("w1:p1", "claude", "working")], |_| {}).await;
    let (agent, h) = bound(&r);
    let h = refs(&h);
    let mut p = fx("claude/payload-UserPromptSubmit.json");
    p["session_id"] = SID.into();
    r.hook("claude", "UserPromptSubmit", &p, &h).await;
    until("bound", || {
        (r.agent(&agent).is_some() && r.agent(&format!("claude:{SID}")).is_none()).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let n = r.hooks.socket_resolutions();
    assert!(n > 0);
    for i in 0..5 {
        r.hook("claude", "UserPromptSubmit", &p, &h).await;
        r.t3.set_for_tests(&format!("t3:other-{i}"), Some(&format!("other-{i}")));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(r.hooks.socket_resolutions(), n, "nothing resolved again");
    assert!(r.agent(&format!("claude:{SID}")).is_none(), "still bound");
}

/// Sol R3-5 (A7, L1): the keys engine itself holds the delivery lock; one
/// of its herdr calls is held by the fake until the client's deadline. A
/// takeover must complete within 2 x CALL_TIMEOUT (+ slack).
async fn l1_rig() -> (Rig, String, String) {
    // herdr's own watch must not poll p1 (it would take the read gate).
    let r = rig_with(
        &[fake_agent("w1:p1", "claude", "blocked")],
        |c| c.held_dialog_poll = Duration::from_secs(60),
        |h| h.timing.poll = Duration::from_secs(60),
    )
    .await;
    let (agent, h) = bound(&r);
    let h = refs(&h);
    let held = r
        .hold(&fx("claude/payload-PermissionRequest-Bash.json"), &h)
        .await;
    let rid = until("request", || {
        r.agent(&agent).and_then(|a| a.pending).map(|p| p.id)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(700)).await; // captured
    drop(held); // switch to keys
    until("target", || {
        r.herdr.as_ref().unwrap().keys_targets().get(&rid)
    })
    .await;
    (r, agent, rid)
}

fn answering(r: &Rig, rid: &str) -> tokio::task::JoinHandle<acd_protocol::CommandResult> {
    let core = r.core.clone();
    let rid = rid.to_owned();
    tokio::spawn(async move {
        core.handle(
            "c",
            ClientMessage::RequestAnswer {
                id: "a1".into(),
                request_id: rid,
                revision: None,
                option_id: Some("allow".into()),
                text: None,
            },
        )
        .await
    })
}

async fn takeover_within_l1(r: &Rig, rid: &str) {
    let t = Instant::now();
    r.t3.set_for_tests("t3:thread-1", Some(SID));
    let limit = 2 * acd_adapters::herdr::client::CALL_TIMEOUT + Duration::from_millis(1000);
    tokio::time::timeout(limit, async {
        while r.core.request(rid).is_some() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("L1: takeover still waiting after {:?}", t.elapsed()));
}

#[tokio::test]
async fn a_real_keys_holder_stalled_in_its_guard_read_releases_within_l1() {
    let (r, agent, rid) = l1_rig().await;
    let f = r.fake.as_ref().unwrap();
    // Never released: the client's deadline ends the call.
    let gate = f.block_next("agent.read@w1:p1");
    let answer = answering(&r, &rid);
    // Handshake from the actual holder: the engine's guard read reached
    // herdr, and the delivery lock is provably taken.
    tokio::time::timeout(Duration::from_secs(5), gate.arrived())
        .await
        .expect("the guard read reached the gate");
    let locks = r.herdr.as_ref().unwrap().agent_locks();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), locks.lock(&agent))
            .await
            .is_err(),
        "the keys engine holds the delivery lock during its guard read"
    );
    takeover_within_l1(&r, &rid).await;
    assert!(f.keys_sent().is_empty(), "no key after a failed guard read");
    let _ = answer.await;
    drop(gate);
}

#[tokio::test]
async fn a_real_keys_holder_stalled_in_its_key_write_releases_within_l1() {
    let (r, agent, rid) = l1_rig().await;
    let f = r.fake.as_ref().unwrap();
    // Held before any side effect, never released: the client's deadline
    // ends the call.
    let gate = f.block_next("agent.send_keys");
    let answer = answering(&r, &rid);
    tokio::time::timeout(Duration::from_secs(5), gate.arrived())
        .await
        .expect("the key write reached the gate");
    // The engine holds the lock through the write, not while it waits for
    // herdr's reply: the stalled reply holds nothing.
    let locks = r.herdr.as_ref().unwrap().agent_locks();
    assert!(
        tokio::time::timeout(Duration::from_millis(500), locks.lock(&agent))
            .await
            .is_ok(),
        "a stalled key reply holds no delivery lock"
    );
    takeover_within_l1(&r, &rid).await;
    assert!(f.keys_sent().is_empty(), "the held write never took effect");
    let _ = answer.await;
    drop(gate);
}
