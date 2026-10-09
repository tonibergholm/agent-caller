//! The hook token store: `hook-tokens.json` is the single source of truth
//! and the rotation transaction. Later tasks add retention and header rewrites.
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

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

pub fn rotate_store(dir: &Path, now_ms: i64) -> Result<Store> {
    let held = writer_lock::acquire(dir)?;
    rotate_store_locked(dir, &held, now_ms)
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
        rotate_store(d, 1_000).unwrap()
    }
    #[test]
    fn generation_overflow_is_an_error_and_leaves_the_store_unchanged() {
        let d = dir();
        let mut s = load_after_ensure(d.path());
        s.generation = u64::MAX;
        s.headers.generation = u64::MAX;
        write_store(d.path(), &s);
        let before = std::fs::read(d.path().join(STORE_FILE)).unwrap();
        let e = format!("{:#}", rotate_store(d.path(), 5).unwrap_err());
        assert!(e.contains("maximum") && e.contains(STORE_FILE), "{e}");
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
}
