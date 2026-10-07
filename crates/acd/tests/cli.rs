use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn acd(dir: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_acd"));
    c.env("ACD_CONFIG_DIR", dir).env_remove("RUST_LOG");
    c
}

fn mode(p: std::path::PathBuf) -> u32 { std::fs::metadata(p).unwrap().permissions().mode() & 0o777 }

#[test]
fn init_creates_private_files_and_refuses_to_run_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    let out = acd(&dir).arg("init").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(mode(dir.clone()), 0o700);
    for f in ["client.token", "hook.token", "config.toml", "machine-id"] {
        assert_eq!(mode(dir.join(f)), 0o600, "{f}");
    }
    let token = std::fs::read_to_string(dir.join("client.token")).unwrap();
    assert_eq!(token.trim().len(), 43, "32 bytes base64url without padding");
    assert!(String::from_utf8_lossy(&out.stdout).contains(token.trim()));

    let again = acd(&dir).arg("init").output().unwrap();
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("already initialized"));
}

#[test]
fn token_rotate_replaces_client_token() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    assert!(acd(&dir).arg("init").status().unwrap().success());
    let before = std::fs::read_to_string(dir.join("client.token")).unwrap();
    assert!(acd(&dir).args(["token", "rotate", "client"]).status().unwrap().success());
    let after = std::fs::read_to_string(dir.join("client.token")).unwrap();
    assert_ne!(before, after);
    assert_eq!(mode(dir.join("client.token")), 0o600);
}

#[test]
fn loose_token_mode_is_refused_with_fix() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    assert!(acd(&dir).arg("init").status().unwrap().success());
    std::fs::set_permissions(dir.join("client.token"), std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = acd(&dir).arg("run").output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("client.token") && err.contains("chmod 600"), "{err}");
}

#[test]
fn rotate_on_missing_dir_says_run_init() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    let out = acd(&dir).args(["token", "rotate", "client"]).output().unwrap();
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
    let out = acd(&dir).args(["token", "rotate", "client"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("chmod 700"));
    assert_eq!(before, std::fs::read_to_string(dir.join("client.token")).unwrap());
}

#[test]
fn init_repairs_partial_dir_and_force_keeps_machine_id() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    assert!(acd(&dir).arg("init").status().unwrap().success());
    let client = std::fs::read_to_string(dir.join("client.token")).unwrap();
    let id = std::fs::read_to_string(dir.join("machine-id")).unwrap();
    std::fs::remove_file(dir.join("hook.token")).unwrap();
    let out = acd(&dir).arg("init").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("hook.token"));
    assert_eq!(mode(dir.join("hook.token")), 0o600);
    assert_eq!(client, std::fs::read_to_string(dir.join("client.token")).unwrap());

    assert!(acd(&dir).args(["init", "--force"]).status().unwrap().success());
    assert_ne!(client, std::fs::read_to_string(dir.join("client.token")).unwrap());
    assert_eq!(id, std::fs::read_to_string(dir.join("machine-id")).unwrap());
}

/// Runs `acd run` and expects it to exit on its own within 10 s; otherwise
/// kills it and fails, so a regression cannot hang the suite.
fn run_expecting_exit(dir: &std::path::Path) -> (std::process::ExitStatus, String) {
    use std::io::Read;
    let mut child = acd(dir).arg("run").stderr(std::process::Stdio::piped()).spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() { break st; }
        if std::time::Instant::now() > deadline {
            child.kill().ok();
            child.wait().ok();
            panic!("`acd run` did not exit within 10 s");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let mut err = String::new();
    child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    (status, err)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[test]
fn run_serves_loopback_and_status_reports_machine() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("acd");
    assert!(acd(&dir).arg("init").status().unwrap().success());
    let port = free_port();
    let cfg = format!("machine_name = \"e2e-box\"\nport = {port}\nlisten = [\"loopback\"]\n");
    std::fs::write(dir.join("config.toml"), cfg).unwrap();

    let mut daemon = acd(&dir).arg("run").spawn().unwrap();
    let mut ok = None;
    for _ in 0..50 {
        let out = acd(&dir).arg("status").output().unwrap();
        if out.status.success() { ok = Some(String::from_utf8_lossy(&out.stdout).into_owned()); break; }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    daemon.kill().unwrap();
    daemon.wait().ok();
    let out = ok.expect("status never succeeded");
    assert!(out.contains("machine: e2e-box"), "{out}");
    assert!(out.contains("agents: 0"), "{out}");
}

#[test]
fn run_refuses_empty_or_short_client_token() {
    for bad in ["", "short"] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("acd");
        assert!(acd(&dir).arg("init").status().unwrap().success());
        let port = free_port();
        std::fs::write(dir.join("config.toml"), format!("port = {port}\nlisten = [\"loopback\"]\n")).unwrap();
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
    std::fs::write(dir.join("config.toml"), format!("port = {port}\nlisten = [\"loopback\"]\n")).unwrap();
    let (status, err) = run_expecting_exit(&dir);
    assert!(!status.success());
    assert!(err.contains("cannot bind"), "{err}");
}
