//! A minimal HTTP/1.1 POST client (hyper + rustls) shared by the relay's FCM
//! client and acd's wake sender.
//!
//! Rules: redirects are never followed (a 3xx is returned as a status),
//! proxy environment variables are ignored, TLS roots come only from a CA
//! file the caller names, the response body is read up to a cap and the
//! whole exchange runs under one deadline. Errors never contain the URL,
//! headers or body.

use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Request, Uri};
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    #[error("bad url")]
    BadUrl,
    #[error("plain http is not allowed")]
    PlainNotAllowed,
    #[error("connect failed")]
    Connect,
    #[error("tls failed")]
    Tls,
    #[error("timeout")]
    Timeout,
    #[error("protocol error")]
    Protocol,
}

#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    /// At most `max_body` bytes.
    pub body: Vec<u8>,
}

#[derive(Clone)]
pub struct HttpClient {
    tls: Option<Arc<rustls::ClientConfig>>,
    allow_plain: bool,
    timeout: Duration,
    max_body: usize,
}

impl HttpClient {
    /// HTTPS only, trusting exactly the certificates in `ca_pem`.
    pub fn https(ca_pem: &[u8], timeout: Duration, max_body: usize) -> Result<HttpClient, HttpError> {
        let mut roots = rustls::RootCertStore::empty();
        let mut n = 0;
        for c in CertificateDer::pem_slice_iter(ca_pem) {
            let c = c.map_err(|_| HttpError::Tls)?;
            roots.add(c).map_err(|_| HttpError::Tls)?;
            n += 1;
        }
        if n == 0 {
            return Err(HttpError::Tls);
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| HttpError::Tls)?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(HttpClient { tls: Some(Arc::new(cfg)), allow_plain: false, timeout, max_body })
    }

    /// Plain `http://` only. For tests against local fakes; production code
    /// uses [`HttpClient::https`].
    pub fn plain_for_tests(timeout: Duration, max_body: usize) -> HttpClient {
        HttpClient { tls: None, allow_plain: true, timeout, max_body }
    }

    pub async fn post(
        &self,
        url: &str,
        content_type: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Result<HttpResponse, HttpError> {
        tokio::time::timeout(self.timeout, self.post_inner(url, content_type, headers, body))
            .await
            .map_err(|_| HttpError::Timeout)?
    }

    async fn post_inner(
        &self,
        url: &str,
        content_type: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Result<HttpResponse, HttpError> {
        let uri: Uri = url.parse().map_err(|_| HttpError::BadUrl)?;
        let host = uri.host().ok_or(HttpError::BadUrl)?.to_string();
        let https = match uri.scheme_str() {
            Some("https") => true,
            Some("http") => false,
            _ => return Err(HttpError::BadUrl),
        };
        if uri.authority().is_none_or(|a| a.as_str().contains('@')) {
            return Err(HttpError::BadUrl);
        }
        let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });
        let tcp = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|_| HttpError::Connect)?;
        let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
        let mut req = Request::post(path)
            .header("host", uri.authority().map(|a| a.as_str()).unwrap_or(&host))
            .header("content-type", content_type)
            .header("content-length", body.len())
            .header("connection", "close");
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let req = req.body(Full::new(Bytes::from(body))).map_err(|_| HttpError::BadUrl)?;
        if https {
            let cfg = self.tls.clone().ok_or(HttpError::Tls)?;
            let name = ServerName::try_from(host.clone()).map_err(|_| HttpError::BadUrl)?;
            let tls = TlsConnector::from(cfg)
                .connect(name, tcp)
                .await
                .map_err(|_| HttpError::Tls)?;
            self.exchange(TokioIo::new(tls), req).await
        } else {
            if !self.allow_plain {
                return Err(HttpError::PlainNotAllowed);
            }
            self.exchange(TokioIo::new(tcp), req).await
        }
    }

    async fn exchange<T>(&self, io: T, req: Request<Full<Bytes>>) -> Result<HttpResponse, HttpError>
    where
        T: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
    {
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|_| HttpError::Protocol)?;
        let task = tokio::spawn(async move {
            let _ = conn.await;
        });
        let res = sender.send_request(req).await.map_err(|_| HttpError::Protocol);
        let out = match res {
            Ok(res) => {
                let status = res.status().as_u16();
                let mut body = res.into_body();
                let mut buf = Vec::new();
                while buf.len() < self.max_body {
                    match body.frame().await {
                        Some(Ok(f)) => {
                            if let Some(d) = f.data_ref() {
                                let room = self.max_body - buf.len();
                                buf.extend_from_slice(&d[..d.len().min(room)]);
                            }
                        }
                        Some(Err(_)) => break,
                        None => break,
                    }
                }
                Ok(HttpResponse { status, body: buf })
            }
            Err(e) => Err(e),
        };
        task.abort();
        out
    }
}
