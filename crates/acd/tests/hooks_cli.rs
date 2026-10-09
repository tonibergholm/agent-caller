mod common;
use common::*;

pub fn store_of(dir: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("hook-tokens.json")).unwrap()).unwrap()
}

pub fn hook_token_of(dir: &std::path::Path) -> String {
    store_of(dir)["current"].as_str().unwrap().to_owned()
}

fn home_with_acd() -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let dir = init_dir(home.path());
    (home, dir)
}

fn mode(p: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn init_creates_a_private_store_and_no_hook_token_file() {
    let (_home, dir) = home_with_acd();
    assert_eq!(mode(&dir.join("hook-tokens.json")), 0o600);
    assert!(!dir.join("hook.token").exists());
    assert_eq!(store_of(&dir)["generation"], 1);
}

#[cfg(debug_assertions)]
#[test]
fn a_failed_store_write_leaves_the_old_store_intact() {
    let (home, dir) = home_with_acd();
    let before = store_of(&dir);
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_FAIL_AT", "store-write")
        .args(["token", "rotate", "hook"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("injected failure at store-write"));
    assert_eq!(store_of(&dir), before);
}

fn run_within(mut cmd: std::process::Command, secs: u64) -> std::process::Output {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("did not finish within {secs} s (lock reacquired?)");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn init_migrates_a_legacy_only_dir_without_hanging() {
    let (home, dir) = home_with_acd();
    let token = hook_token_of(&dir);
    std::fs::remove_file(dir.join("hook-tokens.json")).unwrap();
    std::fs::write(dir.join("hook.token"), format!("{token}\n")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        dir.join("hook.token"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mut init = acd_home(&dir, home.path());
    init.arg("init");
    assert!(run_within(init, 10).status.success());
    assert_eq!(hook_token_of(&dir), token);
    assert!(!dir.join("hook.token").exists());
}

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq)]
enum Reply {
    Normal,
    /// Never answers.
    Stall,
    /// Sends a complete reply with Content-Length but keeps the connection open.
    KeepOpen,
    /// Accepts, never reads and never answers.
    NoRead,
    /// Sends this raw reply, then closes.
    Raw(&'static str),
}

fn set_mode(p: &std::path::Path, m: u32) {
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
}

/// A one-shot HTTP server on `<dir>/hook.sock` (mode 0600): records the
/// request and replies with `status` and `body` as `reply` says.
fn one_shot(
    dir: &std::path::Path,
    status: u16,
    body: &'static str,
    reply: Reply,
) -> mpsc::Receiver<String> {
    let sock = dir.join("hook.sock");
    let l = UnixListener::bind(&sock).unwrap();
    set_mode(&sock, 0o600);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut s, _)) = l.accept() else { return };
        if reply == Reply::NoRead {
            std::thread::sleep(Duration::from_secs(5));
            return;
        }
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut req = Vec::new();
        let mut buf = [0u8; 65536];
        loop {
            match s.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    req.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&req);
                    if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if rest.len() >= len {
                            break;
                        }
                    }
                }
            }
        }
        let _ = tx.send(String::from_utf8_lossy(&req).into_owned());
        if reply == Reply::Stall {
            std::thread::sleep(Duration::from_secs(5));
            return;
        }
        if let Reply::Raw(raw) = reply {
            let _ = s.write_all(raw.as_bytes());
            return;
        }
        let _ = write!(
            s,
            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let _ = s.flush();
        if reply == Reply::KeepOpen {
            std::thread::sleep(Duration::from_secs(5));
        }
    });
    rx
}

fn shim_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = init_dir(tmp.path());
    set_mode(&dir, 0o700);
    (tmp, dir)
}

fn run_shim(
    dir: &std::path::Path,
    event: &str,
    stdin: &[u8],
    env: &[(&str, &str)],
) -> (std::process::Output, Duration) {
    let mut cmd = acd(dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .args(["hook", "codex", event, "acd-managed:v1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let t = Instant::now();
    child.stdin.take().unwrap().write_all(stdin).ok();
    let out = child.wait_with_output().unwrap();
    (out, t.elapsed())
}

fn codex_fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/../../tests/fixtures/hooks/codex/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[test]
fn the_shim_forwards_the_hook_and_prints_the_reply() {
    let (_tmp, dir) = shim_dir();
    let rx = one_shot(&dir, 200, "{\"ok\":1}", Reply::Normal);
    let token = hook_token_of(&dir);
    let body = codex_fixture("payload-PermissionRequest-Bash.json");
    let (out, _) = run_shim(
        &dir,
        "PermissionRequest",
        &body,
        &[("HERDR_PANE_ID", "w1:p2"), ("HERDR_SOCKET_PATH", "")],
    );
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "{\"ok\":1}");
    let req = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        req.starts_with("POST /hook/codex/PermissionRequest HTTP/1.1\r\n"),
        "{req}"
    );
    assert!(req.contains(&format!("X-Acd-Token: {token}\r\n")));
    assert!(req.contains("X-Herdr-Pane: w1:p2\r\n"));
    assert!(
        !req.contains("X-Herdr-Socket"),
        "empty env values are not sent"
    );
    assert!(req.ends_with(std::str::from_utf8(&body).unwrap()));
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains(&token)
            && !String::from_utf8_lossy(&out.stderr).contains(&token)
    );
}

#[test]
fn the_shim_fails_open_when_acd_is_down() {
    let (_tmp, dir) = shim_dir(); // no hook.sock
    let (out, took) = run_shim(
        &dir,
        "PermissionRequest",
        &codex_fixture("payload-PermissionRequest-Bash.json"),
        &[],
    );
    assert!(out.status.success());
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    assert!(took < Duration::from_secs(1), "{took:?}");
}

#[test]
fn the_shim_fails_open_when_acd_stalls() {
    let (_tmp, dir) = shim_dir();
    let _rx = one_shot(&dir, 200, "", Reply::Stall);
    let (out, took) = run_shim(
        &dir,
        "PermissionRequest",
        &codex_fixture("payload-PermissionRequest-Bash.json"),
        &[],
    );
    assert!(out.status.success() && out.stdout.is_empty());
    assert!(
        took < Duration::from_millis(1200),
        "inside Codex's 1 s budget plus process start: {took:?}"
    );
}

#[test]
fn the_shim_prints_nothing_for_a_non_2xx_reply() {
    let (_tmp, dir) = shim_dir();
    let _rx = one_shot(&dir, 401, "{\"no\":1}", Reply::Normal);
    let (out, _) = run_shim(&dir, "Stop", b"{\"session_id\":\"s\"}", &[]);
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
}

#[test]
fn the_shim_refuses_oversized_input_without_sending() {
    let (_tmp, dir) = shim_dir();
    let rx = one_shot(&dir, 200, "x", Reply::Normal);
    let big = vec![b'a'; (1 << 20) + 1];
    let (out, _) = run_shim(&dir, "Stop", &big, &[]);
    assert!(out.status.success() && out.stdout.is_empty());
    assert!(
        rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "nothing was sent"
    );
}

#[test]
fn the_shim_without_a_token_prints_nothing() {
    let (_tmp, dir) = shim_dir();
    let rx = one_shot(&dir, 200, "x", Reply::Normal);
    std::fs::remove_file(dir.join("hook-tokens.json")).unwrap();
    let (out, _) = run_shim(&dir, "Stop", b"{}", &[]);
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
}

#[test]
fn the_shim_honours_an_explicit_config_dir() {
    let (_tmp, dir) = shim_dir();
    let rx = one_shot(&dir, 200, "ok", Reply::Normal);
    let other_home = tempfile::tempdir().unwrap();
    let mut cmd = acd_home(other_home.path(), other_home.path());
    cmd.env_remove("ACD_CONFIG_DIR");
    let out = cmd
        .args(["hook", "codex", "Stop", "--config-dir"])
        .arg(&dir)
        .arg("acd-managed:v1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok");
    assert!(rx.recv_timeout(Duration::from_secs(1)).is_ok());
}

/// The shim refuses an unsafe layout without connecting.
fn assert_refused(dir: &std::path::Path, rx: &mpsc::Receiver<String>) {
    let (out, _) = run_shim(dir, "Stop", b"{}", &[]);
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    assert!(
        rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "nothing was sent"
    );
}

#[test]
fn the_shim_refuses_a_config_dir_that_is_not_0700() {
    let (_tmp, dir) = shim_dir();
    let rx = one_shot(&dir, 200, "x", Reply::Normal);
    set_mode(&dir, 0o755);
    assert_refused(&dir, &rx);
    set_mode(&dir, 0o700);
}

#[test]
fn the_shim_refuses_a_socket_that_is_not_0600() {
    let (_tmp, dir) = shim_dir();
    let rx = one_shot(&dir, 200, "x", Reply::Normal);
    set_mode(&dir.join("hook.sock"), 0o666);
    assert_refused(&dir, &rx);
}

#[test]
fn the_shim_refuses_a_symlinked_socket() {
    let (_tmp, dir) = shim_dir();
    let other = tempfile::tempdir().unwrap();
    let real = other.path().join("real.sock");
    let l = UnixListener::bind(&real).unwrap();
    set_mode(&real, 0o600);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        if l.accept().is_ok() {
            let _ = tx.send(String::new());
        }
    });
    std::os::unix::fs::symlink(&real, dir.join("hook.sock")).unwrap();
    assert_refused(&dir, &rx);
}

#[test]
fn the_shim_follows_a_symlinked_config_dir_to_a_safe_canonical_dir() {
    let (tmp, dir) = shim_dir();
    let _rx = one_shot(&dir, 200, "ok", Reply::Normal);
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    let (out, _) = run_shim(&link, "Stop", b"{}", &[]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok");
}

#[test]
fn the_shim_refuses_a_symlink_chain_through_a_writable_directory() {
    let (tmp, dir) = shim_dir();
    let rx = one_shot(&dir, 200, "x", Reply::Normal);
    let bridge = tmp.path().join("bridge");
    std::fs::create_dir(&bridge).unwrap();
    set_mode(&bridge, 0o777);
    let link = bridge.join("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    assert_refused(&link, &rx);
}

#[test]
fn the_shim_with_no_socket_fails_open_fast() {
    let (_tmp, dir) = shim_dir();
    let (out, took) = run_shim(&dir, "PermissionRequest", b"{}", &[]);
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    assert!(took < Duration::from_millis(500), "{took:?}");
}

#[test]
fn the_shim_stops_reading_at_content_length() {
    let (_tmp, dir) = shim_dir();
    let _rx = one_shot(&dir, 200, "{\"ok\":2}", Reply::KeepOpen);
    let (out, took) = run_shim(&dir, "PermissionRequest", b"{}", &[]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "{\"ok\":2}");
    assert!(took < Duration::from_millis(700), "{took:?}");
}

#[test]
fn the_shim_gives_up_when_stdin_never_closes() {
    let (_tmp, dir) = shim_dir();
    let rx = one_shot(&dir, 200, "x", Reply::Normal);
    let child = acd(&dir)
        .args(["hook", "codex", "PermissionRequest", "acd-managed:v1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = child.unwrap();
    let _stdin = child.stdin.take().unwrap(); // held open, nothing written
    let t = Instant::now();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    assert!(
        t.elapsed() < Duration::from_millis(1200),
        "{:?}",
        t.elapsed()
    );
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
}

#[test]
fn the_shim_refuses_a_short_or_ambiguous_reply() {
    for raw in [
        "HTTP/1.1 200 X\r\nContent-Length: 100\r\n\r\n{}",
        "HTTP/1.1 200 X\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
    ] {
        let (_tmp, dir) = shim_dir();
        let _rx = one_shot(&dir, 200, "", Reply::Raw(raw));
        let (out, _) = run_shim(&dir, "Stop", b"{}", &[]);
        assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    }
}

#[test]
fn the_shim_refuses_a_writable_ancestor_unless_sticky() {
    let (tmp, dir) = shim_dir();
    let rx = one_shot(&dir, 200, "ok", Reply::Normal);
    set_mode(tmp.path(), 0o777);
    assert_refused(&dir, &rx);
    // The refused run never connected, so the server still waits.
    set_mode(tmp.path(), 0o1777);
    let (out, _) = run_shim(&dir, "Stop", b"{}", &[]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok");
}

#[test]
fn a_write_stall_is_bounded_by_the_deadline() {
    let (_tmp, dir) = shim_dir();
    let _rx = one_shot(&dir, 200, "x", Reply::NoRead);
    let big = vec![b'a'; 1 << 20];
    let (out, took) = run_shim(&dir, "PermissionRequest", &big, &[]);
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    assert!(took < Duration::from_millis(1200), "{took:?}");
}
