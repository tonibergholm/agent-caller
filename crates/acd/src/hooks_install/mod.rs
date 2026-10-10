//! `acd hooks install|uninstall claude|codex` settings primitives.
mod entries;
mod git;
pub(crate) mod journal;
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
    /// Claude settings files whose install record was removed, with the
    /// token store's generation at that moment (`u64::MAX` when the store
    /// could not be read). Sessions started before the removal keep the
    /// headers they read, so their registry root is scanned while the
    /// previous token is that generation or older (Sol final F4). Pruned
    /// once no accepted token is that old.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub uninstalled_claude: BTreeMap<PathBuf, u64>,
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
    /// Sets (`Some`) or removes (`None`) the install record of `dest`. The
    /// caller holds the writer lock. A removed Claude record moves to
    /// `uninstalled_claude` (F4).
    pub fn set(&mut self, config_dir: &Path, dest: &Path, rec: Option<FileRecord>) {
        let store = crate::hook_token::load(config_dir).ok();
        match rec {
            Some(r) => {
                self.uninstalled_claude.remove(dest);
                self.files.insert(dest.to_path_buf(), r);
            }
            None => {
                if self
                    .files
                    .remove(dest)
                    .is_some_and(|r| r.target == Target::Claude)
                {
                    let generation = store.as_ref().map_or(u64::MAX, |s| s.generation);
                    self.uninstalled_claude
                        .insert(dest.to_path_buf(), generation);
                }
            }
        }
        if let Some(s) = &store {
            let oldest = s.previous.as_ref().map_or(s.generation, |p| p.generation);
            self.uninstalled_claude.retain(|_, g| *g >= oldest);
        }
    }
}

/// The registry roots (`<root>/sessions`) of recorded Claude installs and
/// of uninstalled ones whose sessions may still hold the previous token
/// (F4). An error when the install records cannot be read: the caller must
/// then treat any scan as incomplete. An unreadable token store keeps every
/// uninstalled root.
pub fn recorded_claude_roots(config_dir: &Path) -> Result<Vec<PathBuf>> {
    let record = Record::load(config_dir)?;
    let previous = crate::hook_token::load(config_dir)
        .ok()
        .map(|s| s.previous.map(|p| p.generation));
    let mut destinations = record.claude_destinations();
    destinations.extend(
        record
            .uninstalled_claude
            .iter()
            .filter(|(_, g)| match previous {
                None => true,
                Some(None) => false,
                Some(Some(p)) => **g >= p,
            })
            .map(|(p, _)| p.clone()),
    );
    let mut roots: Vec<PathBuf> = Vec::new();
    for root in destinations
        .iter()
        .filter_map(|p| p.parent().map(|d| d.join("sessions")))
    {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    Ok(roots)
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
    /// Set when recovery rebuilt an install record before this command ran.
    pub recovery_note: Option<String>,
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

/// Reads `name` relative to the pinned directory, with the same refusals as
/// `read_settings`. Also returns the file's permission bits.
fn read_pinned(
    dir: &crate::secrets::DirFd,
    name: &std::ffi::OsStr,
) -> Result<Option<(String, u32)>> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let shown = dir.path().join(name);
    let mut f = match dir.open_nofollow(name) {
        Ok(None) => return Ok(None),
        Ok(Some(f)) => f,
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            bail!("refusing to edit {}: it is a symlink", shown.display())
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", shown.display())),
    };
    let m = f.metadata()?;
    if m.uid() != acd_securefs::euid() {
        bail!(
            "refusing to edit {}: it is not owned by you",
            shown.display()
        );
    }
    if !m.is_file() {
        bail!("refusing to edit {}: not a regular file", shown.display());
    }
    let mut text = String::new();
    f.read_to_string(&mut text)
        .with_context(|| format!("reading {}", shown.display()))?;
    Ok(Some((text, m.permissions().mode() & 0o7777)))
}

/// `read_settings` for a path, but through the same pinned-directory,
/// no-follow, descriptor-validated read the writes use. A missing directory
/// reads as a missing file.
fn read_settings_pinned(path: &Path) -> Result<Option<String>> {
    let parent = path.parent().context("settings path has no parent")?;
    let name = path.file_name().context("settings path has no file name")?;
    let dir = match crate::secrets::DirFd::open(parent, true) {
        Ok(d) => d,
        Err(_)
            if matches!(std::fs::symlink_metadata(parent),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    Ok(read_pinned(&dir, name)?.map(|(text, _)| text))
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

fn file_mode(existing: Option<u32>, t: Target) -> u32 {
    match t {
        Target::Claude => 0o600,
        Target::Codex => existing.map_or(0o600, |m| m & 0o777),
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
/// was read, so the caller retries the whole edit. Git is asked by path (it
/// cannot take a descriptor); the content compare uses the pinned directory.
fn verify_before_commit(
    dir: &crate::secrets::DirFd,
    name: &std::ffi::OsStr,
    before: Option<&str>,
    t: Target,
    flags: Flags,
) -> Result<bool> {
    if t == Target::Claude {
        git::check(
            &dir.path().join(name),
            flags.allow_tracked,
            flags.allow_unknown_git,
        )?;
    }
    Ok(read_pinned(dir, name)?.map(|(text, _)| text).as_deref() == before)
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
    // Pinned once: staging, compare, rename, unlink and fsync all go through
    // this descriptor, so replacing the directory's path cannot redirect them.
    let dir =
        crate::secrets::DirFd::open(dest.parent().context("settings path has no parent")?, true)?;
    let name = dest.file_name().context("settings path has no file name")?;
    let mut backed_up: Option<(Option<String>, PathBuf)> = None;
    for attempt in 0..MAX_ATTEMPTS {
        let (before, existing_mode) = match read_pinned(&dir, name)? {
            Some((text, mode)) => (Some(text), Some(mode)),
            None => (None, None),
        };
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
            // `render` already ends in a newline when the input did; adding
            // another on every pass would make a re-install never a no-op.
            let mut rendered = doc.render();
            if !rendered.ends_with('\n') {
                rendered.push('\n');
            }
            Some(rendered)
        };
        let current_record = Record::load(&ctx.config_dir)?.files.get(dest).cloned();
        // A Claude file must also already be exactly 0600: a loosened mode
        // (or a setuid/setgid/sticky bit) is rewritten through the pinned dir.
        let mode_ok = t != Target::Claude || existing_mode.is_none_or(|m| m == 0o600);
        if mode_ok && text.as_deref() == before.as_deref() && rec == current_record {
            return Ok(Report {
                path: dest.to_path_buf(),
                backup: None,
                removed_file: false,
                suspicious,
                recovery_note: None,
            });
        }
        // Stage first: the temp file is written and fsynced before the last
        // content check, so that check sits right next to the rename.
        let staged = text
            .as_deref()
            .map(|tx| dir.stage(name, tx.as_bytes(), file_mode(existing_mode, t)))
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
                target: t,
                conflict: None,
            },
        )?;
        // Until the rename the journal is only a promise: any early exit
        // removes it, and dropping `staged` removes the temp file.
        match verify_before_commit(&dir, name, before.as_deref(), t, flags) {
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
            // B: the deletion is durable only once the directory is synced;
            // until then (or if the sync fails) the journal stays.
            None => inject("settings-write")
                .and_then(|()| dir.unlink(name))
                .map_err(CommitError::not_renamed)
                .and_then(|()| {
                    dir.sync()
                        .map_err(anyhow::Error::from)
                        .and_then(|()| inject("settings-post-rename"))
                        .map_err(|error| CommitError {
                            renamed: true,
                            error,
                        })
                }),
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
        record.set(&ctx.config_dir, dest, rec);
        record.save(&ctx.config_dir)?;
        journal::finish(&ctx.config_dir)?;
        return Ok(Report {
            path: dest.to_path_buf(),
            backup,
            removed_file: text.is_none(),
            suspicious,
            recovery_note: None,
        });
    }
    bail!(
        "{} kept changing while acd edited it; try again",
        dest.display()
    )
}

pub fn install(ctx: &Ctx, t: Target, flags: Flags) -> Result<Report> {
    let report = install_entries(ctx, t, flags)?;
    if t == Target::Claude {
        let rewritten = crate::hook_token::update_headers(&ctx.config_dir, ctx, flags)?;
        for (path, why) in &rewritten.failed {
            eprintln!(
                "warning: could not update the hook token in {}: {why}",
                path.display()
            );
        }
    }
    Ok(report)
}

fn install_entries(ctx: &Ctx, t: Target, flags: Flags) -> Result<Report> {
    let held = lock_writers(&ctx.config_dir)?;
    let recovery_note = journal::recover(&ctx.config_dir)?;
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
    .map(|mut r| {
        r.recovery_note = recovery_note;
        r
    })
}

pub fn uninstall(ctx: &Ctx, t: Target) -> Result<Report> {
    let _lock = lock_writers(&ctx.config_dir)?;
    let recovery_note = journal::recover(&ctx.config_dir)?;
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
    .map(|mut r| {
        r.recovery_note = recovery_note;
        r
    })
}

/// J2: how many entries are exactly acd's for `target`.
pub fn owned_entries(text: &str, target: Target) -> Result<usize> {
    let v: Value = serde_json::from_str(text)?;
    let Some(hooks) = v.get("hooks").and_then(Value::as_object) else {
        return Ok(0);
    };
    Ok(hooks
        .iter()
        .flat_map(|(event, groups)| {
            groups
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|g| {
                    g.get("hooks")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .filter(move |e| entries::classify_for(target, event, e) == Ownership::Owned)
        })
        .count())
}

/// `acd hooks status`: takes the writer lock, runs recovery, and reports the
/// journal state and the recorded destinations. A conflict is reported in the
/// text; installer commands are the ones that refuse.
pub fn status(ctx: &Ctx) -> Result<String> {
    let _held = lock_writers(&ctx.config_dir)?;
    let mut out = String::new();
    match journal::recover(&ctx.config_dir) {
        Ok(note) => {
            out.push_str("journal: none\n");
            if let Some(n) = note {
                out.push_str(&format!("note: {n}\n"));
            }
        }
        Err(e) => out.push_str(&format!("journal: {e:#}\n")),
    }
    for (path, r) in Record::load(&ctx.config_dir)?.files {
        let target = match r.target {
            Target::Claude => "claude",
            Target::Codex => "codex",
        };
        out.push_str(&format!("{target}: {}\n", path.display()));
    }
    Ok(out)
}

#[derive(Debug)]
pub struct RewriteReport {
    pub updated: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
    #[allow(dead_code)] // Task 27 reports it
    pub recovery_note: Option<String>,
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
                Ownership::Owned => e.get("headers").is_some_and(|headers| {
                    headers.get("X-Acd-Token").and_then(Value::as_str) != Some(token)
                }),
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

/// Before rotation commits, every recorded Claude destination must pass the
/// same safe read and Git checks used by the subsequent writes.
pub fn precheck_claude(ctx: &Ctx, _held: &WriterLock, flags: Flags) -> Result<()> {
    for dest in Record::load(&ctx.config_dir)?.claude_destinations() {
        let text = read_settings_pinned(&dest)?;
        git::check(&dest, flags.allow_tracked, flags.allow_unknown_git)?;
        let Some(text) = text else { continue };
        let value: Value = serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid JSON", dest.display()))?;
        let Some(hooks) = value.get("hooks").and_then(Value::as_object) else {
            continue;
        };
        for (event, groups) in hooks {
            for group in groups.as_array().into_iter().flatten() {
                for entry in group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if entries::classify_for(Target::Claude, event, entry) == Ownership::Suspicious
                    {
                        bail!(
                            "{}: an unexpected entry for {event} carries the acd marker; remove it by hand",
                            dest.display()
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn rewrite_claude_token(
    ctx: &Ctx,
    _held: &WriterLock,
    token: &str,
    flags: Flags,
) -> Result<RewriteReport> {
    let recovery_note = journal::recover(&ctx.config_dir)?;
    let mut report = RewriteReport {
        updated: vec![],
        failed: vec![],
        recovery_note,
    };
    let destinations = Record::load(&ctx.config_dir)?.claude_destinations();
    for recorded in &destinations {
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
            // J4: a failure that left a journal is transactional: stop here,
            // so the journal is never overwritten and the evidence stays.
            Err(e) if journal::pending(&ctx.config_dir) => {
                return Err(e.context(format!(
                    "stopped at {}; run `acd hooks status`",
                    recorded.display()
                )));
            }
            Err(e) => report.failed.push((recorded.clone(), format!("{e:#}"))),
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
    fn records_larger_than_4_kib_round_trip() {
        let d = tempfile::tempdir().unwrap();
        crate::secrets::ensure_private_dir(d.path()).unwrap();
        let mut r = Record {
            version: 2,
            ..Default::default()
        };
        for i in 0..40 {
            r.files.insert(
                format!("/home/u/{}/{i}/settings.json", "x".repeat(150)).into(),
                FileRecord {
                    target: Target::Claude,
                    created_file: false,
                    created_hooks_key: true,
                    created_events: vec!["Stop".into()],
                },
            );
        }
        r.save(d.path()).unwrap();
        assert_eq!(
            Record::load(d.path()).unwrap(),
            r,
            "F1: many canonical destinations"
        );
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
        let err = rewrite_claude_token(&f.ctx, &held, "new-token", FLAGS).unwrap_err();
        INJECTED.with(|i| i.borrow_mut().clear());

        let (first, second) = if a < b { (&a, &b) } else { (&b, &a) };
        assert!(
            format!("{err:#}").contains("injected failure at record-write"),
            "{err:#}"
        );
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
        // Repair: recovery applies the journal, then a second pass covers both.
        let report = rewrite_claude_token(&f.ctx, &held, "new-token", FLAGS).unwrap();
        assert_eq!(report.updated.len(), 2, "{:?}", report.failed);
        assert!(!journal::pending(&f.ctx.config_dir));
        for p in [&a, &b] {
            assert!(std::fs::read_to_string(p).unwrap().contains("new-token"));
        }
        assert_eq!(Record::load(&f.ctx.config_dir).unwrap().files.len(), 2);
    }

    #[test]
    fn owned_entries_counts_only_acd_entries() {
        let mut f = fixture();
        let original =
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"say done"}]}]}}"#;
        let path = install_into(&mut f, "claude-a", Some(original));
        let text = std::fs::read_to_string(path).unwrap();
        assert_eq!(owned_entries(&text, Target::Claude).unwrap(), 7);
        assert_eq!(owned_entries(original, Target::Claude).unwrap(), 0);
        assert_eq!(owned_entries("{}", Target::Claude).unwrap(), 0);
        assert_eq!(
            owned_entries(&text, Target::Codex).unwrap(),
            0,
            "Claude shape only"
        );
        assert!(owned_entries("nope", Target::Claude).is_err());
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
