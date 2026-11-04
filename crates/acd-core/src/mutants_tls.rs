//! Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
//! Private items of `tls`, so this is a child module of it.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug)]
struct AcceptAnyCert;
impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &rustls::pki_types::ServerName<'_>,
        _: &[u8], _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self, _: &[u8], _: &CertificateDer<'_>, _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self, _: &[u8], _: &CertificateDer<'_>, _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

/// A connected TLS pair: the server side wrapped in `TlsIo` with the given
/// deadline and authentication state, and the plain TLS client.
async fn pair(
    authenticated: bool,
    deadline: Instant,
) -> (TlsIo, tokio_rustls::client::TlsStream<TcpStream>) {
    let config = server_config(&generate("mutants")).unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    let accept = tokio::spawn(async move {
        let (stream, _) = tcp.accept().await.unwrap();
        TlsAcceptor::from(config).accept(stream).await.unwrap()
    });
    let client_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
        .with_no_client_auth();
    let client = tokio_rustls::TlsConnector::from(Arc::new(client_config))
        .connect("localhost".try_into().unwrap(), TcpStream::connect(addr).await.unwrap())
        .await
        .unwrap();
    let stream = accept.await.unwrap();
    let admit = Admit::new(
        Admission::new(1).try_acquire("127.0.0.1".parse().unwrap()).unwrap(),
        None,
    );
    if authenticated {
        admit.authenticated();
    }
    let (_tx, rx) = watch::channel(false);
    (TlsIo::new(stream, admit, deadline, rx), client)
}

/// `Admit::is_authenticated -> false` and `TlsIo::ended` `delete !`: the
/// pre-auth deadline binds only a connection that has not authenticated.
#[tokio::test]
async fn the_deadline_ends_unauthenticated_connections_only() {
    // Deadline already passed, authenticated: still served.
    let (mut io, mut client) = pair(true, Instant::now()).await;
    client.write_all(b"pending").await.unwrap();
    client.flush().await.unwrap();
    let mut buf = [0u8; 16];
    assert_eq!(io.read(&mut buf).await.unwrap(), 7);
    io.write_all(b"ok").await.unwrap();
    io.flush().await.unwrap();

    // Deadline already passed, not authenticated: read ends, writes fail.
    let (mut io, mut client) = pair(false, Instant::now()).await;
    client.write_all(b"pending").await.unwrap();
    client.flush().await.unwrap();
    assert_eq!(io.read(&mut buf).await.unwrap(), 0, "expired read is EOF");
    assert_eq!(io.write(b"x").await.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(io.flush().await.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
}

/// `poll_shutdown -> Ready(Ok(()))`: shutting down the server side sends the
/// TLS close_notify, which the client reads as a clean end of stream.
#[tokio::test]
async fn shutdown_sends_close_notify() {
    let (mut io, mut client) = pair(true, Instant::now() + Duration::from_secs(3600)).await;
    io.shutdown().await.unwrap();
    let mut buf = [0u8; 1];
    let n = client.read(&mut buf).await.unwrap();
    assert_eq!(n, 0);
}

/// `AbortOnDrop::drop -> ()`: dropping the guard aborts the task, which drops
/// what the task holds.
#[tokio::test]
async fn abort_on_drop_aborts_the_task() {
    let (held, watcher) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _held = held;
        std::future::pending::<()>().await;
    });
    // Let the task start so it owns `held`.
    tokio::task::yield_now().await;
    drop(AbortOnDrop(task));
    // No deadline: a task that was not aborted keeps `held` alive and this hangs.
    assert!(watcher.await.is_err(), "the task was dropped");
}
