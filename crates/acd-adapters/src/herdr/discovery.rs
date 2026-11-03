//! herdr socket discovery and supervision (spec §6.4): finds live sockets,
//! runs one reconciler per socket, and reports the herdr source status.
use std::collections::{HashMap, VecDeque};
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use acd_core::Core;
use acd_protocol::{Source, SourceState, SourceStatus};
use tokio::net::UnixStream;
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval, timeout};

use super::client::Herdr;
use super::keys::KeysTargets;
use super::reconciler::{self, Health, PaneMap, ReconcilerHandle, Timing};

const LIVE_TIMEOUT: Duration = Duration::from_secs(1);
const HEALTH_POLL: Duration = Duration::from_millis(500);
/// Consecutive scans a known socket may be missing before it is dropped.
const MISSED_LIMIT: u32 = 2;

pub struct Discovery {
    pub env_socket: Option<PathBuf>,
    pub home: PathBuf,
    pub extra: Vec<PathBuf>,
    pub interval: Duration,
    pub timing: Timing,
    pub keys: Arc<KeysTargets>,
}

/// Candidate sockets in priority order, canonicalized when possible and
/// deduplicated.
pub fn candidates(env_socket: Option<&Path>, home: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let base = home.join(".config/herdr");
    // An empty home would make `base` cwd-relative: skip the home-based paths.
    let has_home = !home.as_os_str().is_empty();
    let mut all: Vec<PathBuf> = Vec::new();
    match env_socket {
        Some(p) => all.push(p.to_path_buf()),
        None if has_home => all.push(base.join("herdr.sock")),
        None => {}
    }
    if has_home {
        all.extend(session_sockets(&base.join("sessions")));
    }
    all.extend(extra.iter().cloned());
    let mut out: Vec<PathBuf> = Vec::new();
    for p in all {
        let p = p.canonicalize().unwrap_or(p);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

fn session_sockets(dir: &Path) -> Vec<PathBuf> {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound && !LOGGED.swap(true, Ordering::Relaxed) {
                tracing::debug!(dir = %dir.display(), error = %e, "herdr sessions dir unreadable");
            }
            return Vec::new();
        }
    };
    let mut socks: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path().join("herdr.sock"))
        .filter(|p| p.symlink_metadata().is_ok())
        .collect();
    socks.sort();
    socks
}

/// Our effective uid, or `None` where there are no owner checks (Android):
/// discovery then refuses every socket, because it cannot prove who owns it.
#[cfg(owner_checks)]
fn current_euid() -> Option<u32> {
    Some(acd_securefs::euid())
}
#[cfg(not(owner_checks))]
fn current_euid() -> Option<u32> {
    None
}

/// The path is a socket owned by the current user, under directories no other
/// user can change, and accepts a connection
/// within `LIVE_TIMEOUT`. A socket owned by anyone else is refused (fail
/// closed): acd would show its screens and send it keys and typed text.
/// The connect is async, so a hung herdr leaves no blocked thread behind.
pub async fn is_live(path: &Path) -> bool {
    is_live_as(path, current_euid()).await
}

async fn is_live_as(path: &Path, euid: Option<u32>) -> bool {
    if std::fs::symlink_metadata(path).is_err() {
        return false;
    }
    let Ok(m) = std::fs::metadata(path) else {
        return false;
    };
    if !m.file_type().is_socket() {
        return false;
    }
    // The socket and every ancestor directory must be ours or root's and not
    // swappable by another user (see `check_socket_path_as`). Connect to the
    // canonical path that was checked.
    let Some(Ok(real)) = euid.map(|e| acd_securefs::check_socket_path_as(path, e)) else {
        warn_refused(path);
        return false;
    };
    matches!(
        timeout(LIVE_TIMEOUT, UnixStream::connect(&real)).await,
        Ok(Ok(_))
    )
}

/// How many refused paths are remembered to keep the warning to one each.
const SEEN_LIMIT: usize = 64;

/// True the first time a path is offered; remembers at most `SEEN_LIMIT`,
/// dropping the oldest.
fn first_sighting(seen: &mut VecDeque<PathBuf>, path: &Path) -> bool {
    if seen.iter().any(|p| p == path) {
        return false;
    }
    if seen.len() >= SEEN_LIMIT {
        seen.pop_front();
    }
    seen.push_back(path.to_path_buf());
    true
}

/// One warning per path: discovery rescans on an interval.
fn warn_refused(path: &Path) {
    static SEEN: Mutex<VecDeque<PathBuf>> = Mutex::new(VecDeque::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if first_sighting(&mut seen, path) {
        tracing::warn!(socket = %path.display(), "herdr socket or one of its directories is not owned by the current user or can be changed by others; refusing it");
    }
}

pub fn aggregate(healths: &[(PathBuf, Health)]) -> SourceStatus {
    if healths.is_empty() {
        return SourceStatus {
            state: SourceState::Unconfigured,
            message: Some("no herdr socket found".into()),
        };
    }
    let problems: Vec<String> = healths
        .iter()
        .filter_map(|(path, h)| {
            let why = match h {
                Health::Connected => return None,
                Health::Connecting => "connecting",
                Health::Down(m) | Health::Incompatible(m) => m.as_str(),
            };
            Some(format!("{}: {why}", path.display()))
        })
        .collect();
    if problems.is_empty() {
        SourceStatus {
            state: SourceState::Ok,
            message: None,
        }
    } else {
        SourceStatus {
            state: SourceState::Degraded,
            message: Some(problems.join("; ")),
        }
    }
}

struct Known {
    handle: ReconcilerHandle,
    missed: u32,
}

pub fn spawn(d: Discovery, core: Arc<Core>, panes: Arc<PaneMap>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut known: HashMap<PathBuf, Known> = HashMap::new();
        let mut scan_tick = interval(d.interval);
        scan_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut health_tick = interval(HEALTH_POLL);
        health_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = scan_tick.tick() => {
                    scan(&d, &core, &panes, &mut known).await;
                    report(&core, &known);
                }
                _ = health_tick.tick() => report(&core, &known),
            }
        }
    })
}

async fn live_set(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let probes: Vec<_> = paths
        .into_iter()
        .map(|p| tokio::spawn(async move { is_live(&p).await.then_some(p) }))
        .collect();
    let mut live = Vec::new();
    for probe in probes {
        if let Ok(Some(p)) = probe.await {
            live.push(p);
        }
    }
    live
}

async fn scan(
    d: &Discovery,
    core: &Arc<Core>,
    panes: &Arc<PaneMap>,
    known: &mut HashMap<PathBuf, Known>,
) {
    let mut paths = candidates(d.env_socket.as_deref(), &d.home, &d.extra);
    for p in known.keys() {
        if !paths.contains(p) {
            paths.push(p.clone());
        }
    }
    let live = live_set(paths).await;
    for path in &live {
        match known.get_mut(path) {
            Some(k) => k.missed = 0,
            None => {
                tracing::info!(socket = %path.display(), "herdr socket found");
                let handle = reconciler::spawn(
                    Herdr::new(path.clone()),
                    core.clone(),
                    panes.clone(),
                    d.timing.clone(),
                    d.keys.clone(),
                );
                known.insert(path.clone(), Known { handle, missed: 0 });
            }
        }
    }
    let gone: Vec<PathBuf> = known
        .iter_mut()
        .filter(|(p, _)| !live.contains(p))
        .filter_map(|(p, k)| {
            k.missed += 1;
            (k.missed >= MISSED_LIMIT).then(|| p.clone())
        })
        .collect();
    for path in gone {
        if let Some(k) = known.remove(&path) {
            tracing::info!(socket = %path.display(), "herdr socket gone");
            k.handle.shutdown(true).await;
        }
    }
}

fn report(core: &Core, known: &HashMap<PathBuf, Known>) {
    let mut healths: Vec<(PathBuf, Health)> = known
        .iter()
        .map(|(p, k)| (p.clone(), k.handle.health().borrow().clone()))
        .collect();
    healths.sort_by(|a, b| a.0.cmp(&b.0));
    core.set_source_status(Source::Herdr, aggregate(&healths));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn candidate_order_and_dedup() {
        let home = tempfile::Builder::new()
            .prefix("acd")
            .tempdir_in("/tmp")
            .unwrap(); // macOS socket path limit
        let base = home.path().join(".config/herdr");
        std::fs::create_dir_all(base.join("sessions/a")).unwrap();
        std::fs::create_dir_all(base.join("sessions/b")).unwrap();
        let _l1 = std::os::unix::net::UnixListener::bind(base.join("herdr.sock")).unwrap();
        let _l2 =
            std::os::unix::net::UnixListener::bind(base.join("sessions/a/herdr.sock")).unwrap();
        let extra = vec![base.join("herdr.sock")]; // duplicate of the default
        let c = candidates(None, home.path(), &extra);
        let names: Vec<_> = c
            .iter()
            .map(|p| {
                p.strip_prefix(base.canonicalize().unwrap())
                    .unwrap()
                    .to_path_buf()
            })
            .collect();
        assert_eq!(
            names,
            vec![
                PathBuf::from("herdr.sock"),
                PathBuf::from("sessions/a/herdr.sock")
            ]
        );
        let env = home.path().join("custom.sock");
        assert_eq!(
            candidates(Some(&env), home.path(), &[])[0]
                .file_name()
                .unwrap(),
            "custom.sock"
        );
    }

    #[test]
    fn empty_home_skips_home_paths_but_keeps_env_and_extra() {
        assert!(candidates(None, Path::new(""), &[]).is_empty());
        let c = candidates(
            Some(Path::new("/e.sock")),
            Path::new(""),
            &[PathBuf::from("/x.sock")],
        );
        assert_eq!(c, vec![PathBuf::from("/e.sock"), PathBuf::from("/x.sock")]);
    }

    #[tokio::test]
    async fn only_a_listening_socket_is_live() {
        let dir = tempfile::Builder::new()
            .prefix("acd")
            .tempdir_in("/tmp")
            .unwrap(); // macOS socket path limit
        let live = dir.path().join("live.sock");
        let _l = std::os::unix::net::UnixListener::bind(&live).unwrap();
        assert!(is_live(&live).await);
        let stale = dir.path().join("stale.sock");
        drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
        assert!(!is_live(&stale).await, "no listener");
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert!(!is_live(&file).await);
        assert!(!is_live(&dir.path().join("missing")).await);
    }

    #[tokio::test]
    async fn a_socket_owned_by_another_user_is_refused() {
        let dir = tempfile::Builder::new()
            .prefix("acd")
            .tempdir_in("/tmp")
            .unwrap();
        let sock = dir.path().join("herdr.sock");
        let _l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let owner = std::fs::metadata(&sock).unwrap().uid();
        assert!(is_live_as(&sock, Some(owner)).await, "own socket");
        assert!(!is_live_as(&sock, Some(owner + 1)).await, "foreign owner");
        assert!(!is_live_as(&sock, None).await, "no owner checks");
    }

    #[tokio::test]
    async fn a_socket_under_a_world_writable_non_sticky_dir_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::Builder::new().prefix("acd").tempdir_in("/tmp").unwrap();
        let d = t.path().join("w");
        std::fs::create_dir(&d).unwrap();
        let sock = d.join("herdr.sock");
        let _l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let me = std::fs::metadata(&sock).unwrap().uid();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!is_live_as(&sock, Some(me)).await, "non-sticky 0777");
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o1777)).unwrap();
        assert!(is_live_as(&sock, Some(me)).await, "sticky, own socket");
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(is_live_as(&sock, Some(me)).await, "private dir");
    }

    #[tokio::test]
    async fn a_path_owned_by_someone_else_is_refused() {
        // Judged as a user who owns nothing here, so the path belongs to
        // someone else. This fakes the foreign owner by changing the euid
        // we judge as (chown needs root); the per-ancestor rule itself is
        // pinned by `socket_chain_rules` in acd-securefs.
        let t = tempfile::Builder::new().prefix("acd").tempdir_in("/tmp").unwrap();
        let sock = t.path().join("herdr.sock");
        let _l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let me = std::fs::metadata(&sock).unwrap().uid();
        assert!(!is_live_as(&sock, Some(me.wrapping_add(7))).await);
    }

    #[test]
    fn refused_path_warnings_are_bounded() {
        let mut seen = VecDeque::new();
        assert!(first_sighting(&mut seen, Path::new("/a")));
        assert!(!first_sighting(&mut seen, Path::new("/a")));
        for i in 0..SEEN_LIMIT * 2 {
            first_sighting(&mut seen, Path::new(&format!("/p{i}")));
        }
        assert_eq!(seen.len(), SEEN_LIMIT);
        assert!(first_sighting(&mut seen, Path::new("/a")), "oldest was dropped");
    }

    #[test]
    fn aggregate_status() {
        assert_eq!(aggregate(&[]).state, SourceState::Unconfigured);
        assert_eq!(
            aggregate(&[("/a".into(), Health::Connected)]).state,
            SourceState::Ok
        );
        let s = aggregate(&[
            ("/a".into(), Health::Connected),
            ("/b".into(), Health::Incompatible("herdr 0.10.0".into())),
        ]);
        assert_eq!(s.state, SourceState::Degraded);
        assert!(s.message.unwrap().contains("/b"));
    }
}
