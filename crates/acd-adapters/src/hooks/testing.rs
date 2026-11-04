//! A raw HTTP/1.1 client for hook tests: no client library, and it can
//! close a connection mid-hold the way Claude does.
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub async fn send(
    port: u16,
    path: &str,
    token: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if !token.is_empty() {
        request.push_str(&format!("X-Acd-Token: {token}\r\n"));
    }
    for (key, value) in headers {
        request.push_str(&format!("{key}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).await.unwrap();
    stream
}

pub async fn read_response(mut stream: TcpStream) -> (u16, String) {
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes).await;
    let response = String::from_utf8_lossy(&bytes).into_owned();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, body)
}

pub async fn post(
    port: u16,
    path: &str,
    token: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    read_response(send(port, path, token, headers, body).await).await
}

/// Like [`send`], but HTTP/1.1 keep-alive (no `Connection: close`), so the
/// connection outlives its response. `lf_only` ends every header line with
/// a bare `\n`, which hyper accepts and a `\r\n\r\n` scan would not see.
pub async fn send_keep_alive(
    port: u16,
    path: &str,
    token: &str,
    body: &str,
    lf_only: bool,
) -> TcpStream {
    let eol = if lf_only { "\n" } else { "\r\n" };
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let request = format!(
        "POST {path} HTTP/1.1{eol}Host: 127.0.0.1{eol}Content-Type: application/json{eol}Content-Length: {}{eol}X-Acd-Token: {token}{eol}{eol}{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream
}

/// Reads one response (head and `Content-Length` body) without waiting for
/// the server to close the connection.
pub async fn read_one_response(stream: &mut TcpStream) -> (u16, String) {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                let status = head
                    .split_whitespace()
                    .nth(1)
                    .and_then(|code| code.parse().ok())
                    .unwrap_or(0);
                let body = String::from_utf8_lossy(&bytes[end + 4..end + 4 + length]);
                return (status, body.into_owned());
            }
        }
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0, "the server closed before a full response");
        bytes.extend_from_slice(&chunk[..n]);
    }
}
