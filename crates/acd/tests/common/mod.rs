//! Shared helpers for tests that run the `acd` binary. Every command is
//! isolated from the developer's environment: HOME and ACD_CONFIG_DIR
//! point into a temp dir, and variables that would reach real herdr,
//! Claude or Codex state are removed.
#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const ISOLATED: [&str; 16] = [
    "HERDR_SOCKET_PATH",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "CLAUDE_CODE_ENTRYPOINT",
    "RUST_LOG",
    "ACD_TEST_MAX_UNAUTH",
    "ACD_TEST_SERVICE_RUNNER",
    "ACD_TEST_PANIC_AFTER_MS",
    "ACD_TEST_STOP_SERVER",
    "ACD_TEST_FAIL_AT",
    "ACD_TEST_HOOKS_FAST",
    "ACD_TEST_GIT",
    "ACD_TEST_TOUCH_SETTINGS",
    "ACD_TEST_SERVICE_FAIL",
    "ACD_TEST_PAUSE_AT",
    "ACD_TEST_FAILPOINT",
];

pub fn acd(dir: &Path) -> Command {
    acd_home(dir, dir)
}

pub fn acd_home(dir: &Path, home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_acd"));
    c.env("ACD_CONFIG_DIR", dir).env("HOME", home);
    for k in ISOLATED {
        c.env_remove(k);
    }
    // Strip every ACD_TEST_* override, so future ones cannot leak in.
    for (k, _) in std::env::vars() {
        if k.starts_with("ACD_TEST_") {
            c.env_remove(k);
        }
    }
    c
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A running `acd run`, killed and reaped when dropped.
pub struct Daemon(pub Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

pub struct Started {
    pub daemon: Daemon,
    pub port: u16,
    pub hook_port: u16,
    pub log: PathBuf,
}

pub fn init_dir(parent: &Path) -> PathBuf {
    let dir = parent.join("acd");
    let out = acd(&dir).arg("init").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

/// Starts `acd run` with fresh free ports. `listen` is the TOML value of the
/// `listen` key (e.g. `["loopback"]`); `None` means `["loopback"]`.
/// `extra_config` is appended verbatim and must not contain `listen`.
pub fn start_daemon(
    dir: &Path,
    listen: Option<&str>,
    extra_config: &str,
    env: &[(&str, &str)],
) -> Started {
    start_daemon_with(dir, listen, extra_config, env, || {
        (free_port(), free_port())
    })
}

pub fn start_daemon_with(
    dir: &Path,
    listen: Option<&str>,
    extra_config: &str,
    env: &[(&str, &str)],
    mut pick: impl FnMut() -> (u16, u16),
) -> Started {
    let mut last_log = String::new();
    for attempt in 0..5 {
        let (port, hook_port) = pick();
        let listen = format!("listen = {}\n", listen.unwrap_or("[\"loopback\"]"));
        std::fs::write(
            dir.join("config.toml"),
            format!("port = {port}\nhook_port = {hook_port}\n{listen}{extra_config}"),
        )
        .unwrap();
        let log = dir.join(format!("daemon-{attempt}.log"));
        let mut cmd = acd(dir);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let child = cmd
            .arg("run")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let mut daemon = Daemon(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if ready(dir) {
                return Started {
                    daemon,
                    port,
                    hook_port,
                    log,
                };
            }
            if let Some(status) = daemon.0.try_wait().unwrap() {
                let text = std::fs::read_to_string(&log).unwrap_or_default();
                // Only TCP bind failures are retried; the Unix-socket bind
                // error also says "cannot bind" but a new port will not fix it.
                if text.contains("cannot bind 127.")
                    || text.contains("cannot bind [")
                    || text.contains("Address already in use")
                {
                    last_log = text;
                    break;
                }
                panic!("acd run exited with {status}: {text}");
            }
            assert!(Instant::now() < deadline, "daemon did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    panic!("could not start acd after 5 attempts; last log: {last_log}");
}

fn ready(dir: &Path) -> bool {
    dir.join("acd.sock").exists()
        && acd(dir)
            .arg("status")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}
