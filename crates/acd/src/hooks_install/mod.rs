//! `acd hooks install|uninstall claude|codex` settings primitives.
mod entries;
mod git;
mod journal;
mod json;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use self::entries::Ownership;
use self::journal::{Journal, digest};
use self::json::{GroupEdit, Obj, edit_group, raw, raw_text, render_array};
use crate::secrets::CommitError;

pub const MARKER: &str = "acd-managed:v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Claude,
    Codex,
}

pub use crate::writer_lock::{WriterLock, acquire as lock_writers};

pub const RECORD_FILE: &str = "hooks-installed.json";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FileRecord {
    pub target: Target,
    pub created_file: bool,
    pub created_hooks_key: bool,
    pub created_events: Vec<String>,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Record {
    pub version: u32,
    pub files: BTreeMap<PathBuf, FileRecord>,
}

impl Record {
    pub fn load(dir: &Path) -> Result<Record> {
        let p = dir.join(RECORD_FILE);
        if p.symlink_metadata().is_err() {
            return Ok(Record {
                version: 2,
                ..Default::default()
            });
        }
        Ok(serde_json::from_str(&crate::secrets::read_structured(&p)?)?)
    }
    pub fn save(&self, dir: &Path) -> Result<()> {
        inject("record-write")?;
        crate::secrets::write_structured(
            &dir.join(RECORD_FILE),
            &serde_json::to_string_pretty(self)?,
        )
    }
    pub fn claude_destinations(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter(|(_, r)| r.target == Target::Claude)
            .map(|(p, _)| p.clone())
            .collect()
    }
}

#[allow(dead_code)]
pub fn recorded_claude_roots(config_dir: &Path) -> Vec<PathBuf> {
    Record::load(config_dir)
        .map(|r| {
            r.claude_destinations()
                .iter()
                .filter_map(|p| p.parent().map(|d| d.join("sessions")))
                .collect()
        })
        .unwrap_or_default()
}

/// Resolves the parent directory once. Every later step (Git check, backup,
/// journal, staging, rename) uses the returned path, so a parent symlink
/// retargeted afterwards cannot move the write. The parent must be a real
/// directory owned by the user.
fn canonical_destination(path: &Path, create: bool) -> Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let parent = path.parent().context("settings path has no parent")?;
    if create {
        std::fs::create_dir_all(parent)?;
    }
    let parent = std::fs::canonicalize(parent)
        .with_context(|| format!("resolving the directory of {}", path.display()))?;
    let meta = std::fs::symlink_metadata(&parent)?;
    if !meta.is_dir() || meta.uid() != acd_securefs::euid() {
        bail!(
            "refusing to edit in {}: not a directory owned by you",
            parent.display()
        );
    }
    Ok(parent.join(path.file_name().context("settings path has no file name")?))
}

pub struct Report {
    pub path: PathBuf,
    pub backup: Option<PathBuf>,
    pub removed_file: bool,
    pub suspicious: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Flags {
    pub allow_tracked: bool,
    pub allow_unknown_git: bool,
}

fn strip_owned(hooks: &mut Obj) -> Result<usize> {
    let mut suspicious = 0;
    for (event, groups_raw) in hooks.0.iter_mut() {
        let Ok(groups) = serde_json::from_str::<Vec<Box<RawValue>>>(groups_raw.get()) else {
            continue;
        };
        let mut out = Vec::new();
        let mut changed = false;
        for g in groups {
            let v: Value = serde_json::from_str(g.get())?;
            suspicious += v["hooks"].as_array().map_or(0, |a| {
                a.iter()
                    .filter(|e| entries::classify(event, e) == Ownership::Suspicious)
                    .count()
            });
            match edit_group(&g, |e| entries::classify(event, e) == Ownership::Owned)? {
                GroupEdit::Keep => out.push(g),
                GroupEdit::Drop => changed = true,
                GroupEdit::Rewrite(r) => {
                    changed = true;
                    out.push(r);
                }
            }
        }
        if changed {
            *groups_raw = raw_text(render_array(&out))?;
        }
    }
    Ok(suspicious)
}

fn groups_for(ctx: &Ctx, held: &WriterLock, t: Target) -> Result<Vec<(String, Value)>> {
    Ok(match t {
        Target::Claude => {
            let token = crate::hook_token::migrate_locked(&ctx.config_dir, held)?.current;
            entries::claude_events(ctx.permission_hold)
                .into_iter()
                .map(|(e, m, t)| (e.to_string(), entries::claude_group(ctx, &token, e, m, t)))
                .collect()
        }
        Target::Codex => entries::CODEX_EVENTS
            .iter()
            .map(|(e, t)| (e.to_string(), entries::codex_group(ctx, e, *t)))
            .collect(),
    })
}

pub(crate) fn read_settings(path: &Path) -> Result<Option<String>> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            bail!("refusing to edit {}: it is a symlink", path.display())
        }
        Ok(m) if m.uid() != acd_securefs::euid() => bail!(
            "refusing to edit {}: it is not owned by you",
            path.display()
        ),
        Ok(m) if !m.is_file() => bail!("refusing to edit {}: not a regular file", path.display()),
        Ok(_) => Ok(Some(std::fs::read_to_string(path)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// A private copy of the pre-edit text. Names are unique and never reused:
/// `create_new` refuses an existing file or symlink.
fn backup(ctx: &Ctx, path: &Path, text: &str) -> Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = ctx.config_dir.join("backups");
    crate::secrets::ensure_private_dir(&dir)?;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    let mut rnd = [0u8; 6];
    getrandom::fill(&mut rnd).expect("OS randomness is available");
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let b = dir.join(format!(
        "{name}.{ms}.{}.bak",
        rnd.iter().map(|x| format!("{x:02x}")).collect::<String>()
    ));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&b)
        .with_context(|| format!("creating {}", b.display()))?;
    let written = f.write_all(text.as_bytes()).and_then(|()| f.sync_all());
    if let Err(e) = written {
        let _ = std::fs::remove_file(&b);
        return Err(e.into());
    }
    Ok(b)
}

fn file_mode(path: &Path, t: Target) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    match t {
        Target::Claude => 0o600,
        Target::Codex => std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0o600),
    }
}

fn test_touch(path: &Path, attempt: usize) {
    #[cfg(debug_assertions)]
    if attempt == 0
        && let Ok(content) = std::env::var("ACD_TEST_TOUCH_SETTINGS")
    {
        let _ = std::fs::write(path, content);
    }
    let _ = (path, attempt);
}

/// Debug builds only: `ACD_TEST_RUN_AT=<stage>=<shell command>` runs the
/// command when the installer reaches `stage`.
fn test_stage(stage: &str) {
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("ACD_TEST_RUN_AT")
        && let Some((s, cmd)) = v.split_once('=')
        && s == stage
    {
        let _ = std::process::Command::new("sh").arg("-c").arg(cmd).status();
    }
    let _ = stage;
}

#[cfg(test)]
thread_local! {
    static INJECTED: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Failure injection: `ACD_TEST_FAIL_AT` in debug builds, or the
/// thread-local list in unit tests.
fn inject(stage: &str) -> Result<()> {
    #[cfg(test)]
    if INJECTED.with(|i| i.borrow().contains(&stage)) {
        bail!("injected failure at {stage}");
    }
    crate::hook_token::fail_at(stage)
}

const MAX_ATTEMPTS: usize = 3;

/// Checks made after the temp file is staged and the journal exists, as late
/// as possible before the rename. `Ok(false)` means the file changed since it
/// was read, so the caller retries the whole edit.
fn verify_before_commit(
    dest: &Path,
    before: Option<&str>,
    t: Target,
    flags: Flags,
) -> Result<bool> {
    if t == Target::Claude {
        git::check(dest, flags.allow_tracked, flags.allow_unknown_git)?;
    }
    Ok(read_settings(dest)?.as_deref() == before)
}

/// Edits the settings file at `dest`, which the caller resolved once with
/// `canonical_destination`.
fn edit_settings(
    ctx: &Ctx,
    t: Target,
    dest: &Path,
    flags: Flags,
    record_after: impl Fn(Option<&str>, &Obj) -> Option<FileRecord>,
    edit: impl Fn(&mut Obj) -> Result<usize>,
) -> Result<Report> {
    let mut backed_up: Option<(Option<String>, PathBuf)> = None;
    for attempt in 0..MAX_ATTEMPTS {
        let before = read_settings(dest)?;
        if t == Target::Claude {
            git::check(dest, flags.allow_tracked, flags.allow_unknown_git)?;
        }
        let mut doc = Obj::parse(before.as_deref().unwrap_or_default())
            .with_context(|| format!("{} is not a JSON object", dest.display()))?;
        let suspicious = edit(&mut doc)?;
        let rec = record_after(before.as_deref(), &doc);
        let remove = doc.is_empty()
            && rec.is_none()
            && Record::load(&ctx.config_dir)?
                .files
                .get(dest)
                .is_some_and(|r| r.created_file);
        let text = if remove {
            None
        } else {
            Some(format!("{}\n", doc.render()))
        };
        // Stage first: the temp file is written and fsynced before the last
        // content check, so that check sits right next to the rename.
        let staged = text
            .as_deref()
            .map(|tx| crate::secrets::stage_atomic(dest, tx.as_bytes(), file_mode(dest, t)))
            .transpose()?;
        test_touch(dest, attempt);
        test_stage("after-stage");
        let before_digest = digest(before.as_deref());
        let backup = match (&before, &backed_up) {
            (None, _) => None,
            (Some(_), Some((d, b))) if *d == before_digest => Some(b.clone()),
            (Some(b), _) => {
                let made = backup(ctx, dest, b)?;
                backed_up = Some((before_digest.clone(), made.clone()));
                Some(made)
            }
        };
        journal::begin(
            &ctx.config_dir,
            &Journal {
                destination: dest.to_path_buf(),
                before: before_digest,
                after: digest(text.as_deref()),
                record_after: rec.clone(),
            },
        )?;
        // Until the rename the journal is only a promise: any early exit
        // removes it, and dropping `staged` removes the temp file.
        match verify_before_commit(dest, before.as_deref(), t, flags) {
            Ok(true) => {}
            Ok(false) => {
                journal::finish(&ctx.config_dir)?;
                continue;
            }
            Err(e) => {
                journal::finish(&ctx.config_dir)?;
                return Err(e);
            }
        }
        let committed = match staged {
            Some(s) => inject("settings-write")
                .map_err(CommitError::not_renamed)
                .and_then(|()| s.commit())
                .and_then(|()| {
                    inject("settings-post-rename").map_err(|error| CommitError {
                        renamed: true,
                        error,
                    })
                }),
            None => inject("settings-write")
                .and_then(|()| std::fs::remove_file(dest).map_err(Into::into))
                .map_err(CommitError::not_renamed),
        };
        if let Err(e) = committed {
            if e.renamed {
                // The file may already hold the new text: keep the journal.
                return Err(e.error.context(format!(
                    "{} may have been replaced; the journal was kept and the next acd hooks command recovers it",
                    dest.display()
                )));
            }
            journal::finish(&ctx.config_dir)?;
            return Err(e.error);
        }
        let mut record = Record::load(&ctx.config_dir)?;
        match rec {
            Some(r) => {
                record.files.insert(dest.to_path_buf(), r);
            }
            None => {
                record.files.remove(dest);
            }
        }
        record.save(&ctx.config_dir)?;
        journal::finish(&ctx.config_dir)?;
        return Ok(Report {
            path: dest.to_path_buf(),
            backup,
            removed_file: text.is_none(),
            suspicious,
        });
    }
    bail!(
        "{} kept changing while acd edited it; try again",
        dest.display()
    )
}

pub fn install(ctx: &Ctx, t: Target, flags: Flags) -> Result<Report> {
    install_entries(ctx, t, flags)
}

fn install_entries(ctx: &Ctx, t: Target, flags: Flags) -> Result<Report> {
    let held = lock_writers(&ctx.config_dir)?;
    journal::recover(&ctx.config_dir)?;
    let path = ctx.settings_path(t);
    let dest = canonical_destination(&path, true)?;
    let groups = groups_for(ctx, &held, t)?;
    let prior = Record::load(&ctx.config_dir)?.files.get(&dest).cloned();
    let created = std::cell::RefCell::new((false, Vec::<String>::new()));
    edit_settings(
        ctx,
        t,
        &dest,
        flags,
        |before, _| {
            let (hooks_key, events) = created.borrow().clone();
            let mut r = prior.clone().unwrap_or(FileRecord {
                target: t,
                created_file: before.is_none(),
                created_hooks_key: false,
                created_events: vec![],
            });
            r.created_hooks_key |= hooks_key;
            for e in events {
                if !r.created_events.contains(&e) {
                    r.created_events.push(e);
                }
            }
            Some(r)
        },
        |doc| {
            let mut c = created.borrow_mut();
            *c = (false, Vec::new());
            let mut hooks = match doc.get("hooks") {
                Some(h) => Obj::parse(h.get())?,
                None => {
                    c.0 = true;
                    Obj::default()
                }
            };
            let suspicious = strip_owned(&mut hooks)?;
            for (event, group) in &groups {
                let mut items: Vec<Box<RawValue>> = match hooks.get(event) {
                    Some(a) => serde_json::from_str(a.get())
                        .with_context(|| format!("hooks.{event} is not an array"))?,
                    None => {
                        c.1.push(event.clone());
                        Vec::new()
                    }
                };
                items.push(raw(group));
                hooks.set(event, raw_text(render_array(&items))?);
            }
            doc.set("hooks", raw_text(hooks.render())?);
            Ok(suspicious)
        },
    )
}

pub fn uninstall(ctx: &Ctx, t: Target) -> Result<Report> {
    let _lock = lock_writers(&ctx.config_dir)?;
    journal::recover(&ctx.config_dir)?;
    let path = ctx.settings_path(t);
    let dest = canonical_destination(&path, true)?;
    let rec = Record::load(&ctx.config_dir)?.files.get(&dest).cloned();
    let events = rec
        .as_ref()
        .map(|r| r.created_events.clone())
        .unwrap_or_default();
    let created_hooks = rec.as_ref().is_some_and(|r| r.created_hooks_key);
    edit_settings(
        ctx,
        t,
        &dest,
        Flags {
            allow_tracked: true,
            allow_unknown_git: true,
        },
        |_, _| None,
        |doc| {
            let Some(h) = doc.get("hooks") else {
                return Ok(0);
            };
            let mut hooks = Obj::parse(h.get())?;
            let suspicious = strip_owned(&mut hooks)?;
            for event in &events {
                if hooks.get(event).is_some_and(|a| {
                    serde_json::from_str::<Vec<Value>>(a.get()).is_ok_and(|v| v.is_empty())
                }) {
                    hooks.remove(event);
                }
            }
            if hooks.is_empty() && created_hooks {
                doc.remove("hooks");
            } else {
                doc.set("hooks", raw_text(hooks.render())?);
            }
            Ok(suspicious)
        },
    )
}

pub struct RewriteReport {
    pub updated: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

/// Replaces the token in every owned entry of one event's groups. Only
/// groups holding such an entry are rewritten; every other group, and every
/// foreign entry inside a rewritten one, keeps its bytes. Returns the new
/// array text when something changed.
fn rewrite_event_token(
    event: &str,
    groups_raw: &RawValue,
    token: &str,
    suspicious: &mut usize,
) -> Result<Option<Box<RawValue>>> {
    let Ok(groups) = serde_json::from_str::<Vec<Box<RawValue>>>(groups_raw.get()) else {
        return Ok(None);
    };
    let mut out = Vec::with_capacity(groups.len());
    let mut changed = false;
    for g in groups {
        let value: Value = serde_json::from_str(g.get())?;
        let Some(items) = value.get("hooks").and_then(Value::as_array) else {
            out.push(g);
            continue;
        };
        let owned: Vec<bool> = items
            .iter()
            .map(|e| match entries::classify(event, e) {
                Ownership::Owned => e.get("headers").is_some(),
                Ownership::Suspicious => {
                    *suspicious += 1;
                    false
                }
                Ownership::Foreign => false,
            })
            .collect();
        if !owned.contains(&true) {
            out.push(g);
            continue;
        }
        let mut group = Obj::parse(g.get())?;
        let list = group.get("hooks").context("group lost its hooks")?;
        let raw_items: Vec<Box<RawValue>> = serde_json::from_str(list.get())?;
        let mut rebuilt = Vec::with_capacity(raw_items.len());
        for (item, is_owned) in raw_items.into_iter().zip(&owned) {
            if !is_owned {
                rebuilt.push(item);
                continue;
            }
            let mut entry = Obj::parse(item.get())?;
            let headers = entry.get("headers").context("entry lost its headers")?;
            let mut headers = Obj::parse(headers.get())?;
            headers.set("X-Acd-Token", raw(&Value::String(token.to_owned())));
            entry.set("headers", raw_text(headers.render())?);
            rebuilt.push(raw_text(entry.render())?);
        }
        group.set("hooks", raw_text(render_array(&rebuilt))?);
        out.push(raw_text(group.render())?);
        changed = true;
    }
    changed.then(|| raw_text(render_array(&out))).transpose()
}

#[allow(dead_code)]
pub fn rewrite_claude_token(
    ctx: &Ctx,
    _held: &WriterLock,
    token: &str,
    flags: Flags,
) -> Result<RewriteReport> {
    journal::recover(&ctx.config_dir)?;
    let mut report = RewriteReport {
        updated: vec![],
        failed: vec![],
    };
    let destinations = Record::load(&ctx.config_dir)?.claude_destinations();
    let journal_path = ctx.config_dir.join(journal::JOURNAL_FILE);
    for (i, recorded) in destinations.iter().enumerate() {
        let result = canonical_destination(recorded, false).and_then(|dest| {
            if dest != *recorded {
                bail!("the recorded destination now resolves to {}", dest.display());
            }
            edit_settings(
                ctx,
                Target::Claude,
                &dest,
                flags,
                |_, _| {
                    Record::load(&ctx.config_dir)
                        .ok()
                        .and_then(|r| r.files.get(&dest).cloned())
                },
                |doc| {
                    let Some(h) = doc.get("hooks") else {
                        return Ok(0);
                    };
                    let mut hooks = Obj::parse(h.get())?;
                    let mut suspicious = 0;
                    let mut touched = false;
                    for (event, groups_raw) in hooks.0.iter_mut() {
                        if let Some(new) =
                            rewrite_event_token(event, groups_raw, token, &mut suspicious)?
                        {
                            *groups_raw = new;
                            touched = true;
                        }
                    }
                    if suspicious > 0 {
                        bail!(
                            "{suspicious} unexpected entries carry the acd marker; remove them by hand"
                        );
                    }
                    if touched {
                        doc.set("hooks", raw_text(hooks.render())?);
                    }
                    Ok(0)
                },
            )
        });
        match result {
            Ok(_) => report.updated.push(recorded.clone()),
            Err(e) => {
                report.failed.push((recorded.clone(), format!("{e:#}")));
                // J4: a failure that leaves a journal stops the batch, so
                // the journal is never overwritten and the evidence stays.
                if journal_path.symlink_metadata().is_ok() {
                    for rest in &destinations[i + 1..] {
                        report.failed.push((
                            rest.clone(),
                            "not attempted: an earlier destination left a journal".into(),
                        ));
                    }
                    break;
                }
            }
        }
    }
    Ok(report)
}

#[allow(dead_code)]
pub struct Ctx {
    pub home: PathBuf,
    pub claude_dir: PathBuf,
    pub codex_dir: PathBuf,
    pub config_dir: PathBuf,
    pub exe: PathBuf,
    pub hook_port: u16,
    pub permission_hold: u64,
}

impl Ctx {
    pub fn from_env(config_dir: &Path) -> Result<Ctx> {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        let cfg = crate::config::Config::load(config_dir)?;
        let env_dir = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Ok(Ctx {
            claude_dir: env_dir("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude")),
            codex_dir: env_dir("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
            home,
            config_dir: std::fs::canonicalize(config_dir)?,
            exe: std::fs::canonicalize(std::env::current_exe()?)?,
            hook_port: cfg.hook_port,
            permission_hold: cfg.claude.permission_hold,
        })
    }

    pub fn settings_path(&self, target: Target) -> PathBuf {
        match target {
            Target::Claude => self.claude_dir.join("settings.json"),
            Target::Codex => self.codex_dir.join("hooks.json"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _root: tempfile::TempDir,
        root: PathBuf,
        ctx: Ctx,
    }

    fn fixture() -> Fixture {
        let t = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(t.path()).unwrap();
        let cfg = root.join("cfg");
        crate::secrets::ensure_private_dir(&cfg).unwrap();
        crate::hook_token::ensure(&cfg, false).unwrap();
        let ctx = Ctx {
            home: root.clone(),
            claude_dir: root.join("claude-a"),
            codex_dir: root.join("codex"),
            config_dir: cfg,
            exe: PathBuf::from("/bin/acd"),
            hook_port: 47999,
            permission_hold: 3600,
        };
        Fixture {
            _root: t,
            root,
            ctx,
        }
    }

    const FLAGS: Flags = Flags {
        allow_tracked: false,
        allow_unknown_git: true,
    };

    fn install_into(f: &mut Fixture, dir: &str, original: Option<&str>) -> PathBuf {
        f.ctx.claude_dir = f.root.join(dir);
        let path = f.ctx.settings_path(Target::Claude);
        if let Some(text) = original {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        }
        install(&f.ctx, Target::Claude, FLAGS).unwrap();
        std::fs::canonicalize(path).unwrap()
    }

    #[test]
    fn a_batch_stops_at_the_first_failure_that_leaves_a_journal() {
        let mut f = fixture();
        let a = install_into(&mut f, "claude-a", None);
        let b = install_into(&mut f, "claude-b", None);
        let old_a = std::fs::read_to_string(&a).unwrap();
        let old_b = std::fs::read_to_string(&b).unwrap();
        let held = lock_writers(&f.ctx.config_dir).unwrap();
        INJECTED.with(|i| i.borrow_mut().push("record-write"));
        let report = rewrite_claude_token(&f.ctx, &held, "new-token", FLAGS).unwrap();
        INJECTED.with(|i| i.borrow_mut().clear());

        let (first, second) = if a < b { (&a, &b) } else { (&b, &a) };
        assert!(report.updated.is_empty());
        assert_eq!(report.failed.len(), 2, "{:?}", report.failed);
        assert!(
            report.failed[0]
                .1
                .contains("injected failure at record-write")
        );
        assert!(report.failed[1].1.contains("not attempted"));
        let old_of = |p: &PathBuf| if p == &a { &old_a } else { &old_b };
        assert_eq!(
            std::fs::read_to_string(second).unwrap(),
            *old_of(second),
            "the second destination is untouched"
        );
        assert!(
            std::fs::read_to_string(first)
                .unwrap()
                .contains("new-token")
        );
        let j: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(f.ctx.config_dir.join(journal::JOURNAL_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            j["destination"],
            first.to_str().unwrap(),
            "the first journal is intact"
        );
    }

    #[test]
    fn a_token_rewrite_keeps_foreign_bytes() {
        let mut f = fixture();
        let foreign_a = r#"{"type":"command",   "command":"keep-me", "n": 1, "n": 2}"#;
        let foreign_b =
            r#"{ "matcher" : "Bash",  "hooks":[ {"type":"command","command":"guard"} ] }"#;
        let original = format!(
            "{{\n \"hooks\": {{\n  \"Stop\": [{{\"hooks\": [{foreign_a}]}}],\n  \"PreToolUse\": [{foreign_b}]\n }},\n \"model\": \"opus\"\n}}\n"
        );
        let path = install_into(&mut f, "claude-a", Some(&original));
        let held = lock_writers(&f.ctx.config_dir).unwrap();
        let report = rewrite_claude_token(&f.ctx, &held, "fresh-token", FLAGS).unwrap();
        assert_eq!(report.updated, vec![path.clone()], "{:?}", report.failed);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(foreign_a), "{text}");
        assert!(text.contains(foreign_b), "{text}");
        assert_eq!(text.matches("fresh-token").count(), 7);
        assert!(text.contains(" \"model\": \"opus\""), "{text}");
    }
}
