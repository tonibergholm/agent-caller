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

/// A hello timeout short enough to wait for, long enough to open a few
/// connections first.
const HELLO_SHORT: Duration = Duration::from_secs(3);

async fn within<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(STEP, f)
        .await
        .unwrap_or_else(|_| panic!("{what} timed out after {STEP:?}"))
}

/// Loopback aliases other than 127.0.0.1 exist on Linux, not by default on
/// macOS. On Linux they must exist, so a missing alias fails the test; only
/// elsewhere does a test needing distinct sources skip (the pool rules are
/// also covered without sockets by the `source_pool_tests` unit tests).
fn can_bind(ips: &[&str]) -> bool {
    ips.iter().all(|ip| match std::net::TcpListener::bind(format!("{ip}:0")) {
        Ok(_) => true,
        Err(e) if cfg!(target_os = "linux") => panic!("cannot bind loopback alias {ip} on Linux: {e}"),
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
    serve_opts(hello, handshake, preauth, None, "127.0.0.1:0", 8).await
}

async fn serve_gated(hello: Duration, gate: watch::Receiver<bool>) -> Served {
    serve_opts(hello, HOLD, HOLD, Some(gate), "127.0.0.1:0", 8).await
}

/// Listens on every interface, so loopback and this machine's own address
/// are two real sources. `max_preauth` sizes the post-upgrade network pool.
async fn serve_everywhere(max_preauth: usize) -> Served {
    serve_opts(HOLD, HOLD, HOLD, None, "0.0.0.0:0", max_preauth).await
}

async fn serve_opts(
    hello: Duration,
    handshake: Duration,
    preauth: Duration,
    hello_gate: Option<watch::Receiver<bool>>,
    bind: &str,
    max_preauth: usize,
) -> Served {
    let f = tls::generate("t");
    let (tx, rx) = watch::channel(tls::server_config(&f).unwrap());
    let tcp = tokio::net::TcpListener::bind(bind).await.unwrap();
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
            hello_gate,
            max_preauth,
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
    // A counter, not the clock: macOS ticks in microseconds, so two parallel
    // tests could start in the same tick and share one directory and socket.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "acd-preauth-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

/// How an open attempt ended when it did not give a WebSocket.
/// The payloads are read through `Debug` in assertion messages.
#[allow(dead_code)]
#[derive(Debug)]
enum Refused {
    /// TCP connect or TLS handshake failed: a connection-level failure
    /// (reset or EOF), not an HTTP answer.
    Connection(String),
    /// The server answered the upgrade request with this HTTP status.
    Http(u16),
    /// The upgrade failed in some other way.
    Other(String),
}

fn upgrade_failure(e: tokio_tungstenite::tungstenite::Error) -> Refused {
    match e {
        tokio_tungstenite::tungstenite::Error::Http(r) => Refused::Http(r.status().as_u16()),
        e => Refused::Other(e.to_string()),
    }
}

/// TCP connect from `source`, TLS, then the WebSocket upgrade. `None` when
/// the server refuses at any step.
async fn open_tls(server: SocketAddr, source: &str) -> Option<TlsWs> {
    open_tls_result(server, source).await.ok()
}

async fn open_tls_result(server: SocketAddr, source: &str) -> Result<TlsWs, Refused> {
    within("open_tls", open_tls_inner(server, source)).await
}

async fn open_tls_inner(server: SocketAddr, source: &str) -> Result<TlsWs, Refused> {
    let sock = TcpSocket::new_v4().unwrap();
    sock.bind(format!("{source}:0").parse().unwrap()).unwrap();
    let stream = sock
        .connect(server)
        .await
        .map_err(|e| Refused::Connection(format!("tcp connect: {e}")))?;
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
        .map_err(|e| Refused::Connection(format!("tls: {e}")))?;
    client_async("wss://localhost/", tls)
        .await
        .map(|(ws, _)| ws)
        .map_err(upgrade_failure)
}

async fn open_unix(path: &std::path::Path) -> Option<WebSocketStream<UnixStream>> {
    open_unix_result(path).await.ok()
}

async fn open_unix_result(path: &std::path::Path) -> Result<WebSocketStream<UnixStream>, Refused> {
    within("open_unix", async {
        let s = UnixStream::connect(path)
            .await
            .map_err(|e| Refused::Connection(format!("unix connect: {e}")))?;
        client_async("ws://localhost/", s)
            .await
            .map(|(ws, _)| ws)
            .map_err(upgrade_failure)
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

/// On Linux any local account can bind any 127.x.y.z. Eight loopback
/// aliases are one source: they hold two slots in all, so they can neither
/// fill the network pool nor lock the local socket out. Fails if the pools
/// count each alias as a source of its own.
#[tokio::test]
async fn loopback_aliases_are_one_source_and_cannot_fill_the_network_pool() {
    let sources: Vec<String> = (2..10).map(|i| format!("127.0.0.{i}")).collect();
    let all: Vec<&str> = sources.iter().map(String::as_str).collect();
    if !can_bind(&all) {
        return;
    }
    let s = serve().await;
    let mut held = Vec::new();
    for src in &sources {
        held.extend(idle_tls(s.tcp, src, 1).await);
    }
    assert_eq!(held.len(), 2, "eight loopback aliases hold two slots in all");
    assert!(
        matches!(open_tls_result(s.tcp, "127.0.0.9").await, Err(Refused::Connection(_))),
        "another alias is the same source: refused before TLS"
    );
    let mut local = open_unix(&s.unix)
        .await
        .expect("local upgrade is not refused");
    assert!(hello_ok(&mut local).await);
}

#[tokio::test]
async fn one_source_holds_at_most_two_pre_auth_connections() {
    let s = serve().await;
    let held = idle_tls(s.tcp, "127.0.0.1", 2).await;
    assert_eq!(held.len(), 2, "per-source cap");
    // The third is closed right after TCP accept, before TLS: a
    // connection-level failure, never an HTTP status.
    match open_tls_result(s.tcp, "127.0.0.1").await {
        Err(Refused::Connection(_)) => {}
        other => panic!("expected a pre-TLS connection failure, got {:?}", other.map(|_| "ws")),
    }
}

/// The slots come back when the server cuts the idle connections at the
/// hello timeout. The two deadlines are kept apart: the hello timeout (3 s)
/// is far shorter than the TCP pre-auth deadline (60 s, longer than any
/// wait here), so only the hello timeout can release the slots. The hello
/// timeout is gated (`ServerOptions::hello_gate`) and starts only after the
/// setup is done, so setup speed cannot affect the result. The test waits
/// for the server to close the held connections, then checks at once.
#[tokio::test]
async fn the_saturating_source_is_released_after_the_hello_timeout() {
    // The hello timeout is gated: it does not start until the test opens
    // the gate, after the two upgrades are held and the third is refused.
    // Setup speed therefore cannot matter; no retry and no budget.
    let (open, gate) = watch::channel(false);
    let s = serve_gated(HELLO_SHORT, gate).await;
    let held = idle_tls(s.tcp, "127.0.0.1", 2).await;
    let capped = open_tls_result(s.tcp, "127.0.0.1").await;
    let mut held = held;
    open.send(true).unwrap();
    let started = tokio::time::Instant::now();
    assert_eq!(held.len(), 2, "slots were held before the deadline");
    assert!(
        matches!(capped, Err(Refused::Connection(_))),
        "still capped while the slots are held"
    );
    // Deterministic sync: the server's hello timeout closes both held
    // connections; wait for those closes instead of sleeping.
    for ws in &mut held {
        let closed = tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(Ok(m)) = ws.next().await {
                if matches!(m, Message::Close(_)) {
                    break;
                }
            }
        })
        .await;
        assert!(closed.is_ok(), "the server never closed the idle connection");
    }
    assert!(
        started.elapsed() < HOLD,
        "the TCP deadline must not be what freed the slots"
    );
    let mut again = None;
    for _ in 0..20 {
        // The slot is freed just after the close; allow a brief lag.
        if let Some(ws) = open_tls(s.tcp, "127.0.0.1").await {
            again = Some(ws);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut again = again.expect("slots never came back after the hello timeout");
    assert!(hello_ok(&mut again).await);
}

#[tokio::test]
async fn filling_the_local_pool_does_not_block_tcp() {
    let s = serve().await;
    let mut local = Vec::new();
    for _ in 0..8 {
        local.push(open_unix(&s.unix).await.expect("local upgrade"));
    }
    assert_eq!(local.len(), 8, "all eight local slots are held");
    assert_eq!(
        open_unix_result(&s.unix).await.err().map(|r| format!("{r:?}")),
        Some("Http(503)".to_string()),
        "a full local pool answers the upgrade with HTTP 503"
    );
    let mut remote = open_tls(s.tcp, "127.0.0.1")
        .await
        .expect("tcp pool is separate from the local pool");
    assert!(hello_ok(&mut remote).await);
}

/// This machine's own non-loopback address, if it has one (a UDP connect
/// sends nothing). A local process connecting to it is seen as that address,
/// not as loopback: a second real source. `None` also when this process
/// cannot reach a listener of its own on that address (a host firewall can
/// drop it, for a test binary it has not seen), which a skip covers.
fn own_non_loopback_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        return None;
    }
    // A connect alone proves nothing: the kernel completes it even when a
    // firewall never lets the listener see the connection. Echo one byte.
    use std::io::{Read, Write};
    let probe = std::net::TcpListener::bind("0.0.0.0:0").ok()?;
    let to = SocketAddr::new(ip, probe.local_addr().ok()?.port());
    std::thread::spawn(move || {
        if let Ok((mut c, _)) = probe.accept() {
            let _ = c.write_all(b"x");
        }
    });
    let mut c = std::net::TcpStream::connect_timeout(&to, Duration::from_secs(1)).ok()?;
    c.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
    c.read_exact(&mut [0u8; 1]).ok()?;
    Some(ip)
}

/// A source as (server address to connect to, address to bind). `None`, with
/// a note, when the machine has no non-loopback address.
type Source = (SocketAddr, String);

fn two_sources(s: &Served) -> Option<(Source, Source)> {
    let Some(net) = own_non_loopback_ip() else {
        eprintln!("SKIPPED: no non-loopback address, or a firewall drops connections to it");
        return None;
    };
    let port = s.tcp.port();
    Some((
        (SocketAddr::from(([127, 0, 0, 1], port)), "127.0.0.1".into()),
        (SocketAddr::new(net, port), net.to_string()),
    ))
}

async fn idle_from(src: &Source, n: usize) -> Vec<TlsWs> {
    idle_tls(src.0, &src.1, n).await
}

/// One source holding its whole share does not lock the other out, in either
/// direction. Fails if every address maps to one source key: the second
/// source is then refused before TLS.
#[tokio::test]
async fn a_source_holding_its_share_does_not_lock_out_the_other() {
    let s = serve_everywhere(8).await;
    let Some((loopback, net)) = two_sources(&s) else { return };
    let held = idle_from(&loopback, 2).await;
    assert_eq!(held.len(), 2, "loopback holds its share");
    let mut other = open_tls(net.0, &net.1).await.expect("the network source is not locked out");
    assert!(hello_ok(&mut other).await);

    let s = serve_everywhere(8).await;
    let Some((loopback, net)) = two_sources(&s) else { return };
    let held = idle_from(&net, 2).await;
    assert_eq!(held.len(), 2, "the network source holds its share");
    let mut other = open_tls(loopback.0, &loopback.1).await.expect("loopback is not locked out");
    assert!(hello_ok(&mut other).await);
}

/// With a post-upgrade pool of 4 and a reserve of 2, the reserved slots go
/// only to a network source holding none. Loopback holds two slots (2/4),
/// the network source then takes one, and its second is refused with 503
/// although 3/4 are taken. In the other order loopback gets one slot: it
/// never takes the reserve. Fails if every address maps to one source key.
#[tokio::test]
async fn the_reserve_goes_to_a_network_source_holding_none() {
    let s = serve_everywhere(4).await;
    let Some((loopback, net)) = two_sources(&s) else { return };
    let held = idle_from(&loopback, 2).await;
    assert_eq!(held.len(), 2, "loopback holds its share");
    let first = open_tls(net.0, &net.1).await;
    assert!(first.is_some(), "a source holding none takes a reserved slot");
    assert!(
        matches!(open_tls_result(net.0, &net.1).await, Err(Refused::Http(503))),
        "it holds one now: its second is refused at 3/4"
    );

    let s = serve_everywhere(4).await;
    let Some((loopback, net)) = two_sources(&s) else { return };
    let held = idle_from(&net, 1).await;
    assert_eq!(held.len(), 1);
    let first = open_tls(loopback.0, &loopback.1).await;
    assert!(first.is_some(), "loopback takes an unreserved slot");
    assert!(
        matches!(open_tls_result(loopback.0, &loopback.1).await, Err(Refused::Http(503))),
        "loopback never takes the reserve, though it is under its cap and the pool is at 2/4"
    );
}
