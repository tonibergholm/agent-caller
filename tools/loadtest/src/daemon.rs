//! Starting, stopping and restarting the process under test.
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tokio::net::UnixStream;

#[derive(Clone)]
pub enum Target {
    /// The real `acd` binary, driven through the producer API on hook.sock.
    Acd { bin: PathBuf },
    /// This binary's own `host` mode (instrumented core, built-in agents).
    Host { args: Vec<String> },
}

pub struct Proc {
    pub target: Target,
    pub dir: PathBuf,
    pub child: Option<Child>,
    pub generation: u32,
    pub ports: (u16, u16),
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn acd_cmd(bin: &Path, dir: &Path) -> Command {
    let mut c = Command::new(bin);
    c.env("ACD_CONFIG_DIR", dir).env("HOME", dir);
    for k in [
        "HERDR_SOCKET_PATH", "HERDR_PANE_ID", "CLAUDE_CONFIG_DIR", "CODEX_HOME",
        "CLAUDE_CODE_ENTRYPOINT", "RUST_LOG",
    ] {
        c.env_remove(k);
    }
    for (k, _) in std::env::vars() {
        if k.starts_with("ACD_TEST_") {
            c.env_remove(k);
        }
    }
    c
}

impl Proc {
    pub fn new(target: Target, dir: PathBuf) -> Proc {
        Proc { target, dir, child: None, generation: 0, ports: (free_port(), free_port()) }
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    pub fn acd_sock(&self) -> PathBuf {
        self.dir.join("acd.sock")
    }
    pub fn hook_sock(&self) -> PathBuf {
        self.dir.join("hook.sock")
    }

    /// One-time setup: `acd init` and the config. Never prints a token.
    pub fn init(&self) -> Result<(), String> {
        crate::workdir::claim(&self.dir)?;
        if let Target::Acd { bin } = &self.target {
            let out = acd_cmd(bin, &self.dir).arg("init").output().map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(format!("acd init failed: {}", String::from_utf8_lossy(&out.stderr)));
            }
            std::fs::write(
                self.dir.join("config.toml"),
                format!("port = {}\nhook_port = {}\nlisten = [\"127.0.0.1\"]\n", self.ports.0, self.ports.1),
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn hook_token(&self) -> Result<String, String> {
        let raw = std::fs::read_to_string(self.dir.join("hook-tokens.json")).map_err(|e| e.to_string())?;
        let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        v["current"].as_str().map(str::to_owned).ok_or_else(|| "no current hook token".into())
    }

    pub async fn start(&mut self) -> Result<(), String> {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("process.log"))
            .map_err(|e| e.to_string())?;
        let mut cmd = match &self.target {
            Target::Acd { bin } => {
                let mut c = acd_cmd(bin, &self.dir);
                c.arg("run");
                c
            }
            Target::Host { args } => {
                let mut c = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
                c.arg("host").arg("--dir").arg(&self.dir).args(args);
                c
            }
        };
        let child = cmd.stdout(Stdio::null()).stderr(log).spawn().map_err(|e| e.to_string())?;
        self.child = Some(child);
        self.generation += 1;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let up = UnixStream::connect(self.acd_sock()).await.is_ok()
                && (matches!(self.target, Target::Host { .. }) || UnixStream::connect(self.hook_sock()).await.is_ok());
            if up {
                return Ok(());
            }
            if let Some(c) = self.child.as_mut()
                && let Ok(Some(st)) = c.try_wait()
            {
                return Err(format!("process exited at start: {st}"));
            }
            if Instant::now() > deadline {
                return Err("process did not come up in 30 s".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Stops the process: SIGTERM and wait (a clean stop), or SIGKILL.
    pub fn stop(&mut self, graceful: bool) {
        let Some(mut c) = self.child.take() else { return };
        if graceful {
            // SAFETY: signalling a child this struct spawned.
            unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if matches!(c.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let _ = c.kill();
        let _ = c.wait();
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.stop(false);
    }
}
