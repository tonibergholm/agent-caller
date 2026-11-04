//! Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
//! A child module of `paths`, for the crate-private `Paths` cache.
use super::*;
use std::sync::atomic::Ordering;

fn paths(freshness: Duration) -> (Paths, Arc<AtomicU64>) {
    let resolutions = Arc::new(AtomicU64::new(0));
    let resolver = PathResolver::new(|p| Some(p.join("canonical")));
    let paths = Paths::new(resolver, 64, Duration::from_secs(3600), freshness, resolutions.clone());
    (paths, resolutions)
}

/// Waits (by yielding, never by sleeping) until the cache has an answer for `p`.
async fn settled(paths: &Paths, p: &Path) -> Resolved {
    loop {
        match paths.get(p) {
            Resolved::Unknown => {
                tokio::task::yield_now().await;
                std::thread::yield_now();
            }
            known => return known,
        }
    }
}

/// paths.rs:118 `<` -> `<=` (`lookup`): an answer exactly `freshness` old is stale (fail
/// closed, since a symlink can be retargeted at any time), one a moment younger is fresh.
#[tokio::test(start_paused = true)]
async fn an_answer_exactly_as_old_as_the_freshness_window_is_unknown() {
    let freshness = Duration::from_secs(10);
    let (paths, resolutions) = paths(freshness);
    let p = Path::new("/some/dir");
    assert_eq!(settled(&paths, p).await, Resolved::Canonical(p.join("canonical")));
    assert_eq!(resolutions.load(Ordering::SeqCst), 1);
    tokio::time::advance(freshness - Duration::from_nanos(1)).await;
    assert_eq!(paths.get(p), Resolved::Canonical(p.join("canonical")));
    assert_eq!(resolutions.load(Ordering::SeqCst), 1, "still fresh: no new worker");
    tokio::time::advance(Duration::from_nanos(1)).await;
    assert_eq!(paths.get(p), Resolved::Unknown, "stale: unknown until a worker refreshes it");
    assert_eq!(settled(&paths, p).await, Resolved::Canonical(p.join("canonical")));
    assert_eq!(resolutions.load(Ordering::SeqCst), 2);
}

/// paths.rs:144 `delete !` (`lookup`): the cache never grows past `MAX_KNOWN`; a full
/// cache makes room by dropping its oldest answer.
#[tokio::test]
async fn the_path_cache_is_bounded() {
    let (paths, _) = paths(Duration::from_secs(3600));
    for i in 0..MAX_KNOWN + 25 {
        let p = PathBuf::from(format!("/bound/{i}"));
        settled(&paths, &p).await;
        assert!(paths.inner.lock().unwrap().known.len() <= MAX_KNOWN, "after {i}");
    }
    assert_eq!(paths.inner.lock().unwrap().known.len(), MAX_KNOWN);
}
