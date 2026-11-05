//! A minimal HTTP/1.1 client over a Unix socket (one request per
//! connection), enough for the producer API on `hook.sock`.
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

pub async fn request(
    sock: &Path,
    method: &str,
    path: &str,
    token: &str,
    body: &str,
    limit: Duration,
) -> Result<(u16, String), String> {
    let work = async {
        let mut s = UnixStream::connect(sock).await.map_err(|e| format!("connect: {e}"))?;
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: acd\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nX-Acd-Token: {token}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).await.map_err(|e| format!("write: {e}"))?;
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).await.map_err(|e| format!("read: {e}"))?;
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").ok_or("incomplete reply")?;
        let status = head
            .strip_prefix("HTTP/1.1 ")
            .and_then(|r| r.get(..3))
            .and_then(|c| c.parse().ok())
            .ok_or("no status line")?;
        Ok((status, body.to_owned()))
    };
    match tokio::time::timeout(limit, work).await {
        Ok(r) => r,
        Err(_) => Err("timeout".into()),
    }
}
