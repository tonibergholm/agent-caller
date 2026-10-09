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

// The reply is written to stdout without a bound of its own: this relies on
// the caller (Codex) draining stdout and enforcing its own hook timeout.
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
    let event = event.to_owned();
    // stdin is read concurrently with the filesystem checks; everything runs
    // inside the deadline.
    let (stdin_tx, stdin_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut body = Vec::new();
        let ok = std::io::stdin()
            .lock()
            .take(MAX_BODY as u64 + 1)
            .read_to_end(&mut body)
            .is_ok();
        let _ = stdin_tx.send(ok.then_some(body));
    });
    within(deadline, move || {
        let dir = match config_dir {
            Some(d) => d,
            None => crate::paths::config_dir().ok()?,
        };
        // Everything below uses the resolved directory, never the given path.
        let (dir, sock) = checked_socket(&dir)?;
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
        let body = stdin_rx
            .recv_timeout(deadline.checked_duration_since(Instant::now())?)
            .ok()??;
        if body.len() > MAX_BODY {
            return None;
        }
        post(
            &sock,
            &format!("/hook/codex/{event}"),
            &headers,
            &body,
            deadline,
        )
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

/// An ancestor directory is safe when root or the effective uid owns it and
/// nobody else can rename its entries: not group/other-writable, unless the
/// sticky bit is set (as on /tmp).
fn ancestor_safe(uid: u32, mode: u32, euid: u32) -> bool {
    (uid == 0 || uid == euid) && (mode & 0o022 == 0 || mode & 0o1000 != 0)
}

/// Every ancestor of `dir`, lexically and after resolving symlinks, is a
/// safe directory, and each symlink on the way belongs to root or us.
fn ancestors_safe(dir: &Path, euid: u32) -> bool {
    let check = |p: &Path| {
        std::fs::metadata(p).is_ok_and(|m| m.is_dir() && ancestor_safe(m.uid(), m.mode(), euid))
    };
    let Ok(abs) = std::path::absolute(dir) else {
        return false;
    };
    let Ok(canon) = std::fs::canonicalize(dir) else {
        return false;
    };
    for p in abs.ancestors().skip(1) {
        if !check(p) {
            return false;
        }
        match std::fs::symlink_metadata(p) {
            Ok(m) if m.file_type().is_symlink() && m.uid() != 0 && m.uid() != euid => {
                return false;
            }
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    canon.ancestors().skip(1).all(check)
}

/// Resolves `dir` once and returns the canonical directory and its
/// `hook.sock` if both are what amendment A1 requires: the canonical dir is
/// owned by the effective uid with mode 0700, every ancestor (given path and
/// canonical path) is unrenameable by others, and the socket is a socket
/// (not a symlink) owned by the effective uid with mode 0600. Callers must
/// connect to the returned path: a canonical path holds no symlinks, so
/// redirecting it needs a writable ancestor, which was just rejected.
fn checked_socket(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    // SAFETY: geteuid has no preconditions.
    let euid = unsafe { libc::geteuid() };
    if !ancestors_safe(dir, euid) {
        return None;
    }
    let canon = std::fs::canonicalize(dir).ok()?;
    let d = std::fs::symlink_metadata(&canon).ok()?;
    if !d.file_type().is_dir() || d.uid() != euid || d.mode() & 0o7777 != 0o700 {
        return None;
    }
    let sock = canon.join("hook.sock");
    let m = std::fs::symlink_metadata(&sock).ok()?;
    if !m.file_type().is_socket() || m.uid() != euid || m.mode() & 0o7777 != 0o600 {
        return None;
    }
    Some((canon, sock))
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
                if response_end(&resp)?.is_some() {
                    // Complete; any surplus bytes make parse_response reject.
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    parse_response(&resp)
}

/// The single `Content-Length` of a head: `Ok(None)` if absent, `Err` if
/// duplicated or malformed.
fn content_length(head: &str) -> Result<Option<usize>, ()> {
    let mut found = None;
    for l in head.lines() {
        if let Some(v) = l.to_ascii_lowercase().strip_prefix("content-length:") {
            if found.is_some() {
                return Err(());
            }
            // ASCII digits only (optional surrounding blanks): no sign.
            let v = v.trim_matches([' ', '\t']);
            if v.is_empty() || v.len() > 10 || !v.bytes().all(|b| b.is_ascii_digit()) {
                return Err(());
            }
            found = Some(v.parse::<usize>().map_err(|_| ())?);
        }
    }
    Ok(found)
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
    let Some(len) = content_length(&head).ok()? else {
        return Some(None);
    };
    if len > MAX_BODY {
        return None;
    }
    let end = pos + 4 + len;
    Some((resp.len() >= end).then_some(end))
}

/// The body of a 2xx reply with a plain (non-chunked) body of exactly the
/// declared length.
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
    if let Some(len) = content_length(head).ok()?
        && body.len() != len
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
    fn the_declared_length_must_match_exactly() {
        let ok = |r: &[u8]| parse_response(r).is_some();
        assert!(!ok(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{}"));
        assert!(!ok(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n{}"));
        assert!(!ok(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}"
        ));
        assert!(!ok(b"HTTP/1.1 200 OK\r\nContent-Length: x\r\n\r\n{}"));
        assert!(!ok(b"HTTP/1.1 200 OK\r\nContent-Length: +2\r\n\r\n{}"));
        assert!(!ok(b"HTTP/1.1 200 OK\r\nContent-Length: 2 2\r\n\r\n{}"));
        assert!(!ok(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}extra"));
        assert!(ok(b"HTTP/1.1 200 OK\r\nContent-Length:2\r\n\r\n{}"));
        assert!(
            response_end(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\n")
                .is_none()
        );
    }

    #[test]
    fn ancestors_must_be_unrenameable_by_others() {
        let me = 501;
        assert!(ancestor_safe(me, 0o755, me));
        assert!(ancestor_safe(0, 0o755, me));
        assert!(ancestor_safe(0, 0o1777, me), "sticky /tmp");
        assert!(!ancestor_safe(me, 0o777, me));
        assert!(!ancestor_safe(me, 0o775, me));
        assert!(!ancestor_safe(me, 0o757, me));
        assert!(!ancestor_safe(502, 0o755, me), "another user's directory");
        assert!(!ancestor_safe(502, 0o1777, me));
    }

    #[test]
    fn unexpected_arguments_send_nothing() {
        let a = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(run(&a(&["claude", "Stop"])).is_none());
        assert!(run(&a(&["codex", "Stop;rm"])).is_none());
        assert!(run(&a(&["codex", "Stop", "--surprise"])).is_none());
    }
}
