use rustls_pki_types::pem::PemObject;

use axum::{
    extract::connect_info::Connected,
    serve::{IncomingStream, Listener},
};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer},
};
use std::{
    fmt,
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
    task::JoinSet,
    time::{Instant, Sleep},
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("invalid PEM: {0}")]
    Pem(String),
    #[error("invalid certificate: {0}")]
    Cert(String),
    #[error("TLS configuration: {0}")]
    Config(String),
}

pub struct KeyFiles {
    pub key_pem: String,
    pub cert_pem: String,
}
impl KeyFiles {
    pub fn to_bundle(&self) -> String {
        format!("{}{}", self.key_pem, self.cert_pem)
    }
    pub fn from_bundle(s: &str) -> Result<KeyFiles, TlsError> {
        let mut keys: Vec<String> = Vec::new();
        let mut certs: Vec<String> = Vec::new();
        let mut current = Vec::new();
        let mut label = String::new();
        for line in s.lines() {
            if let Some(name) = line
                .strip_prefix("-----BEGIN ")
                .and_then(|x| x.strip_suffix("-----"))
            {
                if !current.is_empty() {
                    return Err(TlsError::Pem("nested PEM block".into()));
                }
                label = name.to_string();
                current.push(line);
            } else if !current.is_empty() {
                current.push(line);
                if line == format!("-----END {label}-----") {
                    let block = format!("{}\n", current.join("\n"));
                    match label.as_str() {
                        "PRIVATE KEY" => {
                            rustls_pki_types::PrivatePkcs8KeyDer::from_pem_slice(block.as_bytes())
                                .map_err(|e| TlsError::Pem(e.to_string()))?;
                            keys.push(block)
                        }
                        "RSA PRIVATE KEY" => {
                            rustls_pki_types::PrivatePkcs1KeyDer::from_pem_slice(block.as_bytes())
                                .map_err(|e| TlsError::Pem(e.to_string()))?;
                            keys.push(block)
                        }
                        "EC PRIVATE KEY" => {
                            rustls_pki_types::PrivateSec1KeyDer::from_pem_slice(block.as_bytes())
                                .map_err(|e| TlsError::Pem(e.to_string()))?;
                            keys.push(block)
                        }
                        "CERTIFICATE" => {
                            CertificateDer::from_pem_slice(block.as_bytes())
                                .map_err(|e| TlsError::Pem(e.to_string()))?;
                            certs.push(block)
                        }
                        _ => return Err(TlsError::Pem("unexpected PEM block".into())),
                    }
                    current.clear();
                    label.clear();
                }
            } else if !line.trim().is_empty() {
                return Err(TlsError::Pem("text outside PEM blocks".into()));
            }
        }
        if !current.is_empty() {
            return Err(TlsError::Pem("unterminated PEM block".into()));
        }
        if keys.len() != 1 || certs.len() != 1 {
            return Err(TlsError::Pem(
                "bundle must contain exactly one key and one certificate".into(),
            ));
        }
        Ok(KeyFiles {
            key_pem: keys.remove(0),
            cert_pem: certs.remove(0),
        })
    }
}

pub fn generate(common_name: &str) -> KeyFiles {
    let key = rcgen::KeyPair::generate().expect("P-256 key generation");
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("empty SAN list");
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, common_name);
    let cert = params.self_signed(&key).expect("self-signed certificate");
    KeyFiles {
        key_pem: key.serialize_pem(),
        cert_pem: cert.pem(),
    }
}

fn certificate_spki(
    cert_pem: &str,
) -> Result<(CertificateDer<'static>, Vec<u8>, Vec<u8>), TlsError> {
    let cert_der = CertificateDer::from_pem_slice(cert_pem.as_bytes())
        .map_err(|e| TlsError::Pem(e.to_string()))?;
    let (_, cert) = x509_parser::parse_x509_certificate(cert_der.as_ref())
        .map_err(|e| TlsError::Cert(e.to_string()))?;
    {
        let raw_key = cert
            .tbs_certificate
            .subject_pki
            .subject_public_key
            .data
            .to_vec();
        let spki = cert.tbs_certificate.subject_pki.raw.to_vec();
        drop(cert);
        Ok((cert_der, raw_key, spki))
    }
}

pub fn check_pair(f: &KeyFiles) -> Result<String, TlsError> {
    let key = rcgen::KeyPair::from_pem(&f.key_pem).map_err(|e| TlsError::Pem(e.to_string()))?;
    let (_, raw_key, spki) = certificate_spki(&f.cert_pem)?;
    if key.public_key_raw() != raw_key {
        return Err(TlsError::Cert("key does not match certificate".into()));
    }
    Ok(acd_protocol::pairing::spki_pin(&spki))
}

pub fn pin_of_cert_pem(cert_pem: &str) -> Result<String, TlsError> {
    let (_, _, spki) = certificate_spki(cert_pem)?;
    Ok(acd_protocol::pairing::spki_pin(&spki))
}

pub fn server_config(files: &KeyFiles) -> Result<Arc<ServerConfig>, TlsError> {
    check_pair(files)?;
    let key = rustls::pki_types::PrivatePkcs8KeyDer::pem_slice_iter(files.key_pem.as_bytes())
        .next()
        .map(|r| r.map(PrivateKeyDer::Pkcs8))
        .or_else(|| {
            rustls::pki_types::PrivatePkcs1KeyDer::pem_slice_iter(files.key_pem.as_bytes())
                .next()
                .map(|r| r.map(PrivateKeyDer::Pkcs1))
        })
        .or_else(|| {
            rustls::pki_types::PrivateSec1KeyDer::pem_slice_iter(files.key_pem.as_bytes())
                .next()
                .map(|r| r.map(PrivateKeyDer::Sec1))
        })
        .ok_or_else(|| TlsError::Pem("missing private key".into()))?
        .map_err(|e| TlsError::Pem(e.to_string()))?;
    let cert = CertificateDer::from_pem_slice(files.cert_pem.as_bytes())
        .map_err(|e| TlsError::Pem(e.to_string()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| TlsError::Config(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .map_err(|e| TlsError::Config(e.to_string()))?;
    Ok(Arc::new(config))
}

#[derive(Clone)]
pub struct Admission {
    semaphore: Arc<Semaphore>,
}
impl Admission {
    pub fn new(max_unauthenticated: usize) -> Admission {
        Admission {
            semaphore: Arc::new(Semaphore::new(max_unauthenticated)),
        }
    }
    fn try_acquire(&self) -> Option<OwnedSemaphorePermit> {
        self.semaphore.clone().try_acquire_owned().ok()
    }
}

pub struct Limits {
    pub handshake: Duration,
    pub preauth_deadline: Duration,
    pub max_handshakes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(5),
            preauth_deadline: Duration::from_secs(10),
            max_handshakes: 16,
        }
    }
}
pub const MAX_UNAUTHENTICATED_TCP: usize = 32;

struct AdmitInner {
    permit: std::sync::Mutex<Option<OwnedSemaphorePermit>>,
    authenticated: std::sync::atomic::AtomicBool,
}
#[derive(Clone)]
pub struct Admit {
    inner: Arc<AdmitInner>,
}
impl Admit {
    fn new(permit: OwnedSemaphorePermit) -> Self {
        Self {
            inner: Arc::new(AdmitInner {
                permit: std::sync::Mutex::new(Some(permit)),
                authenticated: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }
    pub fn authenticated(&self) {
        if !self
            .inner
            .authenticated
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            self.inner.permit.lock().unwrap().take();
        }
    }
    fn is_authenticated(&self) -> bool {
        self.inner
            .authenticated
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

#[derive(Clone)]
pub struct KillSwitch {
    tx: watch::Sender<bool>,
}
impl KillSwitch {
    /// Kill active connections. If all senders are dropped without calling this,
    /// receivers observe channel closure and keep their streams open.
    pub fn kill(&self) {
        self.tx.send_replace(true);
    }
}

type KillFuture = Pin<Box<dyn Future<Output = Result<(), watch::error::RecvError>> + Send>>;

pub struct TlsIo {
    stream: TlsStream<TcpStream>,
    admit: Admit,
    deadline: Instant,
    sleep: Pin<Box<Sleep>>,
    killed: Option<KillFuture>,
    dead: bool,
}
impl TlsIo {
    fn new(
        stream: TlsStream<TcpStream>,
        admit: Admit,
        deadline: Instant,
        killed: watch::Receiver<bool>,
    ) -> Self {
        let killed = Box::pin(async move {
            let mut killed = killed;
            killed.wait_for(|k| *k).await.map(|_| ())
        });
        Self {
            stream,
            admit,
            deadline,
            sleep: Box::pin(tokio::time::sleep_until(deadline)),
            killed: Some(killed),
            dead: false,
        }
    }
    pub fn admit(&self) -> Admit {
        self.admit.clone()
    }
    fn ended(&mut self, cx: &mut Context<'_>) -> bool {
        if self.dead {
            return true;
        }
        if let Some(killed) = &mut self.killed {
            match killed.as_mut().poll(cx) {
                Poll::Ready(Ok(())) => {
                    self.killed = None;
                    self.dead = true;
                    return true;
                }
                Poll::Ready(Err(_)) => self.killed = None,
                Poll::Pending => {}
            }
        }
        if !self.admit.is_authenticated() && Instant::now() >= self.deadline {
            return true;
        }
        if !self.admit.is_authenticated() && self.sleep.as_mut().poll(cx).is_ready() {
            return true;
        }
        false
    }
}
impl AsyncRead for TlsIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.ended(cx) {
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl AsyncWrite for TlsIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.ended(cx) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "connection expired",
            )));
        }
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.ended(cx) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "connection expired",
            )));
        }
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct TlsListener {
    rx: mpsc::Receiver<(TlsIo, SocketAddr)>,
    _task: AbortOnDrop,
    local: SocketAddr,
    kill: KillSwitch,
}
impl TlsListener {
    pub fn new(
        tcp: TcpListener,
        config: watch::Receiver<Arc<ServerConfig>>,
        admission: Admission,
        limits: Limits,
    ) -> TlsListener {
        let local = tcp.local_addr().expect("bound TCP listener");
        let (tx, rx) = mpsc::channel(16);
        let (kill_tx, _) = watch::channel(false);
        let kill = KillSwitch { tx: kill_tx };
        let kill_rx = kill.tx.subscribe();
        let task = tokio::spawn(async move {
            let mut handshakes = JoinSet::new();
            let tcp = tcp;
            let mut config = config;
            loop {
                tokio::select! {
                    biased;
                    _ = tx.closed() => return,
                    Some(_) = handshakes.join_next(), if !handshakes.is_empty() => {}
                    result = tcp.accept() => {
                        let Ok((stream, addr)) = result else { continue };
                        let accepted_at = Instant::now();
                        let Some(permit) = admission.try_acquire() else { drop(stream); continue };
                        if handshakes.len() >= limits.max_handshakes { drop(stream); drop(permit); continue; }
                        let admit = Admit::new(permit);
                        let deadline = accepted_at + limits.preauth_deadline;
                        let acceptor = TlsAcceptor::from(config.borrow_and_update().clone());
                        let tx = tx.clone();
                        let kill = kill_rx.clone();
                        let timeout = limits.handshake;
                        handshakes.spawn(async move {
                            let Ok(Ok(tls)) = tokio::time::timeout(timeout, acceptor.accept(stream)).await else { return };
                            let io = TlsIo::new(tls, admit, deadline, kill);
                            let _ = tx.send((io, addr)).await;
                        });
                    }
                }
            }
        });
        TlsListener {
            rx,
            _task: AbortOnDrop(task),
            local,
            kill,
        }
    }
    pub fn kill_switch(&self) -> KillSwitch {
        self.kill.clone()
    }
}
impl Listener for TlsListener {
    type Io = TlsIo;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        self.rx.recv().await.expect("TLS listener task ended")
    }
    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local)
    }
}
impl<'a> Connected<IncomingStream<'a, TlsListener>> for Admit {
    fn connect_info(stream: IncomingStream<'a, TlsListener>) -> Self {
        stream.io().admit()
    }
}
impl fmt::Debug for TlsListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsListener")
            .field("local", &self.local)
            .finish()
    }
}

#[cfg(test)]
mod io_tests {
    use super::*;
    use rustls::{
        SignatureScheme,
        client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        pki_types::{CertificateDer, ServerName, UnixTime},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Debug)]
    struct AcceptAnyCert;
    impl ServerCertVerifier for AcceptAnyCert {
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
            _: &rustls::DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![
                SignatureScheme::RSA_PSS_SHA256,
                SignatureScheme::RSA_PKCS1_SHA256,
                SignatureScheme::ECDSA_NISTP256_SHA256,
                SignatureScheme::ED25519,
            ]
        }
    }

    #[tokio::test]
    async fn kill_latch_keeps_reads_at_eof_and_rejects_writes_and_flushes() {
        let files = generate("kill-latch");
        let config = server_config(&files).unwrap();
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
            .connect(
                "localhost".try_into().unwrap(),
                TcpStream::connect(addr).await.unwrap(),
            )
            .await
            .unwrap();
        let mut client = client;
        let stream = accept.await.unwrap();
        // Pending peer data: without the latch, a later read would return it.
        client.write_all(b"pending").await.unwrap();
        client.flush().await.unwrap();

        let admission = Admission::new(1);
        let admit = Admit::new(admission.semaphore.clone().try_acquire_owned().unwrap());
        admit.authenticated();
        let (kill_tx, kill_rx) = watch::channel(false);
        let kill = KillSwitch { tx: kill_tx };
        let mut io = TlsIo::new(
            stream,
            admit,
            Instant::now() + Duration::from_secs(30),
            kill_rx,
        );

        kill.kill();
        let mut buf = [0u8; 1];
        for _ in 0..3 {
            assert_eq!(
                io.read(&mut buf).await.unwrap(),
                0,
                "killed read stays at EOF"
            );
        }
        for _ in 0..3 {
            assert_eq!(
                io.write(b"x").await.unwrap_err().kind(),
                io::ErrorKind::BrokenPipe
            );
            assert_eq!(
                io.flush().await.unwrap_err().kind(),
                io::ErrorKind::BrokenPipe
            );
        }
        drop(client);
    }
}
