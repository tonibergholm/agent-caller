mod common;
use acd_client::conn::*;
use acd_core::{Auth, Core, ServerOptions};
use acd_core::{Observation, testing::*, tls};
use acd_protocol::*;
use common::*;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn connects_over_pinned_tls_and_mirrors_agents() {
    let d = daemon().await;
    d.core.observe(Observation::AgentUpsert(agent_obs("a1")));
    let h = spawn_machine(tcp(&d, TOKEN));
    let mut rx = h.state();
    wait_for(&mut rx, "a1 online", |s| {
        s.status == MachineStatus::Online && s.session.agents.contains_key("a1")
    })
    .await;
    let mut obs = agent_obs("a1");
    obs.state = AgentState::Done;
    d.core.observe(Observation::AgentUpsert(obs));
    wait_for(&mut rx, "a1 done", |s| {
        s.session.agents["a1"].state == AgentState::Done
    })
    .await;
}

#[tokio::test]
async fn pin_mismatch_shows_identity_changed_and_sends_nothing() {
    let d = daemon().await;
    let mut cfg = tcp(&d, TOKEN);
    cfg.endpoint = acd_client::endpoint::Endpoint::Tcp {
        host: "127.0.0.1".into(),
        port: d.port,
        pin: tls::pin_of_cert_pem(&tls::generate("other").cert_pem).unwrap(),
    };
    let h = spawn_machine(cfg);
    let mut rx = h.state();
    wait_for(&mut rx, "identity_changed", |s| {
        matches!(s.status, MachineStatus::IdentityChanged { .. })
    })
    .await;
}

#[tokio::test]
async fn reconnects_after_drop_and_catches_up() {
    let d = daemon().await;
    let h = spawn_machine(tcp(&d, TOKEN));
    let mut rx = h.state();
    let first = wait_for(&mut rx, "online", |s| s.status == MachineStatus::Online)
        .await
        .conn_gen;
    let mut ev = h.events();
    while ev.try_recv().is_ok() {}
    h.drop_connection();
    d.core.observe(Observation::AgentUpsert(agent_obs("late")));
    let st = wait_for(&mut rx, "late agent", |s| {
        s.status == MachineStatus::Online && s.session.agents.contains_key("late")
    })
    .await;
    assert!(st.conn_gen > first);
    let ended = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok((
                _,
                MachineEvent::ConnectionEnded {
                    generation: ended_gen,
                },
            )) = ev.recv().await
            {
                return ended_gen;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(ended, first);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), async {
            loop {
                if let Ok((_, MachineEvent::ConnectionEnded { generation })) = ev.recv().await {
                    return generation;
                }
            }
        })
        .await
        .is_err(),
        "one authenticated connection produces one end event"
    );
    drop(ev);
    let mut ev = h.events();
    h.drop_connection();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok((_, MachineEvent::ConnectionEnded { generation })) = ev.recv().await
                && generation == st.conn_gen
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    drop(h);
}

#[tokio::test]
async fn offline_then_online_when_the_daemon_appears() {
    let tcp_l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = tcp_l.local_addr().unwrap().port();
    drop(tcp_l);
    let files = tls::generate("t");
    let pin = tls::pin_of_cert_pem(&files.cert_pem).unwrap();
    let cfg = MachineConfig {
        key: "m1".into(),
        instance: 1,
        endpoint: acd_client::endpoint::Endpoint::Tcp {
            host: "127.0.0.1".into(),
            port,
            pin,
        },
        credential: Credential::Token(TOKEN.into()),
        client: client(),
        timing: fast(),
    };
    let h = spawn_machine(cfg);
    let mut rx = h.state();
    let s = wait_for(&mut rx, "offline", |s| {
        matches!(s.status, MachineStatus::Offline { .. })
    })
    .await;
    let MachineStatus::Offline { since_ms, .. } = s.status else {
        unreachable!()
    };
    assert!(since_ms > 0.0);
    let l = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let _d = serve_on(l, port, FakeAdapter::new(), files).await;
    wait_for(&mut rx, "online", |s| s.status == MachineStatus::Online).await;
}

#[tokio::test]
async fn remote_unauthorized_stops_until_repaired() {
    let d = daemon().await;
    let h = spawn_machine(tcp(&d, "ZZECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"));
    let mut rx = h.state();
    wait_for(&mut rx, "auth_failed", |s| {
        matches!(s.status, MachineStatus::AuthFailed { .. })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        matches!(rx.borrow().status, MachineStatus::AuthFailed { .. }),
        "no retry loop"
    );
}

#[tokio::test]
async fn local_endpoint_over_unix_socket() {
    let dir = local_dir(TOKEN);
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    let auth = Arc::new(Auth::new(TOKEN.into()));
    let _server = serve_unix(&dir, core, auth).await;
    let h = spawn_machine(local_cfg(&dir));
    let mut rx = h.state();
    wait_for(&mut rx, "local online", |s| {
        s.status == MachineStatus::Online
    })
    .await;
}

#[tokio::test]
async fn local_rotation_rereads_token() {
    let dir = local_dir(TOKEN);
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    let auth = Arc::new(Auth::new(TOKEN.into()));
    let _server = serve_unix(&dir, core, auth.clone()).await;
    let h = spawn_machine(local_cfg(&dir));
    let mut rx = h.state();
    wait_for(&mut rx, "local online", |s| {
        s.status == MachineStatus::Online
    })
    .await;
    let new = "AQECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
    write_token(&dir, new);
    auth.rotate(new.into());
    wait_for(&mut rx, "rotation leaves online", |s| {
        s.status != MachineStatus::Online
    })
    .await;
    wait_for(&mut rx, "re-authenticated", |s| {
        s.status == MachineStatus::Online
    })
    .await;
}

#[tokio::test]
async fn slow_tls_handshakes_do_not_block_others() {
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    let files = tls::generate("slow");
    let pin = tls::pin_of_cert_pem(&files.cert_pem).unwrap();
    let (config_tx, rx) = tokio::sync::watch::channel(tls::server_config(&files).unwrap());
    let listener = tls::TlsListener::new(
        tcp,
        rx,
        tls::Admission::new(tls::MAX_UNAUTHENTICATED_TCP),
        tls::Limits {
            handshake: Duration::from_millis(300),
            preauth_deadline: Duration::from_millis(600),
            max_handshakes: 16,
        },
    );
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    let auth = Arc::new(Auth::new(TOKEN.into()));
    let app = acd_core::router(core, auth, ServerOptions::default());
    let _server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<tls::Admit>(),
        )
        .await
        .unwrap()
    });
    let mut silent = Vec::new();
    for _ in 0..17 {
        silent.push(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap(),
        );
    }
    let cfg = MachineConfig {
        key: "m1".into(),
        instance: 1,
        endpoint: acd_client::endpoint::Endpoint::Tcp {
            host: "127.0.0.1".into(),
            port,
            pin,
        },
        credential: Credential::Token(TOKEN.into()),
        client: client(),
        timing: fast(),
    };
    let h = spawn_machine(cfg);
    let mut rx = h.state();
    wait_for(&mut rx, "online alongside silent handshakes", |s| {
        s.status == MachineStatus::Online
    })
    .await;
    for mut stream in silent {
        let mut b = [0u8; 1];
        let read = tokio::time::timeout(
            Duration::from_secs(1),
            tokio::io::AsyncReadExt::read(&mut stream, &mut b),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(read, 0, "silent TLS socket must reach EOF");
    }
    drop(config_tx);
}

#[tokio::test]
async fn dropping_all_handles_stops_reconnects() {
    let d = daemon().await;
    let h = spawn_machine(tcp(&d, TOKEN));
    let mut rx = h.state();
    wait_for(&mut rx, "online", |s| s.status == MachineStatus::Online).await;
    let mut ev = h.events();
    while ev.try_recv().is_ok() {}
    drop(h);
    drop(rx);
    let closed = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if matches!(
                ev.recv().await,
                Err(tokio::sync::broadcast::error::RecvError::Closed)
            ) {
                break;
            }
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "task exits and drops its event sender after handles are gone"
    );
    drop(ev);
}

#[tokio::test]
async fn offline_submission_does_not_shorten_retry_delay() {
    let tcp_l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = tcp_l.local_addr().unwrap().port();
    drop(tcp_l);
    let files = tls::generate("t");
    let pin = tls::pin_of_cert_pem(&files.cert_pem).unwrap();
    let mut cfg = MachineConfig {
        key: "m1".into(),
        instance: 1,
        endpoint: acd_client::endpoint::Endpoint::Tcp {
            host: "127.0.0.1".into(),
            port,
            pin,
        },
        credential: Credential::Token(TOKEN.into()),
        client: client(),
        timing: fast(),
    };
    cfg.timing.backoff_base = Duration::from_millis(400);
    cfg.timing.backoff_max = Duration::from_millis(400);
    let h = spawn_machine(cfg);
    let mut rx = h.state();
    wait_for(&mut rx, "offline", |s| {
        matches!(s.status, MachineStatus::Offline { .. })
    })
    .await;
    let offline_at = tokio::time::Instant::now();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let _d = serve_on(listener, port, FakeAdapter::new(), files).await;
    let outcome = h
        .submit(
            acd_client::commands::Command::MarkSeen {
                agent_id: "x".into(),
            },
            None,
        )
        .await;
    assert!(matches!(outcome, acd_client::commands::Outcome::NotSent(_)));
    wait_for(&mut rx, "scheduled retry connects", |s| {
        s.status == MachineStatus::Online
    })
    .await;
    // The listener was up the whole time, so only the 400 ms backoff can
    // delay the reconnect; a Submit that cut the wait short connects sooner.
    assert!(
        offline_at.elapsed() >= Duration::from_millis(350),
        "reconnected after {:?}, before the scheduled retry",
        offline_at.elapsed()
    );
}

#[tokio::test]
async fn answer_reaches_the_adapter_once() {
    let d = daemon().await;
    d.core.observe(Observation::AgentUpsert(agent_obs("a1")));
    d.core
        .observe(Observation::RequestSeen(permission_obs("a1", "k1")));
    let h = spawn_machine(tcp(&d, TOKEN));
    let mut rx = h.state();
    let s = wait_for(&mut rx, "pending", |s| {
        s.status == MachineStatus::Online
            && s.session
                .agents
                .get("a1")
                .is_some_and(|a| a.pending.is_some())
    })
    .await;
    let rid = s.session.agents["a1"].pending.as_ref().unwrap().id.clone();
    assert_eq!(
        h.submit(
            acd_client::commands::Command::Answer {
                request_id: rid,
                option_id: "allow".into()
            },
            None
        )
        .await,
        acd_client::commands::Outcome::Delivered
    );
    assert_eq!(d.fake.delivered().len(), 1);
}

#[tokio::test]
async fn rotation_revokes_inflight_and_never_resends() {
    let dir = local_dir(TOKEN);
    let fake = FakeAdapter::gated();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let auth = Arc::new(Auth::new(TOKEN.into()));
    serve_unix(&dir, core.clone(), auth.clone()).await;
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    core.observe(Observation::RequestSeen(permission_obs("a1", "k1")));
    let h = Arc::new(spawn_machine(local_cfg(&dir)));
    let mut rx = h.state();
    let s = wait_for(&mut rx, "pending", |s| {
        s.status == MachineStatus::Online
            && s.session
                .agents
                .get("a1")
                .is_some_and(|a| a.pending.is_some())
    })
    .await;
    let rid = s.session.agents["a1"].pending.as_ref().unwrap().id.clone();
    let h2 = h.clone();
    let task = tokio::spawn(async move {
        h2.submit(
            acd_client::commands::Command::Answer {
                request_id: rid,
                option_id: "allow".into(),
            },
            None,
        )
        .await
    });
    wait_for(&mut rx, "answering", |s| {
        s.session.agents["a1"]
            .pending
            .as_ref()
            .is_some_and(|r| r.status == RequestStatus::Answering)
    })
    .await;
    let new = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWY";
    write_token(&dir, new);
    auth.rotate(new.into());
    assert_eq!(
        task.await.unwrap(),
        acd_client::commands::Outcome::Uncertain
    );
    wait_for(&mut rx, "online with new token", |s| {
        s.status == MachineStatus::Online
    })
    .await;
    fake.open_gate();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(fake.delivered().len() <= 1);
}

#[tokio::test]
async fn rotation_before_delivery_is_uncertain_and_new_token_works() {
    let dir = local_dir(TOKEN);
    let fake = FakeAdapter::gated();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let auth = Arc::new(Auth::new(TOKEN.into()));
    serve_unix(&dir, core.clone(), auth.clone()).await;
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    core.observe(Observation::RequestSeen(permission_obs("a1", "k1")));
    let h = Arc::new(spawn_machine(local_cfg(&dir)));
    let mut rx = h.state();
    wait_for(&mut rx, "pending", |s| {
        s.status == MachineStatus::Online
            && s.session
                .agents
                .get("a1")
                .is_some_and(|a| a.pending.is_some())
    })
    .await;
    let rid = rx.borrow().session.agents["a1"]
        .pending
        .as_ref()
        .unwrap()
        .id
        .clone();
    let h2 = h.clone();
    let task = tokio::spawn(async move {
        h2.submit(
            acd_client::commands::Command::Answer {
                request_id: rid,
                option_id: "allow".into(),
            },
            None,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(request) = rx.borrow().session.agents["a1"].pending.as_ref()
                && request.status == RequestStatus::Answering
            {
                break;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let new = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWY";
    write_token(&dir, new);
    auth.rotate(new.into());
    assert_eq!(
        task.await.unwrap(),
        acd_client::commands::Outcome::Uncertain
    );
    wait_for(&mut rx, "online with new token", |s| {
        s.status == MachineStatus::Online
    })
    .await;
    fake.open_gate();
    core.observe(Observation::RequestSeen(permission_obs("a1", "k2")));
    let state = wait_for(&mut rx, "new request on rotated connection", |s| {
        s.status == MachineStatus::Online
            && s.session.agents["a1"]
                .pending
                .as_ref()
                .is_some_and(|request| request.status == RequestStatus::Open)
    })
    .await;
    let rid = state.session.agents["a1"]
        .pending
        .as_ref()
        .unwrap()
        .id
        .clone();
    assert_eq!(
        h.submit(
            acd_client::commands::Command::Answer {
                request_id: rid,
                option_id: "allow".into()
            },
            None
        )
        .await,
        acd_client::commands::Outcome::Delivered
    );
}

#[tokio::test]
async fn offline_submit_is_not_sent() {
    let tcp_l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = tcp_l.local_addr().unwrap().port();
    drop(tcp_l);
    let files = tls::generate("offline");
    let pin = tls::pin_of_cert_pem(&files.cert_pem).unwrap();
    let cfg = MachineConfig {
        key: "m1".into(),
        instance: 1,
        endpoint: acd_client::endpoint::Endpoint::Tcp {
            host: "127.0.0.1".into(),
            port,
            pin,
        },
        credential: Credential::Token(TOKEN.into()),
        client: client(),
        timing: fast(),
    };
    let h = spawn_machine(cfg);
    assert_eq!(
        h.submit(
            acd_client::commands::Command::MarkSeen {
                agent_id: "a".into()
            },
            None
        )
        .await,
        acd_client::commands::Outcome::NotSent(acd_client::commands::NotSent::Offline)
    );
}
