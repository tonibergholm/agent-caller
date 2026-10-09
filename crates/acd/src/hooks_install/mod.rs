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
        crate::hook_token::fail_at("record-write")?;
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

fn canonical_destination(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().context("settings path has no parent")?;
    std::fs::create_dir_all(parent)?;
    Ok(std::fs::canonicalize(parent)?
        .join(path.file_name().context("settings path has no file name")?))
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

fn backup(ctx: &Ctx, path: &Path, text: &str) -> Result<PathBuf> {
    let dir = ctx.config_dir.join("backups");
    crate::secrets::ensure_private_dir(&dir)?;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let b = dir.join(format!("{name}.{ms}.bak"));
    crate::secrets::write_atomic(&b, text.as_bytes(), 0o600)?;
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

fn edit_settings(
    ctx: &Ctx,
    t: Target,
    path: &Path,
    flags: Flags,
    record_after: impl Fn(Option<&str>, &Obj) -> Option<FileRecord>,
    edit: impl Fn(&mut Obj) -> Result<usize>,
) -> Result<Report> {
    let dest = canonical_destination(path)?;
    for attempt in 0..3 {
        let before = read_settings(path)?;
        if t == Target::Claude {
            git::check(path, flags.allow_tracked, flags.allow_unknown_git)?;
        }
        let mut doc = Obj::parse(before.as_deref().unwrap_or_default())
            .with_context(|| format!("{} is not a JSON object", path.display()))?;
        let suspicious = edit(&mut doc)?;
        let remove = doc.is_empty()
            && record_after(before.as_deref(), &doc).is_none()
            && Record::load(&ctx.config_dir)?
                .files
                .get(&dest)
                .is_some_and(|r| r.created_file);
        let text = if remove {
            None
        } else {
            Some(format!("{}\n", doc.render()))
        };
        let backup = before
            .as_deref()
            .map(|b| backup(ctx, path, b))
            .transpose()?;
        let rec = record_after(before.as_deref(), &doc);
        journal::begin(
            &ctx.config_dir,
            &Journal {
                destination: dest.clone(),
                before: digest(before.as_deref()),
                after: digest(text.as_deref()),
                record_after: rec.clone(),
            },
        )?;
        test_touch(path, attempt);
        let current = read_settings(path)?;
        if current != before {
            journal::finish(&ctx.config_dir)?;
            continue;
        }
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            use std::os::unix::fs::MetadataExt;
            if meta.file_type().is_symlink()
                || meta.uid() != acd_securefs::euid()
                || !meta.is_file()
            {
                journal::finish(&ctx.config_dir)?;
                bail!(
                    "refusing to edit {}: destination changed to an unsafe file",
                    path.display()
                );
            }
        }
        let written = crate::hook_token::fail_at("settings-write").and_then(|()| match &text {
            Some(text) => crate::secrets::write_atomic(path, text.as_bytes(), file_mode(path, t)),
            None => std::fs::remove_file(path).map_err(Into::into),
        });
        if let Err(e) = written {
            journal::finish(&ctx.config_dir)?;
            return Err(e);
        }
        let mut record = Record::load(&ctx.config_dir)?;
        match rec {
            Some(r) => {
                record.files.insert(dest.clone(), r);
            }
            None => {
                record.files.remove(&dest);
            }
        }
        record.save(&ctx.config_dir)?;
        journal::finish(&ctx.config_dir)?;
        return Ok(Report {
            path: path.to_path_buf(),
            backup,
            removed_file: text.is_none(),
            suspicious,
        });
    }
    bail!(
        "{} kept changing while acd edited it; try again",
        path.display()
    )
}

pub fn install(ctx: &Ctx, t: Target, flags: Flags) -> Result<Report> {
    install_entries(ctx, t, flags)
}

fn install_entries(ctx: &Ctx, t: Target, flags: Flags) -> Result<Report> {
    let held = lock_writers(&ctx.config_dir)?;
    journal::recover(&ctx.config_dir)?;
    let path = ctx.settings_path(t);
    let dest = canonical_destination(&path);
    let dest = dest?;
    let groups = groups_for(ctx, &held, t)?;
    let prior = Record::load(&ctx.config_dir)?.files.get(&dest).cloned();
    let created = std::cell::RefCell::new((false, Vec::<String>::new()));
    edit_settings(
        ctx,
        t,
        &path,
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
    let dest = canonical_destination(&path)?;
    let rec = Record::load(&ctx.config_dir)?.files.get(&dest).cloned();
    let events = rec
        .as_ref()
        .map(|r| r.created_events.clone())
        .unwrap_or_default();
    let created_hooks = rec.as_ref().is_some_and(|r| r.created_hooks_key);
    edit_settings(
        ctx,
        t,
        &path,
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
    for dest in Record::load(&ctx.config_dir)?.claude_destinations() {
        let result = edit_settings(
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
                for (event, groups_raw) in hooks.0.iter_mut() {
                    let Ok(mut groups) = serde_json::from_str::<Vec<Value>>(groups_raw.get())
                    else {
                        continue;
                    };
                    let mut touched = false;
                    for g in groups.iter_mut() {
                        for e in g
                            .get_mut("hooks")
                            .and_then(Value::as_array_mut)
                            .into_iter()
                            .flatten()
                        {
                            match entries::classify(event, e) {
                                Ownership::Owned if e.get("headers").is_some() => {
                                    e["headers"]["X-Acd-Token"] = token.into();
                                    touched = true
                                }
                                Ownership::Suspicious => suspicious += 1,
                                _ => {}
                            }
                        }
                    }
                    if touched {
                        *groups_raw = raw(&Value::Array(groups));
                    }
                }
                if suspicious > 0 {
                    bail!(
                        "{suspicious} unexpected entries carry the acd marker; remove them by hand"
                    );
                }
                doc.set("hooks", raw_text(hooks.render())?);
                Ok(0)
            },
        );
        match result {
            Ok(_) => report.updated.push(dest),
            Err(e) => report.failed.push((dest, format!("{e:#}"))),
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
