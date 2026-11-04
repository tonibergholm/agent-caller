use acd_core::{Auth, Core, ServerOptions, router, testing::*, tls};
use axum::serve::Listener;
use rustls::pki_types::pem::PemObject;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::watch,
};

struct Served {
    port: u16,
    task: tokio::task::JoinHandle<()>,
    kill: tls::KillSwitch,
    _tx: watch::Sender<Arc<rustls::ServerConfig>>,
}

async fn serve(limits: tls::Limits, admission: tls::Admission) -> Served {
    serve_with_token(limits, admission, "secret").await
}

async fn serve_with_token(limits: tls::Limits, admission: tls::Admission, token: &str) -> Served {
    let f = tls::generate("t");
    let (tx, rx) = watch::channel(tls::server_config(&f).unwrap());
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    let listener = tls::TlsListener::new(tcp, rx, admission, limits);
    let kill = listener.kill_switch();
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    let app = router(
        core,
        Arc::new(Auth::new(token.into())),
        ServerOptions::default(),
    );
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<tls::Admit>(),
        )
        .await
        .unwrap()
    });
    Served {
        port,
        task,
        kill,
        _tx: tx,
    }
}

fn short() -> tls::Limits {
    tls::Limits {
        handshake: Duration::from_millis(300),
        preauth_deadline: Duration::from_millis(600),
        max_handshakes: 16,
    }
}

#[test]
fn generated_bundles_round_trip_and_mismatched_pairs_are_refused() {
    let a = tls::generate("a");
    let pin = tls::check_pair(&a).unwrap();
    assert_eq!(tls::pin_of_cert_pem(&a.cert_pem).unwrap(), pin);
    let back = tls::KeyFiles::from_bundle(&a.to_bundle()).unwrap();
    assert_eq!(tls::check_pair(&back).unwrap(), pin);
    let b = tls::generate("b");
    let mixed = tls::KeyFiles {
        key_pem: a.key_pem.clone(),
        cert_pem: b.cert_pem.clone(),
    };
    assert!(
        tls::check_pair(&mixed).is_err(),
        "no pin for a key that does not match the certificate"
    );
    assert!(tls::server_config(&mixed).is_err());
    assert!(
        tls::KeyFiles::from_bundle(&a.key_pem).is_err(),
        "a bundle needs both blocks"
    );
    let malformed_key = format!(
        "-----BEGIN PRIVATE KEY-----\nnot base64!\n-----END PRIVATE KEY-----\n{}",
        a.cert_pem
    );
    assert!(tls::KeyFiles::from_bundle(&malformed_key).is_err());
    let malformed_cert = format!(
        "{}-----BEGIN CERTIFICATE-----\nnot base64!\n-----END CERTIFICATE-----\n",
        a.key_pem
    );
    assert!(tls::KeyFiles::from_bundle(&malformed_cert).is_err());

    let cert_der =
        rustls::pki_types::CertificateDer::from_pem_slice(a.cert_pem.as_bytes()).unwrap();
    let (_, parsed) = x509_parser::parse_x509_certificate(cert_der.as_ref()).unwrap();
    let expected = acd_protocol::pairing::spki_pin(parsed.tbs_certificate.subject_pki.raw);
    assert_eq!(
        pin, expected,
        "pin hashes the full DER SubjectPublicKeyInfo"
    );
}

#[tokio::test]
async fn killing_listener_wakes_an_authenticated_parked_read() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let s = serve(tls::Limits::default(), tls::Admission::new(32)).await;
    let (mut ws, _) =
        tokio_tungstenite::client_async("wss://localhost/", tls_connect(s.port).await)
            .await
            .unwrap();
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
    ws.send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if matches!(ws.next().await.unwrap().unwrap(), Message::Text(text) if matches!(acd_protocol::decode_server(text.as_str()).unwrap(), Some(acd_protocol::ServerMessage::Welcome { .. }))) { break; }
        }
    }).await.expect("welcome received");

    let mut clients = vec![ws];
    for _ in 0..2 {
        clients.push(authenticate_ok(s.port).await);
    }
    let (parked_tx, mut parked_rx) = tokio::sync::mpsc::channel(clients.len());
    let reads = clients
        .into_iter()
        .map(|ws| {
            let (_sink, mut stream) = ws.split();
            let parked_tx = parked_tx.clone();
            tokio::spawn(async move {
                parked_tx.send(()).await.unwrap();
                stream.next().await
            })
        })
        .collect::<Vec<_>>();
    drop(parked_tx);
    for _ in 0..reads.len() {
        parked_rx.recv().await.unwrap();
    }
    s.kill.kill();
    for read in reads {
        let result = tokio::time::timeout(Duration::from_secs(2), read).await;
        assert!(result.is_ok(), "every parked read wakes when killed");
    }
    s.task.abort();
}

#[tokio::test]
async fn plaintext_ws_is_refused() {
    let s = serve(tls::Limits::default(), tls::Admission::new(32)).await;
    assert!(
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/", s.port))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn completed_handshake_frees_capacity_under_continuous_accepts() {
    let files = tls::generate("t");
    let (tx, rx) = watch::channel(tls::server_config(&files).unwrap());
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    let mut limits = short();
    limits.max_handshakes = 1;
    let listener = tls::TlsListener::new(tcp, rx, tls::Admission::new(32), limits);
    let kill = listener.kill_switch();
    let task = tokio::spawn(async move {
        let mut listener = listener;
        let mut clients = Vec::new();
        // Keep accepts ready while the first handshake is completing.
        for _ in 0..40 {
            clients.push(tokio::spawn(tls_connect(port)));
            tokio::task::yield_now().await;
        }
        let Ok(first) = tokio::time::timeout(Duration::from_secs(3), listener.accept()).await
        else {
            return false;
        };
        drop(first);
        let second = tokio::time::timeout(Duration::from_secs(3), listener.accept()).await;
        for client in clients {
            client.abort();
        }
        second.is_ok()
    });
    let accepted = task.await.unwrap();
    assert!(
        accepted,
        "each completed handshake releases its bounded slot for the next connection"
    );
    kill.kill();
    drop(tx);
}

#[tokio::test]
async fn dropping_the_listener_releases_the_port_and_ends_pending_handshakes() {
    let s = serve(short(), tls::Admission::new(32)).await;
    let mut silent = tokio::net::TcpStream::connect(("127.0.0.1", s.port))
        .await
        .unwrap();
    s.task.abort();
    let _ = s.task.await;
    let rebound = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Ok(l) = tokio::net::TcpListener::bind(("127.0.0.1", s.port)).await {
                return l;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(rebound.is_ok(), "port released");
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(1), silent.read(&mut buf))
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(n, 0, "the pending handshake was ended");
}

#[tokio::test]
async fn tls_then_silent_or_trickling_http_is_cut_and_slots_recover() {
    let admission = tls::Admission::new(2);
    let s = serve(short(), admission).await;
    let started = std::time::Instant::now();
    let mut quiet = tls_connect(s.port).await;
    let (mut trickle_rd, mut trickle_wr) = tokio::io::split(tls_connect(s.port).await);
    let tricker = tokio::spawn(async move {
        for b in b"GET / HTTP/1.1\r\nHost: x\r\n" {
            if trickle_wr.write_all(&[*b]).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    let mut third = tokio::net::TcpStream::connect(("127.0.0.1", s.port))
        .await
        .unwrap();
    let mut b = [0u8; 1];
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(300), third.read(&mut b))
            .await
            .unwrap()
            .unwrap_or(0),
        0
    );
    let mut buf = [0u8; 16];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), quiet.read(&mut buf))
            .await
            .unwrap()
            .unwrap_or(0),
        0,
        "silent cut"
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), trickle_rd.read(&mut buf))
            .await
            .unwrap()
            .unwrap_or(0),
        0,
        "trickler cut"
    );
    assert!(started.elapsed() < Duration::from_millis(1500));
    tricker.abort();
    let a1 = authenticate_ok(s.port).await;
    let a2 = authenticate_ok(s.port).await;
    let _q = tls_connect(s.port).await;
    let a3 = authenticate_ok(s.port).await;
    drop((a1, a2, a3));
}

async fn tls_connect(port: u16) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
    use rustls::{
        DigitallySignedStruct, SignatureScheme,
        client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        pki_types::{CertificateDer, ServerName, UnixTime},
    };
    use std::sync::Arc;
    #[derive(Debug)]
    struct AcceptAny;
    impl ServerCertVerifier for AcceptAny {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![
                SignatureScheme::ECDSA_NISTP256_SHA256,
                SignatureScheme::ED25519,
                SignatureScheme::RSA_PSS_SHA256,
            ]
        }
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(AcceptAny))
    .with_no_client_auth();
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(
            ServerName::try_from("localhost").unwrap().to_owned(),
            stream,
        )
        .await
        .unwrap()
}

async fn authenticate_ok(
    port: u16,
) -> tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let tls = tls_connect(port).await;
    let (mut ws, _) = tokio_tungstenite::client_async("wss://localhost/", tls)
        .await
        .unwrap();
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
    ws.send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let msg = ws.next().await.unwrap().unwrap();
            if let Message::Text(text) = msg
                && matches!(
                    acd_protocol::decode_server(text.as_str()).unwrap(),
                    Some(acd_protocol::ServerMessage::Welcome { .. })
                )
            {
                break;
            }
        }
    })
    .await
    .expect("server welcomes authenticated client");
    ws
}

/// Everything the daemon logs while this test binary runs.
fn captured_log() -> &'static std::sync::Mutex<Vec<u8>> {
    use std::sync::{Mutex, OnceLock};
    static BUF: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
    BUF.get_or_init(|| {
        #[derive(Clone)]
        struct W;
        impl std::io::Write for W {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                captured_log().lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_writer(|| W)
                .with_ansi(false)
                .finish(),
        )
        .unwrap();
        Mutex::new(Vec::new())
    })
}

async fn hello_with(port: u16, token: &str, client_id: &str) {
    hello_named(port, token, client_id, "log-test").await
}

async fn hello_named(port: u16, token: &str, client_id: &str, name: &str) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let tls = tls_connect(port).await;
    let (mut ws, _) = tokio_tungstenite::client_async("wss://localhost/", tls)
        .await
        .unwrap();
    let hello = acd_protocol::ClientMessage::Hello {
        protocol: acd_protocol::PROTOCOL.into(),
        token: token.into(),
        client: acd_protocol::ClientInfo {
            id: client_id.into(),
            name: name.into(),
            version: "9.9".into(),
        },
        resume: None,
    };
    ws.send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await
        .unwrap();
    // Wait for the verdict: a welcome, or the connection ending.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Ok(m)) = ws.next().await {
            if matches!(m, Message::Close(_)) {
                break;
            }
            if let Message::Text(t) = m
                && matches!(
                    acd_protocol::decode_server(t.as_str()),
                    Ok(Some(acd_protocol::ServerMessage::Welcome { .. }))
                )
            {
                break;
            }
        }
    })
    .await;
}

#[tokio::test]
async fn accepted_and_rejected_hellos_are_logged_without_the_token() {
    captured_log();
    let s = serve(short(), tls::Admission::new(tls::MAX_UNAUTHENTICATED_TCP)).await;
    hello_with(s.port, "wrong-token-7f3a", "bad-client").await;
    hello_with(s.port, "secret", "good-client-1").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let log = String::from_utf8(captured_log().lock().unwrap().clone()).unwrap();
    let rejected = log
        .lines()
        .find(|l| l.contains("hello rejected") && l.contains("invalid token"))
        .expect("a rejected hello is logged");
    assert!(rejected.contains("invalid token"), "{rejected}");
    assert!(rejected.contains("127.0.0.1:"), "{rejected}");
    let accepted = log
        .lines()
        .find(|l| l.contains("hello accepted") && l.contains("client_id_len=13"))
        .expect("an accepted hello is logged");
    assert!(accepted.contains("127.0.0.1:"), "{accepted}");
    // Peer free text is not logged: only lengths and a short hash.
    assert!(!accepted.contains("good-client-1"), "{accepted}");
    assert!(!accepted.contains("log-test"), "{accepted}");
    assert!(accepted.contains("client_id_sha="), "{accepted}");
    assert!(!log.contains("wrong-token-7f3a"), "presented token leaked");
    assert!(!log.contains("secret"), "stored token leaked");
}

/// A peer that sets client.id and client.name to its own token must not get
/// the token into the log, whole or in part.
#[tokio::test]
async fn a_hello_naming_itself_after_the_token_does_not_log_the_token() {
    captured_log();
    let token = "tok-Zq81xKd4Wm9vRb27";
    let s = serve_with_token(short(), tls::Admission::new(tls::MAX_UNAUTHENTICATED_TCP), token).await;
    hello_named(s.port, token, token, token).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let log = String::from_utf8(captured_log().lock().unwrap().clone()).unwrap();
    assert!(log.contains("hello accepted"), "the hello was accepted and logged");
    for i in 0..=token.len() - 8 {
        let piece = &token[i..i + 8];
        assert!(!log.contains(piece), "token piece {piece:?} leaked");
    }
}
