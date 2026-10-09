mod common;
use common::*;

pub fn store_of(dir: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("hook-tokens.json")).unwrap()).unwrap()
}

pub fn hook_token_of(dir: &std::path::Path) -> String {
    store_of(dir)["current"].as_str().unwrap().to_owned()
}

fn rotate(dir: &std::path::Path, home: &std::path::Path, extra: &[&str]) -> std::process::Output {
    acd_home(dir, home)
        .args(["token", "rotate", "hook"])
        .args(extra)
        .output()
        .unwrap()
}

fn settings_token(home: &std::path::Path) -> serde_json::Value {
    json_of(&home.join(".claude/settings.json"))["hooks"]["Stop"][0]["hooks"][0]["headers"]["X-Acd-Token"].clone()
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

#[test]
fn rotation_commits_the_store_then_rewrites_the_headers() {
    let (home, dir) = home_with_acd();
    assert!(install(&dir, home.path(), "claude", &[]).status.success());
    let old = hook_token_of(&dir);
    let out = rotate(&dir, home.path(), &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let store = store_of(&dir);
    let new = hook_token_of(&dir);
    assert_ne!(old, new);
    assert_eq!(store["previous"]["token"], old.as_str());
    assert_eq!(store["headers"]["state"], "done");
    assert_eq!(settings_token(home.path()), new.as_str());
    for output in [&out.stdout, &out.stderr] {
        let output = String::from_utf8_lossy(output);
        assert!(
            !output.contains(&new) && !output.contains(&old),
            "hook tokens are never printed"
        );
    }
}

#[test]
fn rotation_refuses_a_journal_conflict_without_changing_the_store() {
    let (home, dir) = home_with_acd();
    assert!(install(&dir, home.path(), "claude", &[]).status.success());
    let before = store_of(&dir);
    let destination = home.path().join(".claude/settings.json");
    let journal = serde_json::json!({
        "destination": destination,
        "before": "not-the-current-digest",
        "after": "also-not-the-current-digest",
        "record_after": null,
        "target": "claude",
        "conflict": null
    });
    let journal_path = dir.join("hooks-journal.json");
    std::fs::write(&journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();
    set_mode(&journal_path, 0o600);
    let moved = home.path().join("settings-moved.json");
    std::fs::rename(&destination, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &destination).unwrap();
    let out = rotate(&dir, home.path(), &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("conflict"));
    assert_eq!(store_of(&dir), before);
}

#[cfg(debug_assertions)]
#[test]
fn a_record_save_failure_stops_rewrite_and_install_recovers_both_destinations() {
    let (home, dir) = home_with_acd();
    assert!(install(&dir, home.path(), "claude", &[]).status.success());
    let second_home = home.path().join("other-home");
    std::fs::create_dir(&second_home).unwrap();
    let second_settings = second_home.join(".claude/settings.json");
    let out = acd_home(&dir, home.path())
        .env("CLAUDE_CONFIG_DIR", second_home.join(".claude"))
        .args(["hooks", "install", "claude", "--allow-unknown-git"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_FAIL_AT", "record-write")
        .args(["token", "rotate", "hook", "--allow-unknown-git"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("record-write"));
    assert!(dir.join("hooks-journal.json").exists());
    assert_eq!(store_of(&dir)["generation"], 2);
    let token_in = |p: &std::path::Path| -> serde_json::Value {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
        v["hooks"]["Stop"][0]["hooks"][0]["headers"]["X-Acd-Token"].clone()
    };
    let new_token = hook_token_of(&dir);
    assert_eq!(
        token_in(&home.path().join(".claude/settings.json")),
        new_token.as_str(),
        "the first destination already has the new token"
    );
    assert_ne!(
        token_in(&second_settings),
        new_token.as_str(),
        "the second destination still has the old token"
    );
    assert!(
        install(&dir, home.path(), "claude", &["--allow-unknown-git"])
            .status
            .success()
    );
    assert!(!dir.join("hooks-journal.json").exists());
    let current = hook_token_of(&dir);
    assert_eq!(settings_token(home.path()), current.as_str());
    let other_value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(second_settings).unwrap()).unwrap();
    assert_eq!(
        other_value["hooks"]["Stop"][0]["hooks"][0]["headers"]["X-Acd-Token"],
        current.as_str()
    );
    assert_eq!(store_of(&dir)["headers"]["state"], "done");
    assert_eq!(store_of(&dir)["headers"]["failed"], serde_json::json!([]));
}

#[cfg(debug_assertions)]
#[test]
fn a_crash_after_the_store_commit_is_repaired_by_install() {
    for stage in ["after-store", "before-headers-state"] {
        let (home, dir) = home_with_acd();
        assert!(install(&dir, home.path(), "claude", &[]).status.success());
        let out = acd_home(&dir, home.path())
            .env("ACD_TEST_FAIL_AT", stage)
            .args(["token", "rotate", "hook"])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{stage}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(&format!("injected failure at {stage}")),
            "{stage} was reached"
        );
        let store = store_of(&dir);
        assert_eq!(store["generation"], 2, "{stage}: rotation committed");
        assert!(store["previous"].is_object());
        assert_ne!(store["headers"]["state"], "done");
        assert!(!rotate(&dir, home.path(), &["--force"]).status.success());
        assert_eq!(store_of(&dir)["generation"], 2);
        assert!(install(&dir, home.path(), "claude", &[]).status.success());
        assert_eq!(store_of(&dir)["headers"]["state"], "done");
        assert_eq!(settings_token(home.path()), hook_token_of(&dir).as_str());
    }
}

#[test]
fn a_symlinked_settings_file_is_refused_before_committing() {
    let (home, dir) = home_with_acd();
    assert!(install(&dir, home.path(), "claude", &[]).status.success());
    let before = store_of(&dir);
    let settings = home.path().join(".claude/settings.json");
    let moved = home.path().join("moved.json");
    std::fs::rename(&settings, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &settings).unwrap();
    let out = rotate(&dir, home.path(), &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("symlink"));
    assert_eq!(store_of(&dir), before);
}

#[test]
fn a_symlinked_settings_directory_is_refused_before_committing() {
    let (home, dir) = home_with_acd();
    assert!(install(&dir, home.path(), "claude", &[]).status.success());
    let before = store_of(&dir);
    let claude = home.path().join(".claude");
    let moved = home.path().join("moved-claude");
    std::fs::rename(&claude, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &claude).unwrap();
    let out = rotate(&dir, home.path(), &[]);
    assert!(!out.status.success());
    assert_eq!(store_of(&dir), before, "the precheck left the store alone");
}

#[test]
fn install_restores_0600_on_an_unchanged_but_loosened_settings_file() {
    let (home, dir) = home_with_acd();
    assert!(install(&dir, home.path(), "claude", &[]).status.success());
    let settings = home.path().join(".claude/settings.json");
    let text = std::fs::read(&settings).unwrap();
    for loose in [0o644, 0o4600] {
        set_mode(&settings, loose);
        let out = install(&dir, home.path(), "claude", &[]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(mode(&settings), 0o600, "restored from {loose:o}");
        assert_eq!(
            String::from_utf8(std::fs::read(&settings).unwrap()).unwrap(),
            String::from_utf8(text.clone()).unwrap()
        );
    }
}

#[cfg(debug_assertions)]
#[test]
fn a_failed_header_write_fails_rotation_until_install_succeeds() {
    let (home, dir) = home_with_acd();
    assert!(
        install(&dir, home.path(), "claude", &["--allow-unknown-git"])
            .status
            .success()
    );
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_FAIL_AT", "settings-write")
        .args(["token", "rotate", "hook", "--allow-unknown-git"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("injected failure at settings-write"), "{err}");
    assert!(err.contains("settings.json"), "names the file");
    let store = store_of(&dir);
    assert_eq!(
        (
            store["generation"].as_u64(),
            store["headers"]["state"].as_str()
        ),
        (Some(2), Some("failed"))
    );
    assert!(!dir.join("hooks-journal.json").exists());
    assert!(
        install(&dir, home.path(), "claude", &["--allow-unknown-git"])
            .status
            .success()
    );
    assert_eq!(store_of(&dir)["headers"]["state"], "done");
    assert_eq!(settings_token(home.path()), hook_token_of(&dir).as_str());
}

#[cfg(debug_assertions)]
#[test]
fn header_completion_is_recorded_after_the_last_write() {
    let (home, dir) = home_with_acd();
    assert!(
        install(&dir, home.path(), "claude", &["--allow-unknown-git"])
            .status
            .success()
    );
    let gate = home.path().join("gate");
    let mut child = acd_home(&dir, home.path())
        .env(
            "ACD_TEST_PAUSE_AT",
            format!("before-headers-state={}", gate.display()),
        )
        .args(["token", "rotate", "hook", "--allow-unknown-git"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let reached = home.path().join("gate.reached");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !reached.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "rotation never reached the gate"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let session_started = chrono::Utc::now().timestamp_millis();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&gate, b"").unwrap();
    assert!(child.wait().unwrap().success());
    let updated = store_of(&dir)["headers"]["updated_at_ms"].as_i64().unwrap();
    assert!(
        updated > session_started,
        "updated_at_ms {updated} must follow the gap ({session_started})"
    );
}

#[test]
fn rotation_refuses_a_tracked_file_before_committing() {
    let (home, dir) = home_with_acd();
    assert!(install(&dir, home.path(), "claude", &[]).status.success());
    let before = store_of(&dir);
    let claude = home.path().join(".claude");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(&claude)
            .args(args)
            .env("HOME", home.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    git(&["add", "settings.json"]);
    let out = rotate(&dir, home.path(), &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("tracked by Git"));
    assert_eq!(store_of(&dir), before);
    assert!(
        rotate(&dir, home.path(), &["--allow-tracked"])
            .status
            .success()
    );
}

#[test]
fn rotation_refuses_a_suspicious_entry_before_committing() {
    let (home, dir) = home_with_acd();
    assert!(install(&dir, home.path(), "claude", &[]).status.success());
    let before = store_of(&dir);
    let path = home.path().join(".claude/settings.json");
    let mut value = json_of(&path);
    value["hooks"]["Stop"].as_array_mut().unwrap().push(serde_json::json!({"hooks": [{"type": "http", "url": "http://10.0.0.1:47801/hook/claude/Stop?marker=acd-managed:v1", "headers": {}}]}));
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
    let out = rotate(&dir, home.path(), &[]);
    assert!(!out.status.success());
    assert_eq!(store_of(&dir), before);
    assert!(
        json_of(&path)["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["hooks"][0]["headers"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_second_rotation_needs_force_while_the_previous_token_is_retained() {
    let (home, dir) = home_with_acd();
    assert!(install(&dir, home.path(), "claude", &[]).status.success());
    assert!(rotate(&dir, home.path(), &[]).status.success());
    let gen2 = hook_token_of(&dir);
    let out = rotate(&dir, home.path(), &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--force"));
    assert_eq!(store_of(&dir)["generation"], 2);
    let out = rotate(&dir, home.path(), &["--force"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no longer accepted"));
    assert_eq!(store_of(&dir)["previous"]["token"], gen2.as_str());
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
    // Amendment A1: the residual risk of loopback http hooks.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Trust note") && stdout.contains("127.0.0.1:47999"),
        "{stdout}"
    );
    assert!(stdout.contains("trusted hosts"), "{stdout}");
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
fn a_journal_with_an_unparseable_rewrite_is_kept_and_blocks_new_edits() {
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
    std::fs::write(&p, "claude rewrote me").unwrap();
    // The first refusal marks the journal; later ones leave it byte-identical.
    assert!(
        !install(&dir, home.path(), "claude", &["--allow-unknown-git"])
            .status
            .success()
    );
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
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "claude rewrote me");
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

fn rewrite_like_claude(path: &std::path::Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let mut v = json_of(path);
    edit(&mut v);
    let tmp = path.with_extension("tmp-claude");
    std::fs::write(&tmp, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::rename(&tmp, path).unwrap(); // an atomic rewrite, as Claude does
}

fn hooks_status(dir: &std::path::Path, home: &std::path::Path) -> std::process::Output {
    acd_home(dir, home)
        .args(["hooks", "status"])
        .output()
        .unwrap()
}

#[cfg(debug_assertions)]
#[test]
fn a_rewrite_after_a_record_failure_recovers_ownership_from_the_entries() {
    let (home, dir) = home_with_acd();
    let path = home.path().join(".claude/settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, FOREIGN_CLAUDE).unwrap();
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_FAIL_AT", "record-write")
        .args(["hooks", "install", "claude", "--allow-unknown-git"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("injected failure at record-write"));
    rewrite_like_claude(&path, |v| v["model"] = "opus".into()); // neither before nor after now
    let out = hooks_status(&dir, home.path());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !dir.join("hooks-journal.json").exists(),
        "J2: recovered, not discarded"
    );
    let record = json_of(&dir.join("hooks-installed.json"));
    assert_eq!(
        record["files"].as_object().unwrap().len(),
        1,
        "the destination is recorded"
    );
    assert!(uninstall(&dir, home.path(), "claude").status.success());
    let mut want: serde_json::Value = serde_json::from_str(FOREIGN_CLAUDE).unwrap();
    want["model"] = "opus".into();
    assert_eq!(
        json_of(&path),
        want,
        "uninstall removed exactly acd's entries"
    );
}

#[cfg(debug_assertions)]
#[test]
fn an_unreadable_settings_file_is_a_reported_conflict() {
    let (home, dir) = home_with_acd();
    let path = home.path().join(".claude/settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, FOREIGN_CLAUDE).unwrap();
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_FAIL_AT", "record-write")
        .args(["hooks", "install", "claude", "--allow-unknown-git"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("injected failure at record-write"));
    let moved = home.path().join("elsewhere.json");
    std::fs::rename(&path, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &path).unwrap();
    let st = hooks_status(&dir, home.path());
    let text = String::from_utf8_lossy(&st.stdout);
    assert!(
        text.contains("conflict") && text.contains("symlink"),
        "{text}"
    );
    assert!(dir.join("hooks-journal.json").exists(), "J3: kept");
    let out = install(&dir, home.path(), "claude", &["--allow-unknown-git"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("settings.json"));
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&moved, &path).unwrap();
    assert!(
        install(&dir, home.path(), "claude", &["--allow-unknown-git"])
            .status
            .success()
    );
    assert!(!dir.join("hooks-journal.json").exists());
}

/// Spawns `acd run` on free ports, capturing stderr for tests where the daemon
/// is expected to exit by itself.
#[cfg(debug_assertions)]
fn spawn_run(dir: &std::path::Path, env: &[(&str, &str)]) -> Daemon {
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "port = {}\nhook_port = {}\nlisten = [\"loopback\"]\n",
            free_port(),
            free_port()
        ),
    )
    .unwrap();
    let mut cmd = acd(dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    Daemon(
        cmd.arg("run")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    )
}

fn wait_for_exit(daemon: &mut Daemon, what: &str) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(s) = daemon.0.try_wait().unwrap() {
            return s;
        }
        assert!(std::time::Instant::now() < deadline, "{what}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(debug_assertions)] // ACD_TEST_STOP_SERVER is debug-only
#[test]
fn a_dead_server_task_exits_1_so_the_supervisor_restarts_acd() {
    let (_home, dir) = home_with_acd();
    for (server, name) in [
        ("local", "local socket"),
        ("hook", "hook endpoint"),
        ("hook-sock", "hook socket"),
    ] {
        let mut daemon = spawn_run(&dir, &[("ACD_TEST_STOP_SERVER", server)]);
        let sock = dir.join("acd.sock");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !sock.exists() {
            assert!(
                daemon.0.try_wait().unwrap().is_none(),
                "{server}: acd exited before binding its local socket"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "{server}: acd did not bind its local socket"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let status = wait_for_exit(
            &mut daemon,
            &format!("acd kept running without its {name} server"),
        );
        assert_eq!(status.code(), Some(1), "{server}");
        use std::io::Read as _;
        let mut stderr = String::new();
        daemon
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(
            stderr.contains("a server task stopped; exiting") && stderr.contains(name),
            "{server}: {stderr}"
        );
        assert!(
            !sock.exists() && !dir.join("hook.sock").exists(),
            "{server}"
        );
    }
}

#[test]
fn sigterm_exits_cleanly_and_removes_the_socket() {
    let (_home, dir) = home_with_acd();
    let started = start_daemon(&dir, None, "", &[]);
    let mut daemon = started.daemon;
    let pid = daemon.0.id().to_string();
    assert!(
        std::process::Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    let status = wait_for_exit(&mut daemon, "acd did not exit on SIGTERM");
    assert_eq!(
        status.code(),
        Some(0),
        "launchd and systemd must not restart a clean stop"
    );
    assert!(!dir.join("acd.sock").exists());
}

#[cfg(debug_assertions)] // ACD_TEST_PANIC_AFTER_MS is debug-only
#[test]
fn a_panic_exits_with_70_so_the_supervisor_restarts_acd() {
    let (_home, dir) = home_with_acd();
    let mut daemon = spawn_run(&dir, &[("ACD_TEST_PANIC_AFTER_MS", "200")]);
    let status = wait_for_exit(&mut daemon, "acd did not exit after a panic");
    assert_eq!(status.code(), Some(70));
}

// ---- Task 30: daemon wiring (hook listeners, A1 socket rules) ----

fn claude_fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/hooks/claude/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn status_text(dir: &std::path::Path) -> String {
    String::from_utf8_lossy(&acd(dir).arg("status").output().unwrap().stdout).into_owned()
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn inode(p: &std::path::Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(p).unwrap().ino()
}

fn write_ports(dir: &std::path::Path) {
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "port = {}\nhook_port = {}\nlisten = [\"loopback\"]\n",
            free_port(),
            free_port()
        ),
    )
    .unwrap();
}

/// Runs `acd run` on fresh ports and expects it to exit by itself within
/// 10 s. Retries when a picked port was taken meanwhile.
fn run_to_exit(dir: &std::path::Path) -> (std::process::ExitStatus, String) {
    for _ in 0..5 {
        write_ports(dir);
        let mut daemon = Daemon(
            acd(dir)
                .arg("run")
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(s) = daemon.0.try_wait().unwrap() {
                break s;
            }
            assert!(Instant::now() < deadline, "acd run did not exit");
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut err = String::new();
        daemon
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut err)
            .unwrap();
        if !err.contains("Address already in use") {
            return (status, err);
        }
    }
    panic!("no free ports after 5 attempts");
}

#[test]
fn the_daemon_serves_hooks_on_loopback_and_shows_the_session() {
    let (_home, dir) = home_with_acd();
    let started = start_daemon(&dir, None, "", &[]);
    let token = store_token(&dir);
    let body = claude_fixture("payload-UserPromptSubmit.json");
    let path = "/hook/claude/UserPromptSubmit";
    assert_eq!(
        post_hook(started.hook_port, path, "wrong", &[], &body).0,
        401
    );
    assert_eq!(
        post_hook(
            started.hook_port,
            path,
            &token,
            &[("X-Claude-Entrypoint", "cli")],
            &body
        ),
        (200, String::new())
    );
    let mut out = String::new();
    wait_until("the hooks-only session appears", || {
        out = status_text(&dir);
        out.contains("Working Claude")
    });
    assert!(out.contains("hooks=Ok"), "{out}");
}

#[test]
fn the_daemon_serves_the_same_endpoint_on_an_owner_only_hook_sock() {
    let (_home, dir) = home_with_acd();
    let _started = start_daemon(&dir, None, "", &[]);
    let sock = dir.join("hook.sock");
    assert_eq!(mode(&sock), 0o600);
    let token = store_token(&dir);
    let body = claude_fixture("payload-UserPromptSubmit.json");
    let path = "/hook/claude/UserPromptSubmit";
    assert_eq!(post_hook_unix(&sock, path, "wrong", &[], &body).0, 401);
    assert_eq!(
        post_hook_unix(
            &sock,
            path,
            &token,
            &[("X-Claude-Entrypoint", "cli")],
            &body
        ),
        (200, String::new())
    );
    wait_until("the hooks-only session appears", || {
        status_text(&dir).contains("Working Claude")
    });
}

#[test]
fn a_taken_hook_port_is_fatal_before_any_socket_is_served() {
    let (_home, dir) = home_with_acd();
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let hook_port = blocker.local_addr().unwrap().port();
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "port = {}\nhook_port = {hook_port}\nlisten = [\"loopback\"]\n",
            free_port()
        ),
    )
    .unwrap();
    let mut daemon = Daemon(
        acd(&dir)
            .arg("run")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(s) = daemon.0.try_wait().unwrap() {
            break s;
        }
        assert!(
            Instant::now() < deadline,
            "acd kept running without its hook endpoint"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut err = String::new();
    daemon
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert!(!status.success());
    assert!(err.contains("hook endpoint"), "{err}");
    assert!(!dir.join("acd.sock").exists());
    assert!(!dir.join("hook.sock").exists());
}

#[test]
fn the_daemon_and_init_migrate_a_legacy_dir_concurrently_without_hanging() {
    // Sol N7: both take hooks.lock once; neither reacquires it.
    let (home, dir) = home_with_acd();
    let token = store_token(&dir);
    std::fs::remove_file(dir.join("hook-tokens.json")).unwrap();
    std::fs::write(dir.join("hook.token"), format!("{token}\n")).unwrap();
    set_mode(&dir.join("hook.token"), 0o600);
    let inits: Vec<_> = (0..3)
        .map(|_| {
            acd_home(&dir, home.path())
                .arg("init")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    let started = start_daemon(&dir, None, "", &[]); // readiness bounds the daemon side (10 s)
    let deadline = Instant::now() + Duration::from_secs(10);
    for mut c in inits {
        loop {
            if let Some(st) = c.try_wait().unwrap() {
                assert!(st.success());
                break;
            }
            assert!(Instant::now() < deadline, "acd init hung");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    assert_eq!(
        store_token(&dir),
        token,
        "one migration; everyone agrees on the token"
    );
    assert!(!dir.join("hook.token").exists());
    assert_eq!(
        post_hook(started.hook_port, "/hook/claude/Stop", &token, &[], "{}").0,
        200
    );
}

#[test]
fn concurrent_starts_leave_exactly_one_daemon_with_its_sockets() {
    let (_home, dir) = home_with_acd();
    for _attempt in 0..5 {
        write_ports(&dir);
        let mut daemons: Vec<(Daemon, std::path::PathBuf)> = (0..3)
            .map(|i| {
                let log = dir.join(format!("concurrent-{i}.log"));
                let child = acd(&dir)
                    .arg("run")
                    .stdout(Stdio::null())
                    .stderr(std::fs::File::create(&log).unwrap())
                    .spawn()
                    .unwrap();
                (Daemon(child), log)
            })
            .collect();
        // Losers exit at once; the winner keeps running.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut exited = Vec::new();
        while exited.len() < 2 && Instant::now() < deadline {
            exited = (0..daemons.len())
                .filter(|&i| daemons[i].0.0.try_wait().unwrap().is_some())
                .collect();
            std::thread::sleep(Duration::from_millis(20));
        }
        // Let the survivor either become ready or fail its binds.
        while !ready(&dir)
            && daemons
                .iter_mut()
                .any(|(d, _)| d.0.try_wait().unwrap().is_none())
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        let logs: Vec<String> = daemons
            .iter()
            .map(|(_, log)| std::fs::read_to_string(log).unwrap_or_default())
            .collect();
        if logs.iter().any(|l| l.contains("Address already in use")) {
            // A picked port was taken from outside: whoever got the lock
            // failed its binds and a sibling may have won after it. Stop
            // every survivor and start the attempt over.
            drop(daemons);
            continue;
        }
        assert_eq!(exited.len(), 2, "{logs:#?}");
        for &i in &exited {
            assert!(
                !daemons[i].0.0.try_wait().unwrap().unwrap().success(),
                "{}",
                logs[i]
            );
            assert!(logs[i].contains("another acd is running"), "{}", logs[i]);
        }
        wait_until("the winner is ready", || ready(&dir));
        let (acd_sock, hook_sock) = (inode(&dir.join("acd.sock")), inode(&dir.join("hook.sock")));
        let winner = (0..3).find(|i| !exited.contains(i)).unwrap();
        assert!(daemons[winner].0.0.try_wait().unwrap().is_none());
        // A later start is refused too and leaves the winner's sockets alone.
        let (status, err) = run_to_exit(&dir);
        assert!(!status.success());
        assert!(err.contains("another acd is running"), "{err}");
        assert_eq!(inode(&dir.join("acd.sock")), acd_sock);
        assert_eq!(inode(&dir.join("hook.sock")), hook_sock);
        assert!(ready(&dir));
        daemons.clear();
        return;
    }
    panic!("no free ports after 5 attempts");
}

#[test]
fn a_stale_hook_sock_is_replaced() {
    let (_home, dir) = home_with_acd();
    let sock = dir.join("hook.sock");
    drop(UnixListener::bind(&sock).unwrap()); // left by a crashed daemon
    let stale = inode(&sock);
    let _started = start_daemon(&dir, None, "", &[]);
    assert_ne!(inode(&sock), stale);
    assert_eq!(mode(&sock), 0o600);
    let body = claude_fixture("payload-UserPromptSubmit.json");
    assert_eq!(
        post_hook_unix(&sock, "/hook/claude/Stop", &store_token(&dir), &[], &body).0,
        200
    );
}

#[test]
fn a_live_socket_is_never_replaced() {
    for name in ["hook.sock", "acd.sock"] {
        let (_home, dir) = home_with_acd();
        let sock = dir.join(name);
        let _live = UnixListener::bind(&sock).unwrap();
        let before = inode(&sock);
        let (status, err) = run_to_exit(&dir);
        assert!(!status.success(), "{name}");
        assert!(err.contains("is in use"), "{name}: {err}");
        assert_eq!(inode(&sock), before, "{name}");
        // Nothing acd bound before the refusal is left behind.
        for other in ["hook.sock", "acd.sock"] {
            if other != name {
                assert!(!dir.join(other).exists(), "{name}: {other} left behind");
            }
        }
    }
}

#[test]
fn a_path_that_is_not_a_socket_is_never_replaced() {
    let (_home, dir) = home_with_acd();
    let target = dir.join("elsewhere.sock");
    drop(UnixListener::bind(&target).unwrap());
    for (name, make) in [
        (
            "hook.sock",
            &(|p: &std::path::Path| std::fs::write(p, b"x").unwrap()) as &dyn Fn(&std::path::Path),
        ),
        ("hook.sock", &|p: &std::path::Path| {
            std::fs::create_dir(p).unwrap()
        }),
        ("acd.sock", &|p: &std::path::Path| {
            std::os::unix::fs::symlink(dir.join("elsewhere.sock"), p).unwrap()
        }),
    ] {
        let path = dir.join(name);
        make(&path);
        let (status, err) = run_to_exit(&dir);
        assert!(!status.success(), "{name}");
        assert!(err.contains("not a socket"), "{name}: {err}");
        assert!(std::fs::symlink_metadata(&path).is_ok(), "{name} removed");
        if path.is_dir() && !path.is_symlink() {
            std::fs::remove_dir(&path).unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }
    }
    assert!(std::fs::symlink_metadata(&target).is_ok());
}

#[test]
fn sigterm_answers_a_held_permission_request_empty_before_exiting() {
    let (_home, dir) = home_with_acd();
    let started = start_daemon(&dir, None, "", &[]);
    let token = store_token(&dir);
    let port = started.hook_port;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let body = claude_fixture("payload-PermissionRequest-Bash.json");
        write!(
            s,
            "POST /hook/claude/PermissionRequest HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nX-Acd-Token: {token}\r\nX-Claude-Entrypoint: cli\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        let _ = tx.send(String::from_utf8_lossy(&out).into_owned());
    });
    wait_until("the request is held and shown", || {
        status_text(&dir).contains("NeedsYou Claude")
    });
    assert!(
        rx.try_recv().is_err(),
        "the request was answered before the stop"
    );
    let mut daemon = started.daemon;
    let pid = daemon.0.id().to_string();
    assert!(
        std::process::Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    let reply = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the held request ended");
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert_eq!(reply.split_once("\r\n\r\n").map(|(_, b)| b), Some(""));
    assert_eq!(
        wait_for_exit(&mut daemon, "acd did not exit on SIGTERM").code(),
        Some(0)
    );
    assert!(!dir.join("hook.sock").exists());
}

#[cfg(debug_assertions)] // ACD_TEST_BLOCKING_MS is debug-only
#[test]
fn a_blocking_task_does_not_delay_a_clean_exit() {
    let (_home, dir) = home_with_acd();
    let started = start_daemon(&dir, None, "", &[("ACD_TEST_BLOCKING_MS", "60000")]);
    let mut daemon = started.daemon;
    let pid = daemon.0.id().to_string();
    assert!(
        std::process::Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        wait_for_exit(&mut daemon, "a blocking task delayed the exit").code(),
        Some(0)
    );
}

#[test]
fn init_records_a_random_hook_port_and_keeps_an_existing_one() {
    let read_port = |dir: &std::path::Path| -> u16 {
        let text = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        let v: toml::Value = toml::from_str(&text).unwrap();
        v["hook_port"].as_integer().unwrap() as u16
    };
    let (home, dir) = home_with_acd();
    let first = read_port(&dir);
    let (_home2, dir2) = home_with_acd();
    assert!(
        first != 47801 || read_port(&dir2) != 47801,
        "init must not write the well-known default"
    );
    assert!(
        acd_home(&dir, home.path())
            .arg("init")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(read_port(&dir), first, "an existing config keeps its port");
}

#[test]
fn status_and_hooks_status_warn_when_the_hook_port_is_held_while_acd_is_down() {
    let (home, dir) = home_with_acd();
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = squatter.local_addr().unwrap().port();
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "port = {}\nhook_port = {port}\nlisten = [\"loopback\"]\n",
            free_port()
        ),
    )
    .unwrap();
    let warning = "is held by another process while acd is not running";
    let out = acd(&dir).arg("status").output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(warning) && err.contains(&port.to_string()),
        "{err}"
    );
    let out = hooks_status(&dir, home.path());
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(warning), "{err}");
    drop(squatter);
    let out = hooks_status(&dir, home.path());
    assert!(!String::from_utf8_lossy(&out.stderr).contains(warning));
    // A running acd holds the port itself.
    let _started = start_daemon(&dir, None, "", &[]);
    let out = hooks_status(&dir, home.path());
    assert!(!String::from_utf8_lossy(&out.stderr).contains(warning));
}

/// A Unix listener on `path` with a backlog of 0 that never accepts, filled
/// with pending connections until the kernel refuses more. Returns every
/// descriptor; they close when the returned files drop.
fn saturated_listener(path: &std::path::Path) -> Vec<std::fs::File> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;
    fn addr(path: &std::path::Path) -> libc::sockaddr_un {
        // SAFETY: an all-zero sockaddr_un is valid.
        let mut a: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        a.sun_family = libc::AF_UNIX as _;
        let bytes = path.as_os_str().as_bytes();
        assert!(bytes.len() < a.sun_path.len());
        for (d, s) in a.sun_path.iter_mut().zip(bytes) {
            *d = *s as libc::c_char;
        }
        a
    }
    let a = addr(path);
    let len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    // SAFETY: plain socket calls on descriptors this function owns.
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        assert!(fd >= 0);
        assert_eq!(
            libc::bind(fd, &a as *const _ as *const libc::sockaddr, len),
            0
        );
        assert_eq!(libc::listen(fd, 0), 0);
        let mut fds = vec![std::fs::File::from_raw_fd(fd)];
        for _ in 0..256 {
            let c = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
            assert!(c >= 0);
            libc::fcntl(c, libc::F_SETFL, libc::O_NONBLOCK);
            let rc = libc::connect(c, &a as *const _ as *const libc::sockaddr, len);
            fds.push(std::fs::File::from_raw_fd(c));
            if rc != 0 {
                break; // the backlog is full
            }
        }
        fds
    }
}

#[test]
fn a_saturated_acd_sock_cannot_hang_status_or_hooks_status() {
    let (home, dir) = home_with_acd();
    write_ports(&dir);
    let _held = saturated_listener(&dir.join("acd.sock"));
    for args in [&["status"][..], &["hooks", "status"][..]] {
        let mut cmd = acd_home(&dir, home.path());
        cmd.args(args);
        let started = Instant::now();
        let out = run_within(cmd, 10);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{args:?} took {:?}",
            started.elapsed()
        );
        // A listening acd.sock counts as a running acd: no squat warning.
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("is held by another process"),
            "{args:?}"
        );
    }
}

fn permission_request(n: usize) -> String {
    let mut v: serde_json::Value =
        serde_json::from_str(&claude_fixture("payload-PermissionRequest-Bash.json")).unwrap();
    v["session_id"] = format!("00000000-0000-4000-8000-{n:012}").into();
    v.to_string()
}

fn hold_request(token: &str, body: &str) -> String {
    format!(
        "POST /hook/claude/PermissionRequest HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nX-Acd-Token: {token}\r\nX-Claude-Entrypoint: cli\r\n\r\n{body}",
        body.len()
    )
}

/// Still waiting for its reply: no byte and no close yet.
fn unanswered(s: &mut dyn Read) -> bool {
    let mut b = [0u8; 1];
    matches!(s.read(&mut b), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
}

#[test]
fn both_hook_listeners_share_one_hold_cap_and_distinct_hold_ids() {
    const MAX_HOLDS: usize = 64;
    let (_home, dir) = home_with_acd();
    let started = start_daemon(&dir, None, "", &[]);
    let token = store_token(&dir);
    let sock = dir.join("hook.sock");
    // Half the holds on each listener, one session each.
    let mut held: Vec<Box<dyn Read>> = Vec::new();
    for n in 0..MAX_HOLDS {
        let req = hold_request(&token, &permission_request(n));
        if n % 2 == 0 {
            let mut s = std::net::TcpStream::connect(("127.0.0.1", started.hook_port)).unwrap();
            s.write_all(req.as_bytes()).unwrap();
            s.set_nonblocking(true).unwrap();
            held.push(Box::new(s));
        } else {
            let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
            s.write_all(req.as_bytes()).unwrap();
            s.set_nonblocking(true).unwrap();
            held.push(Box::new(s));
        }
    }
    // Every hold reached the reconciler as its own request: hold ids
    // that collided would replace (and release) each other.
    wait_until("every request is held and shown", || {
        status_text(&dir).matches("NeedsYou Claude").count() == MAX_HOLDS
    });
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        status_text(&dir).matches("NeedsYou Claude").count(),
        MAX_HOLDS
    );
    for (n, s) in held.iter_mut().enumerate() {
        assert!(unanswered(s.as_mut()), "hold {n} was released");
    }
    // The cap is shared: one more on either listener fails open at once.
    let extra = permission_request(MAX_HOLDS);
    let cli = [("X-Claude-Entrypoint", "cli")];
    let path = "/hook/claude/PermissionRequest";
    let at = Instant::now();
    assert_eq!(
        post_hook(started.hook_port, path, &token, &cli, &extra),
        (200, String::new())
    );
    let extra = permission_request(MAX_HOLDS + 1);
    assert_eq!(
        post_hook_unix(&sock, path, &token, &cli, &extra),
        (200, String::new())
    );
    assert!(at.elapsed() < Duration::from_secs(3), "{:?}", at.elapsed());
    for (n, s) in held.iter_mut().enumerate() {
        assert!(unanswered(s.as_mut()), "hold {n} was released");
    }
    // Hold ids are distinct across listeners: closing the first TCP hold
    // releases that hold only. With a shared id, the close would end the
    // hook.sock hold that reused it, which then gets its empty reply. The
    // closed request itself stays open (no capture), so give the
    // reconciler a moment instead of waiting on a status change.
    drop(held.remove(0));
    std::thread::sleep(Duration::from_secs(1));
    for (n, s) in held.iter_mut().enumerate() {
        assert!(unanswered(s.as_mut()), "hold {} was released", n + 1);
    }
}
