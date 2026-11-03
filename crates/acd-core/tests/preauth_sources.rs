//! One anonymous peer must not be able to fill the pre-auth slots for
//! everyone, including the local socket (security review, finding 1).
//! The real router is served the way the daemon serves it: clones of one
//! router on a TLS listener and on a Unix socket.

use acd_core::{Auth, Core, ServerOptions, router, testing::*, tls};
use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpSocket, UnixListener, UnixStream},
    sync::watch,
};
use tokio_tungstenite::{WebSocketStream, client_async, tungstenite::Message};

/// Every network step is bounded, so a hung helper fails the test with a
/// message instead of hanging the run.
const STEP: Duration = Duration::from_secs(5);

/// Deadlines for tests that must find an attack still holding its slots:
/// far longer than any CI runner needs to open a dozen connections.
const HOLD: Duration = Duration::from_secs(60);

async fn within<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(STEP, f)
        .await
        .unwrap_or_else(|_| panic!("{what} timed out after {STEP:?}"))
}

/// Loopback aliases other than 127.0.0.1 exist on Linux, not by default on
/// macOS. Tests that need distinct sources skip when they cannot bind them.
fn can_bind(ips: &[&str]) -> bool {
    ips.iter().all(|ip| match std::net::TcpListener::bind(format!("{ip}:0")) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("SKIPPED: cannot bind {ip}: {e}");
            false
        }
    })
}

struct Served {
    tcp: SocketAddr,
    unix: std::path::PathBuf,
    dir: std::path::PathBuf,
    _tx: watch::Sender<Arc<rustls::ServerConfig>>,
}

async fn serve() -> Served {
    serve_with(HOLD, HOLD, HOLD).await
}

async fn serve_with(hello: Duration, handshake: Duration, preauth: Duration) -> Served {
    let f = tls::generate("t");
    let (tx, rx) = watch::channel(tls::server_config(&f).unwrap());
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    let limits = tls::Limits {
        handshake,
        preauth_deadline: preauth,
        max_handshakes: 16,
    };
    let listener = tls::TlsListener::new(tcp, rx, tls::Admission::new(32), limits);
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    let app = router(
        core,
        Arc::new(Auth::new("secret".into())),
        ServerOptions {
            hello_timeout: hello,
            ..Default::default()
        },
    );
    let tls_app = app.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            tls_app.into_make_service_with_connect_info::<tls::Admit>(),
        )
        .await
        .unwrap()
    });
    let dir = std::env::temp_dir().join(format!(
        "acd-preauth-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let unix = dir.join("acd.sock");
    let ul = UnixListener::bind(&unix).unwrap();
    tokio::spawn(async move { axum::serve(ul, app).await.unwrap() });
    Served {
        tcp: addr,
        unix,
        dir,
        _tx: tx,
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[derive(Debug)]
struct AcceptAny;
impl rustls::client::danger::ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
        ]
    }
}

type TlsWs = WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

/// TCP connect from `source`, TLS, then the WebSocket upgrade. `None` when
/// the server refuses at any step.
async fn open_tls(server: SocketAddr, source: &str) -> Option<TlsWs> {
    within("open_tls", open_tls_inner(server, source)).await
}

async fn open_tls_inner(server: SocketAddr, source: &str) -> Option<TlsWs> {
    let sock = TcpSocket::new_v4().unwrap();
    sock.bind(format!("{source}:0").parse().unwrap()).unwrap();
    let stream = sock.connect(server).await.ok()?;
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(AcceptAny))
    .with_no_client_auth();
    let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("localhost").unwrap().to_owned(), stream)
        .await
        .ok()?;
    client_async("wss://localhost/", tls).await.ok().map(|(ws, _)| ws)
}

async fn open_unix(path: &std::path::Path) -> Option<WebSocketStream<UnixStream>> {
    within("open_unix", async {
        let s = UnixStream::connect(path).await.ok()?;
        client_async("ws://localhost/", s).await.ok().map(|(ws, _)| ws)
    })
    .await
}

/// Sends `hello` and reports whether a `welcome` came back.
async fn hello_ok<S: AsyncRead + AsyncWrite + Unpin>(ws: &mut WebSocketStream<S>) -> bool {
    let hello = acd_protocol::ClientMessage::Hello {
        protocol: acd_protocol::PROTOCOL.into(),
        token: "secret".into(),
        client: acd_protocol::ClientInfo {
            id: "test".into(),
            name: "test".into(),
            version: "test".into(),
        },
        resume: None,
    };
    if within("send hello", ws.send(Message::Text(serde_json::to_string(&hello).unwrap().into())))
        .await
        .is_err()
    {
        return false;
    }
    tokio::time::timeout(STEP, async {
        while let Some(Ok(Message::Text(t))) = ws.next().await {
            if matches!(
                acd_protocol::decode_server(t.as_str()),
                Ok(Some(acd_protocol::ServerMessage::Welcome { .. }))
            ) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

/// The attacker: `n` connections that finish the upgrade and say nothing.
async fn idle_tls(server: SocketAddr, source: &str, n: usize) -> Vec<TlsWs> {
    let mut held = Vec::new();
    for _ in 0..n {
        if let Some(ws) = open_tls(server, source).await {
            held.push(ws);
        }
    }
    held
}

#[tokio::test]
async fn one_source_cannot_lock_the_local_socket_out() {
    let s = serve().await;
    let held = idle_tls(s.tcp, "127.0.0.1", 8).await;
    assert_eq!(held.len(), 2, "the attack is holding its slots");
    let mut local = open_unix(&s.unix).await.expect("local upgrade is not refused");
    assert!(hello_ok(&mut local).await);
}

/// Saturates the whole network pool (8 slots) from four sources, so the
/// per-source cap cannot be what protects the local socket. Fails if the
/// local socket shares the network pool.
#[tokio::test]
async fn a_saturated_network_pool_leaves_the_local_socket_usable() {
    let sources = ["127.0.0.2", "127.0.0.3", "127.0.0.4", "127.0.0.5"];
    if !can_bind(&["127.0.0.2", "127.0.0.3", "127.0.0.4", "127.0.0.5", "127.0.0.6"]) {
        return;
    }
    let s = serve().await;
    let mut held = Vec::new();
    for src in sources {
        let h = idle_tls(s.tcp, src, 2).await;
        assert_eq!(h.len(), 2, "{src} holds its two slots");
        held.extend(h);
    }
    assert_eq!(held.len(), 8);
    // Saturated now: a fresh source (under its own cap) gets HTTP 503.
    assert!(
        open_tls(s.tcp, "127.0.0.6").await.is_none(),
        "network pool is saturated"
    );
    let mut local = open_unix(&s.unix)
        .await
        .expect("local upgrade is not refused while the network pool is full");
    assert!(hello_ok(&mut local).await);
}

#[tokio::test]
async fn one_source_holds_at_most_two_pre_auth_connections() {
    let s = serve().await;
    let held = idle_tls(s.tcp, "127.0.0.1", 8).await;
    assert_eq!(held.len(), 2, "per-source cap");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn one_source_cannot_lock_another_source_out() {
    let s = serve().await;
    let held = idle_tls(s.tcp, "127.0.0.1", 8).await;
    assert_eq!(held.len(), 2, "the attack is holding its slots");
    let mut other = open_tls(s.tcp, "127.0.0.2")
        .await
        .expect("another source is not refused");
    assert!(hello_ok(&mut other).await);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn idle_raw_tcp_from_one_source_does_not_block_another_tls_handshake() {
    let s = serve().await;
    let mut raw = Vec::new();
    for _ in 0..32 {
        let sock = TcpSocket::new_v4().unwrap();
        sock.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        if let Ok(c) = within("raw connect", sock.connect(s.tcp)).await {
            raw.push(c);
        }
    }
    let mut other = open_tls(s.tcp, "127.0.0.2")
        .await
        .expect("another source's handshake is not starved");
    assert!(hello_ok(&mut other).await);
}

/// The slots come back when the server cuts the idle connections. The wait
/// is a poll bounded by `STEP`-sized attempts, not a fixed sleep: the
/// server's own 1 s hello timeout is what frees them.
#[tokio::test]
async fn the_saturating_source_is_released_after_the_hello_timeout() {
    let s = serve_with(
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(3),
    )
    .await;
    let held = idle_tls(s.tcp, "127.0.0.1", 8).await;
    assert_eq!(held.len(), 2, "slots were held before the deadline");
    assert!(
        open_tls(s.tcp, "127.0.0.1").await.is_none(),
        "still capped before the deadline (a failure here means the runner \
         took longer than the 1 s hello timeout to get this far)"
    );
    let give_up = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut again = loop {
        if let Some(ws) = open_tls(s.tcp, "127.0.0.1").await {
            break ws;
        }
        assert!(tokio::time::Instant::now() < give_up, "slots never came back");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(hello_ok(&mut again).await);
    drop(held);
}

#[tokio::test]
async fn filling_the_local_pool_does_not_block_tcp() {
    let s = serve().await;
    let mut local = Vec::new();
    for _ in 0..8 {
        local.push(open_unix(&s.unix).await.expect("local upgrade"));
    }
    assert_eq!(local.len(), 8, "all eight local slots are held");
    assert!(open_unix(&s.unix).await.is_none(), "local pool is full");
    let mut remote = open_tls(s.tcp, "127.0.0.1")
        .await
        .expect("tcp pool is separate from the local pool");
    assert!(hello_ok(&mut remote).await);
}
