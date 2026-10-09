mod common;

use common::{acd_home, init_dir};

#[cfg(debug_assertions)]
#[test]
fn service_install_writes_the_agent_and_loads_it_through_the_runner() {
    let home = tempfile::tempdir().unwrap();
    let dir = init_dir(home.path());
    let calls = home.path().join("runner.log");
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_SERVICE_RUNNER", &calls)
        .args(["service", "install"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = std::fs::read_to_string(&calls).unwrap();
    if cfg!(target_os = "macos") {
        let plist = home
            .path()
            .join("Library/LaunchAgents/dev.agent-caller.acd.plist");
        assert!(
            std::fs::read_to_string(&plist)
                .unwrap()
                .contains("<string>run</string>")
        );
        assert!(calls.contains("launchctl bootstrap gui/"), "{calls}");
    } else {
        assert!(
            home.path()
                .join(".config/systemd/user/acd.service")
                .exists()
        );
        assert!(
            calls.contains("systemctl --user restart acd.service"),
            "{calls}"
        );
    }
}

#[cfg(debug_assertions)]
#[test]
fn service_install_print_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let dir = init_dir(home.path());
    let calls = home.path().join("runner.log");
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_SERVICE_RUNNER", &calls)
        .args(["service", "install", "--print"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("run"));
    assert!(!home.path().join("Library").exists());
    assert!(!home.path().join(".config/systemd").exists());
    assert!(!calls.exists());
}

#[cfg(debug_assertions)]
#[test]
fn service_install_reports_a_failed_load() {
    let home = tempfile::tempdir().unwrap();
    let dir = init_dir(home.path());
    let calls = home.path().join("runner.log");
    let word = if cfg!(target_os = "macos") {
        "bootstrap"
    } else {
        "restart"
    };
    let out = acd_home(&dir, home.path())
        .env("ACD_TEST_SERVICE_RUNNER", &calls)
        .env("ACD_TEST_SERVICE_FAIL", word)
        .args(["service", "install"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains(word));
}
