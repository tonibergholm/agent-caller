//! Filesystem path resolution off the reconciler. Resolving a path can
//! block indefinitely (an unavailable network mount), so it runs only in
//! a bounded number of blocking workers. The reconciler reads a cache and
//! waits only for workers it just started, up to a deadline, and only where
//! it holds no lock. Results are plain path facts: they change no hook
//! state, and every use re-runs the binding rules against current state.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Semaphore, oneshot};
use tokio::time::Instant;

/// Resolves a path to its canonical form, `None` when it does not resolve
/// (`std::fs::canonicalize` by default; injectable for tests).
#[derive(Clone)]
pub struct PathResolver(Arc<ResolveFn>);

type ResolveFn = dyn Fn(&Path) -> Option<PathBuf> + Send + Sync;

impl PathResolver {
    pub fn fs() -> Self {
        Self::new(|p| std::fs::canonicalize(p).ok())
    }
    pub fn new(f: impl Fn(&Path) -> Option<PathBuf> + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }
}

impl std::fmt::Debug for PathResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PathResolver")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// Never resolved yet (a worker may be running or none was free).
    Unknown,
    /// Resolution finished: the path does not resolve.
    Missing,
    Canonical(PathBuf),
}

/// Cached paths; payload cwds are attacker-chosen, so the map is bounded.
const MAX_KNOWN: usize = 4096;
/// A path that did not resolve is tried again after this; meanwhile, and
/// while the retry runs, `Missing` stays the answer.
const MISSING_TTL: Duration = Duration::from_secs(5);

struct Entry {
    canonical: Option<PathBuf>,
    at: Instant,
}

#[derive(Default)]
struct Inner {
    known: HashMap<PathBuf, Entry>,
    pending: HashSet<PathBuf>,
}

pub(crate) struct Paths {
    resolver: PathResolver,
    inner: Arc<Mutex<Inner>>,
    /// Outstanding workers. A worker's permit lives in its closure, so a
    /// worker stuck on a mount keeps its slot: at most this many threads
    /// can ever be stuck.
    workers: Arc<Semaphore>,
    wait: Duration,
    resolutions: Arc<AtomicU64>,
}

impl Paths {
    pub fn new(
        resolver: PathResolver,
        max_workers: usize,
        wait: Duration,
        resolutions: Arc<AtomicU64>,
    ) -> Self {
        Self {
            resolver,
            inner: Arc::default(),
            workers: Arc::new(Semaphore::new(max_workers)),
            wait,
            resolutions,
        }
    }

    /// The cached answer; never blocks. An unknown path, or a `Missing`
    /// one past its TTL, gets a worker if one is free.
    pub fn get(&self, p: &Path) -> Resolved {
        self.lookup(p).0
    }

    /// Waits up to the deadline for the workers this call starts, never
    /// for a path already pending, so a stuck path costs the deadline once.
    /// Call only where the reconciler holds no lock.
    pub async fn prefetch(&self, paths: impl IntoIterator<Item = PathBuf>) {
        let started: Vec<_> = paths
            .into_iter()
            .filter_map(|p| self.lookup(&p).1)
            .collect();
        if started.is_empty() {
            return;
        }
        let _ = tokio::time::timeout(self.wait, futures_util::future::join_all(started)).await;
    }

    fn lookup(&self, p: &Path) -> (Resolved, Option<oneshot::Receiver<()>>) {
        let mut inner = self.inner.lock().expect("paths");
        let (value, fresh) = match inner.known.get(p) {
            Some(Entry {
                canonical: Some(c), ..
            }) => (Resolved::Canonical(c.clone()), true),
            Some(Entry {
                canonical: None,
                at,
            }) => (Resolved::Missing, at.elapsed() < MISSING_TTL),
            None => (Resolved::Unknown, false),
        };
        if fresh || inner.pending.contains(p) {
            return (value, None);
        }
        let Ok(permit) = self.workers.clone().try_acquire_owned() else {
            return (value, None);
        };
        inner.pending.insert(p.to_path_buf());
        drop(inner);
        let (done, rx) = oneshot::channel();
        let path = p.to_path_buf();
        let resolver = self.resolver.clone();
        let shared = self.inner.clone();
        let resolutions = self.resolutions.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            resolutions.fetch_add(1, Ordering::SeqCst);
            let canonical = (resolver.0)(&path);
            let mut inner = shared.lock().expect("paths");
            inner.pending.remove(&path);
            if inner.known.len() >= MAX_KNOWN
                && !inner.known.contains_key(&path)
                && let Some(oldest) = inner
                    .known
                    .iter()
                    .min_by_key(|(_, e)| e.at)
                    .map(|(k, _)| k.clone())
            {
                inner.known.remove(&oldest);
            }
            inner.known.insert(
                path,
                Entry {
                    canonical,
                    at: Instant::now(),
                },
            );
            drop(inner);
            let _ = done.send(());
        });
        (value, Some(rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn paths(resolver: PathResolver, workers: usize) -> Paths {
        Paths::new(
            resolver,
            workers,
            Duration::from_millis(200),
            Arc::default(),
        )
    }

    #[tokio::test]
    async fn prefetch_resolves_and_get_reads_the_cache() {
        let p = paths(PathResolver::new(|p| Some(p.join("canon"))), 2);
        assert_eq!(p.get(Path::new("/a")), Resolved::Unknown);
        p.prefetch([PathBuf::from("/b"), PathBuf::from("/c")]).await;
        assert_eq!(
            p.get(Path::new("/b")),
            Resolved::Canonical("/b/canon".into())
        );
        let q = paths(PathResolver::new(|_| None), 1);
        q.prefetch([PathBuf::from("/x")]).await;
        assert_eq!(q.get(Path::new("/x")), Resolved::Missing);
    }

    /// Frees the stuck workers even when an assertion fails, so the
    /// runtime (which waits for blocking tasks) can shut down.
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stuck_workers_are_capped_and_never_waited_for_twice() {
        let release = Arc::new(AtomicBool::new(false));
        let _release = Release(release.clone());
        let r = release.clone();
        let p = paths(
            PathResolver::new(move |_| {
                while !r.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                None
            }),
            2,
        );
        let t = Instant::now();
        p.prefetch([PathBuf::from("/s1")]).await;
        assert!(t.elapsed() >= Duration::from_millis(200), "waited once");
        let t = Instant::now();
        p.prefetch([PathBuf::from("/s1")]).await; // pending: no wait
        p.prefetch([PathBuf::from("/s2")]).await; // the last free worker
        let t2 = Instant::now();
        p.prefetch((3..50).map(|i| PathBuf::from(format!("/s{i}"))))
            .await; // none free
        assert!(
            t2.elapsed() < Duration::from_millis(100),
            "no worker, no wait"
        );
        assert!(t.elapsed() < Duration::from_millis(400));
        assert_eq!(
            p.resolutions.load(Ordering::SeqCst),
            2,
            "two workers at most"
        );
        assert_eq!(p.get(Path::new("/s1")), Resolved::Unknown);
    }
}
