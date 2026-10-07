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
