//! The hook token store: `hook-tokens.json` is the single source of truth
//! and the rotation transaction. Later tasks add retention and header rewrites.
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use acd_protocol::SourceState;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::hooks_install;
use crate::secrets;
use crate::writer_lock::{self, WriterLock};

pub const STORE_FILE: &str = "hook-tokens.json";
pub const LEGACY_FILE: &str = "hook.token";
pub const MAX_RETENTION_MS: i64 = 30 * 86_400_000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Previous {
    pub token: String,
    pub generation: u64,
    pub rotated_at_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HeaderState {
    Pending,
    Done,
    Failed,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Headers {
    pub generation: u64,
    pub state: HeaderState,
    pub updated_at_ms: i64,
    pub failed: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Store {
    pub version: u32,
    pub generation: u64,
    pub current: String,
    pub previous: Option<Previous>,
    pub headers: Headers,
}

pub fn fail_at(stage: &str) -> Result<()> {
    #[cfg(debug_assertions)]
    if std::env::var("ACD_TEST_FAIL_AT").is_ok_and(|s| s == stage) {
        bail!("injected failure at {stage}");
    }
    let _ = stage;
    Ok(())
}

#[allow(dead_code)] // consumed by later hook-retention and race-test tasks
pub fn pause_at(stage: &str) {
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("ACD_TEST_PAUSE_AT")
        && let Some((s, path)) = v.split_once('=')
        && s == stage
    {
        let _ = std::fs::write(format!("{path}.reached"), b"");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !Path::new(path).exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let _ = stage;
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Shared semantic validation for load and save. `file` names the store in errors.
fn validate(s: &Store, file: &Path) -> Result<()> {
    let f = file.display();
    if s.version != 1 {
        bail!(
            "{f} has unsupported version {}; run `acd init --force`",
            s.version
        );
    }
    secrets::parse_token(&s.current).with_context(|| format!("{f} holds an invalid token"))?;
    if let Some(p) = &s.previous {
        secrets::parse_token(&p.token)
            .with_context(|| format!("{f} holds an invalid previous token"))?;
        if p.generation >= s.generation {
            bail!(
                "{f} previous generation {} is not older than current generation {}",
                p.generation,
                s.generation
            );
        }
        if p.expires_at_ms < p.rotated_at_ms {
            bail!("{f} previous token expires before it was rotated");
        }
    }
    Ok(())
}

pub fn save_locked(dir: &Path, _held: &WriterLock, s: &Store) -> Result<()> {
    validate(s, &dir.join(STORE_FILE)).context("refusing to save an invalid hook token store")?;
    fail_at("store-write")?;
    secrets::write_structured(&dir.join(STORE_FILE), &serde_json::to_string_pretty(s)?)
}

fn fresh(token: String, generation: u64) -> Store {
    Store {
        version: 1,
        generation,
        current: token,
        previous: None,
        headers: Headers {
            generation,
            state: HeaderState::Done,
            updated_at_ms: now_ms(),
            failed: vec![],
        },
    }
}

/// Read-only (T6): never migrates, never locks.
pub fn load(dir: &Path) -> Result<Store> {
    let path = dir.join(STORE_FILE);
    if path.symlink_metadata().is_err() {
        if dir.join(LEGACY_FILE).symlink_metadata().is_ok() {
            bail!(
                "{} has not been migrated from hook.token yet; run `acd init` (or start acd)",
                path.display()
            );
        }
        bail!("{} is missing; run `acd init`", path.display());
    }
    let s: Store = serde_json::from_str(&secrets::read_structured(&path)?)
        .with_context(|| format!("{} is corrupt; run `acd init --force`", path.display()))?;
    validate(&s, &path)?;
    Ok(s)
}

#[allow(dead_code)] // public writer entry point; init uses ensure's locked migration
pub fn migrate(dir: &Path) -> Result<Store> {
    let held = writer_lock::acquire(dir)?;
    migrate_locked(dir, &held)
}

pub fn migrate_locked(dir: &Path, _held: &WriterLock) -> Result<Store> {
    if dir.join(STORE_FILE).symlink_metadata().is_ok() {
        return load(dir);
    }
    let legacy = dir.join(LEGACY_FILE);
    if legacy.symlink_metadata().is_err() {
        return load(dir);
    }
    let token = secrets::parse_token(&secrets::read_secret(&legacy)?)
        .context("hook.token is invalid; run `acd init --force`")?;
    let s = fresh(token, 1);
    save_locked(dir, _held, &s)?;
    std::fs::remove_file(&legacy).with_context(|| format!("removing {}", legacy.display()))?;
    Ok(s)
}

pub fn ensure(dir: &Path, force: bool) -> Result<bool> {
    let held = writer_lock::acquire(dir)?;
    ensure_locked(dir, &held, force)
}

pub fn ensure_locked(dir: &Path, held: &WriterLock, force: bool) -> Result<bool> {
    let store = dir.join(STORE_FILE).symlink_metadata().is_ok();
    let legacy = dir.join(LEGACY_FILE).symlink_metadata().is_ok();
    if (store || legacy) && !force {
        migrate_locked(dir, held)?;
        return Ok(false);
    }
    let generation = if store {
        match load(dir) {
            Ok(s) => s.generation.checked_add(1).with_context(|| {
                format!(
                    "{} generation is at its maximum; cannot advance",
                    dir.join(STORE_FILE).display()
                )
            })?,
            Err(_) => 1,
        }
    } else {
        1
    };
    save_locked(dir, held, &fresh(secrets::new_token(), generation))?;
    if legacy {
        std::fs::remove_file(dir.join(LEGACY_FILE))?;
    }
    Ok(true)
}

pub fn rotate_store_locked(dir: &Path, held: &WriterLock, now_ms: i64) -> Result<Store> {
    let old = load(dir)?;
    let generation = old.generation.checked_add(1).with_context(|| {
        format!(
            "{} generation is at its maximum; cannot rotate",
            dir.join(STORE_FILE).display()
        )
    })?;
    let s = Store {
        version: 1,
        generation,
        current: secrets::new_token(),
        previous: Some(Previous {
            token: old.current,
            generation: old.generation,
            rotated_at_ms: now_ms,
            expires_at_ms: now_ms + MAX_RETENTION_MS,
        }),
        headers: Headers {
            generation,
            state: HeaderState::Pending,
            updated_at_ms: old.headers.updated_at_ms,
            failed: vec![],
        },
    };
    save_locked(dir, held, &s)?;
    Ok(s)
}

fn update_headers_locked(
    dir: &Path,
    ctx: &hooks_install::Ctx,
    held: &WriterLock,
    flags: hooks_install::Flags,
) -> Result<hooks_install::RewriteReport> {
    let mut store = load(dir)?;
    if store.headers.state == HeaderState::Done && store.headers.generation == store.generation {
        return Ok(hooks_install::RewriteReport {
            updated: vec![],
            failed: vec![],
            recovery_note: None,
        });
    }
    let report = hooks_install::rewrite_claude_token(ctx, held, &store.current, flags)?;
    pause_at("before-headers-state");
    fail_at("before-headers-state")?;
    let completed_ms = now_ms();
    store.headers = if !report.failed.is_empty() {
        Headers {
            generation: store.generation,
            state: HeaderState::Failed,
            updated_at_ms: store.headers.updated_at_ms,
            failed: report
                .failed
                .iter()
                .map(|(path, _)| path.display().to_string())
                .collect(),
        }
    } else {
        Headers {
            generation: store.generation,
            state: HeaderState::Done,
            updated_at_ms: completed_ms,
            failed: vec![],
        }
    };
    save_locked(dir, held, &store)?;
    Ok(report)
}

/// Idempotently repair managed Claude headers. T6 entry point.
pub fn update_headers(
    dir: &Path,
    ctx: &hooks_install::Ctx,
    flags: hooks_install::Flags,
) -> Result<hooks_install::RewriteReport> {
    let held = writer_lock::acquire(dir)?;
    hooks_install::journal::recover(dir)?;
    migrate_locked(dir, &held)?;
    update_headers_locked(dir, ctx, &held, flags)
}

/// Rotates the hook credential and rewrites its managed Claude headers under
/// one writer lock. `now_ms` is the rotation instant; completion uses a later
/// clock read after the destination writes.
pub fn rotate(
    dir: &Path,
    ctx: &hooks_install::Ctx,
    flags: hooks_install::Flags,
    force: bool,
    now_ms: i64,
) -> Result<(Store, hooks_install::RewriteReport)> {
    let held = writer_lock::acquire(dir)?;
    hooks_install::journal::recover(dir)?;
    let store = migrate_locked(dir, &held)?;
    if store.headers.state != HeaderState::Done || store.headers.generation != store.generation {
        bail!("the last hook token rotation is incomplete: run `acd hooks install claude` first");
    }
    if let Some(previous) = &store.previous
        && now_ms < previous.expires_at_ms
    {
        if !force {
            bail!(
                "the previous hook token is still accepted for Claude sessions started before the last rotation (see `acd status`); rotating again cuts them off. Restart those sessions and wait, or pass --force"
            );
        }
        eprintln!(
            "warning: the token before the last rotation is no longer accepted; restart Claude sessions that still use it"
        );
    }
    hooks_install::precheck_claude(ctx, &held, flags)?;
    rotate_store_locked(dir, &held, now_ms)?;
    fail_at("after-store")?;
    let report = update_headers_locked(dir, ctx, &held, flags)?;
    Ok((load(dir)?, report))
}

/// T4: Claude writes its registry entry shortly after it starts; a scan
/// must start this long after the header update to see every session
/// that read the old headers.
pub const REGISTRY_GRACE_MS: i64 = 5_000;

#[allow(dead_code)] // used from Task 31
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveSession {
    pub session_id: String,
    pub started_at_ms: Option<i64>,
}

/// A complete registry scan (T4), with wall-clock times.
///
/// Caller contract: build one only from a scan that completed (never a
/// partial one); `live` must list every session that may still hold the
/// previous token; and the `on_current` set passed with it must be scoped
/// to the store being evaluated (cleared when the store is reset or
/// rotated). `retire_if_done` additionally requires
/// `started_at_ms <= finished_at_ms <= now`.
#[allow(dead_code)] // used from Task 31
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanView {
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub live: Vec<LiveSession>,
}

/// T4: sessions that may still carry the previous token. A session
/// observed on the current generation is released; one that started
/// after the headers were updated read the new token; the rest (older,
/// or of unknown start) are pending.
#[allow(dead_code)] // used from Task 31
pub fn pending(store: &Store, live: &[LiveSession], on_current: &HashSet<String>) -> usize {
    live.iter()
        .filter(|s| !on_current.contains(&s.session_id))
        .filter(|s| {
            s.started_at_ms
                .is_none_or(|t| t < store.headers.updated_at_ms)
        })
        .count()
}

#[allow(dead_code)] // used from Task 31
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeepReason {
    HeadersNotDone,
    NoFreshScan,
    Pending(usize),
}

#[allow(dead_code)] // used from Task 31
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Retire {
    Nothing,
    /// The store differs from the one the caller evaluated (T5).
    Moved,
    Kept(KeepReason),
    Retired,
}

/// T4, T5 and T6. The 30-day cap always wins; otherwise headers must be
/// done for the current generation and the scan must have started after
/// the header update (plus the grace) and be fresh by a clock read taken
/// after the lock is held. The store on disk must equal `expected` in
/// every field: a generation number alone proves nothing, since a forced
/// reset reuses generation 1. A write failure is an error and keeps the
/// previous token.
#[allow(dead_code)] // used from Task 31
pub fn retire_if_done(
    dir: &Path,
    expected: &Store,
    scan: Option<&ScanView>,
    fresh_within: Duration,
    on_current: &HashSet<String>,
    clock: &dyn Fn() -> i64,
) -> Result<Retire> {
    let held = writer_lock::acquire(dir)?;
    let now_ms = clock(); // after the lock: waiting may have taken long
    let mut s = load(dir)?;
    if s != *expected {
        return Ok(Retire::Moved);
    }
    let Some(prev) = s.previous.clone() else {
        return Ok(Retire::Nothing);
    };
    if now_ms < prev.expires_at_ms {
        if s.headers.state != HeaderState::Done || s.headers.generation != s.generation {
            return Ok(Retire::Kept(KeepReason::HeadersNotDone));
        }
        let fresh_ms = i64::try_from(fresh_within.as_millis()).unwrap_or(i64::MAX);
        let fresh = |sc: &&ScanView| {
            // An inverted interval, a scan finishing after the clock read
            // (a backward clock jump), or a malformed view never counts.
            sc.started_at_ms <= sc.finished_at_ms
                && sc.finished_at_ms <= now_ms
                && sc.started_at_ms >= s.headers.updated_at_ms.saturating_add(REGISTRY_GRACE_MS)
                && now_ms - sc.finished_at_ms <= fresh_ms
        };
        let Some(sc) = scan.filter(fresh) else {
            return Ok(Retire::Kept(KeepReason::NoFreshScan));
        };
        let n = pending(&s, &sc.live, on_current);
        if n > 0 {
            return Ok(Retire::Kept(KeepReason::Pending(n)));
        }
    }
    s.previous = None;
    save_locked(dir, &held, &s).with_context(|| {
        format!("could not write {STORE_FILE} to retire the previous hook token")
    })?;
    Ok(Retire::Retired)
}

#[allow(dead_code)] // used from Task 31
pub fn status_message(
    store: &Store,
    kept: Option<&KeepReason>,
    rejected: usize,
    retire_error: Option<&str>,
) -> Option<(SourceState, String)> {
    if let Some(e) = retire_error {
        return Some((
            SourceState::Degraded,
            format!("could not retire the previous hook token: {e}"),
        ));
    }
    match store.headers.state {
        HeaderState::Failed => {
            return Some((
                SourceState::Degraded,
                format!(
                    "hook token headers could not be updated in {}: run `acd hooks install claude`",
                    store.headers.failed.join(", ")
                ),
            ));
        }
        HeaderState::Pending => {
            return Some((
                SourceState::Degraded,
                "hook token rotation incomplete: run `acd hooks install claude`".into(),
            ));
        }
        HeaderState::Done => {}
    }
    if rejected > 0 {
        return Some((
            SourceState::Degraded,
            format!(
                "{rejected} Claude session(s) use a retired hook token; restart them or run /clear in them"
            ),
        ));
    }
    match kept {
        Some(KeepReason::Pending(n)) => Some((
            SourceState::Ok,
            format!(
                "hook token rotation pending: {n} Claude sessions still use the previous token"
            ),
        )),
        Some(KeepReason::NoFreshScan) => Some((
            SourceState::Ok,
            "hook token rotation waiting for a complete registry scan".into(),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dir() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        crate::secrets::ensure_private_dir(d.path()).unwrap();
        d
    }
    fn legacy_dir() -> (tempfile::TempDir, String) {
        let d = dir();
        let legacy = crate::secrets::new_token();
        crate::secrets::write_secret(&d.path().join(LEGACY_FILE), &legacy).unwrap();
        (d, legacy)
    }
    fn within_5s<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("finished without self-deadlock")
    }
    #[test]
    fn ensure_creates_generation_one_without_a_previous_token() {
        let d = dir();
        assert!(ensure(d.path(), false).unwrap());
        let s = load(d.path()).unwrap();
        assert_eq!(
            (s.generation, s.previous.is_none(), s.headers.state),
            (1, true, HeaderState::Done)
        );
        crate::secrets::parse_token(&s.current).unwrap();
        assert!(!ensure(d.path(), false).unwrap());
        assert_eq!(load(d.path()).unwrap(), s);
    }
    #[test]
    fn load_never_migrates() {
        let (d, _) = legacy_dir();
        let e = format!("{:#}", load(d.path()).unwrap_err());
        assert!(e.contains("acd init"), "{e}");
        assert!(d.path().join(LEGACY_FILE).exists());
    }
    #[test]
    fn a_legacy_hook_token_is_migrated_and_removed() {
        let (d, legacy) = legacy_dir();
        let p = d.path().to_path_buf();
        let s = within_5s(move || migrate(&p).unwrap());
        assert_eq!(s.current, legacy);
        assert!(!d.path().join(LEGACY_FILE).exists());
        assert_eq!(load(d.path()).unwrap(), s);
    }
    #[test]
    fn ensure_on_a_legacy_only_dir_does_not_reacquire_the_lock() {
        let (d, legacy) = legacy_dir();
        let p = d.path().to_path_buf();
        assert!(!within_5s(move || ensure(&p, false).unwrap()));
        assert_eq!(load(d.path()).unwrap().current, legacy);
    }
    #[test]
    fn concurrent_migrations_agree() {
        let (d, legacy) = legacy_dir();
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let p = d.path().to_path_buf();
                std::thread::spawn(move || migrate(&p).unwrap().current)
            })
            .collect();
        let tokens: Vec<String> =
            within_5s(move || handles.into_iter().map(|h| h.join().unwrap()).collect());
        assert!(tokens.iter().all(|t| *t == legacy));
    }
    #[test]
    fn structured_files_are_bounded_before_writing() {
        let d = dir();
        let p = d.path().join("big.json");
        crate::secrets::write_structured(&p, &"x".repeat(10_000)).unwrap();
        assert_eq!(crate::secrets::read_structured(&p).unwrap().len(), 10_001);
        let e =
            crate::secrets::write_structured(&p, &"y".repeat(crate::secrets::STRUCTURED_MAX + 1))
                .unwrap_err();
        assert!(format!("{e:#}").contains("too large"));
        assert_eq!(crate::secrets::read_structured(&p).unwrap().len(), 10_001);
    }
    #[test]
    fn rotation_keeps_the_previous_token_for_at_most_30_days() {
        let d = dir();
        assert!(ensure(d.path(), false).unwrap());
        let before = load(d.path()).unwrap();
        let held = crate::writer_lock::acquire(d.path()).unwrap();
        let after = rotate_store_locked(d.path(), &held, 1_000).unwrap();
        drop(held);
        assert_eq!(after.generation, before.generation + 1);
        let prev = after.previous.clone().unwrap();
        assert_eq!(
            (prev.token, prev.generation),
            (before.current, before.generation)
        );
        assert_eq!(prev.expires_at_ms, 1_000 + MAX_RETENTION_MS);
        assert_eq!(after.headers.state, HeaderState::Pending);
        assert_eq!(load(d.path()).unwrap(), after);
    }
    #[test]
    fn a_corrupt_store_is_an_error_naming_the_file() {
        let d = dir();
        crate::secrets::write_secret(&d.path().join(STORE_FILE), "{not json").unwrap();
        let e = format!("{:#}", load(d.path()).unwrap_err());
        assert!(e.contains(STORE_FILE), "{e}");
    }
    fn write_store(d: &Path, s: &Store) {
        crate::secrets::write_structured(
            &d.join(STORE_FILE),
            &serde_json::to_string_pretty(s).unwrap(),
        )
        .unwrap();
    }
    fn rotated(d: &Path) -> Store {
        ensure(d, false).unwrap();
        let held = writer_lock::acquire(d).unwrap();
        rotate_store_locked(d, &held, 1_000).unwrap()
    }
    #[test]
    fn generation_overflow_is_an_error_and_leaves_the_store_unchanged() {
        let d = dir();
        let mut s = load_after_ensure(d.path());
        s.generation = u64::MAX;
        s.headers.generation = u64::MAX;
        write_store(d.path(), &s);
        let before = std::fs::read(d.path().join(STORE_FILE)).unwrap();
        let held = writer_lock::acquire(d.path()).unwrap();
        let e = format!("{:#}", rotate_store_locked(d.path(), &held, 5).unwrap_err());
        assert!(e.contains("maximum") && e.contains(STORE_FILE), "{e}");
        drop(held);
        let e = format!("{:#}", ensure(d.path(), true).unwrap_err());
        assert!(e.contains("maximum") && e.contains(STORE_FILE), "{e}");
        assert_eq!(std::fs::read(d.path().join(STORE_FILE)).unwrap(), before);
    }
    fn load_after_ensure(d: &Path) -> Store {
        ensure(d, false).unwrap();
        load(d).unwrap()
    }
    #[test]
    fn semantic_validation_rejects_bad_stores_on_load_and_save() {
        let d = dir();
        let good = rotated(d.path());
        let held = writer_lock::acquire(d.path()).unwrap();
        let mut cases: Vec<(&str, Store)> = vec![];
        let mut s = good.clone();
        s.version = 2;
        cases.push(("unsupported version", s));
        let mut s = good.clone();
        s.previous.as_mut().unwrap().token = "bad".into();
        cases.push(("invalid previous token", s));
        let mut s = good.clone();
        s.previous.as_mut().unwrap().generation = s.generation;
        cases.push(("not older", s));
        let mut s = good.clone();
        s.previous.as_mut().unwrap().expires_at_ms = 0;
        cases.push(("expires before", s));
        for (want, s) in cases {
            let e = format!("{:#}", save_locked(d.path(), &held, &s).unwrap_err());
            assert!(
                e.contains(want) && e.contains(STORE_FILE),
                "save {want}: {e}"
            );
            assert_eq!(load(d.path()).unwrap(), good, "save left store unchanged");
            write_store(d.path(), &s);
            let e = format!("{:#}", load(d.path()).unwrap_err());
            assert!(
                e.contains(want) && e.contains(STORE_FILE),
                "load {want}: {e}"
            );
            write_store(d.path(), &good);
        }
    }

    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn live(id: &str, started: Option<i64>) -> LiveSession {
        LiveSession {
            session_id: id.into(),
            started_at_ms: started,
        }
    }

    /// Generation 2 with a previous token; headers done at `done_at` if given.
    fn rotated_at(d: &Path, done_at: Option<i64>) -> Store {
        ensure(d, false).unwrap();
        let held = crate::writer_lock::acquire(d).unwrap();
        let mut s = rotate_store_locked(d, &held, 1_000).unwrap();
        if let Some(t) = done_at {
            s.headers = Headers {
                generation: s.generation,
                state: HeaderState::Done,
                updated_at_ms: t,
                failed: vec![],
            };
            save_locked(d, &held, &s).unwrap();
        }
        s
    }

    fn scan(started: i64, finished: i64, live: Vec<LiveSession>) -> ScanView {
        ScanView {
            started_at_ms: started,
            finished_at_ms: finished,
            live,
        }
    }

    const FRESH: Duration = Duration::from_secs(60);
    const DONE: i64 = 2_000;
    const AFTER: i64 = DONE + REGISTRY_GRACE_MS; // the first scan start that counts

    fn at(t: i64) -> impl Fn() -> i64 {
        move || t
    }

    #[test]
    fn retention_excludes_only_sessions_seen_on_the_new_generation() {
        // Sol I1: a session that authenticated before the rotation stays on
        // the old token; only seeing it on the new generation releases it.
        let d = dir();
        let s = rotated_at(d.path(), Some(DONE));
        let sessions = [
            live("a", Some(500)),
            live("b", Some(3_000)),
            live("c", None),
        ];
        let mut on_new = HashSet::new();
        assert_eq!(
            pending(&s, &sessions, &on_new),
            2,
            "a (older than the headers) and c (unknown start)"
        );
        on_new.insert("a".to_string());
        assert_eq!(
            pending(&s, &sessions, &on_new),
            1,
            "a moved to the new token (for example after /clear)"
        );
    }

    #[test]
    fn retirement_needs_done_headers_and_a_complete_scan_that_started_after_them() {
        let none = HashSet::new();
        let d = dir();
        let s = rotated_at(d.path(), None); // headers still pending
        assert_eq!(
            retire_if_done(
                d.path(),
                &s,
                Some(&scan(AFTER, AFTER, vec![])),
                FRESH,
                &none,
                &at(AFTER)
            )
            .unwrap(),
            Retire::Kept(KeepReason::HeadersNotDone)
        );
        let d = dir();
        let s = rotated_at(d.path(), Some(DONE));
        let r = |sc: Option<&ScanView>, now: i64| {
            retire_if_done(d.path(), &s, sc, FRESH, &none, &at(now)).unwrap()
        };
        assert_eq!(r(None, AFTER), Retire::Kept(KeepReason::NoFreshScan));
        assert_eq!(
            r(Some(&scan(DONE - 1, AFTER, vec![])), AFTER),
            Retire::Kept(KeepReason::NoFreshScan),
            "Sol N5: a scan that started before the headers changed does not count"
        );
        assert_eq!(
            r(Some(&scan(AFTER - 1, AFTER, vec![])), AFTER),
            Retire::Kept(KeepReason::NoFreshScan),
            "a scan within the registry grace does not count either"
        );
        assert_eq!(
            r(Some(&scan(AFTER, AFTER, vec![])), AFTER + 61_000),
            Retire::Kept(KeepReason::NoFreshScan),
            "stale"
        );
        assert_eq!(
            r(Some(&scan(AFTER, AFTER, vec![live("a", Some(10))])), AFTER),
            Retire::Kept(KeepReason::Pending(1))
        );
        assert_eq!(r(Some(&scan(AFTER, AFTER, vec![])), AFTER), Retire::Retired);
        assert!(load(d.path()).unwrap().previous.is_none());
    }

    #[test]
    fn freshness_is_measured_after_the_lock_is_held() {
        // Sol N5: the scan is fresh when retirement is called, but the lock
        // is held elsewhere until it no longer is.
        let d = dir();
        let s = rotated_at(d.path(), Some(DONE));
        let clock = Arc::new(AtomicI64::new(AFTER));
        let held = crate::writer_lock::acquire(d.path()).unwrap();
        let (p, c, s2) = (d.path().to_path_buf(), clock.clone(), s.clone());
        let t = std::thread::spawn(move || {
            retire_if_done(
                &p,
                &s2,
                Some(&scan(AFTER, AFTER, vec![])),
                FRESH,
                &HashSet::new(),
                &|| c.load(Ordering::SeqCst),
            )
            .unwrap()
        });
        // Sol R3-4: advance the clock only once the worker provably waits in
        // `acquire`; a clock read before the lock would see the old time.
        let lock_path = d.path().join(crate::writer_lock::LOCK_FILE);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !crate::writer_lock::tests_support::contended(&lock_path) {
            assert!(
                std::time::Instant::now() < deadline,
                "the worker never reached the contested lock"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        clock.store(AFTER + 120_000, Ordering::SeqCst); // two minutes pass while it waits
        drop(held);
        assert_eq!(t.join().unwrap(), Retire::Kept(KeepReason::NoFreshScan));
        assert!(load(d.path()).unwrap().previous.is_some());
    }

    #[test]
    fn retirement_does_nothing_when_any_field_of_the_store_changed() {
        // Same generation, different header state: still "moved" (Sol N5).
        let d = dir();
        let s = rotated_at(d.path(), Some(DONE));
        let mut changed = s.clone();
        changed.headers.updated_at_ms = DONE + 10;
        {
            let held = crate::writer_lock::acquire(d.path()).unwrap();
            save_locked(d.path(), &held, &changed).unwrap();
        }
        assert_eq!(
            retire_if_done(
                d.path(),
                &s,
                Some(&scan(AFTER + 10, AFTER + 10, vec![])),
                FRESH,
                &HashSet::new(),
                &at(AFTER + 10)
            )
            .unwrap(),
            Retire::Moved
        );
        assert_eq!(load(d.path()).unwrap(), changed, "untouched");
    }

    #[test]
    fn a_forced_reset_that_reuses_a_generation_is_a_moved_store() {
        // `ensure --force` falls back to generation 1 when the old store
        // fails to load. Evidence gathered for the old generation 1 must
        // not retire anything in the new one.
        let d = dir();
        ensure(d.path(), false).unwrap();
        let old = load(d.path()).unwrap();
        assert_eq!(old.generation, 1);
        // A generation-1 store that carries a previous token is invalid, so
        // evaluate a rotated store, then reset it to generation 1 and make
        // the new generation-1 store look identical except for its token.
        let evaluated = rotated_at(d.path(), Some(DONE));
        std::fs::write(d.path().join(STORE_FILE), b"{corrupt").unwrap();
        assert!(ensure(d.path(), true).unwrap());
        let reset = load(d.path()).unwrap();
        assert_eq!(reset.generation, 1, "history keyed by generation is stale");
        assert_eq!(
            retire_if_done(
                d.path(),
                &evaluated,
                Some(&scan(AFTER, AFTER, vec![])),
                FRESH,
                &HashSet::new(),
                &at(AFTER)
            )
            .unwrap(),
            Retire::Moved
        );
        // Same generation number, different tokens: still moved.
        let mut lookalike = reset.clone();
        lookalike.current = crate::secrets::new_token();
        assert_eq!(
            retire_if_done(
                d.path(),
                &lookalike,
                None,
                FRESH,
                &HashSet::new(),
                &at(AFTER)
            )
            .unwrap(),
            Retire::Moved
        );
        assert_eq!(load(d.path()).unwrap(), reset, "untouched");
    }

    #[test]
    fn the_30_day_cap_always_wins() {
        let d = dir();
        let s = rotated_at(d.path(), None); // headers pending, no scan
        let later = 1_000 + MAX_RETENTION_MS;
        assert_eq!(
            retire_if_done(d.path(), &s, None, FRESH, &HashSet::new(), &at(later)).unwrap(),
            Retire::Retired
        );
        // One millisecond earlier it still holds.
        let d = dir();
        let s = rotated_at(d.path(), None);
        assert_eq!(
            retire_if_done(d.path(), &s, None, FRESH, &HashSet::new(), &at(later - 1)).unwrap(),
            Retire::Kept(KeepReason::HeadersNotDone)
        );
    }

    #[test]
    fn a_failed_retirement_is_an_error_and_keeps_the_previous_token() {
        if acd_securefs::euid() == 0 {
            eprintln!("skipped: root writes into a read-only directory anyway");
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let d = dir();
        let s = rotated_at(d.path(), Some(DONE));
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let r = retire_if_done(
            d.path(),
            &s,
            Some(&scan(AFTER, AFTER, vec![])),
            FRESH,
            &HashSet::new(),
            &at(AFTER),
        );
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let e = format!("{:#}", r.unwrap_err());
        assert!(
            e.contains("could not write") && e.contains(STORE_FILE),
            "{e}"
        );
        assert!(load(d.path()).unwrap().previous.is_some());
    }

    #[test]
    fn impossible_scan_times_never_make_a_scan_eligible() {
        let d = dir();
        let s = rotated_at(d.path(), Some(DONE));
        let before = std::fs::read(d.path().join(STORE_FILE)).unwrap();
        let none = HashSet::new();
        let r = |sc: ScanView, now: i64| {
            retire_if_done(d.path(), &s, Some(&sc), FRESH, &none, &at(now)).unwrap()
        };
        let kept = Retire::Kept(KeepReason::NoFreshScan);
        assert_eq!(
            r(scan(AFTER, AFTER + 10_000, vec![]), AFTER),
            kept,
            "completion in the future"
        );
        assert_eq!(
            r(scan(AFTER + 5, AFTER, vec![]), AFTER + 5),
            kept,
            "inverted interval"
        );
        assert_eq!(
            r(scan(AFTER, AFTER + 1_000, vec![]), AFTER - 100_000),
            kept,
            "clock rollback"
        );
        assert_eq!(std::fs::read(d.path().join(STORE_FILE)).unwrap(), before);
        assert!(load(d.path()).unwrap().previous.is_some());
    }

    #[test]
    fn status_messages() {
        let d = dir();
        let s = rotated_at(d.path(), Some(DONE));
        let (st, m) = status_message(&s, Some(&KeepReason::Pending(2)), 0, None).unwrap();
        assert_eq!(st, SourceState::Ok);
        assert!(m.contains("2 Claude sessions"), "{m}");
        let (st, m) = status_message(&s, Some(&KeepReason::NoFreshScan), 0, None).unwrap();
        assert_eq!(st, SourceState::Ok);
        assert!(m.contains("waiting for a complete registry scan"), "{m}");
        let (st, m) = status_message(&s, None, 3, None).unwrap();
        assert_eq!(st, SourceState::Degraded);
        assert!(m.contains("retired hook token"), "{m}");
        let (st, m) = status_message(&s, None, 0, Some("disk full")).unwrap();
        assert_eq!(st, SourceState::Degraded);
        assert!(m.contains("could not retire"), "{m}");
        let mut failed = s.clone();
        failed.headers.state = HeaderState::Failed;
        let (st, m) = status_message(&failed, None, 0, None).unwrap();
        assert_eq!(st, SourceState::Degraded);
        assert!(m.contains("acd hooks install claude"), "{m}");
        let mut done = s.clone();
        done.previous = None;
        assert_eq!(status_message(&done, None, 0, None), None);
    }
}
