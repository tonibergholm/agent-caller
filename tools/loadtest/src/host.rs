//! An instrumented core with built-in synthetic agents, served on
//! `<dir>/acd.sock` like the daemon's local socket. It is the daemon's
//! engine, registry, idempotency cache, event ring and acd/1 server, minus
//! the source adapters. It writes the table sizes to `<dir>/sizes.json`,
//! which the real daemon cannot show. Unlike the producer API it is not
//! limited to 256 agents, and it can burst past the 1000-event ring.
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_core::testing::{agent_obs, permission_obs, test_machine};
use acd_core::{Adapter, Answer, Auth, Core, CorrelationKey, DeliverError, Observation, ServerOptions};
use acd_protocol::{AgentState, AnswerPath, Agent, ResolvedBy, Request, Source};
use async_trait::async_trait;
use tokio::sync::{Notify, oneshot};

use crate::rng::{Rng, now_us, seed_from_clock};

#[derive(Clone)]
pub struct HostArgs {
    pub dir: PathBuf,
    pub agents: usize,
    pub rate: f64,
    pub ack_delay_ms: u64,
    pub withdraw_after_s: f64,
    pub turnover: f64,
    pub burst_extra: usize,
    /// Padding added to every request's detail, to size snapshots.
    pub detail_bytes: usize,
    /// Every agent opens a request at start (and a withdrawn one reopens at once).
    pub open_at_start: bool,
}

#[derive(Default)]
struct LoadAdapter {
    waiting: Mutex<HashMap<String, oneshot::Sender<()>>>,
}

#[async_trait]
impl Adapter for LoadAdapter {
    fn name(&self) -> &'static str {
        "load"
    }
    fn answer_paths(&self) -> &[AnswerPath] {
        &[AnswerPath::Hook]
    }
    fn sources(&self) -> &[Source] {
        &[Source::Hooks]
    }
    async fn deliver(&self, request: &Request, _a: &Answer) -> Result<(), DeliverError> {
        if let Some(tx) = self.waiting.lock().unwrap().remove(&request.agent_id) {
            let _ = tx.send(());
        }
        Ok(())
    }
    async fn send(&self, _a: &Agent, _t: &str) -> Result<(), DeliverError> {
        Ok(())
    }
}

const TOKEN: &str = "Zk3pQe9xV2mRt7YhB5nLw1CdFg8JaUoIsEiKq0MAbcw";

/// Creates the token without following links or truncating anything. An existing file is kept
/// only when it is a regular file holding exactly the synthetic token (a restart in the same
/// run directory); anything else is refused and left alone.
fn write_token(tok: &std::path::Path) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(tok) {
        Ok(mut f) => f.write_all(TOKEN.as_bytes()).map_err(|e| format!("token: {}", e.kind())),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let f = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(tok)
                .map_err(|e| format!("refusing {}: not a file this tool created ({})", tok.display(), e.kind()))?;
            let mut got = String::new();
            let regular = f.metadata().map(|m| m.is_file()).unwrap_or(false);
            if !regular || f.take(256).read_to_string(&mut got).is_err() || got != TOKEN {
                return Err(format!("refusing {}: exists and is not this tool's token", tok.display()));
            }
            Ok(())
        }
        Err(e) => Err(format!("token: {}", e.kind())),
    }
}

/// Removes a leftover socket of an earlier run in this run directory; anything that is not a
/// socket owned by the current user is refused and left alone.
fn clear_stale_socket(sock: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    match std::fs::symlink_metadata(sock) {
        // SAFETY: geteuid has no preconditions.
        Ok(m) if m.file_type().is_socket() && m.uid() == unsafe { libc::geteuid() } => {
            std::fs::remove_file(sock).map_err(|e| format!("socket: {}", e.kind()))
        }
        Ok(_) => Err(format!("refusing {}: exists and is not a socket this tool created", sock.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("socket: {}", e.kind())),
    }
}

pub async fn run(a: HostArgs) -> Result<(), String> {
    // Nothing is touched before the directory proves to be harness-owned.
    crate::workdir::validate(&a.dir)?;
    std::fs::set_permissions(&a.dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    write_token(&a.dir.join("client.token"))?;
    let adapter = Arc::new(LoadAdapter::default());
    let core = Core::new(test_machine(), vec![adapter.clone()]);
    let auth = Arc::new(Auth::new(TOKEN.into()));
    let app = acd_core::router(core.clone(), auth, ServerOptions::default());
    let sock = a.dir.join("acd.sock");
    clear_stale_socket(&sock)?;
    let listener = tokio::net::UnixListener::bind(&sock).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    // Table sizes, once a second.
    {
        let (core, dir) = (core.clone(), a.dir.clone());
        tokio::spawn(async move {
            loop {
                let s = core.sizes();
                let tasks = tokio::runtime::Handle::current().metrics().num_alive_tasks();
                let body = format!(
                    "{{\"agents\":{},\"requests\":{},\"keys\":{},\"seen\":{},\"idem\":{},\"ring\":{},\"receivers\":{},\"tasks\":{}}}",
                    s.agents, s.requests, s.keys, s.seen, s.idem, s.ring, s.receivers, tasks
                );
                let tmp = dir.join("sizes.json.tmp");
                if std::fs::write(&tmp, body).is_ok() {
                    let _ = std::fs::rename(&tmp, dir.join("sizes.json"));
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }

    let burst = Arc::new(Notify::new());
    let counter = Arc::new(AtomicU64::new(0));
    for i in 0..a.agents {
        tokio::spawn(agent_loop(i, a.clone(), core.clone(), adapter.clone(), burst.clone(), counter.clone()));
    }
    // SIGUSR1 asks for a burst: every agent opens a request at once, plus
    // `burst_extra` one-shot agents.
    let mut usr1 = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1()).map_err(|e| e.to_string())?;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|e| e.to_string())?;
    loop {
        tokio::select! {
            _ = usr1.recv() => {
                burst.notify_waiters();
                for j in 0..a.burst_extra {
                    tokio::spawn(one_shot(j, a.clone(), core.clone(), adapter.clone(), counter.clone()));
                }
            }
            _ = term.recv() => { let _ = std::fs::remove_file(&sock); return Ok(()); }
        }
    }
}

async fn open_and_wait(
    agent: &str,
    key: &str,
    a: &HostArgs,
    core: &Arc<Core>,
    adapter: &Arc<LoadAdapter>,
) {
    let (tx, rx) = oneshot::channel();
    adapter.waiting.lock().unwrap().insert(agent.to_owned(), tx);
    let mut obs = permission_obs(agent, key);
    obs.reusable_key = false;
    obs.detail = format!("t0={};k={key};{}", now_us(), "x".repeat(a.detail_bytes));
    core.observe(Observation::RequestSeen(obs));
    let by = tokio::select! {
        _ = rx => {
            tokio::time::sleep(Duration::from_millis(a.ack_delay_ms)).await;
            ResolvedBy::Inbox
        }
        _ = tokio::time::sleep(Duration::from_secs_f64(a.withdraw_after_s)) => {
            adapter.waiting.lock().unwrap().remove(agent);
            ResolvedBy::Terminal
        }
    };
    core.observe(Observation::RequestEnded { agent_id: agent.into(), key: CorrelationKey(key.into()), by });
    // The agent goes back to work (the core does not guess the next state).
    let mut up = agent_obs(agent);
    up.state = AgentState::Working;
    core.observe(Observation::AgentUpsert(up));
}

async fn one_shot(j: usize, a: HostArgs, core: Arc<Core>, adapter: Arc<LoadAdapter>, counter: Arc<AtomicU64>) {
    let id = format!("burst-{}-{j}", counter.fetch_add(1, Relaxed));
    core.observe(Observation::AgentUpsert(agent_obs(&id)));
    open_and_wait(&id, &format!("{id}-k"), &a, &core, &adapter).await;
    core.observe(Observation::AgentGone { agent_id: id });
}

async fn agent_loop(
    i: usize,
    a: HostArgs,
    core: Arc<Core>,
    adapter: Arc<LoadAdapter>,
    burst: Arc<Notify>,
    counter: Arc<AtomicU64>,
) {
    let mut rng = Rng::new(seed_from_clock() ^ ((i as u64 + 1) * 7919));
    let mut incarnation = 0u32;
    let mut id = format!("agent-{i}-{incarnation}");
    core.observe(Observation::AgentUpsert(agent_obs(&id)));
    let mean = a.agents as f64 / a.rate.max(0.001);
    let mut first = a.open_at_start;
    loop {
        if first {
            first = false;
        } else {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs_f64(rng.exp(mean))) => {}
                _ = burst.notified() => {}
            }
        }
        if rng.chance(a.turnover) {
            core.observe(Observation::AgentGone { agent_id: id.clone() });
            incarnation += 1;
            id = format!("agent-{i}-{incarnation}");
            core.observe(Observation::AgentUpsert(agent_obs(&id)));
        }
        let n = counter.fetch_add(1, Relaxed);
        open_and_wait(&id, &format!("{id}-occ-{n}"), &a, &core, &adapter).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lt-host-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn args(dir: PathBuf) -> HostArgs {
        HostArgs { dir, agents: 0, rate: 1.0, ack_delay_ms: 1, withdraw_after_s: 1.0, turnover: 0.0, burst_extra: 0, detail_bytes: 0, open_at_start: false }
    }

    #[tokio::test]
    async fn a_symlinked_client_token_is_refused_and_its_target_survives() {
        let d = scratch("tok");
        let run_dir = d.join("run");
        crate::workdir::claim(&run_dir).unwrap();
        let target = d.join("valuable");
        std::fs::write(&target, b"keep me").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink(&target, run_dir.join("client.token")).unwrap();
        assert!(run(args(run_dir)).await.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep me");
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o644);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn a_symlinked_directory_to_a_real_config_dir_is_untouched() {
        let d = scratch("link");
        let real = d.join("real-acd");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("client.token"), b"real-secret").unwrap();
        let sock = std::os::unix::net::UnixListener::bind(real.join("acd.sock")).unwrap();
        let link = d.join("acd-loadtest-host");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(run(args(link)).await.is_err());
        assert_eq!(std::fs::read(real.join("client.token")).unwrap(), b"real-secret");
        assert!(real.join("acd.sock").exists());
        drop(sock);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn a_directory_without_the_marker_is_untouched() {
        let d = scratch("nomark");
        std::fs::write(d.join("client.token"), b"real-secret").unwrap();
        let sock = std::os::unix::net::UnixListener::bind(d.join("acd.sock")).unwrap();
        assert!(run(args(d.clone())).await.is_err());
        assert_eq!(std::fs::read(d.join("client.token")).unwrap(), b"real-secret");
        assert!(d.join("acd.sock").exists());
        drop(sock);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn a_non_socket_at_the_socket_path_is_refused_and_survives() {
        let d = scratch("sockfile");
        crate::workdir::claim(&d.join("run")).unwrap();
        std::fs::write(d.join("run/acd.sock"), b"not a socket").unwrap();
        assert!(run(args(d.join("run"))).await.is_err());
        assert_eq!(std::fs::read(d.join("run/acd.sock")).unwrap(), b"not a socket");
        let _ = std::fs::remove_dir_all(&d);
    }
}
