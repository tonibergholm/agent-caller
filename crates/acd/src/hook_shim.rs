//! `acd hook codex <event>` (spec §6.1, amendment A1): Codex's command hook.
//! Reads the payload on stdin, forwards it over the owner-only Unix socket
//! `<config_dir>/hook.sock` (never TCP), prints the reply body and exits 0.
//! Any failure prints nothing and still exits 0, so Codex shows its own
//! dialog. It never logs and never prints the token.
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const MAX_BODY: usize = 1 << 20;

pub fn main(args: &[OsString]) -> i32 {
    // Silent panic hook: the shim must never write to stderr.
    std::panic::set_hook(Box::new(|_| {}));
    let args = args.to_vec();
    let result = std::panic::catch_unwind(move || run(&args));
    if let Ok(Some(body)) = result {
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
    let dir = match config_dir {
        Some(d) => d,
        None => crate::paths::config_dir().ok()?,
    };
    let sock = checked_socket(&dir)?;
    let body = within(deadline, || {
        let mut body = Vec::new();
        std::io::stdin()
            .lock()
            .take(MAX_BODY as u64 + 1)
            .read_to_end(&mut body)
            .ok()?;
        Some(body)
    })?;
    if body.len() > MAX_BODY {
        return None;
    }
    let token = crate::hook_token::load(&dir).ok()?.current;
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
    let path = format!("/hook/codex/{event}");
    within(deadline, move || {
        post(&sock, &path, &headers, &body, deadline)
    })
}

/// Runs `f` on a helper thread and gives up at `deadline`. The abandoned
/// thread dies with the process.
fn within<T: Send + 'static>(
    deadline: Instant,
    f: impl FnOnce() -> Option<T> + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(deadline.checked_duration_since(Instant::now())?)
        .ok()?
}

/// `<dir>/hook.sock` if the directory and the socket are what amendment A1
/// requires: no symlinks, owned by the effective uid, modes 0700 and 0600.
fn checked_socket(dir: &Path) -> Option<PathBuf> {
    // SAFETY: geteuid has no preconditions.
    let euid = unsafe { libc::geteuid() };
    let d = std::fs::symlink_metadata(dir).ok()?;
    if !d.file_type().is_dir() || d.uid() != euid || d.mode() & 0o7777 != 0o700 {
        return None;
    }
    let sock = dir.join("hook.sock");
    let m = std::fs::symlink_metadata(&sock).ok()?;
    if !m.file_type().is_socket() || m.uid() != euid || m.mode() & 0o7777 != 0o600 {
        return None;
    }
    Some(sock)
}

fn post(
    sock: &Path,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
    deadline: Instant,
) -> Option<String> {
    let mut s = UnixStream::connect(sock).ok()?;
    // The socket timeouts are best effort: macOS rejects them once the peer
    // has closed, and `within` enforces the deadline regardless.
    let left = deadline.checked_duration_since(Instant::now())?;
    let _ = s.set_write_timeout(Some(left));
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
        let _ = s.set_read_timeout(Some(left));
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                resp.extend_from_slice(&buf[..n]);
                if resp.len() > MAX_BODY + 8192 {
                    return None;
                }
                if let Some(end) = response_end(&resp)? {
                    resp.truncate(end);
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    parse_response(&resp)
}

/// `Some(Some(end))` once the head and `Content-Length` body bytes have all
/// arrived, `Some(None)` while more is needed (or no length is declared and
/// EOF ends the body), `None` for a head that cannot be accepted.
fn response_end(resp: &[u8]) -> Option<Option<usize>> {
    let Some(pos) = resp.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Some(None);
    };
    let head = String::from_utf8_lossy(&resp[..pos]).to_ascii_lowercase();
    if head.lines().any(|l| l.starts_with("transfer-encoding:")) {
        return None;
    }
    let Some(len) = head.lines().find_map(|l| l.strip_prefix("content-length:")) else {
        return Some(None);
    };
    let len: usize = len.trim().parse().ok()?;
    if len > MAX_BODY {
        return None;
    }
    let end = pos + 4 + len;
    Some((resp.len() >= end).then_some(end))
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
