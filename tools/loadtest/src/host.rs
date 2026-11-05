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

/// Device and inode of a filesystem object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Identity {
    dev: u64,
    ino: u64,
}

fn identity_of(m: &std::fs::Metadata) -> Identity {
    use std::os::unix::fs::MetadataExt;
    Identity { dev: m.dev(), ino: m.ino() }
}

/// The identity of the socket this tool bound, recorded beside it as `<dev> <ino>`.
fn sock_record(sock: &std::path::Path) -> std::path::PathBuf {
    sock.with_file_name("acd.sock.ino")
}

fn read_record(path: &std::path::Path) -> Option<Identity> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path).ok()?;
    if !f.metadata().ok()?.is_file() {
        return None;
    }
    let mut s = String::new();
    f.take(64).read_to_string(&mut s).ok()?;
    let mut it = s.split_whitespace();
    let (dev, ino) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    it.next().is_none().then_some(Identity { dev, ino })
}

fn write_record(sock: &std::path::Path, id: Identity) -> Result<(), String> {
    crate::workdir::write_atomic(&sock_record(sock), format!("{} {}", id.dev, id.ino).as_bytes())
}

/// Unlinks `sock` only while it is still a socket with exactly this device and inode.
/// Anything else is left alone. (A swap between the check and the unlink needs a hostile
/// process with write access to the marked directory.)
fn remove_if_identical(sock: &std::path::Path, id: Identity) -> bool {
    use std::os::unix::fs::FileTypeExt;
    match std::fs::symlink_metadata(sock) {
        Ok(m) if m.file_type().is_socket() && identity_of(&m) == id => std::fs::remove_file(sock).is_ok(),
        _ => false,
    }
}

/// An exclusive lock on `<dir>/acd.host.lock`, held for the process lifetime, serialises the
/// startup ownership decision: two hosts can never both take ownership of the socket path.
fn take_lock(dir: &std::path::Path) -> Result<std::fs::File, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let path = dir.join("acd.host.lock");
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|e| format!("refusing {}: {}", path.display(), e.kind()))?;
    if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return Err(format!("refusing {}: not a regular file", path.display()));
    }
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(format!("refusing {}: another host holds the lock", path.display()));
    }
    Ok(f)
}

/// Prepares the socket path. A missing path is fine. An existing one is unlinked only when it
/// is a socket whose device and inode this tool recorded when it bound it, and nothing answers
/// on it. Anything else (a foreign socket, a live host, an unrecorded file) is refused and left
/// alone. The caller holds the host lock.
fn clear_stale_socket(sock: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::FileTypeExt;
    let m = match std::fs::symlink_metadata(sock) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("socket: {}", e.kind())),
    };
    let refuse = |why: &str| Err(format!("refusing {}: {why}", sock.display()));
    if !m.file_type().is_socket() {
        return refuse("exists and is not a socket this tool created");
    }
    let id = identity_of(&m);
    if read_record(&sock_record(sock)) != Some(id) {
        return refuse("a socket this tool did not create");
    }
    match std::os::unix::net::UnixStream::connect(sock) {
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
        Ok(_) => return refuse("another host is listening on it"),
        Err(e) => return refuse(&format!("cannot prove it is stale ({})", e.kind())),
    }
    if remove_if_identical(sock, id) || !sock.exists() {
        Ok(())
    } else {
        refuse("the socket changed while it was being cleared")
    }
}

/// The socket this host bound and the lock that proves nobody else may take it over.
struct Owned {
    listener: std::os::unix::net::UnixListener,
    sock: std::path::PathBuf,
    id: Identity,
    _lock: std::fs::File,
}

impl Owned {
    /// Removes the socket and its record only if they are still the ones this host created.
    fn release(self) {
        remove_if_identical(&self.sock, self.id);
        let rec = sock_record(&self.sock);
        if read_record(&rec) == Some(self.id) {
            let _ = std::fs::remove_file(rec);
        }
    }
}

/// Takes the host lock, clears a provably stale socket and binds `<dir>/acd.sock`.
fn claim_socket(dir: &std::path::Path) -> Result<Owned, String> {
    let lock = take_lock(dir)?;
    let sock = dir.join("acd.sock");
    clear_stale_socket(&sock)?;
    let listener = std::os::unix::net::UnixListener::bind(&sock).map_err(|e| e.to_string())?;
    // Identity of the path as bound (fstat of a socket fd is the socket's, not the path's).
    let id = identity_of(&std::fs::symlink_metadata(&sock).map_err(|e| e.to_string())?);
    let owned = Owned { listener, sock: sock.clone(), id, _lock: lock };
    let setup = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())
        .and_then(|()| write_record(&sock, id))
        .and_then(|()| owned.listener.set_nonblocking(true).map_err(|e| e.to_string()));
    if let Err(e) = setup {
        owned.release();
        return Err(e);
    }
    Ok(owned)
}

/// Publishes the table sizes once a second. Returns the error that stopped it: a failed
/// publish must not leave an old measurement looking current.
async fn publish_sizes(core: Arc<Core>, dir: PathBuf) -> String {
    loop {
        let s = core.sizes();
        let tasks = tokio::runtime::Handle::current().metrics().num_alive_tasks();
        let body = format!(
            "{{\"agents\":{},\"requests\":{},\"keys\":{},\"seen\":{},\"idem\":{},\"ring\":{},\"receivers\":{},\"tasks\":{}}}",
            s.agents, s.requests, s.keys, s.seen, s.idem, s.ring, s.receivers, tasks
        );
        if let Err(e) = crate::workdir::write_atomic(&dir.join("sizes.json"), body.as_bytes()) {
            return format!("sizes.json: {e}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
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
    let owned = claim_socket(&a.dir)?;
    let listener = match owned.listener.try_clone().and_then(tokio::net::UnixListener::from_std) {
        Ok(l) => l,
        Err(e) => {
            owned.release();
            return Err(e.to_string());
        }
    };
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    // Table sizes, once a second; a failed publish stops the host (non-zero exit).
    let (fail_tx, mut fail_rx) = tokio::sync::mpsc::channel::<String>(1);
    {
        let (core, dir) = (core.clone(), a.dir.clone());
        tokio::spawn(async move {
            let _ = fail_tx.send(publish_sizes(core, dir).await).await;
        });
    }

    let burst = Arc::new(Notify::new());
    let counter = Arc::new(AtomicU64::new(0));
    for i in 0..a.agents {
        tokio::spawn(agent_loop(i, a.clone(), core.clone(), adapter.clone(), burst.clone(), counter.clone()));
    }
    // SIGUSR1 asks for a burst: every agent opens a request at once, plus
    // `burst_extra` one-shot agents.
    let signals = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
        .and_then(|u| tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map(|t| (u, t)));
    let (mut usr1, mut term) = match signals {
        Ok(s) => s,
        Err(e) => {
            owned.release();
            return Err(e.to_string());
        }
    };
    loop {
        tokio::select! {
            _ = usr1.recv() => {
                burst.notify_waiters();
                for j in 0..a.burst_extra {
                    tokio::spawn(one_shot(j, a.clone(), core.clone(), adapter.clone(), counter.clone()));
                }
            }
            _ = term.recv() => { owned.release(); return Ok(()); }
            Some(e) = fail_rx.recv() => { owned.release(); return Err(e); }
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
    #[tokio::test]
    async fn a_live_foreign_listener_at_the_socket_path_is_not_unlinked() {
        let d = scratch("foreign");
        crate::workdir::claim(&d.join("run")).unwrap();
        let l = std::os::unix::net::UnixListener::bind(d.join("run/acd.sock")).unwrap();
        assert!(run(args(d.join("run"))).await.is_err());
        assert!(std::os::unix::net::UnixStream::connect(d.join("run/acd.sock")).is_ok());
        drop(l);
        // A dead foreign socket is no more ours: it has no record.
        assert!(run(args(d.join("run"))).await.is_err());
        assert!(d.join("run/acd.sock").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn a_second_host_refuses_and_leaves_the_running_host_alone() {
        let d = scratch("second");
        crate::workdir::claim(&d.join("run")).unwrap();
        let first = tokio::spawn(run(args(d.join("run"))));
        let sock = d.join("run/acd.sock");
        while std::os::unix::net::UnixStream::connect(&sock).is_err() {
            tokio::task::yield_now().await;
        }
        assert!(run(args(d.join("run"))).await.is_err());
        assert!(std::os::unix::net::UnixStream::connect(&sock).is_ok());
        first.abort();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn a_stale_socket_this_tool_recorded_is_replaced() {
        let d = scratch("stale");
        crate::workdir::claim(&d.join("run")).unwrap();
        let sock = d.join("run/acd.sock");
        drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
        write_record(&sock, identity_of(&std::fs::symlink_metadata(&sock).unwrap())).unwrap();
        assert!(clear_stale_socket(&sock).is_ok());
        assert!(!sock.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn shutdown_leaves_a_socket_it_did_not_create() {
        let d = scratch("replaced");
        crate::workdir::claim(&d.join("run")).unwrap();
        let run_dir = d.join("run");
        let owned = claim_socket(&run_dir).unwrap();
        let sock = run_dir.join("acd.sock");
        std::fs::remove_file(&sock).unwrap();
        let foreign = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        owned.release();
        assert!(std::os::unix::net::UnixStream::connect(&sock).is_ok(), "foreign socket was unlinked");
        drop(foreign);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn shutdown_removes_the_socket_and_record_it_created() {
        let d = scratch("release");
        crate::workdir::claim(&d.join("run")).unwrap();
        let run_dir = d.join("run");
        claim_socket(&run_dir).unwrap().release();
        assert!(!run_dir.join("acd.sock").exists());
        assert!(!sock_record(&run_dir.join("acd.sock")).exists());
        // The lock went with the host: a new one can start.
        claim_socket(&run_dir).unwrap().release();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn two_concurrent_starts_over_one_stale_record_leave_exactly_one_host() {
        let d = scratch("race");
        let run_dir = d.join("run");
        crate::workdir::claim(&run_dir).unwrap();
        // A previous host died and left its socket and record behind.
        let sock = run_dir.join("acd.sock");
        drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
        write_record(&sock, identity_of(&std::fs::symlink_metadata(&sock).unwrap())).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let (b, dir) = (barrier.clone(), run_dir.clone());
                std::thread::spawn(move || {
                    b.wait();
                    claim_socket(&dir)
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "exactly one host may run");
        assert!(std::os::unix::net::UnixStream::connect(&sock).is_ok(), "the running host's socket was removed");
        drop(results);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn a_failed_sizes_publish_is_reported_not_swallowed() {
        let d = scratch("sizes");
        // A directory at the target makes the atomic rename fail.
        std::fs::create_dir(d.join("sizes.json")).unwrap();
        let core = Core::new(test_machine(), vec![Arc::new(LoadAdapter::default())]);
        let err = publish_sizes(core, d.clone()).await;
        assert!(err.starts_with("sizes.json:"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
