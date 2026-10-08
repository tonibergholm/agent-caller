//! herdr socket discovery and supervision (spec §6.4): finds live sockets,
//! runs one reconciler per socket, and reports the herdr source status.
use std::collections::HashMap;
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use acd_core::Core;
use acd_protocol::{Source, SourceState, SourceStatus};
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

/// The path is a socket and accepts a connection.
pub fn is_live(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
        && std::fs::metadata(path).is_ok_and(|m| m.file_type().is_socket())
        && UnixStream::connect(path).is_ok()
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
        .map(|p| {
            tokio::spawn(async move {
                let q = p.clone();
                let live = matches!(
                    timeout(
                        LIVE_TIMEOUT,
                        tokio::task::spawn_blocking(move || is_live(&q))
                    )
                    .await,
                    Ok(Ok(true))
                );
                live.then_some(p)
            })
        })
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
