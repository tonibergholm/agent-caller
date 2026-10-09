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

fn install(
    dir: &std::path::Path,
    home: &std::path::Path,
    target: &str,
    extra: &[&str],
) -> std::process::Output {
    acd_home(dir, home)
        .args(["hooks", "install", target])
        .args(extra)
        .output()
        .unwrap()
}
fn uninstall(dir: &std::path::Path, home: &std::path::Path, target: &str) -> std::process::Output {
    acd_home(dir, home)
        .args(["hooks", "uninstall", target])
        .output()
        .unwrap()
}
fn json_of(p: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}
fn owned_count(v: &serde_json::Value) -> usize {
    v["hooks"].as_object().map_or(0, |events| {
        events
            .values()
            .flat_map(|groups| groups.as_array().into_iter().flatten())
            .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
            .filter(|e| e.to_string().contains("acd-managed:v1"))
            .count()
    })
}
const FOREIGN_CLAUDE: &str = r#"{
 "permissions": {"allow": ["Bash(npm test)"]},
 "hooks": {
  "Stop": [{"hooks": [{"type": "command", "command": "say done"}]}],
  "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "guard.sh"}]}]
 },
 "model": "opus"
}"#;
#[test]
fn claude_install_writes_owned_entries_with_the_token_at_mode_0600() {
    let (home, dir) = home_with_acd();
    std::fs::write(dir.join("config.toml"), "hook_port = 47999\n").unwrap();
    let out = install(&dir, home.path(), "claude", &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let p = home.path().join(".claude/settings.json");
    assert_eq!(mode(&p), 0o600);
    let v = json_of(&p);
    let token = hook_token_of(&dir);
    for (event, timeout) in [
        ("UserPromptSubmit", 1),
        ("PermissionRequest", 3610),
        ("PreToolUse", 1),
        ("PostToolUse", 1),
        ("Stop", 1),
        ("SessionEnd", 1),
    ] {
        let e = &v["hooks"][event][0]["hooks"][0];
        assert_eq!(e["type"], "http", "{event}");
        assert_eq!(e["timeout"], timeout, "{event}");
        assert_eq!(
            e["url"],
            format!("http://127.0.0.1:47999/hook/claude/{event}?marker=acd-managed:v1")
        );
        assert_eq!(e["headers"]["X-Acd-Token"], token.as_str());
        assert_eq!(e["headers"]["X-Herdr-Pane"], "$HERDR_PANE_ID");
        assert_eq!(
            e["allowedEnvVars"],
            serde_json::json!([
                "HERDR_PANE_ID",
                "HERDR_SOCKET_PATH",
                "CLAUDE_CODE_ENTRYPOINT"
            ])
        );
    }
    assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "AskUserQuestion");
    assert!(v["hooks"].get("SessionStart").is_none());
    assert_eq!(mode(&dir.join("hooks-installed.json")), 0o600);
    for o in [&out.stdout, &out.stderr] {
        assert!(!String::from_utf8_lossy(o).contains(&token));
    }
}
#[test]
fn install_twice_then_uninstall_is_identity() {
    for (target, rel, original) in [
        ("claude", ".claude/settings.json", FOREIGN_CLAUDE),
        (
            "codex",
            ".codex/hooks.json",
            r#"{"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "notify"}]}]}}"#,
        ),
    ] {
        let (home, dir) = home_with_acd();
        let p = home.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, original).unwrap();
        assert!(install(&dir, home.path(), target, &[]).status.success());
        assert!(install(&dir, home.path(), target, &[]).status.success());
        assert_eq!(
            owned_count(&json_of(&p)),
            if target == "claude" { 7 } else { 5 }
        );
        let out = uninstall(&dir, home.path(), target);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            json_of(&p),
            serde_json::from_str::<serde_json::Value>(original).unwrap()
        );
    }
}
#[test]
fn refuses_symlink_and_tracked_settings() {
    let (home, dir) = home_with_acd();
    let c = home.path().join(".claude");
    std::fs::create_dir_all(&c).unwrap();
    let real = home.path().join("dotfiles-settings.json");
    std::fs::write(&real, "{}").unwrap();
    std::os::unix::fs::symlink(&real, c.join("settings.json")).unwrap();
    let out = install(&dir, home.path(), "claude", &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("symlink"));
    std::fs::remove_file(c.join("settings.json")).unwrap();
    std::fs::write(c.join("settings.json"), "{}").unwrap();
    let git = |a: &[&str]| {
        std::process::Command::new("git")
            .current_dir(&c)
            .args(a)
            .env("HOME", home.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    git(&["add", "settings.json"]);
    let out = install(&dir, home.path(), "claude", &[]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("tracked by Git") && err.contains("--allow-tracked"),
        "{err}"
    );
    assert!(
        install(&dir, home.path(), "claude", &["--allow-tracked"])
            .status
            .success()
    );
}
#[test]
fn uninstall_survives_claude_reformatting() {
    let (home, dir) = home_with_acd();
    let p = home.path().join(".claude/settings.json");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, FOREIGN_CLAUDE).unwrap();
    install(&dir, home.path(), "claude", &[]);
    let mut v = json_of(&p);
    v["permissions"]["allow"]
        .as_array_mut()
        .unwrap()
        .push("Bash(touch /tmp/*)".into());
    std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    assert!(uninstall(&dir, home.path(), "claude").status.success());
    let mut want: serde_json::Value = serde_json::from_str(FOREIGN_CLAUDE).unwrap();
    want["permissions"]["allow"]
        .as_array_mut()
        .unwrap()
        .push("Bash(touch /tmp/*)".into());
    assert_eq!(json_of(&p), want);
}
#[test]
fn no_file_before_install_means_no_file_after_uninstall() {
    let (home, dir) = home_with_acd();
    assert!(install(&dir, home.path(), "codex", &[]).status.success());
    let p = home.path().join(".codex/hooks.json");
    assert!(p.exists());
    assert!(uninstall(&dir, home.path(), "codex").status.success());
    assert!(!p.exists());
}
#[test]
fn codex_commands_call_the_absolute_binary_with_config_dir_and_marker() {
    let (home, dir) = home_with_acd();
    assert!(install(&dir, home.path(), "codex", &[]).status.success());
    let v = json_of(&home.path().join(".codex/hooks.json"));
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_acd")).unwrap();
    for (event, timeout) in [
        ("SessionStart", 2),
        ("UserPromptSubmit", 2),
        ("PermissionRequest", 1),
        ("PostToolUse", 2),
        ("Stop", 2),
    ] {
        let e = &v["hooks"][event][0]["hooks"][0];
        assert_eq!(e["type"], "command");
        assert_eq!(e["timeout"], timeout);
        let cmd = e["command"].as_str().unwrap();
        assert!(
            cmd.starts_with(&format!(
                "'{}' hook codex {event} --config-dir '",
                exe.display()
            )),
            "{cmd}"
        );
        assert!(cmd.ends_with(" acd-managed:v1"));
    }
    assert!(
        !String::from_utf8_lossy(&std::fs::read(dir.join("hooks-installed.json")).unwrap())
            .is_empty()
    );
}
#[test]
fn backups_are_private_and_outside_settings_dir() {
    let (home, dir) = home_with_acd();
    let p = home.path().join(".claude/settings.json");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, FOREIGN_CLAUDE).unwrap();
    install(&dir, home.path(), "claude", &[]);
    let b = dir.join("backups");
    assert_eq!(mode(&b), 0o700);
    let files: Vec<_> = std::fs::read_dir(&b)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(mode(&files[0]), 0o600);
    assert_eq!(std::fs::read_to_string(&files[0]).unwrap(), FOREIGN_CLAUDE);
    assert_eq!(std::fs::read_dir(p.parent().unwrap()).unwrap().count(), 1);
}
#[test]
fn claude_config_dir_is_honoured() {
    let (home, dir) = home_with_acd();
    let c = home.path().join("custom-claude");
    let out = acd_home(&dir, home.path())
        .env("CLAUDE_CONFIG_DIR", &c)
        .args(["hooks", "install", "claude"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(c.join("settings.json").exists());
    assert!(!home.path().join(".claude/settings.json").exists());
}
#[test]
fn concurrent_installs_are_serialized() {
    let (home, dir) = home_with_acd();
    let kids: Vec<_> = (0..4)
        .map(|_| {
            acd_home(&dir, home.path())
                .args(["hooks", "install", "claude"])
                .spawn()
                .unwrap()
        })
        .collect();
    for mut k in kids {
        assert!(k.wait().unwrap().success());
    }
    assert_eq!(
        owned_count(&json_of(&home.path().join(".claude/settings.json"))),
        7
    );
}
#[cfg(debug_assertions)]
#[test]
fn a_change_between_read_and_rename_is_kept() {
    let (home, dir) = home_with_acd();
    let p = home.path().join(".claude/settings.json");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "{}").unwrap();
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_TOUCH_SETTINGS", r#"{"model":"opus"}"#)
        .args(["hooks", "install", "claude"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_of(&p);
    assert_eq!(v["model"], "opus");
    assert_eq!(owned_count(&v), 7);
}
#[test]
fn a_changed_config_root_gets_its_own_record() {
    let (home, dir) = home_with_acd();
    let a = home.path().join("claude-a");
    let b = home.path().join("claude-b");
    for r in [&a, &b] {
        assert!(
            acd_home(&dir, home.path())
                .env("CLAUDE_CONFIG_DIR", r)
                .args(["hooks", "install", "claude"])
                .status()
                .unwrap()
                .success()
        );
    }
    let rec = json_of(&dir.join("hooks-installed.json"));
    assert_eq!(rec["files"].as_object().unwrap().len(), 2);
    assert!(
        acd_home(&dir, home.path())
            .env("CLAUDE_CONFIG_DIR", &b)
            .args(["hooks", "uninstall", "claude"])
            .status()
            .unwrap()
            .success()
    );
    assert!(!b.join("settings.json").exists());
    assert_eq!(owned_count(&json_of(&a.join("settings.json"))), 7);
}
#[cfg(debug_assertions)]
#[test]
fn a_record_write_failure_is_repaired_by_the_journal() {
    let (home, dir) = home_with_acd();
    let p = home.path().join(".claude/settings.json");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, FOREIGN_CLAUDE).unwrap();
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_FAIL_AT", "record-write")
        .args(["hooks", "install", "claude", "--allow-unknown-git"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("injected failure at record-write"));
    assert!(dir.join("hooks-journal.json").exists());
    assert_eq!(owned_count(&json_of(&p)), 7);
    assert!(uninstall(&dir, home.path(), "claude").status.success());
    assert_eq!(
        json_of(&p),
        serde_json::from_str::<serde_json::Value>(FOREIGN_CLAUDE).unwrap()
    );
    assert!(!dir.join("hooks-journal.json").exists());
}
#[cfg(debug_assertions)]
#[test]
fn an_unknown_git_status_is_refused_without_the_flag() {
    let (home, dir) = home_with_acd();
    let run = |extra: &[&str]| {
        acd_home(&dir, home.path())
            .env("ACD_TEST_GIT", "/nonexistent/git")
            .args(["hooks", "install", "claude"])
            .args(extra)
            .output()
            .unwrap()
    };
    let out = run(&[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--allow-unknown-git"));
    assert!(run(&["--allow-unknown-git"]).status.success());
}
#[test]
fn a_deleted_but_still_tracked_settings_file_is_refused() {
    let (home, dir) = home_with_acd();
    let c = home.path().join(".claude");
    std::fs::create_dir_all(&c).unwrap();
    std::fs::write(c.join("settings.json"), "{}").unwrap();
    let git = |a: &[&str]| {
        std::process::Command::new("git")
            .current_dir(&c)
            .args(a)
            .env("HOME", home.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    git(&["add", "settings.json"]);
    std::fs::remove_file(c.join("settings.json")).unwrap();
    let out = install(&dir, home.path(), "claude", &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("tracked by Git"));
}
#[test]
fn entries_that_only_mention_the_marker_are_left_alone() {
    let (home, dir) = home_with_acd();
    let p = home.path().join(".claude/settings.json");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let f = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo acd-managed:v1 >> /tmp/log"}]}]}}"#;
    std::fs::write(&p, f).unwrap();
    let out = install(&dir, home.path(), "claude", &[]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unexpected"));
    assert!(uninstall(&dir, home.path(), "claude").status.success());
    assert_eq!(
        json_of(&p),
        serde_json::from_str::<serde_json::Value>(f).unwrap()
    );
}
#[cfg(debug_assertions)]
#[test]
fn a_failure_after_the_rename_keeps_the_journal() {
    let (home, dir) = home_with_acd();
    let p = home.path().join(".claude/settings.json");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, FOREIGN_CLAUDE).unwrap();
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_FAIL_AT", "settings-post-rename")
        .args(["hooks", "install", "claude", "--allow-unknown-git"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("injected failure at settings-post-rename")
    );
    assert_eq!(owned_count(&json_of(&p)), 7, "the rename happened");
    assert!(
        dir.join("hooks-journal.json").exists(),
        "the journal outlives the rename"
    );
    assert!(uninstall(&dir, home.path(), "claude").status.success());
    assert_eq!(
        json_of(&p),
        serde_json::from_str::<serde_json::Value>(FOREIGN_CLAUDE).unwrap()
    );
    assert!(!dir.join("hooks-journal.json").exists());
}
#[cfg(debug_assertions)]
#[test]
fn a_failure_before_the_rename_removes_the_journal_and_the_temp_file() {
    let (home, dir) = home_with_acd();
    let p = home.path().join(".claude/settings.json");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "{}").unwrap();
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_FAIL_AT", "settings-write")
        .args(["hooks", "install", "claude", "--allow-unknown-git"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "{}");
    assert!(!dir.join("hooks-journal.json").exists());
    let leftovers: Vec<_> = std::fs::read_dir(p.parent().unwrap()).unwrap().collect();
    assert_eq!(leftovers.len(), 1, "no temp file: {leftovers:?}");
}
#[cfg(debug_assertions)]
#[test]
fn a_change_after_staging_is_kept_and_a_constant_rewriter_ends_in_an_error() {
    let (home, dir) = home_with_acd();
    let p = home.path().join(".claude/settings.json");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "{}").unwrap();
    let rewrite = format!(
        "printf '{{\"n\": %s}}' \"$(od -An -N4 -tu4 /dev/urandom | tr -d ' ')\" > '{}'",
        p.display()
    );
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_RUN_AT", format!("after-stage={rewrite}"))
        .args(["hooks", "install", "claude", "--allow-unknown-git"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("kept changing"));
    let v = json_of(&p);
    assert!(
        v.get("n").is_some() && owned_count(&v) == 0,
        "Claude's last write survives"
    );
    assert!(!dir.join("hooks-journal.json").exists());
    let leftovers: Vec<_> = std::fs::read_dir(p.parent().unwrap()).unwrap().collect();
    assert_eq!(leftovers.len(), 1, "no temp file: {leftovers:?}");
}
#[cfg(debug_assertions)]
#[test]
fn a_file_tracked_between_the_check_and_the_write_is_refused() {
    let (home, dir) = home_with_acd();
    let c = home.path().join(".claude");
    std::fs::create_dir_all(&c).unwrap();
    std::fs::write(c.join("settings.json"), "{}").unwrap();
    let git_env = |cmd: &mut std::process::Command| {
        cmd.env("HOME", home.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
    };
    let mut init = std::process::Command::new("git");
    init.current_dir(&c).args(["init", "-q"]);
    git_env(&mut init);
    assert!(init.status().unwrap().success());
    let out = acd_home(&dir, home.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env(
            "ACD_TEST_RUN_AT",
            format!("after-stage=git -C '{}' add settings.json", c.display()),
        )
        .args(["hooks", "install", "claude"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("tracked by Git"));
    assert_eq!(
        std::fs::read_to_string(c.join("settings.json")).unwrap(),
        "{}"
    );
    assert!(!dir.join("hooks-journal.json").exists());
}
#[cfg(debug_assertions)]
#[test]
fn a_retargeted_parent_symlink_does_not_move_the_write() {
    let (home, dir) = home_with_acd();
    let real1 = home.path().join("dotfiles-a");
    let real2 = home.path().join("dotfiles-b");
    std::fs::create_dir_all(&real1).unwrap();
    std::fs::create_dir_all(&real2).unwrap();
    let link = home.path().join(".claude");
    std::os::unix::fs::symlink(&real1, &link).unwrap();
    let swap = format!(
        "rm '{l}' && ln -s '{b}' '{l}'",
        l = link.display(),
        b = real2.display()
    );
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_RUN_AT", format!("after-stage={swap}"))
        .args(["hooks", "install", "claude", "--allow-unknown-git"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        std::fs::read_dir(&real2).unwrap().next().is_none(),
        "nothing written through the new target"
    );
    let written = real1.canonicalize().unwrap().join("settings.json");
    assert_eq!(owned_count(&json_of(&written)), 7);
    let rec = json_of(&dir.join("hooks-installed.json"));
    assert!(
        rec["files"].get(written.to_str().unwrap()).is_some(),
        "{rec}"
    );
}
#[test]
fn backups_never_collide() {
    let (home, dir) = home_with_acd();
    let p = home.path().join(".claude/settings.json");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, FOREIGN_CLAUDE).unwrap();
    for _ in 0..5 {
        assert!(
            install(&dir, home.path(), "claude", &["--allow-unknown-git"])
                .status
                .success()
        );
        assert!(uninstall(&dir, home.path(), "claude").status.success());
        std::fs::write(&p, FOREIGN_CLAUDE).unwrap();
    }
    let n = std::fs::read_dir(dir.join("backups")).unwrap().count();
    assert!(n >= 10, "{n} backups");
}
#[cfg(debug_assertions)]
#[test]
fn a_journal_with_a_foreign_rewrite_is_kept_and_blocks_new_edits() {
    let (home, dir) = home_with_acd();
    let p = home.path().join(".claude/settings.json");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, FOREIGN_CLAUDE).unwrap();
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_FAIL_AT", "record-write")
        .args(["hooks", "install", "claude", "--allow-unknown-git"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    std::fs::write(&p, r#"{"model": "claude rewrote me"}"#).unwrap();
    let journal = std::fs::read(dir.join("hooks-journal.json")).unwrap();
    for out in [
        install(&dir, home.path(), "claude", &["--allow-unknown-git"]),
        uninstall(&dir, home.path(), "claude"),
    ] {
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("conflict"));
        assert_eq!(
            std::fs::read(dir.join("hooks-journal.json")).unwrap(),
            journal
        );
    }
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        r#"{"model": "claude rewrote me"}"#
    );
}
#[cfg(debug_assertions)]
#[test]
fn a_directory_replaced_after_it_is_pinned_does_not_receive_the_write() {
    let (home, dir) = home_with_acd();
    let c = home.path().join(".claude");
    let other = home.path().join("other");
    std::fs::create_dir_all(&c).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(c.join("settings.json"), "{}").unwrap();
    std::fs::write(other.join("settings.json"), "{}").unwrap();
    let moved = home.path().join("claude-moved");
    let swap = format!(
        "mv '{c}' '{m}' && ln -s '{o}' '{c}'",
        c = c.display(),
        m = moved.display(),
        o = other.display()
    );
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_RUN_AT", format!("after-stage={swap}"))
        .args(["hooks", "install", "claude", "--allow-unknown-git"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(other.join("settings.json")).unwrap(),
        "{}"
    );
    assert_eq!(
        std::fs::read_dir(&other).unwrap().count(),
        1,
        "no temp file either"
    );
    assert_eq!(owned_count(&json_of(&moved.join("settings.json"))), 7);
    assert_eq!(
        std::fs::read_dir(&moved).unwrap().count(),
        1,
        "no temp file left"
    );
}
#[cfg(debug_assertions)]
#[test]
fn a_failed_directory_sync_after_an_unlink_keeps_the_journal() {
    let (home, dir) = home_with_acd();
    let p = home.path().join(".claude/settings.json");
    assert!(
        install(&dir, home.path(), "claude", &["--allow-unknown-git"])
            .status
            .success()
    );
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_FAIL_AT", "settings-post-rename")
        .args(["hooks", "uninstall", "claude"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!p.exists(), "the file was removed");
    assert!(dir.join("hooks-journal.json").exists());
    assert!(
        json_of(&dir.join("hooks-installed.json"))["files"]
            .as_object()
            .unwrap()
            .len()
            == 1,
        "the record is saved only after the sync"
    );
    assert!(
        install(&dir, home.path(), "claude", &["--allow-unknown-git"])
            .status
            .success()
    );
    assert!(!dir.join("hooks-journal.json").exists());
    assert_eq!(owned_count(&json_of(&p)), 7);
}
