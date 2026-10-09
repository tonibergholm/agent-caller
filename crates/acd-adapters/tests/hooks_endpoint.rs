use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use acd_adapters::hooks::endpoint::*;
use acd_adapters::hooks::testing::{post, read_response, send};
use acd_adapters::hooks::wire::HookName;
use tokio::time::{Instant, timeout};

const TOKEN: &str = "tok-current-0123456789";

fn cred(token: &str, generation: u64) -> Credential {
    Credential {
        token: token.into(),
        generation,
    }
}

async fn serve(opts: EndpointOptions) -> (u16, Arc<HookAuth>, Mailbox) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mailbox) = mailbox(&opts);
    let auth = Arc::new(HookAuth::new(cred(TOKEN, 1)));
    let app = router(auth.clone(), tx, opts.clone());
    tokio::spawn(async move {
        axum::serve(capped(listener, opts.max_connections), app)
            .await
            .unwrap()
    });
    (port, auth, mailbox)
}

fn fx(rel: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/hooks/{rel}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn next_event(mailbox: &mut Mailbox) -> Inbound {
    let message = timeout(Duration::from_secs(2), mailbox.events.recv())
        .await
        .expect("event in time")
        .unwrap();
    if let Inbound::Event { bytes, .. } = &message {
        mailbox.release(*bytes);
    }
    message
}

async fn next_control(mailbox: &mut Mailbox) -> Inbound {
    timeout(Duration::from_secs(2), mailbox.control.recv())
        .await
        .expect("control in time")
        .unwrap()
}

#[tokio::test]
async fn rejects_bad_tokens_methods_paths_and_agents() {
    let (port, _, _) = serve(EndpointOptions::default()).await;
    let body = fx("claude/payload-UserPromptSubmit.json");
    assert_eq!(
        post(port, "/hook/claude/UserPromptSubmit", "", &[], &body)
            .await
            .0,
        401
    );
    assert_eq!(
        post(port, "/hook/claude/UserPromptSubmit", "wrong", &[], &body)
            .await
            .0,
        401
    );
    assert_eq!(
        post(port, "/hook/aider/UserPromptSubmit", TOKEN, &[], &body)
            .await
            .0,
        401
    );
    assert_eq!(post(port, "/elsewhere", TOKEN, &[], &body).await.0, 401);
    assert_eq!(
        post(port, "/hook/%FF/Stop", TOKEN, &[], &body).await.0,
        401,
        "a malformed path segment is unauthorized, not a 400"
    );
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    tokio::io::AsyncWriteExt::write_all(
        &mut stream,
        format!("GET /hook/claude/Stop HTTP/1.1\r\nHost: x\r\nX-Acd-Token: {TOKEN}\r\nConnection: close\r\n\r\n").as_bytes(),
    ).await.unwrap();
    assert_eq!(read_response(stream).await.0, 401);
    assert_eq!(
        post(
            port,
            "/hook/claude/UserPromptSubmit?marker=acd-managed:v1",
            TOKEN,
            &[],
            &body
        )
        .await
        .0,
        200
    );
}

#[tokio::test]
async fn each_request_carries_the_generation_it_matched() {
    let (port, auth, mut mailbox) = serve(EndpointOptions::default()).await;
    auth.set(cred("tok-new-0123456789", 2), Some(cred(TOKEN, 1)));
    let body = fx("claude/payload-UserPromptSubmit.json");
    assert_eq!(
        post(port, "/hook/claude/UserPromptSubmit", TOKEN, &[], &body)
            .await
            .0,
        200
    );
    assert!(matches!(
        next_event(&mut mailbox).await,
        Inbound::Event { generation: 1, .. }
    ));
    assert_eq!(
        post(
            port,
            "/hook/claude/UserPromptSubmit",
            "tok-new-0123456789",
            &[],
            &body
        )
        .await
        .0,
        200
    );
    assert!(matches!(
        next_event(&mut mailbox).await,
        Inbound::Event { generation: 2, .. }
    ));
    auth.set(cred("tok-new-0123456789", 2), None);
    let before = Instant::now();
    assert_eq!(
        post(
            port,
            "/hook/claude/UserPromptSubmit",
            TOKEN,
            &[("X-Herdr-Pane", "w1:p1")],
            &body
        )
        .await
        .0,
        401
    );
    assert_eq!(auth.rejected_since(before), 1);
}

#[test]
fn rejections_are_logged_by_an_opaque_id_and_the_table_is_capped() {
    let id = remote_id(Some("w1:p1 secret-looking value"));
    assert_eq!(id.len(), 8);
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(!id.contains("w1"));
    assert_eq!(remote_id(None), "none");
    let auth = HookAuth::new(cred(TOKEN, 1));
    for i in 0..300 {
        assert_eq!(auth.check(&format!("bad-{i}")), None);
        auth.note_rejected(Some(&format!("pane-{i}")));
    }
    assert!(auth.rejected_entries() <= 256);
}

#[tokio::test]
async fn lifecycle_hooks_reply_at_once_without_a_consumer() {
    // A live mailbox that is never drained (not a closed one).
    let (port, _, _mailbox) = serve(EndpointOptions::default()).await;
    let started = Instant::now();
    assert_eq!(
        post(
            port,
            "/hook/claude/Stop",
            TOKEN,
            &[],
            &fx("claude/payload-Stop.json")
        )
        .await,
        (200, String::new())
    );
    assert!(started.elapsed() < Duration::from_millis(200));
}

#[tokio::test]
async fn a_full_event_queue_fails_open_but_never_drops_control() {
    let (port, _, mut mailbox) = serve(EndpointOptions {
        max_events: 2,
        ..Default::default()
    })
    .await;
    let stop = fx("claude/payload-Stop.json");
    for _ in 0..3 {
        assert_eq!(
            post(port, "/hook/claude/Stop", TOKEN, &[], &stop).await,
            (200, String::new())
        );
    }
    assert_eq!(mailbox.dropped(), 1);
    let stream = send(
        port,
        "/hook/claude/PermissionRequest",
        TOKEN,
        &[],
        &fx("claude/payload-PermissionRequest-Bash.json"),
    )
    .await;
    // Keep the reply sender: dropping it would release the hold (empty
    // reply, no `HoldClosed`), which is not what this test is about.
    let Inbound::Held {
        hold, reply: _keep, ..
    } = next_control(&mut mailbox).await
    else {
        panic!("hold on control channel")
    };
    drop(stream);
    assert!(
        matches!(next_control(&mut mailbox).await, Inbound::HoldClosed { hold: closed } if closed == hold)
    );
}

#[tokio::test]
async fn the_byte_budget_bounds_queued_bodies() {
    let (port, _, mailbox) = serve(EndpointOptions {
        max_event_bytes: 1500,
        ..Default::default()
    })
    .await;
    let stop = fx("claude/payload-Stop.json");
    post(port, "/hook/claude/Stop", TOKEN, &[], &stop).await;
    post(port, "/hook/claude/Stop", TOKEN, &[], &stop).await;
    assert_eq!(mailbox.dropped(), 1);
}

#[tokio::test]
async fn connections_beyond_the_cap_wait() {
    let (port, _, mut mailbox) = serve(EndpointOptions {
        max_connections: 1,
        ..Default::default()
    })
    .await;
    let idle = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let waiting = tokio::spawn(async move {
        post(
            port,
            "/hook/claude/Stop",
            TOKEN,
            &[],
            &fx("claude/payload-Stop.json"),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!waiting.is_finished());
    drop(idle);
    assert_eq!(waiting.await.unwrap().0, 200);
    let _ = next_event(&mut mailbox).await;
}

#[tokio::test]
async fn a_held_permission_returns_the_adapter_reply() {
    let (port, _, mut mailbox) = serve(EndpointOptions::default()).await;
    let stream = send(
        port,
        "/hook/claude/PermissionRequest",
        TOKEN,
        &[
            ("X-Herdr-Pane", "w1:p2"),
            ("X-Herdr-Socket", ""),
            ("X-Claude-Entrypoint", "cli"),
        ],
        &fx("claude/payload-PermissionRequest-Bash.json"),
    )
    .await;
    let Inbound::Held {
        event,
        reply,
        generation,
        ..
    } = next_control(&mut mailbox).await
    else {
        panic!("not held")
    };
    assert_eq!(
        (event.name.clone(), generation),
        (HookName::PermissionRequest, 1)
    );
    assert_eq!(event.hints.herdr_pane.as_deref(), Some("w1:p2"));
    assert_eq!(event.hints.herdr_socket, None);
    reply
        .send(acd_adapters::hooks::wire::reply_allow())
        .unwrap();
    assert_eq!(
        read_response(stream).await,
        (200, acd_adapters::hooks::wire::reply_allow())
    );
}

#[tokio::test]
async fn a_released_hold_replies_empty() {
    let (port, _, mut mailbox) = serve(EndpointOptions::default()).await;
    let stream = send(
        port,
        "/hook/claude/PermissionRequest",
        TOKEN,
        &[],
        &fx("claude/payload-PermissionRequest-Bash.json"),
    )
    .await;
    let Inbound::Held { reply, .. } = next_control(&mut mailbox).await else {
        panic!()
    };
    drop(reply);
    assert_eq!(read_response(stream).await, (200, String::new()));
}

#[tokio::test]
async fn release_answers_open_and_later_holds_empty_and_reports_them_closed() {
    let opts = EndpointOptions::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut mailbox) = mailbox(&opts);
    let auth = Arc::new(HookAuth::new(cred(TOKEN, 1)));
    let release = Release::default();
    let app = router_with_release(auth, tx, opts.clone(), release.clone());
    tokio::spawn(async move {
        axum::serve(capped(listener, opts.max_connections), app)
            .await
            .unwrap()
    });
    let body = fx("claude/payload-PermissionRequest-Bash.json");
    let stream = send(port, "/hook/claude/PermissionRequest", TOKEN, &[], &body).await;
    // The reconciler keeps the reply sender: only the release can end it.
    let Inbound::Held { hold, reply, .. } = next_control(&mut mailbox).await else {
        panic!("not held")
    };
    release.release();
    assert_eq!(
        timeout(Duration::from_secs(2), read_response(stream))
            .await
            .expect("released in time"),
        (200, String::new())
    );
    match next_control(&mut mailbox).await {
        Inbound::HoldClosed { hold: closed } => assert_eq!(closed, hold),
        other => panic!("{other:?}"),
    }
    drop(reply);
    // A holdable request after the release is answered at once.
    let started = Instant::now();
    assert_eq!(
        post(port, "/hook/claude/PermissionRequest", TOKEN, &[], &body).await,
        (200, String::new())
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn a_client_close_is_seen_as_hold_closed() {
    let (port, _, mut mailbox) = serve(EndpointOptions::default()).await;
    let stream = send(
        port,
        "/hook/claude/PermissionRequest",
        TOKEN,
        &[],
        &fx("claude/payload-PermissionRequest-Bash.json"),
    )
    .await;
    let Inbound::Held { hold, reply, .. } = next_control(&mut mailbox).await else {
        panic!()
    };
    drop(stream);
    let started = Instant::now();
    match next_control(&mut mailbox).await {
        Inbound::HoldClosed { hold: closed } => assert_eq!(closed, hold),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(reply.is_closed());
}

#[tokio::test]
async fn hold_expiry_replies_empty_and_reports_it() {
    let (port, _, mut mailbox) = serve(EndpointOptions {
        permission_hold: Duration::from_millis(200),
        ..Default::default()
    })
    .await;
    let stream = send(
        port,
        "/hook/claude/PermissionRequest",
        TOKEN,
        &[],
        &fx("claude/payload-PermissionRequest-Bash.json"),
    )
    .await;
    let Inbound::Held {
        hold,
        reply: _keep,
        permit: _permit,
        ..
    } = next_control(&mut mailbox).await
    else {
        panic!()
    };
    assert_eq!(read_response(stream).await, (200, String::new()));
    assert!(
        matches!(next_control(&mut mailbox).await, Inbound::HoldExpired { hold: expired } if expired == hold)
    );
}

#[tokio::test]
async fn only_claude_tool_permissions_are_held() {
    let (port, _, mut mailbox) = serve(EndpointOptions::default()).await;
    for (path, fixture) in [
        (
            "/hook/claude/PermissionRequest",
            "claude/payload-PermissionRequest-AskUserQuestion.json",
        ),
        (
            "/hook/codex/PermissionRequest",
            "codex/payload-PermissionRequest-Bash.json",
        ),
    ] {
        assert_eq!(
            post(port, path, TOKEN, &[], &fx(fixture)).await,
            (200, String::new())
        );
        assert!(matches!(
            next_event(&mut mailbox).await,
            Inbound::Event { .. }
        ));
    }
}

#[tokio::test]
async fn holds_beyond_the_cap_fail_open() {
    let (port, _, mut mailbox) = serve(EndpointOptions {
        max_holds: 1,
        ..Default::default()
    })
    .await;
    let body = fx("claude/payload-PermissionRequest-Bash.json");
    let _first = send(port, "/hook/claude/PermissionRequest", TOKEN, &[], &body).await;
    let Inbound::Held {
        reply: _keep,
        permit: _permit,
        ..
    } = next_control(&mut mailbox).await
    else {
        panic!()
    };
    assert_eq!(
        post(port, "/hook/claude/PermissionRequest", TOKEN, &[], &body).await,
        (200, String::new())
    );
    assert!(matches!(
        next_event(&mut mailbox).await,
        Inbound::Event { .. }
    ));
}

const PR: &str = "/hook/claude/PermissionRequest";

/// Waits until the endpoint has queued `total` messages over both channels,
/// without consuming any. The disconnect tests close a connection only
/// after its request was processed: a connection closed before the server
/// reads it is never dispatched (hyper sees the EOF first), so it would
/// test nothing.
async fn settle(mailbox: &Mailbox, total: usize) {
    timeout(Duration::from_secs(2), async {
        while mailbox.control.len() + mailbox.events.len() < total {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{total} messages queued in time"));
}

/// Opens `n` holds and closes each once it is queued; each leaves a `Held`
/// and a `HoldClosed` on the control channel.
async fn hold_and_close(port: u16, mailbox: &Mailbox, body: &str, n: usize) {
    for i in 0..n {
        let stream = send(port, PR, TOKEN, &[], body).await;
        settle(mailbox, 2 * i + 1).await;
        drop(stream);
        settle(mailbox, 2 * i + 2).await;
    }
    assert_eq!(
        mailbox.control.len(),
        2 * n,
        "{n} holds admitted and closed"
    );
}

fn drain_control(mailbox: &mut Mailbox) -> usize {
    let mut count = 0;
    while mailbox.control.try_recv().is_ok() {
        count += 1;
    }
    count
}

#[tokio::test]
async fn a_disconnected_hold_keeps_its_permit_until_its_messages_are_consumed() {
    // Sol N2: closing the connection does not free the slot; consuming the
    // queued `Held` (and its `HoldClosed`) does.
    let (port, _, mut mailbox) = serve(EndpointOptions {
        max_holds: 2,
        ..Default::default()
    })
    .await;
    let body = fx("claude/payload-PermissionRequest-Bash.json");
    hold_and_close(port, &mailbox, &body, 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    let reply = timeout(Duration::from_secs(2), post(port, PR, TOKEN, &[], &body))
        .await
        .expect("no slot: answered at once, not held");
    assert_eq!(reply, (200, String::new()), "no slot: fails open");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "at once, not after a hold"
    );
    assert!(matches!(
        next_event(&mut mailbox).await,
        Inbound::Event { .. }
    ));
    assert_eq!(
        drain_control(&mut mailbox),
        4,
        "two Held and two HoldClosed"
    );
    let _stream = send(port, PR, TOKEN, &[], &body).await;
    assert!(
        matches!(next_control(&mut mailbox).await, Inbound::Held { .. }),
        "a slot is free again"
    );
}

#[tokio::test]
async fn a_consumed_hold_keeps_its_slot_while_the_adapter_owns_the_permit() {
    // A1: taking `Held` off the control channel does not free the slot;
    // dropping the permit (the adapter is done with the hold) does.
    let (port, _, mut mailbox) = serve(EndpointOptions {
        max_holds: 2,
        ..Default::default()
    })
    .await;
    let body = fx("claude/payload-PermissionRequest-Bash.json");
    let mut owned = Vec::new();
    for _ in 0..2 {
        let stream = send(port, PR, TOKEN, &[], &body).await;
        let Inbound::Held { reply, permit, .. } = next_control(&mut mailbox).await else {
            panic!("not held")
        };
        owned.push((stream, reply, permit));
    }
    let reply = timeout(Duration::from_secs(2), post(port, PR, TOKEN, &[], &body))
        .await
        .expect("no slot: answered at once, not held");
    assert_eq!(reply, (200, String::new()), "no slot: fails open");
    assert!(matches!(
        next_event(&mut mailbox).await,
        Inbound::Event { .. }
    ));
    let (stream, reply, permit) = owned.pop().unwrap();
    reply.send(String::new()).unwrap();
    drop(permit);
    assert_eq!(read_response(stream).await, (200, String::new()));
    let _stream = send(port, PR, TOKEN, &[], &body).await;
    assert!(
        matches!(next_control(&mut mailbox).await, Inbound::Held { .. }),
        "a slot is free again"
    );
}

fn direct_request(body: &str) -> axum::extract::Request {
    axum::http::Request::post("/")
        .header("x-acd-token", TOKEN)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_owned()))
        .unwrap()
}

/// Polls `fut` exactly once.
async fn poll_once<F: Future + Unpin>(fut: &mut F) -> Poll<F::Output> {
    std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut *fut).poll(cx))).await
}

#[tokio::test]
async fn a_replied_hold_keeps_its_close_slot_until_its_handler_ends() {
    // R10: the adapter answered hold A and freed its permit, but A's handler
    // has not run since (hyper has not polled it yet). A still owns its
    // reserved close slot, so B fails open instead of taking a slot that
    // A's `HoldClosed` would need.
    let opts = EndpointOptions {
        max_holds: 1,
        ..Default::default()
    };
    let (tx, mut mailbox) = mailbox(&opts);
    let hooks = DirectHooks::new(Arc::new(HookAuth::new(cred(TOKEN, 1))), tx, opts);
    let body = fx("claude/payload-PermissionRequest-Bash.json");
    let call = || Box::pin(hooks.call("claude", "PermissionRequest", direct_request(&body)));

    let mut a = call();
    assert!(poll_once(&mut a).await.is_pending(), "A is held");
    let Ok(Inbound::Held {
        hold: a_hold,
        reply,
        permit,
        ..
    }) = mailbox.control.try_recv()
    else {
        panic!("A's Held is queued")
    };
    reply
        .send(acd_adapters::hooks::wire::reply_allow())
        .unwrap();
    drop(permit); // A1: the adapter is done with A; A is not polled again.

    let mut b = call();
    let Poll::Ready(response) = poll_once(&mut b).await else {
        panic!("B is answered at once, not held")
    };
    assert_eq!(response.status(), 200);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(bytes.is_empty(), "B fails open with an empty reply");
    assert!(
        matches!(mailbox.events.try_recv(), Ok(Inbound::Event { .. })),
        "an unheld copy of B for the keys path"
    );
    assert!(mailbox.control.try_recv().is_err(), "B was not held");

    drop(a); // The client closed before A's handler saw the reply.
    assert!(
        matches!(mailbox.control.try_recv(), Ok(Inbound::HoldClosed { hold }) if hold == a_hold),
        "A's HoldClosed is delivered"
    );

    let mut c = call();
    assert!(poll_once(&mut c).await.is_pending());
    assert!(
        matches!(mailbox.control.try_recv(), Ok(Inbound::Held { .. })),
        "a new hold is admitted once A's slot is free"
    );
}

#[tokio::test]
async fn churning_disconnected_holds_never_grows_the_control_queue() {
    let (port, _, mut mailbox) = serve(EndpointOptions {
        max_holds: 4,
        ..Default::default()
    })
    .await;
    let body = fx("claude/payload-PermissionRequest-Bash.json");
    hold_and_close(port, &mailbox, &body, 4).await;
    for i in 0..196 {
        let stream = send(port, PR, TOKEN, &[], &body).await;
        settle(&mailbox, 8 + i + 1).await;
        drop(stream);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        mailbox.events.len(),
        196,
        "every hold past the cap failed open"
    );
    let queued = drain_control(&mut mailbox);
    assert!(
        queued <= 2 * 4,
        "{queued} control messages retained for 4 permits"
    );
    assert_eq!(queued, 8, "four Held and four HoldClosed");
}

#[tokio::test]
async fn oversized_and_non_object_bodies_fail_open() {
    let (port, _, mut mailbox) = serve(EndpointOptions::default()).await;
    let big = format!("{{\"session_id\":\"s\",\"x\":\"{}\"}}", "a".repeat(1 << 20));
    assert_eq!(
        post(port, "/hook/claude/Stop", TOKEN, &[], &big).await.0,
        413
    );
    assert_eq!(
        post(port, "/hook/claude/Stop", TOKEN, &[], "[1,2]").await,
        (200, String::new())
    );
    assert_eq!(
        post(port, "/hook/claude/Stop", TOKEN, &[], "{\"cwd\":\"/\"}").await,
        (200, String::new())
    );
    assert!(
        timeout(Duration::from_millis(200), mailbox.events.recv())
            .await
            .is_err()
    );
}

#[cfg(unix)]
async fn unix_send(path: &std::path::Path, token: &str, body: &str) -> tokio::net::UnixStream {
    use tokio::io::AsyncWriteExt;
    let mut stream = tokio::net::UnixStream::connect(path).await.unwrap();
    let req = format!(
        "POST /hook/claude/UserPromptSubmit HTTP/1.1\r\nHost: local\r\nX-Acd-Token: {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    stream
}

#[cfg(unix)]
async fn unix_post(path: &std::path::Path, token: &str, body: &str) -> u16 {
    use tokio::io::AsyncReadExt;
    let mut stream = unix_send(path, token, body).await;
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).await;
    let response = String::from_utf8_lossy(&response);
    response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

#[cfg(unix)]
#[tokio::test]
async fn unix_listener_serves_the_same_authenticated_endpoint_and_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hook.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let opts = EndpointOptions {
        max_connections: 1,
        ..Default::default()
    };
    let (tx, mut mailbox) = mailbox(&opts);
    let auth = Arc::new(HookAuth::new(cred(TOKEN, 1)));
    let app = router(auth, tx, opts.clone());
    tokio::spawn(async move {
        serve_unix(listener, app, opts.max_connections)
            .await
            .unwrap()
    });
    let body = fx("claude/payload-UserPromptSubmit.json");

    assert_eq!(unix_post(&path, "wrong", &body).await, 401);
    assert_eq!(unix_post(&path, TOKEN, &body).await, 200);
    assert!(matches!(
        next_event(&mut mailbox).await,
        Inbound::Event { generation: 1, .. }
    ));

    // The connection cap applies to the Unix listener too.
    let idle = tokio::net::UnixStream::connect(&path).await.unwrap();
    let waiting = {
        let path = path.clone();
        let body = body.clone();
        tokio::spawn(async move { unix_post(&path, TOKEN, &body).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !waiting.is_finished(),
        "the second connection waits for a slot"
    );
    drop(idle);
    assert_eq!(waiting.await.unwrap(), 200);
    assert!(matches!(
        next_event(&mut mailbox).await,
        Inbound::Event { .. }
    ));
}
