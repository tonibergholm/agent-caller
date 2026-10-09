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
