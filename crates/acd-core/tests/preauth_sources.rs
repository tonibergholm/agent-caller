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

const HELLO_TIMEOUT: Duration = Duration::from_millis(600);

struct Served {
    tcp: SocketAddr,
    unix: std::path::PathBuf,
    dir: std::path::PathBuf,
    _tx: watch::Sender<Arc<rustls::ServerConfig>>,
}

async fn serve() -> Served {
    let f = tls::generate("t");
    let (tx, rx) = watch::channel(tls::server_config(&f).unwrap());
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    let limits = tls::Limits {
        handshake: Duration::from_millis(300),
        preauth_deadline: Duration::from_millis(1500),
        max_handshakes: 16,
    };
    let listener = tls::TlsListener::new(tcp, rx, tls::Admission::new(32), limits);
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    let app = router(
        core,
        Arc::new(Auth::new("secret".into())),
        ServerOptions {
            hello_timeout: HELLO_TIMEOUT,
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
    let s = UnixStream::connect(path).await.ok()?;
    client_async("ws://localhost/", s).await.ok().map(|(ws, _)| ws)
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
    if ws
        .send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await
        .is_err()
    {
        return false;
    }
    tokio::time::timeout(Duration::from_secs(2), async {
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
    let _held = idle_tls(s.tcp, "127.0.0.1", 8).await;
    let mut local = open_unix(&s.unix).await.expect("local upgrade is not refused");
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
    let _held = idle_tls(s.tcp, "127.0.0.1", 8).await;
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
        if let Ok(c) = sock.connect(s.tcp).await {
            raw.push(c);
        }
    }
    let mut other = open_tls(s.tcp, "127.0.0.2")
        .await
        .expect("another source's handshake is not starved");
    assert!(hello_ok(&mut other).await);
}

#[tokio::test]
async fn the_saturating_source_is_released_after_the_hello_timeout() {
    let s = serve().await;
    let held = idle_tls(s.tcp, "127.0.0.1", 8).await;
    assert!(!held.is_empty());
    tokio::time::sleep(HELLO_TIMEOUT + Duration::from_millis(1100)).await;
    let mut again = open_tls(s.tcp, "127.0.0.1")
        .await
        .expect("slots free again after the deadline");
    assert!(hello_ok(&mut again).await);
    drop(held);
}

#[tokio::test]
async fn filling_the_local_pool_does_not_block_tcp() {
    let s = serve().await;
    let mut local = Vec::new();
    for _ in 0..8 {
        if let Some(ws) = open_unix(&s.unix).await {
            local.push(ws);
        }
    }
    let mut remote = open_tls(s.tcp, "127.0.0.1")
        .await
        .expect("tcp pool is separate from the local pool");
    assert!(hello_ok(&mut remote).await);
}
