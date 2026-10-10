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
