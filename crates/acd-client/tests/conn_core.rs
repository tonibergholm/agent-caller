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
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::io::AsyncReadExt::read(&mut stream, &mut b),
        )
        .await
        .unwrap()
        .unwrap();
    }
    drop(config_tx);
}
