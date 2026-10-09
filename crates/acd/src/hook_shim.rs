//! `acd hook codex <event>` (spec §6.1): Codex's command hook. Reads the
//! payload on stdin, forwards it to the local hook endpoint, prints the
//! reply body and exits 0. Any failure prints nothing and still exits 0, so
//! Codex shows its own dialog. It never logs and never prints the token.
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const MAX_BODY: usize = 1 << 20;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

pub fn main(args: &[OsString]) -> i32 {
    if let Some(body) = run(args) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(body.as_bytes());
        let _ = out.flush();
    }
    0
}

/// Inside Codex's own timeouts (§6.3: 1 s for PermissionRequest, 2 s else).
fn budget(event: &str) -> Duration {
    if event == "PermissionRequest" {
        Duration::from_millis(800)
    } else {
        Duration::from_millis(1500)
    }
}

fn header_safe(v: &str) -> bool {
    !v.is_empty() && v.len() <= 1024 && !v.chars().any(char::is_control)
}

fn run(args: &[OsString]) -> Option<String> {
    let mut it = args.iter().map(|a| a.to_str());
    let agent = it.next()??;
    let event = it.next()??;
    if agent != "codex"
        || event.is_empty()
        || event.len() > 64
        || !event.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return None;
    }
    let mut config_dir: Option<PathBuf> = None;
    while let Some(a) = it.next() {
        match a? {
            "--config-dir" => config_dir = Some(PathBuf::from(it.next()??)),
            "acd-managed:v1" => {}
            _ => return None,
        }
    }
    let deadline = Instant::now() + budget(event);
    let mut body = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut body)
        .ok()?;
    if body.len() > MAX_BODY {
        return None;
    }
    let dir = match config_dir {
        Some(d) => d,
        None => crate::paths::config_dir().ok()?,
    };
    let token = crate::hook_token::load(&dir).ok()?.current;
    let port = crate::config::Config::load(&dir).ok()?.hook_port;
    let mut headers = vec![("X-Acd-Token".to_string(), token)];
    // Hints only (D17): acd never binds Codex with them.
    for (env, header) in [
        ("HERDR_PANE_ID", "X-Herdr-Pane"),
        ("HERDR_SOCKET_PATH", "X-Herdr-Socket"),
    ] {
        if let Ok(v) = std::env::var(env)
            && header_safe(&v)
        {
            headers.push((header.to_string(), v));
        }
    }
    post(
        port,
        &format!("/hook/codex/{event}"),
        &headers,
        &body,
        deadline,
    )
}

fn post(
    port: u16,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
    deadline: Instant,
) -> Option<String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    let left = deadline.checked_duration_since(Instant::now())?;
    s.set_write_timeout(Some(left)).ok()?;
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).ok()?;
    s.write_all(body).ok()?;
    let mut resp = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        s.set_read_timeout(Some(left)).ok()?;
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                resp.extend_from_slice(&buf[..n]);
                if resp.len() > MAX_BODY + 8192 {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
    parse_response(&resp)
}

/// The body of a 2xx reply with a plain (non-chunked) body.
fn parse_response(resp: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(resp).ok()?;
    let (head, body) = text.split_once("\r\n\r\n")?;
    let status: u16 = head.split_whitespace().nth(1)?.parse().ok()?;
    if !(200..300).contains(&status) {
        return None;
    }
    if head
        .lines()
        .any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:"))
    {
        return None;
    }
    Some(body.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_2xx_bodies_are_printed() {
        assert_eq!(
            parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").as_deref(),
            Some("{}")
        );
        assert_eq!(
            parse_response(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").as_deref(),
            Some("")
        );
        assert_eq!(parse_response(b"HTTP/1.1 401 Unauthorized\r\n\r\nno"), None);
        assert_eq!(
            parse_response(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n"
            ),
            None
        );
        assert_eq!(parse_response(b"garbage"), None);
    }

    #[test]
    fn unexpected_arguments_send_nothing() {
        let a = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(run(&a(&["claude", "Stop"])).is_none());
        assert!(run(&a(&["codex", "Stop;rm"])).is_none());
        assert!(run(&a(&["codex", "Stop", "--surprise"])).is_none());
    }
}
