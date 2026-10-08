use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use tokio_tungstenite::tungstenite::Message;

fn acd(dir: &std::path::Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_acd"));
    cmd.env("ACD_CONFIG_DIR", dir).env_remove("RUST_LOG");
    cmd
}

fn init(listen: &str) -> (tempfile::TempDir, std::path::PathBuf, u16) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    let out = acd(&dir).arg("init").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::fs::write(
        dir.join("config.toml"),
        format!("port = {port}\nlisten = {listen}\n"),
    )
    .unwrap();
    (tmp, dir, port)
}

struct Daemon(std::process::Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

fn start(dir: &std::path::Path, log: Option<&std::path::Path>, admission: Option<usize>) -> Daemon {
    let mut cmd = acd(dir);
    cmd.arg("run");
    if let Some(path) = log {
        cmd.env("RUST_LOG", "debug")
            .stderr(std::fs::File::create(path).unwrap());
    }
    if let Some(n) = admission {
        cmd.env("ACD_TEST_MAX_UNAUTH", n.to_string());
    }
    let child = cmd.spawn().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !dir.join("acd.sock").exists() {
        assert!(std::time::Instant::now() < deadline, "daemon did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    Daemon(child)
}

#[derive(Debug)]
struct PinVerifier(String);
impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        use base64::Engine;
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            base64::engine::general_purpose::STANDARD.encode(cert)
        );
        let pin = acd_core::tls::pin_of_cert_pem(&pem)
            .map_err(|e| rustls::Error::General(e.to_string()))?;
        if acd_protocol::pairing::pins_equal(&pin, &self.0) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
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

fn client_config(pin: &str) -> Arc<rustls::ClientConfig> {
    Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinVerifier(pin.to_string())))
        .with_no_client_auth(),
    )
}

async fn tls_stream(
    port: u16,
    pin: &str,
) -> std::io::Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    tokio_rustls::TlsConnector::from(client_config(pin))
        .connect(ServerName::try_from("localhost").unwrap().to_owned(), tcp)
        .await
}

fn pin_of(dir: &std::path::Path) -> String {
    let pem = std::fs::read_to_string(dir.join("tls.pem")).unwrap();
    acd_core::tls::check_pair(&acd_core::tls::KeyFiles::from_bundle(&pem).unwrap()).unwrap()
}

async fn ws_with_pin(
    port: u16,
    pin: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
    Box<dyn std::error::Error>,
> {
    let tls = tls_stream(port, pin).await?;
    let (ws, _) = tokio_tungstenite::client_async("wss://localhost/", tls).await?;
    Ok(ws)
}

async fn hello(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    >,
    token: &str,
) -> acd_protocol::ServerMessage {
    let msg = acd_protocol::ClientMessage::Hello {
        protocol: acd_protocol::PROTOCOL.into(),
        token: token.into(),
        client: acd_protocol::ClientInfo {
            id: "test".into(),
            name: "test".into(),
            version: "test".into(),
        },
        resume: None,
    };
    ws.send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    if let Some(message) = acd_protocol::decode_server(&text).unwrap() {
                        return message;
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => panic!("websocket read failed while awaiting welcome: {error}"),
                None => panic!("websocket closed while awaiting welcome"),
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn tcp_requires_tls_and_a_valid_pin_authenticates_hello() {
    let (_tmp, dir, port) = init("[\"127.0.0.1\"]");
    let _daemon = start(&dir, None, None);
    assert!(
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
            .await
            .is_err()
    );
    let token = std::fs::read_to_string(dir.join("client.token")).unwrap();
    let mut ws = ws_with_pin(port, &pin_of(&dir)).await.unwrap();
    assert!(matches!(
        hello(&mut ws, token.trim()).await,
        acd_protocol::ServerMessage::Welcome { .. }
    ));
}

#[tokio::test]
async fn wrong_pin_fails_before_hello_and_rotation_keeps_open_sessions() {
    let (_tmp, dir, port) = init("[\"127.0.0.1\"]");
    let log = dir.join("daemon.log");
    let _daemon = start(&dir, Some(&log), None);
    let token = std::fs::read_to_string(dir.join("client.token")).unwrap();
    let old = pin_of(&dir);
    let wrong = acd_core::tls::check_pair(&acd_core::tls::generate("wrong")).unwrap();
    let wrong_pin_result = ws_with_pin(port, &wrong).await;
    let log_after_wrong_pin = std::fs::read_to_string(&log).unwrap();
    assert!(
        log_after_wrong_pin.contains("decided upon suite"),
        "TLS handshake log is the positive control: {log_after_wrong_pin}"
    );
    assert!(
        wrong_pin_result.is_err(),
        "wrong-pin client never receives the client-visible welcome frame"
    );
    let mut open = ws_with_pin(port, &old).await.unwrap();
    assert!(matches!(
        hello(&mut open, token.trim()).await,
        acd_protocol::ServerMessage::Welcome { .. }
    ));
    let log_after_valid_hello = std::fs::read_to_string(&log).unwrap();
    assert!(
        log_after_valid_hello.contains("decided upon suite"),
        "valid-pin handshake is logged: {log_after_valid_hello}"
    );
    let out = acd(&dir).args(["tls", "rotate"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if tokio::time::timeout(Duration::from_millis(500), ws_with_pin(port, &old))
            .await
            .is_ok_and(|r| r.is_err())
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "old pin remained accepted"
        );
    }
    open.send(Message::Ping(vec![1].into())).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(message) = open.next().await {
                if matches!(message, Ok(Message::Pong(_))) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap()
    );
    drop(open);
}

#[tokio::test]
async fn admission_budget_is_shared_by_ipv4_and_ipv6_listeners() {
    if std::net::TcpListener::bind("[::1]:0").is_err() {
        return;
    }
    let (_tmp, dir, port) = init("[\"127.0.0.1\", \"::1\"]");
    let _daemon = start(&dir, None, Some(2));
    let pin = pin_of(&dir);
    let cfg = client_config(&pin);
    let mut held = Vec::new();
    for _ in 0..2 {
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        held.push(
            tokio_rustls::TlsConnector::from(cfg.clone())
                .connect(ServerName::try_from("localhost").unwrap().to_owned(), tcp)
                .await
                .unwrap(),
        );
    }
    let mut third = tokio::net::TcpStream::connect(("::1", port)).await.unwrap();
    let mut byte = [0u8; 1];
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::io::AsyncReadExt::read(&mut third, &mut byte)
        )
        .await
        .unwrap()
        .unwrap_or(0),
        0
    );
}
