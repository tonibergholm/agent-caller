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
use std::net::TcpListener;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A one-shot HTTP server: records the request, replies with `status` and
/// `body`, or never replies when `stall`.
fn one_shot(status: u16, body: &'static str, stall: bool) -> (u16, mpsc::Receiver<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut s, _)) = l.accept() else { return };
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
        if stall {
            std::thread::sleep(Duration::from_secs(5));
            return;
        }
        let _ = write!(
            s,
            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
    });
    (port, rx)
}

fn shim_dir(port: u16) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = init_dir(tmp.path());
    std::fs::write(dir.join("config.toml"), format!("hook_port = {port}\n")).unwrap();
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
    let (port, rx) = one_shot(200, "{\"ok\":1}", false);
    let (_tmp, dir) = shim_dir(port);
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
    let port = free_port(); // nothing listens
    let (_tmp, dir) = shim_dir(port);
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
    let (port, _rx) = one_shot(200, "", true);
    let (_tmp, dir) = shim_dir(port);
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
    let (port, _rx) = one_shot(401, "{\"no\":1}", false);
    let (_tmp, dir) = shim_dir(port);
    let (out, _) = run_shim(&dir, "Stop", b"{\"session_id\":\"s\"}", &[]);
    assert!(out.status.success() && out.stdout.is_empty());
}

#[test]
fn the_shim_refuses_oversized_input_without_sending() {
    let (port, rx) = one_shot(200, "x", false);
    let (_tmp, dir) = shim_dir(port);
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
    let (port, rx) = one_shot(200, "x", false);
    let (_tmp, dir) = shim_dir(port);
    std::fs::remove_file(dir.join("hook-tokens.json")).unwrap();
    let (out, _) = run_shim(&dir, "Stop", b"{}", &[]);
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
}

#[test]
fn the_shim_honours_an_explicit_config_dir() {
    let (port, rx) = one_shot(200, "ok", false);
    let (_tmp, dir) = shim_dir(port);
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
