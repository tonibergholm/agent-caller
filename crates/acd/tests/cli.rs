use std::os::unix::fs::PermissionsExt;
mod common;
use common::{acd, free_port};

fn initialized() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let out = acd(dir.path()).arg("init").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

fn pin_of(dir: &std::path::Path) -> String {
    let bundle = std::fs::read_to_string(dir.join("tls.pem")).unwrap();
    acd_core::tls::check_pair(&acd_core::tls::KeyFiles::from_bundle(&bundle).unwrap()).unwrap()
}
fn mode(p: std::path::PathBuf) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn init_creates_one_private_bundle_and_repairs_it() {
    let dir = tempfile::tempdir().unwrap();
    assert!(acd(dir.path()).arg("init").status().unwrap().success());
    assert_eq!(mode(dir.path().join("tls.pem")), 0o600);
    let pin = pin_of(dir.path());
    assert!(acd(dir.path()).arg("init").status().unwrap().success());
    assert_eq!(pin_of(dir.path()), pin, "init keeps a healthy key");
    let a = acd_core::tls::generate("a");
    let b = acd_core::tls::generate("b");
    std::fs::write(
        dir.path().join("tls.pem"),
        format!("{}{}", a.key_pem, b.cert_pem),
    )
    .unwrap();
    assert!(acd(dir.path()).arg("init").status().unwrap().success());
    assert_ne!(pin_of(dir.path()), pin);
}

#[test]
fn client_token_rotation_keeps_the_tls_key() {
    let dir = initialized();
    let pin = pin_of(dir.path());
    assert!(
        acd(dir.path())
            .args(["token", "rotate", "client"])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(pin_of(dir.path()), pin);
}

#[test]
fn tls_rotate_changes_the_pin_and_advises_token_rotation_on_compromise() {
    let dir = initialized();
    let pin = pin_of(dir.path());
    let out = acd(dir.path()).args(["tls", "rotate"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Re-pair") && text.contains("token rotate client"));
    assert_ne!(pin_of(dir.path()), pin);
}

#[test]
fn tls_rotate_replaces_a_corrupt_existing_bundle() {
    let dir = initialized();
    std::fs::write(dir.path().join("tls.pem"), b"corrupt").unwrap();
    let out = acd(dir.path()).args(["tls", "rotate"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.starts_with(b"TLS key replaced."));
    pin_of(dir.path());
}

#[test]
fn concurrent_rotations_leave_one_coherent_bundle() {
    let dir = initialized();
    let kids: Vec<_> = (0..8)
        .map(|_| acd(dir.path()).args(["tls", "rotate"]).spawn().unwrap())
        .collect();
    for mut kid in kids {
        assert!(kid.wait().unwrap().success());
    }
    pin_of(dir.path());
    let leftovers = std::fs::read_dir(dir.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        })
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn a_stray_temporary_file_is_ignored_by_readers() {
    let dir = initialized();
    let pin = pin_of(dir.path());
    std::fs::write(
        dir.path().join("tls.pem.999.deadbeefdeadbeef.tmp"),
        "-----BEGIN PRIVATE KEY-----\ntrunc",
    )
    .unwrap();
    let out = acd(dir.path())
        .args(["pair", "--host", "h"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        acd_protocol::pairing::Pairing::parse(String::from_utf8_lossy(&out.stdout).trim())
            .unwrap()
            .pin,
        pin
    );
}

#[test]
fn a_failed_publication_keeps_the_old_bundle_then_a_good_one_replaces_it() {
    let dir = initialized();
    let before = std::fs::read(dir.path().join("tls.pem")).unwrap();
    let pin = pin_of(dir.path());
    let out = acd(dir.path())
        .args(["tls", "rotate"])
        .env("ACD_TEST_FAILPOINT", "publish_before_rename")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(std::fs::read(dir.path().join("tls.pem")).unwrap(), before);
    assert_eq!(pin_of(dir.path()), pin);
    let paired = acd(dir.path())
        .args(["pair", "--host", "h"])
        .output()
        .unwrap();
    assert_eq!(
        acd_protocol::pairing::Pairing::parse(String::from_utf8_lossy(&paired.stdout).trim())
            .unwrap()
            .pin,
        pin
    );
    assert!(
        acd(dir.path())
            .args(["tls", "rotate"])
            .status()
            .unwrap()
            .success()
    );
    let new_pin = pin_of(dir.path());
    assert_ne!(new_pin, pin);
    let paired = acd(dir.path())
        .args(["pair", "--host", "h"])
        .output()
        .unwrap();
    assert_eq!(
        acd_protocol::pairing::Pairing::parse(String::from_utf8_lossy(&paired.stdout).trim())
            .unwrap()
            .pin,
        new_pin
    );
}

#[test]
fn tls_bundle_must_be_a_private_regular_file() {
    let dir = initialized();
    let real = dir.path().join("real.pem");
    std::fs::rename(dir.path().join("tls.pem"), &real).unwrap();
    std::os::unix::fs::symlink(&real, dir.path().join("tls.pem")).unwrap();
    let out = acd(dir.path())
        .args(["pair", "--host", "h"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("symlink"));
}

#[test]
fn pair_prints_one_parseable_line() {
    let dir = initialized();
    let out = acd(dir.path())
        .args(["pair", "--host", "devbox.example.ts.net"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let p = acd_protocol::pairing::Pairing::parse(String::from_utf8(out.stdout).unwrap().trim())
        .unwrap();
    assert_eq!((p.host.as_str(), p.port), ("devbox.example.ts.net", 47800));
    assert_eq!(p.pin, pin_of(dir.path()));
    assert_eq!(
        p.token,
        std::fs::read_to_string(dir.path().join("client.token"))
            .unwrap()
            .trim()
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("secret"));
}

#[test]
fn init_creates_private_files_and_is_repeatable() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    let out = acd(&dir).arg("init").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(mode(dir.clone()), 0o700);
    for f in ["client.token", "hook-tokens.json", "config.toml", "machine-id"] {
        assert_eq!(mode(dir.join(f)), 0o600, "{f}");
    }
    let token = std::fs::read_to_string(dir.join("client.token")).unwrap();
    assert_eq!(token.trim().len(), 43, "32 bytes base64url without padding");
    assert!(String::from_utf8_lossy(&out.stdout).contains(token.trim()));

    let again = acd(&dir).arg("init").output().unwrap();
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
}

#[test]
fn token_rotate_replaces_client_token() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    assert!(acd(&dir).arg("init").status().unwrap().success());
    let before = std::fs::read_to_string(dir.join("client.token")).unwrap();
    assert!(
        acd(&dir)
            .args(["token", "rotate", "client"])
            .status()
            .unwrap()
            .success()
    );
    let after = std::fs::read_to_string(dir.join("client.token")).unwrap();
    assert_ne!(before, after);
    assert_eq!(mode(dir.join("client.token")), 0o600);
}

#[test]
fn loose_token_mode_is_refused_with_fix() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    assert!(acd(&dir).arg("init").status().unwrap().success());
    std::fs::set_permissions(
        dir.join("client.token"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let out = acd(&dir).arg("run").output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("client.token") && err.contains("chmod 600"),
        "{err}"
    );
}

#[test]
fn rotate_on_missing_dir_says_run_init() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    let out = acd(&dir)
        .args(["token", "rotate", "client"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("acd init"));
    assert!(!dir.exists());
}

#[test]
fn rotate_refuses_loose_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    assert!(acd(&dir).arg("init").status().unwrap().success());
    let before = std::fs::read_to_string(dir.join("client.token")).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let out = acd(&dir)
        .args(["token", "rotate", "client"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("chmod 700"));
    assert_eq!(
        before,
        std::fs::read_to_string(dir.join("client.token")).unwrap()
    );
}

#[test]
fn init_repairs_partial_dir_and_force_keeps_machine_id() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    assert!(acd(&dir).arg("init").status().unwrap().success());
    let client = std::fs::read_to_string(dir.join("client.token")).unwrap();
    let id = std::fs::read_to_string(dir.join("machine-id")).unwrap();
    std::fs::remove_file(dir.join("hook-tokens.json")).unwrap();
    let out = acd(&dir).arg("init").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("hook-tokens.json"));
    assert_eq!(mode(dir.join("hook-tokens.json")), 0o600);
    assert_eq!(
        client,
        std::fs::read_to_string(dir.join("client.token")).unwrap()
    );

    assert!(
        acd(&dir)
            .args(["init", "--force"])
            .status()
            .unwrap()
            .success()
    );
    assert_ne!(
        client,
        std::fs::read_to_string(dir.join("client.token")).unwrap()
    );
    assert_eq!(id, std::fs::read_to_string(dir.join("machine-id")).unwrap());
}

/// Runs `acd run` and expects it to exit on its own within 10 s; otherwise
/// kills it and fails, so a regression cannot hang the suite.
fn run_expecting_exit(dir: &std::path::Path) -> (std::process::ExitStatus, String) {
    use std::io::Read;
    let mut child = acd(dir)
        .arg("run")
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if std::time::Instant::now() > deadline {
            child.kill().ok();
            child.wait().ok();
            panic!("`acd run` did not exit within 10 s");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    (status, err)
}

/// An initialized config dir; `start` writes ports and starts the daemon.
fn init_named() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = common::init_dir(tmp.path());
    (tmp, dir)
}

fn start(dir: &std::path::Path, name: &str, env: &[(&str, &str)]) -> common::Started {
    common::start_daemon(dir, None, &format!("machine_name = \"{name}\"\n"), env)
}

/// Runs `acd status` until it succeeds, for at most 10 s.
fn status_when_ready(dir: &std::path::Path, rust_log: Option<&str>) -> std::process::Output {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let mut cmd = acd(dir);
        if let Some(l) = rust_log {
            cmd.env("RUST_LOG", l);
        }
        let out = cmd.arg("status").output().unwrap();
        if out.status.success() {
            return out;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "status never succeeded: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn run_serves_loopback_and_status_reports_machine() {
    use std::os::unix::fs::FileTypeExt;
    let (_tmp, dir) = init_named();
    // A socket left by a crashed daemon is replaced.
    drop(std::os::unix::net::UnixListener::bind(dir.join("acd.sock")).unwrap());

    let _daemon = start(&dir, "e2e-box", &[]);
    let out = String::from_utf8_lossy(&status_when_ready(&dir, None).stdout).into_owned();
    assert!(out.contains("machine: e2e-box"), "{out}");
    assert!(out.contains("agents: 0"), "{out}");
    let sock = std::fs::symlink_metadata(dir.join("acd.sock")).unwrap();
    assert!(sock.file_type().is_socket());
    assert_eq!(mode(dir.join("acd.sock")), 0o600);
}

/// Every form in which a log line could carry `token`: as text, as hex or
/// decimal bytes of the text, and as hex of the decoded bytes.
fn token_forms(token: &str) -> Vec<String> {
    use base64::Engine;
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .unwrap();
    let decimal = token.as_bytes()[..12]
        .iter()
        .map(|b| b.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    vec![
        token.to_string(),
        hex(token.as_bytes()),
        hex(token.as_bytes()).to_uppercase(),
        decimal,
        hex(&decoded),
    ]
}

#[test]
fn trace_logs_never_contain_the_token() {
    for rust_log in [
        "trace",
        "trace,tungstenite::protocol::frame=trace,tokio_tungstenite=trace,axum::extract::ws=trace",
    ] {
        let (_tmp, dir) = init_named();
        let token = std::fs::read_to_string(dir.join("client.token"))
            .unwrap()
            .trim()
            .to_string();
        let started = start(&dir, "log-box", &[("RUST_LOG", rust_log)]);
        let log_path = started.log.clone();
        let status = status_when_ready(&dir, Some(rust_log));
        drop(started);
        let daemon_log = std::fs::read_to_string(&log_path).unwrap();
        let status_log = String::from_utf8_lossy(&status.stderr).into_owned();
        assert!(
            daemon_log.contains("TRACE"),
            "{rust_log}: expected trace output from the daemon"
        );
        for form in token_forms(&token) {
            assert!(
                !daemon_log.contains(&form),
                "{rust_log}: daemon log contains the token as {form}"
            );
            assert!(
                !status_log.contains(&form),
                "{rust_log}: status log contains the token as {form}"
            );
        }
    }
}

#[test]
fn status_never_sends_the_token_over_tcp() {
    use std::io::Read;
    use std::sync::{Arc, Mutex};
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = squatter.local_addr().unwrap().port();
    squatter.set_nonblocking(true).unwrap();
    let received = Arc::new(Mutex::new(Vec::<u8>::new()));
    let seen = received.clone();
    // Completes the WebSocket handshake, so an old client would go on to send hello.
    let thread = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let Ok((stream, _)) = squatter.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(20));
                continue;
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                .unwrap();
            let mut raw = stream.try_clone().unwrap();
            match tokio_tungstenite::tungstenite::accept(stream) {
                Ok(mut ws) => {
                    while let Ok(m) = ws.read() {
                        seen.lock().unwrap().extend_from_slice(&m.into_data());
                    }
                }
                Err(_) => {
                    let mut rest = Vec::new();
                    let _ = raw.read_to_end(&mut rest);
                    seen.lock().unwrap().extend_from_slice(&rest);
                }
            }
        }
    });

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    assert!(acd(&dir).arg("init").status().unwrap().success());
    std::fs::write(
        dir.join("config.toml"),
        format!("port = {port}\nlisten = [\"loopback\"]\n"),
    )
    .unwrap();
    let token = std::fs::read_to_string(dir.join("client.token"))
        .unwrap()
        .trim()
        .to_string();
    let out = acd(&dir).arg("status").output().unwrap();
    thread.join().unwrap();
    assert!(!out.status.success(), "status succeeded against a squatter");
    let got = String::from_utf8_lossy(&received.lock().unwrap()).into_owned();
    assert!(!got.contains(&token), "the squatter received the token");
}

#[test]
fn run_refuses_empty_or_short_client_token() {
    for bad in ["", "short"] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("acd");
        assert!(acd(&dir).arg("init").status().unwrap().success());
        let port = free_port();
        std::fs::write(
            dir.join("config.toml"),
            format!("port = {port}\nlisten = [\"loopback\"]\n"),
        )
        .unwrap();
        std::fs::write(dir.join("client.token"), bad).unwrap();
        let (status, err) = run_expecting_exit(&dir);
        assert!(!status.success(), "run accepted token {bad:?}");
        assert!(err.contains("client.token is invalid"), "{err}");
        assert!(err.contains("acd token rotate client"), "{err}");
    }
}

#[test]
fn run_fails_when_a_listen_address_cannot_be_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    assert!(acd(&dir).arg("init").status().unwrap().success());
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = blocker.local_addr().unwrap().port();
    std::fs::write(
        dir.join("config.toml"),
        format!("port = {port}\nlisten = [\"loopback\"]\n"),
    )
    .unwrap();
    let (status, err) = run_expecting_exit(&dir);
    assert!(!status.success());
    assert!(err.contains("cannot bind"), "{err}");
}

#[test]
fn second_daemon_on_another_port_leaves_the_live_socket_alone() {
    let (_tmp, dir) = init_named();
    let _daemon = start(&dir, "first", &[]);
    let mut err = String::new();
    for _ in 0..5 {
        // Fresh ports each time, so a retry never reuses ports that just failed.
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "machine_name = \"second\"\nport = {}\nhook_port = {}\nlisten = [\"loopback\"]\n",
                free_port(),
                free_port()
            ),
        )
        .unwrap();
        let (status, e) = run_expecting_exit(&dir);
        assert!(!status.success());
        err = e;
        if !(err.contains("cannot bind 127.") || err.contains("cannot bind [")) {
            break;
        }
    }
    assert!(err.contains("another acd is running"), "{err}");
    let out =
        String::from_utf8_lossy(&acd(&dir).arg("status").output().unwrap().stdout).into_owned();
    assert!(out.contains("machine: first"), "{out}");
}

#[test]
fn init_repairs_loose_modes_of_files_it_keeps() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = common::init_dir(tmp.path());
    let before = std::fs::read_to_string(dir.join("client.token")).unwrap();
    for f in [
        "client.token",
        "hook-tokens.json",
        "machine-id",
        "config.toml",
    ] {
        std::fs::set_permissions(dir.join(f), std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let out = acd(&dir).arg("init").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for f in [
        "client.token",
        "hook-tokens.json",
        "machine-id",
        "config.toml",
    ] {
        assert_eq!(mode(dir.join(f)), 0o600, "{f}");
    }
    assert_eq!(
        before,
        std::fs::read_to_string(dir.join("client.token")).unwrap(),
        "kept, not replaced"
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("repaired"));
}

#[test]
fn init_and_run_refuse_a_symlinked_token() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = common::init_dir(tmp.path());
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::rename(dir.join("hook-tokens.json"), &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join("hook-tokens.json")).unwrap();
    let out = acd(&dir).arg("init").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("symlink"));
    let (status, err) = run_expecting_exit(&dir);
    assert!(!status.success() && err.contains("symlink"), "{err}");
}

#[test]
fn an_empty_machine_id_is_refused_by_run_and_regenerated_by_init() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = common::init_dir(tmp.path());
    std::fs::write(dir.join("machine-id"), "\n").unwrap();
    let (status, err) = run_expecting_exit(&dir);
    assert!(
        !status.success() && err.contains("machine-id is empty"),
        "{err}"
    );
    assert!(acd(&dir).arg("init").status().unwrap().success());
    assert!(
        !std::fs::read_to_string(dir.join("machine-id"))
            .unwrap()
            .trim()
            .is_empty()
    );
}

#[test]
fn init_repairs_a_setuid_secret_and_a_loose_t3_token() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    assert!(acd(&dir).arg("init").status().unwrap().success());
    std::fs::set_permissions(
        dir.join("client.token"),
        std::fs::Permissions::from_mode(0o4600),
    )
    .unwrap();
    std::fs::write(dir.join("t3.token"), "t\n").unwrap();
    std::fs::set_permissions(dir.join("t3.token"), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(acd(&dir).arg("run").output().unwrap().status.code() != Some(0));
    let out = acd(&dir).arg("init").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("client.token") && text.contains("t3.token"),
        "{text}"
    );
    assert_eq!(
        std::fs::metadata(dir.join("client.token"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
    assert_eq!(mode(dir.join("t3.token")), 0o600);
}
